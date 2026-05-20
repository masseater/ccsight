use std::collections::HashMap;

use chrono::{Local, NaiveDate};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};

use crate::PeriodFilter;
use crate::aggregator::{CostCalculator, DailyGroup, DailyGrouper};
use crate::conversation::{ConversationBlock, ConversationMessage, load_conversation};
use crate::infrastructure::FileDiscovery;

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct CcsightServer {
    limit: usize,
    fixed_groups: Option<Vec<DailyGroup>>,
}

struct LoadedData {
    daily_groups: Vec<DailyGroup>,
    summary_cache: HashMap<PathBuf, Option<String>>,
}

fn load_fresh_data(limit: usize) -> LoadedData {
    let files = FileDiscovery::find_jsonl_files_with_limit(limit).unwrap_or_default();
    let cache = crate::infrastructure::Cache::load().ok();
    let daily_groups = DailyGrouper::group_by_date_with_shared_cache(&files, &cache);
    build_loaded_data(daily_groups)
}

fn build_loaded_data(daily_groups: Vec<DailyGroup>) -> LoadedData {
    let mut summary_cache = HashMap::new();

    for group in &daily_groups {
        for session in &group.sessions {
            let path = &session.file_path;
            if session.summary.is_none() && !summary_cache.contains_key(path) {
                summary_cache.insert(path.clone(), derive_summary(path));
            }
        }
    }

    LoadedData {
        daily_groups,
        summary_cache,
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct StatsParams {
    #[schemars(
        description = "Period filter: \"today\", \"week\", \"month\", \"all\" (default). Ignored when date_from is set."
    )]
    period: Option<String>,
    #[schemars(
        description = "Start date for range filter (YYYY-MM-DD). Takes priority over period."
    )]
    date_from: Option<String>,
    #[schemars(
        description = "End date for range filter (YYYY-MM-DD). Defaults to today if omitted."
    )]
    date_to: Option<String>,
    #[schemars(
        description = "Group results by: \"day\" (returns daily breakdown). Omit for totals only. Projects are always included."
    )]
    group_by: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SearchParams {
    #[schemars(
        description = "Search query. Searches across all session conversations (full-text) and metadata (project name, summary, branch, date)."
    )]
    query: String,
    #[schemars(description = "Filter by project name (partial match)")]
    project: Option<String>,
    #[schemars(description = "Start date for range filter (YYYY-MM-DD)")]
    date_from: Option<String>,
    #[schemars(description = "End date for range filter (YYYY-MM-DD). Omit for no upper limit.")]
    date_to: Option<String>,
    #[schemars(description = "Maximum number of results (default: 20)")]
    limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct LiveSessionsParams {
    #[schemars(
        description = "Filter by project name / cwd substring (case-sensitive). Omit for all projects."
    )]
    project: Option<String>,
    #[schemars(
        description = "Filter by status tier: \"busy\" (process actively responding), \"warm\" (idle, last touched within 30 min), \"idle\" (alive but quiet), \"paused\" (process gone, JSONL within 24h or in snapshot). Omit for all."
    )]
    status: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SessionsParams {
    #[schemars(
        description = "Session ID (first 8 chars) to get detail. When set, returns conversation, tool usage, and files touched."
    )]
    session_id: Option<String>,
    #[schemars(
        description = "Filter by keyword (matches project, summary, branch, date metadata only). For full-text content search, use 'search' tool."
    )]
    query: Option<String>,
    #[schemars(description = "Filter by project name (partial match)")]
    project: Option<String>,
    #[schemars(
        description = "Filter by exact date (YYYY-MM-DD). For ranges, use date_from/date_to instead."
    )]
    date: Option<String>,
    #[schemars(description = "Start date for range filter (YYYY-MM-DD)")]
    date_from: Option<String>,
    #[schemars(description = "End date for range filter (YYYY-MM-DD). Defaults to today.")]
    date_to: Option<String>,
    #[schemars(description = "Sort order: \"date\" (default), \"cost\", \"tokens\"")]
    sort: Option<String>,
    #[schemars(description = "Maximum number of results (default: 20)")]
    limit: Option<usize>,
    #[schemars(
        description = "Max conversation entries in session_detail. Default: all. Set 0 to omit conversation entirely."
    )]
    conversation_limit: Option<usize>,
    #[schemars(
        description = "Skip first N conversation entries (for pagination). Use with conversation_limit."
    )]
    conversation_offset: Option<usize>,
    #[schemars(
        description = "Search within conversation. Returns only matching messages with 1 message of context before/after. Each message includes its offset. More useful than conversation_offset for finding specific content."
    )]
    conversation_query: Option<String>,
    #[schemars(description = "Filter to pinned sessions only")]
    pinned: Option<bool>,
}

fn period_to_filter(period: Option<&str>) -> PeriodFilter {
    match period {
        Some("today") => PeriodFilter::Today,
        Some("week") => PeriodFilter::Last7d,
        Some("month") => PeriodFilter::Last30d,
        _ => PeriodFilter::All,
    }
}

fn filter_groups_by_date_range(
    groups: &[DailyGroup],
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
) -> Vec<&DailyGroup> {
    groups
        .iter()
        .filter(|g| {
            if let Some(s) = start
                && g.date < s
            {
                return false;
            }
            if let Some(e) = end
                && g.date > e
            {
                return false;
            }
            true
        })
        .collect()
}

