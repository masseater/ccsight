//! Mouse-event handlers extracted from `main.rs::run`. Same conventions as
//! `keyboard.rs`: `&mut AppState` only, no return value, callers route by
//! `MouseEventKind`.

use crate::state::{ConvListMode, ConversationPane, SCROLL_LINES};
use crate::{AppState, PeriodFilter, Tab};

/// `column`/`row` are inside the area iff both axes contain them. Used by
/// every popup/list/pane click test.
pub(crate) fn in_area(column: u16, row: u16, area: &ratatui::layout::Rect) -> bool {
    column >= area.x && column < area.x + area.width && row >= area.y && row < area.y + area.height
}

/// Handles a left-click — dispatches to the topmost popup, then to tab/panel
/// areas, finally to the conversation list. Click outside any handled region
/// dismisses the topmost overlay (per [`crate::dismiss_overlay`]).
pub(crate) fn handle_mouse_click(state: &mut AppState, column: u16, row: u16) {
    // Popups are topmost - check them first
    if state.show_filter_popup && !state.filter_input_mode {
        if let Some(area) = state.filter_popup_area
            && in_area(column, row, &area)
        {
            let relative_row = (row - area.y).saturating_sub(1) as usize;
            let total_items = PeriodFilter::ALL_VARIANTS.len() + 1;
            if relative_row < total_items {
                state.filter_popup_selected = relative_row;
            }
            return;
        }
        crate::dismiss_overlay(state);
        return;
    }

    if state.show_project_popup {
        if let Some(area) = state.project_popup_area
            && in_area(column, row, &area)
        {
            let relative_row = (row - area.y).saturating_sub(1) as usize;
            let clicked_idx = state.project_popup_scroll + relative_row;
            let total = state.project_list.len() + 1;
            if clicked_idx < total {
                state.project_popup_selected = clicked_idx;
            }
            return;
        }
        crate::dismiss_overlay(state);
        return;
    }

    if state.search_mode
        && !state.search_results.is_empty()
        && let Some(area) = state.search_results_area
        && in_area(column, row, &area)
    {
        let item_height = 2usize;
        let visible_items = area.height.saturating_sub(2) as usize / item_height;
        let scroll_start = if visible_items > 0 && state.search_selected >= visible_items {
            state.search_selected - visible_items + 1
        } else {
            0
        };
        let relative_row = (row - area.y).saturating_sub(1) as usize;
        let clicked_idx = scroll_start + relative_row / item_height;
        if clicked_idx < state.search_results.len() {
            state.search_selected = clicked_idx;
        }
        return;
    }

    // Tool Usage detail popup: tab click switches section.
    if state.show_dashboard_detail && state.dashboard_panel == 3 {
        for (idx, area) in state.tools_detail_tab_areas.clone() {
            if in_area(column, row, &area) {
                state.tools_detail_section = idx;
                state.dashboard_scroll[state.dashboard_panel] = 0;
                // Tools tab is index 0 (Built-in + MCP merged); reset MCP
                // cursor when switching INTO it. Earlier this checked == 1
                // (which was Skills) — a stale leftover from before the
                // Tools/Skills/Commands/Subagents reordering.
                if state.tools_detail_section == 0 {
                    state.mcp_selected_server = 0;
                    state.mcp_selected_tool = None;
                }
                return;
            }
        }
        // Tools tab: clicking a server row toggles expansion and moves cursor.
        if state.tools_detail_section == 0 {
            for (idx, area) in state.mcp_server_row_areas.clone() {
                if in_area(column, row, &area) {
                    state.mcp_selected_server = idx;
                    state.mcp_selected_tool = None;
                    let servers = crate::collect_mcp_servers(state);
                    if let Some(name) = servers.get(idx) {
                        if state.mcp_expanded_servers.contains(name) {
                            state.mcp_expanded_servers.remove(name);
                        } else {
                            state.mcp_expanded_servers.insert(name.clone());
                        }
                    }
                    return;
                }
            }
        }
    }

    // Detail-style popups (help/summary/detail/dashboard_detail/insights_detail):
    // click inside does nothing (preserves text selection), click outside dismisses.
    if crate::has_blocking_popup(state) {
        let popup_area = if state.show_summary {
            state.summary_popup_area
        } else {
            state.active_popup_area
        };
        if popup_area.is_some_and(|a| in_area(column, row, &a)) {
            return;
        }
        crate::dismiss_overlay(state);
        return;
    }

    // Check if click is on help trigger button
    if let Some(area) = state.help_trigger
        && in_area(column, row, &area)
    {
        state.show_help = true;
        state.help_scroll = 0;
        return;
    }

    // Check if click is on filter/project trigger buttons in tab bar
    if !state.show_conversation {
        if let Some(area) = state.filter_popup_area_trigger
            && in_area(column, row, &area)
        {
            state.show_filter_popup = true;
            state.filter_popup_selected = 0;
            return;
        }
        if let Some(area) = state.project_popup_area_trigger
            && in_area(column, row, &area)
        {
            state.rebuild_project_list();
            state.show_project_popup = true;
            state.project_popup_selected = 0;
            state.project_popup_scroll = 0;
            return;
        }
        if let Some(area) = state.pin_view_trigger
            && in_area(column, row, &area)
            && !state.pins.entries().is_empty()
        {
            state.conv_list_mode = ConvListMode::Pinned;
            state.selected_session = 0;
            state.tab = Tab::Daily;
            state.panes.clear();
            state.panes.push(ConversationPane::default());
            state.show_conversation = true;
            state.active_pane_index = None;
            crate::open_conversation_in_pane(state);
            state.active_pane_index = None;
            return;
        }
    }

    // Check if click is on a tab (only when not showing conversation)
    if !state.show_conversation {
        let clicked_tab = state.tab_areas.iter().find_map(|(tab, area)| {
            if in_area(column, row, area) {
                Some(*tab)
            } else {
                None
            }
        });
        if let Some(tab) = clicked_tab {
            state.show_dashboard_detail = false;
            state.show_insights_detail = false;
            state.show_detail = false;
            state.daily_breakdown_focus = false;
            state.daily_breakdown_scroll = 0;
            if state.show_summary || state.generating_summary {
                state.clear_summary();
            }
            state.tab = tab;
            return;
        }
    }

    // Check if click is on session list title (mode toggle) in conversation view
    if state.show_conversation
        && state.tab == Tab::Daily
        && let Some((area, _, _)) = state.session_list_area
        && column >= area.x
        && column < area.x + area.width
        && area.y > 0
        && row == area.y - 1
    {
        state.conv_list_mode = match state.conv_list_mode {
            ConvListMode::Day => {
                if state.pins.entries().is_empty() {
                    ConvListMode::All
                } else {
                    ConvListMode::Pinned
                }
            }
            ConvListMode::Pinned => ConvListMode::All,
            ConvListMode::All => ConvListMode::Day,
        };
        state.selected_session = 0;
        state.active_pane_index = None;
        return;
    }

    // Check if click is on session list (in conversation view)
    if state.show_conversation
        && state.tab == Tab::Daily
        && let Some((area, scroll, item_height)) = state.session_list_area
        && column >= area.x
        && column < area.x + area.width
        && row >= area.y
        && row < area.y + area.height
    {
        let relative_y = (row - area.y) as usize;
        let clicked_idx = scroll + relative_y / item_height;
        let session_count = crate::get_conv_session_count(state);
        if clicked_idx < session_count {
            state.selected_session = clicked_idx;
        }
        state.active_pane_index = None;
        return;
    }

    // Check if click is on a pane (in conversation view)
    if state.show_conversation {
        for (idx, area) in state.pane_areas.iter().enumerate() {
            if in_area(column, row, area) {
                state.active_pane_index = Some(idx);
                let content_y = area.y + 1;
                if row >= content_y
                    && let Some(pane) = state.panes.get_mut(idx)
                    && !pane.message_lines.is_empty()
                {
                    let clicked_line = pane.scroll + (row - content_y) as usize;
                    if let Some(msg_idx) = pane
                        .message_lines
                        .iter()
                        .rposition(|&(start, _)| start <= clicked_line)
                    {
                        pane.selected_message = msg_idx;
                    }
                }
                return;
            }
        }
    }

    // Tools panel: click on a category row (▶ marker) opens the detail popup with
    // that section pre-selected. Checked BEFORE the generic panel-click handler so
    // the row click takes priority over panel selection.
    if state.tab == Tab::Dashboard && !state.show_conversation {
        let category_areas = state.tools_panel_category_areas.clone();
        for (section_idx, area) in category_areas {
            if in_area(column, row, &area) {
                state.dashboard_panel = 3;
                state.tools_detail_section = section_idx;
                state.show_dashboard_detail = true;
                state.dashboard_scroll[3] = 0;
                // Tools tab is section 0 — reset the MCP cursor on entry so
                // a stale value from the prior popup view doesn't push the
                // cursor past the visible server list.
                if section_idx == 0 {
                    state.mcp_selected_server = 0;
                    state.mcp_selected_tool = None;
                }
                return;
            }
        }
    }

    // Check if click is on a dashboard panel
    if state.tab == Tab::Dashboard && !state.show_conversation {
        for (idx, area) in state.dashboard_panel_areas.iter().enumerate() {
            if column >= area.x
                && column < area.x + area.width
                && row >= area.y
                && row < area.y + area.height
            {
                state.dashboard_panel = idx;
                return;
            }
        }
    }

    // Check if click is on an insights panel
    if state.tab == Tab::Insights && !state.show_conversation {
        for (idx, area) in state.insights_panel_areas.iter().enumerate() {
            if column >= area.x
                && column < area.x + area.width
                && row >= area.y
                && row < area.y + area.height
            {
                state.insights_panel = idx;
                return;
            }
        }
    }

    // Check if click is on Daily header (left/right navigation)
    if state.tab == Tab::Daily
        && !state.show_conversation
        && let Some(area) = state.daily_header_area
        && in_area(column, row, &area)
    {
        let mid = area.x + area.width / 2;
        if column < mid {
            // Left half: go to older day
            if state.selected_day < state.daily_groups.len().saturating_sub(1) {
                state.selected_day += 1;
                state.selected_session = 0;
            }
        } else {
            // Right half: go to newer day
            if state.selected_day > 0 {
                state.selected_day -= 1;
                state.selected_session = 0;
            }
        }
        return;
    }

    // Check if click is on a session in Daily view
    if state.tab == Tab::Daily
        && !state.show_conversation
        && let Some((area, scroll, item_height)) = state.session_list_area
        && column >= area.x
        && column < area.x + area.width
        && row >= area.y
        && row < area.y + area.height
    {
        let relative_y = (row - area.y) as usize;
        let clicked_idx = scroll + relative_y / item_height;
        if let Some(group) = state.daily_groups.get(state.selected_day) {
            let session_count = group.user_sessions().count();
            if clicked_idx < session_count {
                state.selected_session = clicked_idx;
            }
        }
        return;
    }

    // Click on empty area dismisses topmost overlay
    crate::dismiss_overlay(state);
}

