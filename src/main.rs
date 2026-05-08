#![allow(clippy::let_underscore_must_use)]

mod aggregator;
mod cli;
mod conversation;
mod domain;
mod handlers;
mod infrastructure;
mod mcp;
mod parser;
mod pins;
mod search;
mod state;
mod summary;
#[cfg(test)]
mod test_helpers;
mod text;
mod ui;

pub use state::*;

pub use conversation::{ConversationBlock, ConversationMessage};

// Re-export pane helpers so callers (and existing fn-pointer dispatch in
// `handlers::keyboard`) can keep using bare names.
pub(crate) use handlers::pane::{
    current_selected_session, current_selected_session_with_index,
    extract_selected_text_from_buffer, get_conv_session_count, get_conv_session_file,
    open_conversation_in_pane, preview_conversation_in_pane, spawn_load_conversation,
};
pub(crate) use handlers::mcp_popup::{adjust_mcp_scroll, collect_mcp_servers, mcp_tool_count};
// `join_conversation_lines` is referenced from `main_tests.rs` only.
#[cfg(test)]
pub(crate) use handlers::pane::join_conversation_lines;

pub(crate) const SUMMARY_MODEL: &str = "claude-haiku-4-5-20251001";

use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use chrono::{Local, NaiveDate};
use clap::Parser;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEventKind, MouseEventKind,
    },
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, EndSynchronizedUpdate,
        EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use ratatui::DefaultTerminal;

use crate::aggregator::{CostCalculator, DailyGroup, DailyGrouper, StatsAggregator};
use crate::infrastructure::FileDiscovery;

#[derive(Parser, Debug)]
#[command(name = "ccsight")]
#[command(author, version, about = "Claude Code log viewer with statistics", long_about = None)]
struct Args {
    /// Maximum number of session files to load (0 = all)
    #[arg(short, long, default_value = "0")]
    limit: usize,

    /// Clear the cache before loading
    #[arg(long)]
    clear_cache: bool,

    /// Show daily cost summary and exit
    #[arg(long)]
    daily: bool,

    /// Run as MCP server (stdio transport)
    #[arg(long)]
    mcp: bool,
}

pub fn cli_help_lines() -> Vec<(String, String)> {
    use clap::CommandFactory;
    let cmd = Args::command();
    let mut lines = Vec::new();
    for arg in cmd.get_arguments() {
        if arg.get_id() == "help" || arg.get_id() == "version" {
            continue;
        }
        let flag = if let Some(short) = arg.get_short() {
            if let Some(long) = arg.get_long() {
                format!("-{short}, --{long}")
            } else {
                format!("-{short}")
            }
        } else if let Some(long) = arg.get_long() {
            format!("    --{long}")
        } else {
            continue;
        };
        let help = arg
            .get_help()
            .map_or_else(String::new, ToString::to_string);
        lines.push((flag, help));
    }
    lines
}

fn main() -> io::Result<()> {
    let args = Args::parse();

    // `--clear-cache` composes with `--daily` / `--mcp` so users can clear and
    // run the requested mode in one invocation (matches the "Clear the cache
    // before loading" help text). When run alone — no companion mode flag and
    // not interactively from a TTY — we exit after clearing so cron-style
    // scripts piping ccsight don't get an unhelpful TUI-init error. An
    // interactive `ccsight --clear-cache` falls through to the TUI for a
    // fresh start.
    // Migrate any pre-1.1 state (`~/.cache/ccsight/`, `~/.config/ccsight/`)
    // into the unified `~/.ccsight/` tree before any other path lookup runs.
    // Idempotent and silent on failure — startup must never block on it.
    infrastructure::migrate_legacy_state_dirs();

    if args.clear_cache {
        if let Ok(cache_path) = infrastructure::cache_path() {
            if cache_path.exists() {
                std::fs::remove_file(&cache_path).ok();
                println!("Cache cleared: {}", cache_path.display());
            } else {
                println!("No cache file found");
            }
        }
        if infrastructure::SearchIndex::clear_index().is_ok() {
            println!("Search index cleared");
        }
        if !args.daily && !args.mcp && !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            return Ok(());
        }
    }

    if args.daily {
        cli::show_daily_costs(args.limit);
        return Ok(());
    }

    if args.mcp {
        let rt = tokio::runtime::Runtime::new()
            .map_err(io::Error::other)?;
        rt.block_on(mcp::run_mcp_server(args.limit))
            .map_err(io::Error::other)?;
        return Ok(());
    }

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture, DisableBracketedPaste);
        original_hook(panic_info);
    }));

    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;
    let terminal = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout()))?;

    let result = run(terminal, args.limit);

    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture, DisableBracketedPaste)?;
    result
}

