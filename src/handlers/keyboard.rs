//! Popup-state-specific keyboard handlers extracted from `main.rs::run`.
//!
//! Each handler owns one `else if state.show_X { match key.code { ... } }`
//! branch from the main event loop. They are dispatched only when their
//! corresponding popup flag is true (caller does the routing).
//!
//! Conventions:
//! - All handlers take `&mut AppState` and a `KeyEvent`.
//! - No return value — keys are always considered consumed.
//! - Each handler must not assume cross-popup interaction; if a popup-A key
//!   triggers popup-B, that is set up via state flags and the next event tick
//!   picks up the new state.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::handlers;
use crate::state::{ConvListMode, ConversationPane, SummaryType, MAX_PANES};
use crate::{search, ui, AppState, PeriodFilter, Tab};

/// `state.show_help == true` branch — Esc/q/? close, j/k/↑↓ scroll the body.
pub(crate) fn handle_help_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => {
            state.show_help = false;
            state.help_scroll = 0;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.help_scroll = state.help_scroll.saturating_add(1);
        }
        KeyCode::Up | KeyCode::Char('k') => {
            state.help_scroll = state.help_scroll.saturating_sub(1);
        }
        KeyCode::PageDown | KeyCode::Char('d') => {
            state.help_scroll = state.help_scroll.saturating_add(10);
        }
        KeyCode::PageUp | KeyCode::Char('u') => {
            state.help_scroll = state.help_scroll.saturating_sub(10);
        }
        KeyCode::Home | KeyCode::Char('g') => {
            state.help_scroll = 0;
        }
        KeyCode::End | KeyCode::Char('G') => {
            // Draw clamps `help_scroll` against the actual content height,
            // so a saturating-large value here lands on the last page.
            state.help_scroll = u16::MAX;
        }
        _ => {}
    }
}

/// `state.show_insights_detail == true` branch — Esc/q/Enter/i close,
/// ←/→ h/l cycle through 4 panels (wrap), ↑/↓ j/k scroll the body.
pub(crate) fn handle_insights_detail_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter | KeyCode::Char('i') => {
            state.show_insights_detail = false;
        }
        KeyCode::Left | KeyCode::Char('h') => {
            state.insights_panel = if state.insights_panel == 0 {
                3
            } else {
                state.insights_panel - 1
            };
            state.insights_detail_scroll = 0;
        }
        KeyCode::Right | KeyCode::Char('l') => {
            state.insights_panel = if state.insights_panel >= 3 {
                0
            } else {
                state.insights_panel + 1
            };
            state.insights_detail_scroll = 0;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            state.insights_detail_scroll = state.insights_detail_scroll.saturating_add(1);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            state.insights_detail_scroll = state.insights_detail_scroll.saturating_sub(1);
        }
        _ => {}
    }
}

/// `state.show_filter_popup == true` branch — period filter popup with two
/// sub-modes (preset list nav vs Custom date input).
pub(crate) fn handle_filter_popup_key(state: &mut AppState, key: KeyEvent) {
    let total_items = PeriodFilter::ALL_VARIANTS.len() + 1;
    if state.filter_input_mode {
        match key.code {
            // Esc backs out of the input field to the list of preset
            // ranges (still inside the filter popup); a second Esc
            // closes the popup. Without this two-step exit users
            // who fat-fingered the Custom row had no way to escape
            // back to the preset list without losing the popup.
            KeyCode::Esc => {
                state.filter_input_mode = false;
                state.filter_input.clear();
                state.filter_input_error = false;
            }
            KeyCode::Enter => {
                if let Some(filter) = PeriodFilter::parse_custom(&state.filter_input.text) {
                    state.period_filter = filter;
                    state.apply_filter();
                    state.show_filter_popup = false;
                    state.filter_input_mode = false;
                    state.filter_input.clear();
                    state.filter_input_error = false;
                } else {
                    state.filter_input_error = true;
                }
            }
            KeyCode::Backspace => {
                state.filter_input.delete_back();
                state.filter_input_error = false;
            }
            KeyCode::Left => {
                state.filter_input.move_left();
            }
            KeyCode::Right => {
                state.filter_input.move_right();
            }
            KeyCode::Home => {
                state.filter_input.move_home();
            }
            KeyCode::End => {
                state.filter_input.move_end();
            }
            KeyCode::Char(c)
                // Restrict input to characters that can legally appear
                // in `YYYY-MM-DD[..YYYY-MM-DD]` / `YYYY-MM`. Without
                // this filter, accidental presses of `f`/`j`/`k` (the
                // popup-trigger and list-nav keys) silently appended
                // garbage to the field, leaving the user no way to
                // recover except Backspace-to-empty.
                if (c.is_ascii_digit() || c == '-' || c == '.') => {
                    state.filter_input.insert_char(c);
                    state.filter_input_error = false;
                }
            _ => {}
        }
    } else {
        let custom_idx = PeriodFilter::ALL_VARIANTS.len();
        let on_custom_row = state.filter_popup_selected == custom_idx;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('f') => {
                state.show_filter_popup = false;
            }
            KeyCode::Up | KeyCode::Char('k')
                if state.filter_popup_selected > 0 => {
                    state.filter_popup_selected -= 1;
                }
            KeyCode::Down | KeyCode::Char('j')
                if state.filter_popup_selected < total_items - 1 => {
                    state.filter_popup_selected += 1;
                }
            KeyCode::Enter => {
                if state.filter_popup_selected < PeriodFilter::ALL_VARIANTS.len() {
                    state.period_filter =
                        PeriodFilter::ALL_VARIANTS[state.filter_popup_selected];
                    state.apply_filter();
                    state.show_filter_popup = false;
                } else {
                    state.filter_input_mode = true;
                    let text = match state.period_filter {
                        PeriodFilter::Custom(s, Some(e)) if s == e => {
                            s.format("%Y-%m-%d").to_string()
                        }
                        PeriodFilter::Custom(s, Some(e)) => {
                            format!("{}..{}", s.format("%Y-%m-%d"), e.format("%Y-%m-%d"))
                        }
                        PeriodFilter::Custom(s, None) => s.format("%Y-%m-%d").to_string(),
                        _ => String::new(),
                    };
                    state.filter_input.set(text);
                    state.filter_input_error = false;
                }
            }
            // When the cursor is on the Custom row, typing a digit
            // (or `-`/`.` separators) jumps straight into input mode
            // with that character as the first keystroke. Saves the
            // user from having to press Enter first.
            KeyCode::Char(c)
                if on_custom_row && (c.is_ascii_digit() || c == '-' || c == '.') =>
            {
                state.filter_input_mode = true;
                state.filter_input.set(String::new());
                state.filter_input.insert_char(c);
                state.filter_input_error = false;
            }
            _ => {}
        }
    }
}

