use std::path::PathBuf;
use std::sync::{Arc, mpsc};

use chrono::NaiveDate;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::aggregator::{CacheStats, CostCalculator, DailyGroup, Stats, TokenStats};
use crate::infrastructure::{RetentionWarning, SearchIndex};
use crate::{ConversationMessage, pins, search};

/// Canonical text-input field. `cursor` is a CHAR index translated via
/// `char_indices`, so multi-byte input (CJK / emoji) won't panic on
/// non-char-boundary slicing. Lint #15 enforces (no raw `.remove(cursor)`).
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

/// The single modal overlay currently shown, or `None`. Exactly one popup
/// at a time — "two open at once" is unrepresentable, and the dismiss /
/// guard sites match exhaustively so a new variant forces a decision. Pane
/// view / search / breakdown-focus are separate axes, not popups here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActivePopup {
    #[default]
    None,
    Help,
    ProjectDetail,
    Summary,
    Detail,
    DashboardDetail,
    InsightsDetail,
    FilterPopup,
    ProjectPopup,
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
        let mut state = AppState::new_initial(0);
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
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
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

    /// Title label for panels whose magnitude axis isn't tokens (e.g. the
    /// Ecosystem popup ranks by call count). `Recency` is always "recent";
    /// the magnitude variant takes the caller's word.
    pub fn magnitude_label(self, word: &'static str) -> &'static str {
        match self {
            RankSort::Recency => "recent",
            RankSort::Tokens => word,
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

/// Per-frame layout scratch: hit-test Rects the render pass writes and the
/// mouse handlers read. No cross-field invariant — each field is reset by the
/// draw pass that owns it (some in `draw()`, the trigger Rects in `draw_tabs`).
/// Grouped out of `AppState` to keep the stateful fields legible.
#[derive(Default)]
pub struct LayoutAreas {
    pub conversation_content_area: Option<ratatui::layout::Rect>,
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
    /// Active-frame row hit-test data. `(inner_rect, scroll, active_count)` so
    /// `handle_mouse_click` can translate row coords into a session index.
    /// The past-snapshot view reuses this single frame.
    pub live_list_area: Option<(ratatui::layout::Rect, usize, usize)>,
    /// Paused-frame row hit-test data. `(inner_rect, scroll)`; the global
    /// session index is `live_active.len() + row`. `None` in past view.
    pub live_paused_list_area: Option<(ratatui::layout::Rect, usize)>,
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
}

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
    /// Per-day cost sums for the aggregate panels (Recent Costs, Costs
    /// detail, monthly rollups). Deliberately carries no unpriced flag: the
    /// "$?"/"*" marking is a per-session-view concern, and aggregate panels
    /// render the plain (lower-bound) sum. `models_without_pricing` is the
    /// surface that says WHICH models are missing rates.
    pub daily_costs: Vec<(NaiveDate, f64)>,
    pub selected_day: usize,
    pub selected_session: usize,
    pub active_popup: ActivePopup,
    /// When set, `draw_detail_popup` renders this session instead of the
    /// daily-filter-driven `(selected_day, selected_session)` pair. Used by
    /// the Live tab so detail can be inspected even when the session is
    /// outside the active period/project filter.
    pub session_detail_override: Option<crate::aggregator::SessionInfo>,
    /// Live process metadata (PID, status, version) to append to the session
    /// detail popup. Set alongside `session_detail_override` when the popup
    /// is opened from the Live tab.
    pub session_detail_live_extra: Option<(u32, String, String)>,
    /// Live sessions tab data — populated by `live_sessions_task` poller.
    pub live_active: Vec<crate::infrastructure::live_sessions::LiveSession>,
    pub live_paused: Vec<crate::infrastructure::live_sessions::LiveSession>,
    pub live_selected: usize,
    /// Active-frame viewport scroll. The Live tab renders Active now and
    /// Recently paused in two separate frames, each scrolling independently;
    /// `live_scroll` follows the cursor while it is in the active section,
    /// `live_paused_scroll` while it is in the paused section.
    pub live_scroll: usize,
    pub live_paused_scroll: usize,
    /// Cached "any frozen snapshot exists" flag, gating the Live tab's
    /// time-travel title hint. The live poll recomputes it instead of
    /// re-scanning the snapshot dir on every render frame.
    pub live_has_snapshot_history: bool,
    pub live_sessions_task: Option<
        mpsc::Receiver<(
            Vec<crate::infrastructure::live_sessions::LiveSession>,
            Vec<crate::infrastructure::live_sessions::LiveSession>,
        )>,
    >,
    pub live_last_update: Option<std::time::Instant>,
    /// Alive-session set from the prior run's final snapshot, frozen once
    /// at boot. Drives the Live tab's `⟳ restorable` marker: a paused
    /// session is restorable iff it was alive when ccsight last had a view
    /// (= the most recent snapshot that existed before this run's first
    /// poll overwrote the latest file).
    pub prior_run_alive:
        std::collections::HashMap<String, crate::infrastructure::live_snapshots::LiveSnapshotEntry>,
    /// Live tab "time travel": 0 = now (live poll), 1..=N = N-th most
    /// recent frozen snapshot from `~/.ccsight/live_snapshots/`. The
    /// `←/→` stepper crosses snapshot boundaries (multiple per day are
    /// possible within the multi-snapshot window) instead of always
    /// stepping a whole day.
    pub live_view_snapshot_offset: usize,
    /// Pseudo-`LiveSession` rows materialised from the frozen snapshot at
    /// `live_view_snapshot_offset`. Populated by the snapshot stepper so
    /// the existing renderer + `live_selected_session` can iterate
    /// uniformly with today's `live_active`. Empty on today (offset = 0).
    pub live_past_sessions: Vec<crate::infrastructure::live_sessions::LiveSession>,
    /// Captured-at and date of the snapshot referenced by
    /// `live_view_snapshot_offset > 0`. `None` on today.
    pub live_past_snapshot_meta: Option<(chrono::DateTime<chrono::Utc>, chrono::NaiveDate)>,
    /// Total snapshots available in the retention window. Drives the
    /// "(M/N)" position indicator in the past-day header. Refreshed
    /// alongside `live_past_sessions`.
    pub live_past_snapshot_total: usize,
    /// Per-project drilldown popup. Opened from the Projects detail popup
    /// (panel 1) via Enter on the focused row. `project_detail_path` is the
    /// raw project_name (matching `SessionInfo::project_name` for lookup).
    pub project_detail_path: String,
    pub project_detail_scroll: usize,
    pub help_scroll: u16,
    pub show_conversation: bool,
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
    /// Viewport (edge-triggered) for cursor-bearing dashboard popups
    /// (panel 1 / Projects). Cursor-less panels keep both in
    /// `dashboard_scroll`. Vim `scrolloff` pattern; mirrors MCP server detail.
    pub dashboard_viewport: [usize; 7],
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
    /// Installed Skills / Commands / Subagents from
    /// `~/.claude/{skills,commands,agents}/` + enabled plugin paths.
    /// Names bare; plugin entries namespaced `<plugin>:<resource>` to
    /// match `tool_usage` key form. Drives zero-call rows in Tools popup.
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
    pub insights_detail_scroll: usize,
    /// Vertical scroll offset for the Session/Conv detail popup (`show_detail`).
    /// Reset to 0 each time the popup is (re)opened.
    pub session_detail_scroll: usize,
    /// "Recent conversation" preview for the Session Detail popup: the last few
    /// `(role, one-line text)` messages, loaded once per session in the
    /// background. `None` while loading. `session_detail_recent_for` records
    /// which session's file the result belongs to so switching sessions
    /// reloads and reopening the same one is instant.
    pub session_detail_recent: Option<Vec<(String, String)>>,
    pub session_detail_recent_task: Option<mpsc::Receiver<Vec<(String, String)>>>,
    pub session_detail_recent_for: Option<PathBuf>,
    pub insights_panel: usize,
    pub toast_message: Option<String>,
    pub toast_time: Option<std::time::Instant>,
    pub panes: Vec<ConversationPane>,
    pub active_pane_index: Option<usize>,
    pub session_list_hidden: bool,
    pub layout: LayoutAreas,
    pub period_filter: PeriodFilter,
    pub filter_popup_selected: usize,
    pub filter_input_mode: bool,
    pub filter_input: TextInput,
    pub filter_input_error: bool,
    pub project_filter: Option<String>,
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
    /// Sort mode for the Dashboard Ecosystem detail popup (panel 3) — all
    /// tabs' item lists and the MCP server rows. `Recency` ranks by
    /// last-used (default); the magnitude variant ranks by call count.
    pub dashboard_ecosystem_sort: RankSort,
    /// Sort mode for the Dashboard Languages panel + detail popup (panel 4).
    /// Same toggle semantics as the other rankable panels; `Recency` (default)
    /// ranks by the last day a file of that language/extension was touched.
    pub dashboard_languages_sort: RankSort,
}

impl AppState {
    /// Show a transient toast. Sets message and timestamp together so a caller
    /// can't leave `toast_time` unset (which would strand the toast on screen).
    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast_message = Some(msg.into());
        self.toast_time = Some(std::time::Instant::now());
    }

    pub fn show_help(&self) -> bool {
        self.active_popup == ActivePopup::Help
    }
    pub fn show_project_detail(&self) -> bool {
        self.active_popup == ActivePopup::ProjectDetail
    }
    pub fn show_summary(&self) -> bool {
        self.active_popup == ActivePopup::Summary
    }
    pub fn show_detail(&self) -> bool {
        self.active_popup == ActivePopup::Detail
    }
    pub fn show_dashboard_detail(&self) -> bool {
        self.active_popup == ActivePopup::DashboardDetail
    }
    pub fn show_insights_detail(&self) -> bool {
        self.active_popup == ActivePopup::InsightsDetail
    }
    pub fn show_filter_popup(&self) -> bool {
        self.active_popup == ActivePopup::FilterPopup
    }
    pub fn show_project_popup(&self) -> bool {
        self.active_popup == ActivePopup::ProjectPopup
    }

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
        let mut sorted: Vec<&(String, u64, chrono::NaiveDate)> = self.project_list.iter().collect();
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
        projects: &mut [(&String, &crate::aggregator::ProjectStats)],
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
                // Build the name → last-date map once (mirrors sort_models)
                // so the comparator is O(1) per probe instead of scanning
                // `project_list` on every comparison.
                let last_date: std::collections::HashMap<&str, chrono::NaiveDate> = self
                    .project_list
                    .iter()
                    .map(|(n, _, d)| (n.as_str(), *d))
                    .collect();
                let last_date_for = |name: &str| last_date.get(name).copied();
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

    /// Map of language name / raw extension → last date it was touched in any
    /// user session. Languages and extensions live in distinct keyspaces
    /// (`"Rust"` vs `"rs"`), so one map serves both the known-language rows
    /// and the unknown-extension rows of the Languages panel. Mirrors
    /// `model_last_used`.
    pub(crate) fn language_last_used(
        &self,
    ) -> std::collections::HashMap<String, chrono::NaiveDate> {
        let mut out = std::collections::HashMap::new();
        for group in &self.daily_groups {
            for session in group.user_sessions() {
                for key in session
                    .day_language_usage
                    .keys()
                    .chain(session.day_extension_usage.keys())
                {
                    let entry = out.entry(key.clone()).or_insert(group.date);
                    if group.date > *entry {
                        *entry = group.date;
                    }
                }
            }
        }
        out
    }

    /// Sort the Languages panel rows `(name, count, is_unknown)` in place to
    /// match the active sort mode. Shared by the compact panel and the detail
    /// popup so both surfaces agree on rank.
    pub(crate) fn sort_languages(&self, entries: &mut [(String, usize, bool)]) {
        match self.dashboard_languages_sort {
            RankSort::Tokens => {
                entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            }
            RankSort::Recency => {
                let last_used = self.language_last_used();
                let last_date_for = |name: &str| last_used.get(name).copied();
                entries.sort_by(|a, b| {
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
    pub(crate) fn new_initial(data_limit: usize) -> Self {
        // Freeze the prior run's final alive set BEFORE any poll runs — the
        // first `save_if_changed` would otherwise overwrite the latest
        // snapshot with the current (mostly empty post-reboot) alive set and
        // break restorable recovery. A non-empty latest snapshot also means
        // time-travel history exists (`load_recent` skips empties).
        let prior_run_alive = crate::infrastructure::live_snapshots::latest_snapshot_alive();
        let live_has_snapshot_history = !prior_run_alive.is_empty();
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
            active_popup: ActivePopup::None,
            session_detail_override: None,
            session_detail_live_extra: None,
            help_scroll: 0,
            live_active: Vec::new(),
            live_paused: Vec::new(),
            live_selected: 0,
            live_scroll: 0,
            live_paused_scroll: 0,
            live_has_snapshot_history,
            live_sessions_task: None,
            live_last_update: None,
            prior_run_alive,
            live_view_snapshot_offset: 0,
            live_past_sessions: Vec::new(),
            live_past_snapshot_meta: None,
            live_past_snapshot_total: 0,
            project_detail_path: String::new(),
            project_detail_scroll: 0,
            show_conversation: false,
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
            insights_detail_scroll: 0,
            session_detail_scroll: 0,
            session_detail_recent: None,
            session_detail_recent_task: None,
            session_detail_recent_for: None,
            insights_panel: 0,
            toast_message: None,
            toast_time: None,
            panes: Vec::new(),
            active_pane_index: None,
            session_list_hidden: false,
            layout: LayoutAreas::default(),
            period_filter: PeriodFilter::All,
            filter_popup_selected: 0,
            filter_input_mode: false,
            filter_input: TextInput::default(),
            filter_input_error: false,
            project_filter: None,
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
            dashboard_ecosystem_sort: RankSort::default(),
            dashboard_languages_sort: RankSort::default(),
        }
    }

    pub(crate) fn clear_summary(&mut self) {
        self.active_popup = crate::ActivePopup::None;
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
                        // merge() carries the 5m/1h cache split — calculate_cost
                        // prices those (no fallback to the flat total), so a
                        // hand-rolled 4-field fold would zero the cache-write
                        // cost for every model.
                        all_model_tokens
                            .entry(model.clone())
                            .or_default()
                            .merge(tokens);
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
                // Subagent rule (matches unfiltered StatsAggregator): only
                // counts exclude subagents; tokens / model / activity /
                // project / tool / language are subagent-inclusive so
                // filtered totals reconcile with subagent-inclusive cost.
                if !session.is_subagent {
                    stats.total_sessions_count += 1;
                    stats.total_session_days += 1;
                    if session.summary.is_some() {
                        stats.sessions_with_summary += 1;
                    }
                    crate::aggregator::StatsAggregator::add_session_adoption(
                        &mut stats,
                        session.day_tool_usage.keys(),
                    );
                }

                let work_tokens = session.work_tokens();
                stats.total_tokens.input_tokens += session.day_input_tokens;
                stats.total_tokens.output_tokens += session.day_output_tokens;

                for (model, tokens) in &session.day_tokens_by_model {
                    stats
                        .model_tokens
                        .entry(model.clone())
                        .or_default()
                        .merge(tokens);

                    stats.total_tokens.cache_creation_tokens += tokens.cache_creation_tokens;
                    stats.total_tokens.cache_creation_5m_tokens += tokens.cache_creation_5m_tokens;
                    stats.total_tokens.cache_creation_1h_tokens += tokens.cache_creation_1h_tokens;
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

#[cfg(test)]
mod filtered_stats_tests {
    use super::*;
    use crate::test_helpers::helpers::{
        make_daily_group, make_session_with_tokens, make_test_app_state,
    };

    // Under a filter, the rebuilt stats must INCLUDE subagent tokens (so the
    // token cards reconcile with the always-subagent-inclusive cost) while the
    // user-facing session COUNT excludes them — matching the unfiltered path.
    #[test]
    fn filtered_rebuild_includes_subagent_tokens_excludes_from_count() {
        let date = NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(); // lint-ok: date-literal
        let mut sub = make_session_with_tokens("~/proj", 5000, 2000, "claude-sonnet-4-20250514");
        sub.is_subagent = true;
        let group = make_daily_group(
            date,
            vec![
                make_session_with_tokens("~/proj", 1000, 500, "claude-sonnet-4-20250514"),
                sub,
            ],
        );
        let mut state = make_test_app_state(vec![group]);
        // A Custom range covering the date takes the filtered-rebuild path.
        state.period_filter = PeriodFilter::Custom(date, Some(date));
        state.apply_filter();
        assert_eq!(
            state.stats.total_tokens.input_tokens, 6000,
            "filtered tokens must include the subagent's input"
        );
        assert_eq!(state.stats.total_tokens.output_tokens, 2500);
        assert_eq!(
            state.stats.total_sessions_count, 1,
            "session count must exclude the subagent"
        );
    }

    // The per-model fold must carry the 5m/1h cache split — calculate_cost
    // prices ONLY those two fields (no fallback to the flat total), so a
    // hand-rolled fold that drops them zeroes the cache-write cost for
    // every model. Pins the rebuild path at its bug site.
    #[test]
    fn filtered_rebuild_keeps_per_model_cache_ttl_split() {
        let date = NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(); // lint-ok: date-literal
        let model = "claude-sonnet-4-20250514";
        let mut s = make_session_with_tokens("~/proj", 1000, 500, model);
        {
            let ts = s.day_tokens_by_model.get_mut(model).unwrap();
            ts.cache_creation_tokens = 300;
            ts.cache_creation_5m_tokens = 200;
            ts.cache_creation_1h_tokens = 100;
            ts.cache_read_tokens = 50;
        }
        let mut state = make_test_app_state(vec![make_daily_group(date, vec![s])]);
        state.period_filter = PeriodFilter::Custom(date, Some(date));
        state.apply_filter();
        let folded = &state.stats.model_tokens[model];
        assert_eq!(folded.cache_creation_5m_tokens, 200, "5m split dropped");
        assert_eq!(folded.cache_creation_1h_tokens, 100, "1h split dropped");
        assert_eq!(folded.cache_creation_tokens, 300);
        assert_eq!(folded.cache_read_tokens, 50);
    }

    #[test]
    fn sort_languages_defaults_to_recency_then_toggles_to_tokens() {
        // "Rust" was touched on the older day with many hits; "Python" on the
        // newer day with few. Recency (the default) must float Python despite
        // its lower count; toggling to Tokens flips to Rust.
        let older = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(); // lint-ok: date-literal
        let newer = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(); // lint-ok: date-literal
        let mut s_old = make_session_with_tokens("~/p", 1, 1, "claude-sonnet-4-20250514");
        s_old.day_language_usage.insert("Rust".to_string(), 100);
        let mut s_new = make_session_with_tokens("~/p", 1, 1, "claude-sonnet-4-20250514");
        s_new.day_language_usage.insert("Python".to_string(), 5);
        let mut state = make_test_app_state(vec![
            make_daily_group(older, vec![s_old]),
            make_daily_group(newer, vec![s_new]),
        ]);

        assert_eq!(state.dashboard_languages_sort, RankSort::Recency);
        let mut entries = vec![
            ("Rust".to_string(), 100usize, false),
            ("Python".to_string(), 5usize, false),
        ];
        state.sort_languages(&mut entries);
        assert_eq!(
            entries[0].0, "Python",
            "recency ranks the more recently used language first"
        );

        state.dashboard_languages_sort = RankSort::Tokens;
        state.sort_languages(&mut entries);
        assert_eq!(
            entries[0].0, "Rust",
            "tokens ranks the higher-count language first"
        );
    }
}
