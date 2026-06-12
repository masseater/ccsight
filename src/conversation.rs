use std::io;

use chrono::Local;

use crate::domain::{ContentBlock, EntryType, MessageContent, Role};
use crate::parser::JsonlParser;

/// Maximum characters of a `thinking` block to keep in the conversation preview.
/// Long thinking blocks are common; truncation keeps the popup readable.
const MAX_THINKING_PREVIEW_CHARS: usize = 500;

/// Maximum characters of a tool result to keep in the conversation preview.
/// Some tool outputs (search hits, file dumps) can be huge.
const MAX_TOOL_RESULT_PREVIEW_CHARS: usize = 2000;

/// Maximum characters of a Bash `command` value rendered in the tool-input summary.
const MAX_BASH_COMMAND_PREVIEW_CHARS: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationBlock {
    Text(String),
    Thinking(String),
    ToolUse { name: String, input_summary: String },
    ToolResult { content: String, is_error: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationMessage {
    pub role: String,
    pub blocks: Vec<ConversationBlock>,
    pub timestamp: Option<String>,
    /// Raw UTC timestamp used to compute the previous-message gap (response
    /// latency). Kept separately from the human-readable `timestamp` string
    /// so the renderer doesn't have to re-parse.
    pub timestamp_utc: Option<chrono::DateTime<chrono::Utc>>,
    pub model: Option<String>,
    pub tokens: Option<(u64, u64)>,
    /// Full per-message usage (input / output / 5m cw / 1h cw / read) so
    /// the conv pane can compute per-turn cost without losing the
    /// 1h-vs-5m cache split that drives ~half of subscription spend.
    pub usage: Option<crate::aggregator::stats::TokenStats>,
}

#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    Parse(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::Parse(msg) => write!(f, "Parse error: {msg}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<io::Error> for LoadError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Loads and parses a conversation from a JSONL file.
///
/// # Errors
/// Returns `LoadError` if the file cannot be read or parsed.
pub fn load_conversation(
    file_path: &std::path::Path,
) -> Result<Vec<ConversationMessage>, LoadError> {
    let entries =
        JsonlParser::parse_file(file_path).map_err(|e| LoadError::Parse(e.to_string()))?;

    let messages = entries
        .into_iter()
        .filter(|e| matches!(e.entry_type, EntryType::User | EntryType::Assistant))
        .filter_map(|e| {
            let message = e.message?;

            let blocks: Vec<ConversationBlock> = match &message.content {
                MessageContent::Empty => vec![],
                MessageContent::Text(s) => {
                    if s.is_empty() {
                        vec![]
                    } else {
                        vec![ConversationBlock::Text(s.clone())]
                    }
                }
                MessageContent::Blocks(content_blocks) => content_blocks
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => {
                            if text.is_empty() {
                                None
                            } else {
                                Some(ConversationBlock::Text(text.clone()))
                            }
                        }
                        ContentBlock::Thinking { thinking, .. } => {
                            if thinking.is_empty() {
                                None
                            } else {
                                let truncated: String =
                                    thinking.chars().take(MAX_THINKING_PREVIEW_CHARS).collect();
                                Some(ConversationBlock::Thinking(truncated))
                            }
                        }
                        ContentBlock::ToolUse { name, input, .. } => {
                            let input_summary = summarize_tool_input(name, input);
                            Some(ConversationBlock::ToolUse {
                                name: name.clone(),
                                input_summary,
                            })
                        }
                        ContentBlock::ToolResult {
                            content, is_error, ..
                        } => {
                            let content_str = match content {
                                serde_json::Value::String(s) => {
                                    s.chars().take(MAX_TOOL_RESULT_PREVIEW_CHARS).collect()
                                }
                                serde_json::Value::Array(arr) => {
                                    let joined = arr
                                        .iter()
                                        .filter_map(|v| v.get("text").and_then(|t| t.as_str()))
                                        .collect::<Vec<_>>()
                                        .join("\n");
                                    joined.chars().take(MAX_TOOL_RESULT_PREVIEW_CHARS).collect()
                                }
                                _ => content
                                    .to_string()
                                    .chars()
                                    .take(MAX_TOOL_RESULT_PREVIEW_CHARS)
                                    .collect(),
                            };
                            Some(ConversationBlock::ToolResult {
                                content: content_str,
                                is_error: *is_error,
                            })
                        }
                        ContentBlock::Unknown => None,
                    })
                    .collect(),
            };

            if blocks.is_empty() {
                return None;
            }

            let role = match message.role {
                Role::User => "user".to_string(),
                Role::Assistant => "assistant".to_string(),
                Role::Unknown => "unknown".to_string(),
            };

            let timestamp = e
                .timestamp
                .map(|ts| ts.with_timezone(&Local).format("%H:%M:%S").to_string());
            let timestamp_utc = e.timestamp;

            let tokens = message
                .usage
                .as_ref()
                .map(|u| (u.input_tokens, u.output_tokens));
            // Mirror the per-TTL split logic in aggregator::stats::TokenStats::add:
            // prefer the structured cache_creation object (1h / 5m); fall back to
            // the flat field at 5m rate for older JSONLs.
            let usage = message.usage.as_ref().map(|u| {
                let (m5, m1) = u.cache_creation.as_ref().map_or_else(
                    || (u.cache_creation_input_tokens, 0),
                    |c| (c.ephemeral_5m_input_tokens, c.ephemeral_1h_input_tokens),
                );
                crate::aggregator::stats::TokenStats {
                    input_tokens: u.input_tokens,
                    output_tokens: u.output_tokens,
                    cache_creation_tokens: u.cache_creation_input_tokens,
                    cache_read_tokens: u.cache_read_input_tokens,
                    cache_creation_5m_tokens: m5,
                    cache_creation_1h_tokens: m1,
                }
            });

            Some(ConversationMessage {
                role,
                blocks,
                timestamp,
                timestamp_utc,
                model: message.model,
                tokens,
                usage,
            })
        })
        .collect();

    Ok(messages)
}

