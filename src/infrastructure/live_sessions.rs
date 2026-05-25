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

/// True when `t` falls on the same local-calendar date as `now`. Used by
/// the live-session sort tier and the row glyph (`◉` today vs `○` older).
/// Local timezone matches the user's mental model — "did I work on this
/// today?" means today in their wall clock, not UTC.
pub fn is_today(t: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    t.with_timezone(&chrono::Local).date_naive() == now.with_timezone(&chrono::Local).date_naive()
}

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
    // Tier 1: status — busy first, then today (touched today), then older.
    // Tier 2: age descending within each tier.
    let zero_ts = DateTime::<Utc>::UNIX_EPOCH;
    let now = chrono::Utc::now();
    out.sort_by_key(|s| {
        let ts = s
            .jsonl_mtime
            .or(s.updated_at)
            .or(s.started_at)
            .unwrap_or(zero_ts);
        let tier: u8 = if s.status.as_deref() == Some("busy") {
            0
        } else if is_today(ts, now) {
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
    prior_run_anchor: Option<DateTime<Utc>>,
) {
    // Anchor = prior ccsight run's last `refresh()` moment. Sessions alive
    // at that poll share that exact timestamp; mid-prior-run deaths keep
    // their earlier stamp and fall outside the 1s tolerance window.
    let Some(anchor) = prior_run_anchor else {
        return;
    };
    let cluster_floor = anchor - chrono::Duration::seconds(1);

    for entry in paused.iter_mut() {
        if let Some(snap_entry) = snapshot.sessions.get(&entry.session_id) {
            if snap_entry.last_seen >= cluster_floor && snap_entry.last_seen <= anchor {
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
        let anchor = snap.prior_run_anchor();

        mark_was_recently_live(&mut paused_list, &snap, anchor);

        assert_eq!(paused_list[0].session_id, "in_snapshot");
        assert!(paused_list[0].was_recently_live);
        assert_eq!(paused_list[1].session_id, "recent_only");
        assert!(!paused_list[1].was_recently_live);
    }

    #[test]
    fn mark_skips_entries_stamped_after_prior_anchor() {
        // Manual-kill case: snapshot has an entry whose stamp is *newer*
        // than the prior run's heartbeat (= would have been stamped during
        // the current run if marking were re-derived). Anchor is fixed at
        // the prior heartbeat — the entry sits outside the cluster.
        let mut paused_list = vec![paused("manually_closed", 30, false)];
        let mut snap = super::super::live_snapshot::LiveSnapshot::default();
        snap.last_refresh_at = Some(Utc::now() - chrono::Duration::seconds(120));
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
        let anchor = snap.prior_run_anchor();
        mark_was_recently_live(&mut paused_list, &snap, anchor);
        assert!(
            !paused_list[0].was_recently_live,
            "session stamped after the prior heartbeat must not be flagged ⟳"
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

        let anchor = snap.prior_run_anchor();
        mark_was_recently_live(&mut paused_list, &snap, anchor);

        // recent (matches anchor) → flagged.
        // old (3h ago, outside the cluster window) → not flagged.
        let recent = paused_list
            .iter()
            .find(|p| p.session_id == "recent")
            .unwrap();
        let old = paused_list.iter().find(|p| p.session_id == "old").unwrap();
        assert!(
            recent.was_recently_live,
            "session at anchor should be flagged ⟳"
        );
        assert!(
            !old.was_recently_live,
            "session from an older run (last_seen outside anchor cluster) must not be flagged ⟳"
        );
    }

    #[test]
    fn mark_excludes_sessions_that_died_one_poll_before_exit() {
        // A session stamped one poll-cycle before exit (mid-run death) must
        // not enter the cluster — only sessions on the anchor itself count
        // as "alive at exit".
        let mut paused_list = vec![
            paused("survivor", 1, false),
            paused("died_mid_run", 1, false),
        ];
        let mut snap = super::super::live_snapshot::LiveSnapshot::default();
        let app_start = Utc::now();
        let final_poll = app_start - chrono::Duration::seconds(120);
        let previous_poll = final_poll - chrono::Duration::seconds(5);
        snap.sessions.insert(
            "survivor".to_string(),
            super::super::live_snapshot::SnapshotEntry {
                session_id: "survivor".to_string(),
                last_seen: final_poll,
                cwd: PathBuf::from("/tmp"),
                jsonl_path: Some(PathBuf::from("/tmp/survivor.jsonl")),
                name: None,
            },
        );
        snap.sessions.insert(
            "died_mid_run".to_string(),
            super::super::live_snapshot::SnapshotEntry {
                session_id: "died_mid_run".to_string(),
                last_seen: previous_poll,
                cwd: PathBuf::from("/tmp"),
                jsonl_path: Some(PathBuf::from("/tmp/died.jsonl")),
                name: None,
            },
        );

        let anchor = snap.prior_run_anchor();
        mark_was_recently_live(&mut paused_list, &snap, anchor);

        let survivor = paused_list
            .iter()
            .find(|p| p.session_id == "survivor")
            .unwrap();
        let died = paused_list
            .iter()
            .find(|p| p.session_id == "died_mid_run")
            .unwrap();
        assert!(survivor.was_recently_live);
        assert!(
            !died.was_recently_live,
            "died_mid_run must not be flagged — was stamped 5s before the final poll"
        );
    }

    #[test]
    fn recomputing_anchor_after_refresh_corrupts_marking() {
        // Pin the anti-pattern: callers must capture the anchor at boot
        // (into `AppState::prior_run_last_refresh`) and never re-derive it
        // mid-run. After the polling thread's `refresh()`, the snapshot's
        // `last_refresh_at` jumps to the current run, so a recomputed
        // anchor sits on current-run time — every prior session falls out
        // of the cluster and silently loses its `⟳`.
        let prior_exit = Utc::now() - chrono::Duration::minutes(30);
        let mut snap = super::super::live_snapshot::LiveSnapshot::default();
        snap.last_refresh_at = Some(prior_exit);
        snap.sessions.insert(
            "alive_at_prior_exit".to_string(),
            super::super::live_snapshot::SnapshotEntry {
                session_id: "alive_at_prior_exit".to_string(),
                last_seen: prior_exit,
                cwd: PathBuf::from("/tmp"),
                jsonl_path: Some(PathBuf::from("/tmp/alive.jsonl")),
                name: None,
            },
        );

        let frozen_anchor = snap.prior_run_anchor();
        assert_eq!(frozen_anchor, Some(prior_exit));

        snap.refresh(&[]);
        // `prior_run_anchor()`'s primary path needs `last_refresh_at < Utc::now()`.
        // On low-resolution clocks under parallel test load, the back-to-back
        // `now()` calls in refresh + prior_run_anchor can land on the same
        // microsecond, dropping to the fallback. A 1ms sleep gives the clock
        // room to advance so the anti-pattern reliably surfaces.
        std::thread::sleep(std::time::Duration::from_millis(1));
        let stale_anchor = snap.prior_run_anchor();
        assert_ne!(
            stale_anchor, frozen_anchor,
            "refresh must visibly move the (incorrectly) recomputed anchor"
        );

        // Frozen anchor → session correctly flagged ⟳.
        let mut paused_frozen = vec![paused("alive_at_prior_exit", 30, false)];
        mark_was_recently_live(&mut paused_frozen, &snap, frozen_anchor);
        assert!(
            paused_frozen[0].was_recently_live,
            "frozen anchor flags the prior-exit-alive session"
        );

        // Stale anchor → marker silently dropped (the bug we prevent).
        let mut paused_stale = vec![paused("alive_at_prior_exit", 30, false)];
        mark_was_recently_live(&mut paused_stale, &snap, stale_anchor);
        assert!(
            !paused_stale[0].was_recently_live,
            "stale anchor sits on the current run and drops the marker"
        );
    }

    #[test]
    fn is_today_uses_local_calendar_date() {
        use chrono::{Local, TimeZone};
        let now = Local
            .with_ymd_and_hms(2026, 5, 22, 14, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let same_morning = Local
            .with_ymd_and_hms(2026, 5, 22, 0, 30, 0)
            .unwrap()
            .with_timezone(&Utc);
        let yesterday_late = Local
            .with_ymd_and_hms(2026, 5, 21, 23, 59, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert!(super::is_today(same_morning, now));
        assert!(!super::is_today(yesterday_late, now));
    }
}
