//! Per-poll history of alive sessions at
//! `~/.ccsight/live_snapshots/<YYYY-MM-DD>-<HHMM>.json`. Filename is
//! lexicographically sortable; `captured_at` inside mirrors it in UTC.
//!
//! Write decisions (no-op vs overwrite vs new file) live on
//! [`LiveSnapshot::save_if_changed_in`]; retention tiers live on
//! [`LiveSnapshot::prune`]. All writes are atomic + `0o600`; failures
//! are silent so polling is never blocked.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use super::live_sessions::LiveSession;

/// Debounce window for in-place file updates. Within this window, polls
/// that observe a changed alive set will OVERWRITE the latest file
/// instead of creating a new one. Tuned so short bursts of helper
/// sessions (rule-generators, summary tasks) don't bloat the file count.
pub const SNAPSHOT_DEBOUNCE: chrono::Duration = chrono::Duration::minutes(30);

/// Recent-window where multiple snapshots per date are preserved. Days
/// older than this get condensed to a single (most-recent) snapshot.
pub const MULTI_SNAPSHOT_DAYS: i64 = 3;

/// Hard retention horizon. Snapshots from dates older than this get
/// removed entirely.
pub const SNAPSHOT_RETENTION_DAYS: i64 = 7;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveSnapshotEntry {
    pub session_id: String,
    pub cwd: PathBuf,
    #[serde(default)]
    pub jsonl_path: Option<PathBuf>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveSnapshot {
    pub date: NaiveDate,
    /// Wall-clock moment the snapshot was first written. The filename's
    /// HHMM matches the local-time hour/minute of this value.
    #[serde(default = "default_captured_at")]
    pub captured_at: DateTime<Utc>,
    pub sessions: HashMap<String, LiveSnapshotEntry>,
}

fn default_captured_at() -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH
}

impl LiveSnapshot {
    fn snapshot_dir() -> Option<PathBuf> {
        super::state_dir::live_snapshots_dir().ok()
    }

    fn build_filename(date: NaiveDate, captured_at_local: chrono::DateTime<Local>) -> String {
        // YYYY-MM-DD-HHMM keeps `read_dir` + sort in chronological order
        // across both date and time boundaries.
        format!(
            "{}-{}.json",
            date.format("%Y-%m-%d"),
            captured_at_local.format("%H%M")
        )
    }

    /// Parse a filename's `<YYYY-MM-DD>` prefix and optional `-HHMM`
    /// suffix. Returns (date, optional minute-of-day) on success.
    fn parse_filename(stem: &str) -> Option<(NaiveDate, Option<u32>)> {
        // Two shapes accepted (legacy first, current second):
        //   YYYY-MM-DD            → date only, no time
        //   YYYY-MM-DD-HHMM       → date + HHMM minute-of-day
        if let Ok(date) = NaiveDate::parse_from_str(stem, "%Y-%m-%d") {
            return Some((date, None));
        }
        // Split off the last `-` segment as candidate HHMM.
        let (date_part, hhmm) = stem.rsplit_once('-')?;
        let date = NaiveDate::parse_from_str(date_part, "%Y-%m-%d").ok()?;
        if hhmm.len() != 4 {
            return None;
        }
        let hours: u32 = hhmm[..2].parse().ok()?;
        let minutes: u32 = hhmm[2..].parse().ok()?;
        if hours >= 24 || minutes >= 60 {
            return None;
        }
        Some((date, Some(hours * 60 + minutes)))
    }

