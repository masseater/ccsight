use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Local, NaiveDate, Timelike, Utc, Weekday};
use serde::{Deserialize, Serialize};

use crate::domain::{EntryType, LogEntry, Usage};
use crate::infrastructure::{
    Cache, CachedDailyStats, CachedFileStats, CachedTokenStats, get_file_modified_secs,
    get_file_size,
};
use crate::parser::JsonlParser;

/// Pull the slash-command name out of a Claude Code "command invocation"
/// user message. Real invocations have a tiny body that is wholly the XML
/// metadata block (`<command-name>`, `<command-message>`, `<command-args>`),
/// so we accept a message only when:
///
/// 1. The message contains **exactly one** `<command-name>` tag (summary
///    regeneration prompts list dozens of historical commands as text and
///    must not be counted), AND
/// 2. The body is short (< 500 chars — real invocations are ~100-200; the
///    cap rejects large prompts that happen to contain a single tag).
///
/// Returns at most one command name (without the leading `/`).
pub(crate) fn extract_command_names(text: &str) -> Vec<String> {
    if text.len() >= 500 {
        return Vec::new();
    }
    if text.matches("<command-name>").count() != 1 {
        return Vec::new();
    }
    let Some(open) = text.find("<command-name>") else {
        return Vec::new();
    };
    let after_open = &text[open + "<command-name>".len()..];
    let Some(close) = after_open.find("</command-name>") else {
        return Vec::new();
    };
    let raw = after_open[..close].trim();
    let name = raw.strip_prefix('/').unwrap_or(raw);
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':')
    {
        return Vec::new();
    }
    vec![name.to_string()]
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub total_entries: usize,
    pub total_tokens: TokenStats,
    pub tool_usage: HashMap<String, usize>,
    pub model_usage: HashMap<String, usize>,
    pub model_tokens: HashMap<String, TokenStats>,
    pub daily_activity: HashMap<NaiveDate, u64>,
    pub daily_work_activity: HashMap<NaiveDate, u64>,
    pub project_stats: HashMap<String, ProjectStats>,
    pub hourly_activity: HashMap<u8, u64>,
    pub hourly_work_activity: HashMap<u8, u64>,
    pub weekday_activity: HashMap<Weekday, u64>,
    pub weekday_work_activity: HashMap<Weekday, u64>,
    pub tool_error_count: usize,
    pub tool_success_count: usize,
    pub sessions_with_summary: usize,
    pub total_sessions_count: usize,
    pub branch_stats: HashMap<String, BranchStats>,
    pub language_usage: HashMap<String, usize>,
    pub extension_usage: HashMap<String, usize>,
    /// Per-tool/Skills/Subagents/MCP key: distinct session-days that used this key at least once.
    pub tool_sessions: HashMap<String, usize>,
    /// Per-MCP-server: distinct session-days that used at least one tool from this server.
    /// A session that uses two tools from the same server still counts as 1. Keyed by the
    /// normalized server name (e.g. `serverA`, `orgA/serverB` for plugin-form servers).
    pub mcp_server_sessions: HashMap<String, usize>,
    /// Session-days that used at least one Skills entry (key starts with `skill:`).
    pub sessions_using_skills: usize,
    /// Session-days that used at least one Subagents entry (key starts with `agent:`).
    pub sessions_using_subagents: usize,
    /// Session-days that used at least one MCP tool (key starts with `mcp__`).
    pub sessions_using_mcp: usize,
    /// Session-days that invoked at least one slash command (key starts with `command:`).
    pub sessions_using_commands: usize,
    /// Total session-days (non-subagent). Denominator for adoption rate calculations.
    pub total_session_days: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BranchStats {
    pub session_count: usize,
    pub total_duration_mins: i64,
    pub first_seen: Option<DateTime<Utc>>,
    pub last_seen: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectStats {
    pub sessions: usize,
    pub tokens: u64,
    pub work_tokens: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TokenStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Sum of 5m + 1h cache writes. Kept as a single field so existing UI /
    /// MCP / aggregation code that reads "cache creation tokens" stays
    /// correct. Cost math uses the per-TTL split below.
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    /// 5-minute TTL portion of `cache_creation_tokens`. Billed at 1.25x
    /// base input. Default 0 lets old cache files deserialize cleanly.
    #[serde(default)]
    pub cache_creation_5m_tokens: u64,
    /// 1-hour TTL portion of `cache_creation_tokens`. Billed at 2x base
    /// input. Claude subscription users (Pro/Max/Team) get this TTL by
    /// default, so on real data this is typically the larger half.
    #[serde(default)]
    pub cache_creation_1h_tokens: u64,
}

impl TokenStats {
    pub fn work_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    pub fn all_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_creation_tokens + self.cache_read_tokens
    }

    pub fn add(&mut self, usage: &Usage) {
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        self.cache_creation_tokens += usage.cache_creation_input_tokens;
        self.cache_read_tokens += usage.cache_read_input_tokens;
        if let Some(ref cc) = usage.cache_creation {
            self.cache_creation_5m_tokens += cc.ephemeral_5m_input_tokens;
            self.cache_creation_1h_tokens += cc.ephemeral_1h_input_tokens;
        } else {
            // JSONL without the structured `cache_creation` breakdown:
            // treat the flat aggregate as 5m TTL (the Anthropic API
            // default). Build flag, not a time question.
            self.cache_creation_5m_tokens += usage.cache_creation_input_tokens;
        }
    }
}

pub struct StatsAggregator;

#[derive(Debug, Clone, Default)]
struct DailyStats {
    first_timestamp: Option<DateTime<Utc>>,
    last_timestamp: Option<DateTime<Utc>>,
    input_tokens: u64,
    output_tokens: u64,
    user_msgs: u64,
    assistant_msgs: u64,
    tokens_by_model: HashMap<String, TokenStats>,
    hourly_activity: HashMap<u8, u64>,
    hourly_work_activity: HashMap<u8, u64>,
    tool_usage: HashMap<String, usize>,
    language_usage: HashMap<String, usize>,
    extension_usage: HashMap<String, usize>,
}

