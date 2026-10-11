// Fire-and-forget `let _ = tx.send(...)` from worker threads (a dropped
// receiver — main loop moved on — isn't an error to react to) and
// best-effort terminal-mode restores on shutdown, both widespread enough
// to warrant a crate-level allow over per-site ones.
#![allow(clippy::let_underscore_must_use)]

mod aggregator;
mod cli;
mod conversation;
mod handlers;
mod infrastructure;
mod mcp;
mod search;
mod search_history;
mod session_state;
mod shell;
mod state;
mod summary;
#[cfg(test)]
mod test_helpers;
mod text;
mod ui;
mod wait_run;

// `domain`, `parser`, `pins` live in the library crate; re-export so
// `crate::domain` / `crate::parser` / `crate::pins` resolve identically here.
// Moving a module into the lib means adding the matching re-export.
pub(crate) use ccsight::{domain, parser, pins};

pub use state::*;

pub use conversation::{ConversationBlock, ConversationMessage};

// Re-export pane helpers so callers (and existing fn-pointer dispatch in
// `handlers::keyboard`) can keep using bare names.
pub(crate) use handlers::mcp_popup::{adjust_mcp_scroll, collect_mcp_servers, mcp_tool_count};
pub(crate) use handlers::pane::{
    current_selected_session, extract_selected_text_from_buffer, get_conv_session_count,
    get_conv_session_file, open_conversation_in_pane, preview_conversation_in_pane,
    spawn_load_conversation,
};
// `join_conversation_lines` is referenced from `main_tests.rs` only.
#[cfg(test)]
pub(crate) use handlers::pane::join_conversation_lines;

pub(crate) const SUMMARY_MODEL: &str = "claude-haiku-4-5-20251001";