fn run(mut terminal: DefaultTerminal, limit: usize) -> io::Result<()> {
    thread::spawn(ui::warmup_syntax_highlighting);
    // Warm the user-language cache off the UI thread so the first summary doesn't
    // block on `defaults read NSGlobalDomain AppleLanguages` (macOS).
    summary::prefetch_user_language();

    let mut state = AppState::new_initial(limit);

    execute!(io::stdout(), BeginSynchronizedUpdate)?;
    let completed = terminal.draw(|f| ui::draw(f, &mut state))?;
    state.screen_buffer = Some(completed.buffer.clone());
    execute!(io::stdout(), EndSynchronizedUpdate)?;

    let (tx, rx) = mpsc::channel::<LoadResult>();

    thread::spawn(move || {
        let result = load_data(limit).map_err(|e| e.to_string());
        let _ = tx.send(result);
    });

    loop {
        if state.tab == Tab::Dashboard && !state.show_dashboard_detail {
            let today = Local::now().date_naive();
            if let Some(oldest) = state.daily_groups.iter().map(|g| g.date).min() {
                let days_from_oldest = (today - oldest).num_days().max(0) as usize;
                let max_scroll = days_from_oldest / 7;
                state.dashboard_scroll[5] = state.dashboard_scroll[5].min(max_scroll);
            }
        }

        if state.needs_draw {
            execute!(io::stdout(), BeginSynchronizedUpdate)?;
            let completed = terminal.draw(|f| ui::draw(f, &mut state))?;
            state.screen_buffer = Some(completed.buffer.clone());
            execute!(io::stdout(), EndSynchronizedUpdate)?;
            state.needs_draw = false;
        }

        let has_pending = state.loading || state.generating_summary
            || state.toast_time.is_some()
            || state.data_reload_task.is_some()
            || state.summary_task.is_some()
            || state.search_task.is_some()
            || state.index_build_task.is_some()
            || state.panes.iter().any(|p| p.loading);
        if has_pending && state.index_build_task.is_none() {
            state.needs_draw = true;
        }

        if state.loading || state.generating_summary || state.panes.iter().any(|p| p.loading) {
            state.animation_frame = state.animation_frame.wrapping_add(1);
        }

        if let Some(toast_time) = state.toast_time
            && toast_time.elapsed() > std::time::Duration::from_secs(2) {
                state.toast_message = None;
                state.toast_time = None;
                state.needs_draw = true;
            }

        if state.loading
            && let Ok(result) = rx.try_recv() {
                match result {
                    Ok(data) => {
                        state.apply_loaded_data(data);
                        state.loading = false;
                        start_index_build(&mut state);
                    }
                    Err(e) => {
                        state.error = Some(e);
                        state.loading = false;
                    }
                }
            }

        if let Some(ref reload_rx) = state.data_reload_task
            && let Ok(result) = reload_rx.try_recv() {
                state.data_reload_task = None;
                // Reset the throttle timestamp regardless of outcome so a
                // transient load failure (filesystem hiccup, partial JSONL)
                // doesn't immediately re-spawn the loader on the next tick
                // and turn into a thread storm.
                state.last_data_update = Some(std::time::Instant::now());
                if let Ok(data) = result {
                    state.apply_loaded_data(data);

                    if state.selected_day >= state.daily_groups.len() {
                        state.selected_day = state.daily_groups.len().saturating_sub(1);
                    }
                    if let Some(group) = state.daily_groups.get(state.selected_day) {
                        let session_count = group.user_sessions().count();
                        if state.selected_session >= session_count {
                            state.selected_session = session_count.saturating_sub(1);
                        }
                    }
                    state.search_results.clear();
                    state.search_selected = 0;
                    start_index_build(&mut state);
                    if state.search_mode && !state.search_input.text.is_empty() {
                        state.search_results = search::perform_search(
                            &state.daily_groups,
                            &state.search_input.text,
                        );
                        start_content_search(&mut state);
                    }
                }
            }

        if !state.loading && state.data_reload_task.is_none() {
            let should_reload = state
                .last_data_update
                .is_some_and(|last| last.elapsed() > std::time::Duration::from_secs(30));

            if should_reload {
                let limit = state.data_limit;
                let (tx, rx) = mpsc::channel();
                thread::spawn(move || {
                    let result = load_data(limit).map_err(|e| e.to_string());
                    let _ = tx.send(result);
                });
                state.data_reload_task = Some(rx);
            }
        }

        if let Some((ref rx, ref file_path, day_idx, _session_idx, actual_idx)) =
            state.updating_task
            && let Ok(result) = rx.try_recv() {
                let file_path = file_path.clone();
                state.updating_task = None;
                state.updating_session = None;

                match result {
                    Ok(new_summary) => {
                        if update_jsonl_summary(&file_path, &new_summary).is_ok() {
                            if let Some(group) = state.daily_groups.get_mut(day_idx)
                                && let Some(session) = group.sessions.get_mut(actual_idx) {
                                    session.summary = Some(new_summary);
                                }
                        } else if !state.show_detail {
                            state.show_summary = true;
                            state.summary_content = "❌ Failed to write JSONL file".to_string();
                            state.summary_scroll = 0;
                        }
                    }
                    Err(e) => {
                        if !state.show_detail {
                            state.show_summary = true;
                            state.summary_content = format!("❌ Error: {e}");
                            state.summary_scroll = 0;
                        }
                    }
                }
            }

        if state.show_conversation {
            for pane in &mut state.panes {
                let should_check = pane
                    .reload_check
                    .is_none_or(|last| last.elapsed() > std::time::Duration::from_millis(500));

                if should_check {
                    pane.reload_check = Some(std::time::Instant::now());
                    if let Some(ref file_path) = pane.file_path.clone()
                        && let Ok(metadata) = std::fs::metadata(file_path)
                            && let Ok(modified) = metadata.modified() {
                                let needs_reload = pane
                                    .last_modified
                                    .is_some_and(|last| modified > last);
                                if needs_reload && pane.load_task.is_none() {
                                    if let Some(&(_, msg_idx)) =
                                        pane.message_lines.get(pane.selected_message)
                                        && let Some(msg) = pane.messages.get(msg_idx) {
                                            pane.focused_timestamp = msg.timestamp.clone();
                                        }
                                    pane.load_task = Some(spawn_load_conversation(file_path));
                                    pane.loading = true;
                                    pane.last_modified = Some(modified);
                                }
                            }
                }
            }
        }

        for pane in &mut state.panes {
            if let Some(ref rx) = pane.load_task
                && let Ok(messages) = rx.try_recv() {
                    let is_reload = !pane.messages.is_empty();
                    let old_count = pane.message_lines.len();
                    let was_at_last = old_count > 0 && pane.selected_message >= old_count - 1;

                    pane.messages = messages;
                    pane.loading = false;
                    pane.load_task = None;
                    pane.rendered = None;

                    if is_reload {
                        if was_at_last {
                            if let Some(msg) = pane
                                .messages
                                .iter()
                                .rev()
                                .find(|m| !ui::is_thinking_only_message(m))
                            {
                                pane.focused_timestamp = msg.timestamp.clone();
                            }
                            pane.scroll = usize::MAX;
                            pane.selected_message = usize::MAX;
                        }
                    } else {
                        pane.search_matches.clear();
                        pane.search_current = 0;
                        pane.search_saved_scroll = None;
                        pane.scroll = usize::MAX;
                        pane.selected_message = usize::MAX;
                    }
                }
        }

        if let Some(ref rx) = state.summary_task
            && let Ok(content) = rx.try_recv() {
                state.summary_content = content;
                state.generating_summary = false;
                state.summary_scroll = 0;
                state.summary_task = None;
            }

        if let Some(ref index_rx) = state.index_build_task {
            match index_rx.try_recv() {
                Ok(index) => {
                    state.search_index = Some(index);
                    state.index_build_task = None;
                    state.last_index_build = Some(std::time::Instant::now());
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    // Builder failed without sending; clear the flag so the
                    // top-bar indicator doesn't lie.
                    state.index_build_task = None;
                    state.last_index_build = Some(std::time::Instant::now());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }

        if let Some((ref rx, ref query)) = state.search_task
            && let Ok(content_results) = rx.try_recv() {
                if *query == state.search_input.text {
                    for result in content_results {
                        if !state.search_results.iter().any(|r| {
                            r.day_idx == result.day_idx && r.session_idx == result.session_idx
                        }) {
                            state.search_results.push(result);
                        }
                    }
                }
                state.search_task = None;
                state.searching = false;
            }

        if event::poll(Duration::from_millis(50))? {
            let ev = match std::panic::catch_unwind(event::read) {
                Ok(result) => result?,
                Err(_) => continue,
            };
            match ev {
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                        state.text_selection = None;
                        state.selecting = false;
                        state.mouse_down_pos = Some((mouse.column, mouse.row));

                        let now = std::time::Instant::now();
                        let is_double_click = state.last_click_time.is_some_and(|t| {
                            now.duration_since(t) < Duration::from_millis(400)
                                && state.last_click_pos == (mouse.column, mouse.row)
                        });
                        if is_double_click {
                            state.last_click_time = None;
                            handlers::mouse::handle_double_click(&mut state, mouse.column, mouse.row);
                        } else {
                            state.last_click_time = Some(now);
                            state.last_click_pos = (mouse.column, mouse.row);
                            handlers::mouse::handle_mouse_click(&mut state, mouse.column, mouse.row);
                        }
                    }
                    MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
                        if let Some((sc, sr)) = state.mouse_down_pos {
                            state.selecting = true;
                            state.text_selection = Some((sc, sr, mouse.column, mouse.row));

                            if state.show_conversation
                                && let Some(ca) = state.conversation_content_area {
                                    let mut scrolled = false;
                                    let scroll_amount = 2;

                                    if mouse.row < ca.y {
                                        let idx = state.active_pane_index.unwrap_or(0);
                                        if let Some(pane) = state.panes.get_mut(idx)
                                            && pane.scroll > 0 {
                                                pane.scroll = pane.scroll.saturating_sub(scroll_amount);
                                                scrolled = true;
                                            }
                                    } else if mouse.row >= ca.y + ca.height {
                                        let idx = state.active_pane_index.unwrap_or(0);
                                        if let Some(pane) = state.panes.get_mut(idx)
                                            && let Some(cached) = pane.rendered.as_ref() {
                                                let max_scroll = cached.0.len().saturating_sub(ca.height as usize);
                                                if pane.scroll < max_scroll {
                                                    pane.scroll = (pane.scroll + scroll_amount).min(max_scroll);
                                                    scrolled = true;
                                                }
                                            }
                                    }

                                    if scrolled {
                                        continue;
                                    }
                                }
                        }
                    }
                    MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
                        if state.selecting {
                            state.selecting = false;
                            state.mouse_down_pos = None;
                            if let (Some(sel), Some(buf)) =
                                (&state.text_selection, &state.screen_buffer)
                            {
                                let conv_area = if let Some(pa) = state.active_popup_area {
                                    Some(ratatui::layout::Rect {
                                        x: pa.x + 1,
                                        y: pa.y + 1,
                                        width: pa.width.saturating_sub(2),
                                        height: pa.height.saturating_sub(2),
                                    })
                                } else if state.show_conversation {
                                    state.conversation_content_area
                                } else {
                                    None
                                };
                                let (wrap_flags, conv_scroll) = if state.show_conversation {
                                    let idx = state.active_pane_index.unwrap_or(0);
                                    state.panes.get(idx)
                                        .and_then(|p| p.rendered.as_ref().map(|(_, _, flags, _)| (flags.as_slice(), p.scroll)))
                                        .map_or((None, 0), |(flags, scroll)| (Some(flags), scroll))
                                } else {
                                    (None, 0)
                                };
                                let text =
                                    extract_selected_text_from_buffer(sel, buf, conv_area, wrap_flags, conv_scroll);
                                if !text.is_empty() {
                                    let len = text.chars().count();
                                    state.toast_message = Some(format!("Copied ({len} chars)"));
                                    state.toast_time = Some(std::time::Instant::now());
                                    handlers::tasks::spawn_clipboard_write(text);
                                }
                            }
                        } else {
                            state.mouse_down_pos = None;
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        state.text_selection = None;
                        handlers::mouse::handle_mouse_scroll(&mut state, mouse.column, mouse.row, true);
                    }
                    MouseEventKind::ScrollDown => {
                        state.text_selection = None;
                        handlers::mouse::handle_mouse_scroll(&mut state, mouse.column, mouse.row, false);
                    }
                    _ => {}
                },
                Event::Paste(text) => {
                    if state.search_mode {
                        for c in text.chars() {
                            state.search_input.insert_char(c);
                        }
                        state.search_results = search::perform_search(
                            &state.daily_groups,
                            &state.search_input.text,
                        );
                        state.search_selected = 0;
                        start_content_search(&mut state);
                    } else if state.filter_input_mode {
                        for c in text.chars() {
                            state.filter_input.insert_char(c);
                        }
                        state.filter_input_error = false;
                    } else if let Some(idx) = state.active_pane_index
                        && let Some(pane) = state.panes.get_mut(idx)
                        && pane.search_mode
                    {
                        for c in text.chars() {
                            pane.search_input.insert_char(c);
                        }
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
                }
                Event::Key(key)
                    if key.kind == KeyEventKind::Press => {
                        use crossterm::event::KeyModifiers;
                        if key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            if state.ctrl_c_pressed {
                                break;
                            }
                            state.ctrl_c_pressed = true;
                            state.toast_message = Some("Press again to quit".to_string());
                            state.toast_time = Some(std::time::Instant::now());
                            state.needs_draw = true;
                            continue;
                        }
                        if key.code != KeyCode::Char('q') {
                            state.ctrl_c_pressed = false;
                        }

                        if state.show_help {
                            handlers::keyboard::handle_help_key(&mut state, key);
                        } else if state.show_filter_popup {
                            handlers::keyboard::handle_filter_popup_key(&mut state, key);
                        } else if state.show_project_popup {
                            handlers::keyboard::handle_project_popup_key(&mut state, key);
                        } else if state.show_summary {
                            handlers::keyboard::handle_summary_popup_key(&mut state, key);
                        } else if state.show_conversation {
                            handlers::keyboard::handle_conversation_key(
                                &mut state,
                                key,
                                preview_conversation_in_pane,
                                open_conversation_in_pane,
                                get_conv_session_file,
                                get_conv_session_count,
                            );
                        } else if state.show_detail {
                            handlers::keyboard::handle_session_detail_key(&mut state, key);
                        } else if state.search_mode {
                            handlers::keyboard::handle_search_mode_key(
                                &mut state,
                                key,
                                start_content_search,
                                open_conversation_in_pane,
                            );
                        } else if state.show_dashboard_detail {
                            handlers::keyboard::handle_dashboard_detail_key(&mut state, key);
                        } else if state.show_insights_detail {
                            handlers::keyboard::handle_insights_detail_key(&mut state, key);
                        } else if handlers::keyboard::handle_default_key(
                            &mut state,
                            key,
                            preview_conversation_in_pane,
                            open_conversation_in_pane,
                        ) {
                            break;
                        }
                    }
                _ => {}
            }
            state.needs_draw = true;
        }
    }
    Ok(())
}

