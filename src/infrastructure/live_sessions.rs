//! Live Claude Code session discovery.
//!
//! Reads `~/.claude/sessions/<pid>.json` — first-party metadata Claude Code
//! writes for every running session. Each file pins `pid → sessionId → cwd`
//! with no ambiguity, so detection is exact (no argv parsing, no temporal
//! heuristics).
//!
//! For "recently paused", scans `~/.claude/projects/**/*.jsonl` for files
//! whose mtime is within the last 24h and whose sessionId is *not* in the
//! active set.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// Idle-but-recently-touched window. A live session that hasn't completed a
/// new message within this many seconds drops from "warm" (🟡) to "idle"
/// (◎). 30 minutes covers a typical continuous work block (left a tab open
/// while grabbing a coffee) without dragging in sessions left open
/// overnight. Single source of truth for the sort tier and the row glyph.
pub const WARM_THRESHOLD_SECS: i64 = 30 * 60;

/// Per-process metadata as written by Claude Code into
/// `~/.claude/sessions/<pid>.json`. Field names match the JSON schema; some
/// are optional because older versions / non-interactive entrypoints may
/// omit them.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionMeta {
    pid: u32,
    session_id: String,
    cwd: PathBuf,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    status: Option<String>,
    /// Unix ms timestamp.
    #[serde(default)]
    started_at: Option<u64>,
    /// Unix ms timestamp; updated as the session continues.
    #[serde(default)]
    updated_at: Option<u64>,
}

/// A discoverable Claude Code session. Subagents never appear here: they
/// don't run as separate processes (so no `<pid>.json`), and `agent-*.jsonl`
/// artifacts are filtered out of the paused scan since they're not
/// user-resumable on their own.
#[derive(Debug, Clone)]
pub struct LiveSession {
    pub session_id: String,
    pub jsonl_path: Option<PathBuf>,
    pub cwd: PathBuf,
    /// Slug from Claude Code's `name` field — usually ai_title / custom_title
    /// or the `--resume <slug>` argument.
    pub name: Option<String>,
    pub status: Option<String>,
    pub pid: u32,
    pub started_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub jsonl_mtime: Option<DateTime<Utc>>,
    /// True when the source was a `~/.claude/sessions/<pid>.json` file (and
    /// the PID is still alive). False when the session was reconstructed
    /// from a recent JSONL mtime alone (paused path).
    pub is_live: bool,
    /// Paused-only signal: this session_id appeared in the persisted
    /// snapshot of "previously observed alive". Powers the post-restart
    /// "I had this open before reboot" hint via a `⟳` glyph.
    pub was_recently_live: bool,
}

/// Slugify a cwd path into the directory name Claude Code uses under
/// `~/.claude/projects/`. Every `/` becomes `-`; leading slash produces a
/// leading `-` (e.g. `/Users/x/dev/foo` → `-Users-x-dev-foo`).
fn cwd_to_project_dir(cwd: &Path) -> String {
    cwd.to_string_lossy().replace('/', "-")
}

fn claude_sessions_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude").join("sessions"))
}

fn claude_projects_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude").join("projects"))
}

fn unix_ms_to_utc(ms: u64) -> Option<DateTime<Utc>> {
    let secs = (ms / 1000) as i64;
    let nsec = ((ms % 1000) * 1_000_000) as u32;
    DateTime::<Utc>::from_timestamp(secs, nsec)
}

/// Cross-platform process-existence check. Uses `kill -0 <pid>` which is
/// POSIX-portable and doesn't require linking libc. ~1ms per check, fine
/// for the typical small (~15) live-session count.
fn is_process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn jsonl_path_for(cwd: &Path, session_id: &str) -> Option<PathBuf> {
    let projects = claude_projects_dir()?;
    Some(
        projects
            .join(cwd_to_project_dir(cwd))
            .join(format!("{session_id}.jsonl")),
    )
}

fn mtime_utc(path: &Path) -> Option<DateTime<Utc>> {
    let m = std::fs::metadata(path).ok()?;
    let modified = m.modified().ok()?;
    let duration = modified.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    DateTime::<Utc>::from_timestamp(duration.as_secs() as i64, duration.subsec_nanos())
}