/// `state.show_detail == true` branch — Session detail popup. Esc/q/Enter/i
/// close, ↑↓/j/k scroll, Space toggles pin, C opens conversation in a new
/// pane, S/r kicks off (re-)summary, R regenerates the JSONL summary.
pub(crate) fn handle_session_detail_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter | KeyCode::Char('i') => {
            state.show_detail = false;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.session_detail_scroll = state.session_detail_scroll.saturating_add(1);
        }
        KeyCode::Up | KeyCode::Char('k') => {
            state.session_detail_scroll = state.session_detail_scroll.saturating_sub(1);
        }
        KeyCode::Char(' ') => {
            if let Some(group) = state.daily_groups.get(state.selected_day) {
                let sessions: Vec<_> = group.user_sessions().collect();
                if let Some(session) = sessions.get(state.selected_session) {
                    state.pins.toggle(&session.file_path);
                    if let Err(e) = state.pins.save() {
                        state.toast_message = Some(format!("Pin save failed: {e}"));
                        state.toast_time = Some(std::time::Instant::now());
                    }
                    state.needs_draw = true;
                }
            }
        }
        KeyCode::Char('C') => {
            let no_loading_panes = state.panes.iter().all(|p| !p.loading);
            if no_loading_panes
                && state.panes.len() < MAX_PANES
                && let Some(group) = state.daily_groups.get(state.selected_day)
            {
                let sessions: Vec<_> = group.user_sessions().collect();
                if let Some(session) = sessions.get(state.selected_session) {
                    state
                        .panes
                        .push(ConversationPane::load_from(&session.file_path));
                    state.active_pane_index = Some(state.panes.len() - 1);
                    state.conv_list_mode = ConvListMode::Day;
                    state.show_conversation = true;
                    state.show_detail = false;
                }
            }
        }
        KeyCode::Char('S') | KeyCode::Char('r') => {
            // Both `S` (force) and `r` (regen-from-popup) reuse the
            // same `generate_session_summary` path here — the popup
            // already shows a stale summary, so even `r` is treated
            // as a fresh generate (the dedicated regen variant is
            // wired through `show_summary`'s own `r` handler).
            if state.summary_task.is_none()
                && let Some(session) = crate::current_selected_session(state)
            {
                handlers::tasks::start_session_summary(state, session, false);
            }
        }
        KeyCode::Char('R') => {
            let selected_day = state.selected_day;
            let selected_session = state.selected_session;
            if let Some((actual_idx, session)) =
                crate::current_selected_session_with_index(state)
                && state.updating_task.is_none()
            {
                handlers::tasks::start_jsonl_regen(
                    state,
                    session,
                    selected_day,
                    selected_session,
                    actual_idx,
                );
            }
        }
        _ => {}
    }
}

