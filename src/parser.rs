use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};

use crate::domain::LogEntry;

// Conservative limits to prevent DoS from malformed files
// These can be overridden via environment variables if needed
// Validated ranges: file_size [1MB, 2GB], line_size [1KB, 100MB], entries [100, 1M]

// Large session JSONLs (~hundreds of MB) appear in normal heavy use,
// so the default must accommodate them or token totals silently drop
// the entries from over-cap files. Streaming parse keeps memory bounded
// by `max_line_size` regardless of file size, so raising the cap costs
// only parse time, not RAM.
const DEFAULT_MAX_FILE_SIZE: u64 = 2 * 1024 * 1024 * 1024; // 2GB
const MIN_MAX_FILE_SIZE: u64 = 1024 * 1024; // 1MB
const MAX_MAX_FILE_SIZE: u64 = 2 * 1024 * 1024 * 1024; // 2GB

const DEFAULT_MAX_LINE_SIZE: usize = 50 * 1024 * 1024; // 50MB
const MIN_MAX_LINE_SIZE: usize = 1024; // 1KB
const MAX_MAX_LINE_SIZE: usize = 100 * 1024 * 1024; // 100MB

const DEFAULT_MAX_ENTRIES: usize = 100_000;
const MIN_MAX_ENTRIES: usize = 100;
const MAX_MAX_ENTRIES: usize = 1_000_000;

fn max_file_size() -> u64 {
    std::env::var("CCSIGHT_MAX_FILE_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .map_or(DEFAULT_MAX_FILE_SIZE, |v: u64| {
            v.clamp(MIN_MAX_FILE_SIZE, MAX_MAX_FILE_SIZE)
        })
}

fn max_line_size() -> usize {
    std::env::var("CCSIGHT_MAX_LINE_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .map_or(DEFAULT_MAX_LINE_SIZE, |v: usize| {
            v.clamp(MIN_MAX_LINE_SIZE, MAX_MAX_LINE_SIZE)
        })
}

fn max_entries() -> usize {
    std::env::var("CCSIGHT_MAX_ENTRIES")
        .ok()
        .and_then(|s| s.parse().ok())
        .map_or(DEFAULT_MAX_ENTRIES, |v: usize| {
            v.clamp(MIN_MAX_ENTRIES, MAX_MAX_ENTRIES)
        })
}

pub struct JsonlParser;

impl JsonlParser {
    pub fn parse_file(path: &Path) -> Result<Vec<LogEntry>> {
        let file =
            File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;

        let metadata = file.metadata()?;
        let file_size_limit = max_file_size();
        if metadata.len() > file_size_limit {
            anyhow::bail!(
                "File too large: {} bytes (max {} bytes)",
                metadata.len(),
                file_size_limit
            );
        }

        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        let mut hash_to_index: HashMap<String, usize> = HashMap::new();
        let entry_limit = max_entries();
        let line_size_limit = max_line_size();

        for line_result in reader.lines() {
            if entries.len() >= entry_limit {
                break;
            }

            let Ok(line) = line_result else {
                continue;
            };

            if line.len() > line_size_limit {
                continue;
            }

            if line.trim().is_empty() {
                continue;
            }

            let Ok(mut entry) = serde_json::from_str::<LogEntry>(&line) else {
                continue;
            };

            // Cowork `audit.jsonl` puts the wall-clock time in `_audit_timestamp`
            // and leaves the standard `timestamp` field null. Fill the gap so
            // downstream date-grouping / hourly aggregation works without
            // every consumer needing to know about the alternate field.
            if entry.timestamp.is_none()
                && let Ok(value) = serde_json::from_str::<serde_json::Value>(&line)
                && let Some(ts_str) = value.get("_audit_timestamp").and_then(|v| v.as_str())
                && let Ok(ts) = chrono::DateTime::parse_from_rfc3339(ts_str)
            {
                entry.timestamp = Some(ts.with_timezone(&chrono::Utc));
            }

            if let Some(hash) = Self::create_dedup_hash(&entry) {
                if let Some(&existing_idx) = hash_to_index.get(&hash) {
                    entries[existing_idx] = entry;
                } else {
                    hash_to_index.insert(hash, entries.len());
                    entries.push(entry);
                }
            } else {
                entries.push(entry);
            }
        }

        Ok(entries)
    }

    pub fn entry_hash(entry: &LogEntry) -> Option<String> {
        Self::create_dedup_hash(entry)
    }

    fn create_dedup_hash(entry: &LogEntry) -> Option<String> {
        let request_id = entry.request_id.as_ref()?;
        let message_id = entry.message.as_ref()?.id.as_ref()?;
        Some(format!("{message_id}:{request_id}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_line() {
        let json = r#"{"uuid":"123","timestamp":"2025-01-01T00:00:00Z","type":"user","message":{"role":"user","content":"hello"}}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.uuid, Some("123".to_string()));
    }

    #[test]
    fn test_parse_summary_entry() {
        let json = r#"{"type":"summary","summary":"Test summary","leafUuid":"abc"}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.summary, Some("Test summary".to_string()));
        assert!(entry.message.is_none());
    }

    #[test]
    fn test_parse_file_history_snapshot() {
        let json = r#"{"type":"file-history-snapshot","messageId":"123","snapshot":{}}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        assert_eq!(
            entry.entry_type,
            crate::domain::EntryType::FileHistorySnapshot
        );
    }

    #[test]
    fn test_audit_timestamp_fallback() {
        // Cowork audit.jsonl emits `timestamp: null` and carries the wall-clock
        // time in `_audit_timestamp` — the parser should backfill so date
        // bucketing works on these files just like Claude Code JSONL.
        let dir = std::env::temp_dir().join(format!("ccsight-parser-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        let line = r#"{"type":"assistant","uuid":"u1","session_id":"s1","timestamp":null,"_audit_timestamp":"2026-04-27T01:23:45.000Z","message":{"role":"assistant","content":"hi"}}"#;
        std::fs::write(&path, format!("{line}\n")).unwrap();

        let entries = JsonlParser::parse_file(&path).unwrap();
        assert_eq!(entries.len(), 1);
        let ts = entries[0]
            .timestamp
            .expect("timestamp filled from _audit_timestamp");
        assert_eq!(ts.to_rfc3339(), "2026-04-27T01:23:45+00:00");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_unknown_type() {
        let json = r#"{"type":"some-unknown-type","data":"test"}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.entry_type, crate::domain::EntryType::Unknown);
    }

    /// Integration test against the developer's local `~/.claude/projects` directory.
    /// Marked `#[ignore]` because results depend on the runner's environment (CI may have
    /// no logs at all). Run explicitly with `cargo test -- --ignored test_parse_actual_files`.
    #[test]
    #[ignore]
    fn test_parse_actual_files() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let projects_dir = format!("{}/.claude/projects", home.to_string_lossy());

        if std::path::Path::new(&projects_dir).exists() {
            let pattern = format!("{}/*/*.jsonl", projects_dir);
            let files: Vec<_> = glob::glob(&pattern)
                .unwrap()
                .filter_map(|r| r.ok())
                .take(5)
                .collect();

            let mut total_entries = 0;
            let mut total_errors = 0;

            for file in &files {
                match JsonlParser::parse_file(file) {
                    Ok(entries) => {
                        total_entries += entries.len();
                    }
                    Err(_) => {
                        total_errors += 1;
                        // JSONL parse failure is non-fatal
                    }
                }
            }

            println!(
                "Parsed {} entries from {} files ({} errors)",
                total_entries,
                files.len(),
                total_errors
            );
            assert!(total_entries > 0, "Should parse at least some entries");
        }
    }
}
