pub(crate) mod buckets;
pub(crate) mod grouping;
pub(crate) mod language;
pub(crate) mod pricing;
pub(crate) mod stats;
pub(crate) mod tool_category;

pub(crate) use buckets::{
    aggregate_monthly_costs, aggregate_monthly_tokens, aggregate_weekday_avg,
};

pub use grouping::*;
pub use pricing::*;
pub use stats::{CacheStats, Stats, StatsAggregator, TokenStats};
pub use tool_category::*;

pub(crate) fn extract_project_name(entries: &[crate::domain::LogEntry]) -> Option<String> {
    // Use the FIRST cwd — sessions normally start in a stable project root
    // and may `cd` into subdirectories during the conversation (e.g.
    // `dev/tmp` → `dev/tmp/.docs` to read internal docs). Picking the latest
    // cwd would re-bucket those into transient subdirectory "projects",
    // fracturing one logical project into many. The canonical project
    // identity is set at session start.
    //
    // Note: this looks like a violation of the
    // session-representative-value rev() rule (used for `model`,
    // `git_branch`, `custom_title`), but `cwd` is different: it's a
    // navigation breadcrumb, not an explicit user choice to redefine the
    // session's identity.
    entries
        .iter()
        .find_map(|e| e.cwd.as_ref())
        .map(|cwd| format_project_path(cwd))
}

/// Last-used timestamp per `tool_usage` key (raw form: `Bash`, `skill:my-skill`,
/// `agent:type-a`, `mcp__server__action`, …).
///
/// Walks every session in `daily_groups`; for each tool key seen in `day_tool_usage`,
/// keeps the maximum `day_last_timestamp`. **MCP-server tools** skip subagent sessions
/// (mirroring `infrastructure::compute_mcp_status`, where double-counting nested
/// subagent activity bloats per-server stats). **Built-in / Skill / Subagent-meta**
/// tools include subagents — otherwise tools used heavily inside subagents (e.g.
/// `TodoWrite`) appear stale even when active recently in nested sessions.
pub fn compute_tool_last_used(
    daily_groups: &[DailyGroup],
) -> std::collections::HashMap<String, chrono::DateTime<chrono::Utc>> {
    let mut map: std::collections::HashMap<String, chrono::DateTime<chrono::Utc>> =
        std::collections::HashMap::new();
    for group in daily_groups {
        for session in &group.sessions {
            let ts = session.day_last_timestamp;
            for tool in session.day_tool_usage.keys() {
                let skip_for_subagent = session.is_subagent
                    && matches!(classify_tool(tool), ToolCategory::Mcp { .. });
                if skip_for_subagent {
                    continue;
                }
                let entry = map.entry(tool.clone()).or_insert(ts);
                if ts > *entry {
                    *entry = ts;
                }
            }
        }
    }
    map
}

/// Returns true if a model name is a real model (not synthetic placeholder).
pub(crate) fn is_real_model(model: &str) -> bool {
    !model.is_empty() && model != "<synthetic>"
}

