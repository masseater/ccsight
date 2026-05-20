use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDate, Timelike, Utc};

use crate::aggregator::{StatsAggregator, TokenStats};
use crate::domain::EntryType;
use crate::infrastructure::Cache;
use crate::parser::JsonlParser;

pub type ModelTokens = TokenStats;

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub file_path: PathBuf,
    pub project_name: String,
    pub git_branch: Option<String>,
    pub session_first_timestamp: DateTime<Utc>,
    pub day_first_timestamp: DateTime<Utc>,
    pub day_last_timestamp: DateTime<Utc>,
    pub day_input_tokens: u64,
    pub day_output_tokens: u64,
    /// User-role messages submitted on this day. Counts only entries whose
    /// `message.role == User` (slash-command tokens included once per user
    /// turn). Used by the session-detail popup to show conversation depth.
    pub day_user_msgs: u64,
    /// Assistant-role messages emitted on this day. Counted per assistant
    /// entry regardless of whether it carried tool calls or just text.
    pub day_assistant_msgs: u64,
    pub day_tokens_by_model: HashMap<String, ModelTokens>,
    pub day_hourly_activity: HashMap<u8, u64>,
    pub day_hourly_work_tokens: HashMap<u8, u64>,
    pub day_tool_usage: HashMap<String, usize>,
    pub day_language_usage: HashMap<String, usize>,
    pub day_extension_usage: HashMap<String, usize>,
    pub summary: Option<String>,
    pub custom_title: Option<String>,
    pub ai_title: Option<String>,
    /// Most recent user-role message text, trimmed. Shown on Live/Daily row
    /// preview to help recognize "which session was this".
    pub last_user_message: Option<String>,
    /// First user-role message text. Acts as the always-available title
    /// fallback when ai_title / custom_title / summary / name are all
    /// absent — virtually every session has at least one user prompt, so
    /// this keeps line 2 populated with meaningful content instead of "—".
    pub first_user_message: Option<String>,
    pub model: Option<String>,
    pub is_subagent: bool,
    pub is_continued: bool,
}

impl SessionInfo {
    /// Sum of `day_input_tokens + day_output_tokens` — the "work" (non-cache)
    /// token total used everywhere we display a per-session token count.
    /// Centralised so callers don't keep re-typing the addition.
    pub fn work_tokens(&self) -> u64 {
        self.day_input_tokens + self.day_output_tokens
    }
}

#[derive(Debug, Clone)]
pub struct DailyGroup {
    pub date: NaiveDate,
    pub sessions: Vec<SessionInfo>,
}

impl DailyGroup {
    /// Iterator over sessions excluding subagents — the canonical "user-facing"
    /// session list. Most aggregations skip subagents to avoid double-counting
    /// nested activity that's already attributed to the parent session.
    pub fn user_sessions(&self) -> impl Iterator<Item = &SessionInfo> + '_ {
        self.sessions.iter().filter(|s| !s.is_subagent)
    }
}

pub struct DailyGrouper;

impl DailyGrouper {
    pub fn group_by_date_with_shared_cache(
        files: &[PathBuf],
        cache: &Option<Cache>,
    ) -> Vec<DailyGroup> {
        let mut date_map: HashMap<NaiveDate, Vec<SessionInfo>> = HashMap::new();

        // Global dedup across files: same (msg_id, requestId) can appear
        // in multiple session JSONLs because Claude Code rewrites prior
        // turns when a session is resumed/branched. Each API request is
        // billed once by Anthropic, so we must collapse the duplicates.
        // Pre-load with hashes already credited to cached files so a
        // fresh-parse file doesn't re-claim them.
        //
        // Restrict pre-load to files whose cache is STILL VALID for this
        // run: a stale cache entry (mtime changed → file about to be
        // re-parsed) would otherwise inject hashes into `global_seen`
        // that the fresh parse should be free to re-credit. Result was
        // those hashes' tokens dropped from totals entirely. Files no
        // longer on disk are excluded by the same `is_valid` check.
        let mut global_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(c) = cache {
            for file in files {
                if c.is_valid(file)
                    && let Some(cached) = c.get(file)
                {
                    for h in &cached.unique_hashes {
                        global_seen.insert(h.clone());
                    }
                }
            }
        }

        // Sorted iteration so fresh-parse ordering is deterministic across
        // runs — otherwise glob() order shifts attribute the same hash to
        // different files between runs, churning the cache without changing
        // totals.
        let mut sorted_files: Vec<&PathBuf> = files.iter().collect();
        sorted_files.sort();

        for file in &sorted_files {
            let sessions_by_date = Self::get_sessions_by_date(file, cache, &mut global_seen);
            for (date, session) in sessions_by_date {
                date_map.entry(date).or_default().push(session);
            }
        }

        for sessions in date_map.values_mut() {
            // Stable tiebreak by file_path so two sessions sharing the same
            // `day_last_timestamp` keep a deterministic order across reloads.
            sessions.sort_by(|a, b| {
                b.day_last_timestamp
                    .cmp(&a.day_last_timestamp)
                    .then_with(|| a.file_path.cmp(&b.file_path))
            });
        }

        let mut groups: Vec<DailyGroup> = date_map
            .into_iter()
            .map(|(date, sessions)| DailyGroup { date, sessions })
            .collect();

        groups.sort_by_key(|g| std::cmp::Reverse(g.date));
        groups
    }

