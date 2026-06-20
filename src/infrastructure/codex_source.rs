//! Adapter for OpenAI Codex CLI session logs under `~/.codex/sessions/`
//! and `~/.codex/archived_sessions/`. Mirrors the `cowork_source.rs`
//! pattern: discover files, test paths, resolve project names.

use std::path::{Path, PathBuf};

/// Returns the Codex root directory, or `None` if it doesn't exist.
fn codex_root() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let path = PathBuf::from(home).join(".codex");
    path.is_dir().then_some(path)
}

/// Walks the Codex session trees and returns every `rollout-*.jsonl` file.
pub fn find_codex_session_files() -> Vec<PathBuf> {
    let Some(root) = codex_root() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for subdir in &["sessions", "archived_sessions"] {
        let dir = root.join(subdir);
        if dir.is_dir() {
            walk(&dir, &mut out, 0);
        }
    }
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
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
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
        {
            out.push(path);
        }
    }
}

/// Returns true if the given path is inside `~/.codex/sessions/` or
/// `~/.codex/archived_sessions/`.
pub fn is_codex_path(path: &Path) -> bool {
    let Some(root) = codex_root() else {
        return false;
    };
    path.starts_with(root.join("sessions")) || path.starts_with(root.join("archived_sessions"))
}

/// Extract the session UUID from a Codex rollout filename.
/// `rollout-2026-06-20T09-01-14-019ee254-e2a1-7bb1-9cb3-52b2580549e3.jsonl`
/// → `019ee254-e2a1-7bb1-9cb3-52b2580549e3`
pub fn codex_session_id_from_path(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let stripped = stem.strip_prefix("rollout-")?;
    // The UUID is the last 36 chars (standard 8-4-4-4-12 format)
    if stripped.len() >= 36 {
        let uuid_part = &stripped[stripped.len() - 36..];
        if uuid_part.chars().filter(|&c| c == '-').count() == 4 {
            return Some(uuid_part.to_string());
        }
    }
    None
}

/// Resolve the project name for a Codex session. Falls back to the cwd
/// extracted from the session entries, keeping only the last path component.
pub fn resolve_codex_project_name(path: &Path, extracted: Option<String>) -> Option<String> {
    if !is_codex_path(path) {
        return extracted;
    }
    Some(
        extracted
            .and_then(|cwd| {
                Path::new(&cwd)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "Codex".to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_walk_finds_rollout_jsonl() {
        let tmp = std::env::temp_dir().join(format!("ccsight-codex-test-{}", std::process::id()));
        let session_dir = tmp.join("sessions/2026/06/20");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(
            session_dir
                .join("rollout-2026-06-20T09-01-14-019ee254-e2a1-7bb1-9cb3-52b2580549e3.jsonl"),
            b"{}\n",
        )
        .unwrap();
        fs::write(session_dir.join("README.md"), b"x").unwrap();

        let mut out = Vec::new();
        walk(&tmp, &mut out, 0);
        assert_eq!(out.len(), 1);
        assert!(
            out[0]
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("rollout-")
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_walk_handles_missing_root() {
        let mut out = Vec::new();
        walk(&PathBuf::from("/nonexistent/path"), &mut out, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn test_session_id_from_path() {
        let path =
            PathBuf::from("rollout-2026-06-20T09-01-14-019ee254-e2a1-7bb1-9cb3-52b2580549e3.jsonl");
        assert_eq!(
            codex_session_id_from_path(&path).as_deref(),
            Some("019ee254-e2a1-7bb1-9cb3-52b2580549e3"),
        );
    }

    #[test]
    fn test_session_id_from_path_no_uuid() {
        let path = PathBuf::from("rollout-short.jsonl");
        assert_eq!(codex_session_id_from_path(&path), None);
    }
}
