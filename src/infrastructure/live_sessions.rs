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
    /// Paused-only signal: this session_id appears in the persisted
    /// snapshot of sessions observed alive prior to this ccsight run.
    /// Powers the post-restart "I had this open before reboot" hint via
    /// a `⟳` glyph.
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

/// Read the launch `cwd` from a session JSONL — recorded on message entries,
/// not the leading summary line, so it surfaces within the first few lines.
/// Returns the FIRST match (the session's project dir = the right `cd` target
/// for resume) — deliberately not the reverse-walk last value. `MAX_LINES`
/// bounds the probe; a session with no message yet yields `None`.
pub fn read_cwd_from_jsonl(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    const MAX_LINES: usize = 100;
    let file = std::fs::File::open(path).ok()?;
    for line in BufReader::new(file)
        .lines()
        .take(MAX_LINES)
        .map_while(Result::ok)
    {
        if !line.contains("\"cwd\"") {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
            // Claude Code: top-level `cwd`; Codex CLI: `payload.cwd`
            let cwd = v
                .get("cwd")
                .or_else(|| v.get("payload").and_then(|p| p.get("cwd")))
                .and_then(|c| c.as_str());
            if let Some(cwd) = cwd {
                if !cwd.is_empty() {
                    return Some(cwd.to_string());
                }
            }
        }
    }
    None
}

/// Scan `~/.claude/projects/**/*.jsonl` and `~/.codex/sessions/**/*.jsonl`
/// for sessions whose mtime is within `window` of `now` and whose UUID is
/// *not* in `active_ids`. The returned sessions carry minimal metadata:
/// cwd / name / status are unknown from a JSONL alone (callers can enrich
/// from `state.daily_groups`).
pub fn discover_recently_paused(
    active_ids: &std::collections::HashSet<String>,
    window: Duration,
    now: SystemTime,
) -> Vec<LiveSession> {
    let cutoff = now - window;
    let mut out = Vec::new();

    // Claude Code sessions
    if let Some(projects) = claude_projects_dir() {
        if let Ok(project_entries) = std::fs::read_dir(&projects) {
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
                    if stem.starts_with("agent-") {
                        continue;
                    }
                    if active_ids.contains(&stem) {
                        continue;
                    }
                    let Ok(modified) = std::fs::metadata(&jsonl_path).and_then(|m| m.modified())
                    else {
                        continue;
                    };
                    if modified < cutoff {
                        continue;
                    }
                    let jsonl_mtime = mtime_utc(&jsonl_path);
                    let cwd = read_cwd_from_jsonl(&jsonl_path).map_or_else(
                        || {
                            let dir_name = project_path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("");
                            PathBuf::from(dir_name.replace('-', "/"))
                        },
                        PathBuf::from,
                    );
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
        }
    }

    // Codex CLI sessions
    collect_codex_paused(&mut out, active_ids, cutoff);

    out.sort_by_key(|s| std::cmp::Reverse(s.jsonl_mtime.unwrap_or(DateTime::<Utc>::UNIX_EPOCH)));
    out
}