    fn get_sessions_by_date(
        file: &Path,
        cache: &Option<Cache>,
        global_seen: &mut std::collections::HashSet<String>,
    ) -> Vec<(NaiveDate, SessionInfo)> {
        if let Some(c) = cache
            && c.is_valid(file)
            && let Some(cached) = c.get(file)
            && !cached.daily_stats.is_empty()
        {
            // Cache hit: hashes are already in `global_seen`
            // from the pre-load pass at the top of
            // `group_by_date_with_shared_cache`.
            return Self::build_sessions_from_cache(file, cached);
        }

        Self::parse_and_build_sessions(file, cache, global_seen)
    }

    fn build_sessions_from_cache(
        file: &Path,
        cached: &crate::infrastructure::CachedFileStats,
    ) -> Vec<(NaiveDate, SessionInfo)> {
        let project_name = cached
            .project_name
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        let session_first = cached.first_timestamp.unwrap_or_else(chrono::Utc::now);
        let session_start_date = cached
            .session_date
            .unwrap_or_else(|| session_first.with_timezone(&Local).date_naive());

        cached
            .daily_stats
            .iter()
            .filter_map(|(date_str, ds)| {
                let date = date_str.parse::<NaiveDate>().ok()?;
                let tokens_by_model: HashMap<String, ModelTokens> = ds
                    .tokens_by_model
                    .iter()
                    .map(|(model, ts)| (model.clone(), ts.clone()))
                    .collect();

                Some((
                    date,
                    SessionInfo {
                        file_path: file.to_path_buf(),
                        project_name: project_name.clone(),
                        git_branch: cached.git_branch.clone(),
                        session_first_timestamp: session_first,
                        day_first_timestamp: ds.first_timestamp.unwrap_or(session_first),
                        day_last_timestamp: ds.last_timestamp.unwrap_or(session_first),
                        day_input_tokens: ds.input_tokens,
                        day_output_tokens: ds.output_tokens,
                        day_user_msgs: ds.user_msgs,
                        day_assistant_msgs: ds.assistant_msgs,
                        day_tokens_by_model: tokens_by_model,
                        day_hourly_activity: ds.hourly_activity.clone(),
                        day_hourly_work_tokens: ds.hourly_work_activity.clone(),
                        day_tool_usage: ds.tool_usage.clone(),
                        day_language_usage: ds.language_usage.clone(),
                        day_extension_usage: ds.extension_usage.clone(),
                        summary: cached.summary.clone(),
                        custom_title: cached.custom_title.clone(),
                        ai_title: cached.ai_title.clone(),
                        last_user_message: cached.last_user_message.clone(),
                        first_user_message: cached.first_user_message.clone(),
                        model: cached.model.clone(),
                        is_subagent: cached.is_subagent,
                        is_continued: date != session_start_date,
                    },
                ))
            })
            .collect()
    }

    fn parse_and_build_sessions(
        file: &Path,
        cache: &Option<Cache>,
        global_seen: &mut std::collections::HashSet<String>,
    ) -> Vec<(NaiveDate, SessionInfo)> {
        let Ok(entries) = JsonlParser::parse_file(file) else {
            return vec![];
        };

        if entries.is_empty() {
            return vec![];
        }

        let entries_with_timestamp: Vec<_> =
            entries.iter().filter(|e| e.timestamp.is_some()).collect();

        if entries_with_timestamp.is_empty() {
            return vec![];
        }

        let session_first = entries_with_timestamp
            .first()
            .expect("entries_with_timestamp is not empty (checked above)")
            .timestamp
            .expect("entry has timestamp (filtered above)");
        let session_start_date = session_first.with_timezone(&Local).date_naive();

        // Project name resolution shares one helper with the cache writer in
        // `stats.rs`. For Cowork audit.jsonl this swaps the sandbox /
        // outputs-dir cwd for the metadata `processName`; non-cowork files
        // are returned unchanged.
        let project_name = crate::infrastructure::resolve_project_name(
            file,
            Self::extract_project_name_from_entries(&entries),
        )
        .unwrap_or_else(|| "unknown".to_string());

        // Branch can change mid-session (`git checkout`, worktree switch),
        // so use the LATEST entry's value to match the user's mental model
        // of "what branch was this session on". Other session-representative
        // fields (model, custom title) follow the same `rev().find_map(...)`
        // pattern.
        let git_branch = entries_with_timestamp
            .iter()
            .rev()
            .find_map(|e| e.git_branch.clone());
        let is_subagent_by_entry = entries_with_timestamp
            .first()
            .is_some_and(|e| e.is_sidechain);
        let is_subagent_by_filename = file
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("agent-"));
        let is_subagent = is_subagent_by_entry || is_subagent_by_filename;

