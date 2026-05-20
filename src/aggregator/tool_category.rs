#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCategory {
    BuiltIn,
    Mcp {
        server: String,
    },
    Skill {
        name: String,
    },
    Agent {
        subtype: String,
    },
    /// Slash command — `/clear`, `/model`, or a user-defined `~/.claude/commands/<name>.md`.
    /// Stored under `tool_usage` keys with the `command:<name>` prefix.
    Command {
        name: String,
    },
}

pub fn classify_tool(name: &str) -> ToolCategory {
    if let Some(rest) = name.strip_prefix("skill:") {
        return ToolCategory::Skill {
            name: rest.to_string(),
        };
    }
    if let Some(rest) = name.strip_prefix("agent:") {
        return ToolCategory::Agent {
            subtype: rest.to_string(),
        };
    }
    if let Some(rest) = name.strip_prefix("command:") {
        return ToolCategory::Command {
            name: rest.to_string(),
        };
    }
    if let Some(rest) = name.strip_prefix("mcp__") {
        let server_raw = rest.rsplit_once("__").map_or(rest, |(s, _)| s);
        let server = extract_server(server_raw);
        ToolCategory::Mcp { server }
    } else {
        ToolCategory::BuiltIn
    }
}

pub fn mcp_server_of(name: &str) -> Option<String> {
    match classify_tool(name) {
        ToolCategory::Mcp { server } => Some(server),
        _ => None,
    }
}

pub fn format_tool_short(name: &str) -> String {
    // Strip category prefixes (`skill:` / `agent:` / `command:`) for display:
    // every caller renders meta-tools inside a category-labelled section
    // (Skills tab, Insights "Top tools" Skills block, etc.), so repeating
    // the category name on every row is pure noise.
    if let Some(rest) = name.strip_prefix("skill:") {
        return rest.to_string();
    }
    if let Some(rest) = name.strip_prefix("agent:") {
        return rest.to_string();
    }
    if let Some(rest) = name.strip_prefix("command:") {
        return rest.to_string();
    }
    let Some(rest) = name.strip_prefix("mcp__") else {
        return name.to_string();
    };
    let Some((server_raw, tool)) = rest.rsplit_once("__") else {
        return name.to_string();
    };
    let server = extract_server(server_raw);
    format!("{server}:{tool}")
}

/// Normalize a raw tool name + input into the storage key used in `tool_usage` maps.
///
/// Meta-tools like `Skill`, `Agent`, and `Task` carry a sub-identifier in their input
/// (`input.skill`, `input.subagent_type`). Collapsing every invocation under the bare
/// `"Skill"` key hides which skill was actually used, so the key is rewritten to
/// `skill:<name>` / `agent:<subtype>` at insertion time. Other tools are unchanged.
pub fn tool_usage_key(name: &str, input: &serde_json::Value) -> String {
    match name {
        "Skill" => input
            .get("skill")
            .and_then(|v| v.as_str())
            .map_or_else(|| name.to_string(), |s| format!("skill:{s}")),
        "Agent" | "Task" => input
            .get("subagent_type")
            .and_then(|v| v.as_str())
            .map_or_else(|| name.to_string(), |s| format!("agent:{s}")),
        _ => name.to_string(),
    }
}

fn extract_server(server_raw: &str) -> String {
    if let Some(plugin_part) = server_raw.strip_prefix("plugin_") {
        if let Some((plugin, srv)) = plugin_part.split_once('_') {
            plugin_server_key(plugin, srv)
        } else {
            plugin_part.to_string()
        }
    } else {
        server_raw.to_string()
    }
}

