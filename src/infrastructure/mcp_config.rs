use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::aggregator::{DailyGroup, mcp_server_of};

/// Represents an MCP server configured in Claude Code.
#[derive(Debug, Clone)]
pub struct McpServerStatus {
    pub name: String,
    pub configured: bool,
    pub last_used: Option<DateTime<Utc>>,
    pub total_calls: usize,
}

impl McpServerStatus {
    pub fn days_since_last_use(&self, now: DateTime<Utc>) -> Option<i64> {
        self.last_used.map(|t| (now - t).num_days())
    }

    pub fn is_underutilized(&self, now: DateTime<Utc>, threshold_days: i64) -> bool {
        if !self.configured {
            return false;
        }
        match self.days_since_last_use(now) {
            None => true,
            Some(days) => days >= threshold_days,
        }
    }
}

/// Read the union of MCP server names that Claude Code currently considers
/// "configured":
///
/// 1. **Global user MCP servers** — `~/.claude.json::mcpServers` keys.
/// 2. **Plugin-provided MCP servers** — every enabled plugin in
///    `~/.claude/settings.json::enabledPlugins` contributes the servers
///    declared in its bundled `.mcp.json` / `.claude-plugin/plugin.json`,
///    namespaced as `<plugin>/<server>` to match the runtime form ccsight
///    extracts in `aggregator/tool_category::mcp_server_of`.
///
/// Without (2), plugin-installed servers (e.g. an internal company plugin
/// providing 15 MCP servers) get incorrectly flagged as "inactive (not in
/// global config)" when in fact they're loaded by the plugin layer.
///
/// Project-scope `.mcp.json` and per-project `.claude/settings.json` are
/// intentionally NOT consulted — see Tools popup design notes for rationale.
pub fn read_configured_mcp_servers() -> HashSet<String> {
    let mut servers = read_global_mcp_servers();
    servers.extend(read_plugin_mcp_servers());
    servers
}

fn read_global_mcp_servers() -> HashSet<String> {
    let Some(path) = claude_config_path() else {
        return HashSet::new();
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return HashSet::new();
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) else {
        return HashSet::new();
    };
    parsed
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default()
}

/// Walk every enabled plugin and collect its bundled MCP server names,
/// returning `<plugin-name>/<server-name>` strings to match the runtime form.
/// Empty set when settings or installed_plugins.json are missing.
fn read_plugin_mcp_servers() -> HashSet<String> {
    let Ok(home) = std::env::var("HOME") else {
        return HashSet::new();
    };
    let home = PathBuf::from(home);
    let settings_path = home.join(".claude/settings.json");
    let installed_path = home.join(".claude/plugins/installed_plugins.json");

    // enabledPlugins is `{ "<plugin>@<marketplace>": true|false }`.
    let enabled: Vec<String> = std::fs::read_to_string(&settings_path)
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

    let installed: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&installed_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("plugins").cloned())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    let mut result = HashSet::new();
    for key in &enabled {
        let plugin_name = key.split('@').next().unwrap_or(key);
        let Some(records) = installed.get(key).and_then(|v| v.as_array()) else {
            continue;
        };
        for record in records {
            let Some(install_path) = record
                .get("installPath")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
            else {
                continue;
            };
            for server in plugin_servers_from_install_path(&install_path) {
                // Single source of truth for the compression rule: see
                // `aggregator::tool_category::plugin_server_key` doc comment.
                result.insert(crate::aggregator::plugin_server_key(plugin_name, &server));
            }
        }
    }
    result
}

/// Read server names from a plugin's `.mcp.json` and `.claude-plugin/plugin.json`
/// (inline `mcpServers` form). Both are optional; either may be present.
fn plugin_servers_from_install_path(install_path: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    let mcp_json = install_path.join(".mcp.json");
    if let Ok(content) = std::fs::read_to_string(&mcp_json)
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&content)
        && let Some(obj) = v.get("mcpServers").and_then(|v| v.as_object())
    {
        for k in obj.keys() {
            out.insert(k.clone());
        }
    }
    let plugin_json = install_path.join(".claude-plugin/plugin.json");
    if let Ok(content) = std::fs::read_to_string(&plugin_json)
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&content)
        && let Some(obj) = v.get("mcpServers").and_then(|v| v.as_object())
    {
        for k in obj.keys() {
            out.insert(k.clone());
        }
    }
    out
}

fn claude_config_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".claude.json"))
}

