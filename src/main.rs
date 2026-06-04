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
mod search_history;
mod shell;
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
pub(crate) use handlers::mcp_popup::{adjust_mcp_scroll, collect_mcp_servers, mcp_tool_count};
pub(crate) use handlers::pane::{
    current_selected_session, current_selected_session_with_index,
    extract_selected_text_from_buffer, get_conv_session_count, get_conv_session_file,
    open_conversation_in_pane, preview_conversation_in_pane, spawn_load_conversation,
};
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
        BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
        disable_raw_mode, enable_raw_mode,
    },
};
use ratatui::DefaultTerminal;

use crate::aggregator::{CostCalculator, DailyGroup, DailyGrouper, StatsAggregator};
use crate::infrastructure::FileDiscovery;

#[derive(Parser, Debug)]
#[command(name = "ccsight")]
#[command(author, version, about = "Claude Code session analytics TUI", long_about = None)]
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

    /// Show weekly (ISO Mon-Sun) cost summary and exit
    #[arg(long)]
    weekly: bool,

    /// Show monthly cost summary and exit
    #[arg(long)]
    monthly: bool,

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
        let help = arg.get_help().map_or_else(String::new, ToString::to_string);
        lines.push((flag, help));
    }
    lines
}

fn main() -> io::Result<()> {
    let args = Args::parse();

    // `--clear-cache` composes with mode flags (`--daily`, `--mcp`) and
    // falls through to the TUI if interactive; non-interactive `--clear-cache`
    // alone exits to keep cron-style pipelines TTY-error-free.
    // Path migration before any state path is looked up.
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
        if !args.daily
            && !args.weekly
            && !args.monthly
            && !args.mcp
            && !std::io::IsTerminal::is_terminal(&std::io::stdout())
        {
            return Ok(());
        }
    }

    if args.daily {
        cli::show_daily_costs(args.limit);
        return Ok(());
    }
    if args.weekly {
        cli::show_weekly_costs(args.limit);
        return Ok(());
    }
    if args.monthly {
        cli::show_monthly_costs(args.limit);
        return Ok(());
    }

    if args.mcp {
        let rt = tokio::runtime::Runtime::new().map_err(io::Error::other)?;
        rt.block_on(mcp::run_mcp_server(args.limit))
            .map_err(io::Error::other)?;
        return Ok(());
    }

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            DisableBracketedPaste
        );
        original_hook(panic_info);
    }));

    enable_raw_mode()?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let terminal = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout()))?;

    let result = run(terminal, args.limit);

    disable_raw_mode()?;
    execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    result
}

