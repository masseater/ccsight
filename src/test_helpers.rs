#[cfg(test)]
pub mod helpers {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use chrono::{NaiveDate, Utc};

    use crate::aggregator::{DailyGroup, ModelTokens, SessionInfo};
    use crate::state::TextInput;

    /// Default `SessionInfo` skeleton — every field zeroed/empty. Other helpers
    /// in this module mutate the bits they care about, keeping each helper
    /// focused on its discriminating fields rather than re-listing 18 defaults.
    fn default_session() -> SessionInfo {
        SessionInfo {
            file_path: PathBuf::from("/tmp/test.jsonl"),
            project_name: String::new(),
            git_branch: None,
            session_first_timestamp: Utc::now(),
            day_first_timestamp: Utc::now(),
            day_last_timestamp: Utc::now(),
            day_input_tokens: 0,
            day_output_tokens: 0,
            day_user_msgs: 0,
            day_assistant_msgs: 0,
            day_tokens_by_model: HashMap::new(),
            day_hourly_activity: HashMap::new(),
            day_hourly_work_tokens: HashMap::new(),
            day_tool_usage: HashMap::new(),
            day_language_usage: HashMap::new(),
            day_extension_usage: HashMap::new(),
            summary: None,
            custom_title: None,
            ai_title: None,
            last_user_message: None,
            first_user_message: None,
            model: None,
            is_subagent: false,
            is_continued: false,
        }
    }

    pub fn make_session(project: &str, summary: Option<&str>, branch: Option<&str>) -> SessionInfo {
        // Synthesize a unique-per-session file_path so callers that build
        // multiple sessions per group don't collide on the default `/tmp/test.jsonl`.
        // perform_search dedupes by file_path, so without this fixture sessions
        // sharing a path would disappear from the result set.
        let path = format!(
            "/tmp/test/{}/{}.jsonl",
            project.replace('/', "_"),
            branch.unwrap_or("none")
        );
        SessionInfo {
            file_path: PathBuf::from(path),
            project_name: project.to_string(),
            git_branch: branch.map(std::string::ToString::to_string),
            day_input_tokens: 1000,
            day_output_tokens: 500,
            summary: summary.map(std::string::ToString::to_string),
            ..default_session()
        }
    }

    pub fn make_session_with_tokens(
        project: &str,
        input_tokens: u64,
        output_tokens: u64,
        model: &str,
    ) -> SessionInfo {
        let mut tokens_by_model = HashMap::new();
        tokens_by_model.insert(
            model.to_string(),
            ModelTokens {
                input_tokens,
                output_tokens,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                cache_creation_5m_tokens: 0,
                cache_creation_1h_tokens: 0,
            },
        );
        SessionInfo {
            project_name: project.to_string(),
            day_input_tokens: input_tokens,
            day_output_tokens: output_tokens,
            day_tokens_by_model: tokens_by_model,
            model: Some(model.to_string()),
            ..default_session()
        }
    }

    pub fn make_daily_group(date: NaiveDate, sessions: Vec<SessionInfo>) -> DailyGroup {
        DailyGroup { date, sessions }
    }