/// Closes whichever popup overlay is currently showing. Adding a new popup
/// flag (`state.show_<x>`) requires touching THREE other call sites with the
/// same `if state.show_<x> { ... }` guard, otherwise mouse events fall through
/// onto the underlying view:
///
/// - `handlers::mouse::handle_mouse_click` — block clicks behind the popup
/// - `handlers::mouse::handle_double_click` — same, with area check
/// - `handlers::mouse::handle_mouse_scroll` — block scroll if popup is scrollable
///
/// Compiler doesn't enforce this; reviewing the 4 fns side-by-side is the
/// standing rule. (Earlier mechanically-enforced lint had too many false
/// positives — some popups legitimately ignore double-click or scroll.)
pub(crate) fn dismiss_overlay(state: &mut AppState) {
    if state.show_help {
        state.show_help = false;
        return;
    }
    if state.show_filter_popup {
        if state.filter_input_mode {
            state.filter_input_mode = false;
            state.filter_input.clear();
            state.filter_input_error = false;
        } else {
            state.show_filter_popup = false;
        }
        return;
    }
    if state.show_project_popup {
        state.show_project_popup = false;
        return;
    }
    if state.show_summary {
        state.clear_summary();
        return;
    }
    if state.search_mode {
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
        return;
    }
    if state.show_detail {
        state.show_detail = false;
        return;
    }
    if state.show_dashboard_detail {
        state.show_dashboard_detail = false;
        return;
    }
    if state.show_insights_detail {
        state.show_insights_detail = false;
        return;
    }
    if state.show_conversation {
        if state.panes.len() > 1 {
            if let Some(idx) = state.active_pane_index {
                if state.panes.get(idx).is_some_and(|p| p.search_mode) {
                    state.panes[idx].search_mode = false;
                    return;
                }
                state.panes.remove(idx);
                if state.panes.is_empty() {
                    state.show_conversation = false;
                    state.active_pane_index = None;
                    state.conv_list_mode = ConvListMode::Day;
                } else {
                    state.active_pane_index = Some(idx.min(state.panes.len() - 1));
                }
            } else if !state.panes.is_empty() {
                state.active_pane_index = Some(0);
            }
        } else {
            let has_search = state.panes.first().is_some_and(|p| p.search_mode);
            if has_search {
                if let Some(pane) = state.panes.first_mut() {
                    pane.search_mode = false;
                }
                return;
            }
            state.show_conversation = false;
            state.panes.clear();
            state.active_pane_index = None;
            state.conv_list_mode = ConvListMode::Day;
        }
        if !state.show_conversation {
            state.conv_list_mode = ConvListMode::Day;
        }
        return;
    }
    if state.daily_breakdown_focus {
        state.daily_breakdown_focus = false;
        state.daily_breakdown_scroll = 0;
    }
}