        // One stat() instead of five: each per-field `c.is_valid(file)` was
        // a fresh `fs::metadata` syscall. Hoist the cache lookup to a
        // single Option borrow so the per-field branches reuse it.
        let cached = cache
            .as_ref()
            .filter(|c| c.is_valid(file))
            .and_then(|c| c.get(file));

        let summary = if let Some(cached) = cached {
            cached.summary.clone()
        } else {
            entries
                .iter()
                .rfind(|e| e.entry_type == EntryType::Summary)
                .and_then(|e| e.summary.clone())
        };

        let custom_title = if let Some(cached) = cached {
            cached.custom_title.clone()
        } else {
            entries
                .iter()
                .rfind(|e| e.entry_type == EntryType::CustomTitle)
                .and_then(|e| e.custom_title.clone())
        };
        // Cowork sessions don't emit a `customTitle` entry; pull the curated
        // `title` from sibling metadata json. Non-cowork files: returns None.
        let custom_title =
            custom_title.or_else(|| crate::infrastructure::resolve_cowork_title(file));

        // Anthropic-generated short title; preferred over custom_title and
        // summary in the display precedence.
        let ai_title = if let Some(cached) = cached {
            cached.ai_title.clone()
        } else {
            entries
                .iter()
                .rfind(|e| e.entry_type == EntryType::AiTitle)
                .and_then(|e| e.ai_title.clone())
        };

        // Last user-role message — drives the Live/Daily preview's
        // "remind me what I was doing" line. Cached so unchanged files skip
        // the entries walk.
        let last_user_message = if let Some(cached) = cached {
            cached.last_user_message.clone()
        } else {
            extract_last_user_message(&entries)
        };

        // First user-role message — used as the universal title fallback in
        // the Live tab when no other title source exists.
        let first_user_message = if let Some(cached) = cached {
            cached.first_user_message.clone()
        } else {
            extract_first_user_message(&entries)
        };

        let model = super::extract_session_model(&entries);

        let mut daily_stats: HashMap<NaiveDate, DailyStats> = HashMap::new();

        for entry in &entries {
            if let Some(ts) = entry.timestamp {
                let date = ts.with_timezone(&Local).date_naive();
                let stats = daily_stats.entry(date).or_insert_with(|| DailyStats {
                    first_timestamp: ts,
                    last_timestamp: ts,
                    input_tokens: 0,
                    output_tokens: 0,
                    user_msgs: 0,
                    assistant_msgs: 0,
                    tokens_by_model: HashMap::new(),
                    hourly_activity: HashMap::new(),
                    hourly_work_activity: HashMap::new(),
                    tool_usage: HashMap::new(),
                    language_usage: HashMap::new(),
                    extension_usage: HashMap::new(),
                });

                if ts < stats.first_timestamp {
                    stats.first_timestamp = ts;
                }
                if ts > stats.last_timestamp {
                    stats.last_timestamp = ts;
                }

                if let Some(ref message) = entry.message {
                    use crate::domain::MessageContent;
                    // Slash commands appear as `<command-name>/foo</command-name>`
                    // XML metadata in user message text — count them under `command:foo`
                    // so the per-day tool_usage reflects command invocations.
                    if matches!(message.role, crate::domain::Role::User) {
                        stats.user_msgs += 1;
                        let text = message.content.extract_text();
                        for cmd in crate::aggregator::stats::extract_command_names(&text) {
                            *stats
                                .tool_usage
                                .entry(format!("command:{cmd}"))
                                .or_insert(0) += 1;
                        }
                    } else if matches!(message.role, crate::domain::Role::Assistant) {
                        stats.assistant_msgs += 1;
                    }
                    if let MessageContent::Blocks(ref blocks) = message.content {
                        for block in blocks {
                            if let crate::domain::ContentBlock::ToolUse { name, input, .. } = block
                            {
                                let key = crate::aggregator::tool_usage_key(name, input);
                                *stats.tool_usage.entry(key).or_insert(0) += 1;
                                let extensions =
                                    StatsAggregator::extract_extensions_from_tool_input(
                                        name, input,
                                    );
                                if extensions.is_empty() {
                                    // Special filenames (Dockerfile, Makefile, etc.) carry a
                                    // language but no extension — count them under language only.
                                    if let Some(lang) =
                                        StatsAggregator::extract_language_from_tool_input(
                                            name, input,
                                        )
                                    {
                                        *stats
                                            .language_usage
                                            .entry(lang.to_string())
                                            .or_insert(0) += 1;
                                    }
                                } else {
                                    for ext in extensions {
                                        let lang = crate::aggregator::language::for_extension(&ext);
                                        *stats.extension_usage.entry(ext).or_insert(0) += 1;
                                        *stats
                                            .language_usage
                                            .entry(lang.to_string())
                                            .or_insert(0) += 1;
                                    }
                                }
                            }
                        }
                    }

                    if let Some(ref usage) = message.usage {
                        // Global dedup: same (msg_id, requestId) across
                        // multiple JSONLs (resume / branch) should bill
                        // once. Skip if another file already credited it.
                        if let Some(hash) = crate::parser::JsonlParser::entry_hash(entry) {
                            if !global_seen.insert(hash) {
                                continue;
                            }
                        }
                        let model_key = message
                            .model
                            .clone()
                            .unwrap_or_else(|| "unknown".to_string());
                        if !super::is_real_model(&model_key) {
                            continue;
                        }
                        stats.input_tokens += usage.input_tokens;
                        stats.output_tokens += usage.output_tokens;

                        let model_tokens = stats.tokens_by_model.entry(model_key).or_default();
                        model_tokens.add(usage);

                        let hour = ts.with_timezone(&Local).hour() as u8;
                        let tokens = usage.input_tokens
                            + usage.output_tokens
                            + usage.cache_creation_input_tokens
                            + usage.cache_read_input_tokens;
                        let work_tokens = usage.input_tokens + usage.output_tokens;
                        *stats.hourly_activity.entry(hour).or_insert(0) += tokens;
                        *stats.hourly_work_activity.entry(hour).or_insert(0) += work_tokens;
                    }
                }
            }
        }

