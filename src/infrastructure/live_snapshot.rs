//! Persistent record of sessions observed alive across ccsight runs.
//!
//! After a host restart kills every Claude Code process, the
//! `~/.claude/sessions/<pid>.json` files may or may not survive. This file
//! is our own backup record: every poll, we refresh the snapshot with the
//! currently-alive set (session_id, cwd, jsonl path, name, last_seen) and
//! stamp `last_refresh_at` as the run's heartbeat. Next time ccsight starts,
//! the prior run's heartbeat anchors the `⟳` cluster check:
//!
//! 1. Paused entries stamped in the prior run's final-poll cluster
//!    (`last_seen` within 1s of `last_refresh_at`) get a `⟳` glyph.
//! 2. Snapshot entries in that same cluster whose JSONL fell outside the
//!    24h paused scan (Friday→Monday case) are pulled back via
//!    `recover_missing` — the snapshot knows cwd / jsonl_path so we don't
//!    lose them just because mtime aged out.
//!
//! Retention: 30d. Covers most long holidays (golden week, year-end) plus
//! buffer. File size stays well under 100KB for typical usage.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::SystemTime;

use super::live_sessions::LiveSession;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub session_id: String,
    pub last_seen: DateTime<Utc>,
    pub cwd: PathBuf,
    #[serde(default)]
    pub jsonl_path: Option<PathBuf>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LiveSnapshot {
    /// session_id → richer entry. Map (not Vec) so updates are O(1) and
    /// duplicates collapse to the most-recent observation.
    #[serde(default)]
    pub sessions: HashMap<String, SnapshotEntry>,
    /// Wall-clock moment of the most recent `refresh()`. Read at the next
    /// ccsight startup as "the prior run's last heartbeat" — the authoritative
    /// anchor for the `⟳` cluster check (cannot be inferred from session
    /// stamps alone, since mid-run deaths leave orphan stamps).
    /// `Option` is for backward-compat with snapshots predating this field.
    #[serde(default)]
    pub last_refresh_at: Option<DateTime<Utc>>,
}

