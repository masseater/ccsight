//! Conversation-pane helpers: load conversation messages off-thread, look up
//! the file path / count of the currently visible session, open/preview a
//! conversation in a pane, plus text-selection extraction shared by the mouse
//! Up handler and the Session Detail popup.

use std::sync::mpsc;

use crate::aggregator::DailyGroup;
use crate::state::{ConvListMode, ConversationPane, MAX_PANES};
use crate::{AppState, ConversationMessage, ui};

pub(crate) fn spawn_load_conversation(
    file_path: &std::path::Path,
) -> mpsc::Receiver<Vec<ConversationMessage>> {
    let fp = file_path.to_path_buf();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let messages = ui::load_conversation(&fp).unwrap_or_default();
        let _ = tx.send(messages);
    });
    rx
}

/// Owned clone of the user-session currently highlighted in the Daily tab —
/// the value most "summary" / "regen" key bindings need. Returns `None` if
/// the selection points outside the visible (subagent-filtered) list.
pub(crate) fn current_selected_session(state: &AppState) -> Option<crate::aggregator::SessionInfo> {
    state
        .daily_groups
        .get(state.selected_day)
        .and_then(|g| g.user_sessions().nth(state.selected_session))
        .cloned()
}

/// Same as [`current_selected_session`] but also returns the **raw index** into
/// `group.sessions` (subagent-inclusive). Needed by `R` (JSONL regen) so the
/// task helper can splice the result back into the unfiltered slot.
pub(crate) fn current_selected_session_with_index(
    state: &AppState,
) -> Option<(usize, crate::aggregator::SessionInfo)> {
    let group = state.daily_groups.get(state.selected_day)?;
    let raw_idx = group
        .sessions
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.is_subagent)
        .nth(state.selected_session)
        .map(|(i, _)| i)?;
    Some((raw_idx, group.sessions[raw_idx].clone()))
}

pub(crate) fn get_conv_session_file(state: &AppState, idx: usize) -> Option<std::path::PathBuf> {
    match state.conv_list_mode {
        ConvListMode::Day => state
            .daily_groups
            .get(state.selected_day)
            .and_then(|g| g.user_sessions().nth(idx))
            .map(|s| s.file_path.clone()),
        ConvListMode::Pinned => state.pins.entries().get(idx).map(|e| e.path.clone()),
        ConvListMode::All => state
            .original_daily_groups
            .iter()
            .flat_map(DailyGroup::user_sessions)
            .nth(idx)
            .map(|s| s.file_path.clone()),
        ConvListMode::Live => {
            // Same active+paused order as the Live tab's list.
            if idx < state.live_active.len() {
                state.live_active[idx].jsonl_path.clone()
            } else {
                state
                    .live_paused
                    .get(idx - state.live_active.len())
                    .and_then(|s| s.jsonl_path.clone())
            }
        }
    }
}

pub(crate) fn get_conv_session_count(state: &AppState) -> usize {
    match state.conv_list_mode {
        ConvListMode::Day => state
            .daily_groups
            .get(state.selected_day)
            .map_or(0, |g| g.user_sessions().count()),
        ConvListMode::Pinned => state.pins.entries().len(),
        ConvListMode::All => state
            .original_daily_groups
            .iter()
            .flat_map(DailyGroup::user_sessions)
            .count(),
        ConvListMode::Live => crate::live_visible_count(state),
    }
}

pub(crate) fn preview_conversation_in_pane(state: &mut AppState) {
    let saved = state.active_pane_index;
    open_conversation_in_pane(state);
    state.active_pane_index = saved;
}

pub(crate) fn open_conversation_in_pane(state: &mut AppState) {
    let no_loading = state.panes.iter().all(|p| !p.loading);
    if !no_loading || state.panes.len() >= MAX_PANES {
        return;
    }

    let Some(file_path) = get_conv_session_file(state, state.selected_session) else {
        return;
    };

    let target_idx = state
        .panes
        .iter()
        .position(|p| p.file_path.is_none())
        .unwrap_or_else(|| state.active_pane_index.unwrap_or(0));

    let new_pane = ConversationPane::load_from(&file_path);
    if target_idx < state.panes.len() {
        state.panes[target_idx] = new_pane;
    } else {
        state.panes.push(new_pane);
    }

    state.active_pane_index = Some(target_idx);
    state.show_conversation = true;
}