        daily_stats
            .into_iter()
            .map(|(date, stats)| {
                let is_continued = date != session_start_date;
                (
                    date,
                    SessionInfo {
                        file_path: file.to_path_buf(),
                        project_name: project_name.clone(),
                        git_branch: git_branch.clone(),
                        session_first_timestamp: session_first,
                        day_first_timestamp: stats.first_timestamp,
                        day_last_timestamp: stats.last_timestamp,
                        day_input_tokens: stats.input_tokens,
                        day_output_tokens: stats.output_tokens,
                        day_user_msgs: stats.user_msgs,
                        day_assistant_msgs: stats.assistant_msgs,
                        day_tokens_by_model: stats.tokens_by_model,
                        day_hourly_activity: stats.hourly_activity,
                        day_hourly_work_tokens: stats.hourly_work_activity,
                        day_tool_usage: stats.tool_usage,
                        day_language_usage: stats.language_usage,
                        day_extension_usage: stats.extension_usage,
                        summary: summary.clone(),
                        custom_title: custom_title.clone(),
                        ai_title: ai_title.clone(),
                        last_user_message: last_user_message.clone(),
                        first_user_message: first_user_message.clone(),
                        model: model.clone(),
                        is_subagent,
                        is_continued,
                    },
                )
            })
            .collect()
    }

    fn extract_project_name_from_entries(entries: &[crate::domain::LogEntry]) -> Option<String> {
        super::extract_project_name(entries)
    }
}

/// Public-to-crate alias of [`extract_last_user_message`] so the stats path
/// (which fills the cache) can use the same cleanup logic as the grouper.
pub(crate) fn extract_last_user_message_for_cache(
    entries: &[crate::domain::LogEntry],
) -> Option<String> {
    extract_last_user_message(entries)
}

/// Same as [`extract_last_user_message_for_cache`] but for the *first*
/// user-role message — used as the universal title fallback so empty rows
/// stop showing "—".
pub(crate) fn extract_first_user_message_for_cache(
    entries: &[crate::domain::LogEntry],
) -> Option<String> {
    extract_first_user_message(entries)
}

/// Forward walk for the first user-role message; same cleanup pipeline as
/// the "last" variant so injected wrappers don't end up in the title.
fn extract_first_user_message(entries: &[crate::domain::LogEntry]) -> Option<String> {
    use crate::domain::Role;
    entries.iter().find_map(|e| {
        let msg = e.message.as_ref()?;
        if !matches!(msg.role, Role::User) {
            return None;
        }
        let raw = msg.content.extract_text();
        let cleaned = clean_user_message_preview(&raw);
        if cleaned.is_empty() || looks_like_system_injection(&cleaned) {
            return None;
        }
        Some(cleaned)
    })
}