pub(crate) fn has_blocking_popup(state: &AppState) -> bool {
    state.show_help
        || state.show_summary
        || state.show_detail
        || state.show_dashboard_detail
        || state.show_insights_detail
}

pub(crate) fn dashboard_max_items(state: &AppState) -> usize {
    match state.dashboard_panel {
        0 => state.daily_costs.len(),
        1 => state.stats.project_stats.len(),
        2 => state.model_costs.len(),
        3 => crate::ui::dashboard::tool_usage_line_count(state),
        4 => {
            let known = state.stats.language_usage.iter().filter(|(l, _)| l.as_str() != "Other").count();
            let other = state.stats.extension_usage.iter().filter(|(ext, _)| {
                crate::aggregator::language::for_extension(ext) == "Other"
            }).count();
            known + other
        }
        5 => state.daily_groups.len(),
        6 => 24,
        _ => 0,
    }
}


#[allow(clippy::type_complexity)]
fn load_data(limit: usize) -> anyhow::Result<LoadedData> {
    let files = FileDiscovery::find_jsonl_files_with_limit(limit)?;
    let file_count = files.len();
    let cache = crate::infrastructure::Cache::load().unwrap_or_else(|_| crate::infrastructure::Cache::new_empty());
    let cache_for_grouper = Some(cache.clone());
    let (stats, cache_stats) = StatsAggregator::aggregate_with_shared_cache(&files, cache);
    let daily_groups = DailyGrouper::group_by_date_with_shared_cache(&files, &cache_for_grouper);

    let calculator = CostCalculator::global();
    let model_costs = calculator.calculate_costs_by_model(&stats.model_tokens);
    let aggregated_model_tokens = CostCalculator::aggregate_tokens_by_model(&stats.model_tokens);
    let models_without_pricing = calculator.models_without_pricing(&stats.model_tokens);
    let cost: f64 = model_costs.iter().map(|(_, c)| c).sum();

    // Daily costs include subagent contributions so the per-day breakdown sums
    // back to `total_cost` (Overview). Filtering `!is_subagent` here would
    // make the two totals disagree even though they share the same label.
    // Subagent dispatch is a real spend and belongs on the day it ran.
    let daily_costs: Vec<(NaiveDate, f64)> = daily_groups
        .iter()
        .map(|group| {
            let day_cost: f64 = group
                .sessions
                .iter()
                .flat_map(|s| {
                    s.day_tokens_by_model.iter().map(|(model, tokens)| {
                        calculator
                            .calculate_cost(tokens, Some(model.as_str()))
                            .unwrap_or(0.0)
                    })
                })
                .sum();
            (group.date, day_cost)
        })
        .collect();

    Ok(LoadedData {
        stats,
        cost,
        model_costs,
        aggregated_model_tokens,
        models_without_pricing,
        daily_groups,
        daily_costs,
        file_count,
        cache_stats,
    })
}