/// Handles a left-button double-click — only used for popups + Daily session
/// list (open conversation) + breakdown focus.
pub(crate) fn handle_double_click(state: &mut AppState, column: u16, row: u16) {
    if state.show_summary {
        if let Some(popup_area) = state.summary_popup_area
            && !in_area(column, row, &popup_area)
        {
            state.clear_summary();
        }
        return;
    }

    if state.show_filter_popup && !state.filter_input_mode {
        if state.filter_popup_selected < PeriodFilter::ALL_VARIANTS.len() {
            state.period_filter = PeriodFilter::ALL_VARIANTS[state.filter_popup_selected];
            state.apply_filter();
            state.show_filter_popup = false;
        } else {
            state.filter_input_mode = true;
            let text = match state.period_filter {
                PeriodFilter::Custom(s, Some(e)) if s == e => s.format("%Y-%m-%d").to_string(),
                PeriodFilter::Custom(s, Some(e)) => {
                    format!("{}..{}", s.format("%Y-%m-%d"), e.format("%Y-%m-%d"))
                }
                PeriodFilter::Custom(s, None) => s.format("%Y-%m-%d").to_string(),
                _ => String::new(),
            };
            state.filter_input.set(text);
            state.filter_input_error = false;
        }
        return;
    }

    if state.show_project_popup {
        if state.project_popup_selected == 0 {
            state.project_filter = None;
        } else if let Some((name, _, _)) = state.project_list.get(state.project_popup_selected - 1)
        {
            state.project_filter = Some(name.clone());
        }
        state.apply_filter();
        state.show_project_popup = false;
        return;
    }

    if state.search_mode && !state.search_results.is_empty() {
        let result = &state.search_results[state.search_selected];
        state.selected_day = result.day_idx;
        state.selected_session = result.session_idx;
        state.tab = Tab::Daily;
        state.search_mode = false;
        state.search_task = None;
        state.searching = false;
        return;
    }

    if crate::has_blocking_popup(state) {
        return;
    }

    if state.show_conversation
        && state.tab == Tab::Daily
        && let Some((area, scroll, item_height)) = state.session_list_area
        && in_area(column, row, &area)
    {
        let relative_y = (row - area.y) as usize;
        let clicked_idx = scroll + relative_y / item_height;
        let session_count = crate::get_conv_session_count(state);
        if clicked_idx < session_count {
            state.selected_session = clicked_idx;
            crate::open_conversation_in_pane(state);
        }
        return;
    }

    if !state.show_conversation
        && state.tab == Tab::Daily
        && let Some((area, scroll, item_height)) = state.session_list_area
        && in_area(column, row, &area)
    {
        let relative_y = (row - area.y) as usize;
        let clicked_idx = scroll + relative_y / item_height;
        if let Some(group) = state.daily_groups.get(state.selected_day) {
            let session_count = group.user_sessions().count();
            if clicked_idx < session_count {
                state.selected_session = clicked_idx;
                crate::open_conversation_in_pane(state);
            }
        }
        return;
    }

    if !state.show_conversation
        && state.tab == Tab::Daily
        && let Some(area) = state.breakdown_panel_area
        && in_area(column, row, &area)
    {
        state.daily_breakdown_focus = true;
        state.daily_breakdown_scroll = 0;
        return;
    }

    if !state.show_conversation && state.tab == Tab::Dashboard {
        for (idx, area) in state.dashboard_panel_areas.iter().enumerate() {
            if in_area(column, row, area) {
                state.dashboard_panel = idx;
                state.show_dashboard_detail = true;
                return;
            }
        }
    }

    if !state.show_conversation && state.tab == Tab::Insights {
        for (idx, area) in state.insights_panel_areas.iter().enumerate() {
            if in_area(column, row, area) {
                state.insights_panel = idx;
                state.show_insights_detail = true;
                state.insights_detail_scroll = 0;
                return;
            }
        }
    }
}

