//! Mouse-event handlers extracted from `main.rs::run`. Same conventions as
//! `keyboard.rs`: `&mut AppState` only, no return value, callers route by
//! `MouseEventKind`.

use crate::state::{ConvListMode, ConversationPane, SCROLL_LINES};
use crate::{AppState, PeriodFilter, Tab};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::helpers::{make_daily_group, make_session, make_test_app_state};
    use ratatui::layout::Rect;

    fn daily_state_for_click() -> AppState {
        // The mouse-click math is independent of the date; pick a fixed
        // reference so the fixture does not couple to the wall clock.
        let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(); // lint-ok: date-literal
        let group = make_daily_group(date, vec![make_session("proj", None, None)]);
        let mut state = make_test_app_state(vec![group]);
        state.tab = Tab::Daily;
        state.show_conversation = false;
        // session_list_area: full-width 140 col panel, height 10, starting at row 13.
        // item_height = 2 (line1 + line2 per session).
        state.session_list_area = Some((
            Rect {
                x: 0,
                y: 13,
                width: 140,
                height: 10,
            },
            0,
            2,
        ));
        state
    }

    #[test]
    fn info_button_click_opens_detail() {
        let mut state = daily_state_for_click();
        // Row 14 = line1 of session 0 (area.y=13, relative_y=1, line_in_session=0).
        // Column 137 falls within [136, 139) — the [i] hit zone.
        handle_mouse_click(&mut state, 137, 14);
        assert!(state.show_detail, "clicking [i] should open session detail");
        assert_eq!(state.selected_session, 0);
    }

    #[test]
    fn click_off_info_button_only_selects() {
        let mut state = daily_state_for_click();
        // Row 14 line1 of session 0, but column 10 is far from [i].
        handle_mouse_click(&mut state, 10, 14);
        assert!(!state.show_detail, "non-[i] click must not open detail");
        assert_eq!(state.selected_session, 0);
    }

    #[test]
    fn click_on_line2_of_info_column_does_not_open() {
        let mut state = daily_state_for_click();
        // Row 15 = line2 of session 0 (relative_y=2, line_in_session=1).
        // Even at the info-button column, line2 must not trigger the popup.
        handle_mouse_click(&mut state, 137, 15);
        assert!(
            !state.show_detail,
            "line2 click in info column must not open detail"
        );
    }

    fn conv_state_for_click() -> AppState {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(); // lint-ok: date-literal
        let session = make_session("proj", None, None);
        let group = make_daily_group(date, vec![session.clone()]);
        let mut state = make_test_app_state(vec![group]);
        state.tab = Tab::Daily;
        state.show_conversation = true;
        // One pane occupying right half: x=70, width=70. The info-area [i]
        // button hits at row=area.y+1=6, cols [70+66, 70+69) = [136, 139).
        let mut pane = crate::state::ConversationPane::default();
        pane.file_path = Some(session.file_path.clone());
        state.panes.push(pane);
        state.pane_areas = vec![Rect {
            x: 70,
            y: 5,
            width: 70,
            height: 30,
        }];
        state
    }

    #[test]
    fn conv_info_button_click_opens_detail() {
        let mut state = conv_state_for_click();
        // row=6 (area.y+1), col=137 → inside [136, 139)
        handle_mouse_click(&mut state, 137, 6);
        assert!(
            state.show_detail,
            "[i] in conv pane should open session detail"
        );
        assert_eq!(state.active_pane_index, Some(0));
    }

    #[test]
    fn conv_pane_body_click_does_not_open_detail() {
        let mut state = conv_state_for_click();
        // Click inside pane body (not on the info button row/column).
        handle_mouse_click(&mut state, 100, 10);
        assert!(
            !state.show_detail,
            "body click in conv pane should not open session detail"
        );
        assert_eq!(state.active_pane_index, Some(0));
    }

    #[test]
    fn conv_info_button_click_ignored_without_session() {
        let mut state = conv_state_for_click();
        // Drop the file_path so the pane has no session.
        state.panes[0].file_path = None;
        handle_mouse_click(&mut state, 137, 6);
        assert!(
            !state.show_detail,
            "without a session, [i] click must not open detail"
        );
    }
}

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

    // Projects detail popup (panel 1): clicking a project row moves the
    // cursor. Guarded by `!show_project_detail` so a click that falls through
    // an open per-project popup doesn't reset the cursor underneath.
    if state.show_dashboard_detail && state.dashboard_panel == 1 && !state.show_project_detail {
        for (idx, area) in state.project_detail_row_areas.clone() {
            if in_area(column, row, &area) {
                state.dashboard_scroll[1] = idx;
                return;
            }
        }
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
            ConvListMode::Live => ConvListMode::Live,
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
        // [i] button: per-pane right-aligned indicator on the info-area
        // summary row. Coords must match draw_conversation_pane's info_rect
        // (top-right of pane, occupies 3 cells just inside the right border).
        for (idx, area) in state.pane_areas.iter().enumerate() {
            let has_session = state.panes.get(idx).is_some_and(|p| p.file_path.is_some());
            if !has_session {
                continue;
            }
            let btn_left = area.x + area.width.saturating_sub(4);
            let btn_right = area.x + area.width.saturating_sub(1);
            let btn_row = area.y + 1;
            if row == btn_row && column >= btn_left && column < btn_right {
                state.active_pane_index = Some(idx);
                state.show_detail = true;
                state.session_detail_scroll = 0;
                return;
            }
        }

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
                // [i] button: line1 of session, last 3 cols of the inner area open the detail popup.
                let info_btn_left = area.x + area.width.saturating_sub(4);
                let info_btn_right = area.x + area.width.saturating_sub(1);
                let on_line1 = relative_y >= 1 && (relative_y - 1).is_multiple_of(item_height);
                if on_line1 && column >= info_btn_left && column < info_btn_right {
                    state.show_detail = true;
                    state.session_detail_scroll = 0;
                }
            }
        }
        return;
    }

    // Live tab session click — analogous to the Daily handler above, but
    // the row layout has two sections (Active / Paused) interleaved with
    // header lines, so the line-to-session math is bespoke.
    if state.tab == Tab::Live
        && let Some((area, scroll, active_count)) = state.live_list_area
        && column >= area.x
        && column < area.x + area.width
        && row >= area.y
        && row < area.y + area.height
    {
        let line_idx = scroll + (row - area.y) as usize;
        // Layout (must mirror `draw_live` exactly):
        //   line 0: "Active now (N)" header + status-count subtitle
        //   line 1..: active rows × 3 (metadata + title + last user message)
        //   then: blank separator + "Recently paused" header + paused rows × 3
        // The active block starts at line 1 — before this it was line 2 with
        // a leading blank, but that blank was removed for compactness and
        // this hit-test math has to follow.
        const ROW_H: usize = 3;
        let active_block_start = 1usize;
        let active_block_end = active_block_start + active_count * ROW_H;
        let paused_block_start = active_block_end + 2;
        let (session_idx, row_line0) =
            if line_idx >= active_block_start && line_idx < active_block_end {
                let rel = line_idx - active_block_start;
                (
                    Some(rel / ROW_H),
                    active_block_start + (rel / ROW_H) * ROW_H,
                )
            } else if line_idx >= paused_block_start {
                let rel = line_idx - paused_block_start;
                let paused_count = state.live_paused.len();
                if rel / ROW_H < paused_count {
                    (
                        Some(active_count + rel / ROW_H),
                        paused_block_start + (rel / ROW_H) * ROW_H,
                    )
                } else {
                    (None, 0)
                }
            } else {
                (None, 0)
            };
        if let Some(idx) = session_idx {
            state.live_selected = idx;
            // [i] hit zone: last 3 cols of line 1 of the clicked row.
            let info_btn_left = area.x + area.width.saturating_sub(4);
            let info_btn_right = area.x + area.width.saturating_sub(1);
            let on_line1 = line_idx == row_line0;
            if on_line1 && column >= info_btn_left && column < info_btn_right {
                // Reuse the `i` key path so the popup wiring stays in one
                // place (see handlers::keyboard `KeyCode::Char('i')` arm).
                let live_info = crate::live_selected_session(state).map(|live| {
                    let started = live
                        .started_at
                        .map(|t| {
                            t.with_timezone(&chrono::Local)
                                .format("%Y-%m-%d %H:%M")
                                .to_string()
                        })
                        .unwrap_or_default();
                    (
                        live.jsonl_path.clone(),
                        live.pid,
                        live.status.clone().unwrap_or_else(|| "—".to_string()),
                        started,
                    )
                });
                if let Some((Some(jsonl), pid, status, started)) = live_info {
                    let found = state.original_daily_groups.iter().find_map(|g| {
                        g.sessions
                            .iter()
                            .find(|s| s.file_path == jsonl && !s.is_subagent)
                            .cloned()
                    });
                    if let Some(session) = found {
                        state.session_detail_override = Some(session);
                        state.session_detail_live_extra = Some((pid, status, started));
                        state.show_detail = true;
                        state.session_detail_scroll = 0;
                    }
                }
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

    // Double-click on a project row in Projects detail popup opens the
    // per-project detail popup (mirrors keyboard Enter). Guarded by
    // `!show_project_detail` so a double-click *inside* an already-open
    // per-project popup doesn't reopen the underlying row. Sort key must
    // mirror dashboard.rs panel 1 — including the alphabetical tiebreaker
    // (lint #30) — so `idx` indexes into the same list shown on screen.
    if state.show_dashboard_detail && state.dashboard_panel == 1 && !state.show_project_detail {
        for (idx, area) in state.project_detail_row_areas.clone() {
            if in_area(column, row, &area) {
                let mut projects: Vec<_> = state.stats.project_stats.iter().collect();
                projects.sort_by(|a, b| {
                    b.1.work_tokens
                        .cmp(&a.1.work_tokens)
                        .then_with(|| a.0.cmp(b.0))
                });
                if let Some((name, _)) = projects.get(idx) {
                    state.project_detail_path = (*name).clone();
                    state.project_detail_scroll = 0;
                    state.show_project_detail = true;
                    state.dashboard_scroll[1] = idx;
                }
                return;
            }
        }
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

    if state.show_project_detail {
        if up {
            state.project_detail_scroll = state.project_detail_scroll.saturating_sub(SCROLL_LINES);
        } else {
            state.project_detail_scroll = state.project_detail_scroll.saturating_add(SCROLL_LINES);
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
            state.insights_detail_scroll =
                state.insights_detail_scroll.saturating_add(SCROLL_LINES);
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

    if state.tab == Tab::Live {
        // Mirror the j/k keyboard handler: scroll = move the selected row.
        // The draw pass auto-adjusts `live_scroll` (viewport) to keep the
        // selection in view, so we only need to nudge `live_selected`.
        let n = crate::live_visible_count(state);
        if up {
            state.live_selected = state.live_selected.saturating_sub(1);
        } else if state.live_selected + 1 < n {
            state.live_selected += 1;
        }
        return;
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
