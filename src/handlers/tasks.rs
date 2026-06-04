//! Background-task spawn helpers extracted from inline `thread::spawn` blocks
//! in `main.rs::run`. Each helper:
//! 1. Sets the relevant `state.*_task = Some(rx)` flag so the main loop polls it
//! 2. Captures the cloned session/day data into a `move ||` closure
//! 3. Sends the result back through an `mpsc::channel`
//!
//! Callers must verify the precondition (e.g. `state.summary_task.is_none()`)
//! before invoking — these helpers do not check, they just start.

use std::sync::mpsc;
use std::thread;

use crate::aggregator::{DailyGroup, SessionInfo};
use crate::state::SummaryType;
use crate::{AppState, summary};

/// Detached clipboard write — `arboard::Clipboard::set_text` is
/// synchronous and can stall multi-second on macOS NSPasteboard
/// contention, freezing the event loop. Returns a receiver so the caller
/// can replace the optimistic "Copied" toast on failure.
pub(crate) fn spawn_clipboard_write(text: String) -> mpsc::Receiver<Result<(), String>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = match arboard::Clipboard::new() {
            Ok(mut clipboard) => clipboard.set_text(&text).map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        let _ = tx.send(result);
    });
    rx
}

/// Spawn an AI session-summary task (generate or regenerate) and stash the
/// receiver in `state.summary_task`. Also flips the `generating_summary` /
/// `show_summary` / `summary_type` UI flags so the popup is shown immediately.
pub(crate) fn start_session_summary(state: &mut AppState, session: SessionInfo, regenerate: bool) {
    state.generating_summary = true;
    state.show_summary = true;
    state.show_detail = false;
    state.summary_type = Some(SummaryType::Session(Box::new(session.clone())));
    let summary_date = state.daily_groups.get(state.selected_day).map(|g| g.date);
    let (tx, rx) = mpsc::channel();
    state.summary_task = Some(rx);
    thread::spawn(move || {
        let result = if regenerate {
            summary::regenerate_session_summary(&session, summary_date)
        } else {
            summary::generate_session_summary(&session, summary_date)
        };
        let _ = tx.send(result);
    });
}

/// Spawn an AI day-summary task (generate or regenerate). Same UI-flag side
/// effects as [`start_session_summary`] but for `SummaryType::Day`.
pub(crate) fn start_day_summary(state: &mut AppState, group: DailyGroup, regenerate: bool) {
    state.generating_summary = true;
    state.show_summary = true;
    state.show_detail = false;
    state.summary_type = Some(SummaryType::Day(group.clone()));
    let (tx, rx) = mpsc::channel();
    state.summary_task = Some(rx);
    thread::spawn(move || {
        let result = if regenerate {
            summary::regenerate_day_summary(&group)
        } else {
            summary::generate_day_summary(&group)
        };
        let _ = tx.send(result);
    });
}

/// Spawn the JSONL-summary regeneration task triggered by `R` in Daily / Session
/// detail popups. Records `(day, session)` indices in `state.updating_session`
/// so the main loop can splice the result back into the right place.
pub(crate) fn start_jsonl_regen(
    state: &mut AppState,
    session: SessionInfo,
    day: usize,
    sess: usize,
    actual_idx: usize,
) {
    let file_path = session.file_path.clone();
    let (tx, rx) = mpsc::channel();
    state.updating_session = Some((day, sess));
    state.updating_task = Some((rx, file_path, day, sess, actual_idx));
    thread::spawn(move || {
        let result = summary::regenerate_jsonl_summary(&session);
        let _ = tx.send(result);
    });
}