/// Handles a wheel scroll event — popup-aware, panel-aware. `up = true` means
/// the user scrolled the wheel toward themselves (content moves down visually).
pub(crate) fn handle_mouse_scroll(state: &mut AppState, column: u16, row: u16, up: bool) {
    if state.show_filter_popup {
        let max = PeriodFilter::ALL_VARIANTS.len();
        if up {
            state.filter_popup_selected = state.filter_popup_selected.saturating_sub(1);
        } else if state.filter_popup_selected < max {
            state.filter_popup_selected += 1;
        }
        return;
    }

    if state.show_project_popup {
        let max = state.project_list.len().saturating_sub(1);
        if up {
            state.project_popup_selected = state.project_popup_selected.saturating_sub(1);
        } else if state.project_popup_selected < max {
            state.project_popup_selected += 1;
        }
        return;
    }

    if state.show_summary {
        if up {
            state.summary_scroll = state.summary_scroll.saturating_sub(SCROLL_LINES);
        } else {
            state.summary_scroll += SCROLL_LINES;
        }
        return;
    }

    if state.search_mode && !state.search_results.is_empty() {
        let max = state.search_results.len().saturating_sub(1);
        if up {
            state.search_selected = state.search_selected.saturating_sub(1);
        } else if state.search_selected < max {
            state.search_selected += 1;
        }
        return;
    }

    if state.show_help {
        if up {
            state.help_scroll = state.help_scroll.saturating_sub(1);
        } else {
            state.help_scroll = state.help_scroll.saturating_add(1);
        }
        return;
    }

    if state.show_detail {
        if up {
            state.session_detail_scroll = state.session_detail_scroll.saturating_sub(1);
        } else {
            state.session_detail_scroll = state.session_detail_scroll.saturating_add(1);
        }
        return;
    }

    if state.show_insights_detail {
        if up {
            state.insights_detail_scroll =
                state.insights_detail_scroll.saturating_sub(SCROLL_LINES);
        } else {
            state.insights_detail_scroll = state.insights_detail_scroll.saturating_add(SCROLL_LINES);
        }
        return;
    }

    if state.show_dashboard_detail {
        if up {
            state.dashboard_scroll[state.dashboard_panel] =
                state.dashboard_scroll[state.dashboard_panel].saturating_sub(1);
        } else {
            let max_items = crate::dashboard_max_items(state);
            let scroll = &mut state.dashboard_scroll[state.dashboard_panel];
            if *scroll + 1 < max_items {
                *scroll += 1;
            }
        }
        return;
    }

    if state.show_conversation {
        if let Some((area, _, _)) = state.session_list_area
            && in_area(column, row, &area)
        {
            let max = crate::get_conv_session_count(state).saturating_sub(1);
            if up {
                state.selected_session = state.selected_session.saturating_sub(1);
            } else if state.selected_session < max {
                state.selected_session += 1;
            }
            return;
        }

        for (idx, area) in state.pane_areas.iter().enumerate() {
            if in_area(column, row, area) {
                if let Some(pane) = state.panes.get_mut(idx) {
                    if up {
                        pane.scroll = pane.scroll.saturating_sub(SCROLL_LINES);
                    } else {
                        // Bound against the rendered cache when present so a
                        // burst of scroll-down events can't drive `scroll`
                        // past the actual content (the draw clamp would
                        // recover it, but `selected_message` below is
                        // computed before the next draw and would land on
                        // the last message spuriously).
                        let max_scroll = pane
                            .rendered
                            .as_ref()
                            .map_or(usize::MAX, |(lines, _, _, _)| {
                                lines.len().saturating_sub(area.height as usize)
                            });
                        pane.scroll = pane.scroll.saturating_add(SCROLL_LINES).min(max_scroll);
                    }
                    if !pane.message_lines.is_empty() {
                        let msg_idx = if up {
                            pane.message_lines
                                .iter()
                                .rposition(|&(start, _)| start <= pane.scroll)
                                .unwrap_or(0)
                        } else {
                            pane.message_lines
                                .iter()
                                .position(|&(start, _)| start >= pane.scroll)
                                .unwrap_or(pane.message_lines.len() - 1)
                        };
                        pane.selected_message = msg_idx;
                    }
                }
                return;
            }
        }

        return;
    }

    if state.tab == Tab::Dashboard {
        for (idx, area) in state.dashboard_panel_areas.iter().enumerate() {
            if in_area(column, row, area) {
                let (scroll_up, scroll_down) = if idx == 5 { (!up, up) } else { (up, !up) };
                if scroll_up {
                    state.dashboard_scroll[idx] = state.dashboard_scroll[idx].saturating_sub(1);
                } else if scroll_down {
                    let saved = state.dashboard_panel;
                    state.dashboard_panel = idx;
                    let max_items = crate::dashboard_max_items(state);
                    state.dashboard_panel = saved;
                    if state.dashboard_scroll[idx] + 1 < max_items {
                        state.dashboard_scroll[idx] += 1;
                    }
                }
                return;
            }
        }
    }

    if state.tab == Tab::Daily {
        if state.daily_breakdown_focus {
            if up {
                state.daily_breakdown_scroll =
                    state.daily_breakdown_scroll.saturating_sub(SCROLL_LINES);
            } else if state.daily_breakdown_scroll < state.daily_breakdown_max_scroll {
                state.daily_breakdown_scroll = (state.daily_breakdown_scroll + SCROLL_LINES)
                    .min(state.daily_breakdown_max_scroll);
            }
        } else {
            let max = state
                .daily_groups
                .get(state.selected_day)
                .map_or(0, |g| g.user_sessions().count().saturating_sub(1));
            if up {
                state.selected_session = state.selected_session.saturating_sub(1);
            } else if state.selected_session < max {
                state.selected_session += 1;
            }
        }
    }
}