#[derive(Debug, Clone, Default)]
struct FileStats {
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_creation_5m_tokens: u64,
    cache_creation_1h_tokens: u64,
    cache_read_tokens: u64,
    /// Hashes that this file uniquely contributed (i.e. were NOT already in
    /// the global_seen set when this file was parsed). Saved to cache so
    /// next run's pre-load can rebuild global_seen without re-parsing.
    unique_hashes: Vec<String>,
    tool_usage: HashMap<String, usize>,
    model_usage: HashMap<String, usize>,
    model_tokens: HashMap<String, TokenStats>,
    session_date: Option<NaiveDate>,
    session_id: Option<String>,
    git_branch: Option<String>,
    first_timestamp: Option<DateTime<Utc>>,
    last_timestamp: Option<DateTime<Utc>>,
    summary: Option<String>,
    custom_title: Option<String>,
    ai_title: Option<String>,
    last_user_message: Option<String>,
    first_user_message: Option<String>,
    model: Option<String>,
    is_subagent: bool,
    daily_stats: HashMap<NaiveDate, DailyStats>,
    hourly_activity: HashMap<u8, u64>,
    hourly_work_activity: HashMap<u8, u64>,
    weekday_activity: HashMap<u8, u64>,
    weekday_work_activity: HashMap<u8, u64>,
    tool_error_count: usize,
    tool_success_count: usize,
    session_duration_mins: Option<i64>,
    language_usage: HashMap<String, usize>,
    extension_usage: HashMap<String, usize>,
}

pub struct CacheStats {
    pub cached_files: usize,
    pub parsed_files: usize,
}

impl StatsAggregator {
    /// For one session-day (iterator over the keys used in that day), increment adoption
    /// counters and per-tool `tool_sessions` in `stats`. Each key in the iterator contributes
    /// exactly +1 to `tool_sessions[key]`. The `sessions_using_*` counters are incremented at
    /// most once per call (regardless of how many keys of a category appear).
    pub(crate) fn add_session_adoption<'a, I: IntoIterator<Item = &'a String>>(
        stats: &mut Stats,
        keys: I,
    ) {
        let mut used_skill = false;
        let mut used_subagent = false;
        let mut used_mcp = false;
        let mut used_command = false;
        // Collect MCP servers touched in this session-day. Using a local set so a session
        // that hits two tools from the same server increments `mcp_server_sessions` only
        // once.
        let mut servers_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for key in keys {
            if key.starts_with("skill:") {
                used_skill = true;
            } else if key.starts_with("agent:") {
                used_subagent = true;
            } else if key.starts_with("command:") {
                used_command = true;
            } else if key.starts_with("mcp__") {
                used_mcp = true;
                if let Some(server) = crate::aggregator::mcp_server_of(key) {
                    servers_seen.insert(server);
                }
            }
            *stats.tool_sessions.entry(key.clone()).or_insert(0) += 1;
        }
        if used_skill {
            stats.sessions_using_skills += 1;
        }
        if used_subagent {
            stats.sessions_using_subagents += 1;
        }
        if used_mcp {
            stats.sessions_using_mcp += 1;
        }
        if used_command {
            stats.sessions_using_commands += 1;
        }
        for server in servers_seen {
            *stats.mcp_server_sessions.entry(server).or_insert(0) += 1;
        }
    }

    pub fn aggregate_with_shared_cache(files: &[PathBuf], mut cache: Cache) -> (Stats, CacheStats) {
        let mut stats = Stats::default();

        let mut cache_stats = CacheStats {
            cached_files: 0,
            parsed_files: 0,
        };

        // Pre-load global-dedup set from cache. Mirrors the grouping.rs
        // pass: same (msg_id, requestId) across multiple JSONLs must bill
        // once. Restrict pre-load to STILL-VALID cache entries — a stale
        // entry's hashes belong to a file that is about to be re-parsed
        // and must be free to re-credit them.
        let mut global_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for file in files {
            if cache.is_valid(file)
                && let Some(cached) = cache.get(file)
            {
                for h in &cached.unique_hashes {
                    global_seen.insert(h.clone());
                }
            }
        }

        // Sorted iteration for deterministic hash attribution across runs.
        let mut sorted_files: Vec<&PathBuf> = files.iter().collect();
        sorted_files.sort();

        for file in &sorted_files {
            if cache.is_valid(file)
                && let Some(cached) = cache.get(file)
            {
                let project_name = cached.project_name.clone();
                Self::merge_cached_stats(&mut stats, cached, &project_name);
                cache_stats.cached_files += 1;
                continue;
            }

            if let Ok(entries) = JsonlParser::parse_file(file) {
                // For Cowork audit.jsonl the in-stream `cwd` is meaningless
                // (sandbox / outputs subdir); `resolve_project_name` swaps in
                // the sibling metadata's `processName`. Non-cowork files are
                // unaffected. This is the single source of truth used by both
                // the cache writer here and the grouper in `grouping.rs`.
                let project_name = crate::infrastructure::resolve_project_name(
                    file,
                    Self::extract_project_name_from_entries(&entries),
                );
                let file_stats = Self::aggregate_file_entries(
                    &mut stats,
                    &entries,
                    &project_name,
                    file,
                    &mut global_seen,
                );

                let model_tokens_cached: HashMap<String, CachedTokenStats> = file_stats
                    .model_tokens
                    .iter()
                    .map(|(model, ts)| (model.clone(), ts.clone()))
                    .collect();

                let daily_stats_cached: HashMap<String, CachedDailyStats> = file_stats
                    .daily_stats
                    .into_iter()
                    .map(|(date, ds)| {
                        let tokens_by_model = ds
                            .tokens_by_model
                            .iter()
                            .map(|(model, ts)| (model.clone(), ts.clone()))
                            .collect();
                        (
                            date.to_string(),
                            CachedDailyStats {
                                first_timestamp: ds.first_timestamp,
                                last_timestamp: ds.last_timestamp,
                                input_tokens: ds.input_tokens,
                                output_tokens: ds.output_tokens,
                                tokens_by_model,
                                hourly_activity: ds.hourly_activity,
                                hourly_work_activity: ds.hourly_work_activity,
                                tool_usage: ds.tool_usage,
                                language_usage: ds.language_usage,
                                extension_usage: ds.extension_usage,
                                user_msgs: ds.user_msgs,
                                assistant_msgs: ds.assistant_msgs,
                            },
                        )
                    })
                    .collect();

                let cached_file_stats = CachedFileStats {
                    modified_secs: get_file_modified_secs(file),
                    file_size: get_file_size(file),
                    entry_count: entries.len(),
                    input_tokens: file_stats.input_tokens,
                    output_tokens: file_stats.output_tokens,
                    cache_creation_tokens: file_stats.cache_creation_tokens,
                    cache_creation_5m_tokens: file_stats.cache_creation_5m_tokens,
                    cache_creation_1h_tokens: file_stats.cache_creation_1h_tokens,
                    cache_read_tokens: file_stats.cache_read_tokens,
                    unique_hashes: file_stats.unique_hashes,
                    tool_usage: file_stats.tool_usage,
                    model_usage: file_stats.model_usage,
                    model_tokens: model_tokens_cached,
                    session_date: file_stats.session_date,
                    project_name: project_name.clone(),
                    session_id: file_stats.session_id,
                    git_branch: file_stats.git_branch.clone(),
                    first_timestamp: file_stats.first_timestamp,
                    last_timestamp: file_stats.last_timestamp,
                    summary: file_stats.summary.clone(),
                    custom_title: file_stats.custom_title.clone(),
                    ai_title: file_stats.ai_title.clone(),
                    last_user_message: file_stats.last_user_message.clone(),
                    first_user_message: file_stats.first_user_message.clone(),
                    model: file_stats.model,
                    is_subagent: file_stats.is_subagent,
                    daily_stats: daily_stats_cached,
                    hourly_activity: file_stats.hourly_activity,
                    hourly_work_activity: file_stats.hourly_work_activity,
                    weekday_activity: file_stats.weekday_activity,
                    weekday_work_activity: file_stats.weekday_work_activity,
                    tool_error_count: file_stats.tool_error_count,
                    tool_success_count: file_stats.tool_success_count,
                    session_duration_mins: file_stats.session_duration_mins,
                    language_usage: file_stats.language_usage,
                    extension_usage: file_stats.extension_usage,
                };

                Self::apply_productivity_stats(&mut stats, &cached_file_stats);
                cache.insert(file, cached_file_stats);
                cache_stats.parsed_files += 1;
            }
        }

        let _ = cache.save();

        (stats, cache_stats)
    }

