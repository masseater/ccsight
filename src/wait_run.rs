//! `--wait <session>`: scoped one-shot wait (binary-only). Polls a single
//! session until it needs attention (awaiting input / stalled / error /
//! exited mid-task) or has ended, prints one line to stdout, and exits so
//! shell composition works: `ccsight --wait <id> && notify`. No persisted
//! state, no re-alerts — one transition, one line, done.

use std::path::{Path, PathBuf};

use crate::session_state::{SessionState, live_state, reason_for};

const POLL_SECS: u64 = 5;
/// Idle window before a finished / stalled turn fires. Short by design: the
/// caller opted into watching this one session, so responsiveness beats the
/// re-alert hygiene a broad watcher would need.
const IDLE_THRESHOLD_SECS: u64 = 30;

/// Resolve a session id (or unique prefix) to its JSONL under `root`
/// (`~/.claude/projects`). Cowork audit logs are skipped: their stems are not
/// session ids and the sandbox VM can't be waited on from the local CLI.
fn resolve_session(root: &Path, prefix: &str) -> Result<PathBuf, String> {
    let mut matches: Vec<PathBuf> = Vec::new();
    let Ok(projects) = std::fs::read_dir(root) else {
        return Err(format!("no projects directory at {}", root.display()));
    };
    for project in projects.flatten() {
        let dir = project.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl")
                || crate::infrastructure::is_cowork_audit_path(&path)
            {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            // Subagent transcripts share the projects tree but are not
            // resumable sessions — never resolve (or ambiguate) onto one.
            if stem.starts_with("agent-") {
                continue;
            }
            if stem == prefix {
                return Ok(path);
            }
            if stem.starts_with(prefix) {
                matches.push(path);
            }
        }
    }
    match matches.len() {
        0 => Err(format!("no session matches '{prefix}'")),
        1 => Ok(matches.remove(0)),
        n => {
            let mut stems: Vec<String> = matches
                .iter()
                .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
                .map(String::from)
                .collect();
            stems.sort();
            stems.truncate(5);
            Err(format!(
                "'{prefix}' is ambiguous ({n} sessions): {}…",
                stems.join(", ")
            ))
        }
    }
}

/// One poll's verdict: `Some((label, reason))` fires and ends the wait.
/// A dead process with a cleanly-ended tail classifies as `Active`, but for a
/// scoped wait "not running any more" IS the news — report `ended` instead of
/// waiting forever.
fn verdict(
    alive: bool,
    busy: bool,
    idle_secs: u64,
    jsonl_path: &Path,
) -> Option<(&'static str, String)> {
    // Busy or fresh mtime → still working; defer WITHOUT reading the tail.
    // The fresh-mtime defer applies to dead sessions too: it absorbs the
    // startup race where the wait begins before the pid file appears
    // (alive reads false while the transcript is seconds old).
    if busy || idle_secs < IDLE_THRESHOLD_SECS {
        return None;
    }
    let (state, tail) = live_state(
        alive,
        busy,
        idle_secs,
        IDLE_THRESHOLD_SECS,
        Some(jsonl_path),
    );
    if state != SessionState::Active {
        return Some((state.as_str(), reason_for(state, tail, idle_secs)));
    }
    if !alive {
        return Some((
            "ended",
            "session is not running; last turn ended cleanly".to_string(),
        ));
    }
    None
}

/// One full poll: a vanished transcript (retention cleanup, manual delete)
/// means there is nothing left to wait on, so it fires `ended` rather than
/// polling a nonexistent file forever.
fn poll_verdict(alive: bool, busy: bool, jsonl_path: &Path) -> Option<(&'static str, String)> {
    let Some(idle_secs) = idle_secs_of(jsonl_path) else {
        return Some(("ended", "session file no longer exists".to_string()));
    };
    verdict(alive, busy, idle_secs, jsonl_path)
}

/// Look up the session's live process (pid verified) and busy flag.
fn probe_live(session_id: &str) -> (bool, bool) {
    let live = crate::infrastructure::live_sessions::discover_live();
    live.iter()
        .find(|s| s.session_id == session_id)
        .map_or((false, false), |s| {
            (s.is_live, s.status.as_deref() == Some("busy"))
        })
}

/// `None` when the file is gone (stat failed); a clock skew that puts the
/// mtime in the future reads as 0 (fresh).
fn idle_secs_of(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    Some(
        std::time::SystemTime::now()
            .duration_since(modified)
            .map_or(0, |d| d.as_secs()),
    )
}