fn start_content_search(state: &mut AppState) {
    state.search_task = None;
    state.searching = false;

    if state.search_input.text.len() < 2 {
        return;
    }

    let query = state.search_input.text.clone();

    if let Some(ref index) = state.search_index {
        let results = index.search(&query, 200, 50);
        // The tantivy index stores positions captured at build time. Once a
        // project/period filter shrinks `state.daily_groups`, those positions
        // are stale (they may overshoot or even point at a different
        // session). Remap each result's `(day_idx, session_idx)` against the
        // live `daily_groups` using `session_path` as the stable key, and
        // drop results that aren't represented in the filtered view.
        let mut path_lookup: std::collections::HashMap<String, (usize, usize)> =
            std::collections::HashMap::new();
        for (day_idx, group) in state.daily_groups.iter().enumerate() {
            for (session_idx, session) in
                group.user_sessions().enumerate()
            {
                path_lookup.insert(session.file_path.to_string_lossy().to_string(), (day_idx, session_idx));
            }
        }
        for mut result in results {
            let Some(path) = result.session_path.as_ref() else {
                continue;
            };
            let Some(&(day_idx, session_idx)) = path_lookup.get(path) else {
                continue;
            };
            result.day_idx = day_idx;
            result.session_idx = session_idx;
            if !state
                .search_results
                .iter()
                .any(|r| r.day_idx == result.day_idx && r.session_idx == result.session_idx)
            {
                state.search_results.push(result);
            }
        }
        return;
    }

    let groups_data: Vec<(usize, Vec<(usize, PathBuf)>)> = state
        .daily_groups
        .iter()
        .enumerate()
        .map(|(day_idx, group)| {
            let sessions: Vec<_> = group
                .sessions
                .iter()
                .filter(|s| !s.is_subagent)
                .enumerate()
                .map(|(session_idx, s)| (session_idx, s.file_path.clone()))
                .collect();
            (day_idx, sessions)
        })
        .collect();

    let (tx, rx) = mpsc::channel();
    state.searching = true;
    state.search_task = Some((rx, query.clone()));

    std::thread::spawn(move || {
        let mut results = Vec::new();
        let mut searched = 0;
        let max_files = 100;

        for (day_idx, sessions) in &groups_data {
            for (session_idx, file_path) in sessions {
                if searched >= max_files {
                    break;
                }
                if let Some(snippet) = search::search_session_content(file_path, &query) {
                    results.push(search::SearchResult {
                        day_idx: *day_idx,
                        session_idx: *session_idx,
                        snippet: Some(snippet),
                        match_type: search::SearchMatchType::Content,
                        // Indices computed against current daily_groups,
                        // remap-on-filter not needed.
                        session_path: None,
                    });
                }
                searched += 1;
            }
            if searched >= max_files {
                break;
            }
        }

        let _ = tx.send(results);
    });
}