    fn merge_cached_stats(
        stats: &mut Stats,
        cached: &CachedFileStats,
        project_name: &Option<String>,
    ) {
        stats.total_entries += cached.entry_count;
        stats.total_tokens.input_tokens += cached.input_tokens;
        stats.total_tokens.output_tokens += cached.output_tokens;
        stats.total_tokens.cache_creation_tokens += cached.cache_creation_tokens;
        stats.total_tokens.cache_read_tokens += cached.cache_read_tokens;
        stats.total_tokens.cache_creation_5m_tokens += cached.cache_creation_5m_tokens;
        stats.total_tokens.cache_creation_1h_tokens += cached.cache_creation_1h_tokens;

        for (tool, count) in &cached.tool_usage {
            *stats.tool_usage.entry(tool.clone()).or_insert(0) += count;
        }

        // Per-session-day adoption counts (skip subagent sessions). Denominator is
        // `total_session_days` (also incremented per-day here).
        if !cached.is_subagent {
            for ds in cached.daily_stats.values() {
                Self::add_session_adoption(stats, ds.tool_usage.keys());
                stats.total_session_days += 1;
            }
        }

        for (model, count) in &cached.model_usage {
            *stats.model_usage.entry(model.clone()).or_insert(0) += count;
        }

        for (model, cached_ts) in &cached.model_tokens {
            let model_stats = stats.model_tokens.entry(model.clone()).or_default();
            model_stats.input_tokens += cached_ts.input_tokens;
            model_stats.output_tokens += cached_ts.output_tokens;
            model_stats.cache_creation_tokens += cached_ts.cache_creation_tokens;
            model_stats.cache_read_tokens += cached_ts.cache_read_tokens;
            model_stats.cache_creation_5m_tokens += cached_ts.cache_creation_5m_tokens;
            model_stats.cache_creation_1h_tokens += cached_ts.cache_creation_1h_tokens;
        }

        let session_tokens = cached.input_tokens
            + cached.output_tokens
            + cached.cache_creation_tokens
            + cached.cache_read_tokens;
        let session_work_tokens = cached.input_tokens + cached.output_tokens;

        // Subagent sessions are excluded so the Projects panel's count /
        // tokens reads as "user-driven activity" — matching the cost number
        // (computed via `user_sessions()`) and the filtered-rebuild path.
        if let Some(name) = project_name
            && !cached.is_subagent
        {
            let project = stats.project_stats.entry(name.clone()).or_default();
            project.sessions += 1;
            project.tokens += session_tokens;
            project.work_tokens += session_work_tokens;
        }

        if let Some(date) = cached.session_date {
            *stats.daily_activity.entry(date).or_insert(0) += session_tokens;
            *stats.daily_work_activity.entry(date).or_insert(0) += session_work_tokens;
        }

        for (hour, tokens) in &cached.hourly_activity {
            *stats.hourly_activity.entry(*hour).or_insert(0) += tokens;
        }

        for (hour, tokens) in &cached.hourly_work_activity {
            *stats.hourly_work_activity.entry(*hour).or_insert(0) += tokens;
        }

        for (weekday, tokens) in &cached.weekday_activity {
            *stats
                .weekday_activity
                .entry(Weekday::try_from(*weekday).unwrap_or(Weekday::Mon))
                .or_insert(0) += tokens;
        }

        for (weekday, tokens) in &cached.weekday_work_activity {
            *stats
                .weekday_work_activity
                .entry(Weekday::try_from(*weekday).unwrap_or(Weekday::Mon))
                .or_insert(0) += tokens;
        }

        stats.tool_error_count += cached.tool_error_count;
        stats.tool_success_count += cached.tool_success_count;

        for (lang, count) in &cached.language_usage {
            *stats.language_usage.entry(lang.clone()).or_insert(0) += count;
        }

        for (ext, count) in &cached.extension_usage {
            *stats.extension_usage.entry(ext.clone()).or_insert(0) += count;
        }

        Self::apply_productivity_stats(stats, cached);
    }