use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

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
    /// Load only the N most recently modified session files (0 = all)
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

    /// With --daily / --weekly / --monthly: print JSON instead of a table
    #[arg(long)]
    json: bool,

    /// Run as MCP server (stdio transport)
    #[arg(long)]
    mcp: bool,

    /// Wait for ONE session (id or unique prefix) to need attention — awaiting
    /// input, stalled, error, exited mid-task — or to end. Prints one
    /// tab-separated line (state, session id, reason) and exits 0; an unknown
    /// or ambiguous id exits 2 with candidates on stderr. Composable:
    /// `ccsight --wait <id> && your-notifier`; conflicts with the report modes.
    #[arg(long, value_name = "SESSION_ID", conflicts_with_all = ["daily", "weekly", "monthly", "json", "mcp"])]
    wait: Option<String>,
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
    infrastructure::ensure_private_state_root();

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
            && args.wait.is_none()
            && !std::io::IsTerminal::is_terminal(&std::io::stdout())
        {
            return Ok(());
        }
    }

    if args.json && !args.daily && !args.weekly && !args.monthly {
        eprintln!("--json requires --daily, --weekly, or --monthly");
        std::process::exit(2);
    }
    if args.daily {
        if args.json {
            cli::show_costs_json("daily", args.limit);
        } else {
            cli::show_daily_costs(args.limit);
        }
        return Ok(());
    }
    if args.weekly {
        if args.json {
            cli::show_costs_json("weekly", args.limit);
        } else {
            cli::show_weekly_costs(args.limit);
        }
        return Ok(());
    }
    if args.monthly {
        if args.json {
            cli::show_costs_json("monthly", args.limit);
        } else {
            cli::show_monthly_costs(args.limit);
        }
        return Ok(());
    }

    if args.mcp {
        let rt = tokio::runtime::Runtime::new().map_err(io::Error::other)?;
        rt.block_on(mcp::run_mcp_server(args.limit))
            .map_err(io::Error::other)?;
        return Ok(());
    }

    if let Some(prefix) = &args.wait {
        return wait_run::run_wait(prefix);
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

/// File paths of one day's user sessions, in the order `selected_session`
/// counts them.
fn day_session_paths(state: &AppState, day: usize) -> Vec<PathBuf> {
    state.daily_groups.get(day).map_or_else(Vec::new, |g| {
        g.user_sessions().map(|s| s.file_path.clone()).collect()
    })
}

/// Session ids of the Live rows in display order — active, then paused, the
/// order `live_selected` counts in.
fn live_row_keys(state: &AppState) -> Vec<String> {
    state
        .live_active
        .iter()
        .chain(&state.live_paused)
        .map(|s| s.session_id.clone())
        .collect()
}

fn poll_live_sessions_task(state: &mut AppState) {
    if let Some(ref rx) = state.live_sessions_task {
        match rx.try_recv() {
            Ok((active, paused)) => {
                // A live pid's cwd is a fresh resume witness: after an
                // in-session relocation the transcript's early cwds no longer
                // slug-match the storage dir, but the process cwd does.
                for s in &active {
                    if let Some(dir) = s
                        .jsonl_path
                        .as_deref()
                        .and_then(|p| p.parent())
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        && infrastructure::live_sessions::official_project_slug(
                            &s.cwd.to_string_lossy(),
                        ) == dir
                    {
                        state
                            .verified_project_paths
                            .entry(dir.to_string())
                            .or_insert_with(|| s.cwd.clone());
                    }
                }
                // Captured before the lists are replaced: rows are re-found by
                // session id, as a position alone would name whichever session
                // moved into it.
                let prev_keys = live_row_keys(state);
                state.live_active = active;
                state.live_paused = paused;
                // The discovery thread sorts paused by jsonl_mtime, which
                // clusters on bulk touches. Re-sort here, where the aggregation
                // is available, by the real last-activity the age column shows.
                crate::ui::sort_paused_by_recency(state);
                if state
                    .last_input_at
                    .is_some_and(|t| t.elapsed() < crate::state::LIVE_ORDER_HOLD)
                {
                    let id = |s: &infrastructure::live_sessions::LiveSession| s.session_id.clone();
                    crate::state::hold_order(&mut state.live_active, &prev_keys, id);
                    crate::state::hold_order(&mut state.live_paused, &prev_keys, id);
                }
                state.live_last_update = Some(std::time::Instant::now());
                // Refresh the time-travel-hint flag once per poll (the poll
                // thread just ran `save_if_changed`) instead of re-scanning
                // the snapshot dir every render frame.
                state.live_has_snapshot_history =
                    !infrastructure::live_snapshots::LiveSnapshot::load_recent().is_empty();
                state.live_sessions_task = None;
                // A past snapshot's cursor indexes a different list.
                if state.live_view_snapshot_offset == 0 {
                    let next_keys = live_row_keys(state);
                    let followed =
                        crate::state::follow_selection(&prev_keys, state.live_selected, &next_keys);
                    // The session may have crossed into the pane the current
                    // view hides; stay on a row that is on screen.
                    let (lo, hi) = live_selectable_range(state);
                    state.live_selected = followed.clamp(lo, hi.saturating_sub(1).max(lo));
                    if state.conv_list_mode == crate::ConvListMode::Live {
                        state.selected_session = crate::state::follow_selection(
                            &prev_keys,
                            state.selected_session,
                            &next_keys,
                        );
                    }
                }
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
                state.set_summary_scroll(0);
                state.summary_task = None;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // Summary thread panicked; reset so the user can retry.
                state.summary_task = None;
                state.generating_summary = false;
                state.summary_content = "❌ Summary generation failed".to_string();
                state.set_summary_scroll(0);
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
                state.toast(format!("Clipboard error: {e}"));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                state.clipboard_task = None;
                state.toast("Clipboard write failed (thread panicked)".to_string());
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
}

fn poll_index_build_task(state: &mut AppState) {
    if let Some(ref index_rx) = state.index_build_task {
        match index_rx.try_recv() {
            Ok(index) => {
                // Keep polling after an early (head-chunk) handle: the
                // builder replaces it with the full index, and the ensuing
                // Disconnected clears the "building" indicator.
                state.search_index = Some(index);
                state.last_index_build = Some(std::time::Instant::now());
                state.needs_draw = true;
                // The popup promised content matches once the index lands;
                // re-query so they appear without an edit to the query.
                if state.search_mode && !state.search_preview_mode {
                    start_content_search(state);
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // Builder failed without sending; clear the flag so the
                // top-bar indicator doesn't lie, and surface a toast so
                // the user knows search will be unavailable until the
                // next throttled rebuild (~60s).
                state.index_build_task = None;
                state.last_index_build = Some(std::time::Instant::now());
                state.needs_draw = true;
                if state.search_index.is_none() {
                    state.toast("Search index build failed; will retry".to_string());
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
}

/// File path of the session the detail popup is showing. Must mirror the two
/// `draw_session_detail` call sites so the preview loads the right session:
/// `session_detail_override` (Live) wins; in the conversation view the active
/// pane's file path; otherwise the cursor in the current day's user sessions.
fn detail_session_path(state: &AppState) -> Option<std::path::PathBuf> {
    if let Some(ref s) = state.session_detail_override {
        return Some(s.file_path.clone());
    }
    if state.show_conversation {
        return state
            .active_pane_index
            .and_then(|i| state.panes.get(i))
            .and_then(|p| p.file_path.clone())
            .or_else(|| get_conv_session_file(state, state.selected_session));
    }
    let group = state.daily_groups.get(state.selected_day)?;
    group
        .user_sessions()
        .nth(state.selected_session)
        .map(|s| s.file_path.clone())
}

/// Kick off the background "Recent conversation" load when the detail popup is
/// open and showing a session whose preview isn't loaded yet. Keyed on the
/// file path so switching sessions reloads and reopening the same one is a
/// no-op (cached result renders instantly).
fn ensure_session_recent_load(state: &mut AppState) {
    if !state.show_detail() {
        return;
    }
    let Some(path) = detail_session_path(state) else {
        return;
    };
    if state.session_detail_recent_for.as_ref() == Some(&path) {
        return;
    }
    state.session_detail_recent_for = Some(path.clone());
    state.session_detail_recent = None;
    let (tx, rx) = mpsc::channel();
    state.session_detail_recent_task = Some(rx);
    thread::spawn(move || {
        let recent = conversation::load_conversation(&path)
            .map(|msgs| conversation::recent_message_previews(&msgs, 6))
            .unwrap_or_default();
        let _ = tx.send(recent);
    });
}

fn poll_session_recent_task(state: &mut AppState) {
    if let Some(ref rx) = state.session_detail_recent_task {
        match rx.try_recv() {
            Ok(recent) => {
                state.session_detail_recent = Some(recent);
                state.session_detail_recent_task = None;
                state.needs_draw = true;
            }
            // Loader thread vanished — render as "no messages" rather than a
            // stuck "loading…".
            Err(mpsc::TryRecvError::Disconnected) => {
                state.session_detail_recent = Some(Vec::new());
                state.session_detail_recent_task = None;
                state.needs_draw = true;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
}

/// Periodic-reload result. `Disconnected` means the reload thread panicked
/// before sending — clear `data_reload_task` so the `is_some()` guard doesn't
/// block all future reloads.
fn poll_data_reload_task(state: &mut AppState) {
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
                    // Named by date and path, not position: a new day enters
                    // at the head of the list, and a session written to
                    // re-sorts to the top of its day, so the same indices
                    // would point at a different day or session.
                    let prev_days: Vec<NaiveDate> =
                        state.daily_groups.iter().map(|g| g.date).collect();
                    let prev_sessions = day_session_paths(state, state.selected_day);
                    let (prev_day, prev_session) = (state.selected_day, state.selected_session);
                    state.apply_loaded_data(data);
                    let next_days: Vec<NaiveDate> =
                        state.daily_groups.iter().map(|g| g.date).collect();
                    state.selected_day =
                        crate::state::follow_selection(&prev_days, prev_day, &next_days);
                    if state.conv_list_mode == crate::ConvListMode::Day
                        && prev_days.get(prev_day) == next_days.get(state.selected_day)
                    {
                        let next_sessions = day_session_paths(state, state.selected_day);
                        state.selected_session = crate::state::follow_selection(
                            &prev_sessions,
                            prev_session,
                            &next_sessions,
                        );
                    }

                    // The Recent-conversation preview is keyed by path only,
                    // so without this a reopened detail popup would keep the
                    // preview captured before this reload. Only invalidated
                    // while the popup is closed — clearing mid-view would
                    // flash "loading" and re-read the JSONL every reload.
                    if !state.show_detail() {
                        state.session_detail_recent_for = None;
                        state.session_detail_recent = None;
                    }

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
                    start_index_build(state);
                    if state.search_mode && !state.search_input.text.is_empty() {
                        let ctx_owned = build_search_filter_ctx(state);
                        state.search_results = search::perform_search(
                            &state.daily_groups,
                            &state.search_input.text,
                            &ctx_owned.as_ref(),
                        );
                        start_content_search(state);
                    }
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                state.data_reload_task = None;
                state.last_data_update = Some(std::time::Instant::now());
                state.toast("Reload failed (thread panicked); retrying in 30s");
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
}

/// Drain a tantivy/content search result into `search_results`, remapping
/// stale `session_path` to the current `daily_groups` and re-applying filters
/// (the view may have changed between dispatch and arrival). `Disconnected`
/// clears the task so the spinner stops and `/` can restart.
fn poll_search_task(state: &mut AppState) {
    if let Some((ref rx, ref query)) = state.search_task {
        match rx.try_recv() {
            Ok(content_results) => {
                if *query == state.search_input.text {
                    // Every day a session occupies, newest first (groups run
                    // newest-first), so the period filter can pick a day it covers.
                    let mut path_lookup: std::collections::HashMap<String, Vec<(usize, usize)>> =
                        std::collections::HashMap::new();
                    for (day_idx, group) in state.daily_groups.iter().enumerate() {
                        for (session_idx, session) in group.user_sessions().enumerate() {
                            path_lookup
                                .entry(session.file_path.to_string_lossy().into_owned())
                                .or_default()
                                .push((day_idx, session_idx));
                        }
                    }
                    let (filters, _) = crate::search::parse_search_query(&state.search_input.text);
                    let ctx_owned = build_search_filter_ctx(state);
                    let ctx = ctx_owned.as_ref();
                    let day_in_period = |day_idx: usize| {
                        state.daily_groups.get(day_idx).is_some_and(|g| {
                            crate::search::period_matches_for_filter(
                                g.date,
                                filters.period,
                                ctx.today,
                            )
                        })
                    };
                    // Sessions already listed, built once: a fetch returns up
                    // to thousands of hits, and a per-hit scan of the list
                    // runs on the render thread.
                    let mut shown: std::collections::HashSet<std::path::PathBuf> = state
                        .search_results
                        .iter()
                        .filter_map(|r| {
                            state.daily_groups.get(r.day_idx).and_then(|g| {
                                g.user_sessions()
                                    .nth(r.session_idx)
                                    .map(|s| s.file_path.clone())
                            })
                        })
                        .collect();
                    for mut result in content_results {
                        if let Some(path) = result.session_path.as_ref() {
                            let is_message_day = |day_idx: usize| {
                                result.message_date.is_some_and(|m| {
                                    state.daily_groups.get(day_idx).is_some_and(|g| g.date == m)
                                })
                            };
                            let Some((day_idx, session_idx)) =
                                path_lookup.get(path).and_then(|days| {
                                    crate::search::hit_position(days, day_in_period, is_message_day)
                                })
                            else {
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
                        // Dedupe by file_path so a multi-day session doesn't
                        // appear once per day. perform_search deduped its own
                        // output; this catches tantivy hits that arrive after.
                        if shown.insert(session.file_path.clone()) {
                            state.search_results.push(result);
                        }
                    }
                }
                state.search_task = None;
                state.searching = false;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                state.search_task = None;
                state.searching = false;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
}

/// Splice a JSONL-summary regen result back into the right session, or surface
/// the failure. `Disconnected` clears the slot so a panicked regen thread
/// doesn't block future regens.
fn poll_updating_task(state: &mut AppState) {
    if let Some((ref rx, ref file_path, _day_idx, _session_idx, _actual_idx)) = state.updating_task
    {
        let outcome = rx.try_recv();
        match outcome {
            Ok(result) => {
                let file_path = file_path.clone();
                state.updating_task = None;
                state.updating_session = None;
                // Error surface: a full Summary popup only when nothing else
                // is open — replacing an open popup would clobber it (popups
                // are mutually exclusive). Otherwise a toast, so the failure
                // is still visible without stealing the user's context.
                let report_error = |state: &mut AppState, msg: String| {
                    if state.active_popup == crate::ActivePopup::None {
                        state.active_popup = crate::ActivePopup::Summary { scroll: 0 };
                        state.summary_content = msg;
                    } else {
                        state.toast(msg);
                    }
                };
                match result {
                    Ok(new_title) => {
                        // custom-title rows are keyed by sessionId, which is the
                        // transcript's filename stem (`<sessionId>.jsonl`).
                        let session_id = file_path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .map(std::string::ToString::to_string);
                        if let Some(sid) = session_id {
                            if update_jsonl_custom_title(&file_path, &sid, &new_title).is_ok() {
                                // One cache entry drives every surface's title.
                                state.set_session_title(&file_path, &new_title);
                            } else {
                                report_error(state, "❌ Failed to write JSONL file".to_string());
                            }
                        } else {
                            report_error(
                                state,
                                "❌ No session id — cannot write title".to_string(),
                            );
                        }
                    }
                    Err(e) => {
                        report_error(state, format!("❌ Error: {e}"));
                    }
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                state.updating_task = None;
                state.updating_session = None;
                state.toast("Summary regen failed (thread panicked)");
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
}

/// Drain each conversation pane's background message-load result. `Disconnected`
/// clears the pane's flag so a panicked loader doesn't leave it "Loading…".
fn poll_pane_loads(state: &mut AppState) {
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
                    pane.load_task = None;
                    pane.loading = false;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }
}

/// Initial-load result. `Disconnected` means the loader thread panicked before
/// sending — surface the error and clear `loading` so the spinner doesn't spin
/// forever. `rx` is the run-local initial-load channel.
fn poll_initial_load(state: &mut AppState, rx: &mpsc::Receiver<LoadResult>) {
    if !state.loading {
        return;
    }
    match rx.try_recv() {
        Ok(Ok(data)) => {
            state.apply_loaded_data(data);
            state.loading = false;
            start_index_build(state);
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

    let mut last_render = Instant::now();
    loop {
        if state.tab == Tab::Dashboard && !state.show_dashboard_detail() {
            let today = Local::now().date_naive();
            if let Some(oldest) = state.daily_groups.iter().map(|g| g.date).min() {
                let days_from_oldest = (today - oldest).num_days().max(0) as usize;
                let max_scroll = days_from_oldest / 7;
                state.dashboard_scroll[5] = state.dashboard_scroll[5].min(max_scroll);
            }
        }

        // Coalesce input floods: a trackpad scroll burst emits hundreds of
        // events and a full redraw each would freeze the UI draining the
        // queue. Skip a frame while input is still pending (O(1) handlers),
        // but force one per frame budget so a sustained scroll still animates.
        let render_due = last_render.elapsed() >= Duration::from_millis(16);
        if state.needs_draw && (render_due || !event::poll(Duration::ZERO).unwrap_or(false)) {
            execute!(io::stdout(), BeginSynchronizedUpdate)?;
            let completed = terminal.draw(|f| ui::draw(f, &mut state))?;
            state.screen_buffer = Some(completed.buffer.clone());
            execute!(io::stdout(), EndSynchronizedUpdate)?;
            state.needs_draw = false;
            last_render = Instant::now();
        }

        // Index building is excluded: it can run 10-30s and needs no
        // animation, so it must neither spin redraws by itself nor —
        // by gating the whole condition — freeze the OTHER spinners
        // (summary generation, pane loads) that overlap with it.
        let has_animated_pending = state.loading
            || state.generating_summary
            || state.toast_time.is_some()
            || state.data_reload_task.is_some()
            || state.summary_task.is_some()
            || state.search_task.is_some()
            || state.panes.iter().any(|p| p.loading);
        if has_animated_pending {
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

        poll_initial_load(&mut state, &rx);

        poll_data_reload_task(&mut state);

        // Live sessions: poll every 5s. Reads ~/.claude/sessions/*.json
        // (Claude Code's first-party metadata) so no ambiguity; cost is a
        // dozen ~1KB JSON reads + a liveness probe per poll, negligible.
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

        poll_updating_task(&mut state);

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

        poll_pane_loads(&mut state);

        poll_summary_task(&mut state);

        // Clipboard write outcome — overwrite the optimistic "Copied"
        // toast with an error if `arboard` couldn't acquire the system
        // clipboard (no display server, no clipboard daemon, etc.).
        poll_clipboard_task(&mut state);

        poll_index_build_task(&mut state);

        // Session Detail "Recent conversation" preview: trigger a background
        // load when the popup opens on a new session, then drain the result.
        ensure_session_recent_load(&mut state);
        poll_session_recent_task(&mut state);

        poll_search_task(&mut state);

        if event::poll(Duration::from_millis(50))? {
            let ev = match std::panic::catch_unwind(event::read) {
                Ok(result) => result?,
                Err(_) => continue,
            };
            // Only input the reader directs counts toward `LIVE_ORDER_HOLD`:
            // a bare pointer move or a resize is not steering the list.
            if matches!(ev, Event::Key(_))
                || matches!(ev, Event::Mouse(m) if !matches!(m.kind, MouseEventKind::Moved))
            {
                state.last_input_at = Some(Instant::now());
            }
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
                                && let Some(ca) = state.layout.conversation_content_area
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
                                let conv_area = if let Some(pa) = state.layout.active_popup_area {
                                    Some(ratatui::layout::Rect {
                                        x: pa.x + 1,
                                        y: pa.y + 1,
                                        width: pa.width.saturating_sub(2),
                                        height: pa.height.saturating_sub(2),
                                    })
                                } else if state.show_conversation {
                                    state.layout.conversation_content_area
                                } else {
                                    None
                                };
                                // A popup drawn over the conversation (e.g. Detail
                                // via `[i]`) owns `conv_area` above; its text is
                                // not conversation message content, so the pane's
                                // wrap-continuation flags must not be applied to
                                // it — they'd garble the copied text.
                                let (wrap_flags, conv_scroll) = if state
                                    .layout
                                    .active_popup_area
                                    .is_none()
                                    && state.show_conversation
                                {
                                    let idx = state.active_pane_index.unwrap_or(0);
                                    state
                                        .panes
                                        .get(idx)
                                        .and_then(|p| {
                                            p.rendered
                                                .as_ref()
                                                .map(|(_, _, flags)| (flags.as_slice(), p.scroll))
                                        })
                                        .map_or((None, 0), |(flags, scroll)| (Some(flags), scroll))
                                } else {
                                    (None, 0)
                                };
                                // Compact mode prepends a 2-col hang-indent to
                                // body lines; strip it from copied text. Full
                                // mode / popups have no such indent (0).
                                let body_indent = if state.layout.active_popup_area.is_none()
                                    && state.show_conversation
                                    && state
                                        .active_pane_index
                                        .and_then(|i| state.panes.get(i))
                                        .is_some_and(|p| p.compact)
                                {
                                    2
                                } else {
                                    0
                                };
                                let text = extract_selected_text_from_buffer(
                                    sel,
                                    buf,
                                    conv_area,
                                    wrap_flags,
                                    conv_scroll,
                                    body_indent,
                                );
                                if !text.is_empty() {
                                    let len = text.chars().count();
                                    state.toast(format!("Copied ({len} chars)"));
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
                    if let Some(kind) = paste_into_active_input(&mut state, &text) {
                        apply_text_input_side_effects(&mut state, kind);
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
                        state.toast("Press again to quit".to_string());
                        state.needs_draw = true;
                        continue;
                    }
                    if key.code != KeyCode::Char('q') {
                        state.ctrl_c_pressed = false;
                    }

                    if state.show_help() {
                        handlers::keyboard::handle_help_key(&mut state, key);
                    } else if state.show_project_detail() {
                        handlers::keyboard::handle_project_detail_key(&mut state, key);
                    } else if state.show_filter_popup() {
                        handlers::keyboard::handle_filter_popup_key(&mut state, key);
                    } else if state.show_title_edit() {
                        handlers::keyboard::handle_title_edit_key(&mut state, key);
                    } else if state.show_project_popup() {
                        handlers::keyboard::handle_project_popup_key(&mut state, key);
                    } else if state.show_summary() {
                        handlers::keyboard::handle_summary_popup_key(&mut state, key);
                    } else if state.show_detail() {
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
                    } else if state.show_dashboard_detail() {
                        handlers::keyboard::handle_dashboard_detail_key(&mut state, key);
                    } else if state.show_insights_detail() {
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
/// `ActivePopup` variant requires the same guard in three mouse handlers
/// (`handle_mouse_click` / `handle_double_click` / `handle_mouse_scroll`)
/// — review side-by-side; mechanical lint is too noisy because some
/// popups legitimately skip double-click or scroll.
pub(crate) fn dismiss_overlay(state: &mut AppState) {
    if state.show_help() {
        state.active_popup = crate::ActivePopup::None;
        return;
    }
    if state.show_project_detail() {
        // Drilled into from the Projects-list DashboardDetail popup; closing
        // returns there, not to the bare dashboard (two-level drill-back).
        state.active_popup = crate::ActivePopup::DashboardDetail;
        return;
    }
    if state.show_filter_popup() {
        if state.filter_input_mode() {
            state.set_filter_input_mode(false);
            if let Some(input) = state.filter_input_mut() {
                input.clear();
            }
            state.set_filter_input_error(false);
        } else {
            state.active_popup = crate::ActivePopup::None;
        }
        return;
    }
    if state.show_title_edit() {
        if let crate::ActivePopup::TitleEdit { return_to, .. } = &state.active_popup {
            state.active_popup = return_to.restore();
        }
        return;
    }
    if state.show_project_popup() {
        state.active_popup = crate::ActivePopup::None;
        return;
    }
    if state.show_summary() {
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
    if state.show_detail() {
        state.active_popup = crate::ActivePopup::None;
        state.session_detail_override = None;
        state.session_detail_live_extra = None;
        return;
    }
    if state.show_dashboard_detail() {
        state.active_popup = crate::ActivePopup::None;
        return;
    }
    if state.show_insights_detail() {
        state.active_popup = crate::ActivePopup::None;
        return;
    }
    if state.show_conversation {
        // Same one-level-outward steps as the Esc key: an in-pane search bar
        // first, then focus, then the view. Dismissing never removes a pane —
        // that is `X` — so a click-away cannot silently drop a loaded pane.
        if let Some(idx) = state.active_pane_index {
            if state.panes.get(idx).is_some_and(|p| p.search_mode) {
                state.panes[idx].search_mode = false;
                return;
            }
            state.active_pane_index = None;
            return;
        }
        if let Some(pane) = state.panes.first_mut()
            && pane.search_mode
        {
            pane.search_mode = false;
            return;
        }
        state.show_conversation = false;
        state.panes.clear();
        state.active_pane_index = None;
        state.conv_list_mode = ConvListMode::Day;
        return;
    }
    if state.daily_breakdown_focus {
        state.daily_breakdown_focus = false;
        state.daily_breakdown_scroll = 0;
    }
}

pub(crate) fn has_blocking_popup(state: &AppState) -> bool {
    use crate::ActivePopup::{
        DashboardDetail, Detail, FilterPopup, Help, InsightsDetail, None, ProjectDetail,
        ProjectPopup, Summary, TitleEdit,
    };
    // Exhaustive so a new ActivePopup variant forces a blocking/non-blocking
    // decision here. FilterPopup / ProjectPopup are non-blocking (clicks fall
    // through to the underlying tab); the detail/help overlays and the modal
    // title editor block.
    match &state.active_popup {
        Help { .. }
        | ProjectDetail { .. }
        | Summary { .. }
        | Detail
        | DashboardDetail
        | InsightsDetail { .. }
        | TitleEdit { .. } => true,
        None | FilterPopup { .. } | ProjectPopup { .. } => false,
    }
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

/// `[lo, hi)` range of `live_selected` values reachable in the current view.
/// Full-screen pane modes confine the cursor to the visible list. The flat
/// index is `0..active` for active rows, `active..active+paused` for paused, so
/// the cursor-follow scroll (`live_selected < active_count`) keeps working
/// unchanged. Past view is a single list independent of the pane mode.
pub(crate) fn live_selectable_range(state: &AppState) -> (usize, usize) {
    if state.live_view_snapshot_offset > 0 {
        return (0, state.live_past_sessions.len());
    }
    let active = state.live_active.len();
    let paused = state.live_paused.len();
    match state.live_pane_mode {
        LivePaneMode::Split => (0, active + paused),
        LivePaneMode::ActiveOnly => (0, active),
        LivePaneMode::PausedOnly => (active, active + paused),
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
        0 => crate::ui::dashboard::active_days_body_line_count(
            state,
            chrono::Local::now().date_naive(),
        ),
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
                crate::ui::dashboard::active_days_body_line_count(
                    state,
                    chrono::Local::now().date_naive(),
                )
            }
        }
        6 => 24,
        _ => 0,
    }
}

fn load_data(limit: usize) -> anyhow::Result<LoadedData> {
    let cache = crate::infrastructure::Cache::load()
        .unwrap_or_else(|_| crate::infrastructure::Cache::new_empty());
    load_data_with(limit, cache)
}

/// `load_data` over a caller-supplied cache: the aggregation saves whatever
/// cache it is handed, so a test passes one that cannot write back.
fn load_data_with(limit: usize, cache: crate::infrastructure::Cache) -> anyhow::Result<LoadedData> {
    let files = FileDiscovery::find_jsonl_files_with_limit(limit)?;
    let file_count = files.len();
    // Thread the stats-updated cache into the grouper: cold-start misses are
    // parsed once (by stats) and become grouper cache hits.
    let (stats, cache_stats, cache) = StatsAggregator::aggregate_with_shared_cache(&files, cache);
    let mut cache_for_grouper = Some(cache);
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
            (group.billable_tokens() > 0).then_some((group.date, day_cost))
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

/// Route a paste to whichever field has focus, applying its char policy —
/// one decision shared with the typing path, so a new field can't accept
/// paste that typing would reject. A "selected" restored query is replaced
/// by the paste, exactly like the first typed char.
pub(crate) fn paste_into_active_input(
    state: &mut AppState,
    text: &str,
) -> Option<crate::InputKind> {
    let kind = state.active_text_input().map(|(_, kind)| kind)?;
    match kind {
        crate::InputKind::Search if state.search_select_all => {
            state.search_select_all = false;
            state.search_input.set(String::new());
        }
        crate::InputKind::PaneSearch => {
            if let Some(idx) = state.active_pane_index
                && let Some(pane) = state.panes.get_mut(idx)
                && pane.search_select_all
            {
                pane.search_select_all = false;
                pane.search_input.set(String::new());
            }
        }
        _ => {}
    }
    let (input, kind) = state.active_text_input()?;
    for c in text.chars() {
        if let Some(c) = kind.sanitize_input_char(c) {
            input.insert_char(c);
        }
    }
    Some(kind)
}

/// Refresh derived state after text lands in an input field, once per paste
/// (the typing path runs these per keystroke). Outside `AppState` because it
/// drives main-crate helpers (`start_content_search`, pane match recompute).
pub(crate) fn apply_text_input_side_effects(state: &mut AppState, kind: crate::InputKind) {
    match kind {
        crate::InputKind::Title => {}
        crate::InputKind::Search => {
            let ctx_owned = build_search_filter_ctx(state);
            state.search_results = search::perform_search(
                &state.daily_groups,
                &state.search_input.text,
                &ctx_owned.as_ref(),
            );
            state.search_selected = 0;
            start_content_search(state);
        }
        crate::InputKind::Filter => state.set_filter_input_error(false),
        crate::InputKind::PaneSearch => {
            if let Some(idx) = state.active_pane_index
                && let Some(pane) = state.panes.get_mut(idx)
            {
                ui::on_pane_search_edit(pane);
            }
        }
    }
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

/// Bumped whenever a search starts or ends, so a file scan left behind by a
/// newer keystroke, the index path, or Esc stops at its next file instead of
/// reading every transcript to completion.
static CONTENT_SCAN_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn supersede_content_scan() -> u64 {
    CONTENT_SCAN_GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1
}

/// Days a content match's message must fall in: the view's period filter
/// narrowed by the query's own `filter:` period.
fn message_date_bounds(state: &AppState) -> (Option<NaiveDate>, Option<NaiveDate>) {
    let (filters, _) = crate::search::parse_search_query(&state.search_input.text);
    let today = chrono::Local::now().date_naive();
    crate::search::intersect_bounds(
        state.period_filter.date_range(),
        crate::search::period_bounds(filters.period, today),
    )
}

pub(crate) fn start_content_search(state: &mut AppState) {
    state.search_task = None;
    state.searching = false;
    let generation = supersede_content_scan();

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
        // session_path → current daily_groups (`poll_search_task`).
        let index = std::sync::Arc::clone(index);
        let (from, to) = message_date_bounds(state);
        let (tx, rx) = mpsc::channel();
        state.searching = true;
        state.search_task = Some((rx, task_key.clone()));
        std::thread::spawn(move || {
            // The TUI's project / pin / `project:` / `filter:live` filters all
            // run on the results, so fetch past what one screen shows.
            let results =
                index.search_between(&query, crate::search::FILTERED_INDEX_FETCH, 50, from, to);
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
    // The full input, as on the index path: the receiver compares against it,
    // so a filter-stripped key would drop every result of a filtered query.
    state.search_task = Some((rx, task_key));

    std::thread::spawn(move || {
        let mut results = Vec::new();
        let mut searched = 0;
        let max_files = 100;

        for (day_idx, sessions) in &groups_data {
            for (session_idx, file_path) in sessions {
                // A keystroke starts a newer scan; this one's result would be
                // discarded, and each scan reads whole transcripts.
                if searched >= max_files
                    || CONTENT_SCAN_GENERATION.load(std::sync::atomic::Ordering::Acquire)
                        != generation
                {
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
                        message_date: None,
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
        // First-run builds hand out an early head-chunk snapshot so search
        // works while the tail indexes; the final send replaces it.
        let early_tx = tx.clone();
        let early = move |idx: infrastructure::SearchIndex| {
            let _ = early_tx.send(Arc::new(idx));
        };
        match infrastructure::SearchIndex::update_or_build_with_early(&groups, &early) {
            Ok(index) => {
                let _ = tx.send(Arc::new(index));
            }
            // Another ccsight holding the writer lock blocks the catch-up,
            // not the reading: the existing index is still searchable, and
            // hits are remapped by path, so its staleness only misses the
            // newest messages. Without this, search falls to a file scan.
            Err(_) => {
                if let Some((index, _)) = infrastructure::SearchIndex::open_for_read(&groups) {
                    let _ = tx.send(Arc::new(index));
                }
            }
        }
    });
}

pub(crate) use text::format_number;

pub(crate) use summary::update_jsonl_custom_title;

#[cfg(test)]
include!("main_tests.rs");