/// Returns the most recent real (non-synthetic) model name used in the session.
/// Walks entries in reverse so that mid-session model switches (e.g. `/model opus` after
/// starting on Sonnet) are reflected in the displayed badge — the latest model wins.
pub(crate) fn extract_session_model(entries: &[crate::domain::LogEntry]) -> Option<String> {
    entries
        .iter()
        .rev()
        .find_map(|e| {
            let m = e.message.as_ref()?.model.as_ref()?;
            is_real_model(m).then(|| m.clone())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{EntryType, LogEntry, Message, Role};

    fn assistant_entry(model: Option<&str>) -> LogEntry {
        LogEntry {
            uuid: None,
            parent_uuid: None,
            session_id: None,
            timestamp: None,
            entry_type: EntryType::Assistant,
            message: Some(Message {
                role: Role::Assistant,
                content: Default::default(),
                usage: None,
                model: model.map(|m| m.to_string()),
                id: None,
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
    fn test_extract_session_model_returns_last_real_model() {
        // Mid-session model switch (Sonnet → Opus): the badge should reflect the latest
        // model, not the first one. Walking entries in reverse picks the most recent
        // assistant message that carries a real model name.
        let entries = vec![
            assistant_entry(Some("claude-sonnet-4-20250514")),
            assistant_entry(Some("claude-sonnet-4-20250514")),
            assistant_entry(Some("claude-opus-4-5-20251101")),
        ];
        assert_eq!(
            extract_session_model(&entries).as_deref(),
            Some("claude-opus-4-5-20251101"),
        );
    }

    #[test]
    fn test_extract_session_model_skips_synthetic_at_tail() {
        // <synthetic> placeholders at the end (e.g. tool-only completions) must be
        // ignored so the badge falls back to the most recent real model used.
        let entries = vec![
            assistant_entry(Some("claude-sonnet-4-20250514")),
            assistant_entry(Some("claude-opus-4-5-20251101")),
            assistant_entry(Some("<synthetic>")),
        ];
        assert_eq!(
            extract_session_model(&entries).as_deref(),
            Some("claude-opus-4-5-20251101"),
        );
    }

    use crate::test_helpers::helpers::{make_daily_group, make_session};
    use chrono::{DateTime, NaiveDate, TimeZone, Utc};

    fn ts(d: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap() + chrono::Duration::days(d)
    }

    fn group_with(date: NaiveDate, sessions: Vec<(DateTime<Utc>, &[&str], bool)>) -> DailyGroup {
        let infos: Vec<_> = sessions
            .into_iter()
            .map(|(last, tools, is_subagent)| {
                let mut s = make_session("p", None, None);
                s.day_last_timestamp = last;
                s.is_subagent = is_subagent;
                for t in tools {
                    s.day_tool_usage.insert((*t).to_string(), 1);
                }
                s
            })
            .collect();
        make_daily_group(date, infos)
    }

    #[test]
    fn test_compute_tool_last_used_picks_max_per_tool() {
        let date = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let groups = vec![group_with(
            date,
            vec![
                (ts(0), &["Bash", "Read"], false),
                (ts(2), &["Bash"], false),
                (ts(1), &["skill:my-skill"], false),
            ],
        )];
        let map = compute_tool_last_used(&groups);
        assert_eq!(map.get("Bash").copied(), Some(ts(2)));
        assert_eq!(map.get("Read").copied(), Some(ts(0)));
        assert_eq!(map.get("skill:my-skill").copied(), Some(ts(1)));
    }

    #[test]
    fn test_compute_tool_last_used_includes_subagents_for_builtin() {
        // Built-in tools (Bash, Read, TodoWrite, …) used inside subagent sessions
        // count toward last_used — otherwise tools heavily used by Claude Code's
        // general-purpose subagents look stale even when active "today".
        let date = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let groups = vec![group_with(
            date,
            vec![
                (ts(0), &["Bash"], false),
                (ts(5), &["Bash"], true), // subagent run; must contribute
            ],
        )];
        let map = compute_tool_last_used(&groups);
        assert_eq!(map.get("Bash").copied(), Some(ts(5)));
    }

    #[test]
    fn test_compute_tool_last_used_skips_subagents_for_mcp() {
        // MCP servers stay subagent-excluded to avoid the double-count problem
        // (a parent session already attributes the nested MCP call once).
        let date = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let groups = vec![group_with(
            date,
            vec![
                (ts(0), &["mcp__server1__action"], false),
                (ts(5), &["mcp__server1__action"], true), // subagent — must be ignored
            ],
        )];
        let map = compute_tool_last_used(&groups);
        assert_eq!(map.get("mcp__server1__action").copied(), Some(ts(0)));
    }
}

pub(crate) fn format_project_path(path: &str) -> String {
    let stripped = path
        .strip_prefix("/Users/")
        .or_else(|| path.strip_prefix("/home/"));
    if let Some(stripped) = stripped {
        if let Some(rest) = stripped.split_once('/') {
            format!("~/{}", rest.1)
        } else {
            format!("~/{stripped}")
        }
    } else {
        path.to_string()
    }
}
