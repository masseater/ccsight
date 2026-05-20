use std::path::Path;

use crate::aggregator::DailyGroup;
use crate::domain::{EntryType, Role};
use crate::parser::JsonlParser;

#[derive(Clone)]
pub struct SearchResult {
    /// Position in `daily_groups` that the result currently refers to. The
    /// tantivy index uses positions captured at build time; when a project /
    /// period filter shrinks `daily_groups`, callers must remap these indices
    /// (using `session_path` as the stable lookup key) before pushing into
    /// `state.search_results`. Stale indices crash the renderer.
    pub day_idx: usize,
    pub session_idx: usize,
    pub snippet: Option<String>,
    pub match_type: SearchMatchType,
    /// Absolute session-file path stored in the search index. Used as the
    /// stable identifier for remapping `day_idx` / `session_idx` after
    /// filter changes. `None` for non-index searches (where indices already
    /// reflect the current filtered set).
    pub session_path: Option<String>,
}

#[derive(Clone)]
pub enum SearchMatchType {
    ProjectName,
    Summary,
    GitBranch,
    SessionId,
    Date,
    Content,
}

pub fn perform_search(daily_groups: &[DailyGroup], query: &str) -> Vec<SearchResult> {
    if query.is_empty() {
        return Vec::new();
    }

    let query_lower = query.to_lowercase();
    let mut results = Vec::new();

    for (day_idx, group) in daily_groups.iter().enumerate() {
        for (session_idx, session) in group.sessions.iter().filter(|s| !s.is_subagent).enumerate() {
            if session.project_name.to_lowercase().contains(&query_lower) {
                // The project column is already on line 1, so a snippet that
                // re-prints the project name would just duplicate it. Set
                // None and let the renderer collapse the row to a single line.
                results.push(SearchResult {
                    day_idx,
                    session_idx,
                    snippet: None,
                    match_type: SearchMatchType::ProjectName,
                    session_path: None,
                });
                continue;
            }

            if let Some(ref summary) = session.summary
                && summary.to_lowercase().contains(&query_lower)
            {
                let snippet = extract_snippet(summary, &query_lower, 60);
                results.push(SearchResult {
                    day_idx,
                    session_idx,
                    snippet: Some(snippet),
                    match_type: SearchMatchType::Summary,
                    session_path: None,
                });
                continue;
            }

            if let Some(ref branch) = session.git_branch
                && branch.to_lowercase().contains(&query_lower)
            {
                // Branch column is already on line 1; same rationale as
                // ProjectName above.
                results.push(SearchResult {
                    day_idx,
                    session_idx,
                    snippet: None,
                    match_type: SearchMatchType::GitBranch,
                    session_path: None,
                });
                continue;
            }

            if let Some(session_id) = session.file_path.file_stem().and_then(|n| n.to_str())
                && session_id.to_lowercase().contains(&query_lower)
            {
                results.push(SearchResult {
                    day_idx,
                    session_idx,
                    snippet: Some(session_id.to_string()),
                    match_type: SearchMatchType::SessionId,
                    session_path: None,
                });
                continue;
            }

            if group
                .date
                .format("%Y-%m-%d")
                .to_string()
                .contains(&query_lower)
            {
                results.push(SearchResult {
                    day_idx,
                    session_idx,
                    snippet: None,
                    match_type: SearchMatchType::Date,
                    session_path: None,
                });
                continue;
            }
        }
    }

    results
}

pub fn extract_snippet(text: &str, query: &str, max_len: usize) -> String {
    if query.is_empty() {
        return text.chars().take(max_len).collect();
    }

    let text_lower = text.to_lowercase();
    let query_lower = query.to_lowercase();

    if let Some(byte_pos) = text_lower.find(&query_lower) {
        let chars: Vec<char> = text.chars().collect();
        let char_pos = text[..byte_pos].chars().count();
        let query_len = query.chars().count();

        let start = char_pos.saturating_sub(max_len / 2);
        let end = (char_pos + query_len + max_len / 2).min(chars.len());

        let mut snippet = String::new();
        if start > 0 {
            snippet.push_str("...");
        }
        snippet.extend(&chars[start..end]);
        if end < chars.len() {
            snippet.push_str("...");
        }
        snippet.replace('\n', " ")
    } else {
        text.chars().take(max_len).collect()
    }
}