impl LiveSnapshot {
    /// Load from disk; empty snapshot on any read / parse error so a
    /// corrupt or schema-changed file never blocks startup.
    pub fn load() -> Self {
        let Ok(path) = super::state_dir::live_snapshot_path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Atomic write via tmp + rename. Silent on failure — the snapshot is a
    /// nice-to-have, not load-bearing.
    pub fn save(&self) {
        let Ok(path) = super::state_dir::live_snapshot_path() else {
            return;
        };
        let _ = self.save_to_path(&path);
    }

    /// Atomic write to a specific path. Extracted from `save()` so the
    /// permission and durability properties can be exercised by unit
    /// tests without touching `~/.ccsight/`.
    pub(crate) fn save_to_path(&self, path: &std::path::Path) -> std::io::Result<()> {
        use std::io::Write;
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Atomic write: write tmp, restrict permissions to user-only,
        // fsync to flush page cache to disk, then rename. Snapshot records
        // cwd / jsonl_path / name for every active session — same
        // sensitivity as the cache file, so worth the same `0o600` mode.
        // Permissions are set AFTER write_all + drop because some
        // filesystems (macOS APFS observed) silently reset perms when an
        // open file handle is closed via drop; chmod-after-close beats
        // both that quirk and the umask race.
        let tmp = path.with_extension("json.tmp");
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.sync_all()?;
        drop(f);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Stamp alive sessions with `now`, set `last_refresh_at`, prune
    /// entries older than 30d. Dead entries stay so the next run's anchor
    /// check (`mark_was_recently_live` / `recover_missing`) can compare
    /// their `last_seen` against the frozen heartbeat.
    pub fn refresh(&mut self, alive: &[LiveSession]) {
        let now = Utc::now();
        for s in alive {
            self.sessions.insert(
                s.session_id.clone(),
                SnapshotEntry {
                    session_id: s.session_id.clone(),
                    last_seen: now,
                    cwd: s.cwd.clone(),
                    jsonl_path: s.jsonl_path.clone(),
                    name: s.name.clone(),
                },
            );
        }
        let cutoff = now - chrono::Duration::days(30);
        self.sessions.retain(|_, e| e.last_seen >= cutoff);
        self.last_refresh_at = Some(now);
    }

    /// Cluster anchor for `mark_was_recently_live` / `recover_missing`.
    /// Prefers `last_refresh_at`; falls back to `max(last_seen)` when the
    /// field is absent (old-format snapshot). The fallback is best-effort
    /// — it can pick up an orphan mid-run-death stamp — and exists so a
    /// post-upgrade first run still surfaces `⟳` markers.
    ///
    /// Must be called at boot only; the result is frozen into
    /// `AppState::prior_run_last_refresh`. Recomputing mid-run after the
    /// polling thread's `refresh()` returns a value from the *current*
    /// run instead — see the anti-pattern test in `live_sessions::tests`.
    pub fn prior_run_anchor(&self) -> Option<DateTime<Utc>> {
        let now = Utc::now();
        if let Some(t) = self.last_refresh_at
            && t < now
        {
            return Some(t);
        }
        self.sessions
            .values()
            .filter(|e| e.last_seen < now)
            .map(|e| e.last_seen)
            .max()
    }

    /// Build pseudo-paused `LiveSession` entries from snapshot rows whose
    /// session is **not** already in the active or paused set. Used to
    /// surface sessions whose JSONL fell outside the 24h paused scan but
    /// were alive (per our snapshot) within the retention window — the
    /// "Friday→Monday" recovery case.
    pub fn recover_missing(
        &self,
        active_ids: &HashSet<String>,
        paused_ids: &HashSet<String>,
        prior_run_anchor: Option<DateTime<Utc>>,
    ) -> Vec<LiveSession> {
        // Anchor must be frozen by the caller (AppState) at boot; otherwise
        // an in-run refresh would drag the cluster forward and expire ⟳.
        let Some(anchor) = prior_run_anchor else {
            return Vec::new();
        };
        let cluster_floor = anchor - chrono::Duration::seconds(1);
        let mut out = Vec::new();
        for entry in self.sessions.values() {
            if active_ids.contains(&entry.session_id) || paused_ids.contains(&entry.session_id) {
                continue;
            }
            if entry.last_seen < cluster_floor || entry.last_seen > anchor {
                continue;
            }
            let Some(jsonl) = entry.jsonl_path.as_ref() else {
                continue;
            };
            if !jsonl.exists() {
                continue;
            }
            let jsonl_mtime = std::fs::metadata(jsonl)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
                .and_then(|d| {
                    DateTime::<Utc>::from_timestamp(d.as_secs() as i64, d.subsec_nanos())
                });
            out.push(LiveSession {
                session_id: entry.session_id.clone(),
                jsonl_path: entry.jsonl_path.clone(),
                cwd: entry.cwd.clone(),
                name: entry.name.clone(),
                status: None,
                pid: 0,
                started_at: None,
                updated_at: Some(entry.last_seen),
                jsonl_mtime,
                is_live: false,
                was_recently_live: true,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fake_live(id: &str) -> LiveSession {
        LiveSession {
            session_id: id.to_string(),
            jsonl_path: Some(PathBuf::from(format!("/tmp/{id}.jsonl"))),
            cwd: PathBuf::from("/tmp"),
            name: None,
            status: None,
            pid: 0,
            started_at: None,
            updated_at: None,
            jsonl_mtime: None,
            is_live: true,
            was_recently_live: false,
        }
    }

    #[test]
    fn refresh_stamps_alive_set() {
        let mut snap = LiveSnapshot::default();
        snap.refresh(&[fake_live("a"), fake_live("b")]);
        assert!(snap.sessions.contains_key("a"));
        assert!(snap.sessions.contains_key("b"));
    }

    #[test]
    fn refresh_30d_cutoff_prunes_stale_entries() {
        // 30d safety cutoff for entries that pre-date a long-dormant
        // ccsight install. Manual-kill detection lives in
        // `mark_was_recently_live` (compares `last_seen` to the current
        // run's start time), so `refresh` keeps observed-dead entries —
        // only age-based pruning happens here.
        let mut snap = LiveSnapshot::default();
        snap.sessions.insert(
            "stale".to_string(),
            SnapshotEntry {
                session_id: "stale".to_string(),
                last_seen: Utc::now() - chrono::Duration::days(31),
                cwd: PathBuf::from("/tmp"),
                jsonl_path: Some(PathBuf::from("/tmp/stale.jsonl")),
                name: None,
            },
        );
        snap.refresh(&[]);
        assert!(!snap.sessions.contains_key("stale"));
    }

    #[test]
    fn recover_missing_includes_only_live_jsonl_files() {
        // Two snapshot entries: one with a real (just-touched) JSONL,
        // one with a deleted path. recover_missing should yield only the
        // first.
        let tmpdir = std::env::temp_dir().join(format!("ccsight_snap_test_{}", std::process::id()));
        std::fs::create_dir_all(&tmpdir).unwrap();
        let real = tmpdir.join("real.jsonl");
        std::fs::write(&real, b"{}").unwrap();
        let missing = tmpdir.join("nope.jsonl");

        let mut snap = LiveSnapshot::default();
        snap.sessions.insert(
            "real".to_string(),
            SnapshotEntry {
                session_id: "real".to_string(),
                last_seen: Utc::now() - chrono::Duration::seconds(120),
                cwd: tmpdir.clone(),
                jsonl_path: Some(real.clone()),
                name: None,
            },
        );
        snap.sessions.insert(
            "missing".to_string(),
            SnapshotEntry {
                session_id: "missing".to_string(),
                last_seen: Utc::now() - chrono::Duration::seconds(120),
                cwd: tmpdir.clone(),
                jsonl_path: Some(missing),
                name: None,
            },
        );

        let active = std::collections::HashSet::new();
        let paused = std::collections::HashSet::new();
        let anchor = snap.prior_run_anchor();
        let recovered = snap.recover_missing(&active, &paused, anchor);

        assert_eq!(
            recovered.len(),
            1,
            "only the existing JSONL should be recovered"
        );
        assert_eq!(recovered[0].session_id, "real");
        assert!(recovered[0].was_recently_live);

        let _ = std::fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn recover_missing_only_pulls_in_latest_run_cluster() {
        // Regression: prior to the cluster filter, `recover_missing` pulled
        // in *every* snapshot row whose `last_seen < app_start_time`. With a
        // long-lived snapshot containing entries from multiple historical
        // runs, that meant week-old sessions reappeared as `⟳ from last
        // ccsight run` even though the most recent prior run never saw them.
        let tmpdir =
            std::env::temp_dir().join(format!("ccsight_snap_cluster_test_{}", std::process::id()));
        std::fs::create_dir_all(&tmpdir).unwrap();
        let recent = tmpdir.join("recent.jsonl");
        let old = tmpdir.join("old.jsonl");
        std::fs::write(&recent, b"{}").unwrap();
        std::fs::write(&old, b"{}").unwrap();

        let mut snap = LiveSnapshot::default();
        // Most recent prior run's final poll = -120s.
        snap.sessions.insert(
            "recent".to_string(),
            SnapshotEntry {
                session_id: "recent".to_string(),
                last_seen: Utc::now() - chrono::Duration::seconds(120),
                cwd: tmpdir.clone(),
                jsonl_path: Some(recent.clone()),
                name: None,
            },
        );
        // Older run, hours earlier. Even though its JSONL is still on disk
        // and `last_seen < app_start_time`, it does NOT belong to "the
        // last ccsight run".
        snap.sessions.insert(
            "old_run".to_string(),
            SnapshotEntry {
                session_id: "old_run".to_string(),
                last_seen: Utc::now() - chrono::Duration::hours(6),
                cwd: tmpdir.clone(),
                jsonl_path: Some(old.clone()),
                name: None,
            },
        );

        let active = std::collections::HashSet::new();
        let paused = std::collections::HashSet::new();
        let anchor = snap.prior_run_anchor();
        let recovered = snap.recover_missing(&active, &paused, anchor);

        assert_eq!(
            recovered.len(),
            1,
            "only the latest prior run's cluster should be recovered"
        );
        assert_eq!(recovered[0].session_id, "recent");

        let _ = std::fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn recover_missing_uses_last_refresh_at_when_present() {
        // Distinguishes the primary path from the fallback. Setup: prior
        // ccsight exited at T0 with NO sessions alive — only a mid-run
        // death orphan remains in the snapshot. `last_refresh_at = T0`
        // anchors the cluster on T0, so the orphan must NOT be recovered.
        // (Fallback would anchor on the orphan stamp and wrongly pull it
        // back; this test pins that the primary path takes precedence.)
        let tmpdir =
            std::env::temp_dir().join(format!("ccsight_snap_primary_path_{}", std::process::id()));
        std::fs::create_dir_all(&tmpdir).unwrap();
        let orphan_jsonl = tmpdir.join("orphan.jsonl");
        std::fs::write(&orphan_jsonl, b"{}").unwrap();

        let mut snap = LiveSnapshot::default();
        let prior_exit = Utc::now() - chrono::Duration::seconds(120);
        let mid_death = prior_exit - chrono::Duration::minutes(20);
        snap.last_refresh_at = Some(prior_exit);
        snap.sessions.insert(
            "orphan".to_string(),
            SnapshotEntry {
                session_id: "orphan".to_string(),
                last_seen: mid_death,
                cwd: tmpdir.clone(),
                jsonl_path: Some(orphan_jsonl.clone()),
                name: None,
            },
        );

        let active = std::collections::HashSet::new();
        let paused = std::collections::HashSet::new();
        let anchor = snap.prior_run_anchor();
        assert_eq!(anchor, Some(prior_exit));
        let recovered = snap.recover_missing(&active, &paused, anchor);
        assert!(
            recovered.is_empty(),
            "orphan mid-run-death must NOT be recovered when last_refresh_at anchors elsewhere"
        );

        let _ = std::fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn prior_run_anchor_prefers_last_refresh_at_over_max_heuristic() {
        // Two prior sessions: a survivor stamped at prior exit, and a
        // mid-run-death stamped 22m earlier. Anchor must follow
        // `last_refresh_at`, not `max(last_seen)`, otherwise the mid-run
        // death gets mistaken for the exit cluster.
        let mut snap = LiveSnapshot::default();
        let prior_exit = Utc::now() - chrono::Duration::minutes(60);
        let mid_run_death = prior_exit - chrono::Duration::minutes(22);
        snap.last_refresh_at = Some(prior_exit);
        // Survivor stamped at the actual prior exit moment.
        snap.sessions.insert(
            "survivor".to_string(),
            SnapshotEntry {
                session_id: "survivor".to_string(),
                last_seen: prior_exit,
                cwd: PathBuf::from("/tmp"),
                jsonl_path: Some(PathBuf::from("/tmp/survivor.jsonl")),
                name: None,
            },
        );
        // Mid-run death — last stamped 22m before prior run ended.
        snap.sessions.insert(
            "mid_run_death".to_string(),
            SnapshotEntry {
                session_id: "mid_run_death".to_string(),
                last_seen: mid_run_death,
                cwd: PathBuf::from("/tmp"),
                jsonl_path: Some(PathBuf::from("/tmp/mid.jsonl")),
                name: None,
            },
        );
        let anchor = snap.prior_run_anchor();
        assert_eq!(
            anchor,
            Some(prior_exit),
            "anchor must come from last_refresh_at, not max(last_seen)"
        );
    }

    #[test]
    fn prior_run_anchor_falls_back_to_max_when_field_absent() {
        // Old-format snapshot path: `last_refresh_at == None` triggers the
        // max-stamp-below-app-start fallback so the post-upgrade first run
        // still surfaces some ⟳ markers.
        let mut snap = LiveSnapshot::default();
        snap.last_refresh_at = None;
        let stamp = Utc::now() - chrono::Duration::minutes(10);
        snap.sessions.insert(
            "any".to_string(),
            SnapshotEntry {
                session_id: "any".to_string(),
                last_seen: stamp,
                cwd: PathBuf::from("/tmp"),
                jsonl_path: None,
                name: None,
            },
        );
        let anchor = snap.prior_run_anchor();
        assert_eq!(anchor, Some(stamp));
    }

    #[test]
    fn refresh_writes_last_refresh_at() {
        // Each refresh must stamp the field so the next ccsight process
        // reads an authoritative anchor instead of degrading to the fallback.
        let mut snap = LiveSnapshot::default();
        assert!(snap.last_refresh_at.is_none());
        let before = Utc::now();
        snap.refresh(&[]);
        let after = Utc::now();
        let t = snap.last_refresh_at.expect("refresh must populate field");
        assert!(t >= before && t <= after);
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_with_user_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmpdir = std::env::temp_dir().join(format!("ccsight_snap_perm_{}", std::process::id()));
        std::fs::create_dir_all(&tmpdir).unwrap();
        let path = tmpdir.join("live_snapshot.json");
        let mut snap = LiveSnapshot::default();
        snap.refresh(&[fake_live("perm_test")]);
        snap.save_to_path(&path).expect("save");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "snapshot file must be user-only readable");
        let _ = std::fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn malformed_json_loads_as_default() {
        // We can't easily inject a path override, but we can directly
        // exercise the parse-vs-default fallback by serializing a bogus
        // string: serde_json failure → Self::default().
        let bogus = "{ not json";
        let parsed: LiveSnapshot = serde_json::from_str(bogus).unwrap_or_default();
        assert!(parsed.sessions.is_empty());
    }
}
