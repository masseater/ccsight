use std::path::PathBuf;
use std::sync::{Arc, mpsc};

use chrono::NaiveDate;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::aggregator::{CacheStats, CostCalculator, DailyGroup, Stats, TokenStats};
use crate::infrastructure::{RetentionWarning, SearchIndex};
use crate::{ConversationMessage, pins, search};

/// Canonical input-field type. Every text entry surface (search box, filter,
/// custom date) MUST use this rather than a raw `String + usize` cursor pair.
/// `cursor` is a CHAR index, not a byte offset, and the methods below all
/// translate to byte offsets via `char_indices` so multi-byte input (Japanese,
/// emoji) doesn't panic on non-char-boundary slicing. Lint #15 catches the
/// most common mistake (raw `.remove(cursor)` / `.insert(cursor, …)` ops).
#[derive(Default, Clone)]
pub struct TextInput {
    pub text: String,
    pub cursor: usize,
}

impl TextInput {
    pub fn insert_char(&mut self, c: char) {
        let byte_pos = self
            .text
            .char_indices()
            .nth(self.cursor)
            .map_or(self.text.len(), |(i, _)| i);
        self.text.insert(byte_pos, c);
        self.cursor += 1;
    }

    pub fn delete_back(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let byte_pos = self
                .text
                .char_indices()
                .nth(self.cursor)
                .map_or(self.text.len(), |(i, _)| i);
            self.text.remove(byte_pos);
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.text.chars().count() {
            self.cursor += 1;
        }
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.text.chars().count();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub fn set(&mut self, text: String) {
        self.cursor = text.chars().count();
        self.text = text;
    }

    pub fn render_spans(
        &self,
        prefix: &str,
        style: Style,
        cursor_style: Style,
    ) -> Vec<Span<'static>> {
        let byte_cursor = self
            .text
            .char_indices()
            .nth(self.cursor)
            .map_or(self.text.len(), |(i, _)| i);
        let (before, after) = self.text.split_at(byte_cursor);
        let cursor_char_len = after.chars().next().map_or(0, char::len_utf8);
        let cursor_char = if after.is_empty() {
            " "
        } else {
            &after[..cursor_char_len]
        };
        let rest = if after.is_empty() {
            ""
        } else {
            &after[cursor_char_len..]
        };
        vec![
            Span::styled(prefix.to_string(), style),
            Span::styled(before.to_string(), style),
            Span::styled(cursor_char.to_string(), cursor_style),
            Span::styled(rest.to_string(), style),
        ]
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Dashboard,
    Daily,
    Insights,
    /// Currently-running and recently-paused Claude Code sessions.
    /// Sourced from `~/.claude/sessions/<pid>.json` (active) and
    /// `~/.claude/projects/**/*.jsonl` mtime (recently paused).
    Live,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConvListMode {
    Day,
    Pinned,
    All,
    /// Conv view was opened from the Live tab — left list shows live
    /// sessions (active + paused) and j/k navigates among them.
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodFilter {
    All,
    Pinned,
    Today,
    Last7d,
    Last30d,
    ThisMonth,
    LastMonth,
    Last90d,
    Custom(NaiveDate, Option<NaiveDate>),
}

impl PeriodFilter {
    pub const ALL_VARIANTS: [PeriodFilter; 8] = [
        PeriodFilter::All,
        PeriodFilter::Pinned,
        PeriodFilter::Today,
        PeriodFilter::Last7d,
        PeriodFilter::Last30d,
        PeriodFilter::ThisMonth,
        PeriodFilter::LastMonth,
        PeriodFilter::Last90d,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PeriodFilter::All => "All",
            PeriodFilter::Pinned => "* Pinned",
            PeriodFilter::Today => "Today",
            PeriodFilter::Last7d => "7d",
            PeriodFilter::Last30d => "30d",
            PeriodFilter::ThisMonth => "This Month",
            PeriodFilter::LastMonth => "Last Month",
            PeriodFilter::Last90d => "90d",
            PeriodFilter::Custom(_, _) => "Custom",
        }
    }

    pub fn date_range(self) -> (Option<NaiveDate>, Option<NaiveDate>) {
        use chrono::Datelike;
        let today = chrono::Local::now().date_naive();
        match self {
            PeriodFilter::All | PeriodFilter::Pinned => (None, None),
            PeriodFilter::Today => (Some(today), None),
            PeriodFilter::Last7d => (Some(today - chrono::Duration::days(7)), None),
            PeriodFilter::Last30d => (Some(today - chrono::Duration::days(30)), None),
            PeriodFilter::ThisMonth => {
                let first = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap();
                (Some(first), None)
            }
            PeriodFilter::LastMonth => {
                let first_this = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap();
                let last_prev = first_this - chrono::Duration::days(1);
                let first_prev =
                    NaiveDate::from_ymd_opt(last_prev.year(), last_prev.month(), 1).unwrap();
                (Some(first_prev), Some(last_prev))
            }
            PeriodFilter::Last90d => (Some(today - chrono::Duration::days(90)), None),
            PeriodFilter::Custom(start, end) => (Some(start), end),
        }
    }

    pub fn date_range_label(self) -> String {
        let (start, end) = self.date_range();
        let today = chrono::Local::now().date_naive();
        // Preset filters (Today/7d/30d/...) are always anchored on today, so the
        // year is implicit and we save header real-estate by showing only `%m-%d`.
        // Custom ranges can span arbitrary years, so spell out the year there
        // to avoid ambiguity (e.g. "04-01 - 04-30" of which year?).
        let (fmt, sep) = match self {
            PeriodFilter::Custom(_, _) => ("%Y-%m-%d", ".."),
            _ => ("%m-%d", " - "),
        };
        match (start, end) {
            (Some(s), None) if s == today => format!("({})", s.format(fmt)),
            (Some(s), None) => {
                format!("({}{sep}{})", s.format(fmt), today.format(fmt))
            }
            (Some(s), Some(e)) if s == e => format!("({})", s.format(fmt)),
            (Some(s), Some(e)) => {
                format!("({}{sep}{})", s.format(fmt), e.format(fmt))
            }
            _ => String::new(),
        }
    }

    pub fn parse_custom(input: &str) -> Option<PeriodFilter> {
        let input = input.trim();
        if let Some((left, right)) = input.split_once("..") {
            let start = NaiveDate::parse_from_str(left.trim(), "%Y-%m-%d").ok()?;
            let end = NaiveDate::parse_from_str(right.trim(), "%Y-%m-%d").ok()?;
            Some(PeriodFilter::Custom(start, Some(end)))
        } else if let Ok(date) = NaiveDate::parse_from_str(input, "%Y-%m-%d") {
            Some(PeriodFilter::Custom(date, Some(date)))
        } else {
            let parts: Vec<&str> = input.split('-').collect();
            match parts.as_slice() {
                [y] => {
                    // `YYYY` — whole calendar year (Jan 1 - Dec 31).
                    let year: i32 = y.parse().ok()?;
                    let first = NaiveDate::from_ymd_opt(year, 1, 1)?;
                    let last = NaiveDate::from_ymd_opt(year, 12, 31)?;
                    Some(PeriodFilter::Custom(first, Some(last)))
                }
                [y, m] => {
                    // `YYYY-MM` — whole month.
                    let year: i32 = y.parse().ok()?;
                    let month: u32 = m.parse().ok()?;
                    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
                    let next = if month == 12 {
                        NaiveDate::from_ymd_opt(year + 1, 1, 1)?
                    } else {
                        NaiveDate::from_ymd_opt(year, month + 1, 1)?
                    };
                    Some(PeriodFilter::Custom(
                        first,
                        Some(next - chrono::Duration::days(1)),
                    ))
                }
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod period_filter_tests {
    use super::*;

    #[test]
    fn test_parse_custom_year_only() {
        match PeriodFilter::parse_custom("2025") {
            Some(PeriodFilter::Custom(s, Some(e))) => {
                assert_eq!(s, NaiveDate::from_ymd_opt(2025, 1, 1).unwrap()); // lint-ok: date-literal
                assert_eq!(e, NaiveDate::from_ymd_opt(2025, 12, 31).unwrap()); // lint-ok: date-literal
            }
            other => panic!("expected Custom(2025-01-01, 2025-12-31), got {other:?}"), // lint-ok: date-literal
        }
    }

    #[test]
    fn test_parse_custom_year_month() {
        match PeriodFilter::parse_custom("2026-02") {
            Some(PeriodFilter::Custom(s, Some(e))) => {
                assert_eq!(s, NaiveDate::from_ymd_opt(2026, 2, 1).unwrap()); // lint-ok: date-literal
                assert_eq!(e, NaiveDate::from_ymd_opt(2026, 2, 28).unwrap()); // lint-ok: date-literal
            }
            other => panic!("expected Custom for 2026-02, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_custom_invalid() {
        assert!(PeriodFilter::parse_custom("abc").is_none());
        assert!(PeriodFilter::parse_custom("2025-13").is_none());
        assert!(PeriodFilter::parse_custom("2025-02-30").is_none()); // lint-ok: date-literal
    }
}

#[cfg(test)]
mod project_label_tests {
    use super::*;
    use crate::aggregator::DailyGroup;
    use crate::test_helpers::helpers::make_session;

    fn state_with_projects(names: &[&str]) -> AppState {
        let sessions: Vec<_> = names
            .iter()
            .map(|n| make_session(n, None, Some("main")))
            .collect();
        let mut state = AppState::new_initial(0, None);
        state.original_daily_groups = vec![DailyGroup {
            date: NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(), // lint-ok: date-literal
            sessions,
        }];
        state.rebuild_project_list();
        state
    }

    #[test]
    fn label_disambiguates_colliding_basenames() {
        let state = state_with_projects(&["/work/dev/foo", "/other/area/foo"]);
        assert_eq!(state.project_label("/work/dev/foo"), "foo (dev)");
        assert_eq!(state.project_label("/other/area/foo"), "foo (area)");
    }

    #[test]
    fn label_uses_basename_only_when_unique() {
        let state = state_with_projects(&["/work/dev/alpha", "/work/dev/beta"]);
        assert_eq!(state.project_label("/work/dev/alpha"), "alpha");
        assert_eq!(state.project_label("/work/dev/beta"), "beta");
    }

    #[test]
    fn label_falls_back_for_unknown_path() {
        let state = state_with_projects(&["/work/dev/known"]);
        assert_eq!(state.project_label("/never/seen/path"), "path");
    }

    #[test]
    fn label_handles_three_way_collision() {
        let state = state_with_projects(&["/a/x/tmp", "/b/y/tmp", "/c/z/tmp"]);
        assert_eq!(state.project_label("/a/x/tmp"), "tmp (x)");
        assert_eq!(state.project_label("/b/y/tmp"), "tmp (y)");
        assert_eq!(state.project_label("/c/z/tmp"), "tmp (z)");
    }
}

#[derive(Clone)]
pub enum SummaryType {
    Session(Box<crate::aggregator::SessionInfo>),
    Day(crate::aggregator::DailyGroup),
}

/// Sort key for rankable Dashboard panels (Projects, Models, ...). Toggled
/// with `s` while the panel is focused; defaults to `Recency` so the
/// "what have I been using lately" question is answered at a glance.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum RankSort {
    #[default]
    Recency,
    Tokens,
}

impl RankSort {
    pub fn toggle(self) -> Self {
        match self {
            RankSort::Recency => RankSort::Tokens,
            RankSort::Tokens => RankSort::Recency,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RankSort::Recency => "recent",
            RankSort::Tokens => "tokens",
        }
    }
}

pub const MAX_PANES: usize = 4;
pub const MIN_PANE_WIDTH: u16 = 40;
pub const SESSION_LIST_WIDTH: u16 = 28;
pub const SCROLL_LINES: usize = 5;

#[derive(Default)]
pub struct ConversationPane {
    pub messages: Vec<ConversationMessage>,
    pub scroll: usize,
    pub message_lines: Vec<(usize, usize)>,
    pub rendered: Option<(
        Vec<Line<'static>>,
        Vec<(usize, usize)>,
        Vec<bool>,
        Option<usize>,
    )>,
    pub file_path: Option<PathBuf>,
    pub last_modified: Option<std::time::SystemTime>,
    pub reload_check: Option<std::time::Instant>,
    pub loading: bool,
    pub load_task: Option<mpsc::Receiver<Vec<ConversationMessage>>>,
    pub last_width: Option<u16>,
    pub selected_message: usize,
    pub focused_timestamp: Option<String>,
    pub search_mode: bool,
    pub search_input: TextInput,
    pub search_matches: Vec<usize>,
    pub search_current: usize,
    pub search_saved_scroll: Option<(usize, usize)>,
    // Last viewport height seen by the draw function. Used by j/k to scroll
    // inside a tall message when no next/prev message exists.
    pub last_visible_height: Option<usize>,
}

impl ConversationPane {
    /// Construct a fresh pane wired up to a background load task. Caller is
    /// responsible for polling `load_task`.
    pub(crate) fn load_from(file_path: &std::path::Path) -> Self {
        let mut pane = Self::default();
        pane.load_task = Some(crate::handlers::pane::spawn_load_conversation(file_path));
        pane.loading = true;
        pane.scroll = usize::MAX;
        pane.file_path = Some(file_path.to_path_buf());
        pane.last_modified = std::fs::metadata(file_path).and_then(|m| m.modified()).ok();
        pane
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.scroll = 0;
        self.message_lines.clear();
        self.rendered = None;
        self.file_path = None;
        self.last_modified = None;
        self.reload_check = None;
        self.loading = false;
        self.load_task = None;
        self.last_width = None;
        self.selected_message = 0;
        self.focused_timestamp = None;
        self.search_mode = false;
        self.search_input.clear();
        self.search_matches.clear();
        self.search_current = 0;
        self.last_visible_height = None;
        self.search_saved_scroll = None;
    }
}

pub struct LoadedData {
    pub(crate) stats: Stats,
    pub(crate) cost: f64,
    pub(crate) model_costs: Vec<(String, f64)>,
    pub(crate) aggregated_model_tokens: std::collections::HashMap<String, TokenStats>,
    pub(crate) models_without_pricing: std::collections::HashSet<String>,
    pub(crate) daily_groups: Vec<DailyGroup>,
    pub(crate) daily_costs: Vec<(NaiveDate, f64)>,
    pub(crate) file_count: usize,
    pub(crate) cache_stats: CacheStats,
}

pub(crate) type LoadResult = Result<LoadedData, String>;

pub struct AppState {
    pub needs_draw: bool,
    pub tab: Tab,
    pub pins: pins::Pins,
    pub conv_list_mode: ConvListMode,
    pub stats: Stats,
    pub total_cost: f64,
    pub model_costs: Vec<(String, f64)>,
    pub aggregated_model_tokens: std::collections::HashMap<String, TokenStats>,
    pub models_without_pricing: std::collections::HashSet<String>,
    pub daily_groups: Vec<DailyGroup>,
    pub daily_costs: Vec<(NaiveDate, f64)>,
    pub selected_day: usize,
    pub selected_session: usize,
    pub show_detail: bool,
    /// When set, `draw_detail_popup` renders this session instead of the
    /// daily-filter-driven `(selected_day, selected_session)` pair. Used by
    /// the Live tab so detail can be inspected even when the session is
    /// outside the active period/project filter.
    pub session_detail_override: Option<crate::aggregator::SessionInfo>,
    /// Live process metadata (PID, status, version) to append to the session
    /// detail popup. Set alongside `session_detail_override` when the popup
    /// is opened from the Live tab.
    pub session_detail_live_extra: Option<(u32, String, String)>,
    pub show_help: bool,
    /// Live sessions tab data — populated by `live_sessions_task` poller.
    pub live_active: Vec<crate::infrastructure::live_sessions::LiveSession>,
    pub live_paused: Vec<crate::infrastructure::live_sessions::LiveSession>,
    pub live_selected: usize,
    pub live_scroll: usize,
    pub live_sessions_task: Option<
        mpsc::Receiver<(
            Vec<crate::infrastructure::live_sessions::LiveSession>,
            Vec<crate::infrastructure::live_sessions::LiveSession>,
        )>,
    >,
    pub live_last_update: Option<std::time::Instant>,
    /// Anchor for the `⟳ from last ccsight run` cluster check. Copied from
    /// the on-disk snapshot once at boot and frozen; must not be touched by
    /// in-run polling, otherwise the marker expires after the first refresh.
    pub prior_run_last_refresh: Option<chrono::DateTime<chrono::Utc>>,
    /// Per-project drilldown popup. Opened from the Projects detail popup
    /// (panel 1) via Enter on the focused row. `project_detail_path` is the
    /// raw project_name (matching `SessionInfo::project_name` for lookup).
    pub show_project_detail: bool,
    pub project_detail_path: String,
    pub project_detail_scroll: usize,
    pub help_scroll: u16,
    pub show_conversation: bool,
    pub show_summary: bool,
    pub summary_content: String,
    pub summary_scroll: usize,
    pub summary_type: Option<SummaryType>,
    pub daily_breakdown_focus: bool,
    pub daily_breakdown_scroll: usize,
    pub daily_breakdown_max_scroll: usize,
    pub generating_summary: bool,
    pub summary_task: Option<mpsc::Receiver<String>>,
    pub loading: bool,
    pub error: Option<String>,
    pub file_count: usize,
    pub cache_stats: Option<CacheStats>,
    pub dashboard_panel: usize,
    pub dashboard_scroll: [usize; 7],
    /// Edge-triggered scroll offset for dashboard detail popups that have a
    /// cursor concept (currently panel 1 / Projects). For panels without a
    /// cursor, the equivalent scroll lives in `dashboard_scroll` directly
    /// (where j/k just moves the viewport). For panels WITH a cursor, j/k
    /// moves `dashboard_scroll` (= cursor) and `dashboard_viewport` adjusts
    /// only when the cursor leaves the visible window — same pattern as the
    /// MCP server detail and Vim's `scrolloff`.
    pub dashboard_viewport: [usize; 7],
    pub show_dashboard_detail: bool,
    /// Daily Activity detail popup (panel 5) view toggle: `false` = per-day
    /// bars (the historical default), `true` = ISO-week aggregation. Toggled
    /// by `w` inside the popup; persisted for the session so reopening keeps
    /// the chosen mode.
    pub activity_view_weekly: bool,
    /// Active section in Tools detail popup: 0=Tools (Built-in + MCP),
    /// 1=Skills, 2=Commands, 3=Subagents. Tools merges Built-in and MCP since
    /// both are tools the assistant calls; Subagents are dispatcher-tier meta-
    /// tools and shown last. Only meaningful when
    /// `dashboard_panel == 3 && show_dashboard_detail`.
    pub tools_detail_section: usize,
    /// MCP servers whose tool breakdown is currently expanded (shown inline below the
    /// server row) in the Tools detail Tools tab (MCP subsection). Retained while
    /// the app is running so re-opening the popup preserves the expand/collapse state.
    pub mcp_expanded_servers: std::collections::HashSet<String>,
    /// Cursor index into the sorted MCP server list in the Tools detail Tools tab.
    /// `j`/`k` move this; `Enter`/`Space` toggles the selected server's expansion.
    pub mcp_selected_server: usize,
    /// Tool index within the currently selected server, when the cursor has descended
    /// into an expanded server's tool rows. `None` = cursor is on the server header
    /// row itself. `Enter`/`Space` toggle expansion only when this is `None`.
    pub mcp_selected_tool: Option<usize>,
    pub search_mode: bool,
    pub search_input: TextInput,
    pub search_results: Vec<search::SearchResult>,
    pub search_selected: usize,
    pub search_task: Option<(mpsc::Receiver<Vec<search::SearchResult>>, String)>,
    pub searching: bool,
    pub search_preview_mode: bool,
    pub search_saved_state: Option<(Tab, usize, usize, bool)>,
    /// Persisted query history. ↑/↓ in search mode walks past entries;
    /// Enter (commit) appends the current input.
    pub search_history: crate::search_history::SearchHistory,
    pub mcp_status: Vec<crate::infrastructure::McpServerStatus>,
    /// Configured (installed) Skills / Commands / Subagents discovered from
    /// `~/.claude/{skills,commands,agents}/` plus enabled plugin paths. Names
    /// are bare (no `skill:`/`command:`/`agent:` prefix) and namespaced as
    /// `<plugin>:<resource>` for plugin-provided entries — matching the
    /// discriminator used in `tool_usage` keys (`skill:<name>` → strip prefix,
    /// look up here). Populated at load time and on data reload.
    /// The Tools detail popup uses these to render zero-call rows for entries
    /// that are installed but never invoked, mirroring how MCP stale-never
    /// servers are surfaced.
    pub configured_resources: crate::infrastructure::ConfiguredResources,
    /// Last-used timestamp per `tool_usage` key (Built-in / Skill / Subagent / MCP tool).
    /// Computed by `compute_tool_last_used` from `daily_groups`. Mirrors the per-server
    /// timestamps in `mcp_status` but at tool granularity, so non-MCP categories can also
    /// surface "last used N days ago" in detail popups.
    pub tool_last_used: std::collections::HashMap<String, chrono::DateTime<chrono::Utc>>,
    pub search_index: Option<Arc<SearchIndex>>,
    pub index_build_task: Option<mpsc::Receiver<Arc<SearchIndex>>>,
    pub ctrl_c_pressed: bool,
    pub last_click_time: Option<std::time::Instant>,
    pub last_click_pos: (u16, u16),
    pub text_selection: Option<(u16, u16, u16, u16)>,
    pub selecting: bool,
    pub mouse_down_pos: Option<(u16, u16)>,
    pub screen_buffer: Option<ratatui::buffer::Buffer>,
    pub conversation_content_area: Option<ratatui::layout::Rect>,
    pub updating_session: Option<(usize, usize)>,
    pub updating_task: Option<(
        mpsc::Receiver<Result<String, String>>,
        PathBuf,
        usize,
        usize,
        usize,
    )>,
    pub last_data_update: Option<std::time::Instant>,
    /// When the last successful tantivy build/incremental update finished.
    /// Throttles `start_index_build` so an actively-growing session JSONL
    /// doesn't keep the indexing indicator lit on every reload.
    pub last_index_build: Option<std::time::Instant>,
    pub data_reload_task: Option<mpsc::Receiver<LoadResult>>,
    /// Result of the most recent `spawn_clipboard_write`. The main loop
    /// polls this so a failure (no display server, no clipboard daemon)
    /// can overwrite the optimistic "Copied" toast with an error message.
    pub clipboard_task: Option<mpsc::Receiver<Result<(), String>>>,
    pub data_limit: usize,
    pub animation_frame: usize,
    pub retention_warning: Option<RetentionWarning>,
    pub retention_warning_dismissed: bool,
    pub show_insights_detail: bool,
    pub insights_detail_scroll: usize,
    /// Vertical scroll offset for the Session/Conv detail popup (`show_detail`).
    /// Reset to 0 each time the popup is (re)opened.
    pub session_detail_scroll: usize,
    pub insights_panel: usize,
    pub toast_message: Option<String>,
    pub toast_time: Option<std::time::Instant>,
    pub panes: Vec<ConversationPane>,
    pub active_pane_index: Option<usize>,
    pub session_list_hidden: bool,
    pub tab_areas: Vec<(Tab, ratatui::layout::Rect)>,
    pub tools_detail_tab_areas: Vec<(usize, ratatui::layout::Rect)>,
    /// Click areas for the per-category rows in the Dashboard Tools panel preview.
    /// `(section_index, area)` where section_index matches `tools_detail_section`
    /// (0=Tools (Built-in + MCP), 1=Skills, 2=Commands, 3=Subagents). Built-in and
    /// MCP both route to section 0. Clicking opens the detail popup with that
    /// section pre-selected.
    pub tools_panel_category_areas: Vec<(usize, ratatui::layout::Rect)>,
    /// Click areas for MCP server rows in the Tools detail MCP tab. Each entry is
    /// `(server_index, area)` where the index matches the sorted server list position.
    /// Clicking a recorded area toggles expansion and moves the cursor to that server.
    pub mcp_server_row_areas: Vec<(usize, ratatui::layout::Rect)>,
    /// Click areas for project rows in the Projects detail popup (Dashboard
    /// panel 1). `(project_index, area)` where index is the sorted projects
    /// list position. Click selects the row; second click (or Enter) opens
    /// the per-project detail popup.
    pub project_detail_row_areas: Vec<(usize, ratatui::layout::Rect)>,
    pub pane_areas: Vec<ratatui::layout::Rect>,
    pub dashboard_panel_areas: Vec<ratatui::layout::Rect>,
    pub insights_panel_areas: Vec<ratatui::layout::Rect>,
    pub session_list_area: Option<(ratatui::layout::Rect, usize, usize)>,
    /// Live-tab row hit-test data. `(inner_rect, scroll, active_count)` so
    /// `handle_mouse_click` can translate row coords into a session index
    /// across the two-section (Active / Paused) layout.
    pub live_list_area: Option<(ratatui::layout::Rect, usize, usize)>,
    pub breakdown_panel_area: Option<ratatui::layout::Rect>,
    pub summary_popup_area: Option<ratatui::layout::Rect>,
    pub active_popup_area: Option<ratatui::layout::Rect>,
    pub daily_header_area: Option<ratatui::layout::Rect>,
    // Tab-bar trigger Rects. Populated by `draw_tabs` only, which is skipped
    // in conv view — `draw()` MUST reset these before rendering the conv
    // layout, otherwise stale Rects from the previous frame shadow widgets
    // placed at the same coords (e.g. per-pane `[i]` buttons).
    pub filter_popup_area_trigger: Option<ratatui::layout::Rect>,
    pub project_popup_area_trigger: Option<ratatui::layout::Rect>,
    pub pin_view_trigger: Option<ratatui::layout::Rect>,
    pub help_trigger: Option<ratatui::layout::Rect>,
    pub filter_popup_area: Option<ratatui::layout::Rect>,
    pub project_popup_area: Option<ratatui::layout::Rect>,
    pub search_results_area: Option<ratatui::layout::Rect>,
    pub period_filter: PeriodFilter,
    pub show_filter_popup: bool,
    pub filter_popup_selected: usize,
    pub filter_input_mode: bool,
    pub filter_input: TextInput,
    pub filter_input_error: bool,
    pub project_filter: Option<String>,
    pub show_project_popup: bool,
    pub project_popup_selected: usize,
    pub project_popup_scroll: usize,
    pub project_list: Vec<(String, u64, NaiveDate)>,
    /// Pre-computed disambiguated display labels keyed by full project_name.
    /// Built alongside `project_list` so render paths can append a `(parent)`
    /// suffix on basename collision without re-scanning `project_list` per call.
    pub project_labels: std::collections::HashMap<String, String>,
    pub original_daily_groups: Vec<DailyGroup>,
    pub original_daily_costs: Vec<(NaiveDate, f64)>,
    pub original_stats: Stats,
    pub original_total_cost: f64,
    pub original_model_costs: Vec<(String, f64)>,
    pub original_aggregated_model_tokens: std::collections::HashMap<String, TokenStats>,
    /// Sort mode for the Dashboard Projects panel + detail popup. Toggled
    /// with `s` while the Projects panel is focused. Defaults to `Recency`
    /// so currently-active projects sit at the top.
    pub dashboard_projects_sort: RankSort,
    /// Sort mode for the Dashboard Models panel + detail popup. Same toggle
    /// semantics as `dashboard_projects_sort`, scoped to panel 2.
    pub dashboard_models_sort: RankSort,
}

impl AppState {
    /// Single source of truth for "active days" — calendar days with at
    /// least one billable-token session. Matches the Costs panel ratio
    /// and the Overview headline so all surfaces agree.
    pub(crate) fn active_days(&self) -> usize {
        self.daily_costs.len()
    }

    /// Project filter popup row order — follows the same sort mode as the
    /// Projects panel so the two surfaces agree on rank. References point
    /// into `project_list`; the caller indexes into the returned view.
    pub(crate) fn project_list_sorted(&self) -> Vec<&(String, u64, chrono::NaiveDate)> {
        let mut sorted: Vec<&(String, u64, chrono::NaiveDate)> =
            self.project_list.iter().collect();
        match self.dashboard_projects_sort {
            RankSort::Tokens => {
                sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            }
            RankSort::Recency => {
                sorted.sort_by(|a, b| {
                    b.2.cmp(&a.2)
                        .then_with(|| b.1.cmp(&a.1))
                        .then_with(|| a.0.cmp(&b.0))
                });
            }
        }
        sorted
    }

    /// Sort a `(name, ProjectStats)` vec in place to match the Projects
    /// panel order. The panel draw, the detail popup draw, the keyboard
    /// Enter handler, and the mouse double-click handler all call this so
    /// they index into the same list — independent copies of this
    /// comparator drifted apart between surfaces in prior versions.
    pub(crate) fn sort_projects(
        &self,
        projects: &mut Vec<(&String, &crate::aggregator::ProjectStats)>,
    ) {
        match self.dashboard_projects_sort {
            RankSort::Tokens => {
                projects.sort_by(|a, b| {
                    b.1.work_tokens
                        .cmp(&a.1.work_tokens)
                        .then_with(|| a.0.cmp(b.0))
                });
            }
            RankSort::Recency => {
                // Last-activity date lookup goes via `project_list`, the
                // single place that joins project name → last date.
                let project_list = &self.project_list;
                let last_date_for = |name: &str| -> Option<chrono::NaiveDate> {
                    project_list
                        .iter()
                        .find(|(n, _, _)| n == name)
                        .map(|(_, _, d)| *d)
                };
                projects.sort_by(|a, b| {
                    last_date_for(b.0)
                        .cmp(&last_date_for(a.0))
                        .then_with(|| b.1.work_tokens.cmp(&a.1.work_tokens))
                        .then_with(|| a.0.cmp(b.0))
                });
            }
        }
    }

    /// Map of model display-name → last date it appeared in any user
    /// session. Recomputed per call (cheap relative to dashboard render);
    /// shared by the Models panel sort, the detail popup sort, and any
    /// future "when did I last use X" surface.
    pub(crate) fn model_last_used(&self) -> std::collections::HashMap<String, chrono::NaiveDate> {
        let mut out = std::collections::HashMap::new();
        for group in &self.daily_groups {
            for session in group.user_sessions() {
                for model_name in session.day_tokens_by_model.keys() {
                    let normalized = crate::aggregator::normalize_model_name(model_name);
                    let entry = out.entry(normalized).or_insert(group.date);
                    if group.date > *entry {
                        *entry = group.date;
                    }
                }
            }
        }
        out
    }

    /// Sort a `(name, work_tokens, ...)` vec in place to match the Models
    /// panel order. Mirrors `sort_projects` so the panel draw and the
    /// detail popup line up.
    pub(crate) fn sort_models<T>(&self, models: &mut [(String, u64, T)]) {
        match self.dashboard_models_sort {
            RankSort::Tokens => {
                models.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            }
            RankSort::Recency => {
                let last_used = self.model_last_used();
                let last_date_for = |name: &str| last_used.get(name).copied();
                models.sort_by(|a, b| {
                    last_date_for(&b.0)
                        .cmp(&last_date_for(&a.0))
                        .then_with(|| b.1.cmp(&a.1))
                        .then_with(|| a.0.cmp(&b.0))
                });
            }
        }
    }

    /// Construct a fresh `AppState` with everything zero/empty/None except
    /// `loading = true`, `tab = Dashboard`, the persisted pin list, and the
    /// retention warning derived from the user config. `data_limit` is the
    /// CLI `--limit` value carried into the data-load thread.
    pub(crate) fn new_initial(
        data_limit: usize,
        prior_run_last_refresh: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Self {
        Self {
            needs_draw: true,
            tab: Tab::Dashboard,
            pins: crate::pins::Pins::load().unwrap_or_else(|_| crate::pins::Pins::empty()),
            conv_list_mode: ConvListMode::Day,
            stats: crate::aggregator::Stats::default(),
            total_cost: 0.0,
            model_costs: Vec::new(),
            aggregated_model_tokens: std::collections::HashMap::new(),
            models_without_pricing: std::collections::HashSet::new(),
            daily_groups: Vec::new(),
            daily_costs: Vec::new(),
            selected_day: 0,
            selected_session: 0,
            show_detail: false,
            session_detail_override: None,
            session_detail_live_extra: None,
            show_help: false,
            help_scroll: 0,
            live_active: Vec::new(),
            live_paused: Vec::new(),
            live_selected: 0,
            live_scroll: 0,
            live_sessions_task: None,
            live_last_update: None,
            prior_run_last_refresh,
            show_project_detail: false,
            project_detail_path: String::new(),
            project_detail_scroll: 0,
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
            loading: true,
            error: None,
            file_count: 0,
            cache_stats: None,
            dashboard_panel: 0,
            dashboard_scroll: [0; 7],
            dashboard_viewport: [0; 7],
            activity_view_weekly: false,
            tools_detail_section: 0,
            mcp_expanded_servers: std::collections::HashSet::new(),
            mcp_selected_server: 0,
            mcp_selected_tool: None,
            show_dashboard_detail: false,
            search_mode: false,
            search_input: TextInput::default(),
            search_results: Vec::new(),
            search_selected: 0,
            search_task: None,
            searching: false,
            search_preview_mode: false,
            search_saved_state: None,
            search_history: crate::search_history::SearchHistory::load(),
            mcp_status: Vec::new(),
            configured_resources: crate::infrastructure::ConfiguredResources::default(),
            tool_last_used: std::collections::HashMap::new(),
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
            data_limit,
            animation_frame: 0,
            retention_warning: crate::infrastructure::check_cleanup_period(),
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
            period_filter: PeriodFilter::All,
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
            project_labels: std::collections::HashMap::new(),
            original_daily_groups: Vec::new(),
            original_daily_costs: Vec::new(),
            original_stats: crate::aggregator::Stats::default(),
            original_total_cost: 0.0,
            original_model_costs: Vec::new(),
            original_aggregated_model_tokens: std::collections::HashMap::new(),
            dashboard_projects_sort: RankSort::default(),
            dashboard_models_sort: RankSort::default(),
        }
    }

    pub(crate) fn clear_summary(&mut self) {
        self.show_summary = false;
        self.generating_summary = false;
        self.summary_task = None;
        self.summary_content.clear();
        self.summary_scroll = 0;
        self.summary_type = None;
    }

    pub(crate) fn apply_loaded_data(&mut self, data: LoadedData) {
        self.original_stats = data.stats.clone();
        self.original_total_cost = data.cost;
        self.original_model_costs = data.model_costs.clone();
        self.original_aggregated_model_tokens = data.aggregated_model_tokens.clone();
        self.original_daily_groups = data.daily_groups.clone();
        self.original_daily_costs = data.daily_costs.clone();

        self.stats = data.stats;
        self.total_cost = data.cost;
        self.model_costs = data.model_costs;
        self.aggregated_model_tokens = data.aggregated_model_tokens;
        self.models_without_pricing = data.models_without_pricing;
        self.daily_groups = data.daily_groups;
        self.daily_costs = data.daily_costs;
        self.file_count = data.file_count;
        self.cache_stats = Some(data.cache_stats);
        self.last_data_update = Some(std::time::Instant::now());
        self.mcp_status = crate::infrastructure::compute_mcp_status(&self.original_daily_groups);
        self.configured_resources = crate::infrastructure::discover_configured_resources();
        self.tool_last_used = crate::aggregator::compute_tool_last_used(&self.daily_groups);
        self.rebuild_project_list();

        if !matches!(self.period_filter, PeriodFilter::All) || self.project_filter.is_some() {
            self.apply_filter();
        }
    }

    pub fn apply_filter(&mut self) {
        let (start, end) = self.period_filter.date_range();
        let has_period = start.is_some() || end.is_some();
        let has_project = self.project_filter.is_some();
        let has_pinned = matches!(self.period_filter, PeriodFilter::Pinned);

        if !has_period && !has_project && !has_pinned {
            self.daily_groups = self.original_daily_groups.clone();
            self.daily_costs = self.original_daily_costs.clone();
            self.total_cost = self.original_total_cost;
            self.model_costs = self.original_model_costs.clone();
            self.aggregated_model_tokens = self.original_aggregated_model_tokens.clone();
            self.models_without_pricing =
                CostCalculator::global().models_without_pricing(&self.original_stats.model_tokens);
            self.stats = self.original_stats.clone();
        } else {
            let in_range = |date: &NaiveDate| -> bool {
                start.is_none_or(|s| *date >= s) && end.is_none_or(|e| *date <= e)
            };

            let mut groups: Vec<DailyGroup> = if has_pinned {
                self.original_daily_groups
                    .iter()
                    .filter_map(|g| {
                        let pinned: Vec<_> = g
                            .sessions
                            .iter()
                            .filter(|s| self.pins.is_pinned(&s.file_path))
                            .cloned()
                            .collect();
                        if pinned.is_empty() {
                            None
                        } else {
                            Some(DailyGroup {
                                date: g.date,
                                sessions: pinned,
                            })
                        }
                    })
                    .collect()
            } else {
                self.original_daily_groups
                    .iter()
                    .filter(|g| in_range(&g.date))
                    .cloned()
                    .collect()
            };

            if let Some(ref project) = self.project_filter {
                groups = groups
                    .into_iter()
                    .filter_map(|mut g| {
                        g.sessions.retain(|s| &s.project_name == project);
                        if g.sessions.is_empty() { None } else { Some(g) }
                    })
                    .collect();
            }
            self.daily_groups = groups;

            if has_project || has_pinned {
                // Recompute from the filtered `daily_groups` because both
                // filters retain only a subset of sessions per day; using
                // `original_daily_costs` filtered by date range only would
                // sum every session that day even when most don't match.
                let calculator = CostCalculator::global();
                let mut date_cost: std::collections::HashMap<NaiveDate, f64> =
                    std::collections::HashMap::new();
                for group in &self.daily_groups {
                    for session in &group.sessions {
                        for (model, tokens) in &session.day_tokens_by_model {
                            *date_cost.entry(group.date).or_insert(0.0) += calculator
                                .calculate_cost(tokens, Some(model))
                                .unwrap_or(0.0);
                        }
                    }
                }
                let mut costs: Vec<_> = date_cost.into_iter().collect();
                costs.sort_by_key(|c| std::cmp::Reverse(c.0));
                self.daily_costs = costs;
            } else {
                self.daily_costs = self
                    .original_daily_costs
                    .iter()
                    .filter(|(date, _)| in_range(date))
                    .copied()
                    .collect();
            }

            self.total_cost = self.daily_costs.iter().map(|(_, c)| c).sum();

            let calculator = CostCalculator::global();
            let mut all_model_tokens: std::collections::HashMap<String, TokenStats> =
                std::collections::HashMap::new();
            // Include subagent tokens — same scope as `daily_costs` (cost
            // accounting is subagent-inclusive after 1.1.0). Without this
            // a model used only inside subagents would silently drop out of
            // `aggregated_model_tokens` / `models_without_pricing` whenever
            // a project filter is applied, contradicting the unfiltered view.
            for group in &self.daily_groups {
                for session in &group.sessions {
                    for (model, tokens) in &session.day_tokens_by_model {
                        let entry = all_model_tokens.entry(model.clone()).or_default();
                        entry.input_tokens += tokens.input_tokens;
                        entry.output_tokens += tokens.output_tokens;
                        entry.cache_creation_tokens += tokens.cache_creation_tokens;
                        entry.cache_read_tokens += tokens.cache_read_tokens;
                    }
                }
            }
            self.model_costs = calculator.calculate_costs_by_model(&all_model_tokens);
            self.aggregated_model_tokens =
                CostCalculator::aggregate_tokens_by_model(&all_model_tokens);
            self.models_without_pricing = calculator.models_without_pricing(&all_model_tokens);

            self.rebuild_filtered_stats();
        }

        // Compute against unfiltered groups so "stale (>30d)" stays a
        // wall-clock predicate regardless of the active period filter.
        self.mcp_status = crate::infrastructure::compute_mcp_status(&self.original_daily_groups);

        if self.selected_day >= self.daily_groups.len() {
            self.selected_day = self.daily_groups.len().saturating_sub(1);
        }
        if let Some(group) = self.daily_groups.get(self.selected_day) {
            let session_count = group.user_sessions().count();
            if self.selected_session >= session_count {
                self.selected_session = session_count.saturating_sub(1);
            }
        }
    }

    fn rebuild_filtered_stats(&mut self) {
        use chrono::Datelike;
        let mut stats = Stats::default();

        for group in &self.daily_groups {
            for session in &group.sessions {
                if session.is_subagent {
                    continue;
                }
                stats.total_sessions_count += 1;
                stats.total_session_days += 1;
                if session.summary.is_some() {
                    stats.sessions_with_summary += 1;
                }

                let work_tokens = session.work_tokens();
                stats.total_tokens.input_tokens += session.day_input_tokens;
                stats.total_tokens.output_tokens += session.day_output_tokens;

                for (model, tokens) in &session.day_tokens_by_model {
                    let entry = stats.model_tokens.entry(model.clone()).or_default();
                    entry.input_tokens += tokens.input_tokens;
                    entry.output_tokens += tokens.output_tokens;
                    entry.cache_creation_tokens += tokens.cache_creation_tokens;
                    entry.cache_read_tokens += tokens.cache_read_tokens;

                    stats.total_tokens.cache_creation_tokens += tokens.cache_creation_tokens;
                    stats.total_tokens.cache_read_tokens += tokens.cache_read_tokens;

                    *stats.model_usage.entry(model.clone()).or_insert(0) += 1;
                }

                *stats.daily_activity.entry(group.date).or_insert(0) += session.day_input_tokens
                    + session.day_output_tokens
                    + session
                        .day_tokens_by_model
                        .values()
                        .map(|t| t.cache_creation_tokens + t.cache_read_tokens)
                        .sum::<u64>();
                *stats.daily_work_activity.entry(group.date).or_insert(0) += work_tokens;

                for (hour, tokens) in &session.day_hourly_activity {
                    *stats.hourly_activity.entry(*hour).or_insert(0) += tokens;
                }
                for (hour, tokens) in &session.day_hourly_work_tokens {
                    *stats.hourly_work_activity.entry(*hour).or_insert(0) += tokens;
                }

                let weekday = group.date.weekday();
                *stats.weekday_activity.entry(weekday).or_insert(0) += work_tokens;
                *stats.weekday_work_activity.entry(weekday).or_insert(0) += work_tokens;

                let project_stats = stats
                    .project_stats
                    .entry(session.project_name.clone())
                    .or_default();
                project_stats.sessions += 1;
                project_stats.work_tokens += work_tokens;
                project_stats.tokens += work_tokens;

                for (tool, count) in &session.day_tool_usage {
                    *stats.tool_usage.entry(tool.clone()).or_insert(0) += count;
                }
                crate::aggregator::StatsAggregator::add_session_adoption(
                    &mut stats,
                    session.day_tool_usage.keys(),
                );
                for (lang, count) in &session.day_language_usage {
                    *stats.language_usage.entry(lang.clone()).or_insert(0) += count;
                }
                for (ext, count) in &session.day_extension_usage {
                    *stats.extension_usage.entry(ext.clone()).or_insert(0) += count;
                }
            }
        }

        // tool_error/success counts and branch_stats are file-level aggregates
        // not available per-session, so we use unfiltered values as
        // approximation. When the filter resolves to zero sessions, the
        // unfiltered numbers would render against an empty view; zero them so
        // the success-rate row stays consistent with the rest of the metrics.
        if self.daily_groups.is_empty() {
            stats.tool_error_count = 0;
            stats.tool_success_count = 0;
        } else {
            stats.tool_error_count = self.original_stats.tool_error_count;
            stats.tool_success_count = self.original_stats.tool_success_count;
        }
        stats.branch_stats = self.original_stats.branch_stats.clone();

        self.stats = stats;
    }

    pub(crate) fn rebuild_project_list(&mut self) {
        // All sessions (including subagents) are counted — see the matching
        // rule in `aggregator::stats` where `project_stats` is built. Both
        // surfaces must agree on per-project totals.
        let mut map: std::collections::HashMap<String, (u64, NaiveDate)> =
            std::collections::HashMap::new();
        for group in &self.original_daily_groups {
            for session in &group.sessions {
                let entry = map
                    .entry(session.project_name.clone())
                    .or_insert((0, group.date));
                entry.0 += session.work_tokens();
                if group.date > entry.1 {
                    entry.1 = group.date;
                }
            }
        }
        let mut list: Vec<_> = map
            .into_iter()
            .map(|(name, (tokens, date))| (name, tokens, date))
            .collect();
        // Tiebreaker on name keeps tied-token projects stable across
        // rebuilds; source is a HashMap so unordered tied values would
        // otherwise shuffle.
        list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        // Pre-compute disambiguated labels: when two projects share a basename,
        // append the parent dir so panels can tell them apart.
        let mut basename_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for (name, _, _) in &list {
            let basename = name.rsplit('/').next().unwrap_or(name.as_str());
            *basename_counts.entry(basename).or_insert(0) += 1;
        }
        self.project_labels = list
            .iter()
            .map(|(name, _, _)| {
                let basename = name.rsplit('/').next().unwrap_or(name.as_str());
                let label = if basename_counts.get(basename).copied().unwrap_or(0) <= 1 {
                    basename.to_string()
                } else {
                    let parent = name
                        .rsplit_once('/')
                        .map_or("", |(p, _)| p)
                        .rsplit('/')
                        .next()
                        .unwrap_or("");
                    if parent.is_empty() {
                        basename.to_string()
                    } else {
                        format!("{basename} ({parent})")
                    }
                };
                (name.clone(), label)
            })
            .collect();

        self.project_list = list;
    }

    /// Display label for a project, disambiguated against other projects that
    /// share the same basename. Falls back to `shorten_project` for names not
    /// yet in `project_labels` (e.g. session created mid-frame before reload).
    pub fn project_label(&self, name: &str) -> String {
        self.project_labels
            .get(name)
            .cloned()
            .unwrap_or_else(|| crate::ui::shorten_project(name).to_string())
    }
}