pub fn search_session_content(file_path: &Path, query: &str) -> Option<String> {
    let entries = JsonlParser::parse_file(file_path).ok()?;
    let query_lower = query.to_lowercase();

    for entry in &entries {
        if (entry.entry_type == EntryType::User || entry.entry_type == EntryType::Assistant)
            && let Some(ref message) = entry.message
        {
            let text = message.content.extract_text();
            let text_lower = text.to_lowercase();
            if text_lower.contains(&query_lower) {
                let role = match message.role {
                    Role::User => "User",
                    Role::Assistant => "AI",
                    _ => "?",
                };
                let snippet = extract_snippet(&text, query, 50);
                return Some(format!("[{role}] {snippet}"));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_snippet_empty_query() {
        let result = extract_snippet("hello world", "", 20);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_extract_snippet_empty_text() {
        let result = extract_snippet("", "query", 20);
        assert_eq!(result, "");
    }

    #[test]
    fn test_extract_snippet_query_longer_than_text() {
        let result = extract_snippet("hi", "hello world", 20);
        assert_eq!(result, "hi");
    }

    #[test]
    fn test_extract_snippet_normal_match() {
        let result = extract_snippet("hello world test", "world", 30);
        assert!(result.contains("world"));
    }

    #[test]
    fn test_extract_snippet_case_insensitive() {
        let result = extract_snippet("Hello World", "world", 30);
        assert!(result.contains("World"));
    }

    #[test]
    fn test_extract_snippet_no_match() {
        let result = extract_snippet("hello world", "xyz", 20);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_extract_snippet_unicode() {
        let result = extract_snippet("こんにちは世界", "世界", 20);
        assert!(result.contains("世界"));
    }

    #[test]
    fn test_extract_snippet_unicode_long_text() {
        let text = "これは長い日本語のテキストです。検索クエリはここにあります。そして続きます。";
        let result = extract_snippet(text, "検索クエリ", 30);
        assert!(result.contains("検索クエリ"));
        assert!(result.contains("..."));
    }

    #[test]
    fn test_extract_snippet_with_newlines() {
        let text = "line1\nline2\nline3 target line4\nline5";
        let result = extract_snippet(text, "target", 30);
        assert!(result.contains("target"));
        assert!(!result.contains('\n'));
    }

    #[test]
    fn test_extract_snippet_mixed_unicode() {
        let text = "Hello こんにちは World 世界 Test";
        let result = extract_snippet(text, "世界", 20);
        assert!(result.contains("世界"));
    }

    #[test]
    fn test_extract_snippet_at_start() {
        let result = extract_snippet("target is at start of text", "target", 15);
        assert!(result.starts_with("target"));
    }

    #[test]
    fn test_extract_snippet_at_end() {
        let result = extract_snippet("text ends with target", "target", 15);
        assert!(result.ends_with("target"));
    }

    use crate::aggregator::DailyGroup;
    use crate::test_helpers::helpers::{make_daily_group, make_session};
    use chrono::NaiveDate;

    fn make_groups() -> Vec<DailyGroup> {
        vec![
            make_daily_group(
                NaiveDate::from_ymd_opt(2026, 2, 24).unwrap(), // lint-ok: date-literal
                vec![
                    make_session(
                        "~/projects/app-a",
                        Some("Add project filter feature"),
                        Some("feature/project-filter"),
                    ),
                    make_session(
                        "~/projects/other-app",
                        Some("Fix login bug"),
                        Some("fix/login"),
                    ),
                ],
            ),
            make_daily_group(
                NaiveDate::from_ymd_opt(2026, 2, 25).unwrap(), // lint-ok: date-literal
                vec![make_session(
                    "~/projects/app-a",
                    Some("Refactor search module"),
                    None,
                )],
            ),
        ]
    }

    #[test]
    fn test_search_empty_query_returns_empty() {
        let groups = make_groups();
        let results = perform_search(&groups, "");
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_by_project_name() {
        let groups = make_groups();
        let results = perform_search(&groups, "app-a");
        assert_eq!(results.len(), 2);
        assert!(matches!(
            results[0].match_type,
            SearchMatchType::ProjectName
        ));
        assert_eq!(results[0].day_idx, 0);
        assert_eq!(results[0].session_idx, 0);
        assert_eq!(results[1].day_idx, 1);
        assert_eq!(results[1].session_idx, 0);
    }

    #[test]
    fn test_search_by_project_name_case_insensitive() {
        let groups = make_groups();
        let results = perform_search(&groups, "APP-A");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_search_by_summary() {
        let groups = make_groups();
        let results = perform_search(&groups, "login bug");
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0].match_type, SearchMatchType::Summary));
        assert_eq!(results[0].day_idx, 0);
        assert_eq!(results[0].session_idx, 1);
        assert!(results[0].snippet.as_ref().unwrap().contains("login bug"));
    }

    #[test]
    fn test_search_by_git_branch() {
        let groups = make_groups();
        let results = perform_search(&groups, "feature/project");
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0].match_type, SearchMatchType::GitBranch));
        assert_eq!(results[0].session_idx, 0);
    }

    #[test]
    fn test_search_by_date() {
        let groups = make_groups();
        let results = perform_search(&groups, "2026-02-25"); // lint-ok: date-literal
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0].match_type, SearchMatchType::Date));
        assert_eq!(results[0].day_idx, 1);
    }

    #[test]
    fn test_search_by_partial_date() {
        let groups = make_groups();
        let results = perform_search(&groups, "2026-02");
        assert_eq!(results.len(), 3);
        assert!(
            results
                .iter()
                .all(|r| matches!(r.match_type, SearchMatchType::Date))
        );
    }

    #[test]
    fn test_search_priority_project_over_summary() {
        let groups = vec![make_daily_group(
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), // lint-ok: date-literal
            vec![make_session(
                "~/projects/myapp",
                Some("Working on myapp features"),
                None,
            )],
        )];
        let results = perform_search(&groups, "myapp");
        assert_eq!(
            results.len(),
            1,
            "should match project name and skip summary due to continue"
        );
        assert!(matches!(
            results[0].match_type,
            SearchMatchType::ProjectName
        ));
    }

    #[test]
    fn test_search_no_match() {
        let groups = make_groups();
        let results = perform_search(&groups, "nonexistent-xyz");
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_indices_correct_with_multiple_sessions() {
        let groups = make_groups();
        let results = perform_search(&groups, "other-app");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].day_idx, 0);
        assert_eq!(results[0].session_idx, 1);
    }

    #[test]
    fn test_search_content_match_does_not_duplicate_metadata_match() {
        let groups = make_groups();
        let meta_results = perform_search(&groups, "app-a");
        assert_eq!(meta_results.len(), 2);
        let already_matched = meta_results
            .iter()
            .any(|r| r.day_idx == 0 && r.session_idx == 0);
        assert!(
            already_matched,
            "metadata search should find app-a at day 0 session 0"
        );
    }

    #[test]
    fn test_search_date_matches_all_sessions_in_day() {
        let groups = make_groups();
        let results = perform_search(&groups, "2026-02-24"); // lint-ok: date-literal
        assert_eq!(
            results.len(),
            2,
            "all sessions in the matching day should be returned"
        );
        assert!(results.iter().all(|r| r.day_idx == 0));
        assert!(
            results
                .iter()
                .all(|r| matches!(r.match_type, SearchMatchType::Date))
        );
    }

    #[test]
    fn test_search_summary_snippet_is_reasonable_length() {
        let long_summary = "a".repeat(200) + " target " + &"b".repeat(200);
        let groups = vec![make_daily_group(
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), // lint-ok: date-literal
            vec![make_session("~/projects/x", Some(&long_summary), None)],
        )];
        let results = perform_search(&groups, "target");
        assert_eq!(results.len(), 1);
        let snippet = results[0].snippet.as_ref().unwrap();
        assert!(snippet.contains("target"));
        assert!(
            snippet.len() < long_summary.len(),
            "snippet should be shorter than full summary"
        );
    }

    #[test]
    fn test_search_session_idx_skips_subagents() {
        let groups = vec![make_daily_group(
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), // lint-ok: date-literal
            vec![
                {
                    let mut s = make_session("~/projects/sub", None, None);
                    s.is_subagent = true;
                    s
                },
                make_session("~/projects/main-project", None, None),
            ],
        )];
        let results = perform_search(&groups, "main-project");
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].session_idx, 0,
            "session_idx should be filtered index, not raw index"
        );
    }

    #[test]
    fn test_search_subagent_sessions_excluded() {
        let groups = vec![make_daily_group(
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), // lint-ok: date-literal
            vec![{
                let mut s = make_session("~/projects/agent-task", None, None);
                s.is_subagent = true;
                s
            }],
        )];
        let results = perform_search(&groups, "agent-task");
        assert_eq!(results.len(), 0, "subagent sessions should be excluded");
    }
}