fn collect_codex_paused(
    out: &mut Vec<LiveSession>,
    active_ids: &std::collections::HashSet<String>,
    cutoff: SystemTime,
) {
    let codex_files = super::codex_source::find_codex_session_files();
    for jsonl_path in codex_files {
        let session_id =
            super::codex_source::codex_session_id_from_path(&jsonl_path).unwrap_or_default();
        if session_id.is_empty() || active_ids.contains(&session_id) {
            continue;
        }
        let Ok(modified) = std::fs::metadata(&jsonl_path).and_then(|m| m.modified()) else {
            continue;
        };
        if modified < cutoff {
            continue;
        }
        let jsonl_mtime = mtime_utc(&jsonl_path);
        let cwd =
            read_cwd_from_jsonl(&jsonl_path).map_or_else(|| PathBuf::from("Codex"), PathBuf::from);
        out.push(LiveSession {
            session_id,
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

/// Mark paused entries that were alive in the prior run's final snapshot
/// (`prior_alive`, frozen at boot), then re-sort so restorable rows float
/// to the top. `prior_alive` is passed in because the snapshot reader and
/// the boot-freeze both live outside this module.
pub fn mark_was_recently_live(
    paused: &mut [LiveSession],
    prior_alive: &std::collections::HashMap<String, super::live_snapshots::LiveSnapshotEntry>,
) {
    for entry in paused.iter_mut() {
        if prior_alive.contains_key(&entry.session_id) {
            entry.was_recently_live = true;
        }
    }
    let zero_ts = DateTime::<Utc>::UNIX_EPOCH;
    // Tier 1: was_recently_live first so restorable hints stay at the top
    // regardless of JSONL mtime. Tier 2: mtime desc. Tier 3: session_id —
    // recovered rows arrive in nondeterministic HashMap order and share a
    // tier (often with tied/None mtime), so without this tiebreaker they
    // shuffle between frames (render-stable sort, lint #30).
    paused.sort_by(|a, b| {
        u8::from(!a.was_recently_live)
            .cmp(&u8::from(!b.was_recently_live))
            .then_with(|| {
                b.jsonl_mtime
                    .unwrap_or(zero_ts)
                    .cmp(&a.jsonl_mtime.unwrap_or(zero_ts))
            })
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn read_cwd_from_jsonl_returns_authoritative_path_with_literal_dash() {
        // The real cwd has a literal `-` (multi-word-dir). Reading it
        // from the JSONL `cwd` field must return it verbatim — the lossy
        // slug reversal would have collapsed it to `multi/word/dir`.
        let tmpdir = std::env::temp_dir().join(format!("ccsight_cwd_{}", std::process::id()));
        std::fs::create_dir_all(&tmpdir).unwrap();
        let jp = tmpdir.join("session.jsonl");
        // Line 1: summary with no cwd (mirrors Claude Code's first line).
        // Line 2: a message entry carrying the authoritative cwd.
        std::fs::write(
            &jp,
            "{\"type\":\"summary\",\"summary\":\"x\"}\n\
             {\"type\":\"user\",\"cwd\":\"/Users/me/dev/multi-word-dir\"}\n",
        )
        .unwrap();
        assert_eq!(
            read_cwd_from_jsonl(&jp).as_deref(),
            Some("/Users/me/dev/multi-word-dir")
        );
        let _ = std::fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn read_cwd_from_jsonl_none_when_absent() {
        let tmpdir = std::env::temp_dir().join(format!("ccsight_cwd_none_{}", std::process::id()));
        std::fs::create_dir_all(&tmpdir).unwrap();
        let jp = tmpdir.join("nocwd.jsonl");
        std::fs::write(&jp, "{\"type\":\"summary\"}\n{\"type\":\"user\"}\n").unwrap();
        assert!(read_cwd_from_jsonl(&jp).is_none());
        let _ = std::fs::remove_dir_all(&tmpdir);
    }

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
    fn mark_sorts_restorable_rows_above_non_restorable_regardless_of_mtime() {
        // Two paused rows with `was_recently_live` pre-set to simulate the
        // outcome of the daily-snapshot lookup. The sort tier must put
        // restorable first even when the non-restorable row has fresher
        // mtime.
        let mut paused_list = vec![
            paused("fresh_non_restorable", 10, false),
            paused("old_restorable", 1000, true),
        ];
        // Empty prior_alive — the rows already carry `was_recently_live`,
        // so we only verify the sort tier honors the pre-set flag.
        let prior_alive = std::collections::HashMap::new();
        mark_was_recently_live(&mut paused_list, &prior_alive);
        assert_eq!(paused_list[0].session_id, "old_restorable");
        assert!(paused_list[0].was_recently_live);
        assert_eq!(paused_list[1].session_id, "fresh_non_restorable");
    }

    #[test]
    fn mark_flags_only_sessions_in_prior_alive() {
        use super::super::live_snapshots::LiveSnapshotEntry;
        // Two paused rows; only "in_prior" is in the frozen prior-alive set.
        // It must get the ⟳ flag (and float to top); the other must not.
        let mut paused_list = vec![
            paused("recent_other", 10, false),
            paused("in_prior", 999, false),
        ];
        let mut prior = std::collections::HashMap::new();
        prior.insert(
            "in_prior".to_string(),
            LiveSnapshotEntry {
                session_id: "in_prior".to_string(),
                cwd: PathBuf::from("/tmp"),
                jsonl_path: Some(PathBuf::from("/tmp/in_prior.jsonl")),
                name: None,
            },
        );
        mark_was_recently_live(&mut paused_list, &prior);
        assert_eq!(paused_list[0].session_id, "in_prior");
        assert!(paused_list[0].was_recently_live);
        assert!(
            !paused_list[1].was_recently_live,
            "session absent from prior_alive must not be ⟳"
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
