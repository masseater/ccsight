//! Transforms Codex CLI JSONL events into `Vec<LogEntry>` so the rest of
//! the ccsight pipeline (aggregator, UI, MCP) works unchanged.
//!
//! Codex JSONL uses a `type` + `payload.type` event schema that is
//! structurally different from Claude Code's flat `LogEntry` lines.
//! This module reads the stream, carries session-level state (model,
//! cwd, session_id) forward, and emits one `LogEntry` per user/assistant
//! turn with accumulated tool calls and usage attached.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::domain::{ContentBlock, EntryType, LogEntry, Message, MessageContent, Role, Usage};

use crate::parser::{max_entries, max_file_size, max_line_size};

// ---------------------------------------------------------------------------
// Codex-native event structs (deserialized from JSONL)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CodexLine {
    timestamp: Option<DateTime<Utc>>,
    #[serde(rename = "type")]
    line_type: String,
    #[serde(default)]
    payload: serde_json::Value,
}

#[derive(Deserialize)]
struct SessionMetaPayload {
    id: Option<String>,
    cwd: Option<String>,
    cli_version: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Deserialize)]
struct TurnContextPayload {
    model: Option<String>,
    cwd: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
}

#[derive(Deserialize)]
struct TokenUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    reasoning_output_tokens: u64,
}

