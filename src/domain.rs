use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LogEntry {
    pub uuid: Option<String>,

    #[serde(rename = "parentUuid")]
    pub parent_uuid: Option<String>,

    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,

    pub timestamp: Option<DateTime<Utc>>,

    #[serde(rename = "type")]
    pub entry_type: EntryType,

    #[serde(default)]
    pub message: Option<Message>,

    #[serde(default)]
    pub summary: Option<String>,

    #[serde(rename = "customTitle", default)]
    pub custom_title: Option<String>,

    #[serde(rename = "aiTitle", default)]
    pub ai_title: Option<String>,

    #[serde(default)]
    pub cwd: Option<String>,

    #[serde(rename = "gitBranch")]
    pub git_branch: Option<String>,

    #[serde(default)]
    pub version: Option<String>,

    #[serde(rename = "isSidechain", default)]
    pub is_sidechain: bool,

    #[serde(rename = "userType")]
    pub user_type: Option<UserType>,

    #[serde(rename = "requestId")]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum EntryType {
    User,
    Assistant,
    Summary,
    CustomTitle,
    AiTitle,
    System,
    FileHistorySnapshot,
    QueueOperation,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UserType {
    External,
    Internal,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    pub role: Role,

    #[serde(default)]
    pub content: MessageContent,

    #[serde(default)]
    pub usage: Option<Usage>,

    #[serde(default)]
    pub model: Option<String>,

    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    #[default]
    Assistant,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(untagged)]
pub enum MessageContent {
    #[default]
    Empty,
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl MessageContent {
    pub fn extract_text(&self) -> String {
        match self {
            MessageContent::Empty => String::new(),
            MessageContent::Text(s) => s.clone(),
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    ContentBlock::Thinking { thinking, .. } => Some(thinking.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    pub fn extract_tool_calls(&self) -> Vec<(String, Option<String>)> {
        match self {
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolUse { name, input, .. } => {
                        let file_path = input
                            .get("file_path")
                            .or_else(|| input.get("path"))
                            .or_else(|| input.get("command"))
                            .and_then(|v| v.as_str())
                            .map(std::string::ToString::to_string);
                        Some((name.clone(), file_path))
                    }
                    _ => None,
                })
                .collect(),
            _ => vec![],
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },

    #[serde(rename = "tool_use")]
    ToolUse {
        id: Option<String>,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },

    #[serde(rename = "tool_result")]
    ToolResult {
        #[serde(rename = "tool_use_id")]
        tool_use_id: Option<String>,
        #[serde(default)]
        content: serde_json::Value,
        #[serde(default)]
        is_error: bool,
    },

    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },

    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,

    #[serde(default)]
    pub output_tokens: u64,

    #[serde(default, rename = "cache_creation_input_tokens")]
    pub cache_creation_input_tokens: u64,

    #[serde(default, rename = "cache_read_input_tokens")]
    pub cache_read_input_tokens: u64,

    /// Per-TTL breakdown of `cache_creation_input_tokens`. Claude Code writes
    /// this object on every recent JSONL entry. `ephemeral_1h_input_tokens`
    /// is billed at 2x base input (vs `5m` at 1.25x), so missing this split
    /// silently undercounts cost for subscription users (who default to 1h
    /// TTL per Anthropic's `ENABLE_PROMPT_CACHING_1H` doc). When absent (very
    /// old JSONL), callers fall back to the flat field at 5m rate.
    #[serde(default)]
    pub cache_creation: Option<CacheCreationBreakdown>,

    #[serde(default)]
    pub service_tier: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct CacheCreationBreakdown {
    #[serde(default)]
    pub ephemeral_5m_input_tokens: u64,

    #[serde(default)]
    pub ephemeral_1h_input_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_text_empty() {
        let content = MessageContent::Empty;
        assert_eq!(content.extract_text(), "");
    }

    #[test]
    fn test_extract_text_string() {
        let content = MessageContent::Text("hello world".to_string());
        assert_eq!(content.extract_text(), "hello world");
    }

    #[test]
    fn test_extract_text_blocks_text_only() {
        let content = MessageContent::Blocks(vec![
            ContentBlock::Text {
                text: "first".to_string(),
            },
            ContentBlock::Text {
                text: "second".to_string(),
            },
        ]);
        assert_eq!(content.extract_text(), "first\nsecond");
    }

    #[test]
    fn test_extract_text_blocks_with_thinking() {
        let content = MessageContent::Blocks(vec![
            ContentBlock::Thinking {
                thinking: "let me think".to_string(),
                signature: None,
            },
            ContentBlock::Text {
                text: "answer".to_string(),
            },
        ]);
        assert_eq!(content.extract_text(), "let me think\nanswer");
    }

    #[test]
    fn test_extract_text_blocks_skips_tool_use() {
        let content = MessageContent::Blocks(vec![
            ContentBlock::Text {
                text: "before".to_string(),
            },
            ContentBlock::ToolUse {
                id: Some("t1".to_string()),
                name: "Read".to_string(),
                input: serde_json::json!({"file_path": "/tmp/f.rs"}),
            },
            ContentBlock::Text {
                text: "after".to_string(),
            },
        ]);
        assert_eq!(content.extract_text(), "before\nafter");
    }

    #[test]
    fn test_extract_text_blocks_skips_tool_result() {
        let content = MessageContent::Blocks(vec![
            ContentBlock::Text {
                text: "text".to_string(),
            },
            ContentBlock::ToolResult {
                tool_use_id: Some("t1".to_string()),
                content: serde_json::json!("result"),
                is_error: false,
            },
        ]);
        assert_eq!(content.extract_text(), "text");
    }

    #[test]
    fn test_extract_text_empty_blocks() {
        let content = MessageContent::Blocks(vec![]);
        assert_eq!(content.extract_text(), "");
    }

    #[test]
    fn test_extract_tool_calls_empty() {
        let content = MessageContent::Empty;
        assert!(content.extract_tool_calls().is_empty());
    }

    #[test]
    fn test_extract_tool_calls_text() {
        let content = MessageContent::Text("no tools".to_string());
        assert!(content.extract_tool_calls().is_empty());
    }

    #[test]
    fn test_extract_tool_calls_file_path() {
        let content = MessageContent::Blocks(vec![ContentBlock::ToolUse {
            id: Some("t1".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path": "/src/main.rs"}),
        }]);
        let calls = content.extract_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "Read");
        assert_eq!(calls[0].1.as_deref(), Some("/src/main.rs"));
    }

    #[test]
    fn test_extract_tool_calls_path_fallback() {
        let content = MessageContent::Blocks(vec![ContentBlock::ToolUse {
            id: None,
            name: "Glob".to_string(),
            input: serde_json::json!({"path": "/src", "pattern": "*.rs"}),
        }]);
        let calls = content.extract_tool_calls();
        assert_eq!(calls[0].1.as_deref(), Some("/src"));
    }

    #[test]
    fn test_extract_tool_calls_command_fallback() {
        let content = MessageContent::Blocks(vec![ContentBlock::ToolUse {
            id: None,
            name: "Bash".to_string(),
            input: serde_json::json!({"command": "ls -la"}),
        }]);
        let calls = content.extract_tool_calls();
        assert_eq!(calls[0].0, "Bash");
        assert_eq!(calls[0].1.as_deref(), Some("ls -la"));
    }

    #[test]
    fn test_extract_tool_calls_file_path_priority_over_path() {
        let content = MessageContent::Blocks(vec![ContentBlock::ToolUse {
            id: None,
            name: "Edit".to_string(),
            input: serde_json::json!({"file_path": "/a.rs", "path": "/b.rs"}),
        }]);
        let calls = content.extract_tool_calls();
        assert_eq!(calls[0].1.as_deref(), Some("/a.rs"));
    }

    #[test]
    fn test_extract_tool_calls_no_known_keys() {
        let content = MessageContent::Blocks(vec![ContentBlock::ToolUse {
            id: None,
            name: "Custom".to_string(),
            input: serde_json::json!({"foo": "bar"}),
        }]);
        let calls = content.extract_tool_calls();
        assert_eq!(calls[0].0, "Custom");
        assert_eq!(calls[0].1, None);
    }

    #[test]
    fn test_extract_tool_calls_multiple() {
        let content = MessageContent::Blocks(vec![
            ContentBlock::ToolUse {
                id: None,
                name: "Read".to_string(),
                input: serde_json::json!({"file_path": "/a.rs"}),
            },
            ContentBlock::Text {
                text: "thinking...".to_string(),
            },
            ContentBlock::ToolUse {
                id: None,
                name: "Bash".to_string(),
                input: serde_json::json!({"command": "cargo build"}),
            },
        ]);
        let calls = content.extract_tool_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "Read");
        assert_eq!(calls[1].0, "Bash");
    }

    #[test]
    fn test_extract_tool_calls_skips_non_tool_blocks() {
        let content = MessageContent::Blocks(vec![
            ContentBlock::Text {
                text: "text".to_string(),
            },
            ContentBlock::Thinking {
                thinking: "hmm".to_string(),
                signature: None,
            },
            ContentBlock::ToolResult {
                tool_use_id: None,
                content: serde_json::json!("ok"),
                is_error: false,
            },
        ]);
        assert!(content.extract_tool_calls().is_empty());
    }
}