/// Compute MCP server usage status by joining configured servers with observed tool_usage.
pub fn compute_mcp_status(daily_groups: &[DailyGroup]) -> Vec<McpServerStatus> {
    use std::collections::HashMap;

    let configured = read_configured_mcp_servers();

    // Aggregate per-server last_used + total_calls from daily_groups
    let mut last_used_map: HashMap<String, DateTime<Utc>> = HashMap::new();
    let mut total_calls_map: HashMap<String, usize> = HashMap::new();

    for group in daily_groups {
        for session in &group.sessions {
            if session.is_subagent {
                continue;
            }
            let last_ts = session.day_last_timestamp;
            for (tool_name, count) in &session.day_tool_usage {
                let Some(server) = mcp_server_of(tool_name) else {
                    continue;
                };
                *total_calls_map.entry(server.clone()).or_insert(0) += count;
                let entry = last_used_map.entry(server).or_insert(last_ts);
                if last_ts > *entry {
                    *entry = last_ts;
                }
            }
        }
    }

    let mut all_servers: HashSet<String> = configured.iter().cloned().collect();
    all_servers.extend(total_calls_map.keys().cloned());

    let mut statuses: Vec<McpServerStatus> = all_servers
        .into_iter()
        .map(|name| {
            let configured = configured.contains(&name);
            let last_used = last_used_map.get(&name).copied();
            let total_calls = total_calls_map.get(&name).copied().unwrap_or(0);
            McpServerStatus {
                name,
                configured,
                last_used,
                total_calls,
            }
        })
        .collect();

    // Sort by total_calls descending so the most-used servers come first.
    // Underutilized servers (zero or stale calls) naturally end up at the bottom.
    // Tiebreak on name to keep equal-call servers in a stable order across rebuilds
    // (HashMap iteration order is non-deterministic, so without a tiebreak the UI
    // would shuffle them on every redraw).
    statuses.sort_by(|a, b| b.total_calls.cmp(&a.total_calls).then_with(|| a.name.cmp(&b.name)));
    statuses
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    #[test]
    fn test_underutilized_never_used() {
        let s = McpServerStatus {
            name: "foo".to_string(),
            configured: true,
            last_used: None,
            total_calls: 0,
        };
        assert!(s.is_underutilized(Utc::now(), 30));
    }

    #[test]
    fn test_plugin_servers_from_install_path_reads_dot_mcp_json() {
        // Plugins surface their MCP servers via `<install>/.mcp.json::mcpServers`.
        // Without recognising this, plugin-installed servers (e.g. an internal
        // company plugin shipping 15 servers) get incorrectly flagged as
        // "inactive" in the Tools detail popup.
        let tmp = std::env::temp_dir().join(format!("ccsight-plugin-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join(".mcp.json"),
            r#"{"mcpServers":{"server1":{"command":"x"},"server2":{"command":"y"}}}"#,
        )
        .unwrap();
        let names = plugin_servers_from_install_path(&tmp);
        assert!(names.contains("server1"));
        assert!(names.contains("server2"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_plugin_servers_from_install_path_reads_inline_plugin_json() {
        // Some plugins inline `mcpServers` in `.claude-plugin/plugin.json`
        // instead of providing a separate `.mcp.json`. Both forms must be
        // honoured (per the official plugins reference).
        let tmp = std::env::temp_dir()
            .join(format!("ccsight-plugin-inline-test-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join(".claude-plugin")).unwrap();
        std::fs::write(
            tmp.join(".claude-plugin/plugin.json"),
            r#"{"name":"my-plugin","mcpServers":{"alpha":{"command":"x"}}}"#,
        )
        .unwrap();
        let names = plugin_servers_from_install_path(&tmp);
        assert!(names.contains("alpha"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_underutilized_recent() {
        let now = Utc::now();
        let s = McpServerStatus {
            name: "foo".to_string(),
            configured: true,
            last_used: Some(now - Duration::days(5)),
            total_calls: 10,
        };
        assert!(!s.is_underutilized(now, 30));
    }

    #[test]
    fn test_underutilized_old() {
        let now = Utc::now();
        let s = McpServerStatus {
            name: "foo".to_string(),
            configured: true,
            last_used: Some(now - Duration::days(60)),
            total_calls: 10,
        };
        assert!(s.is_underutilized(now, 30));
    }

    #[test]
    fn test_compute_mcp_status_stable_sort_on_equal_calls() {
        // Regression: when multiple servers have the same total_calls, their ordering
        // must be deterministic (alphabetical by name) so the UI doesn't shuffle them
        // on scroll. HashMap iteration order is non-deterministic, so without the
        // name tiebreak equal-call servers would swap positions across redraws.
        use crate::aggregator::{DailyGroup, SessionInfo};
        use chrono::Local;
        use std::collections::HashMap;
        use std::path::PathBuf;

        let today = Local::now().date_naive();
        let mk = |name: &str| {
            let mut tu = HashMap::new();
            tu.insert(format!("mcp__{name}__action"), 5);
            SessionInfo {
                file_path: PathBuf::from(format!("/tmp/{name}.jsonl")),
                project_name: "p".to_string(),
                git_branch: None,
                session_first_timestamp: Utc::now(),
                day_first_timestamp: Utc::now(),
                day_last_timestamp: Utc::now(),
                day_input_tokens: 0,
                day_output_tokens: 0,
                day_tokens_by_model: HashMap::new(),
                day_hourly_activity: HashMap::new(),
                day_hourly_work_tokens: HashMap::new(),
                day_tool_usage: tu,
                day_language_usage: HashMap::new(),
                day_extension_usage: HashMap::new(),
                summary: None,
                custom_title: None,
                ai_title: None,
                model: None,
                is_subagent: false,
                is_continued: false,
            }
        };
        // Three servers all with 5 calls — tied.
        let groups = vec![DailyGroup {
            date: today,
            sessions: vec![mk("zeta"), mk("alpha"), mk("mu")],
        }];

        // Run twice; ordering must match. `compute_mcp_status` also pulls in any MCP
        // servers configured in the host's `~/.claude.json`; filter to only the ones
        // we injected so the assertion is environment-independent.
        let first = compute_mcp_status(&groups);
        let second = compute_mcp_status(&groups);
        let filter = |s: &McpServerStatus| matches!(s.name.as_str(), "zeta" | "alpha" | "mu");
        let names1: Vec<_> = first
            .iter()
            .filter(|s| filter(s))
            .map(|s| s.name.clone())
            .collect();
        let names2: Vec<_> = second
            .iter()
            .filter(|s| filter(s))
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(names1, names2, "sort must be deterministic across calls");
        assert_eq!(
            names1,
            vec!["alpha".to_string(), "mu".to_string(), "zeta".to_string()]
        );
    }

    #[test]
    fn test_not_underutilized_if_not_configured() {
        // Used in logs but not in current config (e.g., removed from ~/.claude.json)
        let s = McpServerStatus {
            name: "foo".to_string(),
            configured: false,
            last_used: None,
            total_calls: 0,
        };
        assert!(!s.is_underutilized(Utc::now(), 30));
    }
}
