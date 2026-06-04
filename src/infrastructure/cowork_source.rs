//! Adapter for Claude Desktop "Cowork" session logs under
//! `~/Library/Application Support/Claude/local-agent-mode-sessions/`
//! (macOS only; empty list on other platforms).
//!
//! ccsight reads `audit.jsonl` for conversation data and joins the
//! sibling `local_<uuid>.json` for title / processName (sandbox `cwd` =
//! `/sessions/<vmname>` is not user-friendly). The schema is unofficial
//! and may change without notice; the reader skips malformed files
//! silently to stay tolerant of upstream churn.
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Per-session metadata read from `local_<uuid>.json`. Only the fields ccsight
/// uses are deserialized; everything else is ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct CoworkMetadata {
    /// Human-readable VM bundle name (e.g. `intelligent-eager-cerf`). Used as
    /// project name because the sandbox `cwd` is `/sessions/<vmname>` which
    /// isn't user-friendly.
    #[serde(rename = "processName")]
    pub process_name: Option<String>,
    /// User-curated session title (e.g. "IT general controls improvement
    /// report"). Maps to `SessionInfo.custom_title`.
    pub title: Option<String>,
    /// Canonical session UUID also recorded as `session_id` on each
    /// audit.jsonl entry. Used for UI display since `audit.jsonl` would
    /// otherwise yield a literal `audit` file stem for every Cowork session.
    #[serde(rename = "cliSessionId", default)]
    pub cli_session_id: Option<String>,
}

/// Returns the directory under which Cowork sessions live, or `None` on
/// platforms / hosts that don't have it.
pub fn cowork_root() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let path =
        PathBuf::from(home).join("Library/Application Support/Claude/local-agent-mode-sessions");
    path.is_dir().then_some(path)
}

/// Walks the Cowork tree and returns every `audit.jsonl` file ccsight can ingest.
/// Returns empty when the root doesn't exist (Linux / WSL2 / fresh machine).
pub fn find_cowork_audit_files() -> Vec<PathBuf> {
    let Some(root) = cowork_root() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk(&root, &mut out, 0);
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    // Layout depth from cowork_root: account/ env/ local_<uuid>/ audit.jsonl ⇒ 4 levels.
    // Cap at 6 to give a bit of leeway for any future nesting Anthropic might add
    // without letting a runaway tree (e.g. a symlink loop) explode.
    const MAX_DEPTH: usize = 6;
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out, depth + 1);
        } else if path.file_name().is_some_and(|n| n == "audit.jsonl") {
            out.push(path);
        }
    }
}

/// Returns true if the given path is inside the Cowork tree. Used by the
/// parser to decide whether to remap `_audit_timestamp` → `timestamp` and by
/// the grouper to fall back to metadata-driven project name / title.
pub fn is_cowork_audit_path(path: &Path) -> bool {
    let Some(root) = cowork_root() else {
        return false;
    };
    path.starts_with(&root)
}

/// Read sibling `local_<uuid>.json` (one dir up, named after the
/// session dir) for an `audit.jsonl`. `None` on read/parse failure —
/// callers fall back to dir-stem defaults.
pub fn read_metadata_for_audit(audit_path: &Path) -> Option<CoworkMetadata> {
    let session_dir = audit_path.parent()?;
    let session_name = session_dir.file_name()?.to_str()?;
    let parent = session_dir.parent()?;
    let metadata_path = parent.join(format!("{session_name}.json"));
    let content = std::fs::read_to_string(&metadata_path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Best-effort project-name fallback when metadata json is missing/unreadable.
/// Uses the stem of the session directory (`local_<uuid>` ⇒ `local_<uuid>`).
pub fn fallback_project_name_from_audit(audit_path: &Path) -> String {
    audit_path
        .parent()
        .and_then(|d| d.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("cowork")
        .to_string()
}

/// User-facing project name. Regular JSONL → in-stream `cwd`; Cowork
/// audit.jsonl → sibling metadata's `processName` (in-stream `cwd` is
/// a meaningless sandbox path). Single source of truth — both cache
/// writer and live grouper route through this.
pub fn resolve_project_name(file: &Path, extracted: Option<String>) -> Option<String> {
    if !is_cowork_audit_path(file) {
        return extracted;
    }
    let meta = read_metadata_for_audit(file);
    meta.as_ref()
        .and_then(|m| m.process_name.clone())
        .or(extracted)
        .or_else(|| Some(fallback_project_name_from_audit(file)))
}

/// Resolve the curated session title for Cowork sessions. Returns `None`
/// for non-cowork files so the regular `CustomTitle` entry path is unaffected.
pub fn resolve_cowork_title(file: &Path) -> Option<String> {
    if !is_cowork_audit_path(file) {
        return None;
    }
    read_metadata_for_audit(file).and_then(|m| m.title)
}

/// For a Cowork audit.jsonl file, return the canonical `cliSessionId` from
/// the sibling metadata json. UI uses this in place of `file_stem()` so each
/// session gets a real per-session identifier instead of the literal string
/// `audit` (the stem of every `audit.jsonl`). Returns `None` for non-cowork
/// files so the regular file-stem path is unaffected.
pub fn cowork_session_id(file: &Path) -> Option<String> {
    if !is_cowork_audit_path(file) {
        return None;
    }
    read_metadata_for_audit(file).and_then(|m| m.cli_session_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn test_walk_finds_audit_jsonl() {
        // Layout: tmp/account/env/local_xxx/audit.jsonl + sibling metadata.
        let tmp = std::env::temp_dir().join(format!("ccsight-cowork-test-{}", std::process::id()));
        let session_dir = tmp.join("acc/env/local_xxx");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(session_dir.join("audit.jsonl"), b"{}\n").unwrap();
        fs::write(
            tmp.join("acc/env/local_xxx.json"),
            r#"{"processName":"happy-fox","title":"Test session"}"#,
        )
        .unwrap();
        // A non-audit file that should NOT be picked up:
        fs::write(session_dir.join("README.md"), b"x").unwrap();

        let mut out = Vec::new();
        walk(&tmp, &mut out, 0);
        assert_eq!(out.len(), 1);
        assert!(out[0].ends_with("audit.jsonl"));

        // Metadata join should locate the sibling json.
        let meta = read_metadata_for_audit(&out[0]).expect("metadata present");
        assert_eq!(meta.process_name.as_deref(), Some("happy-fox"));
        assert_eq!(meta.title.as_deref(), Some("Test session"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_fallback_project_name() {
        let p = PathBuf::from("/tmp/acc/env/local_abc/audit.jsonl");
        assert_eq!(fallback_project_name_from_audit(&p), "local_abc");
    }

    #[test]
    fn test_walk_handles_missing_root() {
        let mut out = Vec::new();
        walk(&PathBuf::from("/nonexistent/path"), &mut out, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn test_metadata_deserializes_cli_session_id() {
        // Regression: UI relies on `cliSessionId` for the displayed short_id
        // because every Cowork audit.jsonl has the same stem (`audit`). The
        // deserializer must pick this up via the camelCase rename.
        let json = r#"{
            "processName": "happy-fox",
            "title": "Demo",
            "cliSessionId": "abc12345-aaaa-bbbb-cccc-ddddeeeeffff"
        }"#;
        let meta: CoworkMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(
            meta.cli_session_id.as_deref(),
            Some("abc12345-aaaa-bbbb-cccc-ddddeeeeffff")
        );
    }
}