/// Pull the most recent meaningful user-role text from the entry stream.
/// Strips well-known system-injected wrappers; for anything still bearing
/// an unrecognised kebab-case tag (`<local-command-caveat>`,
/// `<task-notification>`, etc.) skips the whole message and falls through
/// to the previous user entry — same approach as
/// `extract_first_user_message`.
fn extract_last_user_message(entries: &[crate::domain::LogEntry]) -> Option<String> {
    use crate::domain::Role;
    entries.iter().rev().find_map(|e| {
        let msg = e.message.as_ref()?;
        if !matches!(msg.role, Role::User) {
            return None;
        }
        let raw = msg.content.extract_text();
        let cleaned = clean_user_message_preview(&raw);
        if cleaned.is_empty() || looks_like_system_injection(&cleaned) {
            return None;
        }
        Some(cleaned)
    })
}

/// Heuristic: after the known-tag strip pass, any remaining `<kebab-case>`
/// marker (one or more `-` inside ASCII alphanumeric) is treated as an
/// unstripped system-injected wrapper. Single-word tags (`<foo>`) and
/// arrow-style prose (`x < 3` / `if a > b`) are NOT matched so genuine
/// user content with `<`/`>` is preserved.
fn looks_like_system_injection(s: &str) -> bool {
    let mut rest = s;
    while let Some(start) = rest.find('<') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('>') else {
            return false;
        };
        let inside = &rest[..end];
        let name = inside.trim_start_matches('/');
        let is_kebab_tag = name.contains('-')
            && !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if is_kebab_tag {
            return true;
        }
        rest = &rest[end + 1..];
    }
    false
}