    pub fn make_test_app_state(groups: Vec<DailyGroup>) -> crate::AppState {
        let daily_costs: Vec<(NaiveDate, f64)> = groups.iter().map(|g| (g.date, 1.0)).collect();

        crate::AppState {
            needs_draw: false,
            tab: crate::Tab::Dashboard,
            pins: crate::pins::Pins::empty(),
            conv_list_mode: crate::ConvListMode::Day,
            stats: crate::aggregator::Stats::default(),
            total_cost: daily_costs.len() as f64,
            model_costs: Vec::new(),
            aggregated_model_tokens: HashMap::new(),
            models_without_pricing: std::collections::HashSet::new(),
            daily_groups: groups.clone(),
            daily_costs: daily_costs.clone(),
            selected_day: 0,
            selected_session: 0,
            show_detail: false,
            session_detail_override: None,
            session_detail_live_extra: None,
            show_help: false,
            live_active: Vec::new(),
            live_paused: Vec::new(),
            live_selected: 0,
            live_scroll: 0,
            live_paused_scroll: 0,
            live_has_snapshot_history: false,
            live_sessions_task: None,
            live_last_update: None,
            prior_run_alive: std::collections::HashMap::new(),
            live_view_snapshot_offset: 0,
            live_past_sessions: Vec::new(),
            live_past_snapshot_meta: None,
            live_past_snapshot_total: 0,
            show_project_detail: false,
            project_detail_path: String::new(),
            project_detail_scroll: 0,
            help_scroll: 0,
            show_conversation: false,
            show_summary: false,
            summary_content: String::new(),
            summary_scroll: 0,
            summary_type: None,
            daily_breakdown_focus: false,
            daily_breakdown_scroll: 0,
            daily_breakdown_max_scroll: 0,
            generating_summary: false,
            summary_task: None,
            loading: false,
            error: None,
            file_count: 0,
            cache_stats: None,
            dashboard_panel: 0,
            dashboard_scroll: [0; 7],
            dashboard_viewport: [0; 7],
            activity_view_weekly: false,
            show_dashboard_detail: false,
            tools_detail_section: 0,
            mcp_expanded_servers: std::collections::HashSet::new(),
            mcp_selected_server: 0,
            mcp_selected_tool: None,
            search_mode: false,
            search_input: TextInput::default(),
            search_results: Vec::new(),
            search_selected: 0,
            search_task: None,
            searching: false,
            search_preview_mode: false,
            search_saved_state: None,
            search_history: crate::search_history::SearchHistory::default(),
            mcp_status: Vec::new(),
            configured_resources: crate::infrastructure::ConfiguredResources::default(),
            tool_last_used: HashMap::new(),
            search_index: None,
            index_build_task: None,
            ctrl_c_pressed: false,
            last_click_time: None,
            last_click_pos: (0, 0),
            text_selection: None,
            selecting: false,
            mouse_down_pos: None,
            screen_buffer: None,
            conversation_content_area: None,
            updating_session: None,
            updating_task: None,
            last_data_update: None,
            last_index_build: None,
            data_reload_task: None,
            clipboard_task: None,
            data_limit: 0,
            animation_frame: 0,
            retention_warning: None,
            retention_warning_dismissed: false,
            show_insights_detail: false,
            insights_detail_scroll: 0,
            session_detail_scroll: 0,
            insights_panel: 0,
            toast_message: None,
            toast_time: None,
            panes: Vec::new(),
            active_pane_index: None,
            session_list_hidden: false,
            tab_areas: Vec::new(),
            tools_detail_tab_areas: Vec::new(),
            tools_panel_category_areas: Vec::new(),
            mcp_server_row_areas: Vec::new(),
            project_detail_row_areas: Vec::new(),
            pane_areas: Vec::new(),
            dashboard_panel_areas: Vec::new(),
            insights_panel_areas: Vec::new(),
            session_list_area: None,
            live_list_area: None,
            live_paused_list_area: None,
            breakdown_panel_area: None,
            summary_popup_area: None,
            active_popup_area: None,
            daily_header_area: None,
            filter_popup_area_trigger: None,
            project_popup_area_trigger: None,
            pin_view_trigger: None,
            help_trigger: None,
            filter_popup_area: None,
            project_popup_area: None,
            search_results_area: None,
            period_filter: crate::PeriodFilter::All,
            show_filter_popup: false,
            filter_popup_selected: 0,
            filter_input_mode: false,
            filter_input: TextInput::default(),
            filter_input_error: false,
            project_filter: None,
            show_project_popup: false,
            project_popup_selected: 0,
            project_popup_scroll: 0,
            project_list: Vec::new(),
            project_labels: HashMap::new(),
            original_daily_groups: groups,
            original_daily_costs: daily_costs.clone(),
            original_stats: crate::aggregator::Stats::default(),
            original_total_cost: daily_costs.len() as f64,
            original_model_costs: Vec::new(),
            original_aggregated_model_tokens: HashMap::new(),
            dashboard_projects_sort: crate::state::RankSort::default(),
            dashboard_models_sort: crate::state::RankSort::default(),
            dashboard_ecosystem_sort: crate::state::RankSort::default(),
            dashboard_languages_sort: crate::state::RankSort::default(),
        }
    }
}
