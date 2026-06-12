//! Persistent record of sessions observed alive across ccsight runs.
//!
//! Stores the most recent (session_id → cwd / jsonl_path / name / last_seen)
//! observation for each session. This file is **diagnostic-only**: the
//! per-session restorable-marker logic now lives in
//! [`super::live_snapshots`], which keeps per-local-date alive snapshots that
//! drive the `⟳` predicate. `last_refresh_at` is still updated here so
//! external readers (MCP / debugging) can answer "when did the last ccsight
//! poll happen".
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
pub struct LiveDiagnostic {
    /// session_id → richer entry. Map (not Vec) so updates are O(1) and
    /// duplicates collapse to the most-recent observation.
    #[serde(default)]
    pub sessions: HashMap<String, SnapshotEntry>,
    /// Wall-clock moment of the most recent `refresh()`. Diagnostic only.
    #[serde(default)]
    pub last_refresh_at: Option<DateTime<Utc>>,
}

impl LiveDiagnostic {
    /// Load from disk; empty snapshot on any read / parse error so a
    /// corrupt or schema-changed file never blocks startup.
    pub fn load() -> Self {
        let Ok(path) = super::state_dir::live_diagnostic_path() else {
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
        let Ok(path) = super::state_dir::live_diagnostic_path() else {
            return;
        };
        let _ = self.save_to_path(&path);
    }

    /// Atomic write to a specific path. Extracted from `save()` so the
    /// permission and durability properties can be exercised by unit
    /// tests without touching `~/.ccsight/`.
    pub(crate) fn save_to_path(&self, path: &std::path::Path) -> std::io::Result<()> {
        // Pretty JSON: this singleton is debug-inspected by hand (CLAUDE.md).
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        super::atomic_write(path, &json)
    }

    /// Stamp alive sessions with `now`, set `last_refresh_at`, prune
    /// entries older than 30d. No transition detection — restorable status
    /// is computed off `super::live_snapshots` snapshots, not this file.
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

    /// Build pseudo-paused `LiveSession` entries for sessions that were
    /// alive in the prior run's final snapshot (`prior_alive`, frozen at
    /// boot) but are absent from both the currently-alive set and the
    /// JSONL-mtime paused scan. Covers post-reboot recovery where the 24h
    /// scan misses sessions whose JSONL hasn't been touched.
    pub fn recover_missing(
        active_ids: &HashSet<String>,
        paused_ids: &HashSet<String>,
        prior_alive: &HashMap<String, super::live_snapshots::LiveSnapshotEntry>,
    ) -> Vec<LiveSession> {
        let mut out = Vec::new();
        for (id, entry) in prior_alive.clone() {
            if active_ids.contains(&id) || paused_ids.contains(&id) {
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
                session_id: id,
                jsonl_path: entry.jsonl_path.clone(),
                cwd: entry.cwd.clone(),
                name: entry.name.clone(),
                status: None,
                pid: 0,
                started_at: None,
                updated_at: jsonl_mtime,
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
        let mut snap = LiveDiagnostic::default();
        snap.refresh(&[fake_live("a"), fake_live("b")]);
        assert!(snap.sessions.contains_key("a"));
        assert!(snap.sessions.contains_key("b"));
    }

    #[test]
    fn recover_missing_only_pulls_from_prior_alive_with_existing_jsonl() {
        use super::super::live_snapshots::LiveSnapshotEntry;
        let tmpdir = std::env::temp_dir().join(format!("ccsight_recover_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmpdir);
        std::fs::create_dir_all(&tmpdir).unwrap();
        let real_jsonl = tmpdir.join("real.jsonl");
        std::fs::write(&real_jsonl, b"{}").unwrap();

        let mut prior: HashMap<String, LiveSnapshotEntry> = HashMap::new();
        // (a) In prior_alive, jsonl exists → recovered.
        prior.insert(
            "recoverable".to_string(),
            LiveSnapshotEntry {
                session_id: "recoverable".to_string(),
                cwd: tmpdir.clone(),
                jsonl_path: Some(real_jsonl.clone()),
                name: None,
            },
        );
        // (b) In prior_alive, jsonl missing on disk → skipped.
        prior.insert(
            "ghost".to_string(),
            LiveSnapshotEntry {
                session_id: "ghost".to_string(),
                cwd: tmpdir.clone(),
                jsonl_path: Some(tmpdir.join("does-not-exist.jsonl")),
                name: None,
            },
        );

        let active = HashSet::new();
        let paused = HashSet::new();
        let recovered = LiveDiagnostic::recover_missing(&active, &paused, &prior);
        let ids: Vec<&str> = recovered.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["recoverable"],
            "only existing-jsonl prior-alive rows recover"
        );
        assert!(recovered[0].was_recently_live);
        // The recovered row must carry the snapshot entry's cwd / jsonl_path
        // verbatim — a field-mapping regression (the commit's cwd theme)
        // would otherwise pass with only the id/flag checks above.
        assert_eq!(recovered[0].cwd, tmpdir, "recovered cwd from entry");
        assert_eq!(
            recovered[0].jsonl_path.as_deref(),
            Some(real_jsonl.as_path()),
            "recovered jsonl_path from entry"
        );
        let _ = std::fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn recover_missing_excludes_active_and_paused_ids() {
        use super::super::live_snapshots::LiveSnapshotEntry;
        let tmpdir =
            std::env::temp_dir().join(format!("ccsight_recover_excl_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmpdir);
        std::fs::create_dir_all(&tmpdir).unwrap();
        let jp = tmpdir.join("x.jsonl");
        std::fs::write(&jp, b"{}").unwrap();
        let mut prior: HashMap<String, LiveSnapshotEntry> = HashMap::new();
        for id in ["a", "b", "c"] {
            prior.insert(
                id.to_string(),
                LiveSnapshotEntry {
                    session_id: id.to_string(),
                    cwd: tmpdir.clone(),
                    jsonl_path: Some(jp.clone()),
                    name: None,
                },
            );
        }
        let active: HashSet<String> = ["a".to_string()].into_iter().collect();
        let paused: HashSet<String> = ["b".to_string()].into_iter().collect();
        let recovered = LiveDiagnostic::recover_missing(&active, &paused, &prior);
        let ids: Vec<&str> = recovered.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(ids, vec!["c"], "active+paused excluded, only c remains");
        let _ = std::fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn refresh_30d_cutoff_prunes_stale_entries() {
        let mut snap = LiveDiagnostic::default();
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
    fn refresh_writes_last_refresh_at() {
        let mut snap = LiveDiagnostic::default();
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
        let mut snap = LiveDiagnostic::default();
        snap.refresh(&[fake_live("perm_test")]);
        snap.save_to_path(&path).expect("save");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "snapshot file must be user-only readable");
        let _ = std::fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn malformed_json_loads_as_default() {
        let bogus = "{ not json";
        let parsed: LiveDiagnostic = serde_json::from_str(bogus).unwrap_or_default();
        assert!(parsed.sessions.is_empty());
    }
}