/// Strip injected XML wrappers and collapse whitespace so the preview line
/// reads as a single natural-language snippet.
fn clean_user_message_preview(raw: &str) -> String {
    // Strip well-known injected tags. Treated greedily — these are emitted
    // by Claude Code / hooks and never contain user-authored prose worth
    // showing in a one-line preview.
    let strip_tags = |s: String, tag: &str| -> String {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        let mut out = String::new();
        let mut rest = s.as_str();
        while let Some(start) = rest.find(&open) {
            out.push_str(&rest[..start]);
            if let Some(end) = rest[start..].find(&close) {
                rest = &rest[start + end + close.len()..];
            } else {
                rest = &rest[start + open.len()..];
            }
        }
        out.push_str(rest);
        out
    };
    let mut s = raw.to_string();
    for tag in [
        "command-name",
        "command-message",
        "command-args",
        "system-reminder",
        "local-command-stdout",
        "user-prompt-submit-hook",
    ] {
        s = strip_tags(s, tag);
    }
    // Collapse all whitespace runs to single spaces so multi-line messages
    // become single-line previews.
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct DailyStats {
    first_timestamp: DateTime<Utc>,
    last_timestamp: DateTime<Utc>,
    input_tokens: u64,
    output_tokens: u64,
    user_msgs: u64,
    assistant_msgs: u64,
    tokens_by_model: HashMap<String, ModelTokens>,
    hourly_activity: HashMap<u8, u64>,
    hourly_work_activity: HashMap<u8, u64>,
    tool_usage: HashMap<String, usize>,
    language_usage: HashMap<String, usize>,
    extension_usage: HashMap<String, usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::CachedTokenStats;

    fn user_entry(text: &str) -> crate::domain::LogEntry {
        use crate::domain::{ContentBlock, EntryType, LogEntry, Message, MessageContent, Role};
        LogEntry {
            uuid: None,
            parent_uuid: None,
            session_id: None,
            timestamp: None,
            entry_type: EntryType::User,
            message: Some(Message {
                id: None,
                role: Role::User,
                content: MessageContent::Blocks(vec![ContentBlock::Text {
                    text: text.to_string(),
                }]),
                model: None,
                usage: None,
            }),
            summary: None,
            custom_title: None,
            ai_title: None,
            cwd: None,
            git_branch: None,
            version: None,
            is_sidechain: false,
            user_type: None,
            request_id: None,
        }
    }

    #[test]
    fn extract_first_user_message_returns_first_entry() {
        let entries = vec![
            user_entry("first prompt"),
            user_entry("middle"),
            user_entry("last"),
        ];
        assert_eq!(
            extract_first_user_message(&entries),
            Some("first prompt".to_string()),
        );
    }

    #[test]
    fn extract_last_user_message_returns_final_entry() {
        let entries = vec![
            user_entry("first"),
            user_entry("middle"),
            user_entry("final prompt"),
        ];
        assert_eq!(
            extract_last_user_message(&entries),
            Some("final prompt".to_string()),
        );
    }

    #[test]
    fn extract_user_message_strips_command_wrappers() {
        // System-injected command/system-reminder wrappers should be peeled
        // off so the preview reads as natural prose, not noise.
        let entries = vec![user_entry(
            "<command-name>compact</command-name>\nReal user text after the wrapper.",
        )];
        assert_eq!(
            extract_first_user_message(&entries),
            Some("Real user text after the wrapper.".to_string()),
        );
    }

    #[test]
    fn extract_user_message_skips_empty_after_cleanup() {
        // Pure command-wrapper messages collapse to "" after stripping —
        // the extractor should fall through to the next user entry.
        let entries = vec![
            user_entry("<command-name>foo</command-name>"),
            user_entry("real prompt"),
        ];
        assert_eq!(
            extract_first_user_message(&entries),
            Some("real prompt".to_string()),
        );
    }

    #[test]
    fn extract_user_message_returns_none_for_no_user_entries() {
        let entries: Vec<crate::domain::LogEntry> = vec![];
        assert_eq!(extract_first_user_message(&entries), None);
        assert_eq!(extract_last_user_message(&entries), None);
    }

    #[test]
    fn extract_user_message_skips_unknown_kebab_wrappers() {
        // Wrappers Claude Code may inject in the future (or that aren't on
        // the static strip list) should not bleed into the preview. The
        // extractor falls through to the next user message that's clean.
        let entries = vec![
            user_entry("<local-command-caveat>Caveat: DO NOT respond.</local-command-caveat>"),
            user_entry("<task-notification><task-id>abc</task-id></task-notification>"),
            user_entry("genuine human prompt"),
        ];
        assert_eq!(
            extract_first_user_message(&entries),
            Some("genuine human prompt".to_string()),
        );
        assert_eq!(
            extract_last_user_message(&entries),
            Some("genuine human prompt".to_string()),
        );
    }

    #[test]
    fn extract_user_message_preserves_arrow_prose() {
        // `if x < 3 then` / `<foo>` (single-word) are NOT kebab-cased
        // and so must NOT trigger the system-injection skip.
        let entries = vec![user_entry("compare x < 3 and y > 5 then <foo>")];
        assert_eq!(
            extract_first_user_message(&entries),
            Some("compare x < 3 and y > 5 then <foo>".to_string()),
        );
    }

    #[test]
    fn test_model_tokens_work_tokens() {
        let tokens = ModelTokens {
            input_tokens: 1000,
            output_tokens: 500,
            cache_creation_tokens: 200,
            cache_read_tokens: 300,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
        };
        assert_eq!(tokens.work_tokens(), 1500);
    }

    #[test]
    fn test_model_tokens_all_tokens() {
        let tokens = ModelTokens {
            input_tokens: 1000,
            output_tokens: 500,
            cache_creation_tokens: 200,
            cache_read_tokens: 300,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
        };
        assert_eq!(tokens.all_tokens(), 2000);
    }

    #[test]
    fn test_model_tokens_all_tokens_zero() {
        let tokens = ModelTokens::default();
        assert_eq!(tokens.all_tokens(), 0);
    }

    #[test]
    fn test_model_tokens_from_cached_token_stats() {
        let cached = CachedTokenStats {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_tokens: 20,
            cache_read_tokens: 10,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
        };
        let model_tokens = cached.clone();

        assert_eq!(model_tokens.input_tokens, 100);
        assert_eq!(model_tokens.output_tokens, 50);
        assert_eq!(model_tokens.cache_creation_tokens, 20);
        assert_eq!(model_tokens.cache_read_tokens, 10);
        assert_eq!(model_tokens.all_tokens(), 180);
    }

    #[test]
    fn test_format_project_path_users() {
        assert_eq!(
            super::super::format_project_path("/Users/alice/work/myapp"),
            "~/work/myapp"
        );
    }

    #[test]
    fn test_format_project_path_users_short() {
        assert_eq!(super::super::format_project_path("/Users/bob"), "~/bob");
    }

    #[test]
    fn test_format_project_path_non_users() {
        assert_eq!(
            super::super::format_project_path("/opt/project"),
            "/opt/project"
        );
    }

    #[test]
    fn test_format_project_path_nested() {
        assert_eq!(
            super::super::format_project_path("/Users/charlie/workspace/deep/nested"),
            "~/workspace/deep/nested"
        );
    }

    #[test]
    fn test_format_project_path_linux_home() {
        assert_eq!(
            super::super::format_project_path("/home/alice/work/myapp"),
            "~/work/myapp"
        );
    }

    #[test]
    fn test_format_project_path_linux_home_short() {
        assert_eq!(super::super::format_project_path("/home/bob"), "~/bob");
    }

    #[test]
    fn test_extract_project_name_from_entries() {
        let entries = vec![crate::domain::LogEntry {
            uuid: None,
            parent_uuid: None,
            session_id: None,
            timestamp: None,
            entry_type: crate::domain::EntryType::User,
            message: None,
            summary: None,
            custom_title: None,
            ai_title: None,
            cwd: Some("/Users/test/projects/myproject".to_string()),
            git_branch: None,
            version: None,
            is_sidechain: false,
            user_type: None,
            request_id: None,
        }];
        let result = DailyGrouper::extract_project_name_from_entries(&entries);
        assert_eq!(result, Some("~/projects/myproject".to_string()));
    }

    #[test]
    fn test_extract_project_name_no_cwd() {
        let entries = vec![crate::domain::LogEntry {
            uuid: None,
            parent_uuid: None,
            session_id: None,
            timestamp: None,
            entry_type: crate::domain::EntryType::User,
            message: None,
            summary: None,
            custom_title: None,
            ai_title: None,
            cwd: None,
            git_branch: None,
            version: None,
            is_sidechain: false,
            user_type: None,
            request_id: None,
        }];
        let result = DailyGrouper::extract_project_name_from_entries(&entries);
        assert_eq!(result, None);
    }

    use crate::infrastructure::{CachedDailyStats, CachedFileStats};

    fn make_cached_file_stats(daily_stats: HashMap<String, CachedDailyStats>) -> CachedFileStats {
        CachedFileStats {
            modified_secs: 0,
            file_size: 0,
            entry_count: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            unique_hashes: Vec::new(),
            cache_read_tokens: 0,
            tool_usage: HashMap::new(),
            model_usage: HashMap::new(),
            model_tokens: HashMap::new(),
            session_date: None,
            project_name: None,
            session_id: None,
            git_branch: None,
            first_timestamp: None,
            last_timestamp: None,
            summary: None,
            custom_title: None,
            ai_title: None,
            last_user_message: None,
            first_user_message: None,
            model: None,
            is_subagent: false,
            daily_stats,
            hourly_activity: HashMap::new(),
            hourly_work_activity: HashMap::new(),
            weekday_activity: HashMap::new(),
            weekday_work_activity: HashMap::new(),
            tool_error_count: 0,
            tool_success_count: 0,
            session_duration_mins: None,
            language_usage: HashMap::new(),
            extension_usage: HashMap::new(),
        }
    }

    fn make_cached_daily(input: u64, output: u64) -> CachedDailyStats {
        CachedDailyStats {
            first_timestamp: Some(chrono::Utc::now()),
            last_timestamp: Some(chrono::Utc::now()),
            input_tokens: input,
            output_tokens: output,
            tokens_by_model: HashMap::new(),
            hourly_activity: HashMap::new(),
            hourly_work_activity: HashMap::new(),
            tool_usage: HashMap::new(),
            language_usage: HashMap::new(),
            extension_usage: HashMap::new(),
            user_msgs: 0,
            assistant_msgs: 0,
        }
    }

    #[test]
    fn test_build_sessions_from_cache_basic() {
        let mut tokens_by_model = HashMap::new();
        tokens_by_model.insert(
            "claude-sonnet-4".to_string(),
            CachedTokenStats {
                input_tokens: 1000,
                output_tokens: 500,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                cache_creation_5m_tokens: 0,
                cache_creation_1h_tokens: 0,
            },
        );

        let mut ds = make_cached_daily(1000, 500);
        ds.tokens_by_model = tokens_by_model;
        let mut daily_stats = HashMap::new();
        daily_stats.insert("2026-02-25".to_string(), ds); // lint-ok: date-literal

        let mut cached = make_cached_file_stats(daily_stats);
        cached.entry_count = 10;
        cached.input_tokens = 1000;
        cached.output_tokens = 500;
        cached.session_date = Some(NaiveDate::from_ymd_opt(2026, 2, 25).unwrap()); // lint-ok: date-literal
        cached.project_name = Some("~/projects/test".to_string());
        cached.git_branch = Some("main".to_string());
        cached.first_timestamp = Some(chrono::Utc::now());
        cached.last_timestamp = Some(chrono::Utc::now());
        cached.summary = Some("test summary".to_string());
        cached.model = Some("claude-sonnet-4".to_string());

        let file = Path::new("/tmp/test_session.jsonl");
        let sessions = DailyGrouper::build_sessions_from_cache(file, &cached);

        assert_eq!(sessions.len(), 1);
        let (date, session) = &sessions[0];
        assert_eq!(*date, NaiveDate::from_ymd_opt(2026, 2, 25).unwrap()); // lint-ok: date-literal
        assert_eq!(session.project_name, "~/projects/test");
        assert_eq!(session.git_branch.as_deref(), Some("main"));
        assert_eq!(session.summary.as_deref(), Some("test summary"));
        assert_eq!(session.day_input_tokens, 1000);
        assert_eq!(session.day_output_tokens, 500);
        assert!(!session.is_subagent);
        assert!(!session.is_continued);
    }

    #[test]
    fn test_build_sessions_from_cache_multi_day() {
        let mut daily_stats = HashMap::new();
        daily_stats.insert("2026-02-24".to_string(), make_cached_daily(500, 200)); // lint-ok: date-literal
        daily_stats.insert("2026-02-25".to_string(), make_cached_daily(800, 300)); // lint-ok: date-literal

        let mut cached = make_cached_file_stats(daily_stats);
        cached.session_date = Some(NaiveDate::from_ymd_opt(2026, 2, 24).unwrap()); // lint-ok: date-literal
        cached.project_name = Some("~/projects/multi".to_string());
        cached.first_timestamp = Some(chrono::Utc::now());

        let file = Path::new("/tmp/test_multi.jsonl");
        let sessions = DailyGrouper::build_sessions_from_cache(file, &cached);

        assert_eq!(sessions.len(), 2);
        let has_continued = sessions.iter().any(|(_, s)| s.is_continued);
        let has_original = sessions.iter().any(|(_, s)| !s.is_continued);
        assert!(
            has_continued,
            "multi-day session should have continued entry"
        );
        assert!(has_original, "multi-day session should have original entry");
    }

    #[test]
    fn test_build_sessions_from_cache_no_project_name() {
        let mut ds = make_cached_daily(100, 50);
        ds.first_timestamp = None;
        ds.last_timestamp = None;
        let mut daily_stats = HashMap::new();
        daily_stats.insert("2026-02-25".to_string(), ds); // lint-ok: date-literal

        let cached = make_cached_file_stats(daily_stats);

        let file = Path::new("/tmp/test_noname.jsonl");
        let sessions = DailyGrouper::build_sessions_from_cache(file, &cached);

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].1.project_name, "unknown");
    }

    #[test]
    fn test_build_sessions_from_cache_subagent() {
        let mut ds = make_cached_daily(100, 50);
        ds.first_timestamp = None;
        ds.last_timestamp = None;
        let mut daily_stats = HashMap::new();
        daily_stats.insert("2026-02-25".to_string(), ds); // lint-ok: date-literal

        let mut cached = make_cached_file_stats(daily_stats);
        cached.project_name = Some("~/projects/task".to_string());
        cached.is_subagent = true;

        let file = Path::new("/tmp/agent-test.jsonl");
        let sessions = DailyGrouper::build_sessions_from_cache(file, &cached);

        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].1.is_subagent);
    }

    #[test]
    fn test_parse_and_build_sessions_basic() {
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("ccsight_grouping_test_{id}.jsonl"));

        let entries = vec![
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-02-25T10:00:00Z",
                "cwd": "/Users/test/projects/myproject",
                "message": {"role": "user", "content": "hello"}
            }),
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-02-25T10:01:00Z",
                "message": {
                    "role": "assistant",
                    "content": "hi",
                    "model": "claude-sonnet-4-20250514",
                    "usage": {"input_tokens": 100, "output_tokens": 50}
                }
            }),
        ];

        {
            let mut f = std::fs::File::create(&path).unwrap();
            for e in &entries {
                writeln!(f, "{}", serde_json::to_string(e).unwrap()).unwrap();
            }
        }

        let sessions = DailyGrouper::parse_and_build_sessions(
            &path,
            &None,
            &mut std::collections::HashSet::new(),
        );
        std::fs::remove_file(&path).ok();

        assert_eq!(sessions.len(), 1);
        let (_, session) = &sessions[0];
        assert_eq!(session.project_name, "~/projects/myproject");
        assert_eq!(session.day_input_tokens, 100);
        assert_eq!(session.day_output_tokens, 50);
        assert!(!session.is_subagent);
    }

    #[test]
    fn test_parse_and_build_sessions_empty_file() {
        let path = std::env::temp_dir().join("ccsight_grouping_empty.jsonl");
        {
            std::fs::File::create(&path).unwrap();
        }
        let sessions = DailyGrouper::parse_and_build_sessions(
            &path,
            &None,
            &mut std::collections::HashSet::new(),
        );
        std::fs::remove_file(&path).ok();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_parse_and_build_sessions_subagent_by_filename() {
        use std::io::Write;
        let path = std::env::temp_dir().join("agent-ccsight_subagent_test.jsonl");
        let entries = vec![serde_json::json!({
            "type": "user",
            "timestamp": "2026-02-25T10:00:00Z",
            "cwd": "/Users/test/projects/task",
            "message": {"role": "user", "content": "do task"}
        })];
        {
            let mut f = std::fs::File::create(&path).unwrap();
            for e in &entries {
                writeln!(f, "{}", serde_json::to_string(e).unwrap()).unwrap();
            }
        }
        let sessions = DailyGrouper::parse_and_build_sessions(
            &path,
            &None,
            &mut std::collections::HashSet::new(),
        );
        std::fs::remove_file(&path).ok();

        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].1.is_subagent);
    }
}