/// Enumerate live sessions from Claude Code's session metadata directory.
/// Returns sessions whose PID is verifiably alive (stale files are skipped).
pub fn discover_live() -> Vec<LiveSession> {
    let Some(dir) = claude_sessions_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<SessionMeta>(&raw) else {
            continue;
        };
        if !is_process_alive(meta.pid) {
            continue;
        }
        // Defensive: Claude Code subagents share their parent's PID and so
        // shouldn't produce their own `<pid>.json`, but skip any future
        // entries that explicitly self-identify as agents.
        if meta
            .kind
            .as_deref()
            .is_some_and(|k| k.eq_ignore_ascii_case("agent"))
        {
            continue;
        }
        let jsonl_path = jsonl_path_for(&meta.cwd, &meta.session_id);
        let jsonl_mtime = jsonl_path.as_deref().and_then(mtime_utc);
        out.push(LiveSession {
            session_id: meta.session_id,
            jsonl_path,
            cwd: meta.cwd,
            name: meta.name,
            status: meta.status,
            pid: meta.pid,
            started_at: meta.started_at.and_then(unix_ms_to_utc),
            updated_at: meta.updated_at.and_then(unix_ms_to_utc),
            jsonl_mtime,
            is_live: true,
            was_recently_live: false,
        });
    }
    // Tier 1: status — busy first, then "warm" (idle but recently touched),
    // then long-idle. Tier 2: age descending within each tier.
    let zero_ts = DateTime::<Utc>::UNIX_EPOCH;
    let now = chrono::Utc::now();
    let warm_threshold = chrono::Duration::seconds(WARM_THRESHOLD_SECS);
    out.sort_by_key(|s| {
        let ts = s.updated_at.or(s.started_at).unwrap_or(zero_ts);
        let tier: u8 = if s.status.as_deref() == Some("busy") {
            0
        } else if (now - ts) < warm_threshold {
            1
        } else {
            2
        };
        (tier, std::cmp::Reverse(ts))
    });
    out
}

