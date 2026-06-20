use std::path::PathBuf;

use anyhow::Result;
use glob::glob;
use serde::Deserialize;

pub struct FileDiscovery;

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ClaudeSettings {
    #[serde(default)]
    cleanup_period_days: Option<u32>,
}

pub struct RetentionWarning {
    pub days: u32,
    pub is_default: bool,
}

pub fn check_cleanup_period() -> Option<RetentionWarning> {
    let home = home::home_dir()?;
    let settings_path = home.join(".claude/settings.json");

    if !settings_path.exists() {
        return Some(RetentionWarning {
            days: 30,
            is_default: true,
        });
    }

    let content = std::fs::read_to_string(&settings_path).ok()?;
    let settings: ClaudeSettings = serde_json::from_str(&content).unwrap_or_default();

    match settings.cleanup_period_days {
        Some(days) if days <= 30 => Some(RetentionWarning {
            days,
            is_default: false,
        }),
        None => Some(RetentionWarning {
            days: 30,
            is_default: true,
        }),
        _ => None,
    }
}

impl FileDiscovery {
    pub fn find_jsonl_files_with_limit(limit: usize) -> Result<Vec<PathBuf>> {
        let home =
            home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;

        let claude_projects = home.join(".claude/projects");

        // `**/*.jsonl` (globstar) reaches sessions nested under per-project
        // subdirs — Claude Code stores subagent transcripts at
        // `~/.claude/projects/<project>/<session>/subagents/agent-*.jsonl`.
        // A single-level `*/*.jsonl` pattern silently skips them, which
        // can materially undercount tokens / cost for heavy subagent users.
        let mut files: Vec<PathBuf> = if claude_projects.exists() {
            let pattern = claude_projects.join("**/*.jsonl");
            let pattern_str = pattern.to_string_lossy();
            glob(&pattern_str)?
                .filter_map(std::result::Result::ok)
                .collect()
        } else {
            Vec::new()
        };

        // External session logs (Cowork / Codex) live outside ~/.claude/
        // and use different JSONL schemas; per-source parsers handle the
        // translation. Empty on platforms without the respective dirs.
        files.extend(super::cowork_source::find_cowork_audit_files());
        files.extend(super::codex_source::find_codex_session_files());

        files.sort_by(|a, b| {
            let a_modified = a.metadata().and_then(|m| m.modified()).ok();
            let b_modified = b.metadata().and_then(|m| m.modified()).ok();
            b_modified.cmp(&a_modified)
        });

        if limit > 0 && files.len() > limit {
            files.truncate(limit);
        }

        Ok(files)
    }
}

/// Local stub for home directory lookup — avoids pulling in the `dirs` crate.
mod home {
    use std::path::PathBuf;

    pub fn home_dir() -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}