/// `state.show_conversation == true` branch — handles both the in-pane content
/// search mode AND the larger pane navigation / list nav state. The original
/// branch in `main.rs` was ~530 lines and used `continue` to skip to the next
/// iteration; we translate that to plain `return;` (the only post-dispatch
/// work in the loop is `state.needs_draw = true`, which is harmless to also
/// run).
pub(crate) fn handle_conversation_key(
    state: &mut AppState,
    key: crossterm::event::KeyEvent,
    preview_conversation_in_pane: fn(&mut AppState),
    open_conversation_in_pane: fn(&mut AppState),
    get_conv_session_file: fn(&AppState, usize) -> Option<std::path::PathBuf>,
    get_conv_session_count: fn(&AppState) -> usize,
) {
    let pane_search_mode = state
        .active_pane_index
        .and_then(|i| state.panes.get(i))
        .is_some_and(|p| p.search_mode);

    if pane_search_mode {
        if let Some(idx) = state.active_pane_index
            && let Some(pane) = state.panes.get_mut(idx)
        {
            match key.code {
                KeyCode::Esc => {
                    pane.search_mode = false;
                    if pane.search_matches.is_empty() {
                        pane.search_input.clear();
                        if let Some((saved_scroll, saved_msg)) = pane.search_saved_scroll.take()
                        {
                            pane.scroll = saved_scroll;
                            pane.selected_message = saved_msg;
                        }
                    } else {
                        pane.search_saved_scroll = None;
                    }
                }
                KeyCode::Enter
                    if !pane.search_matches.is_empty() => {
                        if key.modifiers.contains(KeyModifiers::SHIFT) {
                            pane.search_current = pane
                                .search_current
                                .checked_sub(1)
                                .unwrap_or(pane.search_matches.len() - 1);
                        } else {
                            pane.search_current =
                                (pane.search_current + 1) % pane.search_matches.len();
                        }
                        pane.scroll = pane.search_matches[pane.search_current];
                        if let Some(msg_idx) = pane
                            .message_lines
                            .iter()
                            .rposition(|&(start, _)| start <= pane.scroll)
                        {
                            pane.selected_message = msg_idx;
                        }
                    }
                KeyCode::Backspace => {
                    pane.search_input.delete_back();
                    ui::update_pane_search_matches(pane);
                    pane.search_current = 0;
                    if let Some(&first) = pane.search_matches.first() {
                        pane.scroll = first;
                        if let Some(msg_idx) = pane
                            .message_lines
                            .iter()
                            .rposition(|&(start, _)| start <= first)
                        {
                            pane.selected_message = msg_idx;
                        }
                    }
                }
                KeyCode::Left => {
                    pane.search_input.move_left();
                }
                KeyCode::Right => {
                    pane.search_input.move_right();
                }
                KeyCode::Home => {
                    pane.search_input.move_home();
                }
                KeyCode::End => {
                    pane.search_input.move_end();
                }
                KeyCode::Char(c) => {
                    pane.search_input.insert_char(c);
                    ui::update_pane_search_matches(pane);
                    pane.search_current = 0;
                    if let Some(&first) = pane.search_matches.first() {
                        pane.scroll = first;
                        if let Some(msg_idx) = pane
                            .message_lines
                            .iter()
                            .rposition(|&(start, _)| start <= first)
                        {
                            pane.selected_message = msg_idx;
                        }
                    }
                }
                _ => {}
            }
        }
        return;
    }

    match key.code {
        KeyCode::Char('0') => {
            state.active_pane_index = None;
        }
        KeyCode::Char('1') | KeyCode::Char('2') | KeyCode::Char('3') | KeyCode::Char('4') => {
            let target_idx = match key.code {
                KeyCode::Char('1') => 0,
                KeyCode::Char('2') => 1,
                KeyCode::Char('3') => 2,
                KeyCode::Char('4') => 3,
                _ => unreachable!(),
            };
            if state.active_pane_index.is_none() {
                if let Some(group) = state.daily_groups.get(state.selected_day) {
                    let sessions: Vec<_> = group.user_sessions().collect();
                    if let Some(session) = sessions.get(state.selected_session) {
                        let new_pane = ConversationPane::load_from(&session.file_path);
                        while state.panes.len() <= target_idx {
                            state.panes.push(ConversationPane::default());
                        }
                        state.panes[target_idx] = new_pane;
                        state.active_pane_index = Some(target_idx);
                    }
                }
            } else if target_idx < state.panes.len() {
                state.active_pane_index = Some(target_idx);
            }
        }
        KeyCode::Char('Q') => {
            state.show_conversation = false;
            state.panes.clear();
            state.active_pane_index = None;
            state.conv_list_mode = ConvListMode::Day;
        }
        KeyCode::Char('T') => {
            state.session_list_hidden = !state.session_list_hidden;
        }
        KeyCode::Esc | KeyCode::Char('q') => {
            if state.show_detail {
                state.show_detail = false;
                return;
            }
            let has_search = state
                .active_pane_index
                .and_then(|i| state.panes.get(i))
                .is_some_and(|p| !p.search_input.text.is_empty());
            if has_search {
                if let Some(idx) = state.active_pane_index
                    && let Some(pane) = state.panes.get_mut(idx)
                {
                    pane.search_input.text.clear();
                    pane.search_input.cursor = 0;
                    pane.search_matches.clear();
                    pane.search_current = 0;
                    if let Some((saved_scroll, saved_msg)) = pane.search_saved_scroll.take() {
                        pane.scroll = saved_scroll;
                        pane.selected_message = saved_msg;
                    }
                }
            } else if state.search_preview_mode {
                state.show_conversation = false;
                for pane in &mut state.panes {
                    pane.clear();
                }
                state.active_pane_index = None;
                if let Some((tab, day, session, _)) = &state.search_saved_state {
                    state.tab = *tab;
                    state.selected_day = *day;
                    state.selected_session = *session;
                }
                state.search_mode = true;
                state.search_preview_mode = false;
            } else if state.active_pane_index.is_none() {
                if !state.panes.is_empty() {
                    state.active_pane_index = Some(0);
                } else {
                    state.show_conversation = false;
                    state.conv_list_mode = ConvListMode::Day;
                }
            } else if let Some(idx) = state.active_pane_index {
                state.panes.remove(idx);
                if state.panes.is_empty() {
                    state.show_conversation = false;
                    state.active_pane_index = None;
                    state.conv_list_mode = ConvListMode::Day;
                } else {
                    let new_idx = idx.min(state.panes.len() - 1);
                    state.active_pane_index = Some(new_idx);
                }
            }
        }
        KeyCode::Tab | KeyCode::Char('l') | KeyCode::Right => {
            let pane_count = state.panes.len();
            if pane_count > 0 {
                state.active_pane_index = match state.active_pane_index {
                    None => Some(0),
                    Some(idx) => {
                        if idx + 1 < pane_count {
                            Some(idx + 1)
                        } else {
                            None
                        }
                    }
                };
            }
        }
        KeyCode::Char('h') | KeyCode::Left => {
            let pane_count = state.panes.len();
            if pane_count > 0 {
                state.active_pane_index = match state.active_pane_index {
                    None => Some(pane_count - 1),
                    Some(idx) => {
                        if idx > 0 {
                            Some(idx - 1)
                        } else {
                            None
                        }
                    }
                };
            }
        }
        KeyCode::Char('/') => {
            if let Some(idx) = state.active_pane_index
                && let Some(pane) = state.panes.get_mut(idx)
            {
                pane.search_saved_scroll = Some((pane.scroll, pane.selected_message));
                pane.search_mode = true;
                pane.search_input.clear();
                pane.search_matches.clear();
                pane.search_current = 0;
            }
        }
        // j/k bindings live with the Down/Up arms below
        // so vim-style nav and arrow-key nav share one
        // implementation (and the help popup's
        // `j/k: Select message` actually works once a
        // pane is focused).
        KeyCode::Char('H')
            if state.conv_list_mode == ConvListMode::Day
                && state.selected_day < state.daily_groups.len().saturating_sub(1)
            => {
                state.selected_day += 1;
                state.selected_session = 0;
                if state.panes.len() == 1 {
                    preview_conversation_in_pane(state);
                }
            }
        KeyCode::Char('L')
            if state.conv_list_mode == ConvListMode::Day && state.selected_day > 0 => {
                state.selected_day -= 1;
                state.selected_session = 0;
                if state.panes.len() == 1 {
                    preview_conversation_in_pane(state);
                }
            }
        KeyCode::Char(' ') => {
            if state.show_detail {
                let fp = state
                    .active_pane_index
                    .and_then(|i| state.panes.get(i))
                    .and_then(|p| p.file_path.clone())
                    .or_else(|| get_conv_session_file(state, state.selected_session));
                if let Some(fp) = fp {
                    state.pins.toggle(&fp);
                    if let Err(e) = state.pins.save() {
                        state.toast_message = Some(format!("Pin save failed: {e}"));
                        state.toast_time = Some(std::time::Instant::now());
                    }
                    state.needs_draw = true;
                }
            } else if state.active_pane_index.is_none()
                && let Some(fp) = get_conv_session_file(state, state.selected_session)
            {
                state.pins.toggle(&fp);
                if let Err(e) = state.pins.save() {
                    state.toast_message = Some(format!("Pin save failed: {e}"));
                    state.toast_time = Some(std::time::Instant::now());
                }
                state.needs_draw = true;
            }
        }
        KeyCode::Char('m') => {
            // Toggle: Pinned <-> Day. Pressing `m` twice
            // returns to the daily view, matching the help
            // text's "m: pins" hint as a switch rather than
            // a one-way trip.
            state.conv_list_mode = match state.conv_list_mode {
                ConvListMode::Pinned => ConvListMode::Day,
                _ if state.pins.entries().is_empty() => state.conv_list_mode,
                _ => ConvListMode::Pinned,
            };
            state.selected_session = 0;
            state.active_pane_index = None;
            if state.panes.len() == 1 {
                preview_conversation_in_pane(state);
            }
        }
        KeyCode::Char('C') => {
            // Open the currently-selected session in a new side-by-side pane
            // (mirrors the default-mode handler so multi-pane is reachable
            // without escaping the conversation view first).
            let no_loading = state.panes.iter().all(|p| !p.loading);
            if no_loading
                && state.panes.len() < MAX_PANES
                && let Some(group) = state.daily_groups.get(state.selected_day)
            {
                let sessions: Vec<_> = group.user_sessions().collect();
                if let Some(session) = sessions.get(state.selected_session) {
                    state
                        .panes
                        .push(ConversationPane::load_from(&session.file_path));
                    state.active_pane_index = Some(state.panes.len() - 1);
                }
            }
        }
        KeyCode::BackTab
            if state.active_pane_index.is_none() => {
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
                if state.panes.len() == 1 {
                    preview_conversation_in_pane(state);
                }
            }
        KeyCode::Down | KeyCode::Char('j') => {
            if state.active_pane_index.is_none() {
                let max = get_conv_session_count(state).saturating_sub(1);
                if state.selected_session < max {
                    state.selected_session += 1;
                } else if state.conv_list_mode == ConvListMode::Day
                    && state.selected_day < state.daily_groups.len().saturating_sub(1)
                {
                    state.selected_day += 1;
                    state.selected_session = 0;
                }
                if state.panes.len() == 1 {
                    preview_conversation_in_pane(state);
                }
            } else if let Some(idx) = state.active_pane_index
                && let Some(pane) = state.panes.get_mut(idx)
            {
                let msg_count = pane.message_lines.len();
                if msg_count > 0 {
                    pane.selected_message =
                        pane.selected_message.saturating_add(1).min(msg_count - 1);
                }
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if state.active_pane_index.is_none() {
                if state.selected_session > 0 {
                    state.selected_session -= 1;
                } else if state.conv_list_mode == ConvListMode::Day && state.selected_day > 0 {
                    state.selected_day -= 1;
                    state.selected_session = get_conv_session_count(state).saturating_sub(1);
                }
                if state.panes.len() == 1 {
                    preview_conversation_in_pane(state);
                }
            } else if let Some(idx) = state.active_pane_index
                && let Some(pane) = state.panes.get_mut(idx)
                && pane.selected_message > 0
            {
                pane.selected_message -= 1;
            }
        }
        KeyCode::PageDown | KeyCode::Char('d') => {
            // Auto-focus the first pane so d/u work even
            // without pressing Tab/l first — the help text
            // implies these scroll the conversation, but
            // previously they silently no-op'd until a
            // pane was explicitly focused.
            if state.active_pane_index.is_none() && !state.panes.is_empty() {
                state.active_pane_index = Some(0);
            }
            if let Some(idx) = state.active_pane_index
                && let Some(pane) = state.panes.get_mut(idx)
            {
                pane.scroll = pane.scroll.saturating_add(20);
                if !pane.message_lines.is_empty() {
                    let msg_idx = pane
                        .message_lines
                        .iter()
                        .position(|&(start, _)| start >= pane.scroll)
                        .unwrap_or(pane.message_lines.len() - 1);
                    pane.selected_message = msg_idx;
                }
            }
        }
        KeyCode::PageUp | KeyCode::Char('u') => {
            if state.active_pane_index.is_none() && !state.panes.is_empty() {
                state.active_pane_index = Some(0);
            }
            if let Some(idx) = state.active_pane_index
                && let Some(pane) = state.panes.get_mut(idx)
            {
                pane.scroll = pane.scroll.saturating_sub(20);
                if !pane.message_lines.is_empty() {
                    let msg_idx = pane
                        .message_lines
                        .iter()
                        .rposition(|&(start, _)| start <= pane.scroll)
                        .unwrap_or(0);
                    pane.selected_message = msg_idx;
                }
            }
        }
        KeyCode::Home | KeyCode::Char('g') => {
            if let Some(idx) = state.active_pane_index
                && let Some(pane) = state.panes.get_mut(idx)
            {
                pane.scroll = 0;
                pane.selected_message = 0;
            }
        }
        KeyCode::End | KeyCode::Char('G') => {
            if let Some(idx) = state.active_pane_index
                && let Some(pane) = state.panes.get_mut(idx)
            {
                pane.scroll = usize::MAX;
                let msg_count = pane.message_lines.len();
                if msg_count > 0 {
                    pane.selected_message = msg_count - 1;
                }
            }
        }
        KeyCode::Char('n') => {
            if let Some(idx) = state.active_pane_index
                && let Some(pane) = state.panes.get_mut(idx)
            {
                if !pane.search_matches.is_empty() {
                    pane.search_current =
                        (pane.search_current + 1) % pane.search_matches.len();
                    pane.scroll = pane.search_matches[pane.search_current];
                    if let Some(msg_idx) = pane
                        .message_lines
                        .iter()
                        .rposition(|&(start, _)| start <= pane.scroll)
                    {
                        pane.selected_message = msg_idx;
                    }
                } else if let Some(&(next_pos, _)) = pane
                    .message_lines
                    .iter()
                    .find(|&&(pos, _)| pos > pane.scroll + 2)
                {
                    pane.scroll = next_pos;
                }
            }
        }
        KeyCode::Char('N') => {
            if let Some(idx) = state.active_pane_index
                && let Some(pane) = state.panes.get_mut(idx)
            {
                if !pane.search_matches.is_empty() {
                    pane.search_current = pane
                        .search_current
                        .checked_sub(1)
                        .unwrap_or(pane.search_matches.len() - 1);
                    pane.scroll = pane.search_matches[pane.search_current];
                    if let Some(msg_idx) = pane
                        .message_lines
                        .iter()
                        .rposition(|&(start, _)| start <= pane.scroll)
                    {
                        pane.selected_message = msg_idx;
                    }
                } else if let Some(&(prev_pos, _)) = pane
                    .message_lines
                    .iter()
                    .rev()
                    .find(|&&(pos, _)| pos + 2 < pane.scroll)
                {
                    pane.scroll = prev_pos;
                } else {
                    pane.scroll = 0;
                }
            }
        }
        KeyCode::Char('J') => {
            if let Some(idx) = state.active_pane_index
                && let Some(pane) = state.panes.get_mut(idx)
                && let Some(&(next_pos, _)) = pane
                    .message_lines
                    .iter()
                    .find(|&&(pos, _)| pos > pane.scroll + 2)
            {
                pane.scroll = next_pos;
            }
        }
        KeyCode::Char('K') => {
            if let Some(idx) = state.active_pane_index
                && let Some(pane) = state.panes.get_mut(idx)
            {
                if let Some(&(prev_pos, _)) = pane
                    .message_lines
                    .iter()
                    .rev()
                    .find(|&&(pos, _)| pos + 2 < pane.scroll)
                {
                    pane.scroll = prev_pos;
                } else {
                    pane.scroll = 0;
                }
            }
        }
        KeyCode::Char('y') => {
            if let Some(idx) = state.active_pane_index
                && let Some(pane) = state.panes.get_mut(idx)
                && let Some(&(_, msg_idx)) = pane.message_lines.get(pane.selected_message)
                && let Some(msg) = pane.messages.get(msg_idx)
            {
                let content = ui::extract_message_text(msg);
                let len = content.chars().count();
                state.toast_message = Some(format!("Copied ({len} chars)"));
                state.toast_time = Some(std::time::Instant::now());
                crate::handlers::tasks::spawn_clipboard_write(content);
            }
        }
        KeyCode::Char('i') => {
            state.show_detail = !state.show_detail;
            if state.show_detail {
                state.session_detail_scroll = 0;
            }
        }
        KeyCode::Enter
            if state.active_pane_index.is_none() => {
                open_conversation_in_pane(state);
            }
        _ => {}
    }
}

/// Default branch (no popup, no search-mode, no conversation pane). Handles
/// global keys: tab switching, panel nav, summary/regen triggers, Daily-tab
/// session list nav. Returns `true` when the second `q` press confirms quit
/// — the caller breaks the event loop.
pub(crate) fn handle_default_key(
    state: &mut AppState,
    key: KeyEvent,
    preview_conversation_in_pane: fn(&mut AppState),
    open_conversation_in_pane: fn(&mut AppState),
) -> bool {
    match key.code {
        KeyCode::Char('q') => {
            if state.ctrl_c_pressed {
                return true;
            }
            state.ctrl_c_pressed = true;
            state.toast_message = Some("Press q again to quit".to_string());
            state.toast_time = Some(std::time::Instant::now());
            state.needs_draw = true;
        }
        KeyCode::Esc
            if state.daily_breakdown_focus => {
                state.daily_breakdown_focus = false;
                state.daily_breakdown_scroll = 0;
            }
        KeyCode::Char('x')
            if state.retention_warning.is_some() && !state.retention_warning_dismissed => {
                state.retention_warning_dismissed = true;
            }
        KeyCode::Char('?') => {
            state.show_help = true;
        }
        KeyCode::Char('f') => {
            state.show_filter_popup = true;
            state.filter_popup_selected =
                if matches!(state.period_filter, PeriodFilter::Custom(_, _)) {
                    PeriodFilter::ALL_VARIANTS.len()
                } else {
                    PeriodFilter::ALL_VARIANTS
                        .iter()
                        .position(|&v| v == state.period_filter)
                        .unwrap_or(0)
                };
        }
        KeyCode::Char('p') => {
            state.show_project_popup = true;
            state.project_popup_selected = match &state.project_filter {
                Some(name) => state
                    .project_list
                    .iter()
                    .position(|(n, _, _)| n == name)
                    .map_or(0, |i| i + 1),
                None => 0,
            };
            state.project_popup_scroll = 0;
        }
        KeyCode::Char(' ') => {
            if state.tab == Tab::Daily
                && !state.show_conversation
                && let Some(group) = state.daily_groups.get(state.selected_day)
            {
                let sessions: Vec<_> = group.user_sessions().collect();
                if let Some(session) = sessions.get(state.selected_session) {
                    state.pins.toggle(&session.file_path);
                    if let Err(e) = state.pins.save() {
                        state.toast_message = Some(format!("Pin save failed: {e}"));
                        state.toast_time = Some(std::time::Instant::now());
                    }
                    state.needs_draw = true;
                }
            }
        }
        KeyCode::Char('m')
            if !state.pins.entries().is_empty() => {
                state.conv_list_mode = ConvListMode::Pinned;
                state.selected_session = 0;
                state.tab = Tab::Daily;
                if !state.show_conversation {
                    state.panes.clear();
                    state.panes.push(ConversationPane::default());
                    state.show_conversation = true;
                }
                state.active_pane_index = None;
                if state.panes.len() == 1 {
                    preview_conversation_in_pane(state);
                }
            }
        KeyCode::Char('/') => {
            state.search_mode = true;
            state.search_input.move_end();
        }
        KeyCode::Tab | KeyCode::Char('1') | KeyCode::Char('2') | KeyCode::Char('3') => {
            if state.show_summary || state.generating_summary {
                state.clear_summary();
            }
            state.show_dashboard_detail = false;
            state.show_insights_detail = false;
            state.show_detail = false;
            state.daily_breakdown_focus = false;
            state.daily_breakdown_scroll = 0;
            state.tab = match key.code {
                KeyCode::Char('1') => Tab::Dashboard,
                KeyCode::Char('2') => Tab::Daily,
                KeyCode::Char('3') => Tab::Insights,
                KeyCode::Tab => match state.tab {
                    Tab::Dashboard => Tab::Daily,
                    Tab::Daily => Tab::Insights,
                    Tab::Insights => Tab::Dashboard,
                },
                _ => state.tab,
            };
        }
        KeyCode::Left | KeyCode::Char('h') => {
            if state.tab == Tab::Dashboard {
                state.dashboard_panel = if state.dashboard_panel == 0 {
                    6
                } else {
                    state.dashboard_panel - 1
                };
            } else if state.tab == Tab::Daily
                && state.selected_day < state.daily_groups.len().saturating_sub(1)
            {
                state.selected_day += 1;
                state.selected_session = 0;
                state.daily_breakdown_scroll = 0;
            } else if state.tab == Tab::Insights {
                state.insights_panel = if state.insights_panel == 0 {
                    3
                } else {
                    state.insights_panel - 1
                };
                state.insights_detail_scroll = 0;
            }
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if state.tab == Tab::Dashboard {
                state.dashboard_panel = if state.dashboard_panel >= 6 {
                    0
                } else {
                    state.dashboard_panel + 1
                };
            } else if state.tab == Tab::Daily && state.selected_day > 0 {
                state.selected_day -= 1;
                state.selected_session = 0;
                state.daily_breakdown_scroll = 0;
            } else if state.tab == Tab::Insights {
                state.insights_panel = if state.insights_panel >= 3 {
                    0
                } else {
                    state.insights_panel + 1
                };
                state.insights_detail_scroll = 0;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if state.tab == Tab::Dashboard {
                // Panel 5 (heatmap) uses inverted scroll: older dates are at top
                if state.dashboard_panel == 5 {
                    let scroll = &mut state.dashboard_scroll[5];
                    *scroll = scroll.saturating_add(1);
                } else {
                    let scroll = &mut state.dashboard_scroll[state.dashboard_panel];
                    if *scroll > 0 {
                        *scroll -= 1;
                    }
                }
            } else if state.tab == Tab::Daily {
                if state.daily_breakdown_focus {
                    if state.daily_breakdown_scroll > 0 {
                        state.daily_breakdown_scroll -= 1;
                    }
                } else if state.selected_session > 0 {
                    state.selected_session -= 1;
                }
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if state.tab == Tab::Dashboard {
                if state.dashboard_panel == 5 {
                    let scroll = &mut state.dashboard_scroll[5];
                    *scroll = scroll.saturating_sub(1);
                } else {
                    let max_items = crate::dashboard_max_items(state);
                    let scroll = &mut state.dashboard_scroll[state.dashboard_panel];
                    if *scroll + 1 < max_items {
                        *scroll += 1;
                    }
                }
            } else if state.tab == Tab::Daily {
                if state.daily_breakdown_focus {
                    if state.daily_breakdown_scroll < state.daily_breakdown_max_scroll {
                        state.daily_breakdown_scroll += 1;
                    }
                } else {
                    let max = state
                        .daily_groups
                        .get(state.selected_day)
                        .map_or(0, |g| g.user_sessions().count().saturating_sub(1));
                    if state.selected_session < max {
                        state.selected_session += 1;
                    }
                }
            }
        }
        KeyCode::Enter
            if key.modifiers.contains(KeyModifiers::SHIFT)
                && state.tab == Tab::Daily
                && state.panes.iter().all(|p| !p.loading)
                && state.panes.len() < MAX_PANES =>
        {
            if let Some(group) = state.daily_groups.get(state.selected_day) {
                let sessions: Vec<_> = group.user_sessions().collect();
                if let Some(session) = sessions.get(state.selected_session) {
                    state
                        .panes
                        .push(ConversationPane::load_from(&session.file_path));
                    state.active_pane_index = Some(state.panes.len() - 1);
                    state.conv_list_mode = ConvListMode::Day;
                    state.show_conversation = true;
                }
            }
        }
        KeyCode::Enter => {
            if state.tab == Tab::Daily {
                if state.daily_breakdown_focus {
                    state.daily_breakdown_focus = false;
                    state.daily_breakdown_scroll = 0;
                }
                open_conversation_in_pane(state);
            } else if state.tab == Tab::Dashboard {
                state.show_dashboard_detail = true;
            } else if state.tab == Tab::Insights {
                state.show_insights_detail = true;
                state.insights_detail_scroll = 0;
            }
        }
        KeyCode::Char('C') => {
            let no_loading = state.panes.iter().all(|p| !p.loading);
            if state.tab == Tab::Daily
                && no_loading
                && state.panes.len() < MAX_PANES
                && let Some(group) = state.daily_groups.get(state.selected_day)
            {
                let sessions: Vec<_> = group.user_sessions().collect();
                if let Some(session) = sessions.get(state.selected_session) {
                    state
                        .panes
                        .push(ConversationPane::load_from(&session.file_path));
                    state.active_pane_index = Some(state.panes.len() - 1);
                    state.conv_list_mode = ConvListMode::Day;
                    state.show_conversation = true;
                }
            }
        }
        KeyCode::Char('S') => {
            if state.tab == Tab::Daily
                && state.summary_task.is_none()
                && let Some(group) = state.daily_groups.get(state.selected_day).cloned()
            {
                handlers::tasks::start_day_summary(state, group, false);
            }
        }
        KeyCode::Char('s') => {
            if state.tab == Tab::Daily
                && state.summary_task.is_none()
                && let Some(session) = crate::current_selected_session(state)
            {
                handlers::tasks::start_session_summary(state, session, false);
            }
        }
        KeyCode::Char('r') => {
            if state.tab == Tab::Daily
                && state.summary_task.is_none()
                && let Some(session) = crate::current_selected_session(state)
            {
                handlers::tasks::start_session_summary(state, session, true);
            }
        }
        KeyCode::Char('R')
            if state.tab == Tab::Daily => {
                let selected_day = state.selected_day;
                let selected_session = state.selected_session;
                if let Some((actual_idx, session)) =
                    crate::current_selected_session_with_index(state)
                    && state.updating_task.is_none()
                {
                    handlers::tasks::start_jsonl_regen(
                        state,
                        session,
                        selected_day,
                        selected_session,
                        actual_idx,
                    );
                }
            }
        KeyCode::Char('b')
            if state.tab == Tab::Daily => {
                state.daily_breakdown_focus = !state.daily_breakdown_focus;
                if state.daily_breakdown_focus {
                    state.daily_breakdown_scroll = 0;
                }
            }
        KeyCode::Char('t')
            if state.tab == Tab::Daily && !state.daily_groups.is_empty() => {
                state.selected_day = 0;
                state.selected_session = 0;
            }
        KeyCode::Char('i') => {
            if state.tab == Tab::Daily {
                state.show_detail = true;
                state.session_detail_scroll = 0;
            } else if state.tab == Tab::Insights {
                state.show_insights_detail = true;
                state.insights_detail_scroll = 0;
            }
        }
        _ => {}
    }
    false
}

/// `state.show_dashboard_detail == true` branch — Tools detail popup with
/// MCP-tab-specific cursor logic + cross-section/cross-panel navigation.
/// MCP tab uses row-based cursor (`mcp_selected_server` indexes the sorted
/// server list; `mcp_selected_tool` is `Option<usize>` for tool index within
/// the selected server). Enter/Space toggles expansion only on the header row.
pub(crate) fn handle_dashboard_detail_key(state: &mut AppState, key: KeyEvent) {
    // After the Built-in/MCP merge, MCP server-grouped rendering lives inside
    // the Tools tab (index 0). MCP-specific keys (Enter/Space/o/c, j/k cursor)
    // still target MCP rows there — built-in rows above them are non-navigable.
    let mcp_tab_active = state.dashboard_panel == 3 && state.tools_detail_section == 0;
    match key.code {
        KeyCode::Enter | KeyCode::Char(' ')
            if mcp_tab_active && state.mcp_selected_tool.is_none() =>
        {
            let servers = crate::collect_mcp_servers(state);
            if let Some(name) = servers.get(state.mcp_selected_server) {
                if state.mcp_expanded_servers.contains(name) {
                    state.mcp_expanded_servers.remove(name);
                } else {
                    state.mcp_expanded_servers.insert(name.clone());
                }
            }
        }
        KeyCode::Char('o') if mcp_tab_active => {
            let servers = crate::collect_mcp_servers(state);
            state.mcp_expanded_servers = servers.into_iter().collect();
        }
        KeyCode::Char('c') if mcp_tab_active => {
            state.mcp_expanded_servers.clear();
            state.mcp_selected_tool = None;
        }
        KeyCode::Down | KeyCode::Char('j') if mcp_tab_active => {
            let servers = crate::collect_mcp_servers(state);
            let max_server = servers.len().saturating_sub(1);
            let cur_server = servers.get(state.mcp_selected_server);
            let cur_tool_count = cur_server.map_or(0, |s| crate::mcp_tool_count(state, s));
            let cur_expanded =
                cur_server.is_some_and(|s| state.mcp_expanded_servers.contains(s));
            match state.mcp_selected_tool {
                None if cur_expanded && cur_tool_count > 0 => {
                    state.mcp_selected_tool = Some(0);
                }
                Some(t) if t + 1 < cur_tool_count => {
                    state.mcp_selected_tool = Some(t + 1);
                }
                _ => {
                    if state.mcp_selected_server < max_server {
                        state.mcp_selected_server += 1;
                        state.mcp_selected_tool = None;
                    }
                }
            }
            crate::adjust_mcp_scroll(state, &servers);
        }
        KeyCode::Up | KeyCode::Char('k') if mcp_tab_active => {
            let servers = crate::collect_mcp_servers(state);
            match state.mcp_selected_tool {
                Some(0) => {
                    state.mcp_selected_tool = None;
                }
                Some(t) => {
                    state.mcp_selected_tool = Some(t - 1);
                }
                None => {
                    if state.mcp_selected_server > 0 {
                        state.mcp_selected_server -= 1;
                        let new_server = servers.get(state.mcp_selected_server);
                        let expanded = new_server
                            .is_some_and(|s| state.mcp_expanded_servers.contains(s));
                        let count =
                            new_server.map_or(0, |s| crate::mcp_tool_count(state, s));
                        state.mcp_selected_tool =
                            if expanded && count > 0 { Some(count - 1) } else { None };
                    }
                }
            }
            crate::adjust_mcp_scroll(state, &servers);
        }
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
            state.show_dashboard_detail = false;
        }
        KeyCode::Up | KeyCode::Char('k')
            if state.dashboard_scroll[state.dashboard_panel] > 0 => {
                state.dashboard_scroll[state.dashboard_panel] -= 1;
            }
        KeyCode::Down | KeyCode::Char('j') => {
            let max_items = crate::dashboard_max_items(state);
            let scroll = &mut state.dashboard_scroll[state.dashboard_panel];
            if *scroll + 1 < max_items {
                *scroll += 1;
            }
        }
        KeyCode::PageUp | KeyCode::Char('u') => {
            let scroll = &mut state.dashboard_scroll[state.dashboard_panel];
            *scroll = scroll.saturating_sub(10);
        }
        KeyCode::PageDown | KeyCode::Char('d') => {
            let max_items = crate::dashboard_max_items(state);
            let scroll = &mut state.dashboard_scroll[state.dashboard_panel];
            *scroll = scroll.saturating_add(10).min(max_items.saturating_sub(1));
        }
        KeyCode::Home | KeyCode::Char('g') => {
            state.dashboard_scroll[state.dashboard_panel] = 0;
        }
        KeyCode::End | KeyCode::Char('G') => {
            let max_items = crate::dashboard_max_items(state);
            state.dashboard_scroll[state.dashboard_panel] = max_items.saturating_sub(1);
        }
        KeyCode::Char(c @ '1'..='4') if state.dashboard_panel == 3 => {
            state.tools_detail_section = (c as u8 - b'1') as usize;
            state.dashboard_scroll[state.dashboard_panel] = 0;
            if state.tools_detail_section == 0 {
                state.mcp_selected_server = 0;
                state.mcp_selected_tool = None;
            }
        }
        // Tab/BackTab + ←/→ + h/l all cycle the Tool Usage section.
        // Forward (Tab / Right / l) advances; backward (BackTab /
        // Left / h) retreats. Mirrors number keys 1-4.
        KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') if state.dashboard_panel == 3 => {
            state.tools_detail_section = (state.tools_detail_section + 1) % 4;
            state.dashboard_scroll[state.dashboard_panel] = 0;
            if state.tools_detail_section == 0 {
                state.mcp_selected_server = 0;
                state.mcp_selected_tool = None;
            }
        }
        KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h')
            if state.dashboard_panel == 3 =>
        {
            state.tools_detail_section = (state.tools_detail_section + 3) % 4;
            state.dashboard_scroll[state.dashboard_panel] = 0;
            if state.tools_detail_section == 0 {
                state.mcp_selected_server = 0;
                state.mcp_selected_tool = None;
            }
        }
        KeyCode::Left | KeyCode::Char('h') => {
            state.dashboard_panel = if state.dashboard_panel == 0 {
                6
            } else {
                state.dashboard_panel - 1
            };
        }
        KeyCode::Right | KeyCode::Char('l') => {
            state.dashboard_panel = if state.dashboard_panel >= 6 {
                0
            } else {
                state.dashboard_panel + 1
            };
        }
        _ => {}
    }
}

/// `state.show_summary == true` branch — Esc/q close the popup, ↑↓/j/k scroll
/// the body, `r` re-runs the summary generation.
pub(crate) fn handle_summary_popup_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            state.clear_summary();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.summary_scroll = state.summary_scroll.saturating_add(1);
        }
        KeyCode::Up | KeyCode::Char('k') => {
            state.summary_scroll = state.summary_scroll.saturating_sub(1);
        }
        KeyCode::Char('r') => {
            if state.summary_task.is_none()
                && let Some(summary_type) = state.summary_type.clone()
            {
                match summary_type {
                    SummaryType::Session(session) => {
                        handlers::tasks::start_session_summary(state, *session, true);
                    }
                    SummaryType::Day(group) => {
                        handlers::tasks::start_day_summary(state, group, true);
                    }
                }
            }
        }
        _ => {}
    }
}