    fn apply_productivity_stats(stats: &mut Stats, cached: &CachedFileStats) {
        if !cached.is_subagent {
            stats.total_sessions_count += 1;
            if cached.summary.is_some() {
                stats.sessions_with_summary += 1;
            }

            if let Some(ref branch) = cached.git_branch {
                let branch_stats = stats.branch_stats.entry(branch.clone()).or_default();
                branch_stats.session_count += 1;
                if let Some(duration) = cached.session_duration_mins {
                    branch_stats.total_duration_mins += duration;
                }
                if let Some(ts) = cached.first_timestamp
                    && (branch_stats.first_seen.is_none() || Some(ts) < branch_stats.first_seen)
                {
                    branch_stats.first_seen = Some(ts);
                }
                if let Some(ts) = cached.last_timestamp
                    && (branch_stats.last_seen.is_none() || Some(ts) > branch_stats.last_seen)
                {
                    branch_stats.last_seen = Some(ts);
                }
            }
        }
    }

    fn aggregate_file_entries(
        stats: &mut Stats,
        entries: &[LogEntry],
        project_name: &Option<String>,
        file: &Path,
        global_seen: &mut std::collections::HashSet<String>,
    ) -> FileStats {
        let mut file_stats = FileStats::default();

        stats.total_entries += entries.len();

        let entries_with_ts: Vec<_> = entries.iter().filter(|e| e.timestamp.is_some()).collect();
        if let Some(first) = entries_with_ts.first() {
            file_stats.first_timestamp = first.timestamp;
            file_stats.session_id = first.session_id.clone();
            // git_branch is mutable mid-session (`git checkout`); use the
            // LATEST value, not the first. Matches `grouping.rs` and the
            // session-representative-value rule.
            file_stats.git_branch = entries_with_ts
                .iter()
                .rev()
                .find_map(|e| e.git_branch.clone());
            let is_subagent_by_entry = first.is_sidechain;
            let is_subagent_by_filename = file
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("agent-"));
            file_stats.is_subagent = is_subagent_by_entry || is_subagent_by_filename;
        }
        if let Some(last) = entries_with_ts.last() {
            file_stats.last_timestamp = last.timestamp;
        }

        file_stats.summary = entries
            .iter()
            .rfind(|e| e.entry_type == EntryType::Summary)
            .and_then(|e| e.summary.clone())
            .or_else(|| {
                entries
                    .iter()
                    .rev()
                    .filter(|e| {
                        e.entry_type == EntryType::User || e.entry_type == EntryType::System
                    })
                    .find_map(|e| {
                        let text = e.message.as_ref()?.content.extract_text();
                        let text = text.trim();
                        if text.is_empty() || text.starts_with('<') {
                            None
                        } else {
                            let truncated: String = text.chars().take(80).collect();
                            Some(truncated)
                        }
                    })
            });

        file_stats.custom_title = entries
            .iter()
            .rfind(|e| e.entry_type == EntryType::CustomTitle)
            .and_then(|e| e.custom_title.clone())
            // Cowork sessions don't emit a `customTitle` JSONL entry; pull the
            // user-curated `title` from the sibling metadata json instead so
            // the cached value matches what the grouper would resolve.
            .or_else(|| crate::infrastructure::resolve_cowork_title(file));

        file_stats.ai_title = entries
            .iter()
            .rfind(|e| e.entry_type == EntryType::AiTitle)
            .and_then(|e| e.ai_title.clone());

        file_stats.last_user_message =
            crate::aggregator::grouping::extract_last_user_message_for_cache(entries);
        file_stats.first_user_message =
            crate::aggregator::grouping::extract_first_user_message_for_cache(entries);

        file_stats.model = super::extract_session_model(entries);

        for entry in entries {
            // Decide once per entry whether its tokens are already credited
            // to another file (global dedup). The two sub-aggregations
            // below (per-day daily_stats / global stats) both consult this
            // single decision — without sharing, one branch could skip
            // while the other adds, producing the divergence between TUI
            // Overview (uses global stats) and CLI --daily (reads
            // daily_stats via the grouper).
            let skip_tokens = if entry.entry_type == EntryType::Assistant
                && entry
                    .message
                    .as_ref()
                    .and_then(|m| m.usage.as_ref())
                    .is_some()
            {
                match crate::parser::JsonlParser::entry_hash(entry) {
                    Some(hash) => {
                        if !global_seen.insert(hash.clone()) {
                            true
                        } else {
                            file_stats.unique_hashes.push(hash);
                            false
                        }
                    }
                    None => false,
                }
            } else {
                false
            };

            if let Some(ts) = entry.timestamp {
                let date = ts.with_timezone(&Local).date_naive();
                let daily = file_stats.daily_stats.entry(date).or_default();

                if daily.first_timestamp.is_none() || Some(ts) < daily.first_timestamp {
                    daily.first_timestamp = Some(ts);
                }
                if daily.last_timestamp.is_none() || Some(ts) > daily.last_timestamp {
                    daily.last_timestamp = Some(ts);
                }

                if entry.entry_type == EntryType::Assistant
                    && let Some(ref message) = entry.message
                    && let Some(ref usage) = message.usage
                {
                    let model_key = message
                        .model
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string());
                    // Filter synthetic / empty-model entries from the
                    // entry stream entirely. `continue` skips Block B
                    // below too, preserving the prior semantics that
                    // these never reach model_usage / tool_usage.
                    if !super::is_real_model(&model_key) {
                        continue;
                    }
                    // Deduped entries: token math skipped here, but
                    // Block B's tool/model_usage still runs.
                    if !skip_tokens {
                        daily.input_tokens += usage.input_tokens;
                        daily.output_tokens += usage.output_tokens;

                        let daily_model = daily.tokens_by_model.entry(model_key).or_default();
                        daily_model.add(usage);

                        let local_ts = ts.with_timezone(&Local);
                        let hour = local_ts.hour() as u8;
                        let weekday = local_ts.weekday().num_days_from_monday() as u8;
                        let tokens = usage.input_tokens
                            + usage.output_tokens
                            + usage.cache_creation_input_tokens
                            + usage.cache_read_input_tokens;
                        let work_tokens = usage.input_tokens + usage.output_tokens;

                        *daily.hourly_activity.entry(hour).or_insert(0) += tokens;
                        *daily.hourly_work_activity.entry(hour).or_insert(0) += work_tokens;
                        *file_stats.hourly_activity.entry(hour).or_insert(0) += tokens;
                        *file_stats.hourly_work_activity.entry(hour).or_insert(0) += work_tokens;
                        *file_stats.weekday_activity.entry(weekday).or_insert(0) += tokens;
                        *file_stats.weekday_work_activity.entry(weekday).or_insert(0) +=
                            work_tokens;
                        *stats.hourly_activity.entry(hour).or_insert(0) += tokens;
                        *stats.hourly_work_activity.entry(hour).or_insert(0) += work_tokens;
                        *stats
                            .weekday_activity
                            .entry(local_ts.weekday())
                            .or_insert(0) += tokens;
                        *stats
                            .weekday_work_activity
                            .entry(local_ts.weekday())
                            .or_insert(0) += work_tokens;
                    }
                }
            }

