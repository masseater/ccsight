mod tests {
    use super::*;
    use crate::ui::{
        parse_text_with_code_blocks, render_text_with_highlighting,
        render_tool_result_with_highlighting, TextSegment,
    };
    use chrono::Datelike;
    use std::time::Instant;

    #[test]
    fn test_format_number() {
        // Under 1K
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(500), "500");
        assert_eq!(format_number(999), "999");

        // 1K - 10K (2 decimal places)
        assert_eq!(format_number(1000), "1.00K");
        assert_eq!(format_number(1500), "1.50K");
        assert_eq!(format_number(6800), "6.80K");
        assert_eq!(format_number(9500), "9.50K");

        // 10K - 100K (1 decimal place)
        assert_eq!(format_number(10000), "10.0K");
        assert_eq!(format_number(10900), "10.9K");
        assert_eq!(format_number(99500), "99.5K");

        // 100K - 1M (no decimal)
        assert_eq!(format_number(100000), "100K");
        assert_eq!(format_number(500000), "500K");
        assert_eq!(format_number(961400), "961K");
        assert_eq!(format_number(999999), "1000K");

        // 1M - 10M (2 decimal places)
        assert_eq!(format_number(1000000), "1.00M");
        assert_eq!(format_number(1500000), "1.50M");
        assert_eq!(format_number(6800000), "6.80M");
        assert_eq!(format_number(9500000), "9.50M");

        // 10M - 100M (1 decimal place)
        assert_eq!(format_number(10000000), "10.0M");
        assert_eq!(format_number(10900000), "10.9M");

        // 100M - 1B (no decimal)
        assert_eq!(format_number(100000000), "100M");
        assert_eq!(format_number(500000000), "500M");

        // 1B - 10B (2 decimal places)
        assert_eq!(format_number(1000000000), "1.00B");
        assert_eq!(format_number(1500000000), "1.50B");
        assert_eq!(format_number(9500000000), "9.50B");

        // 10B - 100B (1 decimal place)
        assert_eq!(format_number(10000000000), "10.0B");

        // 100B+ (no decimal)
        assert_eq!(format_number(100000000000), "100B");
    }

    #[test]
    fn test_load_data_performance() {
        let result = load_data(20).unwrap();
        if result.file_count == 0 {
            return; // No data available (CI environment)
        }
        assert!(result.file_count <= 20);
    }

    #[test]
    #[ignore] // Deletes cache file — run manually with `cargo test -- --ignored`
    fn test_cache_speedup() {
        std::fs::remove_file(
            std::path::PathBuf::from(
                std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .unwrap_or_else(|_| "/tmp".to_string()),
            )
            .join(".ccsight/cache.json"),
        )
        .ok();

        let start1 = Instant::now();
        let result1 = load_data(20);
        let duration1 = start1.elapsed();
        let _cache1 = result1.unwrap().cache_stats;

        let start2 = Instant::now();
        let result2 = load_data(20);
        let duration2 = start2.elapsed();
        let cache2 = result2.unwrap().cache_stats;

        assert!(cache2.cached_files > 0, "Second load should use cache");
        assert!(
            duration2 < duration1 || duration2.as_millis() < 500,
            "Cached load ({duration2:?}) should be faster than uncached ({duration1:?})"
        );
    }

    #[test]
    fn test_load_data_integration() {
        let result = load_data(5).unwrap();
        if result.file_count == 0 {
            return; // No data available (CI environment)
        }

        assert!(!result.stats.daily_activity.is_empty());
        assert!(!result.stats.project_stats.is_empty());
        assert!(
            result.models_without_pricing.is_empty(),
            "Models without pricing: {:?}",
            result.models_without_pricing
        );

        let groups = &result.daily_groups;
        assert!(!groups.is_empty());
        for i in 1..groups.len() {
            assert!(groups[i - 1].date >= groups[i].date);
        }
    }

    /// Regression: `StatsAggregator` (Overview cost) and `DailyGrouper`
    /// (Daily tab / `--daily`) are independent aggregation paths over the
    /// same files. Token totals MUST match — past divergences came from
    /// dedup / `skip_tokens` quirks. Both honor the cache_5m/1h split.
    #[test]
    fn test_stats_and_grouper_agree_on_token_totals() {
        let result = load_data(0).unwrap();
        if result.file_count == 0 {
            return;
        }

        let mut g_input = 0u64;
        let mut g_output = 0u64;
        let mut g_cache_w = 0u64;
        let mut g_cache_r = 0u64;
        let mut g_cache_5m = 0u64;
        let mut g_cache_1h = 0u64;
        for group in &result.daily_groups {
            for session in &group.sessions {
                for ts in session.day_tokens_by_model.values() {
                    g_input += ts.input_tokens;
                    g_output += ts.output_tokens;
                    g_cache_w += ts.cache_creation_tokens;
                    g_cache_r += ts.cache_read_tokens;
                    g_cache_5m += ts.cache_creation_5m_tokens;
                    g_cache_1h += ts.cache_creation_1h_tokens;
                }
            }
        }

        // Two aggregators open JSONLs independently; in a dev env with a
        // live ccsight session, kilotoken drift can leak between passes.
        // Real regressions produced megatoken gaps, so 0.1% stays sharp.
        let s = &result.stats.total_tokens;
        let pairs: [(u64, u64, &str); 6] = [
            (g_input, s.input_tokens, "input"),
            (g_output, s.output_tokens, "output"),
            (g_cache_w, s.cache_creation_tokens, "cache_w"),
            (g_cache_r, s.cache_read_tokens, "cache_r"),
            (g_cache_5m, s.cache_creation_5m_tokens, "cache_5m"),
            (g_cache_1h, s.cache_creation_1h_tokens, "cache_1h"),
        ];
        for (g, s, name) in pairs {
            let max = g.max(s);
            let diff = g.abs_diff(s);
            // 0.5% + 1024-floor: tolerates inter-pass live-JSONL drift,
            // still catches real regressions (multi-megatoken gaps).
            let allowed = (max / 200).max(1024);
            assert!(
                diff <= allowed,
                "{name}: grouper={g} stats={s} diff={diff} > allowed={allowed} \
                 — the two aggregation paths diverged beyond mid-test drift."
            );
        }
    }

    #[test]
    #[ignore]
    fn bench_search_index() {
        let _ = infrastructure::SearchIndex::clear_index();
        let result = load_data(0).unwrap();
        if result.daily_groups.is_empty() {
            return;
        }
        let groups = &result.daily_groups;
        let session_count: usize = groups
            .iter()
            .map(|g| g.user_sessions().count())
            .sum();
        println!("Sessions: {session_count}");

        let start = Instant::now();
        let index = infrastructure::SearchIndex::update_or_build(groups).unwrap();
        let build_time = start.elapsed();
        println!("Index build: {build_time:?}");

        let queries = ["cargo", "hello", "commit", "error", "ratatui", "日本語テスト"];
        println!("\n--- Search breakdown ---");
        for q in &queries {
            let start = Instant::now();
            let results = index.search(q, 200, 50);
            let total = start.elapsed();
            println!("'{q}': {total:?} total, {} results", results.len());
        }

        println!("\n--- Warm search (2nd call) ---");
        for q in &queries {
            let start = Instant::now();
            let results = index.search(q, 200, 50);
            let total = start.elapsed();
            println!("'{q}': {total:?} total, {} results", results.len());
        }

        println!("\n--- Fallback regex path ---");
        let special_queries = ["cargo build", "hello world", "fn main()", "state.search_"];
        for q in &special_queries {
            let start = Instant::now();
            let results = index.search(q, 200, 50);
            let total = start.elapsed();
            println!("'{q}': {total:?} total, {} results", results.len());
        }

        println!("\n--- Linear scan comparison ---");
        let start = Instant::now();
        let mut linear_count = 0;
        let mut searched = 0;
        for group in groups {
            for session in group.user_sessions() {
                if searched >= 100 { break; }
                if search::search_session_content(&session.file_path, "cargo").is_some() {
                    linear_count += 1;
                }
                searched += 1;
            }
        }
        let linear_time = start.elapsed();
        println!("Linear 'cargo' ({searched} files): {linear_time:?} ({linear_count} matches)");

        println!("\n--- 2nd update_or_build (no changes) ---");
        let start = Instant::now();
        let _index2 = infrastructure::SearchIndex::update_or_build(groups).unwrap();
        println!("Open existing: {:?}", start.elapsed());
    }

    #[test]
    fn test_parse_text_with_code_blocks_simple() {
        let text = "Hello\n```rust\nfn main() {}\n```\nWorld";
        let segments = parse_text_with_code_blocks(text);

        assert_eq!(segments.len(), 3);

        match &segments[0] {
            TextSegment::Plain(s) => assert_eq!(s, "Hello"),
            _ => panic!("Expected Plain segment"),
        }

        match &segments[1] {
            TextSegment::Code { lang, content } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert_eq!(content, "fn main() {}");
            }
            _ => panic!("Expected Code segment"),
        }

        match &segments[2] {
            TextSegment::Plain(s) => assert_eq!(s, "World"),
            _ => panic!("Expected Plain segment"),
        }
    }

    #[test]
    fn test_parse_text_with_code_blocks_multiple() {
        let text = "```rust\nlet x = 1;\n```\nText\n```python\nprint('hi')\n```";
        let segments = parse_text_with_code_blocks(text);

        assert_eq!(
            segments.len(),
            3,
            "segments: {:?}",
            segments
                .iter()
                .map(|s| match s {
                    TextSegment::Plain(p) => format!("Plain({p:?})"),
                    TextSegment::Code { lang, content } =>
                        format!("Code({lang:?}, {content:?})"),
                })
                .collect::<Vec<_>>()
        );

        match &segments[0] {
            TextSegment::Code { lang, content } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert_eq!(content, "let x = 1;");
            }
            _ => panic!("Expected Code segment"),
        }

        match &segments[1] {
            TextSegment::Plain(s) => assert_eq!(s, "Text"),
            _ => panic!("Expected Plain segment"),
        }

        match &segments[2] {
            TextSegment::Code { lang, content } => {
                assert_eq!(lang.as_deref(), Some("python"));
                assert_eq!(content, "print('hi')");
            }
            _ => panic!("Expected Code segment"),
        }
    }

    #[test]
    fn test_parse_text_with_code_blocks_no_lang() {
        let text = "```\nsome code\n```";
        let segments = parse_text_with_code_blocks(text);

        assert_eq!(segments.len(), 1);

        match &segments[0] {
            TextSegment::Code { lang, content } => {
                assert!(lang.is_none());
                assert_eq!(content, "some code");
            }
            _ => panic!("Expected Code segment"),
        }
    }

    #[test]
    fn test_parse_text_with_code_blocks_real_jsonl() {
        let text = "コードブロックの例:\n```rust\nfn create_dedup_hash(entry: &LogEntry) -> Option<String> {\n    let request_id = entry.request_id.as_ref()?;\n    Some(format!(\"{}:{}\", message_id, request_id))\n}\n```\n改善点を説明します。";
        let segments = parse_text_with_code_blocks(text);

        assert_eq!(segments.len(), 3);

        match &segments[0] {
            TextSegment::Plain(s) => assert!(s.contains("コードブロックの例")),
            _ => panic!("Expected Plain segment"),
        }

        match &segments[1] {
            TextSegment::Code { lang, content } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert!(content.contains("create_dedup_hash"));
                assert!(content.contains("Option<String>"));
            }
            _ => panic!("Expected Code segment"),
        }

        match &segments[2] {
            TextSegment::Plain(s) => assert!(s.contains("改善点")),
            _ => panic!("Expected Plain segment"),
        }
    }

    #[test]
    fn test_render_text_with_highlighting() {
        let text = "Hello\n```rust\nfn main() {\n    println!(\"test\");\n}\n```\nWorld";
        let (lines, flags) = render_text_with_highlighting(text, 80);

        println!("Rendered {} lines:", lines.len());
        for (i, line) in lines.iter().enumerate() {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            println!("Line {i}: {text}");
        }

        assert_eq!(lines.len(), flags.len());
        assert!(lines.len() >= 7, "Should have at least 7 lines (plain + code header + 3 code lines + code footer + plain)");

        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(all_text.contains("Hello"));
        assert!(all_text.contains("rust"));
        assert!(all_text.contains("fn main()"));
        assert!(all_text.contains("World"));
    }

    #[test]
    fn test_highlighting_with_actual_jsonl_text() {
        let text = "全コードを確認しました。レビュー結果をまとめます。\n\n---\n\n## コードレビュー\n\n### 1. 重複エントリ除外\n\n**良い点:**\n- `HashSet`を使った効率的な重複検出\n\n**改善点:**\n```rust\nfn create_dedup_hash(entry: &LogEntry) -> Option<String> {\n    let request_id = entry.request_id.as_ref()?;\n    let message_id = entry.message.as_ref()?.id.as_ref()?;\n    Some(format!(\"{}:{}\", message_id, request_id))\n}\n```\n- 毎回`format!`で文字列を生成";

        let segments = parse_text_with_code_blocks(text);
        println!("Segments: {}", segments.len());

        let mut found_code = false;
        for (i, seg) in segments.iter().enumerate() {
            match seg {
                TextSegment::Plain(p) => println!("Segment {}: Plain({} chars)", i, p.len()),
                TextSegment::Code { lang, content } => {
                    println!(
                        "Segment {}: Code(lang={:?}, {} chars)",
                        i,
                        lang,
                        content.len()
                    );
                    found_code = true;
                    assert_eq!(lang.as_deref(), Some("rust"));
                    assert!(content.contains("create_dedup_hash"));
                }
            }
        }

        assert!(found_code, "Should find at least one code block");

        let (lines, _flags) = render_text_with_highlighting(text, 80);
        println!("\nRendered {} lines", lines.len());

        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();

        assert!(
            all_text.contains("コードレビュー"),
            "Should contain Japanese text"
        );
        assert!(all_text.contains("rust"), "Should contain language label");
        assert!(
            all_text.contains("create_dedup_hash"),
            "Should contain function name"
        );
    }

    #[test]
    fn test_render_tool_result_with_highlighting() {
        let content = "The file /home/user/project/src/main.rs has been updated. Here's the result of running `cat -n` on a snippet of the edited file:\n     1→use std::io;\n     2→\n     3→fn main() {\n     4→    println!(\"Hello\");\n     5→}\n";

        let (lines, _flags) = render_tool_result_with_highlighting(content, 80);

        println!("Rendered {} lines:", lines.len());
        for (i, line) in lines.iter().enumerate() {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            println!("Line {i}: {text}");
        }

        assert!(lines.len() >= 5, "Should have at least 5 lines");

        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();

        assert!(all_text.contains("use std::io"), "Should contain code");
        assert!(all_text.contains("fn main()"), "Should contain function");
        assert!(
            all_text.contains("1→") || all_text.contains("→"),
            "Should contain line numbers"
        );
    }

    #[test]
    fn test_parse_custom_single_date() {
        let f = PeriodFilter::parse_custom("2026-02-15").unwrap();  // lint-ok: date-literal
        match f {
            PeriodFilter::Custom(start, Some(end)) => {
                assert_eq!(start, NaiveDate::from_ymd_opt(2026, 2, 15).unwrap());  // lint-ok: date-literal
                assert_eq!(end, NaiveDate::from_ymd_opt(2026, 2, 15).unwrap());  // lint-ok: date-literal
            }
            _ => panic!("expected Custom with same start/end"),
        }
    }

    #[test]
    fn test_parse_custom_year_month() {
        let f = PeriodFilter::parse_custom("2026-02").unwrap();
        match f {
            PeriodFilter::Custom(start, Some(end)) => {
                assert_eq!(start, NaiveDate::from_ymd_opt(2026, 2, 1).unwrap());  // lint-ok: date-literal
                assert_eq!(end, NaiveDate::from_ymd_opt(2026, 2, 28).unwrap());  // lint-ok: date-literal
            }
            _ => panic!("expected Custom with month range"),
        }
    }

    #[test]
    fn test_parse_custom_year_month_december() {
        let f = PeriodFilter::parse_custom("2025-12").unwrap();
        match f {
            PeriodFilter::Custom(start, Some(end)) => {
                assert_eq!(start, NaiveDate::from_ymd_opt(2025, 12, 1).unwrap());  // lint-ok: date-literal
                assert_eq!(end, NaiveDate::from_ymd_opt(2025, 12, 31).unwrap());  // lint-ok: date-literal
            }
            _ => panic!("expected Custom with december range"),
        }
    }

    #[test]
    fn test_parse_custom_leap_year_february() {
        let f = PeriodFilter::parse_custom("2024-02").unwrap();
        match f {
            PeriodFilter::Custom(start, Some(end)) => {
                assert_eq!(start, NaiveDate::from_ymd_opt(2024, 2, 1).unwrap());  // lint-ok: date-literal
                assert_eq!(end, NaiveDate::from_ymd_opt(2024, 2, 29).unwrap());  // lint-ok: date-literal
            }
            _ => panic!("expected leap year feb range"),
        }
    }

    #[test]
    fn test_parse_custom_range() {
        let f = PeriodFilter::parse_custom("2026-01-01..2026-01-31").unwrap();  // lint-ok: date-literal
        match f {
            PeriodFilter::Custom(start, Some(end)) => {
                assert_eq!(start, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());  // lint-ok: date-literal
                assert_eq!(end, NaiveDate::from_ymd_opt(2026, 1, 31).unwrap());  // lint-ok: date-literal
            }
            _ => panic!("expected Custom range"),
        }
    }

    #[test]
    fn test_parse_custom_range_with_spaces() {
        let f = PeriodFilter::parse_custom("  2026-01-01 .. 2026-01-31  ").unwrap();  // lint-ok: date-literal
        match f {
            PeriodFilter::Custom(start, Some(end)) => {
                assert_eq!(start, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());  // lint-ok: date-literal
                assert_eq!(end, NaiveDate::from_ymd_opt(2026, 1, 31).unwrap());  // lint-ok: date-literal
            }
            _ => panic!("expected Custom range with trimmed spaces"),
        }
    }

    #[test]
    fn test_parse_custom_invalid_garbage() {
        assert!(PeriodFilter::parse_custom("abc").is_none());
    }

    #[test]
    fn test_parse_custom_invalid_month() {
        assert!(PeriodFilter::parse_custom("2026-13").is_none());
    }

    #[test]
    fn test_parse_custom_empty_string() {
        assert!(PeriodFilter::parse_custom("").is_none());
    }

    #[test]
    fn test_parse_custom_invalid_date() {
        assert!(PeriodFilter::parse_custom("2026-02-30").is_none());  // lint-ok: date-literal
    }

    #[test]
    fn test_date_range_label_all() {
        assert_eq!(PeriodFilter::All.date_range_label(), "");
    }

    #[test]
    fn test_date_range_label_today() {
        let label = PeriodFilter::Today.date_range_label();
        assert!(label.starts_with('('));
        assert!(label.ends_with(')'));
    }

    #[test]
    fn test_date_range_label_custom_range() {
        let start = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();  // lint-ok: date-literal
        let end = NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();  // lint-ok: date-literal
        let label = PeriodFilter::Custom(start, Some(end)).date_range_label();
        assert!(label.contains("01-01"));
        assert!(label.contains("01-31"));
    }

    #[test]
    fn test_period_filter_date_range_all() {
        let (start, end) = PeriodFilter::All.date_range();
        assert!(start.is_none());
        assert!(end.is_none());
    }

    #[test]
    fn test_period_filter_date_range_last_month() {
        let (start, end) = PeriodFilter::LastMonth.date_range();
        assert!(start.is_some());
        assert!(end.is_some());
        let s = start.unwrap();
        let e = end.unwrap();
        assert_eq!(s.day(), 1);
        assert!(e >= s);
    }

    #[test]
    fn test_apply_filter_no_filter_restores_original() {
        use crate::test_helpers::helpers::*;
        let today = chrono::Local::now().date_naive();
        let groups = vec![
            make_daily_group(today, vec![make_session("~/projects/a", None, None)]),
            make_daily_group(
                today - chrono::Duration::days(5),
                vec![make_session("~/projects/b", None, None)],
            ),
        ];
        let mut state = make_test_app_state(groups);
        state.period_filter = PeriodFilter::All;
        state.project_filter = None;
        state.apply_filter();
        assert_eq!(state.daily_groups.len(), 2);
    }

    #[test]
    fn test_apply_filter_project_only() {
        use crate::test_helpers::helpers::*;
        let today = chrono::Local::now().date_naive();
        let groups = vec![make_daily_group(
            today,
            vec![
                make_session_with_tokens("~/projects/alpha", 1000, 500, "claude-sonnet-4-20250514"),
                make_session_with_tokens("~/projects/beta", 2000, 800, "claude-sonnet-4-20250514"),
            ],
        )];
        let mut state = make_test_app_state(groups);
        state.project_filter = Some("~/projects/alpha".to_string());
        state.apply_filter();

        assert_eq!(state.daily_groups.len(), 1);
        assert_eq!(state.daily_groups[0].sessions.len(), 1);
        assert_eq!(
            state.daily_groups[0].sessions[0].project_name,
            "~/projects/alpha"
        );
    }

    #[test]
    fn test_apply_filter_project_removes_empty_groups() {
        use crate::test_helpers::helpers::*;
        let today = chrono::Local::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let groups = vec![
            make_daily_group(today, vec![make_session("~/projects/alpha", None, None)]),
            make_daily_group(yesterday, vec![make_session("~/projects/beta", None, None)]),
        ];
        let mut state = make_test_app_state(groups);
        state.project_filter = Some("~/projects/alpha".to_string());
        state.apply_filter();

        assert_eq!(state.daily_groups.len(), 1);
        assert_eq!(state.daily_groups[0].date, today);
    }

    #[test]
    fn test_apply_filter_period_and_project_combined() {
        use crate::test_helpers::helpers::*;
        let today = chrono::Local::now().date_naive();
        let old = today - chrono::Duration::days(60);
        let groups = vec![
            make_daily_group(
                today,
                vec![
                    make_session("~/projects/alpha", None, None),
                    make_session("~/projects/beta", None, None),
                ],
            ),
            make_daily_group(old, vec![make_session("~/projects/alpha", None, None)]),
        ];
        let mut state = make_test_app_state(groups);
        state.period_filter = PeriodFilter::Last30d;
        state.project_filter = Some("~/projects/alpha".to_string());
        state.apply_filter();

        assert_eq!(state.daily_groups.len(), 1);
        assert_eq!(state.daily_groups[0].sessions.len(), 1);
        assert_eq!(
            state.daily_groups[0].sessions[0].project_name,
            "~/projects/alpha"
        );
    }

    #[test]
    fn test_apply_filter_resets_selected_day_when_out_of_bounds() {
        use crate::test_helpers::helpers::*;
        let today = chrono::Local::now().date_naive();
        let groups = vec![
            make_daily_group(today, vec![make_session("~/projects/a", None, None)]),
            make_daily_group(
                today - chrono::Duration::days(1),
                vec![make_session("~/projects/b", None, None)],
            ),
        ];
        let mut state = make_test_app_state(groups);
        state.selected_day = 5;
        state.project_filter = Some("~/projects/a".to_string());
        state.apply_filter();

        assert!(state.selected_day < state.daily_groups.len());
    }

    #[test]
    fn test_rebuild_project_list() {
        use crate::test_helpers::helpers::*;
        let today = chrono::Local::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let groups = vec![
            make_daily_group(
                today,
                vec![
                    make_session_with_tokens("~/projects/big", 5000, 3000, "sonnet"),
                    make_session_with_tokens("~/projects/small", 100, 50, "sonnet"),
                ],
            ),
            make_daily_group(
                yesterday,
                vec![make_session_with_tokens("~/projects/big", 2000, 1000, "sonnet")],
            ),
        ];
        let mut state = make_test_app_state(groups);
        state.rebuild_project_list();

        assert_eq!(state.project_list.len(), 2);
        assert_eq!(state.project_list[0].0, "~/projects/big");
        assert!(state.project_list[0].1 > state.project_list[1].1);
        assert_eq!(state.project_list[0].2, today);
    }

    #[test]
    fn test_rebuild_project_list_includes_subagents() {
        // Subagent sessions in their own bucket must appear: project_stats (dashboard
        // panel) and apply_filter both include subagents, and excluding them here
        // would make the project popup understate the total relative to the panel.
        use crate::test_helpers::helpers::*;
        let today = chrono::Local::now().date_naive();
        let mut subagent = make_session("~/projects/agent-task", None, None);
        subagent.is_subagent = true;
        let groups = vec![make_daily_group(
            today,
            vec![make_session("~/projects/main", None, None), subagent],
        )];
        let mut state = make_test_app_state(groups);
        state.rebuild_project_list();

        assert_eq!(state.project_list.len(), 2);
        let names: Vec<&String> = state.project_list.iter().map(|(n, _, _)| n).collect();
        assert!(names.iter().any(|n| n.as_str() == "~/projects/main"));
        assert!(names.iter().any(|n| n.as_str() == "~/projects/agent-task"));
    }

    fn make_buffer(width: u16, height: u16, lines: &[&str]) -> ratatui::buffer::Buffer {
        let area = ratatui::layout::Rect::new(0, 0, width, height);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        for (row, line) in lines.iter().enumerate() {
            buf.set_string(0, row as u16, line, ratatui::style::Style::default());
        }
        buf
    }

    #[test]
    fn test_extract_ascii_single_line() {
        let buf = make_buffer(20, 3, &["Hello, World!       ", "Second line         "]);
        let sel = (0, 0, 12, 0);
        let text = extract_selected_text_from_buffer(&sel, &buf, None, None, 0);
        assert_eq!(text, "Hello, World!");
    }

    #[test]
    fn test_extract_ascii_multi_line() {
        let buf = make_buffer(20, 3, &["Hello               ", "World               "]);
        let sel = (0, 0, 19, 1);
        let text = extract_selected_text_from_buffer(&sel, &buf, None, None, 0);
        assert_eq!(text, "Hello\nWorld");
    }

    #[test]
    fn test_extract_cjk_no_extra_spaces() {
        let buf = make_buffer(20, 2, &["自動スクロール      "]);
        let sel = (0, 0, 19, 0);
        let text = extract_selected_text_from_buffer(&sel, &buf, None, None, 0);
        assert_eq!(text, "自動スクロール");
    }

    #[test]
    fn test_extract_cjk_mixed_with_ascii() {
        let buf = make_buffer(30, 2, &["Hello自動World      "]);
        let sel = (0, 0, 29, 0);
        let text = extract_selected_text_from_buffer(&sel, &buf, None, None, 0);
        assert_eq!(text, "Hello自動World");
    }

    #[test]
    fn test_extract_cjk_partial_selection() {
        let buf = make_buffer(20, 2, &["あいうえお          "]);
        let sel = (2, 0, 7, 0);
        let text = extract_selected_text_from_buffer(&sel, &buf, None, None, 0);
        assert_eq!(text, "いうえ");
    }

    #[test]
    fn test_extract_with_clamp_area() {
        let buf = make_buffer(
            40,
            5,
            &[
                "│ sidebar │content line 1               ",
                "│ sidebar │content line 2               ",
                "│ sidebar │content line 3               ",
            ],
        );
        let conv_area = ratatui::layout::Rect::new(11, 0, 29, 3);
        let sel = (11, 0, 39, 2);
        let text = extract_selected_text_from_buffer(&sel, &buf, Some(conv_area), None, 0);
        assert_eq!(text, "content line 1\ncontent line 2\ncontent line 3");
    }

    #[test]
    fn test_extract_clamp_excludes_outside_rows() {
        let buf = make_buffer(
            30,
            5,
            &[
                "header                        ",
                "content A                     ",
                "content B                     ",
                "footer                        ",
            ],
        );
        let conv_area = ratatui::layout::Rect::new(0, 1, 30, 2);
        let sel = (0, 1, 29, 2);
        let text = extract_selected_text_from_buffer(&sel, &buf, Some(conv_area), None, 0);
        assert_eq!(text, "content A\ncontent B");
    }

    #[test]
    fn test_extract_clamp_not_applied_when_start_outside() {
        let buf = make_buffer(
            40,
            3,
            &[
                "│ sidebar │ content                     ",
                "│ sidebar │ more                        ",
            ],
        );
        let conv_area = ratatui::layout::Rect::new(11, 0, 29, 2);
        let sel = (0, 0, 39, 1);
        let text = extract_selected_text_from_buffer(&sel, &buf, Some(conv_area), None, 0);
        assert!(text.contains("sidebar"));
    }

    #[test]
    fn test_extract_reversed_selection() {
        let buf = make_buffer(20, 2, &["Hello World         "]);
        let sel = (10, 0, 0, 0);
        let text = extract_selected_text_from_buffer(&sel, &buf, None, None, 0);
        assert_eq!(text, "Hello World");
    }

    #[test]
    fn test_extract_trailing_empty_lines_removed() {
        let buf = make_buffer(20, 5, &["Hello", "World", "   ", "   "]);
        let sel = (0, 0, 19, 3);
        let text = extract_selected_text_from_buffer(&sel, &buf, None, None, 0);
        assert_eq!(text, "Hello\nWorld");
    }

    #[test]
    fn test_join_conversation_lines_word_wrap() {
        let lines = vec![
            "  This is a very long line that was word".to_string(),
            "  wrapped by the renderer".to_string(),
        ];
        let flags = vec![false, true];
        let text = join_conversation_lines(&lines, &flags);
        assert_eq!(text, "This is a very long line that was word wrapped by the renderer");
    }

    #[test]
    fn test_join_conversation_lines_preserves_paragraph_break() {
        let lines = vec![
            "  Short line".to_string(),
            "  Another line".to_string(),
        ];
        let flags = vec![false, false];
        let text = join_conversation_lines(&lines, &flags);
        assert_eq!(text, "Short line\nAnother line");
    }

    #[test]
    fn test_join_conversation_lines_strips_arrow_prefix() {
        let lines = vec![
            "▶ First message".to_string(),
            "  continuation".to_string(),
        ];
        let flags = vec![false, false];
        let text = join_conversation_lines(&lines, &flags);
        assert_eq!(text, "First message\ncontinuation");
    }

    #[test]
    fn test_join_conversation_lines_cjk_word_wrap() {
        let lines = vec![
            "  あいうえおかきくけこさしすせそたちつてと".to_string(),
            "  なにぬねの".to_string(),
        ];
        let flags = vec![false, true];
        let text = join_conversation_lines(&lines, &flags);
        assert_eq!(
            text,
            "あいうえおかきくけこさしすせそたちつてと なにぬねの"
        );
    }

    #[test]
    fn test_join_conversation_lines_empty() {
        let lines: Vec<String> = vec![];
        let flags: Vec<bool> = vec![];
        let text = join_conversation_lines(&lines, &flags);
        assert_eq!(text, "");
    }

    #[test]
    fn test_join_conversation_lines_empty_lines_between() {
        let lines = vec![
            "  Paragraph one end that fills the width".to_string(),
            String::new(),
            "  Paragraph two".to_string(),
        ];
        let flags = vec![false, false, false];
        let text = join_conversation_lines(&lines, &flags);
        assert_eq!(text, "Paragraph one end that fills the width\n\nParagraph two");
    }

    #[test]
    fn test_extract_with_wrap_flags_removes_newlines() {
        let buf = make_buffer(20, 2, &[
            "  hello world this",
            "  is a test",
        ]);
        let conv_area = ratatui::layout::Rect::new(0, 0, 20, 2);
        let flags = vec![false, true];
        let sel = (0, 0, 19, 1);
        let text = extract_selected_text_from_buffer(&sel, &buf, Some(conv_area), Some(&flags), 0);
        assert_eq!(text, "hello world this is a test");
    }

    #[test]
    fn test_extract_with_wrap_flags_preserves_real_newlines() {
        let buf = make_buffer(20, 2, &[
            "  line one",
            "  line two",
        ]);
        let conv_area = ratatui::layout::Rect::new(0, 0, 20, 2);
        let flags = vec![false, false];
        let sel = (0, 0, 19, 1);
        let text = extract_selected_text_from_buffer(&sel, &buf, Some(conv_area), Some(&flags), 0);
        assert_eq!(text, "line one\nline two");
    }

    #[test]
    fn test_extract_with_wrap_flags_scroll_offset() {
        let buf = make_buffer(20, 2, &[
            "  wrapped line",
            "  continuation",
        ]);
        let conv_area = ratatui::layout::Rect::new(0, 0, 20, 2);
        let flags: Vec<bool> = vec![false; 5].into_iter()
            .chain(vec![false, true])
            .collect();
        let sel = (0, 0, 19, 1);
        let text = extract_selected_text_from_buffer(&sel, &buf, Some(conv_area), Some(&flags), 5);
        assert_eq!(text, "wrapped line continuation");
    }

    #[test]
    fn test_extract_with_wrap_flags_cjk() {
        let buf = make_buffer(24, 2, &[
            "  あいうえおかきくけこ",
            "  さしすせそ",
        ]);
        let conv_area = ratatui::layout::Rect::new(0, 0, 24, 2);
        let flags = vec![false, true];
        let sel = (0, 0, 23, 1);
        let text = extract_selected_text_from_buffer(&sel, &buf, Some(conv_area), Some(&flags), 0);
        assert_eq!(text, "あいうえおかきくけこ さしすせそ");
    }
}