#[tool_router]
impl CcsightServer {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            fixed_groups: None,
        }
    }

    #[cfg(test)]
    fn from_groups(daily_groups: Vec<DailyGroup>) -> Self {
        Self {
            limit: 0,
            fixed_groups: Some(daily_groups),
        }
    }

    fn load(&self) -> LoadedData {
        if let Some(ref groups) = self.fixed_groups {
            build_loaded_data(groups.clone())
        } else {
            load_fresh_data(self.limit)
        }
    }

    #[tool(
        description = "Get aggregated usage statistics: cost, tokens (input/output/cache), model breakdown, projects, hourly patterns, tool usage, languages. Use group_by=day for daily breakdown. All dates/times are in local timezone."
    )]
    fn stats(
        &self,
        Parameters(params): Parameters<StatsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let (start, end) = if let Some(ref sd) = params.date_from {
            let s = sd.parse::<NaiveDate>().ok();
            let e = params
                .date_to
                .as_deref()
                .and_then(|d| d.parse::<NaiveDate>().ok());
            (s, e)
        } else {
            let filter = period_to_filter(params.period.as_deref());
            filter.date_range()
        };
        let data = self.load();
        let filtered = filter_groups_by_date_range(&data.daily_groups, start, end);

        let calculator = CostCalculator::global();
        let mut total_cost = 0.0;
        let mut total_input = 0u64;
        let mut total_output = 0u64;
        let mut session_count = 0usize;
        let mut model_agg: HashMap<String, (f64, u64)> = HashMap::new();
        let mut project_agg: HashMap<String, (usize, u64, f64)> = HashMap::new();
        let mut hourly_agg: HashMap<u8, u64> = HashMap::new();
        let group_by = params.group_by.as_deref();
        let mut daily_agg: HashMap<NaiveDate, (f64, usize, u64)> = HashMap::new();
        let mut total_cache_creation = 0u64;
        let mut total_cache_read = 0u64;
        let mut tool_counts: HashMap<String, usize> = HashMap::new();
        let mut lang_counts: HashMap<String, usize> = HashMap::new();

        // Track models encountered in the filtered window that lack a pricing entry,
        // so the stats response can surface the silent-$0 risk to the caller.
        let mut models_without_pricing: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for group in &filtered {
            for session in &group.sessions {
                let mut session_cost = 0.0;
                for (model, tokens) in &session.day_tokens_by_model {
                    let cost = calculator
                        .calculate_cost(tokens, Some(model))
                        .unwrap_or(0.0);
                    if !calculator.has_pricing(Some(model)) {
                        models_without_pricing
                            .insert(crate::aggregator::normalize_model_name(model));
                    }
                    session_cost += cost;
                    total_cost += cost;
                    total_cache_creation += tokens.cache_creation_tokens;
                    total_cache_read += tokens.cache_read_tokens;
                    let entry = model_agg
                        .entry(crate::aggregator::normalize_model_name(model))
                        .or_default();
                    entry.0 += cost;
                    entry.1 += tokens.work_tokens();
                }

                total_input += session.day_input_tokens;
                total_output += session.day_output_tokens;

                if session.is_subagent {
                    continue;
                }

                session_count += 1;

                if group_by == Some("day") {
                    let day = daily_agg.entry(group.date).or_default();
                    day.0 += session_cost;
                    day.1 += 1;
                    day.2 += session.work_tokens();
                }

                let proj = project_agg.entry(session.project_name.clone()).or_default();
                proj.0 += 1;
                proj.1 += session.work_tokens();
                proj.2 += session_cost;

                for (hour, tokens) in &session.day_hourly_work_tokens {
                    *hourly_agg.entry(*hour).or_default() += tokens;
                }

                for (tool, count) in &session.day_tool_usage {
                    *tool_counts.entry(tool.clone()).or_default() += count;
                }
                for (lang, count) in &session.day_language_usage {
                    *lang_counts.entry(lang.clone()).or_default() += count;
                }
            }
        }

        let mut model_breakdown: Vec<serde_json::Value> = model_agg
            .into_iter()
            .map(|(model, (cost, tokens))| {
                serde_json::json!({
                    "model": model,
                    "cost_usd": (cost * 100.0).round() / 100.0,
                    "tokens": tokens,
                })
            })
            .collect();
        model_breakdown.sort_by(|a, b| {
            b["cost_usd"]
                .as_f64()
                .unwrap_or(0.0)
                .partial_cmp(&a["cost_usd"].as_f64().unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut projects: Vec<serde_json::Value> = project_agg
            .into_iter()
            .map(|(project, (sessions, work_tokens, cost))| {
                serde_json::json!({
                    "project": project,
                    "sessions": sessions,
                    "work_tokens": work_tokens,
                    "cost_usd": (cost * 100.0).round() / 100.0,
                })
            })
            .collect();
        projects.sort_by(|a, b| {
            b["cost_usd"]
                .as_f64()
                .unwrap_or(0.0)
                .partial_cmp(&a["cost_usd"].as_f64().unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut hourly_pattern: HashMap<String, u64> = HashMap::new();
        for (hour, tokens) in &hourly_agg {
            hourly_pattern.insert(hour.to_string(), *tokens);
        }

        let today = Local::now().date_naive();
        let period_label = if params.date_from.is_some() {
            "custom"
        } else {
            params.period.as_deref().unwrap_or("all")
        };
        let date_range = match (start, end) {
            (Some(s), Some(e)) => serde_json::json!({"start": s.to_string(), "end": e.to_string()}),
            (Some(s), None) => {
                serde_json::json!({"start": s.to_string(), "end": today.to_string()})
            }
            _ => serde_json::json!(null),
        };

        let cache_hit_rate = if total_input + total_cache_read > 0 {
            (total_cache_read as f64 / (total_input + total_cache_read) as f64 * 1000.0).round()
                / 10.0
        } else {
            0.0
        };

        let total_tool_calls: usize = tool_counts.values().sum();
        let mut top_tools: Vec<serde_json::Value> = tool_counts
            .into_iter()
            .map(|(tool, count)| serde_json::json!({"tool": tool, "count": count}))
            .collect();
        top_tools.sort_by(|a, b| {
            b["count"]
                .as_u64()
                .unwrap_or(0)
                .cmp(&a["count"].as_u64().unwrap_or(0))
        });
        top_tools.truncate(10);

        let mut languages: Vec<serde_json::Value> = lang_counts
            .into_iter()
            .map(|(lang, count)| serde_json::json!({"language": lang, "count": count}))
            .collect();
        languages.sort_by(|a, b| {
            b["count"]
                .as_u64()
                .unwrap_or(0)
                .cmp(&a["count"].as_u64().unwrap_or(0))
        });

        // Surface models that lack a pricing entry. Without this hint, consumers of the
        // stats tool would see a `total_cost_usd` that silently excludes any spend on
        // unknown models (e.g., a newly released model before `pricing.rs` is updated).
        let pricing_gap_json = if models_without_pricing.is_empty() {
            serde_json::Value::Null
        } else {
            let mut names: Vec<&String> = models_without_pricing.iter().collect();
            names.sort();
            serde_json::json!({
                "count": names.len(),
                "models": names,
                "note": "total_cost_usd excludes spend on these models because pricing is not yet defined",
            })
        };

        // Per-MCP-server adoption snapshot: configured, last_used, total_calls,
        // status (active / stale-idle / stale-never / inactive). Mirrors what
        // the Tools popup shows. Useful for users asking "which servers are
        // I paying mental tax on but never invoking" via the MCP API.
        let mcp_status_json = {
            let now = chrono::Utc::now();
            let owned: Vec<crate::aggregator::DailyGroup> =
                filtered.iter().map(|g| (*g).clone()).collect();
            let mut entries: Vec<serde_json::Value> =
                crate::infrastructure::compute_mcp_status(&owned)
                    .into_iter()
                    .map(|s| {
                        // `is_underutilized(now, 30)` is the canonical "stale"
                        // predicate; reusing it here keeps the MCP API field
                        // in lockstep with the TUI legend / popup ⚠ marker.
                        let status = if !s.configured {
                            "inactive"
                        } else if s.last_used.is_none() {
                            "stale-never"
                        } else if s.is_underutilized(now, 30) {
                            "stale-idle"
                        } else {
                            "active"
                        };
                        serde_json::json!({
                            "name": s.name,
                            "configured": s.configured,
                            "last_used": s.last_used.map(|t| t.to_rfc3339()),
                            "total_calls": s.total_calls,
                            "status": status,
                        })
                    })
                    .collect();
            entries.sort_by(|a, b| {
                a["status"]
                    .as_str()
                    .cmp(&b["status"].as_str())
                    .then_with(|| {
                        b["total_calls"]
                            .as_u64()
                            .unwrap_or(0)
                            .cmp(&a["total_calls"].as_u64().unwrap_or(0))
                    })
                    .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
            });
            entries
        };

        let mut result = serde_json::json!({
            "period": period_label,
            "date_range": date_range,
            "total_cost_usd": (total_cost * 100.0).round() / 100.0,
            "pricing_gap": pricing_gap_json,
            "mcp_servers": mcp_status_json,
            "total_sessions": session_count,
            "tokens": {
                "input": total_input,
                "output": total_output,
                "cache_creation": total_cache_creation,
                "cache_read": total_cache_read,
                "total_work": total_input + total_output,
            },
            "cache_hit_rate_pct": cache_hit_rate,
            "model_breakdown": model_breakdown,
            "projects": projects,
            "hourly_pattern": hourly_pattern,
            "tool_calls": total_tool_calls,
            "top_tools": top_tools,
            "languages": languages,
        });

        if group_by == Some("day") {
            let mut daily: Vec<serde_json::Value> = daily_agg
                .into_iter()
                .map(|(date, (cost, sessions, tokens))| {
                    serde_json::json!({
                        "date": date.to_string(),
                        "cost_usd": (cost * 100.0).round() / 100.0,
                        "sessions": sessions,
                        "work_tokens": tokens,
                    })
                })
                .collect();
            daily.sort_by(|a, b| a["date"].as_str().cmp(&b["date"].as_str()));
            result["daily"] = serde_json::json!(daily);
        }

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&result).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Full-text search across all session conversations using tantivy index. Returns matching sessions with snippets. Also searches metadata (project, summary, branch, date). All dates/times are in local timezone."
    )]
    fn search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let data = self.load();
        let limit = params.limit.unwrap_or(20);
        let query = &params.query;

        let fetch_limit = limit * 5;
        let mut meta_results = crate::search::perform_search(&data.daily_groups, query);

        if let Ok(index) = crate::infrastructure::SearchIndex::update_or_build(&data.daily_groups) {
            let content_results = index.search(query, fetch_limit, 200);
            for result in content_results {
                if !meta_results
                    .iter()
                    .any(|r| r.day_idx == result.day_idx && r.session_idx == result.session_idx)
                {
                    meta_results.push(result);
                }
            }
        }

        let date_from = params
            .date_from
            .as_deref()
            .and_then(|d| d.parse::<NaiveDate>().ok());
        let date_to = params
            .date_to
            .as_deref()
            .and_then(|d| d.parse::<NaiveDate>().ok());

        let calculator = CostCalculator::global();
        let mut results_json: Vec<serde_json::Value> = Vec::new();

        for result in &meta_results {
            if results_json.len() >= limit {
                break;
            }
            let Some(group) = data.daily_groups.get(result.day_idx) else {
                continue;
            };
            if let Some(from) = date_from
                && group.date < from
            {
                continue;
            }
            if let Some(to) = date_to
                && group.date > to
            {
                continue;
            }
            let Some(session) = group
                .sessions
                .iter()
                .filter(|s| !s.is_subagent)
                .nth(result.session_idx)
            else {
                continue;
            };
            if let Some(ref proj) = params.project
                && !session
                    .project_name
                    .to_lowercase()
                    .contains(&proj.to_lowercase())
            {
                continue;
            };

            let cost: f64 = session
                .day_tokens_by_model
                .iter()
                .filter_map(|(model, tokens)| calculator.calculate_cost(tokens, Some(model)))
                .sum();

            let session_id: String = session
                .file_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .chars()
                .take(8)
                .collect();

            let match_type = match result.match_type {
                crate::search::SearchMatchType::ProjectName => "project",
                crate::search::SearchMatchType::Summary => "summary",
                crate::search::SearchMatchType::GitBranch => "branch",
                crate::search::SearchMatchType::SessionId => "session_id",
                crate::search::SearchMatchType::Date => "date",
                crate::search::SearchMatchType::Content => "content",
            };

            results_json.push(serde_json::json!({
                "date": group.date.to_string(),
                "project": session.project_name,
                "summary": session.ai_title.as_deref().or(session.custom_title.as_deref()).or(session.summary.as_deref()),
                "snippet": result.snippet,
                "match_type": match_type,
                "tokens": session.work_tokens(),
                "cost_usd": (cost * 100.0).round() / 100.0,
                "session_id": session_id,
                "git_branch": session.git_branch,
            }));
        }

        let output = serde_json::json!({
            "query": query,
            "total_results": results_json.len(),
            "results": results_json,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&output).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "List currently running and recently disconnected Claude Code sessions. Sources: ~/.claude/sessions/<pid>.json (PID liveness verified), ~/.claude/projects/**/*.jsonl mtime (24h window), and ccsight's snapshot of previously-alive session IDs. Each row reports session_id, cwd, project, status tier (busy/warm/idle/paused), age (seconds since last update), pid (0 for paused), today's tokens/cost, model, ai_title, and last user-message preview. Use the `live` tab semantics to find what's open right now or what was running before a reboot. All timestamps local timezone."
    )]
    fn live_sessions(
        &self,
        Parameters(params): Parameters<LiveSessionsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        use crate::infrastructure::live_sessions::{
            LiveSession, WARM_THRESHOLD_SECS, discover_live, discover_recently_paused,
            mark_was_recently_live,
        };
        use crate::infrastructure::live_snapshot::LiveSnapshot;

        let active = discover_live();
        let active_ids: std::collections::HashSet<String> =
            active.iter().map(|s| s.session_id.clone()).collect();
        let mut paused = discover_recently_paused(
            &active_ids,
            std::time::Duration::from_secs(24 * 3600),
            std::time::SystemTime::now(),
        );
        let snapshot = LiveSnapshot::load();
        // MCP is a one-shot read; it doesn't observe alive→dead transitions
        // across its own poll cycles, so use `now` as the horizon. Every
        // snapshot match qualifies as `⟳` (the previous flag semantics).
        // Currently-alive sessions are already filtered out of `paused`.
        let horizon = chrono::Utc::now();
        mark_was_recently_live(&mut paused, &snapshot, horizon);
        let paused_ids: std::collections::HashSet<String> =
            paused.iter().map(|s| s.session_id.clone()).collect();
        let recovered = snapshot.recover_missing(&active_ids, &paused_ids, horizon);
        paused.extend(recovered);
        mark_was_recently_live(&mut paused, &snapshot, horizon);

        // Lookup table from `daily_groups` so we can enrich each live row
        // with ai_title / last_user_message / tokens / cost / model. Build
        // once per call (not per row) — this is the same memoization
        // pattern the TUI Live tab needs.
        let data = self.load();
        let calculator = CostCalculator::global();
        let mut by_path: HashMap<&std::path::Path, &crate::aggregator::SessionInfo> =
            HashMap::new();
        for g in &data.daily_groups {
            for s in &g.sessions {
                by_path.entry(s.file_path.as_path()).or_insert(s);
            }
        }

        let now = chrono::Utc::now();
        let classify_tier = |s: &LiveSession| -> &'static str {
            if !s.is_live {
                "paused"
            } else if s.status.as_deref() == Some("busy") {
                "busy"
            } else {
                let secs = s
                    .updated_at
                    .or(s.jsonl_mtime)
                    .or(s.started_at)
                    .map_or(0, |t| (now - t).num_seconds().max(0));
                if secs < WARM_THRESHOLD_SECS {
                    "warm"
                } else {
                    "idle"
                }
            }
        };

        let want_status = params.status.as_deref();
        let want_project = params.project.as_deref();

        let project_label = |cwd: &std::path::Path| -> String {
            cwd.file_name()
                .and_then(|n| n.to_str())
                .map_or_else(|| cwd.display().to_string(), str::to_string)
        };

        let to_row = |s: &LiveSession, tier: &'static str| -> serde_json::Value {
            let info = s
                .jsonl_path
                .as_deref()
                .and_then(|p| by_path.get(p).copied());
            let age_secs = s
                .updated_at
                .or(s.jsonl_mtime)
                .or(s.started_at)
                .map(|t| (now - t).num_seconds().max(0));
            let tokens: u64 = info.map_or(0, |i| {
                i.day_tokens_by_model
                    .values()
                    .map(crate::aggregator::TokenStats::work_tokens)
                    .sum()
            });
            let cost: f64 = info.map_or(0.0, |i| {
                i.day_tokens_by_model
                    .iter()
                    .map(|(m, t)| calculator.calculate_cost(t, Some(m)).unwrap_or(0.0))
                    .sum()
            });
            let model_normalized = info
                .and_then(|i| i.model.as_ref())
                .map(|m| crate::aggregator::normalize_model_name(m));
            serde_json::json!({
                "session_id": s.session_id,
                "cwd": s.cwd.display().to_string(),
                "project": project_label(&s.cwd),
                "status": tier,
                "age_secs": age_secs,
                "pid": s.pid,
                "was_recently_live": s.was_recently_live,
                "tokens": tokens,
                "cost": cost,
                "model": model_normalized,
                "ai_title": info.and_then(|i| i.ai_title.clone()),
                "last_user_message": info.and_then(|i| i.last_user_message.clone()),
            })
        };

        let mut rows: Vec<serde_json::Value> = Vec::new();
        for s in &active {
            let tier = classify_tier(s);
            if let Some(want) = want_status
                && want != tier
            {
                continue;
            }
            if let Some(proj) = want_project
                && !s.cwd.to_string_lossy().contains(proj)
            {
                continue;
            }
            rows.push(to_row(s, tier));
        }
        for s in &paused {
            let tier = "paused";
            if let Some(want) = want_status
                && want != tier
            {
                continue;
            }
            if let Some(proj) = want_project
                && !s.cwd.to_string_lossy().contains(proj)
            {
                continue;
            }
            rows.push(to_row(s, tier));
        }

        let output = serde_json::json!({
            "active_count": active.len(),
            "paused_count": paused.len(),
            "sessions": rows,
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&output).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "List sessions or get session detail. With session_id: returns conversation, tool usage, files touched. Without: lists sessions filtered by query (metadata only: project/summary/branch/date), project, date range, sort. For full-text content search, use 'search' tool instead. All dates/times are in local timezone."
    )]
    fn sessions(
        &self,
        Parameters(params): Parameters<SessionsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let data = self.load();

        if let Some(ref sid) = params.session_id {
            return Self::session_detail_inner_with_data(
                &data,
                sid,
                params.date.as_deref(),
                params.conversation_limit,
                params.conversation_offset,
                params.conversation_query.as_deref(),
            );
        }

        let limit = params.limit.unwrap_or(20);
        let calculator = CostCalculator::global();
        let mut sessions_out: Vec<serde_json::Value> = Vec::new();

        let date_exact = params
            .date
            .as_deref()
            .and_then(|d| d.parse::<NaiveDate>().ok());
        let date_from = params
            .date_from
            .as_deref()
            .and_then(|d| d.parse::<NaiveDate>().ok());
        let date_to = params
            .date_to
            .as_deref()
            .and_then(|d| d.parse::<NaiveDate>().ok());

        let date_matches = |group_date: NaiveDate| -> bool {
            if let Some(exact) = date_exact {
                return group_date == exact;
            }
            if let Some(from) = date_from
                && group_date < from
            {
                return false;
            }
            if let Some(to) = date_to
                && group_date > to
            {
                return false;
            }
            true
        };

        let pins = if params.pinned == Some(true) {
            Some(crate::pins::Pins::load().unwrap_or_else(|_| crate::pins::Pins::empty()))
        } else {
            None
        };

        let should_include =
            |group: &DailyGroup, session: &crate::aggregator::SessionInfo| -> bool {
                if !date_matches(group.date) {
                    return false;
                }
                if let Some(ref project) = params.project
                    && !session
                        .project_name
                        .to_lowercase()
                        .contains(&project.to_lowercase())
                {
                    return false;
                }
                if let Some(ref p) = pins
                    && !p.is_pinned(&session.file_path)
                {
                    return false;
                }
                true
            };

        for group in &data.daily_groups {
            for session in &group.sessions {
                if session.is_subagent {
                    continue;
                }
                if !should_include(group, session) {
                    continue;
                }
                let mut json =
                    build_session_json(group.date, session, calculator, &data.summary_cache);
                let auto = json["summary"]
                    .as_str()
                    .is_some_and(|s| AUTO_PREFIXES.iter().any(|p| s.starts_with(p)));
                if auto {
                    json["auto"] = serde_json::json!(true);
                }
                if let Some(ref query) = params.query {
                    let q = query.to_lowercase();
                    let meta_match = session.project_name.to_lowercase().contains(&q)
                        || json["summary"]
                            .as_str()
                            .is_some_and(|s| s.to_lowercase().contains(&q))
                        || session
                            .git_branch
                            .as_deref()
                            .is_some_and(|b| b.to_lowercase().contains(&q))
                        || group.date.to_string().contains(&q);
                    if !meta_match {
                        continue;
                    }
                }
                sessions_out.push(json);
            }
        }

        match params.sort.as_deref() {
            Some("cost") => sessions_out.sort_by(|a, b| {
                b["cost_usd"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .partial_cmp(&a["cost_usd"].as_f64().unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            Some("tokens") => sessions_out.sort_by(|a, b| {
                let ta = a["tokens"]["input"].as_u64().unwrap_or(0)
                    + a["tokens"]["output"].as_u64().unwrap_or(0);
                let tb = b["tokens"]["input"].as_u64().unwrap_or(0)
                    + b["tokens"]["output"].as_u64().unwrap_or(0);
                tb.cmp(&ta)
            }),
            _ => {}
        }

        sessions_out.truncate(limit);

        let result = serde_json::json!({
            "count": sessions_out.len(),
            "sessions": sessions_out,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&result).unwrap_or_default(),
        )]))
    }
}

impl CcsightServer {
    fn session_detail_inner_with_data(
        data: &LoadedData,
        session_id: &str,
        date_str: Option<&str>,
        conv_limit: Option<usize>,
        conv_offset: Option<usize>,
        conv_query: Option<&str>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let date_filter = date_str.and_then(|d| d.parse::<NaiveDate>().ok());

        let found = data
            .daily_groups
            .iter()
            .filter(|g| date_filter.is_none() || Some(g.date) == date_filter)
            .flat_map(|g| g.sessions.iter().map(move |s| (g.date, s)))
            .find(|(_, s)| {
                s.file_path
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(session_id))
            });

        let Some((date, session)) = found else {
            return Ok(CallToolResult::success(vec![Content::text(
                serde_json::json!({"error": "Session not found"}).to_string(),
            )]));
        };

        let calculator = CostCalculator::global();
        let session_json = build_session_json(date, session, calculator, &data.summary_cache);

        let messages = load_conversation(&session.file_path).unwrap_or_default();
        let files_touched = extract_files_touched(&messages);

        let conversation_full = extract_conversation(&messages);
        let total = conversation_full.len();

        let conversation = if let Some(q) = conv_query {
            let q_lower = q.to_lowercase();
            let match_indices: Vec<usize> = conversation_full
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    c["text"]
                        .as_str()
                        .is_some_and(|t| t.to_lowercase().contains(&q_lower))
                })
                .map(|(i, _)| i)
                .collect();

            let limit = conv_limit.unwrap_or(30);
            let mut include = std::collections::BTreeSet::new();
            for &idx in &match_indices {
                if idx > 0 {
                    include.insert(idx - 1);
                }
                include.insert(idx);
                if idx + 1 < total {
                    include.insert(idx + 1);
                }
            }

            let mut result_conv = Vec::new();
            let mut prev_idx: Option<usize> = None;
            for &idx in &include {
                if result_conv.len() >= limit {
                    break;
                }
                if let Some(prev) = prev_idx
                    && idx > prev + 1
                {
                    result_conv.push(serde_json::json!({
                        "role": "...",
                        "text": format!("({} entries omitted)", idx - prev - 1),
                    }));
                }
                let mut entry = conversation_full[idx].clone();
                entry["offset"] = serde_json::json!(idx);
                if match_indices.contains(&idx) {
                    entry["match"] = serde_json::json!(true);
                }
                result_conv.push(entry);
                prev_idx = Some(idx);
            }
            result_conv
        } else {
            let max_conv = conv_limit.unwrap_or(usize::MAX);
            let offset = conv_offset.unwrap_or(0);
            if max_conv == 0 {
                vec![]
            } else if offset > 0 {
                let start = offset.min(total);
                let end = (start + max_conv).min(total);
                conversation_full[start..end].to_vec()
            } else if total <= max_conv {
                conversation_full
            } else {
                let head = max_conv * 2 / 3;
                let tail = max_conv - head;
                let mut v = conversation_full[..head].to_vec();
                v.push(serde_json::json!({
                    "role": "...",
                    "text": format!("({} entries omitted. Use conversation_offset to paginate, e.g. offset={} limit={})", total - head - tail, head, max_conv),
                }));
                v.extend_from_slice(&conversation_full[total - tail..]);
                v
            }
        };

        let result = serde_json::json!({
            "session": session_json,
            "conversation": conversation,
            "conversation_total": total,
            "tool_usage": session.day_tool_usage,
            "files_touched": files_touched,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&result).unwrap_or_default(),
        )]))
    }
}