fn start_index_build(state: &mut AppState) {
    use std::sync::Arc;

    // Skip when a build is already in flight: overwriting the receiver
    // orphans the prior thread (its `tx.send` will fail silently after
    // committing) and leaves a `Vec<DailyGroup>` clone alive in RAM until
    // it finishes. The data reload path can fire this while the initial
    // build is still running.
    if state.index_build_task.is_some() {
        return;
    }

    // Throttle to one rebuild per minute. Initial launch (last = None)
    // always builds; subsequent reload-driven rebuilds wait at least 60 s
    // so an actively-growing session JSONL doesn't keep the indicator lit.
    if let Some(last) = state.last_index_build
        && last.elapsed() < std::time::Duration::from_secs(60) {
            return;
        }

    // Index unfiltered groups so the manifest covers every session.
    let groups: Vec<DailyGroup> = state.original_daily_groups.clone();
    let (tx, rx) = mpsc::channel();
    state.index_build_task = Some(rx);

    std::thread::spawn(move || {
        if let Ok(index) = infrastructure::SearchIndex::update_or_build(&groups) {
            let _ = tx.send(Arc::new(index));
        }
    });
}

pub(crate) use text::format_number;

pub(crate) use summary::update_jsonl_summary;


#[cfg(test)]
include!("main_tests.rs");