/// Compose the canonical key for a plugin-provided MCP server. When the
/// plugin name and server name are identical (a plugin that exposes a single
/// server with the same name as itself), the runtime form
/// `mcp__plugin_<X>_<X>__...` collapses to just `<X>` — the join with
/// observed `tool_usage` only succeeds if both sides agree on this rule.
/// Both `extract_server` (which parses observed tool keys) and
/// `mcp_config::read_plugin_mcp_servers` (which builds the configured set
/// from `.mcp.json`) call this helper so the rule lives in one place.
pub fn plugin_server_key(plugin: &str, server: &str) -> String {
    if plugin == server {
        server.to_string()
    } else {
        format!("{plugin}/{server}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_classify_builtin() {
        assert_eq!(classify_tool("Bash"), ToolCategory::BuiltIn);
        assert_eq!(classify_tool("Read"), ToolCategory::BuiltIn);
        assert_eq!(classify_tool("Edit"), ToolCategory::BuiltIn);
    }

    #[test]
    fn test_classify_mcp_standard() {
        // Generic placeholder server/tool names — never use real product/org/plugin names.
        assert_eq!(
            classify_tool("mcp__server1__action"),
            ToolCategory::Mcp {
                server: "server1".to_string()
            }
        );
    }

    #[test]
    fn test_classify_mcp_plugin() {
        assert_eq!(
            classify_tool("mcp__plugin_orgA_serverB__action"),
            ToolCategory::Mcp {
                server: "orgA/serverB".to_string()
            }
        );
    }

    #[test]
    fn test_classify_mcp_plugin_compressed() {
        // When plugin and server names match, server is shown without prefix duplication.
        assert_eq!(
            classify_tool("mcp__plugin_serverC_serverC__action"),
            ToolCategory::Mcp {
                server: "serverC".to_string()
            }
        );
    }

    #[test]
    fn test_classify_skill_prefix() {
        assert_eq!(
            classify_tool("skill:my-skill"),
            ToolCategory::Skill {
                name: "my-skill".to_string()
            }
        );
    }

    #[test]
    fn test_classify_agent_prefix() {
        assert_eq!(
            classify_tool("agent:type-a"),
            ToolCategory::Agent {
                subtype: "type-a".to_string()
            }
        );
        // Subtype may itself contain a colon (namespaced subagent).
        assert_eq!(
            classify_tool("agent:ns:type-b"),
            ToolCategory::Agent {
                subtype: "ns:type-b".to_string()
            }
        );
    }

    #[test]
    fn test_format_builtin_unchanged() {
        assert_eq!(format_tool_short("Bash"), "Bash");
        assert_eq!(format_tool_short("Read"), "Read");
    }

    #[test]
    fn test_format_mcp_standard() {
        assert_eq!(format_tool_short("mcp__server1__action"), "server1:action");
    }

    #[test]
    fn test_format_mcp_plugin() {
        assert_eq!(
            format_tool_short("mcp__plugin_orgA_serverB__action"),
            "orgA/serverB:action"
        );
        assert_eq!(
            format_tool_short("mcp__plugin_serverC_serverC__action"),
            "serverC:action"
        );
    }

    #[test]
    fn test_format_mcp_edge_cases() {
        assert_eq!(format_tool_short("mcp__server1"), "mcp__server1");
        assert_eq!(format_tool_short("foo__bar"), "foo__bar");
    }

    #[test]
    fn test_format_strips_category_prefix() {
        // Category prefixes are stripped because every display surface groups
        // these rows under a category-labelled section, making the prefix
        // redundant noise.
        assert_eq!(format_tool_short("skill:my-skill"), "my-skill");
        assert_eq!(format_tool_short("agent:type-a"), "type-a");
        assert_eq!(format_tool_short("command:my-cmd"), "my-cmd");
    }

    #[test]
    fn test_mcp_server_of() {
        assert_eq!(
            mcp_server_of("mcp__server1__action"),
            Some("server1".to_string())
        );
        assert_eq!(mcp_server_of("Bash"), None);
        assert_eq!(mcp_server_of("skill:my-skill"), None);
        assert_eq!(mcp_server_of("agent:type-a"), None);
    }

    #[test]
    fn test_tool_usage_key_skill() {
        let input = json!({ "skill": "my-skill" });
        assert_eq!(tool_usage_key("Skill", &input), "skill:my-skill");
    }

    #[test]
    fn test_tool_usage_key_skill_missing_field() {
        let input = json!({});
        assert_eq!(tool_usage_key("Skill", &input), "Skill");
    }

    #[test]
    fn test_tool_usage_key_agent() {
        let input = json!({ "subagent_type": "type-a" });
        assert_eq!(tool_usage_key("Agent", &input), "agent:type-a");
    }

    #[test]
    fn test_tool_usage_key_task() {
        let input = json!({ "subagent_type": "type-b" });
        assert_eq!(tool_usage_key("Task", &input), "agent:type-b");
    }

    #[test]
    fn test_tool_usage_key_agent_missing_field() {
        let input = json!({ "prompt": "foo" });
        assert_eq!(tool_usage_key("Agent", &input), "Agent");
    }

    #[test]
    fn test_tool_usage_key_passthrough() {
        let input = json!({ "file_path": "/foo" });
        assert_eq!(tool_usage_key("Read", &input), "Read");
        // Standard MCP form is passed through unchanged.
        assert_eq!(
            tool_usage_key("mcp__server1__action", &input),
            "mcp__server1__action"
        );
        // Plugin MCP form is also passed through unchanged (regression).
        assert_eq!(
            tool_usage_key("mcp__plugin_orgA_serverB__action", &input),
            "mcp__plugin_orgA_serverB__action"
        );
    }

    #[test]
    fn test_classify_handles_both_mcp_forms() {
        // Regression: ensure plugin-form MCP keys land in the same `Mcp` category as
        // the standard-form keys. A future refactor that special-cases `plugin_` could
        // accidentally route them elsewhere.
        match classify_tool("mcp__server1__action") {
            ToolCategory::Mcp { server } => assert_eq!(server, "server1"),
            other => panic!("standard MCP misclassified: {other:?}"),
        }
        match classify_tool("mcp__plugin_orgA_serverB__action") {
            ToolCategory::Mcp { server } => assert_eq!(server, "orgA/serverB"),
            other => panic!("plugin MCP misclassified: {other:?}"),
        }
    }
}