#[derive(Deserialize)]
struct TokenInfo {
    #[serde(default)]
    last_token_usage: Option<TokenUsage>,
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

pub fn parse_codex_file(path: &Path) -> Result<Vec<LogEntry>> {
    let file =
        File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;

    let metadata = file.metadata()?;
    if metadata.len() > max_file_size() {
        anyhow::bail!("Codex file too large: {} bytes", metadata.len());
    }

    let reader = BufReader::new(file);
    let mut entries: Vec<LogEntry> = Vec::new();

    // Session-level state carried across events
    let mut session_id: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut model: Option<String> = None;
    let mut version: Option<String> = None;
    let git_branch: Option<String> = None;

    // Per-turn accumulator: tool calls for the current assistant turn
    let mut pending_tool_calls: Vec<ContentBlock> = Vec::new();
    // Map call_id → index in pending_tool_calls for matching results
    let mut call_id_map: HashMap<String, usize> = HashMap::new();

    for line_result in reader.lines() {
        if entries.len() >= max_entries() {
            break;
        }

        let Ok(line) = line_result else { continue };
        if line.len() > max_line_size() || line.trim().is_empty() {
            continue;
        }

        let Ok(event) = serde_json::from_str::<CodexLine>(&line) else {
            continue;
        };

        match event.line_type.as_str() {
            "session_meta" => {
                if let Ok(meta) = serde_json::from_value::<SessionMetaPayload>(event.payload) {
                    session_id = meta.id;
                    cwd = meta.cwd;
                    version = meta.cli_version;
                    let _ = meta.source; // reserved for future use
                }
            }
            "turn_context" => {
                if let Ok(ctx) = serde_json::from_value::<TurnContextPayload>(event.payload) {
                    if ctx.model.is_some() {
                        model = ctx.model;
                    }
                    if ctx.cwd.is_some() {
                        cwd = ctx.cwd;
                    }
                    let _ = ctx.reasoning_effort;
                }
            }
            "event_msg" => {
                let payload = &event.payload;
                let sub_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");

                match sub_type {
                    "user_message" => {
                        let text = payload
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        entries.push(LogEntry {
                            uuid: None,
                            parent_uuid: None,
                            session_id: session_id.clone(),
                            timestamp: event.timestamp,
                            entry_type: EntryType::User,
                            message: Some(Message {
                                role: Role::User,
                                content: MessageContent::Text(text),
                                usage: None,
                                model: None,
                                id: None,
                            }),
                            summary: None,
                            custom_title: None,
                            ai_title: None,
                            cwd: cwd.clone(),
                            git_branch: git_branch.clone(),
                            version: version.clone(),
                            is_sidechain: false,
                            user_type: None,
                            request_id: None,
                        });
                    }
                    // agent_message is skipped: text is almost always
                    // empty, and the actual response arrives via
                    // response_item/message (role:assistant).
                    "token_count" => {
                        if let Some(info_val) = payload.get("info") {
                            if let Ok(token_info) =
                                serde_json::from_value::<TokenInfo>(info_val.clone())
                            {
                                if let Some(last) = token_info.last_token_usage {
                                    if last.input_tokens == 0
                                        && last.cached_input_tokens == 0
                                        && last.output_tokens == 0
                                        && last.reasoning_output_tokens == 0
                                    {
                                        continue;
                                    }
                                    // output_tokens already includes
                                    // reasoning_output_tokens as a subset.
                                    let usage = Usage {
                                        input_tokens: last.input_tokens,
                                        output_tokens: last.output_tokens,
                                        cache_creation_input_tokens: 0,
                                        cache_read_input_tokens: last.cached_input_tokens,
                                        cache_creation: None,
                                        service_tier: None,
                                    };
                                    ensure_assistant_entry(
                                        &mut entries,
                                        &session_id,
                                        event.timestamp,
                                        &cwd,
                                        &git_branch,
                                        &version,
                                        &model,
                                        &mut pending_tool_calls,
                                        &mut call_id_map,
                                    );
                                    attach_usage_to_last_assistant(&mut entries, usage);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            "response_item" => {
                let payload = &event.payload;
                let sub_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");

                match sub_type {
                    "function_call" => {
                        let name = payload
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let call_id = payload
                            .get("call_id")
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        let arguments = payload
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}");
                        let input: serde_json::Value =
                            serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null);

                        let idx = pending_tool_calls.len();
                        pending_tool_calls.push(ContentBlock::ToolUse {
                            id: call_id.clone(),
                            name,
                            input,
                        });
                        if let Some(cid) = call_id {
                            call_id_map.insert(cid, idx);
                        }
                    }
                    // ToolResult lands on assistant-role entries (unlike
                    // Claude Code where ToolResult is on user-role). All
                    // current consumers iterate blocks unconditionally.
                    "function_call_output" => {
                        let output = payload
                            .get("output")
                            .cloned()
                            .unwrap_or(serde_json::Value::String(String::new()));
                        let call_id = payload
                            .get("call_id")
                            .and_then(|v| v.as_str())
                            .map(String::from);

                        pending_tool_calls.push(ContentBlock::ToolResult {
                            tool_use_id: call_id,
                            content: output,
                            is_error: false,
                        });
                    }
                    "message" => {
                        let role_str = payload.get("role").and_then(|v| v.as_str()).unwrap_or("");
                        let text = extract_text_from_content_array(payload);
                        if text.is_empty() {
                            continue;
                        }
                        // event_msg/user_message is the canonical user turn;
                        // response_item/message role:user duplicates it, so
                        // only assistant responses are captured here.
                        if role_str == "assistant" {
                            let content = if pending_tool_calls.is_empty() {
                                MessageContent::Text(text)
                            } else {
                                let mut blocks = vec![ContentBlock::Text { text }];
                                blocks.append(&mut pending_tool_calls);
                                call_id_map.clear();
                                MessageContent::Blocks(blocks)
                            };
                            entries.push(LogEntry {
                                uuid: None,
                                parent_uuid: None,
                                session_id: session_id.clone(),
                                timestamp: event.timestamp,
                                entry_type: EntryType::Assistant,
                                message: Some(Message {
                                    role: Role::Assistant,
                                    content,
                                    usage: None,
                                    model: model.clone(),
                                    id: None,
                                }),
                                summary: None,
                                custom_title: None,
                                ai_title: None,
                                cwd: cwd.clone(),
                                git_branch: git_branch.clone(),
                                version: version.clone(),
                                is_sidechain: false,
                                user_type: None,
                                request_id: None,
                            });
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // Flush remaining tool calls into a synthetic assistant entry
    if !pending_tool_calls.is_empty() {
        entries.push(LogEntry {
            uuid: None,
            parent_uuid: None,
            session_id: session_id.clone(),
            timestamp: entries.last().and_then(|e| e.timestamp),
            entry_type: EntryType::Assistant,
            message: Some(Message {
                role: Role::Assistant,
                content: MessageContent::Blocks(pending_tool_calls),
                usage: None,
                model: model.clone(),
                id: None,
            }),
            summary: None,
            custom_title: None,
            ai_title: None,
            cwd,
            git_branch,
            version,
            is_sidechain: false,
            user_type: None,
            request_id: None,
        });
    }

    Ok(entries)
}

fn attach_usage_to_last_assistant(entries: &mut [LogEntry], usage: Usage) {
    for entry in entries.iter_mut().rev() {
        if entry.entry_type == EntryType::Assistant {
            if let Some(ref mut msg) = entry.message {
                match msg.usage {
                    Some(ref mut existing) => {
                        existing.input_tokens += usage.input_tokens;
                        existing.output_tokens += usage.output_tokens;
                        existing.cache_read_input_tokens += usage.cache_read_input_tokens;
                    }
                    None => {
                        msg.usage = Some(usage);
                    }
                }
            }
            return;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn ensure_assistant_entry(
    entries: &mut Vec<LogEntry>,
    session_id: &Option<String>,
    timestamp: Option<DateTime<Utc>>,
    cwd: &Option<String>,
    git_branch: &Option<String>,
    version: &Option<String>,
    model: &Option<String>,
    pending_tool_calls: &mut Vec<ContentBlock>,
    call_id_map: &mut HashMap<String, usize>,
) {
    if entries
        .last()
        .is_some_and(|e| e.entry_type == EntryType::Assistant)
    {
        pending_tool_calls.clear();
        call_id_map.clear();
        return;
    }
    let content = if pending_tool_calls.is_empty() {
        MessageContent::Empty
    } else {
        let blocks = std::mem::take(pending_tool_calls);
        call_id_map.clear();
        MessageContent::Blocks(blocks)
    };
    entries.push(LogEntry {
        uuid: None,
        parent_uuid: None,
        session_id: session_id.clone(),
        timestamp,
        entry_type: EntryType::Assistant,
        message: Some(Message {
            role: Role::Assistant,
            content,
            usage: None,
            model: model.clone(),
            id: None,
        }),
        summary: None,
        custom_title: None,
        ai_title: None,
        cwd: cwd.clone(),
        git_branch: git_branch.clone(),
        version: version.clone(),
        is_sidechain: false,
        user_type: None,
        request_id: None,
    });
}

fn extract_text_from_content_array(payload: &serde_json::Value) -> String {
    let Some(content) = payload.get("content").and_then(|v| v.as_array()) else {
        return String::new();
    };
    content
        .iter()
        .filter_map(|block| {
            let t = block.get("type")?.as_str()?;
            if t == "input_text" || t == "output_text" || t == "text" {
                block.get("text").and_then(|v| v.as_str()).map(String::from)
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn write_codex_session(dir: &Path, events: &[&str]) -> PathBuf {
        let path = dir.join("rollout-test.jsonl");
        let content = events.join("\n") + "\n";
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn test_parse_basic_session() {
        let tmp = std::env::temp_dir().join(format!("ccsight-codex-parse-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();

        let path = write_codex_session(
            &tmp,
            &[
                r#"{"timestamp":"2026-06-20T00:01:16Z","type":"session_meta","payload":{"id":"abc-123","cwd":"/home/user/project","cli_version":"0.142.0"}}"#,
                r#"{"timestamp":"2026-06-20T00:01:17Z","type":"turn_context","payload":{"model":"gpt-5.5","cwd":"/home/user/project"}}"#,
                r#"{"timestamp":"2026-06-20T00:01:18Z","type":"event_msg","payload":{"type":"user_message","message":"Hello world"}}"#,
                r#"{"timestamp":"2026-06-20T00:01:19Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Hi there!"}]}}"#,
                r#"{"timestamp":"2026-06-20T00:01:22Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":1000,"cached_input_tokens":200,"output_tokens":500,"reasoning_output_tokens":100}}}}"#,
            ],
        );

        let entries = parse_codex_file(&path).unwrap();
        assert_eq!(entries.len(), 2);

        // User entry
        assert_eq!(entries[0].entry_type, EntryType::User);
        assert_eq!(entries[0].session_id.as_deref(), Some("abc-123"));
        assert_eq!(entries[0].cwd.as_deref(), Some("/home/user/project"));
        assert_eq!(
            entries[0].message.as_ref().unwrap().content.extract_text(),
            "Hello world"
        );

        // Assistant entry with usage
        assert_eq!(entries[1].entry_type, EntryType::Assistant);
        assert_eq!(
            entries[1].message.as_ref().unwrap().model.as_deref(),
            Some("gpt-5.5")
        );
        assert_eq!(
            entries[1].message.as_ref().unwrap().content.extract_text(),
            "Hi there!"
        );
        let usage = entries[1].message.as_ref().unwrap().usage.as_ref().unwrap();
        assert_eq!(usage.input_tokens, 1000);
        assert_eq!(usage.output_tokens, 500); // reasoning already included
        assert_eq!(usage.cache_read_input_tokens, 200);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_parse_with_tool_calls() {
        let tmp = std::env::temp_dir().join(format!("ccsight-codex-tools-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();

        let path = write_codex_session(
            &tmp,
            &[
                r#"{"timestamp":"2026-06-20T00:01:16Z","type":"session_meta","payload":{"id":"xyz","cwd":"/proj"}}"#,
                r#"{"timestamp":"2026-06-20T00:01:17Z","type":"turn_context","payload":{"model":"o3"}}"#,
                r#"{"timestamp":"2026-06-20T00:01:18Z","type":"event_msg","payload":{"type":"user_message","message":"Fix the bug"}}"#,
                r#"{"timestamp":"2026-06-20T00:01:19Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"ls\"}","call_id":"c1"}}"#,
                r#"{"timestamp":"2026-06-20T00:01:20Z","type":"response_item","payload":{"type":"function_call_output","output":"file.rs","call_id":"c1"}}"#,
                r#"{"timestamp":"2026-06-20T00:01:21Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Found it"}]}}"#,
            ],
        );

        let entries = parse_codex_file(&path).unwrap();
        assert_eq!(entries.len(), 2);

        let assistant = &entries[1];
        let blocks = match &assistant.message.as_ref().unwrap().content {
            MessageContent::Blocks(b) => b,
            other => panic!("expected Blocks, got {other:?}"),
        };
        assert!(blocks.len() >= 2);
        match &blocks[1] {
            ContentBlock::ToolUse { name, .. } => assert_eq!(name, "exec_command"),
            other => panic!("expected ToolUse, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_parse_empty_file() {
        let tmp = std::env::temp_dir().join(format!("ccsight-codex-empty-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("rollout-empty.jsonl");
        fs::write(&path, "").unwrap();

        let entries = parse_codex_file(&path).unwrap();
        assert!(entries.is_empty());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_parse_malformed_lines_skipped() {
        let tmp =
            std::env::temp_dir().join(format!("ccsight-codex-malform-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();

        let path = write_codex_session(
            &tmp,
            &[
                "not valid json",
                r#"{"timestamp":"2026-06-20T00:01:16Z","type":"session_meta","payload":{"id":"s1","cwd":"/p"}}"#,
                r#"{"timestamp":"2026-06-20T00:01:18Z","type":"event_msg","payload":{"type":"user_message","message":"Hi"}}"#,
            ],
        );

        let entries = parse_codex_file(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry_type, EntryType::User);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_parse_response_item_assistant_message() {
        let tmp =
            std::env::temp_dir().join(format!("ccsight-codex-resp-asst-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();

        let path = write_codex_session(
            &tmp,
            &[
                r#"{"timestamp":"2026-06-20T00:01:16Z","type":"session_meta","payload":{"id":"r1","cwd":"/proj"}}"#,
                r#"{"timestamp":"2026-06-20T00:01:17Z","type":"turn_context","payload":{"model":"gpt-5.5"}}"#,
                r#"{"timestamp":"2026-06-20T00:01:18Z","type":"event_msg","payload":{"type":"user_message","message":"Hello"}}"#,
                r#"{"timestamp":"2026-06-20T00:01:19Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Here is my response"}]}}"#,
            ],
        );

        let entries = parse_codex_file(&path).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].entry_type, EntryType::Assistant);
        assert_eq!(
            entries[1].message.as_ref().unwrap().content.extract_text(),
            "Here is my response"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_parse_tool_only_turn_creates_assistant_via_token_count() {
        let tmp =
            std::env::temp_dir().join(format!("ccsight-codex-toolonly-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();

        let path = write_codex_session(
            &tmp,
            &[
                r#"{"timestamp":"2026-06-20T00:01:16Z","type":"session_meta","payload":{"id":"t1","cwd":"/proj"}}"#,
                r#"{"timestamp":"2026-06-20T00:01:17Z","type":"turn_context","payload":{"model":"o3"}}"#,
                r#"{"timestamp":"2026-06-20T00:01:18Z","type":"event_msg","payload":{"type":"user_message","message":"Run tests"}}"#,
                r#"{"timestamp":"2026-06-20T00:01:19Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{}","call_id":"c1"}}"#,
                r#"{"timestamp":"2026-06-20T00:01:20Z","type":"response_item","payload":{"type":"function_call_output","output":"ok","call_id":"c1"}}"#,
                r#"{"timestamp":"2026-06-20T00:01:21Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":500,"cached_input_tokens":0,"output_tokens":200,"reasoning_output_tokens":50}}}}"#,
            ],
        );

        let entries = parse_codex_file(&path).unwrap();
        // User + synthetic assistant (created by ensure_assistant_entry)
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].entry_type, EntryType::Assistant);
        let usage = entries[1].message.as_ref().unwrap().usage.as_ref().unwrap();
        assert_eq!(usage.input_tokens, 500);
        assert_eq!(usage.output_tokens, 200);

        let _ = fs::remove_dir_all(&tmp);
    }
}
