//! Persistent record of sessions observed alive across ccsight runs.
//!
//! After a host restart kills every Claude Code process, the
//! `~/.claude/sessions/<pid>.json` files may or may not survive. This file
//! is our own backup record: every poll, we refresh the snapshot with the
//! currently-alive set (session_id, cwd, jsonl path, name, last_seen). Next
//! time ccsight starts:
//!
//! 1. Paused entries whose `session_id` is in the snapshot get a `⟳` glyph
//!    so "I had this open before the reboot" is visible at a glance.
//! 2. Sessions in the snapshot whose JSONL is older than the paused-window
//!    (e.g. touched Friday afternoon, looking at it Monday) get pulled back
//!    into the paused list anyway via `recover_missing` — the snapshot
//!    knows the cwd / jsonl_path so we don't lose them just because their
//!    mtime fell out of the 24h JSONL scan.
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

    /// Stamp every alive session with `now`, then drop entries older than
    /// 30d so the file stays bounded. The "abruptly lost vs user-closed"
    /// distinction lives in the caller: `mark_was_recently_live` and
    /// `recover_missing` compare each entry's `last_seen` against the
    /// current ccsight run's start time, so manual-kill detection works
    /// without pruning observed-dead entries here. Keeping dead entries
    /// preserves the `⟳` flag for the entire current run instead of
    /// expiring it after the first poll cycle.
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
        app_start_time: DateTime<Utc>,
    ) -> Vec<LiveSession> {
        let mut out = Vec::new();
        for entry in self.sessions.values() {
            if active_ids.contains(&entry.session_id) || paused_ids.contains(&entry.session_id) {
                continue;
            }
            // Same filter as `mark_was_recently_live`: only entries
            // stamped before the current run survive a previous ccsight
            // session and qualify as `⟳`.
            if entry.last_seen >= app_start_time {
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
        // Snapshot entries are stamped 120s ago; treat the current run as
        // having started 60s ago so they qualify as "from a prior run".
        let app_start_time = Utc::now() - chrono::Duration::seconds(60);
        let recovered = snap.recover_missing(&active, &paused, app_start_time);

        assert_eq!(
            recovered.len(),
            1,
            "only the existing JSONL should be recovered"
        );
        assert_eq!(recovered[0].session_id, "real");
        assert!(recovered[0].was_recently_live);

        let _ = std::fs::remove_dir_all(&tmpdir);
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
