//! Discovery of configured Skills / Commands / Subagents.
//!
//! Mirrors `mcp_config.rs` but for the three other tool categories. Reads:
//!   1. User-level: `~/.claude/{skills,commands,agents}/`
//!   2. Plugin-level: every enabled plugin's `<installPath>/{skills,commands,agents}/`
//!
//! Returned names are **bare** (no `skill:` / `command:` / `agent:` prefix) and
//! match the discriminator used inside the storage keys
//! (`skill:<name>` / `command:<name>` / `agent:<name>`). For plugin-provided
//! resources, names are namespaced as `<plugin>:<resource>` to align with the
//! runtime form Claude Code emits.
//!
//! The Tools detail popup uses these sets to surface "configured but never
//! invoked" entries as zero-call rows so users can see the full menu of
//! available tooling at a glance — matching how MCP servers are rendered.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct ConfiguredResources {
    pub skills: HashSet<String>,
    pub commands: HashSet<String>,
    pub agents: HashSet<String>,
}

/// Discover every configured Skill, Command, Subagent across user and plugin
/// scopes. Quiet on every error (missing files, permission errors) — discovery
/// is best-effort and should never block the UI.
pub fn discover_configured_resources() -> ConfiguredResources {
    let mut out = ConfiguredResources::default();

    let Some(home) = crate::infrastructure::home_dir() else {
        return out;
    };

    // (1) User scope.
    collect_from_root(&home.join(".claude"), None, &mut out);

    // (2) Plugin scope.
    let settings_path = home.join(".claude/settings.json");
    let installed_path = home.join(".claude/plugins/installed_plugins.json");

    let enabled: Vec<String> = fs::read_to_string(&settings_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("enabledPlugins").cloned())
        .and_then(|v| v.as_object().cloned())
        .map(|obj| {
            obj.iter()
                .filter(|(_, v)| v.as_bool().unwrap_or(false))
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default();

    let installed: serde_json::Map<String, serde_json::Value> = fs::read_to_string(&installed_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("plugins").cloned())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    for plug in &enabled {
        let plugin_name = plug.split('@').next().unwrap_or(plug).to_string();
        // A plugin can have multiple install records (different versions
        // installed side-by-side, marketplace re-syncs). `mcp_config.rs`
        // walks all of them; mirror that here so a skill / command / agent
        // declared in any record is discovered.
        let Some(records) = installed.get(plug).and_then(|v| v.as_array()) else {
            continue;
        };
        for record in records {
            let Some(install_path) = record.get("installPath").and_then(|v| v.as_str()) else {
                continue;
            };
            collect_from_root(Path::new(install_path), Some(&plugin_name), &mut out);
        }
    }

    out
}

fn collect_from_root(root: &Path, plugin_namespace: Option<&str>, out: &mut ConfiguredResources) {
    collect_skills(root, plugin_namespace, &mut out.skills);
    collect_flat(&root.join("commands"), plugin_namespace, &mut out.commands);
    collect_flat(&root.join("agents"), plugin_namespace, &mut out.agents);
}

/// Skills are directories: `<root>/skills/<name>/SKILL.md`. We require the
/// `SKILL.md` (or `skill.md`) marker so unrelated subdirectories don't pollute
/// the set.
fn collect_skills(root: &Path, plugin_namespace: Option<&str>, dest: &mut HashSet<String>) {
    let dir = root.join("skills");
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let has_marker = path.join("SKILL.md").is_file() || path.join("skill.md").is_file();
        if !has_marker {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let key = match plugin_namespace {
            Some(plug) => format!("{plug}:{name}"),
            None => name.to_string(),
        };
        dest.insert(key);
    }
}

/// Commands and agents are flat: `<root>/<dir>/<name>.<ext>`. Both `.md` and
/// `.toml` count (older command format used `.toml`).
fn collect_flat(dir: &Path, plugin_namespace: Option<&str>, dest: &mut HashSet<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
            continue;
        };
        if ext != "md" && ext != "toml" {
            continue;
        }
        let key = match plugin_namespace {
            Some(plug) => format!("{plug}:{stem}"),
            None => stem.to_string(),
        };
        dest.insert(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static UNIQ: AtomicUsize = AtomicUsize::new(0);

    fn unique_root(label: &str) -> PathBuf {
        let n = UNIQ.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let dir = env::temp_dir().join(format!("ccsight-resource-test-{pid}-{n}-{label}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn collect_skills_requires_marker_file() {
        let root = unique_root("skills-marker");
        let skills_dir = root.join("skills");
        fs::create_dir_all(skills_dir.join("real-skill")).unwrap();
        fs::write(skills_dir.join("real-skill/SKILL.md"), "x").unwrap();
        fs::create_dir_all(skills_dir.join("not-a-skill")).unwrap();

        let mut dest = HashSet::new();
        collect_skills(&root, None, &mut dest);
        assert!(dest.contains("real-skill"));
        assert!(!dest.contains("not-a-skill"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn collect_skills_namespaces_plugin_entries() {
        let root = unique_root("skills-plug");
        let skills_dir = root.join("skills");
        fs::create_dir_all(skills_dir.join("foo")).unwrap();
        fs::write(skills_dir.join("foo/SKILL.md"), "x").unwrap();

        let mut dest = HashSet::new();
        collect_skills(&root, Some("my-plugin"), &mut dest);
        assert!(dest.contains("my-plugin:foo"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn collect_flat_picks_up_md_and_toml() {
        let root = unique_root("commands-ext");
        let dir = root.join("commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("md-cmd.md"), "x").unwrap();
        fs::write(dir.join("toml-cmd.toml"), "x").unwrap();
        fs::write(dir.join("ignored.txt"), "x").unwrap();

        let mut dest = HashSet::new();
        collect_flat(&dir, None, &mut dest);
        assert!(dest.contains("md-cmd"));
        assert!(dest.contains("toml-cmd"));
        assert!(!dest.contains("ignored"));
        let _ = fs::remove_dir_all(&root);
    }
}