/// The `--wait` entry point. Exit codes: 0 = fired (printed one line),
/// 2 = the session id didn't resolve.
pub(crate) fn run_wait(prefix: &str) -> std::io::Result<()> {
    let root = crate::infrastructure::home_dir()
        .map(|h| h.join(".claude").join("projects"))
        .unwrap_or_default();
    let jsonl = match resolve_session(&root, prefix) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ccsight --wait: {e}");
            std::process::exit(2);
        }
    };
    let session_id = jsonl
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(prefix)
        .to_string();

    loop {
        let (alive, busy) = probe_live(&session_id);
        if let Some((label, reason)) = poll_verdict(alive, busy, &jsonl) {
            println!("{label}\t{session_id}\t{reason}");
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(POLL_SECS));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_jsonl(dir: &Path, project: &str, stem: &str, tail: &str) -> PathBuf {
        let pdir = dir.join(project);
        std::fs::create_dir_all(&pdir).unwrap();
        let path = pdir.join(format!("{stem}.jsonl"));
        std::fs::write(&path, format!("{tail}\n")).unwrap();
        path
    }

    const ASSISTANT_END: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#;
    const TOOL_PENDING: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{}}]}}"#;

    #[test]
    fn resolve_finds_unique_prefix_and_rejects_ambiguous_or_missing() {
        let root = std::env::temp_dir().join(format!("ccsight-wait-res-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write_jsonl(&root, "proj-a", "aaaa1111-0000", ASSISTANT_END);
        write_jsonl(&root, "proj-b", "aaaa2222-0000", ASSISTANT_END);
        write_jsonl(&root, "proj-b", "bbbb3333-0000", ASSISTANT_END);

        // Unique prefix resolves across project dirs.
        let hit = resolve_session(&root, "bbbb").unwrap();
        assert!(hit.ends_with("proj-b/bbbb3333-0000.jsonl"));
        // Exact id wins immediately.
        assert!(resolve_session(&root, "aaaa1111-0000").is_ok());
        // Shared prefix is ambiguous; the error names the candidates.
        let err = resolve_session(&root, "aaaa").unwrap_err();
        assert!(err.contains("ambiguous"), "{err}");
        assert!(err.contains("aaaa1111-0000"), "{err}");
        // No match.
        assert!(resolve_session(&root, "zzzz").is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn verdict_decision_table() {
        let root = std::env::temp_dir().join(format!("ccsight-wait-v-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ended = write_jsonl(&root, "p", "s1", ASSISTANT_END);
        let pending = write_jsonl(&root, "p", "s2", TOOL_PENDING);

        // Alive + busy → keep waiting regardless of idle.
        assert!(verdict(true, true, 9999, &ended).is_none());
        // Alive + fresh mtime → keep waiting.
        assert!(verdict(true, false, IDLE_THRESHOLD_SECS - 1, &ended).is_none());
        // Alive + idle + turn ended → awaiting_input fires.
        let (label, reason) = verdict(true, false, IDLE_THRESHOLD_SECS, &ended).unwrap();
        assert_eq!(label, "awaiting_input");
        assert!(!reason.is_empty());
        // Dead + mid-task tail → exited fires.
        let (label, _) = verdict(false, false, IDLE_THRESHOLD_SECS, &pending).unwrap();
        assert_eq!(label, "exited");
        // Dead + clean end → `ended` (a scoped wait must not wait forever).
        let (label, _) = verdict(false, false, IDLE_THRESHOLD_SECS, &ended).unwrap();
        assert_eq!(label, "ended");
        // Dead + fresh mtime → defer for EVERY tail, including mid-task ones:
        // this is the pid-file startup race, not a crashed session.
        assert!(verdict(false, false, IDLE_THRESHOLD_SECS - 1, &ended).is_none());
        assert!(verdict(false, false, IDLE_THRESHOLD_SECS - 1, &pending).is_none());
        // Busy / fresh defers must not touch the file at all.
        let gone = root.join("p").join("missing.jsonl");
        assert!(verdict(true, true, 9999, &gone).is_none());
        assert!(verdict(false, false, 0, &gone).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn poll_verdict_fires_ended_when_the_transcript_vanishes() {
        let root = std::env::temp_dir().join(format!("ccsight-wait-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let path = write_jsonl(&root, "p", "s1", ASSISTANT_END);
        std::fs::remove_file(&path).unwrap();
        let (label, reason) = poll_verdict(false, false, &path).unwrap();
        assert_eq!(label, "ended");
        assert!(reason.contains("no longer exists"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resolve_skips_subagent_transcripts() {
        let root = std::env::temp_dir().join(format!("ccsight-wait-ag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write_jsonl(&root, "p", "agent-abc1234", ASSISTANT_END);
        write_jsonl(&root, "p", "abc9999-0000", ASSISTANT_END);
        // agent-* is never a target...
        assert!(resolve_session(&root, "agent-abc").is_err());
        // ...and never makes a real prefix ambiguous.
        let hit = resolve_session(&root, "a").unwrap();
        assert!(hit.ends_with("p/abc9999-0000.jsonl"));
        std::fs::remove_dir_all(&root).ok();
    }
}
