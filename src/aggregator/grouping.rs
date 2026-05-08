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
    pub day_tokens_by_model: HashMap<String, ModelTokens>,
    pub day_hourly_activity: HashMap<u8, u64>,
    pub day_hourly_work_tokens: HashMap<u8, u64>,
    pub day_tool_usage: HashMap<String, usize>,
    pub day_language_usage: HashMap<String, usize>,
    pub day_extension_usage: HashMap<String, usize>,
    pub summary: Option<String>,
    pub custom_title: Option<String>,
    pub ai_title: Option<String>,
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

        for file in files {
            let sessions_by_date = Self::get_sessions_by_date(file, cache);
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

    fn get_sessions_by_date(file: &Path, cache: &Option<Cache>) -> Vec<(NaiveDate, SessionInfo)> {
        if let Some(c) = cache
            && c.is_valid(file)
                && let Some(cached) = c.get(file)
                    && !cached.daily_stats.is_empty() {
                        return Self::build_sessions_from_cache(file, cached);
                    }

        Self::parse_and_build_sessions(file, cache)
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
                        day_tokens_by_model: tokens_by_model,
                        day_hourly_activity: ds.hourly_activity.clone(),
                        day_hourly_work_tokens: ds.hourly_work_activity.clone(),
                        day_tool_usage: ds.tool_usage.clone(),
                        day_language_usage: ds.language_usage.clone(),
                        day_extension_usage: ds.extension_usage.clone(),
                        summary: cached.summary.clone(),
                        custom_title: cached.custom_title.clone(),
                        ai_title: cached.ai_title.clone(),
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

        let summary = if let Some(c) = cache {
            if c.is_valid(file) {
                c.get(file).and_then(|cached| cached.summary.clone())
            } else {
                entries
                    .iter()
                    .rfind(|e| e.entry_type == EntryType::Summary)
                    .and_then(|e| e.summary.clone())
            }
        } else {
            entries
                .iter()
                .rfind(|e| e.entry_type == EntryType::Summary)
                .and_then(|e| e.summary.clone())
        };

        let custom_title = if let Some(c) = cache {
            if c.is_valid(file) {
                c.get(file).and_then(|cached| cached.custom_title.clone())
            } else {
                entries
                    .iter()
                    .rfind(|e| e.entry_type == EntryType::CustomTitle)
                    .and_then(|e| e.custom_title.clone())
            }
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
        let ai_title = if let Some(c) = cache {
            if c.is_valid(file) {
                c.get(file).and_then(|cached| cached.ai_title.clone())
            } else {
                entries
                    .iter()
                    .rfind(|e| e.entry_type == EntryType::AiTitle)
                    .and_then(|e| e.ai_title.clone())
            }
        } else {
            entries
                .iter()
                .rfind(|e| e.entry_type == EntryType::AiTitle)
                .and_then(|e| e.ai_title.clone())
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
                        let text = message.content.extract_text();
                        for cmd in crate::aggregator::stats::extract_command_names(&text) {
                            *stats.tool_usage.entry(format!("command:{cmd}")).or_insert(0) += 1;
                        }
                    }
                    if let MessageContent::Blocks(ref blocks) = message.content {
                        for block in blocks {
                            if let crate::domain::ContentBlock::ToolUse { name, input, .. } = block
                            {
                                let key = crate::aggregator::tool_usage_key(name, input);
                                *stats.tool_usage.entry(key).or_insert(0) += 1;
                                let extensions =
                                    StatsAggregator::extract_extensions_from_tool_input(name, input);
                                if extensions.is_empty() {
                                    // Special filenames (Dockerfile, Makefile, etc.) carry a
                                    // language but no extension — count them under language only.
                                    if let Some(lang) =
                                        StatsAggregator::extract_language_from_tool_input(name, input)
                                    {
                                        *stats.language_usage.entry(lang.to_string()).or_insert(0) += 1;
                                    }
                                } else {
                                    for ext in extensions {
                                        let lang = crate::aggregator::language::for_extension(&ext);
                                        *stats.extension_usage.entry(ext).or_insert(0) += 1;
                                        *stats.language_usage.entry(lang.to_string()).or_insert(0) += 1;
                                    }
                                }
                            }
                        }
                    }

                    if let Some(ref usage) = message.usage {
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
                        model_tokens.input_tokens += usage.input_tokens;
                        model_tokens.output_tokens += usage.output_tokens;
                        model_tokens.cache_creation_tokens += usage.cache_creation_input_tokens;
                        model_tokens.cache_read_tokens += usage.cache_read_input_tokens;

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
                        day_tokens_by_model: stats.tokens_by_model,
                        day_hourly_activity: stats.hourly_activity,
                        day_hourly_work_tokens: stats.hourly_work_activity,
                        day_tool_usage: stats.tool_usage,
                        day_language_usage: stats.language_usage,
                        day_extension_usage: stats.extension_usage,
                        summary: summary.clone(),
                        custom_title: custom_title.clone(),
                        ai_title: ai_title.clone(),
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

struct DailyStats {
    first_timestamp: DateTime<Utc>,
    last_timestamp: DateTime<Utc>,
    input_tokens: u64,
    output_tokens: u64,
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

    #[test]
    fn test_model_tokens_work_tokens() {
        let tokens = ModelTokens {
            input_tokens: 1000,
            output_tokens: 500,
            cache_creation_tokens: 200,
            cache_read_tokens: 300,
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
            },
        );

        let mut ds = make_cached_daily(1000, 500);
        ds.tokens_by_model = tokens_by_model;
        let mut daily_stats = HashMap::new();
        daily_stats.insert("2026-02-25".to_string(), ds);

        let mut cached = make_cached_file_stats(daily_stats);
        cached.entry_count = 10;
        cached.input_tokens = 1000;
        cached.output_tokens = 500;
        cached.session_date = Some(NaiveDate::from_ymd_opt(2026, 2, 25).unwrap());
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
        assert_eq!(*date, NaiveDate::from_ymd_opt(2026, 2, 25).unwrap());
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
        daily_stats.insert("2026-02-24".to_string(), make_cached_daily(500, 200));
        daily_stats.insert("2026-02-25".to_string(), make_cached_daily(800, 300));

        let mut cached = make_cached_file_stats(daily_stats);
        cached.session_date = Some(NaiveDate::from_ymd_opt(2026, 2, 24).unwrap());
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
        daily_stats.insert("2026-02-25".to_string(), ds);

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
        daily_stats.insert("2026-02-25".to_string(), ds);

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

        let sessions = DailyGrouper::parse_and_build_sessions(&path, &None);
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
        let sessions = DailyGrouper::parse_and_build_sessions(&path, &None);
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
        let sessions = DailyGrouper::parse_and_build_sessions(&path, &None);
        std::fs::remove_file(&path).ok();

        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].1.is_subagent);
    }
}