/// Scan `~/.claude/projects/**/*.jsonl` for sessions whose mtime is within
/// `window` of `now` and whose UUID is *not* in `active_ids`. The returned
/// sessions carry minimal metadata: cwd / name / status are unknown from a
/// JSONL alone (callers can enrich from `state.daily_groups`).
pub fn discover_recently_paused(
    active_ids: &std::collections::HashSet<String>,
    window: Duration,
    now: SystemTime,
) -> Vec<LiveSession> {
    let Some(projects) = claude_projects_dir() else {
        return Vec::new();
    };
    let cutoff = now - window;
    let Ok(project_entries) = std::fs::read_dir(&projects) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for project_dir in project_entries.flatten() {
        let project_path = project_dir.path();
        if !project_path.is_dir() {
            continue;
        }
        let Ok(jsonl_entries) = std::fs::read_dir(&project_path) else {
            continue;
        };
        for jsonl_entry in jsonl_entries.flatten() {
            let jsonl_path = jsonl_entry.path();
            if jsonl_path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(stem) = jsonl_path
                .file_stem()
                .and_then(|n| n.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            // Subagent JSONL artifacts (`agent-<uuid>.jsonl`) are byproducts
            // of a parent session and not user-resumable on their own —
            // listing them in "Recently paused" is noise.
            if stem.starts_with("agent-") {
                continue;
            }
            if active_ids.contains(&stem) {
                continue;
            }
            let Ok(modified) = std::fs::metadata(&jsonl_path).and_then(|m| m.modified()) else {
                continue;
            };
            if modified < cutoff {
                continue;
            }
            let jsonl_mtime = mtime_utc(&jsonl_path);
            // Reconstruct the cwd from the slugified directory name by
            // reversing the `/`→`-` substitution. This is best-effort —
            // multiple original paths can map to the same slug, but it
            // matches Claude Code's own convention.
            let dir_name = project_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            let cwd = PathBuf::from(dir_name.replace('-', "/"));
            out.push(LiveSession {
                session_id: stem,
                jsonl_path: Some(jsonl_path),
                cwd,
                name: None,
                status: None,
                pid: 0,
                started_at: None,
                updated_at: None,
                jsonl_mtime,
                is_live: false,
                was_recently_live: false,
            });
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.jsonl_mtime.unwrap_or(DateTime::<Utc>::UNIX_EPOCH)));
    out
}

/// Mark paused entries whose `session_id` appears in the persisted snapshot
/// and re-sort so "previously open before restart" rows float to the top of
/// the paused list. Run on the caller side because the snapshot lives
/// outside this module.
pub fn mark_was_recently_live(
    paused: &mut [LiveSession],
    snapshot: &super::live_snapshot::LiveSnapshot,
    app_start_time: DateTime<Utc>,
) {
    // "From last ccsight run" means alive at the END of the most recent
    // prior run — not "alive in any past run". Each `refresh()` stamps
    // currently-alive sessions with the same `now`, so the final poll's
    // `now` is reflected as the maximum `last_seen` across the snapshot.
    // Sessions sharing that timestamp (within a small tolerance) were the
    // surviving-alive set at run end; older `last_seen` values came from
    // either earlier runs or mid-run deaths.
    let Some(max_last_seen) = snapshot
        .sessions
        .values()
        .filter(|e| e.last_seen < app_start_time)
        .map(|e| e.last_seen)
        .max()
    else {
        return;
    };
    // Tolerance covers graceful-shutdown skew where the final poll's stamp
    // can land a few seconds apart across rapid concurrent updates.
    let cluster_window = chrono::Duration::seconds(60);
    let cluster_floor = max_last_seen - cluster_window;

    for entry in paused.iter_mut() {
        if let Some(snap_entry) = snapshot.sessions.get(&entry.session_id) {
            if snap_entry.last_seen >= cluster_floor && snap_entry.last_seen < app_start_time {
                entry.was_recently_live = true;
                // Stamp `updated_at` with the snapshot's `last_seen` so the
                // row age reflects "when ccsight last saw this alive".
                entry.updated_at = Some(snap_entry.last_seen);
            }
        }
    }
    let zero_ts = DateTime::<Utc>::UNIX_EPOCH;
    paused.sort_by_key(|s| {
        // Tier 1: was_recently_live first so post-restart recovery hints
        // stay at the top regardless of JSONL mtime. Tier 2: mtime desc.
        let tier = u8::from(!s.was_recently_live);
        (tier, std::cmp::Reverse(s.jsonl_mtime.unwrap_or(zero_ts)))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn paused(id: &str, mtime_offset_secs: i64, snapshot_match: bool) -> LiveSession {
        LiveSession {
            session_id: id.to_string(),
            jsonl_path: Some(PathBuf::from(format!("/tmp/{id}.jsonl"))),
            cwd: PathBuf::from("/tmp"),
            name: None,
            status: None,
            pid: 0,
            started_at: None,
            updated_at: None,
            jsonl_mtime: Some(Utc::now() - chrono::Duration::seconds(mtime_offset_secs)),
            is_live: false,
            was_recently_live: snapshot_match,
        }
    }

    #[test]
    fn mark_was_recently_live_promotes_snapshot_matches() {
        // Three paused sessions; only "in_snapshot" is in the snapshot.
        // Expectation: after marking + sort, snapshot match comes first
        // even though it has the OLDEST mtime.
        let mut paused_list = vec![
            paused("recent_only", 10, false),
            paused("middle", 100, false),
            paused("in_snapshot", 1000, false),
        ];
        let mut snap = super::super::live_snapshot::LiveSnapshot::default();
        let prev_run_stamp = Utc::now() - chrono::Duration::seconds(120);
        snap.sessions.insert(
            "in_snapshot".to_string(),
            super::super::live_snapshot::SnapshotEntry {
                session_id: "in_snapshot".to_string(),
                last_seen: prev_run_stamp,
                cwd: PathBuf::from("/tmp"),
                jsonl_path: Some(PathBuf::from("/tmp/in_snapshot.jsonl")),
                name: None,
            },
        );
        // App started 60s ago; the snapshot stamp is 120s ago (= prior run).
        let app_start_time = Utc::now() - chrono::Duration::seconds(60);

        mark_was_recently_live(&mut paused_list, &snap, app_start_time);

        assert_eq!(paused_list[0].session_id, "in_snapshot");
        assert!(paused_list[0].was_recently_live);
        assert_eq!(paused_list[1].session_id, "recent_only");
        assert!(!paused_list[1].was_recently_live);
    }

    #[test]
    fn mark_skips_entries_stamped_during_current_run() {
        // Manual-kill case: the snapshot has an entry, but it was stamped
        // by the current ccsight run (last_seen > app_start_time). That
        // means ccsight observed this session alive then watched it
        // disappear — not a `⟳` candidate.
        let mut paused_list = vec![paused("manually_closed", 30, false)];
        let mut snap = super::super::live_snapshot::LiveSnapshot::default();
        let app_start_time = Utc::now() - chrono::Duration::seconds(120);
        snap.sessions.insert(
            "manually_closed".to_string(),
            super::super::live_snapshot::SnapshotEntry {
                session_id: "manually_closed".to_string(),
                last_seen: Utc::now() - chrono::Duration::seconds(60),
                cwd: PathBuf::from("/tmp"),
                jsonl_path: Some(PathBuf::from("/tmp/manually_closed.jsonl")),
                name: None,
            },
        );
        mark_was_recently_live(&mut paused_list, &snap, app_start_time);
        assert!(
            !paused_list[0].was_recently_live,
            "session stamped within current run must not be flagged ⟳"
        );
    }

    #[test]
    fn mark_only_flags_latest_run_cluster() {
        // Two prior runs survive in the snapshot:
        //   • Run-1 (older) stamped session "old" at T-3h
        //   • Run-2 (newer) stamped session "recent" at T-1h
        // Only "recent" should carry `⟳ from last ccsight run` — "old"
        // came from an even-older run and isn't the most recent prior
        // ccsight context.
        let mut paused_list = vec![paused("old", 10800, false), paused("recent", 3600, false)];
        let mut snap = super::super::live_snapshot::LiveSnapshot::default();
        let app_start = Utc::now();
        snap.sessions.insert(
            "old".to_string(),
            super::super::live_snapshot::SnapshotEntry {
                session_id: "old".to_string(),
                last_seen: app_start - chrono::Duration::hours(3),
                cwd: PathBuf::from("/tmp"),
                jsonl_path: Some(PathBuf::from("/tmp/old.jsonl")),
                name: None,
            },
        );
        snap.sessions.insert(
            "recent".to_string(),
            super::super::live_snapshot::SnapshotEntry {
                session_id: "recent".to_string(),
                last_seen: app_start - chrono::Duration::hours(1),
                cwd: PathBuf::from("/tmp"),
                jsonl_path: Some(PathBuf::from("/tmp/recent.jsonl")),
                name: None,
            },
        );

        mark_was_recently_live(&mut paused_list, &snap, app_start);

        // recent (matches max_last_seen) → flagged.
        // old (3h ago, outside the 60s cluster window) → not flagged.
        let recent = paused_list
            .iter()
            .find(|p| p.session_id == "recent")
            .unwrap();
        let old = paused_list.iter().find(|p| p.session_id == "old").unwrap();
        assert!(
            recent.was_recently_live,
            "session at max_last_seen should be flagged ⟳"
        );
        assert!(
            !old.was_recently_live,
            "session from an older run (last_seen ≠ max_last_seen) must not be flagged ⟳"
        );
    }

    #[test]
    fn warm_threshold_is_thirty_minutes() {
        // Single source of truth: glyph + sort tier read from this constant.
        // If someone bumps it without updating the help-popup legend, this
        // test keeps the values aligned.
        assert_eq!(WARM_THRESHOLD_SECS, 30 * 60);
    }
}