            if let Some(ref message) = entry.message {
                if entry.entry_type == EntryType::Assistant {
                    if let Some(ref usage) = message.usage {
                        if skip_tokens {
                            // tool_usage / model_usage still need this
                            // entry, but token totals must not double-count.
                            if let Some(ref model) = message.model {
                                *stats.model_usage.entry(model.clone()).or_insert(0) += 1;
                                *file_stats.model_usage.entry(model.clone()).or_insert(0) += 1;
                            }
                            let daily_date = entry
                                .timestamp
                                .map(|ts| ts.with_timezone(&Local).date_naive());
                            Self::count_tool_usage_with_file_stats(
                                stats,
                                &mut file_stats,
                                daily_date,
                                message,
                            );
                            continue;
                        }
                        stats.total_tokens.add(usage);
                        file_stats.input_tokens += usage.input_tokens;
                        file_stats.output_tokens += usage.output_tokens;
                        file_stats.cache_creation_tokens += usage.cache_creation_input_tokens;
                        file_stats.cache_read_tokens += usage.cache_read_input_tokens;
                        if let Some(ref cc) = usage.cache_creation {
                            file_stats.cache_creation_5m_tokens += cc.ephemeral_5m_input_tokens;
                            file_stats.cache_creation_1h_tokens += cc.ephemeral_1h_input_tokens;
                        } else {
                            file_stats.cache_creation_5m_tokens +=
                                usage.cache_creation_input_tokens;
                        }

                        if let Some(ref model) = message.model {
                            let model_stats = stats.model_tokens.entry(model.clone()).or_default();
                            model_stats.add(usage);
                            let file_model_stats =
                                file_stats.model_tokens.entry(model.clone()).or_default();
                            file_model_stats.add(usage);
                        }
                    }

                    if let Some(ref model) = message.model {
                        *stats.model_usage.entry(model.clone()).or_insert(0) += 1;
                        *file_stats.model_usage.entry(model.clone()).or_insert(0) += 1;
                    }
                }

                let daily_date = entry
                    .timestamp
                    .map(|ts| ts.with_timezone(&Local).date_naive());
                Self::count_tool_usage_with_file_stats(stats, &mut file_stats, daily_date, message);
            }
        }

        let session_tokens = file_stats.input_tokens
            + file_stats.output_tokens
            + file_stats.cache_creation_tokens
            + file_stats.cache_read_tokens;
        let session_work_tokens = file_stats.input_tokens + file_stats.output_tokens;

        // See the matching merge_cached_stats block: subagent sessions are
        // excluded so `project_stats` stays consistent with the filtered-
        // rebuild path and with how cost is computed downstream.
        if let Some(name) = project_name
            && !file_stats.is_subagent
        {
            let project = stats.project_stats.entry(name.clone()).or_default();
            project.sessions += 1;
            project.tokens += session_tokens;
            project.work_tokens += session_work_tokens;
        }

        // Per-session-day adoption counts (skip subagent sessions). Denominator is
        // `total_session_days` (also incremented per-day here).
        if !file_stats.is_subagent {
            for ds in file_stats.daily_stats.values() {
                Self::add_session_adoption(stats, ds.tool_usage.keys());
                stats.total_session_days += 1;
            }
        }

        file_stats.session_date = Self::extract_session_date(entries);
        if let Some(date) = file_stats.session_date {
            *stats.daily_activity.entry(date).or_insert(0) += session_tokens;
            *stats.daily_work_activity.entry(date).or_insert(0) += session_work_tokens;
        }

        if let (Some(first), Some(last)) = (file_stats.first_timestamp, file_stats.last_timestamp) {
            let duration = (last - first).num_minutes();
            file_stats.session_duration_mins = Some(duration.max(1));
        }

        file_stats
    }

    fn count_tool_usage_with_file_stats(
        stats: &mut Stats,
        file_stats: &mut FileStats,
        daily_date: Option<NaiveDate>,
        message: &crate::domain::Message,
    ) {
        use crate::domain::{ContentBlock, MessageContent};

        // Slash commands (`/clear`, `/model`, `/<custom>`) appear as XML
        // metadata in user message text — they are NOT tool_use blocks. Scan
        // user messages for `<command-name>/foo</command-name>` and bump the
        // `command:foo` key so commands show up in tool_usage stats.
        if matches!(message.role, crate::domain::Role::User) {
            if let Some(date) = daily_date
                && let Some(d) = file_stats.daily_stats.get_mut(&date)
            {
                d.user_msgs += 1;
            }
            let text = message.content.extract_text();
            for cmd in extract_command_names(&text) {
                let key = format!("command:{cmd}");
                *stats.tool_usage.entry(key.clone()).or_insert(0) += 1;
                *file_stats.tool_usage.entry(key.clone()).or_insert(0) += 1;
                if let Some(date) = daily_date
                    && let Some(d) = file_stats.daily_stats.get_mut(&date)
                {
                    *d.tool_usage.entry(key).or_insert(0) += 1;
                }
            }
        } else if matches!(message.role, crate::domain::Role::Assistant)
            && let Some(date) = daily_date
            && let Some(d) = file_stats.daily_stats.get_mut(&date)
        {
            d.assistant_msgs += 1;
        }

        if let MessageContent::Blocks(ref blocks) = message.content {
            for block in blocks {
                match block {
                    ContentBlock::ToolUse { name, input, .. } => {
                        let key = crate::aggregator::tool_usage_key(name, input);
                        *stats.tool_usage.entry(key.clone()).or_insert(0) += 1;
                        *file_stats.tool_usage.entry(key.clone()).or_insert(0) += 1;

                        let exts = Self::extract_extensions_from_tool_input(name, input);
                        // Per-extension language attribution keeps language_usage in
                        // step with extension_usage when one tool call resolves to
                        // multiple extensions (e.g. a glob like `*.{md,mdx}`).
                        // Special filenames without an extension (Dockerfile,
                        // Makefile, etc.) fall through to the language-only path.
                        let mut langs: Vec<&'static str> = exts
                            .iter()
                            .map(|ext| crate::aggregator::language::for_extension(ext))
                            .collect();
                        if exts.is_empty()
                            && let Some(l) = Self::extract_language_from_tool_input(name, input)
                        {
                            langs.push(l);
                        }
                        for lang in &langs {
                            *stats.language_usage.entry((*lang).to_string()).or_insert(0) += 1;
                            *file_stats
                                .language_usage
                                .entry((*lang).to_string())
                                .or_insert(0) += 1;
                        }
                        for ext in &exts {
                            *stats.extension_usage.entry(ext.clone()).or_insert(0) += 1;
                            *file_stats.extension_usage.entry(ext.clone()).or_insert(0) += 1;
                        }

                        if let Some(date) = daily_date
                            && let Some(d) = file_stats.daily_stats.get_mut(&date)
                        {
                            *d.tool_usage.entry(key).or_insert(0) += 1;
                            for lang in &langs {
                                *d.language_usage.entry((*lang).to_string()).or_insert(0) += 1;
                            }
                            for ext in exts {
                                *d.extension_usage.entry(ext).or_insert(0) += 1;
                            }
                        }
                    }
                    ContentBlock::ToolResult { is_error, .. } => {
                        if *is_error {
                            stats.tool_error_count += 1;
                            file_stats.tool_error_count += 1;
                        } else {
                            stats.tool_success_count += 1;
                            file_stats.tool_success_count += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    pub(crate) fn extract_language_from_tool_input(
        tool_name: &str,
        input: &serde_json::Value,
    ) -> Option<&'static str> {
        match tool_name {
            "Read" | "Edit" | "Write" | "MultiEdit" => {
                let path = input
                    .get("file_path")
                    .or_else(|| input.get("path"))
                    .and_then(|v| v.as_str())?;
                super::language::from_path(path)
            }
            "NotebookEdit" => {
                let path = input.get("notebook_path").and_then(|v| v.as_str())?;
                super::language::from_path(path)
            }
            "Glob" => {
                let pattern = input.get("pattern").and_then(|v| v.as_str())?;
                super::language::from_glob_pattern(pattern)
            }
            "Grep" => {
                let glob = input.get("glob").and_then(|v| v.as_str());
                if let Some(g) = glob {
                    return super::language::from_glob_pattern(g);
                }
                let type_filter = input.get("type").and_then(|v| v.as_str())?;
                super::language::from_type_filter(type_filter)
            }
            _ => None,
        }
    }

    pub(crate) fn extract_extensions_from_tool_input(
        tool_name: &str,
        input: &serde_json::Value,
    ) -> Vec<String> {
        match tool_name {
            "Read" | "Edit" | "Write" | "MultiEdit" => {
                let path = input
                    .get("file_path")
                    .or_else(|| input.get("path"))
                    .and_then(|v| v.as_str());
                path.and_then(super::language::extension_from_path)
                    .into_iter()
                    .collect()
            }
            "NotebookEdit" => {
                let path = input.get("notebook_path").and_then(|v| v.as_str());
                path.and_then(super::language::extension_from_path)
                    .into_iter()
                    .collect()
            }
            "Glob" => {
                let pattern = input.get("pattern").and_then(|v| v.as_str());
                pattern
                    .map(super::language::parse_extensions_from_glob)
                    .unwrap_or_default()
            }
            "Grep" => {
                let glob = input.get("glob").and_then(|v| v.as_str());
                glob.map(super::language::parse_extensions_from_glob)
                    .unwrap_or_default()
            }
            _ => vec![],
        }
    }

    fn extract_project_name_from_entries(entries: &[LogEntry]) -> Option<String> {
        super::extract_project_name(entries)
    }

    fn extract_session_date(entries: &[LogEntry]) -> Option<NaiveDate> {
        entries
            .iter()
            .find_map(|e| e.timestamp)
            .map(|ts| ts.with_timezone(&Local).date_naive())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_stats_work_tokens_excludes_cache() {
        let stats = TokenStats {
            input_tokens: 1000,
            output_tokens: 500,
            cache_creation_tokens: 200,
            cache_read_tokens: 300,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
        };
        assert_eq!(stats.work_tokens(), 1500);
    }

    #[test]
    fn test_token_stats_all_tokens_includes_cache() {
        let stats = TokenStats {
            input_tokens: 1000,
            output_tokens: 500,
            cache_creation_tokens: 200,
            cache_read_tokens: 300,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
        };
        assert_eq!(stats.all_tokens(), 2000);
    }

    #[test]
    fn test_token_stats_all_tokens_vs_work_tokens_difference() {
        let stats = TokenStats {
            input_tokens: 1000,
            output_tokens: 500,
            cache_creation_tokens: 200,
            cache_read_tokens: 300,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
        };
        let cache_tokens = stats.cache_creation_tokens + stats.cache_read_tokens;
        assert_eq!(stats.all_tokens(), stats.work_tokens() + cache_tokens);
    }

    #[test]
    fn test_token_stats_add_from_usage() {
        let mut stats = TokenStats::default();
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 20,
            cache_read_input_tokens: 30,
            service_tier: None,
            cache_creation: None,
        };
        stats.add(&usage);

        assert_eq!(stats.input_tokens, 100);
        assert_eq!(stats.output_tokens, 50);
        assert_eq!(stats.cache_creation_tokens, 20);
        assert_eq!(stats.cache_read_tokens, 30);
        assert_eq!(stats.all_tokens(), 200);
    }

    #[test]
    fn test_token_stats_add_accumulates() {
        let mut stats = TokenStats::default();
        let usage1 = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 20,
            cache_read_input_tokens: 30,
            service_tier: None,
            cache_creation: None,
        };
        let usage2 = Usage {
            input_tokens: 200,
            output_tokens: 100,
            cache_creation_input_tokens: 40,
            cache_read_input_tokens: 60,
            service_tier: None,
            cache_creation: None,
        };
        stats.add(&usage1);
        stats.add(&usage2);

        assert_eq!(stats.all_tokens(), 600);
    }

    #[test]
    fn test_get_language_from_path_extensions() {
        assert_eq!(
            crate::aggregator::language::from_path("/src/main.rs"),
            Some("Rust")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/app.tsx"),
            Some("TypeScript")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/script.py"),
            Some("Python")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/data.json"),
            Some("JSON")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/data.jsonl"),
            Some("JSON")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/style.scss"),
            Some("CSS")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/page.vue"),
            Some("Vue")
        );
    }

    #[test]
    fn test_get_language_from_path_new_extensions() {
        assert_eq!(
            crate::aggregator::language::from_path("/readme.txt"),
            Some("Text")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/data.csv"),
            Some("CSV")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/icon.png"),
            Some("Image")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/logo.svg"),
            Some("Image")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/notebook.ipynb"),
            Some("Jupyter")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/Cargo.lock"),
            Some("Lock")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/app.conf"),
            Some("Other")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/test.snap"),
            Some("Snapshot")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/template.hbs"),
            Some("Handlebars")
        );
    }

    #[test]
    fn test_get_language_from_path_dotfiles() {
        assert_eq!(
            crate::aggregator::language::from_path("/project/.gitignore"),
            Some("Other")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/project/.editorconfig"),
            Some("Other")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/project/.env"),
            Some("Other")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/project/.prettierrc"),
            Some("Other")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/project/.node-version"),
            Some("Other")
        );
    }

    #[test]
    fn test_get_language_from_path_extensionless_files() {
        assert_eq!(
            crate::aggregator::language::from_path("/project/Makefile"),
            Some("Makefile")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/project/Dockerfile"),
            Some("Docker")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/project/Gemfile"),
            Some("Ruby")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/project/Justfile"),
            Some("Makefile")
        );
    }

    #[test]
    fn test_get_language_from_path_unknown_extensionless() {
        assert_eq!(
            crate::aggregator::language::from_path("/project/LICENSE"),
            Some("Other")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/project/CHANGELOG"),
            Some("Other")
        );
    }

    #[test]
    fn test_get_language_from_path_real_world_other() {
        assert_eq!(
            crate::aggregator::language::from_path("/config.kdl"),
            Some("KDL")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/.envrc"),
            Some("Other")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/.env.example"),
            Some("Other")
        );
        assert_eq!(
            crate::aggregator::language::from_path("/project/.claude"),
            Some("Other")
        );
    }

    #[test]
    fn test_extract_language_glob_uses_pattern() {
        let input = serde_json::json!({"pattern": "**/*.rs", "path": "/src"});
        assert_eq!(
            StatsAggregator::extract_language_from_tool_input("Glob", &input),
            Some("Rust")
        );
    }

    #[test]
    fn test_extract_language_glob_brace_pattern() {
        let input = serde_json::json!({"pattern": "**/*.{ts,js}", "path": "/src"});
        assert_eq!(
            StatsAggregator::extract_language_from_tool_input("Glob", &input),
            Some("TypeScript")
        );
        let input2 = serde_json::json!({"pattern": "**/*.{json,yaml}", "path": "/"});
        assert_eq!(
            StatsAggregator::extract_language_from_tool_input("Glob", &input2),
            Some("JSON")
        );
    }

    #[test]
    fn test_extract_language_grep_brace_glob() {
        let input = serde_json::json!({"pattern": "TODO", "glob": "*.{js,json}"});
        assert_eq!(
            StatsAggregator::extract_language_from_tool_input("Grep", &input),
            Some("JavaScript")
        );
    }

    #[test]
    fn test_extract_language_grep_uses_glob_field() {
        let input = serde_json::json!({"pattern": "fn main", "glob": "*.py", "path": "/src"});
        assert_eq!(
            StatsAggregator::extract_language_from_tool_input("Grep", &input),
            Some("Python")
        );
    }

    #[test]
    fn test_extract_language_grep_uses_type_filter() {
        let input = serde_json::json!({"pattern": "fn main", "type": "rust", "path": "/src"});
        assert_eq!(
            StatsAggregator::extract_language_from_tool_input("Grep", &input),
            Some("Rust")
        );
    }

    #[test]
    fn test_extract_language_grep_dir_only_returns_none() {
        let input = serde_json::json!({"pattern": "fn main", "path": "/src"});
        assert_eq!(
            StatsAggregator::extract_language_from_tool_input("Grep", &input),
            None
        );
    }

    #[test]
    fn test_extract_language_read_uses_file_path() {
        let input = serde_json::json!({"file_path": "/src/main.rs"});
        assert_eq!(
            StatsAggregator::extract_language_from_tool_input("Read", &input),
            Some("Rust")
        );
    }

    // ─── Adoption / tool_sessions counters ───────────────────────────────────

    fn keys(slice: &[&str]) -> Vec<String> {
        slice.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn test_add_session_adoption_counts_skill_subagent_mcp_once_per_call() {
        let mut stats = Stats::default();
        let day_keys = keys(&[
            "Bash",
            "skill:s1",
            "skill:s2",
            "agent:t1",
            "mcp__server1__action",
        ]);
        StatsAggregator::add_session_adoption(&mut stats, day_keys.iter());

        // Even though two `skill:*` keys are present, sessions_using_skills increments only once.
        assert_eq!(stats.sessions_using_skills, 1);
        assert_eq!(stats.sessions_using_subagents, 1);
        assert_eq!(stats.sessions_using_mcp, 1);
    }

    #[test]
    fn test_add_session_adoption_per_tool_session_count() {
        let mut stats = Stats::default();
        let day1 = keys(&["Bash", "skill:s1"]);
        let day2 = keys(&["Bash", "skill:s1", "skill:s2"]);
        StatsAggregator::add_session_adoption(&mut stats, day1.iter());
        StatsAggregator::add_session_adoption(&mut stats, day2.iter());

        // tool_sessions counts +1 per call where the key appears.
        assert_eq!(stats.tool_sessions.get("Bash"), Some(&2));
        assert_eq!(stats.tool_sessions.get("skill:s1"), Some(&2));
        assert_eq!(stats.tool_sessions.get("skill:s2"), Some(&1));
        // sessions_using_skills increments on every call that has at least one skill key.
        assert_eq!(stats.sessions_using_skills, 2);
    }

    #[test]
    fn test_add_session_adoption_no_meta_keys_no_increment() {
        let mut stats = Stats::default();
        let day_keys = keys(&["Bash", "Read", "Edit"]);
        StatsAggregator::add_session_adoption(&mut stats, day_keys.iter());

        assert_eq!(stats.sessions_using_skills, 0);
        assert_eq!(stats.sessions_using_subagents, 0);
        assert_eq!(stats.sessions_using_mcp, 0);
        assert_eq!(stats.tool_sessions.len(), 3);
    }

    #[test]
    fn test_add_session_adoption_empty_input() {
        let mut stats = Stats::default();
        let empty: Vec<String> = Vec::new();
        StatsAggregator::add_session_adoption(&mut stats, empty.iter());

        assert_eq!(stats.sessions_using_skills, 0);
        assert_eq!(stats.tool_sessions.len(), 0);
    }

    #[test]
    fn test_add_session_adoption_agent_and_task_both_increment_subagents() {
        // Both `agent:*` (rewritten from Agent and Task tools) feed subagents counter.
        let mut stats = Stats::default();
        let day1 = keys(&["agent:type-a"]);
        let day2 = keys(&["agent:type-b"]);
        StatsAggregator::add_session_adoption(&mut stats, day1.iter());
        StatsAggregator::add_session_adoption(&mut stats, day2.iter());

        assert_eq!(stats.sessions_using_subagents, 2);
        assert_eq!(stats.tool_sessions.get("agent:type-a"), Some(&1));
        assert_eq!(stats.tool_sessions.get("agent:type-b"), Some(&1));
    }

    #[test]
    fn test_add_session_adoption_mcp_server_sessions_deduped_per_session() {
        // Regression: a session that hits two tools from the SAME MCP server must
        // increment `mcp_server_sessions[<server>]` only once. Previously the only
        // per-server metric was summed from per-tool `tool_sessions`, which double-
        // counted sessions that used multiple tools from the same server.
        let mut stats = Stats::default();
        let day1 = keys(&[
            "mcp__server1__action1",
            "mcp__server1__action2",
            "mcp__server2__other",
        ]);
        StatsAggregator::add_session_adoption(&mut stats, day1.iter());
        let day2 = keys(&["mcp__server1__action1"]);
        StatsAggregator::add_session_adoption(&mut stats, day2.iter());

        // server1: used in day1 and day2 → 2 sessions (despite 3 tool invocations total)
        assert_eq!(stats.mcp_server_sessions.get("server1"), Some(&2));
        // server2: used only in day1 → 1 session
        assert_eq!(stats.mcp_server_sessions.get("server2"), Some(&1));
        // tool_sessions still records per-key session-day counts.
        assert_eq!(stats.tool_sessions.get("mcp__server1__action1"), Some(&2));
        assert_eq!(stats.tool_sessions.get("mcp__server1__action2"), Some(&1));
    }

    #[test]
    fn test_add_session_adoption_mcp_prefix_match() {
        // Both standard `mcp__server__tool` and plugin `mcp__plugin_*` are detected.
        let mut stats = Stats::default();
        let day_keys = keys(&["mcp__server1__action", "mcp__plugin_orgA_serverB__action"]);
        StatsAggregator::add_session_adoption(&mut stats, day_keys.iter());

        assert_eq!(stats.sessions_using_mcp, 1);
        assert_eq!(stats.tool_sessions.len(), 2);
    }

    #[test]
    fn test_add_session_adoption_plugin_only_session_counts_as_mcp() {
        // Regression: a session that uses ONLY plugin-form MCP tools (no standard form)
        // must still increment sessions_using_mcp. Guards against a future refactor that
        // narrows the prefix check to e.g. `starts_with("mcp__server")`.
        let mut stats = Stats::default();
        let day_keys = keys(&["mcp__plugin_orgA_serverB__action"]);
        StatsAggregator::add_session_adoption(&mut stats, day_keys.iter());

        assert_eq!(stats.sessions_using_mcp, 1);
        assert_eq!(
            stats.tool_sessions.get("mcp__plugin_orgA_serverB__action"),
            Some(&1)
        );
    }
}