/// `state.search_mode == true` branch — global search popup. Esc cancels and
/// restores the saved tab/day/session, Enter jumps to the selected hit (and
/// optionally activates the in-pane content search). Char keys edit the input
/// and re-run the search incrementally.
///
/// Takes two callbacks because the original branch invokes
/// `start_content_search(state)` and `open_conversation_in_pane(state)` —
/// both live in `main.rs` and can't be moved without churn. They are passed
/// in as `fn(&mut AppState)` pointers.
pub(crate) fn handle_search_mode_key(
    state: &mut AppState,
    key: KeyEvent,
    start_content_search: fn(&mut AppState),
    open_conversation_in_pane: fn(&mut AppState),
) {
    match key.code {
        KeyCode::Esc => {
            state.search_mode = false;
            state.search_input.clear();
            state.search_results.clear();
            state.search_selected = 0;
            state.search_task = None;
            state.searching = false;
            if let Some((tab, day, session, show_conv)) = state.search_saved_state.take() {
                state.tab = tab;
                state.selected_day = day;
                state.selected_session = session;
                state.show_conversation = show_conv;
            }
            state.search_preview_mode = false;
        }
        KeyCode::Enter
            if !state.search_results.is_empty() => {
                let result = state.search_results[state.search_selected].clone();
                let query = state.search_input.text.clone();
                let is_content =
                    matches!(result.match_type, search::SearchMatchType::Content);
                if state.search_saved_state.is_none() {
                    state.search_saved_state = Some((
                        state.tab,
                        state.selected_day,
                        state.selected_session,
                        state.show_conversation,
                    ));
                }
                state.selected_day = result.day_idx;
                state.selected_session = result.session_idx;
                state.tab = Tab::Daily;
                state.search_mode = false;
                state.search_preview_mode = true;
                state.search_task = None;
                state.searching = false;
                open_conversation_in_pane(state);
                if is_content
                    && let Some(idx) = state.active_pane_index
                    && let Some(pane) = state.panes.get_mut(idx)
                {
                    pane.search_input.set(query);
                    pane.search_mode = true;
                }
            }
        KeyCode::Down
            if !state.search_results.is_empty() => {
                state.search_selected =
                    (state.search_selected + 1) % state.search_results.len();
            }
        KeyCode::Up
            if !state.search_results.is_empty() => {
                state.search_selected = state
                    .search_selected
                    .checked_sub(1)
                    .unwrap_or(state.search_results.len() - 1);
            }
        KeyCode::Backspace => {
            state.search_input.delete_back();
            state.search_results =
                search::perform_search(&state.daily_groups, &state.search_input.text);
            state.search_selected = 0;
            start_content_search(state);
        }
        KeyCode::Left => {
            state.search_input.move_left();
        }
        KeyCode::Right => {
            state.search_input.move_right();
        }
        KeyCode::Home => {
            state.search_input.move_home();
        }
        KeyCode::End => {
            state.search_input.move_end();
        }
        KeyCode::Char(c) => {
            state.search_input.insert_char(c);
            state.search_results =
                search::perform_search(&state.daily_groups, &state.search_input.text);
            state.search_selected = 0;
            start_content_search(state);
        }
        _ => {}
    }
}

/// `state.show_project_popup == true` branch — Esc/q/p close, ↑↓/j/k navigate,
/// Enter applies the project filter (idx 0 = "All").
pub(crate) fn handle_project_popup_key(state: &mut AppState, key: KeyEvent) {
    let total = state.project_list.len() + 1;
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('p') => {
            state.show_project_popup = false;
        }
        KeyCode::Up | KeyCode::Char('k')
            if state.project_popup_selected > 0 => {
                state.project_popup_selected -= 1;
            }
        KeyCode::Down | KeyCode::Char('j')
            if state.project_popup_selected < total - 1 => {
                state.project_popup_selected += 1;
            }
        KeyCode::Enter => {
            if state.project_popup_selected == 0 {
                state.project_filter = None;
            } else if let Some((name, _, _)) =
                state.project_list.get(state.project_popup_selected - 1)
            {
                state.project_filter = Some(name.clone());
            }
            state.apply_filter();
            state.show_project_popup = false;
        }
        _ => {}
    }
}