/// Last `n` user/assistant messages that carry visible text, oldest-first,
/// each collapsed to a single line `(role, text)`. Tool-only / thinking-only
/// turns are skipped so the Session Detail "Recent conversation" preview reads
/// as an actual exchange rather than a wall of `(tool use)`.
pub fn recent_message_previews(
    messages: &[ConversationMessage],
    n: usize,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for msg in messages.iter().rev() {
        let Some(text) = msg.blocks.iter().find_map(|b| match b {
            ConversationBlock::Text(t) if !t.trim().is_empty() => Some(t.trim()),
            _ => None,
        }) else {
            continue;
        };
        // Collapse newlines / runs of whitespace so each message is one line.
        let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
        out.push((msg.role.clone(), one_line));
        if out.len() == n {
            break;
        }
    }
    out.reverse();
    out
}

fn summarize_tool_input(name: &str, input: &serde_json::Value) -> String {
    let str_field = |key: &str| {
        input
            .get(key)
            .and_then(|v| v.as_str())
            .map(ToString::to_string)
    };
    match name {
        "Read" | "Write" | "Edit" => str_field("file_path").unwrap_or_default(),
        "Bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| {
                let cmd: String = s.chars().take(MAX_BASH_COMMAND_PREVIEW_CHARS).collect();
                if s.chars().count() > MAX_BASH_COMMAND_PREVIEW_CHARS {
                    format!("{cmd}...")
                } else {
                    cmd
                }
            })
            .unwrap_or_default(),
        "Glob" | "Grep" => str_field("pattern").unwrap_or_default(),
        // `Task` / `Agent` carry `subagent_type`; show that plus optional description.
        "Task" | "Agent" => {
            let subagent = str_field("subagent_type");
            let desc = str_field("description");
            match (subagent, desc) {
                (Some(s), Some(d)) => format!("{s}: {d}"),
                (Some(s), None) => s,
                (None, Some(d)) => d,
                (None, None) => String::new(),
            }
        }
        // `Skill` carries `skill` (the skill name). `args` is freeform.
        "Skill" => {
            let skill = str_field("skill").unwrap_or_default();
            let args = str_field("args");
            match args {
                Some(a) if !a.is_empty() => format!("{skill}: {a}"),
                _ => skill,
            }
        }
        "WebFetch" | "WebSearch" => str_field("url")
            .or_else(|| str_field("query"))
            .unwrap_or_default(),
        _ => {
            // MCP tools (mcp__server__action / mcp__plugin_*) end up here. Show first
            // few input keys as a hint.
            let keys: Vec<_> = input
                .as_object()
                .map(|o| o.keys().take(3).cloned().collect())
                .unwrap_or_default();
            keys.join(", ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_summarize_tool_input_read() {
        let input = serde_json::json!({"file_path": "/path/to/file.rs"});
        assert_eq!(summarize_tool_input("Read", &input), "/path/to/file.rs");
    }

    #[test]
    fn test_summarize_tool_input_write() {
        let input = serde_json::json!({"file_path": "/path/to/output.txt", "content": "data"});
        assert_eq!(summarize_tool_input("Write", &input), "/path/to/output.txt");
    }

    #[test]
    fn test_summarize_tool_input_bash() {
        let input = serde_json::json!({"command": "ls -la"});
        assert_eq!(summarize_tool_input("Bash", &input), "ls -la");
    }

    #[test]
    fn test_summarize_tool_input_bash_truncates_long_command() {
        let long_cmd = "a".repeat(100);
        let input = serde_json::json!({"command": long_cmd});
        let result = summarize_tool_input("Bash", &input);
        assert!(result.ends_with("..."));
        assert_eq!(result.len(), 83);
    }

    #[test]
    fn test_summarize_tool_input_grep() {
        let input = serde_json::json!({"pattern": "fn main"});
        assert_eq!(summarize_tool_input("Grep", &input), "fn main");
    }

    #[test]
    fn test_summarize_tool_input_unknown() {
        let input =
            serde_json::json!({"key1": "val1", "key2": "val2", "key3": "val3", "key4": "val4"});
        let result = summarize_tool_input("UnknownTool", &input);
        let keys: Vec<&str> = result.split(", ").collect();
        assert!(keys.len() <= 3);
    }

    #[test]
    fn test_summarize_tool_input_edit() {
        let input = serde_json::json!({"file_path": "/path/to/edit.rs", "old_string": "a", "new_string": "b"});
        assert_eq!(summarize_tool_input("Edit", &input), "/path/to/edit.rs");
    }

    #[test]
    fn test_summarize_tool_input_glob() {
        let input = serde_json::json!({"pattern": "**/*.rs"});
        assert_eq!(summarize_tool_input("Glob", &input), "**/*.rs");
    }

    #[test]
    fn test_summarize_tool_input_task() {
        let input = serde_json::json!({"description": "Find files", "prompt": "..."});
        assert_eq!(summarize_tool_input("Task", &input), "Find files");
    }

    #[test]
    fn test_summarize_tool_input_web_fetch() {
        let input = serde_json::json!({"url": "https://example.com"});
        assert_eq!(
            summarize_tool_input("WebFetch", &input),
            "https://example.com"
        );
    }

    #[test]
    fn test_summarize_tool_input_web_search() {
        let input = serde_json::json!({"query": "rust tutorial"});
        assert_eq!(summarize_tool_input("WebSearch", &input), "rust tutorial");
    }

    #[test]
    fn test_summarize_tool_input_missing_field() {
        let input = serde_json::json!({});
        assert_eq!(summarize_tool_input("Read", &input), "");
    }

    #[test]
    fn test_summarize_tool_input_bash_exact_80() {
        let cmd = "a".repeat(80);
        let input = serde_json::json!({"command": cmd});
        let result = summarize_tool_input("Bash", &input);
        assert_eq!(result.len(), 80);
        assert!(!result.ends_with("..."));
    }

    use std::sync::atomic::{AtomicU64, Ordering};
    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn write_jsonl(lines: &[serde_json::Value]) -> std::path::PathBuf {
        use std::io::Write;
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("ccsight_test_{}_{}.jsonl", std::process::id(), id));
        let mut f = std::fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{}", serde_json::to_string(line).unwrap()).unwrap();
        }
        path
    }

    fn make_entry(entry_type: &str, role: &str, text: &str) -> serde_json::Value {
        serde_json::json!({
            "type": entry_type,
            "timestamp": "2026-02-25T10:00:00Z",
            "message": {
                "role": role,
                "content": text
            }
        })
    }

    #[test]
    fn test_load_conversation_basic_text() {
        let path = write_jsonl(&[
            make_entry("user", "user", "Hello"),
            make_entry("assistant", "assistant", "Hi there"),
        ]);
        let msgs = load_conversation(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].role, "assistant");
        assert!(matches!(&msgs[0].blocks[0], ConversationBlock::Text(t) if t == "Hello"));
    }

    #[test]
    fn test_load_conversation_empty_file() {
        let path = write_jsonl(&[]);
        let msgs = load_conversation(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_load_conversation_skips_summary_entries() {
        let path = write_jsonl(&[
            make_entry("user", "user", "hello"),
            serde_json::json!({
                "type": "summary",
                "timestamp": "2026-02-25T10:01:00Z",
                "summary": "A summary of the session"
            }),
            make_entry("assistant", "assistant", "response"),
        ]);
        let msgs = load_conversation(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].role, "assistant");
    }

    #[test]
    fn test_load_conversation_skips_empty_content() {
        let path = write_jsonl(&[
            make_entry("user", "user", ""),
            make_entry("assistant", "assistant", "real response"),
        ]);
        let msgs = load_conversation(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "assistant");
    }

    #[test]
    fn test_load_conversation_with_blocks() {
        let path = write_jsonl(&[serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-02-25T10:00:00Z",
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "Let me read the file."},
                    {"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": "/src/main.rs"}},
                ],
                "model": "claude-sonnet-4-20250514"
            }
        })]);
        let msgs = load_conversation(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].blocks.len(), 2);
        assert!(
            matches!(&msgs[0].blocks[0], ConversationBlock::Text(t) if t.contains("read the file"))
        );
        assert!(
            matches!(&msgs[0].blocks[1], ConversationBlock::ToolUse { name, input_summary } if name == "Read" && input_summary.contains("main.rs"))
        );
        assert_eq!(msgs[0].model.as_deref(), Some("claude-sonnet-4-20250514"));
    }

    #[test]
    fn test_load_conversation_with_tool_result() {
        let path = write_jsonl(&[serde_json::json!({
            "type": "user",
            "timestamp": "2026-02-25T10:00:00Z",
            "message": {
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "file contents here", "is_error": false}
                ]
            }
        })]);
        let msgs = load_conversation(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(msgs.len(), 1);
        assert!(
            matches!(&msgs[0].blocks[0], ConversationBlock::ToolResult { content, is_error } if content == "file contents here" && !is_error)
        );
    }

    #[test]
    fn test_load_conversation_with_tool_result_error() {
        let path = write_jsonl(&[serde_json::json!({
            "type": "user",
            "timestamp": "2026-02-25T10:00:00Z",
            "message": {
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "permission denied", "is_error": true}
                ]
            }
        })]);
        let msgs = load_conversation(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(msgs.len(), 1);
        assert!(
            matches!(&msgs[0].blocks[0], ConversationBlock::ToolResult { is_error, .. } if *is_error)
        );
    }

    #[test]
    fn test_load_conversation_thinking_truncated() {
        let long_thinking = "x".repeat(1000);
        let path = write_jsonl(&[serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-02-25T10:00:00Z",
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": long_thinking}
                ]
            }
        })]);
        let msgs = load_conversation(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(msgs.len(), 1);
        if let ConversationBlock::Thinking(t) = &msgs[0].blocks[0] {
            assert_eq!(t.chars().count(), 500);
        } else {
            panic!("expected Thinking block");
        }
    }

    #[test]
    fn test_load_conversation_with_usage_tokens() {
        let path = write_jsonl(&[serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-02-25T10:00:00Z",
            "message": {
                "role": "assistant",
                "content": "response",
                "usage": {"input_tokens": 100, "output_tokens": 50}
            }
        })]);
        let msgs = load_conversation(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(msgs[0].tokens, Some((100, 50)));
    }

    #[test]
    fn test_load_conversation_nonexistent_file() {
        let result = load_conversation(std::path::Path::new("/nonexistent/file.jsonl"));
        assert!(result.is_err());
    }

    #[test]
    fn test_load_conversation_has_timestamp() {
        let path = write_jsonl(&[make_entry("user", "user", "test")]);
        let msgs = load_conversation(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert!(msgs[0].timestamp.is_some());
    }

    fn msg(role: &str, blocks: Vec<ConversationBlock>) -> ConversationMessage {
        ConversationMessage {
            role: role.to_string(),
            blocks,
            timestamp: None,
            timestamp_utc: None,
            model: None,
            tokens: None,
            usage: None,
        }
    }

    #[test]
    fn recent_previews_takes_last_n_text_messages_oldest_first() {
        use ConversationBlock::{Text, Thinking, ToolUse};
        let messages = vec![
            msg("user", vec![Text("first".into())]),
            msg("assistant", vec![Thinking("...".into())]), // no text → skipped
            msg("assistant", vec![Text("second\nline".into())]),
            msg(
                "user",
                vec![ToolUse {
                    name: "Read".into(),
                    input_summary: "x".into(),
                }],
            ), // no text → skipped
            msg("user", vec![Text("third".into())]),
            msg("assistant", vec![Text("fourth".into())]),
        ];
        // Last 3 text-bearing messages, oldest-first, newlines collapsed.
        assert_eq!(
            recent_message_previews(&messages, 3),
            vec![
                ("assistant".to_string(), "second line".to_string()),
                ("user".to_string(), "third".to_string()),
                ("assistant".to_string(), "fourth".to_string()),
            ]
        );
    }

    #[test]
    fn recent_previews_empty_when_no_text() {
        let messages = vec![msg(
            "assistant",
            vec![ConversationBlock::Thinking("t".into())],
        )];
        assert!(recent_message_previews(&messages, 6).is_empty());
    }
}