fn poll_live_sessions_task(state: &mut AppState) {
    if let Some(ref rx) = state.live_sessions_task {
        match rx.try_recv() {
            Ok((active, paused)) => {
                state.live_active = active;
                state.live_paused = paused;
                state.live_last_update = Some(std::time::Instant::now());
                // Refresh the time-travel-hint flag once per poll (the poll
                // thread just ran `save_if_changed`) instead of re-scanning
                // the snapshot dir every render frame.
                state.live_has_snapshot_history =
                    !infrastructure::live_snapshots::LiveSnapshot::load_recent().is_empty();
                state.live_sessions_task = None;
                state.live_selected = state
                    .live_selected
                    .min((state.live_active.len() + state.live_paused.len()).saturating_sub(1));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                state.live_sessions_task = None;
                state.live_last_update = Some(std::time::Instant::now());
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
}

fn poll_summary_task(state: &mut AppState) {
    if let Some(ref rx) = state.summary_task {
        match rx.try_recv() {
            Ok(content) => {
                state.summary_content = content;
                state.generating_summary = false;
                state.summary_scroll = 0;
                state.summary_task = None;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // Summary thread panicked; reset so the user can retry.
                state.summary_task = None;
                state.generating_summary = false;
                state.summary_content = "❌ Summary generation failed".to_string();
                state.summary_scroll = 0;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
}

fn poll_clipboard_task(state: &mut AppState) {
    if let Some(ref clip_rx) = state.clipboard_task {
        match clip_rx.try_recv() {
            Ok(Ok(())) => {
                state.clipboard_task = None;
            }
            Ok(Err(e)) => {
                state.clipboard_task = None;
                state.toast_message = Some(format!("Clipboard error: {e}"));
                state.toast_time = Some(std::time::Instant::now());
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                state.clipboard_task = None;
                state.toast_message = Some("Clipboard write failed (thread panicked)".to_string());
                state.toast_time = Some(std::time::Instant::now());
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
}

fn poll_index_build_task(state: &mut AppState) {
    if let Some(ref index_rx) = state.index_build_task {
        match index_rx.try_recv() {
            Ok(index) => {
                state.search_index = Some(index);
                state.index_build_task = None;
                state.last_index_build = Some(std::time::Instant::now());
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // Builder failed without sending; clear the flag so the
                // top-bar indicator doesn't lie, and surface a toast so
                // the user knows search will be unavailable until the
                // next throttled rebuild (~60s).
                state.index_build_task = None;
                state.last_index_build = Some(std::time::Instant::now());
                if state.search_index.is_none() {
                    state.toast_message = Some("Search index build failed; will retry".to_string());
                    state.toast_time = Some(std::time::Instant::now());
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
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

        let has_pending = state.loading
            || state.generating_summary
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
            && toast_time.elapsed() > std::time::Duration::from_secs(2)
        {
            state.toast_message = None;
            state.toast_time = None;
            state.needs_draw = true;
        }

        // Initial-load result. `Disconnected` means the loader thread
        // panicked before sending — without this branch the `loading` flag
        // would stay true forever and the spinner would spin indefinitely.
        if state.loading {
            match rx.try_recv() {
                Ok(Ok(data)) => {
                    state.apply_loaded_data(data);
                    state.loading = false;
                    start_index_build(&mut state);
                }
                Ok(Err(e)) => {
                    state.error = Some(e);
                    state.loading = false;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    state.error = Some("Initial data load failed (thread panicked)".to_string());
                    state.loading = false;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }

        // Periodic-reload result. `Disconnected` means the reload thread
        // panicked before sending — without clearing `data_reload_task`,
        // the `is_some()` guard would block all future reloads.
        if let Some(ref reload_rx) = state.data_reload_task {
            let outcome = reload_rx.try_recv();
            match outcome {
                Ok(result) => {
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
                            let ctx_owned = build_search_filter_ctx(&state);
                            state.search_results = search::perform_search(
                                &state.daily_groups,
                                &state.search_input.text,
                                &ctx_owned.as_ref(),
                            );
                            start_content_search(&mut state);
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    state.data_reload_task = None;
                    state.last_data_update = Some(std::time::Instant::now());
                    state.toast_message =
                        Some("Reload failed (thread panicked); retrying in 30s".to_string());
                    state.toast_time = Some(std::time::Instant::now());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }

        // Live sessions: poll every 5s. Reads ~/.claude/sessions/*.json
        // (Claude Code's first-party metadata) so no ambiguity; cost is a
        // dozen ~1KB JSON reads + `kill -0` checks per poll, negligible.
        // `Disconnected` means the discovery thread panicked — clear the
        // slot so the next 5s tick re-spawns instead of freezing the panel.
        poll_live_sessions_task(&mut state);
        if state.live_sessions_task.is_none() {
            let should_poll = state
                .live_last_update
                .is_none_or(|last| last.elapsed() > std::time::Duration::from_secs(5));
            if should_poll {
                let (tx, rx) = mpsc::channel();
                // Boot-frozen prior-run alive set drives the `⟳` marker;
                // clone it into the thread rather than recomputing, since
                // `save_if_changed` below rewrites the latest snapshot.
                let prior_alive = state.prior_run_alive.clone();
                thread::spawn(move || {
                    let active = infrastructure::live_sessions::discover_live();
                    let active_ids: std::collections::HashSet<String> =
                        active.iter().map(|s| s.session_id.clone()).collect();
                    let mut paused = infrastructure::live_sessions::discover_recently_paused(
                        &active_ids,
                        std::time::Duration::from_secs(24 * 3600),
                        std::time::SystemTime::now(),
                    );

                    // Snapshot pipeline (see live_snapshots.rs / live_diagnostic.rs):
                    // save → mark → recover → mark again → diagnostic refresh → prune.
                    // The second mark catches rows that recover_missing adds.
                    let _ = infrastructure::live_snapshots::LiveSnapshot::save_if_changed(&active);
                    infrastructure::live_sessions::mark_was_recently_live(
                        &mut paused,
                        &prior_alive,
                    );
                    let paused_ids: std::collections::HashSet<String> =
                        paused.iter().map(|s| s.session_id.clone()).collect();
                    let recovered =
                        infrastructure::live_diagnostic::LiveDiagnostic::recover_missing(
                            &active_ids,
                            &paused_ids,
                            &prior_alive,
                        );
                    paused.extend(recovered);
                    infrastructure::live_sessions::mark_was_recently_live(
                        &mut paused,
                        &prior_alive,
                    );
                    let mut snapshot = infrastructure::live_diagnostic::LiveDiagnostic::load();
                    snapshot.refresh(&active);
                    snapshot.save();
                    infrastructure::live_snapshots::LiveSnapshot::prune();

                    let _ = tx.send((active, paused));
                });
                state.live_sessions_task = Some(rx);
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
        {
            let outcome = rx.try_recv();
            match outcome {
                Ok(result) => {
                    let file_path = file_path.clone();
                    state.updating_task = None;
                    state.updating_session = None;
                    match result {
                        Ok(new_summary) => {
                            if update_jsonl_summary(&file_path, &new_summary).is_ok() {
                                if let Some(group) = state.daily_groups.get_mut(day_idx)
                                    && let Some(session) = group.sessions.get_mut(actual_idx)
                                {
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
                Err(mpsc::TryRecvError::Disconnected) => {
                    state.updating_task = None;
                    state.updating_session = None;
                    state.toast_message =
                        Some("Summary regen failed (thread panicked)".to_string());
                    state.toast_time = Some(std::time::Instant::now());
                }
                Err(mpsc::TryRecvError::Empty) => {}
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
                        && let Ok(modified) = metadata.modified()
                    {
                        let needs_reload = pane.last_modified.is_some_and(|last| modified > last);
                        if needs_reload && pane.load_task.is_none() {
                            if let Some(&(_, msg_idx)) =
                                pane.message_lines.get(pane.selected_message)
                                && let Some(msg) = pane.messages.get(msg_idx)
                            {
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
            if let Some(ref rx) = pane.load_task {
                match rx.try_recv() {
                    Ok(messages) => {
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
                    Err(mpsc::TryRecvError::Disconnected) => {
                        // Loader panicked. Without clearing the flag and
                        // the rx, the pane would stay "Loading..." forever.
                        pane.load_task = None;
                        pane.loading = false;
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                }
            }
        }

        poll_summary_task(&mut state);

        // Clipboard write outcome — overwrite the optimistic "Copied"
        // toast with an error if `arboard` couldn't acquire the system
        // clipboard (no display server, no clipboard daemon, etc.).
        poll_clipboard_task(&mut state);

        poll_index_build_task(&mut state);

        if let Some((ref rx, ref query)) = state.search_task {
            match rx.try_recv() {
                Ok(content_results) => {
                    if *query == state.search_input.text {
                        // Remap tantivy `session_path` to current daily_groups
                        // so filter changes between dispatch and arrival don't
                        // leak stale `(day_idx, session_idx)`. File-content
                        // fallback already targets the current view.
                        let path_lookup: std::collections::HashMap<String, (usize, usize)> = state
                            .daily_groups
                            .iter()
                            .enumerate()
                            .flat_map(|(day_idx, group)| {
                                group.user_sessions().enumerate().map(
                                    move |(session_idx, session)| {
                                        (
                                            session.file_path.to_string_lossy().into_owned(),
                                            (day_idx, session_idx),
                                        )
                                    },
                                )
                            })
                            .collect();
                        let (filters, _) =
                            crate::search::parse_search_query(&state.search_input.text);
                        let ctx_owned = build_search_filter_ctx(&state);
                        let ctx = ctx_owned.as_ref();
                        for mut result in content_results {
                            if let Some(path) = result.session_path.as_ref() {
                                let Some(&(day_idx, session_idx)) = path_lookup.get(path) else {
                                    continue;
                                };
                                result.day_idx = day_idx;
                                result.session_idx = session_idx;
                            }
                            // Filter step on top of the tantivy / file-content
                            // hits — same predicates as `perform_search`.
                            let group = state.daily_groups.get(result.day_idx);
                            let session = group.and_then(|g| {
                                g.sessions
                                    .iter()
                                    .filter(|s| !s.is_subagent)
                                    .nth(result.session_idx)
                            });
                            let Some(group) = group else { continue };
                            let Some(session) = session else { continue };
                            if !crate::search::period_matches_for_filter(
                                group.date,
                                filters.period,
                                ctx.today,
                            ) {
                                continue;
                            }
                            if !crate::search::session_passes_filters(session, &filters, &ctx) {
                                continue;
                            }
                            // Dedupe by file_path so a multi-day session
                            // doesn't appear once per day in the result
                            // list. perform_search already deduped its
                            // own output; this guard catches tantivy hits
                            // that arrive after.
                            let already = state.search_results.iter().any(|r| {
                                let other = state.daily_groups.get(r.day_idx).and_then(|g| {
                                    g.sessions
                                        .iter()
                                        .filter(|s| !s.is_subagent)
                                        .nth(r.session_idx)
                                });
                                other.is_some_and(|o| o.file_path == session.file_path)
                            });
                            if !already {
                                state.search_results.push(result);
                            }
                        }
                    }
                    state.search_task = None;
                    state.searching = false;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    // Search thread panicked; clear so the spinner stops
                    // and the next `/` keystroke can start a fresh search.
                    state.search_task = None;
                    state.searching = false;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
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
                            handlers::mouse::handle_double_click(
                                &mut state,
                                mouse.column,
                                mouse.row,
                            );
                        } else {
                            state.last_click_time = Some(now);
                            state.last_click_pos = (mouse.column, mouse.row);
                            handlers::mouse::handle_mouse_click(
                                &mut state,
                                mouse.column,
                                mouse.row,
                            );
                        }
                    }
                    MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
                        if let Some((sc, sr)) = state.mouse_down_pos {
                            state.selecting = true;
                            state.text_selection = Some((sc, sr, mouse.column, mouse.row));

                            if state.show_conversation
                                && let Some(ca) = state.conversation_content_area
                            {
                                let mut scrolled = false;
                                let scroll_amount = 2;

                                if mouse.row < ca.y {
                                    let idx = state.active_pane_index.unwrap_or(0);
                                    if let Some(pane) = state.panes.get_mut(idx)
                                        && pane.scroll > 0
                                    {
                                        pane.scroll = pane.scroll.saturating_sub(scroll_amount);
                                        scrolled = true;
                                    }
                                } else if mouse.row >= ca.y + ca.height {
                                    let idx = state.active_pane_index.unwrap_or(0);
                                    if let Some(pane) = state.panes.get_mut(idx)
                                        && let Some(cached) = pane.rendered.as_ref()
                                    {
                                        let max_scroll =
                                            cached.0.len().saturating_sub(ca.height as usize);
                                        if pane.scroll < max_scroll {
                                            pane.scroll =
                                                (pane.scroll + scroll_amount).min(max_scroll);
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
                                    state
                                        .panes
                                        .get(idx)
                                        .and_then(|p| {
                                            p.rendered.as_ref().map(|(_, _, flags, _)| {
                                                (flags.as_slice(), p.scroll)
                                            })
                                        })
                                        .map_or((None, 0), |(flags, scroll)| (Some(flags), scroll))
                                } else {
                                    (None, 0)
                                };
                                let text = extract_selected_text_from_buffer(
                                    sel,
                                    buf,
                                    conv_area,
                                    wrap_flags,
                                    conv_scroll,
                                );
                                if !text.is_empty() {
                                    let len = text.chars().count();
                                    state.toast_message = Some(format!("Copied ({len} chars)"));
                                    state.toast_time = Some(std::time::Instant::now());
                                    state.clipboard_task =
                                        Some(handlers::tasks::spawn_clipboard_write(text));
                                }
                            }
                        } else {
                            state.mouse_down_pos = None;
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        state.text_selection = None;
                        handlers::mouse::handle_mouse_scroll(
                            &mut state,
                            mouse.column,
                            mouse.row,
                            true,
                        );
                    }
                    MouseEventKind::ScrollDown => {
                        state.text_selection = None;
                        handlers::mouse::handle_mouse_scroll(
                            &mut state,
                            mouse.column,
                            mouse.row,
                            false,
                        );
                    }
                    _ => {}
                },
                Event::Paste(text) => {
                    if state.search_mode {
                        for c in text.chars() {
                            state.search_input.insert_char(c);
                        }
                        let ctx_owned = build_search_filter_ctx(&state);
                        state.search_results = search::perform_search(
                            &state.daily_groups,
                            &state.search_input.text,
                            &ctx_owned.as_ref(),
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
                Event::Key(key) if key.kind == KeyEventKind::Press => {
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
                    } else if state.show_project_detail {
                        handlers::keyboard::handle_project_detail_key(&mut state, key);
                    } else if state.show_filter_popup {
                        handlers::keyboard::handle_filter_popup_key(&mut state, key);
                    } else if state.show_project_popup {
                        handlers::keyboard::handle_project_popup_key(&mut state, key);
                    } else if state.show_summary {
                        handlers::keyboard::handle_summary_popup_key(&mut state, key);
                    } else if state.show_detail {
                        // Detail popup wins over conv-view dispatch so its
                        // footer keys (r/R/s/S, ↑↓, i) reach the popup
                        // handler. Conv handler's Esc/Space duplicates live
                        // in handle_session_detail_key, so nothing is lost.
                        handlers::keyboard::handle_session_detail_key(&mut state, key);
                    } else if state.show_conversation {
                        handlers::keyboard::handle_conversation_key(
                            &mut state,
                            key,
                            preview_conversation_in_pane,
                            open_conversation_in_pane,
                            get_conv_session_file,
                            get_conv_session_count,
                        );
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

/// Closes whichever popup overlay is currently showing. Adding a new
/// `show_<x>` flag requires the same guard in three mouse handlers
/// (`handle_mouse_click` / `handle_double_click` / `handle_mouse_scroll`)
/// — review side-by-side; mechanical lint is too noisy because some
/// popups legitimately skip double-click or scroll.
pub(crate) fn dismiss_overlay(state: &mut AppState) {
    if state.show_help {
        state.show_help = false;
        return;
    }
    if state.show_project_detail {
        state.show_project_detail = false;
        state.project_detail_scroll = 0;
        state.project_detail_path.clear();
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
        // Keep `search_input.text` so a follow-up `/` restores the
        // previous query — same trade-off as the keyboard Esc handler.
        state.search_mode = false;
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
        state.session_detail_override = None;
        state.session_detail_live_extra = None;
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
        || state.show_project_detail
        || state.show_summary
        || state.show_detail
        || state.show_dashboard_detail
        || state.show_insights_detail
}

/// Total visible row count in the Live tab. Today view = active + paused;
/// past-day view = the loaded daily snapshot's rows.
pub(crate) fn live_visible_count(state: &AppState) -> usize {
    if state.live_view_snapshot_offset == 0 {
        state.live_active.len() + state.live_paused.len()
    } else {
        state.live_past_sessions.len()
    }
}

/// Resolve the currently-selected Live session. Today view = active first,
/// then paused; past-day view = single list from the loaded daily snapshot.
pub(crate) fn live_selected_session(
    state: &AppState,
) -> Option<&infrastructure::live_sessions::LiveSession> {
    let idx = state.live_selected;
    if state.live_view_snapshot_offset > 0 {
        return state.live_past_sessions.get(idx);
    }
    if idx < state.live_active.len() {
        state.live_active.get(idx)
    } else {
        state.live_paused.get(idx - state.live_active.len())
    }
}

pub(crate) fn dashboard_max_items(state: &AppState) -> usize {
    match state.dashboard_panel {
        // Body = day rows + month dividers; the j/G key bounds must match the
        // popup's line-based scroll model so the oldest day is reachable.
        0 => crate::ui::dashboard::active_days_body_line_count(state),
        1 => state.stats.project_stats.len(),
        2 => state.model_costs.len(),
        3 => crate::ui::dashboard::tool_usage_line_count(state),
        4 => {
            let known = state
                .stats
                .language_usage
                .iter()
                .filter(|(l, _)| l.as_str() != "Other")
                .count();
            let other = state
                .stats
                .extension_usage
                .iter()
                .filter(|(ext, _)| crate::aggregator::language::for_extension(ext) == "Other")
                .count();
            known + other
        }
        5 => {
            if state.activity_view_weekly {
                crate::ui::dashboard::weekly_activity(state).len()
            } else {
                // Daily view renders active-day rows + month dividers, not every
                // calendar group — match the popup's line-based total.
                crate::ui::dashboard::active_days_body_line_count(state)
            }
        }
        6 => 24,
        _ => 0,
    }
}

fn load_data(limit: usize) -> anyhow::Result<LoadedData> {
    let files = FileDiscovery::find_jsonl_files_with_limit(limit)?;
    let file_count = files.len();
    let cache = crate::infrastructure::Cache::load()
        .unwrap_or_else(|_| crate::infrastructure::Cache::new_empty());
    let mut cache_for_grouper = Some(cache.clone());
    let (stats, cache_stats) = StatsAggregator::aggregate_with_shared_cache(&files, cache);
    let daily_groups =
        DailyGrouper::group_by_date_with_shared_cache(&files, &mut cache_for_grouper);

    let calculator = CostCalculator::global();
    let model_costs = calculator.calculate_costs_by_model(&stats.model_tokens);
    let aggregated_model_tokens = CostCalculator::aggregate_tokens_by_model(&stats.model_tokens);
    let models_without_pricing = calculator.models_without_pricing(&stats.model_tokens);
    let cost: f64 = model_costs.iter().map(|(_, c)| c).sum();

    // Subagent-inclusive so per-day rows sum back to Overview `total_cost`.
    // Zero-token days (errors, cancellations) dropped to keep Costs panel
    // clean; they still appear in Daily tab via daily_groups.
    let daily_costs: Vec<(NaiveDate, f64)> = daily_groups
        .iter()
        .filter_map(|group| {
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
            let day_billable_tokens: u64 = group
                .sessions
                .iter()
                .flat_map(|s| s.day_tokens_by_model.values())
                .map(|t| {
                    t.input_tokens + t.output_tokens + t.cache_creation_tokens + t.cache_read_tokens
                })
                .sum();
            (day_billable_tokens > 0).then_some((group.date, day_cost))
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

/// Snapshot the path sets a search-filter run needs from live/paused
/// session lists. Called per keystroke (cheap — these lists hold ~tens of
/// entries) so `filter:live` always reflects the latest poll.
pub(crate) fn build_search_filter_ctx(state: &AppState) -> SearchFilterCtxOwned {
    let today = chrono::Local::now().date_naive();
    let mut live = std::collections::HashSet::new();
    let mut busy = std::collections::HashSet::new();
    let mut paused = std::collections::HashSet::new();
    for s in &state.live_active {
        if let Some(p) = &s.jsonl_path {
            live.insert(p.clone());
            if s.status.as_deref() == Some("busy") {
                busy.insert(p.clone());
            }
        }
    }
    for s in &state.live_paused {
        if let Some(p) = &s.jsonl_path {
            paused.insert(p.clone());
        }
    }
    SearchFilterCtxOwned {
        today,
        live,
        busy,
        paused,
    }
}

pub(crate) struct SearchFilterCtxOwned {
    today: chrono::NaiveDate,
    live: std::collections::HashSet<PathBuf>,
    busy: std::collections::HashSet<PathBuf>,
    paused: std::collections::HashSet<PathBuf>,
}

impl SearchFilterCtxOwned {
    pub(crate) fn as_ref(&self) -> search::SearchFiltersContext<'_> {
        search::SearchFiltersContext {
            today: self.today,
            live_paths: &self.live,
            busy_paths: &self.busy,
            paused_paths: &self.paused,
        }
    }
}

pub(crate) fn start_content_search(state: &mut AppState) {
    state.search_task = None;
    state.searching = false;

    // The tantivy index only knows about message content — filter tokens
    // (`filter:live`, `project:X` etc.) would be matched as literal text
    // and yield zero hits. Strip them out before issuing the search; the
    // filter step is applied to the returned results downstream.
    let (_, free_text) = crate::search::parse_search_query(&state.search_input.text);
    if free_text.len() < 2 {
        return;
    }

    let query = free_text;
    // Stash the FULL raw input as the task key so the receive path can
    // detect stale results by comparing against the current input
    // (which the user may have edited further while the worker ran).
    let task_key = state.search_input.text.clone();

    if let Some(ref index) = state.search_index {
        // Off-UI-thread to keep typing responsive. Receiver path remaps
        // session_path → current daily_groups (see L617 lookup).
        let index = std::sync::Arc::clone(index);
        let (tx, rx) = mpsc::channel();
        state.searching = true;
        state.search_task = Some((rx, task_key.clone()));
        std::thread::spawn(move || {
            let results = index.search(&query, 200, 50);
            let _ = tx.send(results);
        });
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
        && last.elapsed() < std::time::Duration::from_secs(60)
    {
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