fn clean_summary(text: &str) -> String {
    let text = text.trim();
    if text.starts_with("Implement the following plan:") || text.starts_with("/plan") {
        for line in text.lines().skip(1) {
            let line = line.trim();
            if let Some(title) = line.strip_prefix("# ") {
                return truncate(title.trim(), 120);
            }
            if !line.is_empty() && !line.starts_with('#') && line.len() > 5 {
                return truncate(line, 120);
            }
        }
    }
    truncate(text, 120)
}

fn truncate(text: &str, max: usize) -> String {
    let truncated: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn extract_conversation(messages: &[ConversationMessage]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .filter_map(|msg| {
            let text: String = msg
                .blocks
                .iter()
                .filter_map(|b| match b {
                    ConversationBlock::Text(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            if text.is_empty() {
                return None;
            }
            let max_len = if msg.role == "user" { 300 } else { 200 };
            Some(serde_json::json!({
                "role": msg.role,
                "time": msg.timestamp,
                "text": truncate(&text, max_len),
            }))
        })
        .collect()
}

fn extract_files_touched(messages: &[ConversationMessage]) -> Vec<String> {
    let mut files = std::collections::HashSet::new();
    for msg in messages {
        for block in &msg.blocks {
            if let ConversationBlock::ToolUse {
                name,
                input_summary,
            } = block
                && matches!(name.as_str(), "Read" | "Edit" | "Write" | "MultiEdit")
                && !input_summary.is_empty()
            {
                files.insert(input_summary.clone());
            }
        }
    }
    let mut sorted: Vec<String> = files.into_iter().collect();
    sorted.sort();
    sorted
}

const AUTO_PREFIXES: &[&str] = &["You are a rule generator", "Generate a git commit message"];

fn derive_summary(file_path: &std::path::Path) -> Option<String> {
    use crate::domain::EntryType;
    use crate::parser::JsonlParser;

    let entries = JsonlParser::parse_file(file_path).ok()?;

    for entry in entries.iter().take(50) {
        if entry.entry_type != EntryType::User {
            continue;
        }
        let message = entry.message.as_ref()?;
        let text = message.content.extract_text();
        let text = text.trim();
        if text.is_empty() || text.starts_with('<') || text.starts_with('[') {
            continue;
        }
        if AUTO_PREFIXES.iter().any(|p| text.starts_with(p)) {
            continue;
        }
        return Some(clean_summary(text));
    }
    None
}

fn build_session_json(
    date: NaiveDate,
    session: &crate::aggregator::SessionInfo,
    calculator: &CostCalculator,
    summary_cache: &HashMap<PathBuf, Option<String>>,
) -> serde_json::Value {
    let mut cost = 0.0;
    let mut cache_creation = 0u64;
    let mut cache_read = 0u64;
    for (model, tokens) in &session.day_tokens_by_model {
        cost += calculator
            .calculate_cost(tokens, Some(model))
            .unwrap_or(0.0);
        cache_creation += tokens.cache_creation_tokens;
        cache_read += tokens.cache_read_tokens;
    }

    let first_local = session.day_first_timestamp.with_timezone(&Local);
    let last_local = session.day_last_timestamp.with_timezone(&Local);
    let time_range = format!(
        "{}–{}",
        first_local.format("%H:%M"),
        last_local.format("%H:%M")
    );
    let duration_mins = (session.day_last_timestamp - session.day_first_timestamp).num_minutes();

    let session_id = session
        .file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .chars()
        .take(8)
        .collect::<String>();

    let model_display = session
        .model
        .as_deref()
        .map(crate::aggregator::normalize_model_name)
        .unwrap_or_default();

    let summary = session
        .summary
        .clone()
        .or_else(|| summary_cache.get(&session.file_path).and_then(Clone::clone));

    serde_json::json!({
        "session_id": session_id,
        "date": date.to_string(),
        "time": time_range,
        "project": session.project_name,
        "model": model_display,
        "tokens": {
            "input": session.day_input_tokens,
            "output": session.day_output_tokens,
            "cache_creation": cache_creation,
            "cache_read": cache_read,
        },
        "cost_usd": (cost * 100.0).round() / 100.0,
        "summary": summary,
        "git_branch": session.git_branch,
        "duration_mins": duration_mins,
    })
}

#[tool_handler]
impl ServerHandler for CcsightServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("ccsight", env!("CARGO_PKG_VERSION")))
            .with_instructions("Claude Code usage analytics. Use 'stats' for aggregated metrics, 'sessions' to search sessions. Tool guide: 'search' to find past conversations by content (full-text, returns snippets with session_id) -> 'sessions' with session_id to get conversation detail. 'stats' for cost/token/model summaries. All tools accept date_from/date_to (YYYY-MM-DD) for date filtering.")
    }
}

pub async fn run_mcp_server(limit: usize) -> anyhow::Result<()> {
    let server = CcsightServer::new(limit);
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::test_helpers::helpers::{make_daily_group, make_session, make_session_with_tokens};

    struct TempFile(PathBuf);
    use std::sync::atomic::{AtomicU64, Ordering};
    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    impl TempFile {
        fn new(prefix: &str) -> Self {
            let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!("{prefix}_{id}.jsonl")))
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
        fn write_jsonl(&self, entries: &[serde_json::Value]) {
            let mut f = std::fs::File::create(&self.0).unwrap();
            for e in entries {
                writeln!(f, "{}", serde_json::to_string(e).unwrap()).unwrap();
            }
        }
    }
    impl Drop for TempFile {
        fn drop(&mut self) {
            std::fs::remove_file(&self.0).ok();
        }
    }

    fn extract_json(result: &CallToolResult) -> serde_json::Value {
        let text = match &result.content[0].raw {
            rmcp::model::RawContent::Text(t) => &t.text,
            _ => panic!("expected text content"),
        };
        serde_json::from_str(text).unwrap()
    }

    fn call_stats(server: &CcsightServer, period: Option<&str>) -> serde_json::Value {
        let params = StatsParams {
            period: period.map(String::from),
            date_from: None,
            date_to: None,
            group_by: None,
        };
        extract_json(&server.stats(Parameters(params)).unwrap())
    }

    fn call_stats_range(
        server: &CcsightServer,
        start: &str,
        end: Option<&str>,
    ) -> serde_json::Value {
        let params = StatsParams {
            period: None,
            date_from: Some(start.to_string()),
            date_to: end.map(String::from),
            group_by: None,
        };
        extract_json(&server.stats(Parameters(params)).unwrap())
    }

    fn call_sessions(
        server: &CcsightServer,
        query: Option<&str>,
        project: Option<&str>,
        date: Option<&str>,
        limit: Option<usize>,
    ) -> serde_json::Value {
        let params = SessionsParams {
            session_id: None,
            query: query.map(String::from),
            project: project.map(String::from),
            date: date.map(String::from),
            date_from: None,
            date_to: None,
            sort: None,
            limit,
            conversation_limit: None,
            conversation_offset: None,
            conversation_query: None,
            pinned: None,
        };
        extract_json(&server.sessions(Parameters(params)).unwrap())
    }

    fn call_session_detail(
        server: &CcsightServer,
        session_id: &str,
        date: Option<&str>,
    ) -> serde_json::Value {
        let params = SessionsParams {
            session_id: Some(session_id.to_string()),
            query: None,
            project: None,
            date: date.map(String::from),
            date_from: None,
            date_to: None,
            sort: None,
            limit: None,
            conversation_limit: None,
            conversation_offset: None,
            conversation_query: None,
            pinned: None,
        };
        extract_json(&server.sessions(Parameters(params)).unwrap())
    }

    fn make_test_groups() -> Vec<DailyGroup> {
        let today = Local::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let last_week = today - chrono::Duration::days(8);

        vec![
            make_daily_group(
                today,
                vec![
                    make_session_with_tokens(
                        "~/projects/app-a",
                        100_000,
                        50_000,
                        "claude-sonnet-4-20250514",
                    ),
                    {
                        let mut s = make_session_with_tokens(
                            "~/projects/other",
                            50_000,
                            25_000,
                            "claude-sonnet-4-20250514",
                        );
                        s.summary = Some("Fix login bug".to_string());
                        s.git_branch = Some("fix/login".to_string());
                        s
                    },
                ],
            ),
            make_daily_group(
                yesterday,
                vec![make_session_with_tokens(
                    "~/projects/app-a",
                    80_000,
                    40_000,
                    "claude-opus-4-5-20251101",
                )],
            ),
            make_daily_group(
                last_week,
                vec![make_session_with_tokens(
                    "~/projects/old-project",
                    30_000,
                    10_000,
                    "claude-sonnet-4-20250514",
                )],
            ),
        ]
    }

    #[test]
    fn test_period_to_filter_mappings() {
        assert_eq!(period_to_filter(Some("today")), PeriodFilter::Today);
        assert_eq!(period_to_filter(Some("week")), PeriodFilter::Last7d);
        assert_eq!(period_to_filter(Some("month")), PeriodFilter::Last30d);
        assert_eq!(period_to_filter(Some("all")), PeriodFilter::All);
        assert_eq!(period_to_filter(None), PeriodFilter::All);
        assert_eq!(period_to_filter(Some("unknown")), PeriodFilter::All);
    }

    #[test]
    fn test_filter_groups_by_date_range_no_filter() {
        let groups = make_test_groups();
        let filtered = filter_groups_by_date_range(&groups, None, None);
        assert_eq!(filtered.len(), 3);
    }

    #[test]
    fn test_filter_groups_by_date_range_start_only() {
        let groups = make_test_groups();
        let today = Local::now().date_naive();
        let filtered = filter_groups_by_date_range(&groups, Some(today), None);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].date, today);
    }

    #[test]
    fn test_filter_groups_by_date_range_start_and_end() {
        let groups = make_test_groups();
        let today = Local::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let filtered = filter_groups_by_date_range(&groups, Some(yesterday), Some(today));
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_filter_groups_empty_result() {
        let groups = make_test_groups();
        let future = Local::now().date_naive() + chrono::Duration::days(30);
        let filtered = filter_groups_by_date_range(&groups, Some(future), None);
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_build_session_json_fields() {
        let session = make_session_with_tokens(
            "~/projects/myapp",
            100_000,
            50_000,
            "claude-sonnet-4-20250514",
        );
        let date = NaiveDate::from_ymd_opt(2026, 3, 19).unwrap(); // lint-ok: date-literal
        let calculator = CostCalculator::global();

        let json = build_session_json(date, &session, calculator, &HashMap::new());

        assert_eq!(json["date"], "2026-03-19"); // lint-ok: date-literal
        assert_eq!(json["project"], "~/projects/myapp");
        assert_eq!(json["model"], "Sonnet 4");
        assert_eq!(json["tokens"]["input"], 100_000);
        assert_eq!(json["tokens"]["output"], 50_000);
        assert!(json["cost_usd"].as_f64().unwrap() > 0.0);
        assert!(json["duration_mins"].is_number());
        assert!(json["time"].is_string());
    }

    #[test]
    fn test_build_session_json_with_summary_and_branch() {
        let mut session = make_session(
            "~/projects/app",
            Some("Added MCP support"),
            Some("feature/mcp"),
        );
        session.model = Some("claude-sonnet-4-20250514".to_string());
        let date = NaiveDate::from_ymd_opt(2026, 3, 19).unwrap(); // lint-ok: date-literal
        let json = build_session_json(date, &session, CostCalculator::global(), &HashMap::new());

        assert_eq!(json["summary"], "Added MCP support");
        assert_eq!(json["git_branch"], "feature/mcp");
    }

    #[test]
    fn test_build_session_json_null_optional_fields() {
        let session =
            make_session_with_tokens("~/projects/app", 1000, 500, "claude-sonnet-4-20250514");
        let date = NaiveDate::from_ymd_opt(2026, 3, 19).unwrap(); // lint-ok: date-literal
        let json = build_session_json(date, &session, CostCalculator::global(), &HashMap::new());

        assert!(json["summary"].is_null());
        assert!(json["git_branch"].is_null());
    }

    #[test]
    fn test_build_session_json_session_id_truncated() {
        let mut session =
            make_session_with_tokens("~/projects/app", 1000, 500, "claude-sonnet-4-20250514");
        session.file_path = std::path::PathBuf::from("/tmp/abcdefghijklmnop.jsonl");
        let date = NaiveDate::from_ymd_opt(2026, 3, 19).unwrap(); // lint-ok: date-literal
        let json = build_session_json(date, &session, CostCalculator::global(), &HashMap::new());

        assert_eq!(json["session_id"].as_str().unwrap().len(), 8);
        assert_eq!(json["session_id"], "abcdefgh");
    }

    #[test]
    fn test_stats_all_period() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_stats(&server, None);

        assert_eq!(json["period"], "all");
        assert_eq!(json["total_sessions"], 4);
        assert!(json["total_cost_usd"].as_f64().unwrap() > 0.0);
        assert!(json["tokens"]["input"].as_u64().unwrap() > 0);
        assert!(json["tokens"]["output"].as_u64().unwrap() > 0);
        assert!(!json["model_breakdown"].as_array().unwrap().is_empty());
        assert!(!json["projects"].as_array().unwrap().is_empty());
        assert!(json["projects"][0]["cost_usd"].as_f64().is_some());
        assert!(json["cache_hit_rate_pct"].as_f64().is_some());
        assert!(json["tokens"]["cache_creation"].as_u64().is_some());
        assert!(json["tokens"]["cache_read"].as_u64().is_some());
    }

    #[test]
    fn test_stats_today_period() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_stats(&server, Some("today"));

        assert_eq!(json["period"], "today");
        assert_eq!(json["total_sessions"], 2);
    }

    #[test]
    fn test_stats_week_period() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_stats(&server, Some("week"));

        assert_eq!(json["period"], "week");
        assert_eq!(
            json["total_sessions"], 3,
            "should include today + yesterday but not last_week"
        );
    }

    #[test]
    fn test_stats_subagent_cost_included_sessions_excluded() {
        let today = Local::now().date_naive();
        let mut subagent =
            make_session_with_tokens("~/projects/app", 1000, 500, "claude-sonnet-4-20250514");
        subagent.is_subagent = true;

        let groups = vec![make_daily_group(
            today,
            vec![
                make_session_with_tokens("~/projects/app", 1000, 500, "claude-sonnet-4-20250514"),
                subagent,
            ],
        )];
        let server = CcsightServer::from_groups(groups);
        let json = call_stats(&server, None);

        assert_eq!(
            json["total_sessions"], 1,
            "session count excludes subagents"
        );
        assert_eq!(
            json["tokens"]["input"], 2000,
            "token counts include subagents"
        );
        assert!(
            json["total_cost_usd"].as_f64().unwrap() > 0.0,
            "cost includes subagents"
        );

        let single = call_stats(
            &CcsightServer::from_groups(vec![make_daily_group(
                today,
                vec![make_session_with_tokens(
                    "~/projects/app",
                    1000,
                    500,
                    "claude-sonnet-4-20250514",
                )],
            )]),
            None,
        );
        let cost_without = single["total_cost_usd"].as_f64().unwrap();
        let cost_with = json["total_cost_usd"].as_f64().unwrap();
        assert!(
            cost_with > cost_without,
            "cost with subagent should be higher"
        );
    }

    #[test]
    fn test_stats_projects_returns_all() {
        let today = Local::now().date_naive();
        let sessions: Vec<_> = (0..8)
            .map(|i| {
                make_session_with_tokens(
                    &format!("~/projects/project-{i}"),
                    1000,
                    500,
                    "claude-sonnet-4-20250514",
                )
            })
            .collect();
        let server = CcsightServer::from_groups(vec![make_daily_group(today, sessions)]);
        let json = call_stats(&server, None);

        assert_eq!(json["projects"].as_array().unwrap().len(), 8);
    }

    #[test]
    fn test_stats_empty_groups() {
        let server = CcsightServer::from_groups(vec![]);
        let json = call_stats(&server, None);

        assert_eq!(json["total_sessions"], 0);
        assert_eq!(json["total_cost_usd"], 0.0);
        assert_eq!(json["tokens"]["input"], 0);
        // No sessions → no pricing gap to report.
        assert!(json["pricing_gap"].is_null());
    }

    #[test]
    fn test_stats_pricing_gap_flagged_for_unknown_model() {
        // Regression: when a session uses a model not in `pricing.rs`, the stats tool
        // must surface `pricing_gap` so callers know `total_cost_usd` is undercounted.
        let today = Local::now().date_naive();
        let groups = vec![make_daily_group(
            today,
            vec![make_session_with_tokens(
                "~/projects/app",
                100_000,
                50_000,
                "claude-future-experimental-x",
            )],
        )];
        let server = CcsightServer::from_groups(groups);
        let json = call_stats(&server, None);

        assert!(
            !json["pricing_gap"].is_null(),
            "pricing_gap should be present when an unknown model is used. Got: {json}"
        );
        assert_eq!(json["pricing_gap"]["count"], 1);
        let models = json["pricing_gap"]["models"]
            .as_array()
            .expect("models is array");
        assert_eq!(models.len(), 1);
        // The model name is normalized — future families preserve their raw name.
        assert!(
            models[0]
                .as_str()
                .is_some_and(|s| s.contains("future-experimental-x")),
            "pricing_gap.models should include the raw/normalized unknown model name. Got: {json}"
        );
    }

    #[test]
    fn test_stats_no_pricing_gap_when_all_models_priced() {
        // Regression: when every model in the window has pricing, `pricing_gap` must be
        // null so downstream consumers don't warn unnecessarily.
        let today = Local::now().date_naive();
        let groups = vec![make_daily_group(
            today,
            vec![make_session_with_tokens(
                "~/projects/app",
                100_000,
                50_000,
                "claude-opus-4-5-20251101",
            )],
        )];
        let server = CcsightServer::from_groups(groups);
        let json = call_stats(&server, None);

        assert!(
            json["pricing_gap"].is_null(),
            "pricing_gap should be null when all models are priced. Got: {json}"
        );
    }

    #[test]
    fn test_stats_model_breakdown_sorted_by_cost() {
        let today = Local::now().date_naive();
        let groups = vec![make_daily_group(
            today,
            vec![
                make_session_with_tokens(
                    "~/projects/app",
                    100_000,
                    50_000,
                    "claude-opus-4-5-20251101",
                ),
                make_session_with_tokens(
                    "~/projects/app",
                    100_000,
                    50_000,
                    "claude-sonnet-4-20250514",
                ),
            ],
        )];
        let server = CcsightServer::from_groups(groups);
        let json = call_stats(&server, None);

        let breakdown = json["model_breakdown"].as_array().unwrap();
        assert!(breakdown.len() >= 2);
        let first_cost = breakdown[0]["cost_usd"].as_f64().unwrap();
        let second_cost = breakdown[1]["cost_usd"].as_f64().unwrap();
        assert!(first_cost >= second_cost);
    }

    #[test]
    fn test_stats_hourly_work_tokens() {
        let today = Local::now().date_naive();
        let mut session =
            make_session_with_tokens("~/projects/app", 1000, 500, "claude-sonnet-4-20250514");
        session.day_hourly_work_tokens.insert(10, 5000);
        session.day_hourly_work_tokens.insert(14, 3000);

        let server = CcsightServer::from_groups(vec![make_daily_group(today, vec![session])]);
        let json = call_stats(&server, None);

        let hourly = json["hourly_pattern"].as_object().unwrap();
        assert_eq!(hourly["10"], 5000);
        assert_eq!(hourly["14"], 3000);
    }

    #[test]
    fn test_stats_date_range_for_all() {
        let server = CcsightServer::from_groups(vec![]);
        let json = call_stats(&server, Some("all"));
        assert!(json["date_range"].is_null());
    }

    #[test]
    fn test_stats_date_range_for_today() {
        let server = CcsightServer::from_groups(vec![]);
        let json = call_stats(&server, Some("today"));
        let today = Local::now().date_naive().to_string();
        assert_eq!(json["date_range"]["start"], today);
        assert_eq!(json["date_range"]["end"], today);
    }

    #[test]
    fn test_stats_tokens_total_work_equals_input_plus_output() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_stats(&server, None);

        let input = json["tokens"]["input"].as_u64().unwrap();
        let output = json["tokens"]["output"].as_u64().unwrap();
        let total_work = json["tokens"]["total_work"].as_u64().unwrap();
        assert_eq!(total_work, input + output);
    }

    #[test]
    fn test_sessions_no_filter() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_sessions(&server, None, None, None, None);

        assert_eq!(json["count"], 4);
        assert_eq!(json["sessions"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn test_sessions_project_filter() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_sessions(&server, None, Some("app-a"), None, None);

        assert_eq!(json["count"], 2);
        for s in json["sessions"].as_array().unwrap() {
            assert!(s["project"].as_str().unwrap().contains("app-a"));
        }
    }

    #[test]
    fn test_sessions_date_filter() {
        let today = Local::now().date_naive();
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_sessions(&server, None, None, Some(&today.to_string()), None);

        assert_eq!(json["count"], 2);
        for s in json["sessions"].as_array().unwrap() {
            assert_eq!(s["date"].as_str().unwrap(), today.to_string());
        }
    }

    #[test]
    fn test_sessions_query_filter() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_sessions(&server, Some("login"), None, None, None);

        assert_eq!(json["count"], 1);
        assert_eq!(json["sessions"][0]["summary"], "Fix login bug");
    }

    #[test]
    fn test_sessions_limit() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_sessions(&server, None, None, None, Some(2));
        assert_eq!(json["count"], 2);
    }

    #[test]
    fn test_sessions_excludes_subagents() {
        let today = Local::now().date_naive();
        let mut subagent =
            make_session_with_tokens("~/projects/app", 1000, 500, "claude-sonnet-4-20250514");
        subagent.is_subagent = true;

        let groups = vec![make_daily_group(
            today,
            vec![
                make_session_with_tokens("~/projects/app", 2000, 1000, "claude-sonnet-4-20250514"),
                subagent,
            ],
        )];
        let server = CcsightServer::from_groups(groups);
        let json = call_sessions(&server, None, None, None, None);
        assert_eq!(json["count"], 1);
    }

    #[test]
    fn test_sessions_empty_groups() {
        let server = CcsightServer::from_groups(vec![]);
        let json = call_sessions(&server, None, None, None, None);

        assert_eq!(json["count"], 0);
        assert!(json["sessions"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_sessions_no_match_query() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_sessions(&server, Some("nonexistent-xyz-123"), None, None, None);
        assert_eq!(json["count"], 0);
    }

    #[test]
    fn test_sessions_project_case_insensitive() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_sessions(&server, None, Some("APP-A"), None, None);
        assert_eq!(json["count"], 2);
    }

    #[test]
    fn test_sessions_query_excludes_subagents() {
        let today = Local::now().date_naive();
        let mut subagent = make_session("~/projects/app", Some("Fix login bug"), Some("main"));
        subagent.is_subagent = true;
        let normal = make_session("~/projects/app", Some("Fix login issue"), Some("main"));

        let groups = vec![make_daily_group(today, vec![normal, subagent])];
        let server = CcsightServer::from_groups(groups);
        let json = call_sessions(&server, Some("login"), None, None, None);

        assert_eq!(json["count"], 1, "query results should exclude subagents");
    }

    #[test]
    fn test_sessions_query_with_project_filter() {
        let today = Local::now().date_naive();
        let groups = vec![make_daily_group(
            today,
            vec![
                make_session("~/projects/app-a", Some("Fix login bug"), None),
                make_session("~/projects/app-b", Some("Fix login issue"), None),
            ],
        )];
        let server = CcsightServer::from_groups(groups);
        let json = call_sessions(&server, Some("login"), Some("app-a"), None, None);

        assert_eq!(json["count"], 1);
        assert_eq!(json["sessions"][0]["project"], "~/projects/app-a");
    }

    #[test]
    fn test_sessions_query_with_date_filter() {
        let today = Local::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let groups = vec![
            make_daily_group(
                today,
                vec![make_session(
                    "~/projects/app",
                    Some("Fix login today"),
                    None,
                )],
            ),
            make_daily_group(
                yesterday,
                vec![make_session(
                    "~/projects/app",
                    Some("Fix login yesterday"),
                    None,
                )],
            ),
        ];
        let server = CcsightServer::from_groups(groups);
        let json = call_sessions(&server, Some("login"), None, Some(&today.to_string()), None);

        assert_eq!(json["count"], 1);
        assert_eq!(json["sessions"][0]["date"], today.to_string());
    }

    #[test]
    fn test_sessions_query_with_limit() {
        let today = Local::now().date_naive();
        let groups = vec![make_daily_group(
            today,
            vec![
                make_session("~/projects/app", Some("Fix bug one"), None),
                make_session("~/projects/app", Some("Fix bug two"), None),
                make_session("~/projects/app", Some("Fix bug three"), None),
            ],
        )];
        let server = CcsightServer::from_groups(groups);
        let json = call_sessions(&server, Some("bug"), None, None, Some(2));

        assert_eq!(json["count"], 2);
    }

    #[test]
    fn test_server_info() {
        let server = CcsightServer::from_groups(vec![]);
        let info = server.get_info();

        assert_eq!(info.server_info.name, "ccsight");
        assert!(info.instructions.is_some());
        assert!(info.instructions.unwrap().contains("stats"));
    }

    #[test]
    fn test_session_detail_not_found() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_session_detail(&server, "nonexist", None);
        assert_eq!(json["error"], "Session not found");
    }

    #[test]
    fn test_session_detail_found_by_id() {
        let today = Local::now().date_naive();
        let mut session =
            make_session_with_tokens("~/projects/app", 5000, 2000, "claude-sonnet-4-20250514");
        session.file_path = std::path::PathBuf::from("/tmp/abcd1234-rest-of-id.jsonl");
        session.summary = Some("Test summary".to_string());
        session.day_tool_usage.insert("Read".to_string(), 5);
        session.day_tool_usage.insert("Edit".to_string(), 3);

        let groups = vec![make_daily_group(today, vec![session])];
        let server = CcsightServer::from_groups(groups);
        let json = call_session_detail(&server, "abcd1234", None);

        assert!(json["error"].is_null(), "should find the session");
        assert_eq!(json["session"]["session_id"], "abcd1234");
        assert_eq!(json["session"]["project"], "~/projects/app");
        assert_eq!(json["tool_usage"]["Read"], 5);
        assert_eq!(json["tool_usage"]["Edit"], 3);
    }

    #[test]
    fn test_session_detail_with_date_filter() {
        let today = Local::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let mut s1 =
            make_session_with_tokens("~/projects/a", 1000, 500, "claude-sonnet-4-20250514");
        s1.file_path = std::path::PathBuf::from("/tmp/sameid12-today.jsonl");
        let mut s2 =
            make_session_with_tokens("~/projects/b", 2000, 1000, "claude-sonnet-4-20250514");
        s2.file_path = std::path::PathBuf::from("/tmp/sameid12-yesterday.jsonl");

        let groups = vec![
            make_daily_group(today, vec![s1]),
            make_daily_group(yesterday, vec![s2]),
        ];
        let server = CcsightServer::from_groups(groups);

        let json = call_session_detail(&server, "sameid12", Some(&today.to_string()));
        assert_eq!(json["session"]["project"], "~/projects/a");

        let json2 = call_session_detail(&server, "sameid12", Some(&yesterday.to_string()));
        assert_eq!(json2["session"]["project"], "~/projects/b");
    }

    #[test]
    fn test_extract_conversation_basic() {
        let messages = vec![
            ConversationMessage {
                role: "user".to_string(),
                blocks: vec![ConversationBlock::Text(
                    "Please fix the login bug".to_string(),
                )],
                timestamp: Some("10:00:00".to_string()),
                model: None,
                tokens: None,
            },
            ConversationMessage {
                role: "assistant".to_string(),
                blocks: vec![ConversationBlock::Text("I'll fix that.".to_string())],
                timestamp: Some("10:00:05".to_string()),
                model: Some("claude-sonnet-4".to_string()),
                tokens: Some((1000, 500)),
            },
            ConversationMessage {
                role: "user".to_string(),
                blocks: vec![ConversationBlock::Text("Now add tests".to_string())],
                timestamp: Some("10:05:00".to_string()),
                model: None,
                tokens: None,
            },
        ];
        let conv = extract_conversation(&messages);
        assert_eq!(conv.len(), 3);
        assert_eq!(conv[0]["role"], "user");
        assert!(conv[0]["text"].as_str().unwrap().contains("login bug"));
        assert_eq!(conv[1]["role"], "assistant");
        assert!(conv[1]["text"].as_str().unwrap().contains("fix that"));
        assert_eq!(conv[2]["role"], "user");
        assert!(conv[2]["text"].as_str().unwrap().contains("add tests"));
    }

    #[test]
    fn test_extract_conversation_includes_assistant() {
        let messages = vec![ConversationMessage {
            role: "assistant".to_string(),
            blocks: vec![ConversationBlock::Text("response text".to_string())],
            timestamp: None,
            model: None,
            tokens: None,
        }];
        let conv = extract_conversation(&messages);
        assert_eq!(conv.len(), 1);
        assert_eq!(conv[0]["role"], "assistant");
    }

    #[test]
    fn test_extract_conversation_skips_tool_only_assistant() {
        let messages = vec![ConversationMessage {
            role: "assistant".to_string(),
            blocks: vec![ConversationBlock::ToolUse {
                name: "Read".to_string(),
                input_summary: "/tmp/x".to_string(),
            }],
            timestamp: None,
            model: None,
            tokens: None,
        }];
        let conv = extract_conversation(&messages);
        assert!(
            conv.is_empty(),
            "tool-only assistant messages should be skipped"
        );
    }

    #[test]
    fn test_extract_conversation_truncates_long_text() {
        let long_text = "a".repeat(500);
        let messages = vec![ConversationMessage {
            role: "user".to_string(),
            blocks: vec![ConversationBlock::Text(long_text)],
            timestamp: None,
            model: None,
            tokens: None,
        }];
        let conv = extract_conversation(&messages);
        let text = conv[0]["text"].as_str().unwrap();
        assert!(text.len() <= 304);
        assert!(text.ends_with("..."));
    }

    #[test]
    fn test_extract_conversation_skips_empty() {
        let messages = vec![ConversationMessage {
            role: "user".to_string(),
            blocks: vec![ConversationBlock::ToolUse {
                name: "Read".to_string(),
                input_summary: "/tmp/x".to_string(),
            }],
            timestamp: None,
            model: None,
            tokens: None,
        }];
        let conv = extract_conversation(&messages);
        assert!(conv.is_empty());
    }

    #[test]
    fn test_extract_files_touched() {
        let messages = vec![ConversationMessage {
            role: "assistant".to_string(),
            blocks: vec![
                ConversationBlock::ToolUse {
                    name: "Read".to_string(),
                    input_summary: "/src/main.rs".to_string(),
                },
                ConversationBlock::ToolUse {
                    name: "Edit".to_string(),
                    input_summary: "/src/lib.rs".to_string(),
                },
                ConversationBlock::ToolUse {
                    name: "Read".to_string(),
                    input_summary: "/src/main.rs".to_string(),
                },
                ConversationBlock::ToolUse {
                    name: "Bash".to_string(),
                    input_summary: "cargo test".to_string(),
                },
            ],
            timestamp: None,
            model: None,
            tokens: None,
        }];
        let files = extract_files_touched(&messages);
        assert_eq!(files, vec!["/src/lib.rs", "/src/main.rs"]);
    }

    #[test]
    fn test_extract_files_touched_empty() {
        let messages = vec![ConversationMessage {
            role: "assistant".to_string(),
            blocks: vec![ConversationBlock::Text("hello".to_string())],
            timestamp: None,
            model: None,
            tokens: None,
        }];
        let files = extract_files_touched(&messages);
        assert!(files.is_empty());
    }

    #[test]
    fn test_stats_custom_date_range_single_day() {
        let today = Local::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let groups = vec![
            make_daily_group(
                today,
                vec![make_session_with_tokens(
                    "~/projects/a",
                    1000,
                    500,
                    "claude-sonnet-4-20250514",
                )],
            ),
            make_daily_group(
                yesterday,
                vec![make_session_with_tokens(
                    "~/projects/b",
                    2000,
                    1000,
                    "claude-sonnet-4-20250514",
                )],
            ),
        ];
        let server = CcsightServer::from_groups(groups);
        let json = call_stats_range(
            &server,
            &yesterday.to_string(),
            Some(&yesterday.to_string()),
        );

        assert_eq!(json["period"], "custom");
        assert_eq!(json["total_sessions"], 1);
        assert_eq!(json["tokens"]["input"], 2000);
    }

    #[test]
    fn test_stats_custom_date_range_overrides_period() {
        let today = Local::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let groups = vec![
            make_daily_group(
                today,
                vec![make_session_with_tokens(
                    "~/projects/a",
                    1000,
                    500,
                    "claude-sonnet-4-20250514",
                )],
            ),
            make_daily_group(
                yesterday,
                vec![make_session_with_tokens(
                    "~/projects/b",
                    2000,
                    1000,
                    "claude-sonnet-4-20250514",
                )],
            ),
        ];
        let server = CcsightServer::from_groups(groups);

        let params = StatsParams {
            period: Some("today".to_string()),
            date_from: Some(yesterday.to_string()),
            date_to: None,
            group_by: None,
        };
        let json = extract_json(&server.stats(Parameters(params)).unwrap());

        assert_eq!(json["period"], "custom");
        assert_eq!(
            json["total_sessions"], 2,
            "date_from should override period"
        );
    }

    #[test]
    fn test_stats_custom_date_from_only() {
        let today = Local::now().date_naive();
        let old = today - chrono::Duration::days(30);
        let groups = vec![
            make_daily_group(
                today,
                vec![make_session_with_tokens(
                    "~/projects/a",
                    1000,
                    500,
                    "claude-sonnet-4-20250514",
                )],
            ),
            make_daily_group(
                old,
                vec![make_session_with_tokens(
                    "~/projects/b",
                    2000,
                    1000,
                    "claude-sonnet-4-20250514",
                )],
            ),
        ];
        let server = CcsightServer::from_groups(groups);
        let json = call_stats_range(&server, &today.to_string(), None);

        assert_eq!(json["total_sessions"], 1, "only today should be included");
    }

    #[test]
    fn test_derive_summary_from_file() {
        let tmp = TempFile::new("ccsight_derive_summary.jsonl");
        tmp.write_jsonl(&[serde_json::json!({
            "type": "user",
            "timestamp": "2026-03-20T10:00:00Z",
            "message": {"role": "user", "content": "Add MCP server support to the project"}
        })]);
        assert_eq!(
            derive_summary(tmp.path()).as_deref(),
            Some("Add MCP server support to the project")
        );
    }

    #[test]
    fn test_derive_summary_skips_system_tags() {
        let tmp = TempFile::new("ccsight_derive_skip_tags.jsonl");
        tmp.write_jsonl(&[
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-03-20T10:00:00Z",
                "message": {"role": "user", "content": "<local-command-caveat>system noise</local-command-caveat>"}
            }),
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-03-20T10:01:00Z",
                "message": {"role": "user", "content": "Fix the login bug"}
            }),
        ]);
        assert_eq!(
            derive_summary(tmp.path()).as_deref(),
            Some("Fix the login bug")
        );
    }

    #[test]
    fn test_derive_summary_no_user_message() {
        let tmp = TempFile::new("ccsight_derive_no_user.jsonl");
        tmp.write_jsonl(&[serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-03-20T10:00:00Z",
            "message": {"role": "assistant", "content": "Hello"}
        })]);
        assert!(derive_summary(tmp.path()).is_none());
    }

    #[test]
    fn test_derive_summary_truncates_long() {
        let tmp = TempFile::new("ccsight_derive_long.jsonl");
        let long = "x".repeat(300);
        tmp.write_jsonl(&[serde_json::json!({
            "type": "user",
            "timestamp": "2026-03-20T10:00:00Z",
            "message": {"role": "user", "content": long}
        })]);
        let result = derive_summary(tmp.path()).unwrap();
        assert!(result.len() <= 124);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_sessions_date_from_to() {
        let today = Local::now().date_naive();
        let d1 = today - chrono::Duration::days(3);
        let d2 = today - chrono::Duration::days(2);
        let d3 = today - chrono::Duration::days(1);
        let groups = vec![
            make_daily_group(
                d1,
                vec![make_session_with_tokens(
                    "~/projects/a",
                    1000,
                    500,
                    "claude-sonnet-4-20250514",
                )],
            ),
            make_daily_group(
                d2,
                vec![make_session_with_tokens(
                    "~/projects/b",
                    2000,
                    1000,
                    "claude-sonnet-4-20250514",
                )],
            ),
            make_daily_group(
                d3,
                vec![make_session_with_tokens(
                    "~/projects/c",
                    3000,
                    1500,
                    "claude-sonnet-4-20250514",
                )],
            ),
            make_daily_group(
                today,
                vec![make_session_with_tokens(
                    "~/projects/d",
                    4000,
                    2000,
                    "claude-sonnet-4-20250514",
                )],
            ),
        ];
        let server = CcsightServer::from_groups(groups);
        let params = SessionsParams {
            session_id: None,
            query: None,
            project: None,
            date: None,
            date_from: Some(d2.to_string()),
            date_to: Some(d3.to_string()),
            sort: None,
            limit: None,
            conversation_limit: None,
            conversation_offset: None,
            conversation_query: None,
            pinned: None,
        };
        let json = extract_json(&server.sessions(Parameters(params)).unwrap());
        assert_eq!(json["count"], 2);
    }

    #[test]
    fn test_sessions_sort_by_cost() {
        let today = Local::now().date_naive();
        let groups = vec![make_daily_group(
            today,
            vec![
                make_session_with_tokens("~/projects/cheap", 1000, 500, "claude-sonnet-4-20250514"),
                make_session_with_tokens(
                    "~/projects/expensive",
                    100_000,
                    50_000,
                    "claude-opus-4-5-20251101",
                ),
            ],
        )];
        let server = CcsightServer::from_groups(groups);
        let params = SessionsParams {
            session_id: None,
            query: None,
            project: None,
            date: None,
            date_from: None,
            date_to: None,
            sort: Some("cost".to_string()),
            limit: None,
            conversation_limit: None,
            conversation_offset: None,
            conversation_query: None,
            pinned: None,
        };
        let json = extract_json(&server.sessions(Parameters(params)).unwrap());
        let sessions = json["sessions"].as_array().unwrap();
        assert!(
            sessions[0]["cost_usd"].as_f64().unwrap() > sessions[1]["cost_usd"].as_f64().unwrap()
        );
    }

    #[test]
    fn test_sessions_sort_by_tokens() {
        let today = Local::now().date_naive();
        let groups = vec![make_daily_group(
            today,
            vec![
                make_session_with_tokens("~/projects/small", 1000, 500, "claude-sonnet-4-20250514"),
                make_session_with_tokens(
                    "~/projects/large",
                    100_000,
                    50_000,
                    "claude-sonnet-4-20250514",
                ),
            ],
        )];
        let server = CcsightServer::from_groups(groups);
        let params = SessionsParams {
            session_id: None,
            query: None,
            project: None,
            date: None,
            date_from: None,
            date_to: None,
            sort: Some("tokens".to_string()),
            limit: None,
            conversation_limit: None,
            conversation_offset: None,
            conversation_query: None,
            pinned: None,
        };
        let json = extract_json(&server.sessions(Parameters(params)).unwrap());
        let sessions = json["sessions"].as_array().unwrap();
        let t0 = sessions[0]["tokens"]["input"].as_u64().unwrap()
            + sessions[0]["tokens"]["output"].as_u64().unwrap();
        let t1 = sessions[1]["tokens"]["input"].as_u64().unwrap()
            + sessions[1]["tokens"]["output"].as_u64().unwrap();
        assert!(t0 > t1);
    }

    #[test]
    fn test_session_detail_conversation_truncated() {
        let today = Local::now().date_naive();
        let mut session = make_session("~/projects/app", Some("Many messages"), None);

        let tmp = TempFile::new("ccsight_truncation.jsonl");
        let entries: Vec<serde_json::Value> = (0..30)
            .map(|i| {
                serde_json::json!({
                    "type": "user",
                    "timestamp": format!("2026-03-20T10:{i:02}:00Z"),
                    "message": {"role": "user", "content": format!("request {i}")}
                })
            })
            .collect();
        tmp.write_jsonl(&entries);
        session.file_path = tmp.path().to_path_buf();

        let groups = vec![make_daily_group(today, vec![session])];
        let server = CcsightServer::from_groups(groups);

        let params = SessionsParams {
            session_id: Some("ccsight_t".to_string()),
            query: None,
            project: None,
            date: None,
            date_from: None,
            date_to: None,
            sort: None,
            limit: None,
            conversation_limit: Some(15),
            conversation_offset: None,
            conversation_query: None,
            pinned: None,
        };
        let json = extract_json(&server.sessions(Parameters(params)).unwrap());

        assert_eq!(json["conversation_total"], 30);
        let conv = json["conversation"].as_array().unwrap();
        assert!(conv.len() <= 16, "should be head + omission marker + tail");
        assert_eq!(conv[0]["text"].as_str().unwrap(), "request 0");
        assert!(
            conv.iter().any(|c| c["role"] == "..."),
            "should have omission marker"
        );
        let last = conv.last().unwrap();
        assert_eq!(last["text"].as_str().unwrap(), "request 29");
    }

    #[test]
    fn test_query_searches_derived_summary() {
        let today = Local::now().date_naive();
        let mut session = make_session("~/projects/app", None, None);
        session.summary = None;

        let tmp = TempFile::new("ccsight_query_summary.jsonl");
        tmp.write_jsonl(&[serde_json::json!({
            "type": "user",
            "timestamp": "2026-03-20T10:00:00Z",
            "message": {"role": "user", "content": "Fix the pagination bug in user list"}
        })]);
        session.file_path = tmp.path().to_path_buf();

        let groups = vec![make_daily_group(today, vec![session])];
        let server = CcsightServer::from_groups(groups);
        let json = call_sessions(&server, Some("pagination"), None, None, None);

        assert_eq!(json["count"], 1, "query should match derived summary");
    }

    #[test]
    fn test_clean_summary_extracts_plan_title() {
        let text = "Implement the following plan:\n\n# Fix user authentication flow\n\n## Context\nsome details";
        assert_eq!(clean_summary(text), "Fix user authentication flow");
    }

    #[test]
    fn test_clean_summary_plain_text() {
        assert_eq!(clean_summary("Fix the login bug"), "Fix the login bug");
    }

    #[test]
    fn test_clean_summary_truncates() {
        let long = "x".repeat(200);
        let result = clean_summary(&long);
        assert!(result.len() <= 124);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_stats_group_by_day() {
        let today = Local::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let groups = vec![
            make_daily_group(
                today,
                vec![
                    make_session_with_tokens("~/projects/a", 1000, 500, "claude-sonnet-4-20250514"),
                    make_session_with_tokens(
                        "~/projects/b",
                        2000,
                        1000,
                        "claude-sonnet-4-20250514",
                    ),
                ],
            ),
            make_daily_group(
                yesterday,
                vec![make_session_with_tokens(
                    "~/projects/c",
                    3000,
                    1500,
                    "claude-sonnet-4-20250514",
                )],
            ),
        ];
        let server = CcsightServer::from_groups(groups);
        let params = StatsParams {
            period: None,
            date_from: None,
            date_to: None,
            group_by: Some("day".to_string()),
        };
        let json = extract_json(&server.stats(Parameters(params)).unwrap());

        let daily = json["daily"].as_array().unwrap();
        assert_eq!(daily.len(), 2);
        assert_eq!(daily[0]["date"].as_str().unwrap(), yesterday.to_string());
        assert_eq!(daily[1]["date"].as_str().unwrap(), today.to_string());
        assert_eq!(daily[1]["sessions"], 2);
        assert!(daily[1]["cost_usd"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn test_stats_without_group_by_has_no_daily() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_stats(&server, None);
        assert!(json.get("daily").is_none());
    }

    #[test]
    fn test_sessions_query_matches_metadata_only() {
        let today = Local::now().date_naive();
        let session = make_session(
            "~/projects/app",
            Some("Setup database connection pool"),
            None,
        );
        let groups = vec![make_daily_group(today, vec![session])];
        let server = CcsightServer::from_groups(groups);

        let json = call_sessions(&server, Some("connection pool"), None, None, None);
        assert_eq!(json["count"], 1, "should find session by summary metadata");

        let json2 = call_sessions(&server, Some("nonexistent_xyz"), None, None, None);
        assert_eq!(
            json2["count"], 0,
            "should not match content that is only in conversation"
        );
    }
}