/// Extract text from the rendered terminal buffer for a rectangular mouse
/// selection. When `conv_area` is `Some`, the selection is clamped to that
/// rect (Conversation pane / popup); when `wrap_flags` is also `Some`, the
/// extraction joins wrapped-continuation rows back into single logical lines.
pub(crate) fn extract_selected_text_from_buffer(
    sel: &(u16, u16, u16, u16),
    buffer: &ratatui::buffer::Buffer,
    conv_area: Option<ratatui::layout::Rect>,
    wrap_flags: Option<&[bool]>,
    conv_scroll: usize,
) -> String {
    let (sc, sr, ec, er) = *sel;
    let buf_area = buffer.area;

    let (start_col, start_row, end_col, end_row) = if (sr, sc) <= (er, ec) {
        (sc, sr, ec, er)
    } else {
        (ec, er, sc, sr)
    };

    let clamp = conv_area.filter(|ca| {
        start_row >= ca.y
            && start_row < ca.y + ca.height
            && start_col >= ca.x
            && start_col < ca.x + ca.width
    });

    let mut lines: Vec<String> = Vec::new();
    let mut line_rows: Vec<u16> = Vec::new();
    for row in start_row..=end_row {
        if row < buf_area.y || row >= buf_area.y + buf_area.height {
            continue;
        }
        if let Some(ca) = clamp
            && (row < ca.y || row >= ca.y + ca.height)
        {
            continue;
        }
        let col_start = if row == start_row {
            start_col
        } else {
            clamp.map_or(buf_area.x, |ca| ca.x)
        };
        let col_end = if row == end_row {
            end_col
        } else {
            clamp.map_or(buf_area.x + buf_area.width - 1, |ca| ca.x + ca.width - 1)
        };

        let mut line = String::new();
        let mut col = col_start;
        let mut skip_next = false;
        while col <= col_end && col < buf_area.x + buf_area.width {
            if skip_next {
                skip_next = false;
                col += 1;
                continue;
            }
            let cell = &buffer[(col, row)];
            let sym = cell.symbol();
            line.push_str(sym);
            if sym
                .chars()
                .next()
                .is_some_and(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) > 1)
            {
                skip_next = true;
            }
            col += 1;
        }
        lines.push(line.trim_end().to_string());
        line_rows.push(row);
    }

    while lines.last().is_some_and(std::string::String::is_empty) {
        lines.pop();
        line_rows.pop();
    }

    // If we have wrap_flags, we are copying from the conversation view — apply the
    // conversation-specific cleanup (strip `▶ ` marker, merge wrapped-continuation rows).
    // For any other clamped selection (e.g., the Session Detail popup), preserve the
    // literal rendered layout so multi-line commands like the resume snippet keep their
    // `\<newline>` continuation intact.
    if let (Some(ca), Some(flags)) = (clamp, wrap_flags)
        && !lines.is_empty()
    {
        let continuation_flags: Vec<bool> = line_rows
            .iter()
            .map(|&row| {
                let flag_idx = conv_scroll + (row - ca.y) as usize;
                flags.get(flag_idx).copied().unwrap_or(false)
            })
            .collect();
        return join_conversation_lines(&lines, &continuation_flags);
    }

    lines.join("\n")
}

pub(crate) fn join_conversation_lines(lines: &[String], wrap_continuation: &[bool]) -> String {
    if lines.is_empty() {
        return String::new();
    }

    let strip_prefix = |s: &str| -> String {
        if let Some(stripped) = s.strip_prefix("▶ ") {
            stripped.to_string()
        } else if let Some(stripped) = s.strip_prefix("  ") {
            stripped.to_string()
        } else {
            s.to_string()
        }
    };

    let mut result = String::new();
    let mut i = 0;

    while i < lines.len() {
        let stripped = strip_prefix(&lines[i]);
        result.push_str(&stripped);

        if i + 1 < lines.len() {
            let next_is_continuation = wrap_continuation.get(i + 1).copied().unwrap_or(false);
            if next_is_continuation {
                let next_stripped = strip_prefix(&lines[i + 1]);
                if !next_stripped.is_empty() {
                    result.push(' ');
                } else {
                    result.push('\n');
                }
            } else {
                result.push('\n');
            }
        }

        i += 1;
    }

    result
}