    pub(crate) fn save_to_path(&self, path: &Path) -> std::io::Result<()> {
        use std::io::Write;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
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

    /// Per-poll writer (no-op / overwrite / new file) targeting the
    /// default `~/.ccsight/live_snapshots/`. Tests MUST use
    /// [`Self::save_if_changed_in`] with a tmpdir — calling this from a
    /// test corrupts the user's real Live history (lint #40 catches it).
    pub fn save_if_changed(alive: &[LiveSession]) -> std::io::Result<()> {
        let Some(dir) = Self::snapshot_dir() else {
            return Ok(());
        };
        Self::save_if_changed_in(&dir, alive)
    }

    /// Dir-injectable core of [`Self::save_if_changed`]. Reads the latest
    /// snapshot from `dir` (today's file by local date), decides no-op /
    /// overwrite / new-file, and writes accordingly. Pure-function-ish
    /// over `dir` so tests can isolate to a tmpdir without polluting the
    /// user's real state directory.
    pub fn save_if_changed_in(dir: &Path, alive: &[LiveSession]) -> std::io::Result<()> {
        // An empty alive set carries no history (nothing to restore, nothing
        // to time-travel into). It happens transiently right after a reboot
        // while sessions relaunch one by one; recording those 0-session
        // moments litters the timeline with blank frames. Skip them.
        if alive.is_empty() {
            return Ok(());
        }
        let now_utc = Utc::now();
        let now_local = now_utc.with_timezone(&Local);
        let today = now_local.date_naive();
        let current_ids: HashSet<&str> = alive.iter().map(|s| s.session_id.as_str()).collect();

        let snaps_today = load_for_date_in(dir, today);
        let latest_today = snaps_today.iter().max_by_key(|s| s.captured_at);

        let same_set = latest_today.is_some_and(|prev| {
            prev.sessions.len() == alive.len()
                && current_ids.iter().all(|id| prev.sessions.contains_key(*id))
        });
        if same_set {
            return Ok(());
        }

        let mut sessions = HashMap::with_capacity(alive.len());
        for s in alive {
            sessions.insert(
                s.session_id.clone(),
                LiveSnapshotEntry {
                    session_id: s.session_id.clone(),
                    cwd: s.cwd.clone(),
                    jsonl_path: s.jsonl_path.clone(),
                    name: s.name.clone(),
                },
            );
        }

        // Within debounce window of the latest file → in-place overwrite.
        // Outside debounce (or no prior file today) → new file.
        let in_debounce =
            latest_today.is_some_and(|prev| now_utc - prev.captured_at < SNAPSHOT_DEBOUNCE);
        let snap = Self {
            date: today,
            captured_at: now_utc,
            sessions,
        };
        let target = if in_debounce {
            // Reuse the latest file's filename (built from ITS captured_at)
            // so the file name doesn't drift across overwrites within a
            // debounce window. Captured_at inside is still updated to now.
            let prev = latest_today.unwrap();
            let prev_local = prev.captured_at.with_timezone(&Local);
            dir.join(Self::build_filename(prev.date, prev_local))
        } else {
            dir.join(Self::build_filename(today, now_local))
        };
        snap.save_to_path(&target)
    }

    /// Load every snapshot file across the retention window, sorted
    /// captured_at descending (most recent first). Caller pages with
    /// `live_view_snapshot_offset`.
    pub fn load_recent() -> Vec<Self> {
        let Some(dir) = Self::snapshot_dir() else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let today = Local::now().date_naive();
        let cutoff = today - chrono::Duration::days(SNAPSHOT_RETENTION_DAYS - 1);
        let mut out: Vec<Self> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some((date, _)) = Self::parse_filename(stem) else {
                continue;
            };
            if date < cutoff {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(snap) = serde_json::from_str::<Self>(&text) else {
                continue;
            };
            // Skip empty snapshots — they carry no alive session and only
            // pad time-travel with blank frames / dilute the restorable
            // source. Handles legacy 0-session files written before
            // `save_if_changed` learned to skip them.
            if snap.sessions.is_empty() {
                continue;
            }
            out.push(snap);
        }
        out.sort_by_key(|s| std::cmp::Reverse(s.captured_at));
        out
    }

    /// Apply the tiered retention policy. Called on every poll.
    pub fn prune() {
        let Some(dir) = Self::snapshot_dir() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        let today = Local::now().date_naive();
        let multi_floor = today - chrono::Duration::days(MULTI_SNAPSHOT_DAYS - 1);
        let retention_floor = today - chrono::Duration::days(SNAPSHOT_RETENTION_DAYS - 1);

        // First pass: classify every file by (date, captured_at) and the
        // path so we can rank within-date and delete the losers in pass 2.
        let mut by_date: HashMap<NaiveDate, Vec<(PathBuf, DateTime<Utc>)>> = HashMap::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some((date, _)) = Self::parse_filename(stem) else {
                continue;
            };
            if date < retention_floor {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            // For dates within the multi-snapshot window we keep every
            // file unconditionally — skip ranking work.
            if date >= multi_floor {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(snap) = serde_json::from_str::<Self>(&text) else {
                continue;
            };
            by_date
                .entry(date)
                .or_default()
                .push((path, snap.captured_at));
        }

        for (_, mut group) in by_date {
            if group.len() <= 1 {
                continue;
            }
            // Most recent first; keep [0], delete the rest.
            group.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
            for (path, _) in group.into_iter().skip(1) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// Load every snapshot stored under a specific date from `dir`. Pure
/// over `dir` so [`LiveSnapshot::save_if_changed_in`] (and its tests)
/// can target any directory.
fn load_for_date_in(dir: &Path, date: NaiveDate) -> Vec<LiveSnapshot> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some((file_date, _)) = LiveSnapshot::parse_filename(stem) else {
            continue;
        };
        if file_date != date {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(snap) = serde_json::from_str::<LiveSnapshot>(&text) else {
            continue;
        };
        out.push(snap);
    }
    out
}

/// Alive-session map from the single most recent snapshot file = the
/// prior run's final poll. Frozen at boot into `AppState::prior_run_alive`
/// to drive `⟳ restorable`. MUST be read once at boot — the current run's
/// first `save_if_changed` overwrites the latest file, after which this
/// degrades to "currently alive" (which never overlaps the paused list).
pub fn latest_snapshot_alive() -> HashMap<String, LiveSnapshotEntry> {
    LiveSnapshot::load_recent()
        .into_iter()
        .next()
        .map(|snap| snap.sessions)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn parse_filename_accepts_both_legacy_and_hhmm_shapes() {
        let (d1, m1) = LiveSnapshot::parse_filename("2026-05-30").unwrap(); // lint-ok: date-literal
        assert_eq!(d1.format("%Y-%m-%d").to_string(), "2026-05-30");
        assert_eq!(m1, None);
        let (d2, m2) = LiveSnapshot::parse_filename("2026-05-30-1432").unwrap(); // lint-ok: date-literal
        assert_eq!(d2.format("%Y-%m-%d").to_string(), "2026-05-30");
        assert_eq!(m2, Some(14 * 60 + 32));
        // Out-of-range hour rejected (avoids silent acceptance of garbage).
        assert!(LiveSnapshot::parse_filename("2026-05-30-2599").is_none()); // lint-ok: date-literal
    }

    #[test]
    fn save_to_path_serialises_captured_at_alongside_sessions() {
        let tmpdir =
            std::env::temp_dir().join(format!("ccsight_daily_v2_rt_{}", std::process::id()));
        std::fs::create_dir_all(&tmpdir).unwrap();
        let path = tmpdir.join("2026-05-30-1432.json"); // lint-ok: date-literal
        let snap = LiveSnapshot {
            date: NaiveDate::from_ymd_opt(2026, 5, 30).unwrap(), // lint-ok: date-literal
            captured_at: Utc::now(),
            sessions: HashMap::from([(
                "a".to_string(),
                LiveSnapshotEntry {
                    session_id: "a".to_string(),
                    cwd: PathBuf::from("/tmp"),
                    jsonl_path: Some(PathBuf::from("/tmp/a.jsonl")),
                    name: None,
                },
            )]),
        };
        snap.save_to_path(&path).unwrap();
        let parsed: LiveSnapshot =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.sessions.len(), 1);
        assert!(parsed.sessions.contains_key("a"));
        let _ = std::fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn save_if_changed_in_writes_then_skips_when_set_unchanged() {
        // Isolated tmpdir — MUST NOT touch the user's real
        // ~/.ccsight/live_snapshots/. The `save_if_changed` wrapper that
        // resolves the state-dir is exercised separately via integration;
        // here we pin the core decision logic.
        let tmpdir = std::env::temp_dir().join(format!("ccsight_save_noop_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmpdir);
        std::fs::create_dir_all(&tmpdir).unwrap();
        let a = fake_live("session-a");
        LiveSnapshot::save_if_changed_in(&tmpdir, std::slice::from_ref(&a)).expect("first save");
        let count1 = load_for_date_in(&tmpdir, chrono::Local::now().date_naive()).len();
        LiveSnapshot::save_if_changed_in(&tmpdir, std::slice::from_ref(&a)).expect("second save");
        let count2 = load_for_date_in(&tmpdir, chrono::Local::now().date_naive()).len();
        assert_eq!(count1, count2, "second call with same set must be a no-op");
        let _ = std::fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn save_if_changed_in_skips_empty_alive_set() {
        // A 0-session poll (transient post-reboot) must NOT create a file —
        // empty snapshots only pad time-travel with blank frames.
        let tmpdir =
            std::env::temp_dir().join(format!("ccsight_save_empty_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmpdir);
        std::fs::create_dir_all(&tmpdir).unwrap();
        LiveSnapshot::save_if_changed_in(&tmpdir, &[]).expect("empty save is a no-op");
        let count = load_for_date_in(&tmpdir, chrono::Local::now().date_naive()).len();
        assert_eq!(count, 0, "empty alive set must not write a snapshot");
        let _ = std::fs::remove_dir_all(&tmpdir);
    }
}
