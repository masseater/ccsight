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

    /// Per-day cost summed over `day_tokens_by_model`. Single source so
    /// every cross-surface view (Overview / Costs / Daily / --daily /
    /// MCP / summaries) agrees. Param-injected calculator keeps it pure.
    pub fn cost(&self, calculator: &crate::aggregator::CostCalculator) -> f64 {
        self.day_tokens_by_model
            .iter()
            .map(|(model, tokens)| {
                calculator
                    .calculate_cost(tokens, Some(model))
                    .unwrap_or(0.0)
            })
            .sum()
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
        cache: &mut Option<Cache>,
    ) -> Vec<DailyGroup> {
        let mut date_map: HashMap<NaiveDate, Vec<SessionInfo>> = HashMap::new();

        // Cross-file dedup: resume/branch rewrites duplicate (msg_id,
        // requestId) across sessions but Anthropic bills once. Pre-load
        // with hashes from VALID-cache files only — stale entries would
        // block fresh-parse files from re-crediting their own tokens.
        let mut global_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(c) = cache.as_ref() {
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

        let mut parsed_any = false;
        for file in &sorted_files {
            let (sessions_by_date, parsed) =
                Self::get_sessions_by_date(file, cache, &mut global_seen);
            if parsed {
                parsed_any = true;
            }
            for (date, session) in sessions_by_date {
                date_map.entry(date).or_default().push(session);
            }
        }

        // Save only when we actually parsed something — pure cache-hit runs
        // skip the I/O so warm `--daily` stays fast.
        if parsed_any && let Some(c) = cache.as_ref() {
            let _ = c.save();
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
        cache: &mut Option<Cache>,
        global_seen: &mut std::collections::HashSet<String>,
    ) -> (Vec<(NaiveDate, SessionInfo)>, bool) {
        if let Some(c) = cache.as_ref()
            && c.is_valid(file)
            && let Some(cached) = c.get(file)
            && !cached.daily_stats.is_empty()
        {
            // Cache hit: hashes are already in `global_seen`
            // from the pre-load pass at the top of
            // `group_by_date_with_shared_cache`.
            return (Self::build_sessions_from_cache(file, cached), false);
        }

        let sessions = Self::parse_and_build_sessions(file, cache, global_seen);
        (sessions, true)
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
        cache: &mut Option<Cache>,
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
        // Hashes credited to THIS file (passed the cross-file dedup check
        // below). Stored in the cache so the next run's pre-load pass
        // restores them to global_seen without re-parsing every file.
        let mut file_unique_hashes: Vec<String> = Vec::new();
        // Per-file rollups needed by Stats (TUI). Computed during the same
        // entry walk so the cache entry is complete on first write — no
        // partial / fix-up pass needed.
        let mut file_tool_error_count: usize = 0;
        let mut file_tool_success_count: usize = 0;
        let mut file_model_usage: HashMap<String, usize> = HashMap::new();
        let session_id_from_entries = entries.first().and_then(|e| e.session_id.clone());

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
                        if let Some(ref m) = message.model {
                            *file_model_usage.entry(m.clone()).or_insert(0) += 1;
                        }
                    }
                    if let MessageContent::Blocks(ref blocks) = message.content {
                        for block in blocks {
                            match block {
                                crate::domain::ContentBlock::ToolUse { name, input, .. } => {
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
                                            let lang =
                                                crate::aggregator::language::for_extension(&ext);
                                            *stats.extension_usage.entry(ext).or_insert(0) += 1;
                                            *stats
                                                .language_usage
                                                .entry(lang.to_string())
                                                .or_insert(0) += 1;
                                        }
                                    }
                                }
                                crate::domain::ContentBlock::ToolResult { is_error, .. } => {
                                    // Tool result counts are per-file, not
                                    // hash-deduped — matches stats.rs so a
                                    // cache entry written here produces the
                                    // same numbers as a fresh Stats parse.
                                    if *is_error {
                                        file_tool_error_count += 1;
                                    } else {
                                        file_tool_success_count += 1;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }

                    if let Some(ref usage) = message.usage {
                        // Global dedup: same (msg_id, requestId) across
                        // multiple JSONLs (resume / branch) should bill
                        // once. Skip if another file already credited it.
                        if let Some(hash) = crate::parser::JsonlParser::entry_hash(entry) {
                            if !global_seen.insert(hash.clone()) {
                                continue;
                            }
                            file_unique_hashes.push(hash);
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

        // Write a full cache entry. Per-FILE rollups (cache_creation_tokens
        // totals, weekday activity, tool error/success, ...) are derived
        // from daily_stats + per-entry counters so both Stats and
        // DailyGrouper hit the same cache without re-parsing.
        if let Some(c) = cache.as_mut() {
            use chrono::Datelike;
            let mut cached_daily: HashMap<String, crate::infrastructure::CachedDailyStats> =
                HashMap::new();
            // Per-FILE accumulators, summed from per-day stats.
            let mut file_input_tokens: u64 = 0;
            let mut file_output_tokens: u64 = 0;
            let mut file_cache_creation_tokens: u64 = 0;
            let mut file_cache_creation_5m_tokens: u64 = 0;
            let mut file_cache_creation_1h_tokens: u64 = 0;
            let mut file_cache_read_tokens: u64 = 0;
            let mut file_tool_usage: HashMap<String, usize> = HashMap::new();
            let mut file_model_tokens: HashMap<String, TokenStats> = HashMap::new();
            let mut file_hourly_activity: HashMap<u8, u64> = HashMap::new();
            let mut file_hourly_work_activity: HashMap<u8, u64> = HashMap::new();
            let mut file_weekday_activity: HashMap<u8, u64> = HashMap::new();
            let mut file_weekday_work_activity: HashMap<u8, u64> = HashMap::new();
            let mut file_language_usage: HashMap<String, usize> = HashMap::new();
            let mut file_extension_usage: HashMap<String, usize> = HashMap::new();
            for (date, ds) in &daily_stats {
                file_input_tokens += ds.input_tokens;
                file_output_tokens += ds.output_tokens;
                // Sum cache breakdowns from per-day tokens_by_model.
                let mut day_tokens_total = ds.input_tokens + ds.output_tokens;
                for (model, ts) in &ds.tokens_by_model {
                    file_cache_creation_tokens += ts.cache_creation_tokens;
                    file_cache_creation_5m_tokens += ts.cache_creation_5m_tokens;
                    file_cache_creation_1h_tokens += ts.cache_creation_1h_tokens;
                    file_cache_read_tokens += ts.cache_read_tokens;
                    day_tokens_total += ts.cache_creation_tokens + ts.cache_read_tokens;
                    let agg = file_model_tokens.entry(model.clone()).or_default();
                    agg.input_tokens += ts.input_tokens;
                    agg.output_tokens += ts.output_tokens;
                    agg.cache_creation_tokens += ts.cache_creation_tokens;
                    agg.cache_creation_5m_tokens += ts.cache_creation_5m_tokens;
                    agg.cache_creation_1h_tokens += ts.cache_creation_1h_tokens;
                    agg.cache_read_tokens += ts.cache_read_tokens;
                }
                for (k, v) in &ds.tool_usage {
                    *file_tool_usage.entry(k.clone()).or_insert(0) += v;
                }
                for (k, v) in &ds.language_usage {
                    *file_language_usage.entry(k.clone()).or_insert(0) += v;
                }
                for (k, v) in &ds.extension_usage {
                    *file_extension_usage.entry(k.clone()).or_insert(0) += v;
                }
                for (h, v) in &ds.hourly_activity {
                    *file_hourly_activity.entry(*h).or_insert(0) += v;
                }
                for (h, v) in &ds.hourly_work_activity {
                    *file_hourly_work_activity.entry(*h).or_insert(0) += v;
                }
                // Weekday rollup: attribute each day's totals to its weekday.
                let weekday = date.weekday().num_days_from_monday() as u8;
                *file_weekday_activity.entry(weekday).or_insert(0) += day_tokens_total;
                *file_weekday_work_activity.entry(weekday).or_insert(0) +=
                    ds.input_tokens + ds.output_tokens;

                cached_daily.insert(
                    date.to_string(),
                    crate::infrastructure::CachedDailyStats {
                        first_timestamp: Some(ds.first_timestamp),
                        last_timestamp: Some(ds.last_timestamp),
                        input_tokens: ds.input_tokens,
                        output_tokens: ds.output_tokens,
                        tokens_by_model: ds.tokens_by_model.clone(),
                        hourly_activity: ds.hourly_activity.clone(),
                        hourly_work_activity: ds.hourly_work_activity.clone(),
                        tool_usage: ds.tool_usage.clone(),
                        language_usage: ds.language_usage.clone(),
                        extension_usage: ds.extension_usage.clone(),
                        user_msgs: ds.user_msgs,
                        assistant_msgs: ds.assistant_msgs,
                    },
                );
            }
            let last_ts = daily_stats.values().map(|d| d.last_timestamp).max();
            let session_duration_mins = last_ts.map(|l| (l - session_first).num_minutes());
            let full_entry = crate::infrastructure::CachedFileStats {
                modified_secs: std::fs::metadata(file)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| {
                        t.duration_since(std::time::UNIX_EPOCH)
                            .ok()
                            .map(|d| d.as_secs())
                    })
                    .unwrap_or(0),
                file_size: std::fs::metadata(file).map_or(0, |m| m.len()),
                entry_count: entries.len(),
                input_tokens: file_input_tokens,
                output_tokens: file_output_tokens,
                cache_creation_tokens: file_cache_creation_tokens,
                cache_read_tokens: file_cache_read_tokens,
                cache_creation_5m_tokens: file_cache_creation_5m_tokens,
                cache_creation_1h_tokens: file_cache_creation_1h_tokens,
                unique_hashes: file_unique_hashes,
                tool_usage: file_tool_usage,
                model_usage: file_model_usage,
                model_tokens: file_model_tokens,
                session_date: Some(session_start_date),
                project_name: Some(project_name.clone()),
                session_id: session_id_from_entries,
                git_branch: git_branch.clone(),
                first_timestamp: Some(session_first),
                last_timestamp: last_ts,
                summary: summary.clone(),
                custom_title: custom_title.clone(),
                ai_title: ai_title.clone(),
                last_user_message: last_user_message.clone(),
                first_user_message: first_user_message.clone(),
                model: model.clone(),
                is_subagent,
                daily_stats: cached_daily,
                hourly_activity: file_hourly_activity,
                hourly_work_activity: file_hourly_work_activity,
                weekday_activity: file_weekday_activity,
                weekday_work_activity: file_weekday_work_activity,
                tool_error_count: file_tool_error_count,
                tool_success_count: file_tool_success_count,
                session_duration_mins,
                language_usage: file_language_usage,
                extension_usage: file_extension_usage,
            };
            c.insert(file, full_entry);
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

/// Most-recent meaningful user-role text. Strips known system wrappers;
/// any remaining unknown `<kebab-tag>` causes fall-through to the prior
/// user entry (same approach as `extract_first_user_message`).
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
            &mut None,
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
    fn parse_and_build_sessions_writes_full_cache_entry() {
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("ccsight_partial_cache_test_{id}.jsonl"));

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

        let mut cache = Some(crate::infrastructure::Cache::new_empty());
        let _ = DailyGrouper::parse_and_build_sessions(
            &path,
            &mut cache,
            &mut std::collections::HashSet::new(),
        );
        std::fs::remove_file(&path).ok();

        let entry = cache.as_ref().unwrap().get(&path).expect("cache entry");
        assert!(
            !entry.daily_stats.is_empty(),
            "per-day stats must be populated"
        );
        // Per-FILE rollups are derived from daily_stats so Stats and
        // DailyGrouper share the same cache.
        assert_eq!(entry.input_tokens, 100);
        assert_eq!(entry.output_tokens, 50);
        assert_eq!(entry.entry_count, 2);
    }

    #[test]
    fn cache_round_trip_dailygrouper_to_stats_matches_fresh_parse() {
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("ccsight_roundtrip_test_{id}.jsonl"));

        let entries = vec![
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-02-25T10:00:00Z",
                "sessionId": "rt-session-1",
                "cwd": "/Users/test/projects/myproject",
                "message": {"role": "user", "content": "hello"}
            }),
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-02-25T10:01:00Z",
                "message": {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "tu_1",
                        "name": "Bash",
                        "input": {"command": "ls"}
                    }],
                    "model": "claude-sonnet-4-20250514",
                    "usage": {
                        "input_tokens": 100,
                        "output_tokens": 50,
                        "cache_creation_input_tokens": 200,
                        "cache_read_input_tokens": 300
                    }
                }
            }),
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-02-25T10:02:00Z",
                "message": {"role": "user", "content": [{
                    "type": "tool_result",
                    "tool_use_id": "tu_1",
                    "content": "ok",
                    "is_error": false
                }]}
            }),
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-02-25T10:03:00Z",
                "message": {"role": "user", "content": [{
                    "type": "tool_result",
                    "tool_use_id": "tu_2",
                    "content": "fail",
                    "is_error": true
                }]}
            }),
        ];
        {
            let mut f = std::fs::File::create(&path).unwrap();
            for e in &entries {
                writeln!(f, "{}", serde_json::to_string(e).unwrap()).unwrap();
            }
        }

        // Path 1: DailyGrouper parses, writes cache entry.
        let mut cache_a = Some(crate::infrastructure::Cache::new_empty());
        let _ = DailyGrouper::parse_and_build_sessions(
            &path,
            &mut cache_a,
            &mut std::collections::HashSet::new(),
        );
        let entry_from_grouper = cache_a
            .as_ref()
            .unwrap()
            .get(&path)
            .expect("grouper entry")
            .clone();

        // Path 2: Stats parses the same file from scratch.
        let cache_b = crate::infrastructure::Cache::new_empty();
        let (_stats, _) = StatsAggregator::aggregate_with_shared_cache(
            std::slice::from_ref(&path),
            cache_b.clone(),
        );
        let _ = cache_b;
        let cache_b_loaded = crate::infrastructure::Cache::load()
            .unwrap_or_else(|_| crate::infrastructure::Cache::new_empty());
        // Stats's aggregate writes to ~/.ccsight; load it back. If it's not
        // there (test environment), skip the cross-check — the per-field
        // assertions below still validate the grouper-written entry.
        let entry_from_stats = cache_b_loaded.get(&path).cloned();

        std::fs::remove_file(&path).ok();

        // Per-FILE rollups must reflect tool counts even when only the
        // DailyGrouper wrote the entry — Stats's reader path depends on
        // these fields.
        assert_eq!(entry_from_grouper.tool_success_count, 1);
        assert_eq!(entry_from_grouper.tool_error_count, 1);
        assert_eq!(entry_from_grouper.input_tokens, 100);
        assert_eq!(entry_from_grouper.output_tokens, 50);
        assert_eq!(entry_from_grouper.cache_creation_tokens, 200);
        assert_eq!(entry_from_grouper.cache_read_tokens, 300);
        assert_eq!(entry_from_grouper.weekday_activity.len(), 1);
        assert!(
            entry_from_grouper
                .model_usage
                .contains_key("claude-sonnet-4-20250514")
        );

        // Cross-path comparison when Stats's cache landed on disk.
        if let Some(stats_entry) = entry_from_stats {
            assert_eq!(
                stats_entry.tool_success_count,
                entry_from_grouper.tool_success_count
            );
            assert_eq!(
                stats_entry.tool_error_count,
                entry_from_grouper.tool_error_count
            );
            assert_eq!(stats_entry.input_tokens, entry_from_grouper.input_tokens);
            assert_eq!(stats_entry.output_tokens, entry_from_grouper.output_tokens);
            assert_eq!(
                stats_entry.cache_creation_tokens,
                entry_from_grouper.cache_creation_tokens
            );
            assert_eq!(
                stats_entry.cache_read_tokens,
                entry_from_grouper.cache_read_tokens
            );
        }
    }

    #[test]
    fn test_parse_and_build_sessions_empty_file() {
        let path = std::env::temp_dir().join("ccsight_grouping_empty.jsonl");
        {
            std::fs::File::create(&path).unwrap();
        }
        let sessions = DailyGrouper::parse_and_build_sessions(
            &path,
            &mut None,
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
            &mut None,
            &mut std::collections::HashSet::new(),
        );
        std::fs::remove_file(&path).ok();

        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].1.is_subagent);
    }
}
