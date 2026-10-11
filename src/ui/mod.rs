pub(crate) mod dashboard;
mod insights;

use std::sync::OnceLock;

use chrono::Local;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

use crate::aggregator::{CostCalculator, SessionInfo};
use crate::search;
use crate::{
    AppState, ConversationBlock, ConversationMessage, ConversationPane, LivePaneMode, SummaryType,
    Tab,
};

/// A session's displayed title: the shared `session_titles` cache (single
/// source, updated on rename) first, else the slice's own `display_title` for a
/// session not yet in the cache. Takes the map (not `&AppState`) so it borrows
/// one field and composes inside closures that also iterate the groups.
fn resolved_title<'a>(
    titles: &'a std::collections::HashMap<std::path::PathBuf, String>,
    s: &'a SessionInfo,
) -> Option<&'a str> {
    let cached = titles.get(&s.file_path).map(String::as_str);
    // A title the user set wins: on disk (`custom_title`), or a rename only the
    // cache holds so far (it then differs from the slice's own title).
    let user_named =
        s.custom_title.is_some() || cached.is_some_and(|t| Some(t) != s.display_title());
    // Otherwise ccsight's own generation calls keep their row but not their
    // prompt: the prompt text as a title reads like the user's work.
    if !user_named && let Some(label) = s.ccsight_generated() {
        return Some(label);
    }
    cached.or_else(|| s.display_title())
}

pub mod theme {
    use ratatui::style::Color;

    // Base: Terracotta orange
    pub const PRIMARY: Color = Color::Rgb(218, 119, 86);
    // Sage green (warm)
    pub const SUCCESS: Color = Color::Rgb(130, 166, 110);
    // Amber gold
    pub const WARNING: Color = Color::Rgb(210, 160, 70);
    // Terracotta red — moderate band; DANGER deepens just enough to read
    // as a distinct tier without going so dark it's hard to scan.
    pub const ERROR: Color = Color::Rgb(220, 110, 90);
    pub const DANGER: Color = Color::Rgb(180, 65, 70);
    // Bright pink-magenta — reserved for cost tiers above DANGER so
    // heavy-usage days stay visually distinct from merely-bad ones. A
    // darker variant gets lost against dark themes; this lighter mix
    // lifts above the surrounding rows.
    pub const CRITICAL: Color = Color::Rgb(220, 120, 175);
    // Teal accent (complement)
    pub const ACCENT: Color = Color::Rgb(86, 165, 180);

    // Neutral tones
    pub const WARM: Color = Color::Rgb(175, 145, 125);
    /// Barely above the background: fills, empty-state filler, decoration,
    /// and shortcut / rank digits kept quiet on purpose. Never for text that
    /// is meant to be read.
    pub const FAINT: Color = Color::Rgb(55, 50, 48);
    pub const BORDER: Color = Color::Rgb(90, 85, 80);
    pub const SEPARATOR: Color = Color::Rgb(60, 55, 50);
    pub const TEXT_BRIGHT: Color = Color::Rgb(240, 235, 230);
    pub const TEXT_DARK: Color = Color::Rgb(30, 28, 26);

    // Text grays stay private: two grays a few points apart read as one, so
    // picking by shade forks the same role across screens. Callers pick the
    // role below; a new role gets a name here, not a new gray.
    const GRAY_META: Color = Color::Rgb(125, 120, 115);
    const GRAY_LABEL: Color = Color::Rgb(140, 130, 120);
    const GRAY_VALUE: Color = Color::Rgb(168, 154, 140);

    /// Clock times, dates, durations and "N ago".
    pub const TIME: Color = GRAY_META;
    /// A time inside the last few minutes.
    pub const TIME_RECENT: Color = ACCENT;
    pub const TOKENS: Color = PRIMARY;
    /// A grand total, month-to-date or forecast (`cost_total_style`).
    pub const COST_TOTAL: Color = WARM;
    pub const PROJECT: Color = WARM;
    pub const SESSION_ID: Color = GRAY_META;
    pub const TITLE: Color = TEXT_BRIGHT;
    /// Quoted message text: last prompt, previews, snippets.
    pub const SNIPPET: Color = GRAY_LABEL;
    /// Counts, percentages and other figures beside the main number.
    pub const META: Color = GRAY_META;
    /// Field names, captions, footer action words, inline hints.
    pub const LABEL: Color = GRAY_LABEL;
    /// A field's value; brighter than its `LABEL`.
    pub const VALUE: Color = GRAY_VALUE;
    /// Section headings inside a popup (caller adds bold).
    pub const HEADER: Color = PRIMARY;
    /// Empty, loading and truncated-content notices.
    pub const PLACEHOLDER: Color = GRAY_LABEL;
    /// A row kept for completeness that must not compete with its neighbours.
    pub const SUBDUED: Color = GRAY_META;
    pub const SELECTED_BG: Color = FAINT;

    // Model colors
    pub const MODEL_OPUS: Color = Color::Rgb(170, 120, 200);
    pub const MODEL_SONNET: Color = Color::Rgb(100, 160, 210);
    pub const MODEL_HAIKU: Color = Color::Rgb(130, 190, 160);
    /// Fable and Mythos, the top tier. Lighter and yellower than `WARNING`'s
    /// amber so a priced top-tier name never reads as an unpriced one.
    pub const MODEL_FABLE: Color = Color::Rgb(240, 205, 110);

    // Special elements
    pub const BRANCH: Color = Color::Rgb(120, 140, 170);
    pub const LINK: Color = Color::Rgb(100, 140, 180);
    pub const THINKING: Color = Color::Rgb(140, 125, 165);
    pub const SEARCH_MATCH: Color = Color::Rgb(60, 55, 35);
    pub const SEARCH_CURRENT: Color = Color::Rgb(130, 110, 70);
    pub const SELECTION: Color = Color::Rgb(60, 60, 120);

    // Heatmap (terracotta gradient)
    pub const HEATMAP_EMPTY: Color = Color::Rgb(35, 32, 30);
    pub const HEATMAP_LOW: Color = Color::Rgb(80, 55, 45);
    pub const HEATMAP_MID: Color = Color::Rgb(140, 85, 65);
    pub const HEATMAP_HIGH: Color = Color::Rgb(200, 110, 80);

    // Ecosystem category palette — single source of truth across all
    // surfaces (Dashboard preview, popup tabs/body, Insights, Daily).
    // Use these aliases, not the raw SUCCESS/WARNING/... names.
    // Tools = BUILTIN (the umbrella's dominant content); MCP keeps its
    // distinct ACCENT hue for sub-row differentiation.
    pub const CAT_TOOLS: Color = SUCCESS;
    pub const CAT_BUILTIN: Color = SUCCESS;
    pub const CAT_MCP: Color = ACCENT;
    pub const CAT_SKILLS: Color = WARNING;
    pub const CAT_COMMANDS: Color = GRAY_VALUE;
    pub const CAT_SUBAGENTS: Color = LINK;

    // PRIMARY color base values for dynamic intensity
    pub const PRIMARY_R: f64 = 218.0;
    pub const PRIMARY_G: f64 = 119.0;
    pub const PRIMARY_B: f64 = 86.0;

    pub fn primary_with_intensity(intensity: f64) -> Color {
        Color::Rgb(
            // lint-ok: raw-rgb — palette constructor
            (PRIMARY_R * intensity) as u8,
            (PRIMARY_G * intensity) as u8,
            (PRIMARY_B * intensity) as u8,
        )
    }

    /// `(base, per-channel range)` for the intensity ramps behind category
    /// bar charts: `ramp_color` = base + range × intensity per channel.
    /// Defined here so a panel's bar hue can't fork from its popup twin.
    pub type Ramp = ((f64, f64, f64), (f64, f64, f64));
    pub const RAMP_PURPLE: Ramp = ((140.0, 100.0, 180.0), (78.0, 68.0, 75.0));
    pub const RAMP_BLUE: Ramp = ((100.0, 140.0, 200.0), (118.0, 78.0, 55.0));
    pub const RAMP_DEEP_TEAL: Ramp = ((40.0, 80.0, 90.0), (46.0, 85.0, 90.0));
    pub const RAMP_TEAL: Ramp = ((80.0, 160.0, 180.0), (100.0, 58.0, 75.0));
    pub const RAMP_OLIVE: Ramp = ((150.0, 180.0, 100.0), (68.0, 38.0, 55.0));

    pub fn ramp_color(ramp: Ramp, intensity: f64) -> Color {
        let ((br, bg, bb), (rr, rg, rb)) = ramp;
        Color::Rgb(
            // lint-ok: raw-rgb — the one constructor the ramps flow through
            (br + rr * intensity) as u8,
            (bg + rg * intensity) as u8,
            (bb + rb * intensity) as u8,
        )
    }

    /// A bar's fill ratio (0.0-1.0) to `primary_with_intensity` input: floors
    /// at 0.3 so a bar with little relative weight still reads as colored
    /// rather than near-black, and caps at 1.0 for `ratio` > 1 (a value
    /// exceeding its own max, e.g. a live total ticking past a cached peak).
    pub fn bar_intensity(ratio: f64) -> f64 {
        (ratio * 0.7 + 0.3).min(1.0) // lint #46: the one sanctioned site
    }
}

/// Canonical color for any tool key, dispatched on category. Use this in
/// every Ecosystem rendering site (popup body, Top tools, Daily breakdown,
/// preview Tier 2) so colors stay in lockstep across views.
pub fn tool_category_color(name: &str) -> ratatui::style::Color {
    use crate::aggregator::{ToolCategory, classify_tool};
    match classify_tool(name) {
        ToolCategory::BuiltIn => theme::CAT_BUILTIN,
        ToolCategory::Mcp { .. } => theme::CAT_MCP,
        ToolCategory::Skill { .. } => theme::CAT_SKILLS,
        ToolCategory::Command { .. } => theme::CAT_COMMANDS,
        ToolCategory::Agent { .. } => theme::CAT_SUBAGENTS,
    }
}

#[derive(Clone)]
pub enum BreakdownItem {
    Project(String, u64, f64),
    Model(String, u64, f64),
    Tool(String, usize, f64),
}

/// Truncate so the result fits within `max_width` columns, appending `…`
/// when truncation occurred. Returns the original string when it already
/// fits. `max_width` < 1 produces an empty string.
pub use crate::text::truncate_with_ellipsis;

/// Split `text` into spans, styling case-insensitive occurrences of any
/// query term with `hit` and the rest with `base`, so a search snippet
/// shows at a glance WHY it matched. Lowercasing can change byte lengths
/// outside ASCII, so matches are found in a per-char lowercase buffer and
/// mapped back to original offsets — snippets routinely carry `—` / CJK.
fn highlight_terms(text: &str, query: &str, base: Style, hit: Style) -> Vec<Span<'static>> {
    let terms: Vec<String> = query
        .split_whitespace()
        .filter(|t| t.chars().count() >= 2)
        .map(str::to_lowercase)
        .collect();
    if terms.is_empty() {
        return vec![Span::styled(text.to_string(), base)];
    }

    // Lowercase buffer + byte map: `starts[i]` = original byte offset of the
    // char that produced lower byte `i`, `ends[i]` = that char's end offset.
    let mut lower = String::new();
    let mut starts: Vec<usize> = Vec::new();
    let mut ends: Vec<usize> = Vec::new();
    for (pos, ch) in text.char_indices() {
        let end = pos + ch.len_utf8();
        for lc in ch.to_lowercase() {
            let from = lower.len();
            lower.push(lc);
            for _ in from..lower.len() {
                starts.push(pos);
                ends.push(end);
            }
        }
    }

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cur = 0usize; // original-text byte offset
    let mut lcur = 0usize; // lower-buffer byte offset
    while lcur < lower.len() {
        let next = terms
            .iter()
            .filter_map(|t| {
                lower[lcur..]
                    .find(t.as_str())
                    .map(|off| (lcur + off, t.len()))
            })
            .min_by_key(|&(s, _)| s);
        match next {
            Some((ls, llen)) => {
                let (os, oe) = (starts[ls], ends[ls + llen - 1]);
                if os > cur {
                    spans.push(Span::styled(text[cur..os].to_string(), base));
                }
                spans.push(Span::styled(text[os..oe].to_string(), hit));
                cur = oe;
                // Advance the lower cursor past every lower byte produced by
                // the matched chars (a char's lowercase can span several).
                lcur = starts.partition_point(|&s| s < oe);
            }
            None => break,
        }
    }
    if cur < text.len() {
        spans.push(Span::styled(text[cur..].to_string(), base));
    }
    spans
}

/// Strip a project path down to its basename. Reserved for AI prompt
/// generation (`summary.rs`) and for the `state.project_label` fallback —
/// render paths must call `state.project_label(name)` instead so two
/// projects that share a basename stay distinguishable. Lint #24 enforces.
pub(crate) fn shorten_project(name: &str) -> &str {
    std::path::Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(name)
}

pub(crate) use crate::aggregator::{
    aggregate_monthly_costs, aggregate_monthly_tokens, aggregate_weekday_avg,
};

/// Whether any session matching `pick` used a model without a price — the
/// figure summed over them is then a lower bound and must carry the mark.
pub(crate) fn any_unpriced(
    groups: &[crate::aggregator::DailyGroup],
    pick: impl Fn(&crate::aggregator::DailyGroup, &SessionInfo) -> bool,
) -> bool {
    let calculator = crate::aggregator::CostCalculator::global();
    groups
        .iter()
        .flat_map(|g| g.sessions.iter().map(move |s| (g, s)))
        .any(|(g, s)| pick(g, s) && s.has_unpriced_model(calculator))
}

/// [`any_unpriced`] over one `"YYYY-MM"` month.
pub(crate) fn month_unpriced(groups: &[crate::aggregator::DailyGroup], key: &str) -> bool {
    use chrono::Datelike;
    let Some((y, m)) = key
        .split_once('-')
        .and_then(|(y, m)| Some((y.parse::<i32>().ok()?, m.parse::<u32>().ok()?)))
    else {
        return false;
    };
    any_unpriced(groups, |g, _| g.date.year() == y && g.date.month() == m)
}

/// Cost colors live in one module so the per-day tier scale can't be called
/// bare: an unpriced figure has to be marked, and a multi-day sum has to use
/// the total color instead of tiers it always tops.
mod cost_color {
    use super::{Modifier, Style, theme};

    /// Five tiers tuned for one day's or one session's spend.
    fn tier(cost: f64) -> Style {
        let c = cost.max(0.0);
        let color = if c > 300.0 {
            theme::CRITICAL
        } else if c > 100.0 {
            theme::DANGER
        } else if c > 60.0 {
            theme::ERROR
        } else if c > 20.0 {
            theme::WARNING
        } else {
            theme::SUCCESS
        };
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    }

    /// Style companion to [`super::format_cost_marked`]: unknown-pricing
    /// figures render in the warning color instead of the tier scale, which
    /// would paint a fake zero as cheap green.
    pub(crate) fn cost_style_marked(cost: f64, unpriced: bool) -> Style {
        if unpriced {
            Style::default().fg(theme::WARNING)
        } else {
            tier(cost)
        }
    }

    /// A grand total, month-to-date or forecast figure.
    pub(crate) fn cost_total_style(unpriced: bool) -> Style {
        let color = if unpriced {
            theme::WARNING
        } else {
            theme::COST_TOTAL
        };
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    }

    #[cfg(test)]
    pub(super) fn cost_style(cost: f64) -> Style {
        tier(cost)
    }
}
#[cfg(test)]
use cost_color::cost_style;
pub(crate) use cost_color::{cost_style_marked, cost_total_style};

/// Cost display that surfaces unknown pricing instead of a silent $0: "$?"
/// when the whole figure is unknown, `format_cost` + "*" when only part of
/// the session's models are priced (the figure is a lower bound). Pair with
/// [`cost_style_marked`] so the mark also reads as a warning.
pub(crate) fn format_cost_marked(cost: f64, unpriced: bool, precision: usize) -> String {
    if !unpriced {
        return format_cost(cost, precision);
    }
    if cost == 0.0 {
        "$?".to_string()
    } else {
        format!("{}*", format_cost(cost, precision))
    }
}

/// Bottom bar for a tab. Key dispatch is exclusive, so while a blocking popup
/// is open none of the tab's keys reach a handler — listing them contradicts
/// the popup's own footer. `Esc` is the one key every blocking popup accepts;
/// the rest belong on that popup's bottom border.
pub(crate) fn tab_help_bar(state: &AppState, pairs: &[(&str, &str)]) -> Line<'static> {
    // Search mode lives outside `ActivePopup`, so the blocking check below
    // cannot see it — yet every printable key goes into the query, which makes
    // a tab's own bar the most misleading thing on screen.
    if state.search_mode {
        return help_bar(&[("Enter", "open"), ("↑↓", "results"), ("Esc", "back")]);
    }
    if crate::has_blocking_popup(state) {
        return help_bar(&[("Esc", "close")]);
    }
    help_bar(pairs)
}

/// Bottom help bar from `(key, action)` pairs, in the documented format:
/// `key:action`, single-space separator, leading space on the first key, no
/// trailing space after the last action (the canonical footer format).
/// Every tab's bar routes through here so a new keybind can't pick up a
/// per-tab styling or spacing variant.
pub(crate) fn help_bar(pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::with_capacity(pairs.len() * 2);
    for (i, (key, action)) in pairs.iter().enumerate() {
        let key_txt = if i == 0 {
            format!(" {key}")
        } else {
            (*key).to_string()
        };
        let action_txt = if i + 1 == pairs.len() {
            format!(":{action}")
        } else {
            format!(":{action} ")
        };
        spans.push(Span::styled(key_txt, Style::default().fg(theme::PRIMARY)));
        spans.push(Span::styled(action_txt, Style::default().fg(theme::LABEL)));
    }
    Line::from(spans)
}

/// Standard overlay-popup frame: themed border + bold themed title
/// (single construction site for the popup frame). Callers pass the title
/// with its canonical ` title ` spacing. Popups with non-standard title
/// styling build their own Block; every other overlay popup routes
/// through here so border/title styling can't drift per-site.
pub(super) fn popup_block(title: &str) -> Block<'static> {
    let accent = Style::default().fg(theme::PRIMARY);
    let frame = Block::default().borders(Borders::ALL).border_style(accent);
    frame.title(Span::styled(title.to_owned(), accent.bold()))
}

pub(crate) fn format_cost(cost: f64, precision: usize) -> String {
    let c = cost.max(0.0);
    // Compact callers pass `precision = 0`. To preserve two significant
    // figures across magnitudes, decimal digits scale down as the value
    // grows: sub-dollar → 2 decimals, single-digit dollars → 1 decimal,
    // ≥10 dollars → integer. Detailed callers (`precision = 2`) always
    // get two decimals.
    if precision == 0 && c > 0.0 {
        if c < 1.0 {
            return format!("${c:.2}");
        }
        if c < 10.0 {
            return format!("${c:.1}");
        }
    }
    match precision {
        0 => format!("${c:.0}"),
        _ => format!("${c:.2}"),
    }
}

pub(crate) fn calc_scroll(
    area_height: u16,
    item_count: usize,
    scroll: usize,
    header: u16,
) -> (usize, usize, usize) {
    let visible = area_height.saturating_sub(header) as usize;
    let max_scroll = item_count.saturating_sub(visible);
    (visible, max_scroll, scroll.min(max_scroll))
}

pub fn model_color(model: &str) -> Color {
    // Call sites pass either the raw id (`claude-opus-5`) or the normalized
    // display name (`Opus 5`) — whichever the surrounding map is keyed by. A
    // case-sensitive match greys out every row on the display-name side.
    let m = model.to_ascii_lowercase();
    if m.contains("opus") {
        theme::MODEL_OPUS
    } else if m.contains("sonnet") {
        theme::MODEL_SONNET
    } else if m.contains("haiku") {
        theme::MODEL_HAIKU
    } else if m.contains("fable") || m.contains("mythos") {
        theme::MODEL_FABLE
    } else {
        theme::LABEL
    }
}

fn draw_scrollbar(frame: &mut Frame, area: Rect, scroll: usize, total: usize, visible: usize) {
    if total <= visible || area.height < 3 {
        return;
    }

    let track_height = area.height.saturating_sub(2) as usize;
    if track_height == 0 {
        return;
    }

    let thumb_size = ((visible as f64 / total as f64) * track_height as f64)
        .ceil()
        .max(1.0) as usize;
    let thumb_size = thumb_size.min(track_height);

    let max_scroll = total.saturating_sub(visible);
    let thumb_pos = if max_scroll > 0 {
        ((scroll as f64 / max_scroll as f64) * (track_height - thumb_size) as f64).round() as usize
    } else {
        0
    };

    let scrollbar_x = area.x + area.width.saturating_sub(1);
    for i in 0..track_height {
        let y = area.y + 1 + i as u16;
        let ch = if i >= thumb_pos && i < thumb_pos + thumb_size {
            "█"
        } else {
            "░"
        };
        let span = Span::styled(ch, Style::default().fg(theme::LABEL));
        frame.render_widget(Paragraph::new(span), Rect::new(scrollbar_x, y, 1, 1));
    }
}

fn get_cat_n_pattern() -> &'static regex::Regex {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| regex::Regex::new(r"^\s*\d+[→\t]").unwrap())
}

fn get_syntax_set() -> &'static SyntaxSet {
    static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn get_theme_set() -> &'static ThemeSet {
    static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();
    THEME_SET.get_or_init(ThemeSet::load_defaults)
}

pub fn warmup_syntax_highlighting() {
    let _ = get_syntax_set();
    let _ = get_theme_set();
}

pub use crate::conversation::load_conversation;
pub use crate::text::{TextSegment, parse_text_with_code_blocks};

fn syntect_to_ratatui_color(color: syntect::highlighting::Color) -> Color {
    Color::Rgb(color.r, color.g, color.b) // lint-ok: raw-rgb — syntect conversion
}

/// Turn counts split by who produced them. Claude Code files every tool
/// result under the user role, so a bare user-message count reads as hundreds
/// of turns for a session of a few dozen prompts.
fn turns_text(prompts: u64, assistant: u64, user_role: u64) -> String {
    format!(
        "you:{prompts}  AI:{assistant}  tool/system:{}",
        user_role.saturating_sub(prompts)
    )
}

/// `line` recoloured subdued — for a row kept for completeness that should
/// not compete with the rows around it, yet stays readable.
fn subdued_line(line: Line<'_>) -> Line<'_> {
    Line::from(
        line.spans
            .into_iter()
            .map(|sp| sp.style(Style::default().fg(theme::SUBDUED)))
            .collect::<Vec<_>>(),
    )
}

/// Display width of a composed span run. Callers that decide layout from the
/// text they just built must measure the built spans, never a second copy of
/// the same literals: two copies drift apart silently.
pub(crate) fn spans_width(spans: &[Span<'_>]) -> usize {
    spans
        .iter()
        .map(|sp| unicode_width::UnicodeWidthStr::width(sp.content.as_ref()))
        .sum()
}

/// Cold-start file progress for the splash. Occupies a line that is otherwise
/// blank, so showing it costs no height: a taller splash would need the
/// centring block resized with it. Empty until the walk publishes a total.
fn progress_spans(done: usize, total: usize) -> Vec<Span<'static>> {
    if total == 0 {
        return Vec::new();
    }
    vec![Span::styled(
        format!("{done} / {total} files"),
        Style::default().fg(theme::META),
    )]
}

fn truncate_spans(spans: Vec<Span<'static>>, max_width: usize) -> Vec<Span<'static>> {
    use unicode_width::UnicodeWidthChar;
    let mut remaining = max_width;
    let mut result = Vec::new();
    for span in spans {
        if remaining == 0 {
            break;
        }
        let mut width = 0;
        let mut truncated = String::new();
        for ch in span.content.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if width + ch_width > remaining {
                break;
            }
            truncated.push(ch);
            width += ch_width;
        }
        remaining -= width;
        if !truncated.is_empty() {
            result.push(Span::styled(truncated, span.style));
        }
    }
    result
}

fn highlight_xml_tags(line: &str) -> Line<'static> {
    if !line.contains('<') {
        return Line::from(line.to_string());
    }
    let mut spans = Vec::new();
    let mut last = 0;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<'
            && let Some(end) = line[i..].find('>')
        {
            let tag = &line[i..i + end + 1];
            if tag.len() >= 3
                && (tag.as_bytes()[1].is_ascii_alphabetic() || tag.as_bytes()[1] == b'/')
            {
                if last < i {
                    spans.push(Span::raw(line[last..i].to_string()));
                }
                spans.push(Span::styled(
                    tag.to_string(),
                    Style::default().fg(theme::LABEL),
                ));
                last = i + end + 1;
                i = last;
                continue;
            }
        }
        i += 1;
    }
    if last < line.len() {
        spans.push(Span::raw(line[last..].to_string()));
    }
    if spans.is_empty() {
        Line::from(line.to_string())
    } else {
        Line::from(spans)
    }
}

fn highlight_code_line(
    line: &str,
    highlighter: &mut HighlightLines,
    syntax_set: &SyntaxSet,
) -> Vec<Span<'static>> {
    let cat_n_pattern = get_cat_n_pattern();

    let (prefix, code_part) = if let Some(mat) = cat_n_pattern.find(line) {
        let prefix = &line[..mat.end()];
        let code = &line[mat.end()..];
        (
            Some(Span::styled(
                prefix.to_string(),
                Style::default().fg(theme::LABEL),
            )),
            code,
        )
    } else {
        (None, line)
    };

    let mut spans = Vec::new();
    if let Some(p) = prefix {
        spans.push(p);
    }

    if let Ok(highlighted) = highlighter.highlight_line(code_part, syntax_set) {
        for (style, text) in highlighted {
            spans.push(Span::styled(
                text.to_string(),
                Style::default().fg(syntect_to_ratatui_color(style.foreground)),
            ));
        }
    } else {
        spans.push(Span::raw(code_part.to_string()));
    }

    spans
}

pub fn render_tool_result_with_highlighting(
    content: &str,
    max_width: usize,
) -> (Vec<Line<'static>>, Vec<bool>) {
    use crate::text::wrap_text_with_continuation;

    let syntax_set = get_syntax_set();
    let theme_set = get_theme_set();
    let theme = &theme_set.themes["base16-ocean.dark"];

    let mut lines = Vec::new();
    let mut wrap_flags = Vec::new();
    let cat_n_pattern = get_cat_n_pattern();

    let has_line_numbers = content.lines().take(3).any(|l| cat_n_pattern.is_match(l));

    if has_line_numbers {
        let extension = content
            .lines()
            .find_map(|l| {
                if let Some(mat) = cat_n_pattern.find(l) {
                    let after = &l[mat.end()..];
                    if after.contains("fn ") || after.contains("let ") || after.contains("impl ") {
                        return Some("rs");
                    }
                    if after.contains("function") || after.contains("const ") {
                        return Some("js");
                    }
                    if after.contains("def ") || after.contains("import ") {
                        return Some("py");
                    }
                }
                None
            })
            .unwrap_or("txt");

        let syntax = syntax_set
            .find_syntax_by_extension(extension)
            .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
        let mut highlighter = HighlightLines::new(syntax, theme);

        for line in content.lines().take(50) {
            let spans = highlight_code_line(line, &mut highlighter, syntax_set);
            lines.push(Line::from(spans));
            wrap_flags.push(false);
        }
        if content.lines().count() > 50 {
            lines.push(Line::from(Span::styled(
                format!("... ({} more lines)", content.lines().count() - 50),
                Style::default().fg(theme::PLACEHOLDER),
            )));
            wrap_flags.push(false);
        }
    } else {
        let (wrapped, flags) = wrap_text_with_continuation(content, max_width);
        for line in wrapped.into_iter().take(30) {
            lines.push(Line::from(Span::styled(
                line,
                Style::default().fg(theme::VALUE),
            )));
        }
        wrap_flags.extend(flags.into_iter().take(30));
    }

    (lines, wrap_flags)
}

pub fn render_text_with_highlighting(
    text: &str,
    max_width: usize,
) -> (Vec<Line<'static>>, Vec<bool>) {
    use crate::text::wrap_text_with_continuation;

    let syntax_set = get_syntax_set();
    let theme_set = get_theme_set();
    let theme = &theme_set.themes["base16-ocean.dark"];

    let segments = parse_text_with_code_blocks(text);
    let mut lines = Vec::new();
    let mut wrap_flags = Vec::new();

    for segment in segments {
        match segment {
            TextSegment::Plain(plain) => {
                let (wrapped, flags) = wrap_text_with_continuation(&plain, max_width);
                for line in wrapped {
                    lines.push(highlight_xml_tags(&line));
                }
                wrap_flags.extend(flags);
            }
            TextSegment::Code { lang, content } => {
                lines.push(Line::from(Span::styled(
                    format!("```{}", lang.as_deref().unwrap_or("")),
                    Style::default().fg(theme::LABEL),
                )));
                wrap_flags.push(false);

                let ext = lang.as_deref().unwrap_or("txt");
                let syntax = syntax_set
                    .find_syntax_by_extension(ext)
                    .or_else(|| syntax_set.find_syntax_by_name(ext))
                    .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
                let mut highlighter = HighlightLines::new(syntax, theme);

                for code_line in content.lines().take(30) {
                    let spans = highlight_code_line(code_line, &mut highlighter, syntax_set);
                    lines.push(Line::from(spans));
                    wrap_flags.push(false);
                }
                if content.lines().count() > 30 {
                    lines.push(Line::from(Span::styled(
                        format!("... ({} more lines)", content.lines().count() - 30),
                        Style::default().fg(theme::PLACEHOLDER),
                    )));
                    wrap_flags.push(false);
                }

                lines.push(Line::from(Span::styled(
                    "```",
                    Style::default().fg(theme::LABEL),
                )));
                wrap_flags.push(false);
            }
        }
    }

    (lines, wrap_flags)
}

pub fn is_tool_only_message(msg: &ConversationMessage) -> bool {
    !msg.blocks.is_empty()
        && msg.blocks.iter().all(|b| {
            matches!(
                b,
                ConversationBlock::ToolUse { .. } | ConversationBlock::ToolResult { .. }
            )
        })
}

pub fn is_thinking_only_message(msg: &ConversationMessage) -> bool {
    !msg.blocks.is_empty()
        && msg
            .blocks
            .iter()
            .all(|b| matches!(b, ConversationBlock::Thinking(_)))
}

pub fn extract_message_text(msg: &ConversationMessage) -> String {
    let mut parts: Vec<String> = Vec::new();

    for block in &msg.blocks {
        match block {
            ConversationBlock::Text(text) => {
                parts.push(text.clone());
            }
            ConversationBlock::ToolUse {
                name,
                input_summary,
            } => {
                parts.push(format!("[Tool: {name}] {input_summary}"));
            }
            ConversationBlock::ToolResult { content, is_error } => {
                let prefix = if *is_error { "[Error] " } else { "" };
                parts.push(format!("{prefix}{content}"));
            }
            ConversationBlock::Thinking(text) => {
                parts.push(format!("[Thinking] {text}"));
            }
        }
    }

    parts.join("\n\n")
}

/// Case-insensitive occurrence count of `query_lower` in `text`.
/// Char-based (not byte) so CJK / accented text can't split a hit.
fn count_query_occurrences(text: &str, query_lower: &str) -> usize {
    query_char_ranges(text, query_lower).len()
}

/// Char-index ranges of every `query_lower` occurrence in `text`,
/// lowercase-folded per char so non-ASCII input matches too.
fn query_char_ranges(text: &str, query_lower: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if query_lower.is_empty() {
        return out;
    }
    let hay: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    // Lowercase folding can expand a char (e.g. 'İ'); map folded positions
    // back to original char indices so span splitting stays aligned.
    let mut fold_to_orig = Vec::with_capacity(hay.len());
    for (i, ch) in text.chars().enumerate() {
        for _ in ch.to_lowercase() {
            fold_to_orig.push(i);
        }
    }
    let needle: Vec<char> = query_lower.chars().collect();
    if needle.is_empty() || hay.len() < needle.len() {
        return out;
    }
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if hay[i..i + needle.len()] == needle[..] {
            let start = fold_to_orig[i];
            let end = fold_to_orig[i + needle.len() - 1] + 1;
            out.push((start, end));
            i += needle.len();
        } else {
            i += 1;
        }
    }
    out
}

fn first_match_from_day(
    messages: &[crate::conversation::ConversationMessage],
    matches: &[(usize, usize)],
    day: chrono::NaiveDate,
) -> Option<usize> {
    matches.iter().position(|&(m, _)| {
        messages[m]
            .timestamp_utc
            .is_some_and(|t| t.with_timezone(&chrono::Local).date_naive() >= day)
    })
}

/// Post-edit refresh shared by every pane-search text mutation (typing,
/// Backspace, paste): consume the selection, recompute matches, restart at
/// the first one, and arm the jump. One path so the sites can't drift.
pub fn on_pane_search_edit(pane: &mut ConversationPane) {
    pane.search_select_all = false;
    update_pane_search_matches(pane);
    pane.search_current = 0;
    pane.pending_search_scroll = !pane.search_matches.is_empty();
}

/// Recompute `(message_idx, occurrence_idx)` matches over the LOGICAL
/// message text — collapsed compact messages stay searchable without
/// forcing the pane to full mode. Callers reset `search_current` on text
/// change; here we only clamp it into the new bounds.
pub fn update_pane_search_matches(pane: &mut ConversationPane) {
    let query_lower = pane.search_input.text.to_lowercase();
    pane.search_matches.clear();
    if !query_lower.is_empty() {
        for (msg_idx, msg) in pane.messages.iter().enumerate() {
            let n = count_query_occurrences(&msg.search_text(), &query_lower);
            for k in 0..n {
                pane.search_matches.push((msg_idx, k));
            }
        }
    }
    if pane.search_matches.is_empty() {
        pane.search_current = 0;
    } else if pane.search_current >= pane.search_matches.len() {
        pane.search_current = pane.search_matches.len() - 1;
    }
}

/// The `expanded`-set key that opens the compact row holding message `m`:
/// consecutive tool-only messages fold into one row keyed by the run's
/// FIRST message, so peeking any member must insert the run head.
fn compact_expand_key(messages: &[crate::ConversationMessage], m: usize) -> usize {
    if messages.get(m).is_none_or(|msg| !is_tool_only_message(msg)) {
        return m;
    }
    let mut start = m;
    while start > 0 && is_tool_only_message(&messages[start - 1]) {
        start -= 1;
    }
    start
}

/// Rendered position of message `m`'s `k`-th query occurrence:
/// `(line_idx, Some(occ_within_line))`, or `(first_line_of_m, None)` when
/// the occurrence isn't in the rendered text (collapsed summary /
/// renderer truncation) so the jump still lands on the message.
fn resolve_match_line(
    lines: &[ratatui::text::Line<'_>],
    message_lines: &[(usize, usize)],
    total_lines: usize,
    m: usize,
    k: usize,
    query_lower: &str,
) -> Option<(usize, Option<usize>)> {
    // Rows fold tool runs, so the row holding `m` is the last row whose
    // msg_idx <= m; its line span ends where the next row starts.
    let row = message_lines.iter().rposition(|&(_, idx)| idx <= m)?;
    let start = message_lines[row].0;
    let end = message_lines
        .get(row + 1)
        .map_or(total_lines, |&(line, _)| line);
    let mut seen = 0usize;
    let mut last_hit: Option<(usize, usize)> = None;
    for (li, line) in lines.iter().enumerate().take(end).skip(start) {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let n = query_char_ranges(&text, query_lower).len();
        if n > 0 {
            last_hit = Some((li, n - 1));
        }
        if seen + n > k {
            return Some((li, Some(k - seen)));
        }
        seen += n;
    }
    // Occurrence `k` isn't in the rendered text (truncated preview or a
    // wrap-split hit): clamp the current-match highlight to the message's
    // last rendered occurrence so navigation never goes visually dark.
    if last_hit.is_some() {
        return last_hit.map(|(li, occ)| (li, Some(occ)));
    }
    Some((start, None))
}

/// Split styled spans at the char positions in `ranges` and paint match
/// backgrounds: every range dim, the `strong`-th range as the current
/// match. Splitting preserves each span's own fg styling.
fn apply_bg_ranges(
    spans: &[Span<'_>],
    ranges: &[(usize, usize)],
    strong: Option<usize>,
) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    for span in spans {
        let chars: Vec<char> = span.content.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            let abs = pos + i;
            match ranges.iter().position(|&(s, e)| abs >= s && abs < e) {
                Some(ri) => {
                    let (_, e) = ranges[ri];
                    let take = (e - abs).min(chars.len() - i);
                    let seg: String = chars[i..i + take].iter().collect();
                    let bg = if strong == Some(ri) {
                        theme::SEARCH_CURRENT
                    } else {
                        theme::SEARCH_MATCH
                    };
                    out.push(Span::styled(seg, span.style.bg(bg)));
                    i += take;
                }
                None => {
                    let next = ranges
                        .iter()
                        .filter(|&&(s, _)| s > abs)
                        .map(|&(s, _)| s)
                        .min()
                        .unwrap_or(usize::MAX);
                    let take = next.saturating_sub(abs).min(chars.len() - i);
                    let seg: String = chars[i..i + take].iter().collect();
                    out.push(Span::styled(seg, span.style));
                    i += take;
                }
            }
        }
        pos += chars.len();
    }
    out
}

pub fn draw(frame: &mut Frame, state: &mut AppState) {
    let area = frame.area();
    state.layout.active_popup_area = None;
    state.layout.tools_detail_tab_areas.clear();
    state.layout.tools_panel_category_areas.clear();
    state.layout.mcp_server_row_areas.clear();
    state.layout.project_detail_row_areas.clear();

    let show_warning =
        state.retention_warning.is_some() && !state.retention_warning_dismissed && !state.loading;
    let warning_height = if show_warning { 4 } else { 0 };

    let chunks = Layout::vertical([
        Constraint::Length(1),              // Header
        Constraint::Length(warning_height), // Warning banner
        Constraint::Length(1),              // Tabs
        Constraint::Min(0),                 // Content
    ])
    .split(area);

    // Only the header renders during the initial load; the splash owns the
    // rest. The tab bar is suppressed because Daily / Insights are empty
    // until the parse finishes (a "0 sessions" view reads as real data),
    // and there's nothing to switch to yet.
    draw_header(frame, chunks[0], state);

    if show_warning {
        draw_retention_warning(frame, chunks[1], state);
    }

    if state.loading {
        let f = state.animation_frame;

        // Timing - balanced speed
        let slow = f / 2;

        // Elegant color palette
        let primary = theme::PRIMARY;
        let warm = theme::WARM;
        let dim = theme::LABEL;
        let faint = theme::FAINT;

        // Logo with creative animation. Lines are padded to a common width so
        // `Paragraph::centered()` lines them up at the same column even though
        // the cloud silhouette is asymmetric.
        let logo_lines = [" ▐▛███▜▌ ", "▝▜█████▛▘", "  ▘▘ ▝▝  "];

        // Random fragment characters for the chaos phase
        let fragment_chars = ['░', '▒', '▓', '█', '▄', '▀', '▌', '▐', '▖', '▗', '▘', '▝'];

        // Build logo from random fragments assembling into final form
        // Animation cycle: 0-5 blank, 5-20 chaos starts, 20-80 lock-in, 80-120 visible, 120-150 fade out
        let cycle_total = 150;
        let cycle = slow % cycle_total;
        let fade_start = 120;

        let build_logo_line = |line: &str, line_idx: usize| -> Vec<Span> {
            let chars: Vec<char> = line.chars().collect();

            chars
                .iter()
                .enumerate()
                .map(|(i, &c)| {
                    if c == ' ' {
                        return Span::styled(" ", Style::default());
                    }

                    // Pseudo-random seed for this character position
                    let seed = (i * 17 + line_idx * 31 + 7) % 100;
                    // Character locks in at this frame (spread across frames 20-80)
                    let lock_frame = 20 + (seed * 60 / 100);

                    // Glitch-out phase (frames 120-150): characters randomly break
                    if cycle >= fade_start {
                        let glitch_progress = (cycle - fade_start) as f32 / 30.0;
                        // Each character breaks at a different time based on seed
                        let break_time = (seed as f32 / 100.0) * 0.8;

                        if glitch_progress < break_time {
                            // Not broken yet - show normally
                            let color = theme::primary_with_intensity(1.0);
                            return Span::styled(c.to_string(), Style::default().fg(color));
                        }

                        // Post-break: progressively corrupt
                        let broken_duration = glitch_progress - break_time;
                        let glitch_state = (slow + i * 29 + line_idx * 41) % 100;

                        if broken_duration > 0.5 {
                            // Fully broken - gone most of the time
                            if glitch_state < 10 {
                                let idx = (slow + i * 11) % fragment_chars.len();
                                let ch = fragment_chars[idx];
                                Span::styled(
                                    ch.to_string(),
                                    Style::default().fg(theme::primary_with_intensity(0.3)),
                                )
                            } else {
                                Span::styled(" ", Style::default())
                            }
                        } else {
                            // Flickering broken state - random fragments + gaps
                            let flicker = glitch_state < 40;
                            if flicker {
                                let idx = (slow + i * 19 + line_idx * 7) % fragment_chars.len();
                                let ch = fragment_chars[idx];
                                let brightness = 0.4 + (glitch_state as f32 / 100.0) * 0.4;
                                Span::styled(
                                    ch.to_string(),
                                    Style::default()
                                        .fg(theme::primary_with_intensity(brightness as f64)),
                                )
                            } else if glitch_state < 70 {
                                Span::styled(" ", Style::default())
                            } else {
                                // Occasional glimpse of original char, dim
                                Span::styled(
                                    c.to_string(),
                                    Style::default().fg(theme::primary_with_intensity(0.5)),
                                )
                            }
                        }
                    } else if cycle >= lock_frame {
                        // Character is locked in - show final form with shimmer
                        let settle_time = cycle - lock_frame;
                        let shimmer = if settle_time < 10 {
                            // Brief bright flash when locking in
                            1.2 - (settle_time as f32 * 0.02)
                        } else {
                            (slow as f32 * 0.15 + i as f32 * 0.5).sin() * 0.15 + 0.85
                        };

                        let color = theme::primary_with_intensity((shimmer as f64).min(1.17));
                        Span::styled(c.to_string(), Style::default().fg(color))
                    } else if cycle >= 5 {
                        // Chaos phase - show random fragments that shift around
                        let chaos_seed = (slow + i * 13 + line_idx * 23) % fragment_chars.len();
                        let fragment = fragment_chars[chaos_seed];

                        // Fragments get brighter as we approach lock-in
                        let proximity = (cycle as f32 / lock_frame as f32).min(1.0);
                        let brightness = 0.3 + proximity * 0.5;

                        // Occasionally flicker to the correct character
                        let flicker = (slow + i * 7).is_multiple_of(12) && proximity > 0.7;
                        let display_char = if flicker { c } else { fragment };

                        let color = theme::primary_with_intensity(brightness as f64);
                        Span::styled(display_char.to_string(), Style::default().fg(color))
                    } else {
                        // Initial blank phase
                        Span::styled(" ", Style::default())
                    }
                })
                .collect()
        };

        let logo1_spans = build_logo_line(logo_lines[0], 0);
        let logo2_spans = build_logo_line(logo_lines[1], 1);
        let logo3_spans = build_logo_line(logo_lines[2], 2);

        // Claude Code style star field with spinner characters
        let star_chars = ['·', '✢', '✳', '✶', '✻', '✽'];
        let make_starfield = |offset: usize| -> Vec<Span> {
            (0..32)
                .map(|i| {
                    let seed = (i * 13 + offset) % 97;
                    let twinkle = (slow + seed) % 48;
                    let (ch, color) = if seed.is_multiple_of(8) {
                        let char_idx = (twinkle / 8) % star_chars.len();
                        match twinkle {
                            0..=7 => (star_chars[char_idx], primary),
                            8..=23 => (star_chars[(char_idx + 1) % star_chars.len()], warm),
                            24..=35 => ('·', dim),
                            _ => (' ', faint),
                        }
                    } else {
                        (' ', faint)
                    };
                    Span::styled(ch.to_string(), Style::default().fg(color))
                })
                .collect()
        };

        // Title with Claude Code style icon and gentle wave
        let icon_frames = ['✻', '✶', '✳', '✢'];
        let icon_idx = (slow / 6) % icon_frames.len();
        let icon = icon_frames[icon_idx];

        let title = "C C S I G H T";
        let mut title_spans: Vec<Span> = vec![Span::styled(
            format!("{icon} "),
            Style::default().fg(primary).add_modifier(Modifier::BOLD),
        )];
        title_spans.extend(title.chars().enumerate().map(|(i, c)| {
            let wave = ((slow as f32 * 0.1 + i as f32 * 0.3).sin() * 15.0 + 15.0) as u8;
            let color = Color::Rgb(150 + wave, 110 + wave / 2, 90 + wave / 3); // lint-ok: raw-rgb — splash wave
            Span::styled(
                c.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )
        }));

        // Claude Code style spinner
        let spinner_frames = ['·', '✢', '✳', '✶', '✻', '✽'];
        let spinner_idx = (slow / 4) % spinner_frames.len();

        // Animated spinner with trail effect
        let bar: Vec<Span> = (0..6)
            .map(|i| {
                let frame_idx = (spinner_idx + 6 - i) % spinner_frames.len();
                let intensity = 1.0 - (i as f32 * 0.15);
                let color = theme::primary_with_intensity(intensity as f64);
                Span::styled(
                    format!(" {} ", spinner_frames[frame_idx]),
                    Style::default().fg(color),
                )
            })
            .collect();

        // Status message - creative rotating messages
        let messages = [
            "Deliberating",
            "Reticulating",
            "Vibing",
            "Mulling",
            "Puzzling",
            "Wibbling",
            "Elucidating",
            "Sussing",
            "Concocting",
            "Envisioning",
            "Actualizing",
            "Processing",
            "Channelling",
            "Wrangling",
            "Stewing",
            "Smooshing",
            "Moseying",
            "Germinating",
            "Brewing",
            "Schlepping",
            "Shimmying",
            "Effecting",
        ];
        let msg_idx = (slow / 25) % messages.len();
        let msg = messages[msg_idx];

        // Gentle cursor blink
        let cursor = if (slow / 8).is_multiple_of(2) {
            "▎"
        } else {
            " "
        };

        // Decorative line
        let deco_line: String = (0..28)
            .map(|i| {
                let pos = (slow + i * 2) % 56;
                if pos == i || pos == 56 - i {
                    '─'
                } else {
                    ' '
                }
            })
            .collect();

        // No leading-space prefixes: `Paragraph::centered()` aligns each line
        // to the same horizontal centre. Mixing prefix widths (4 / 6 / 8)
        // shifted the visible content of each line by different amounts, so
        // logo / title / decoration / bar / message ended up on slightly
        // different vertical lines.
        let loading_text = vec![
            Line::from(""),
            Line::from(make_starfield(0)),
            Line::from(""),
            Line::from(""),
            Line::from(logo1_spans),
            Line::from(logo2_spans),
            Line::from(logo3_spans),
            Line::from(""),
            Line::from(title_spans),
            Line::from(""),
            Line::from(Span::styled(deco_line, Style::default().fg(faint))),
            Line::from(""),
            Line::from(bar),
            Line::from(""),
            Line::from(vec![
                Span::styled(format!("{msg}..."), Style::default().fg(dim)),
                Span::styled(cursor, Style::default().fg(primary)),
            ]),
            Line::from({
                let (done, total) = crate::aggregator::load_progress();
                progress_spans(done, total)
            }),
            Line::from(make_starfield(17)),
            Line::from(""),
            Line::from(Span::styled("press q to quit", Style::default().fg(dim))),
        ];
        let loading = Paragraph::new(loading_text)
            .block(Block::default().borders(Borders::NONE))
            .centered();
        let content_height = 18;
        let split_result = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(content_height),
            Constraint::Min(0),
        ])
        .flex(ratatui::layout::Flex::Center)
        .split(chunks[3]);
        let centered_area = split_result.get(1).copied().unwrap_or(chunks[3]);
        frame.render_widget(loading, centered_area);
    } else if let Some(err) = state.error.clone() {
        draw_tabs(frame, chunks[2], state);
        let error = Paragraph::new(format!("Error: {err}"))
            .style(Style::default().fg(theme::ERROR))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme::BORDER))
                    .title(Span::styled(" Error ", Style::default().fg(theme::ERROR))),
            );
        frame.render_widget(error, chunks[3]);
    } else {
        if !state.show_conversation {
            draw_tabs(frame, chunks[2], state);
        }
        match state.tab {
            Tab::Dashboard => dashboard::draw_dashboard(frame, chunks[3], state),
            Tab::Daily => {
                if !state.show_conversation {
                    draw_daily(frame, chunks[3], state);
                }
            }
            Tab::Insights => insights::draw_insights(frame, chunks[3], state),
            Tab::Live => {
                if !state.show_conversation {
                    draw_live(frame, chunks[3], state);
                }
            }
        }
    }

    if state.show_detail() && !state.show_conversation {
        draw_detail_popup(frame, area, state);
    }

    if state.show_dashboard_detail() {
        dashboard::draw_dashboard_detail_popup(frame, area, state);
    }

    if state.show_insights_detail() {
        insights::draw_insights_detail_popup(frame, area, state);
    }

    if state.show_conversation {
        // The tab bar is not redrawn in conv view, so any tab-bar triggers
        // captured the last time draw_tabs ran are stale. They sit on the
        // exact row range that the conv view overwrites, and would otherwise
        // match clicks on widgets placed there (e.g. the per-pane [i] button).
        state.layout.filter_popup_area_trigger = None;
        state.layout.project_popup_area_trigger = None;
        state.layout.pin_view_trigger = None;
        state.layout.help_trigger = None;

        let conv_layout = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
        let conv_area = conv_layout[0];
        let help_area = conv_layout[1];

        if !state.panes.is_empty() {
            let pane_count = state.panes.len();
            let min_pane_width = crate::MIN_PANE_WIDTH;

            // Dynamic session list width based on available space
            let session_list_width = if conv_area.width >= 100 {
                36
            } else if conv_area.width >= 80 {
                30
            } else if conv_area.width >= 68 {
                26
            } else {
                0
            };

            let show_session_list = !state.session_list_hidden && session_list_width > 0;

            let available_for_panes = if show_session_list {
                conv_area.width.saturating_sub(session_list_width)
            } else {
                conv_area.width
            };

            let pane_width = available_for_panes / pane_count as u16;
            let layout_too_narrow = pane_width < min_pane_width && pane_count > 1;

            let mut constraints = Vec::new();
            if show_session_list {
                constraints.push(Constraint::Length(session_list_width));
            }
            let pane_percentage = 100 / pane_count as u16;
            for _ in 0..pane_count {
                constraints.push(Constraint::Percentage(pane_percentage));
            }
            let layout = Layout::horizontal(constraints).split(conv_area);

            let layout_offset = if show_session_list { 1 } else { 0 };

            if show_session_list {
                draw_split_session_list(frame, layout[0], state, state.active_pane_index.is_none());
            } else {
                state.layout.session_list_area = None;
            }

            // Store pane areas for click detection
            state.layout.pane_areas.clear();
            for i in 0..pane_count {
                state.layout.pane_areas.push(layout[i + layout_offset]);
            }

            let sessions: &[SessionInfo] = state
                .daily_groups
                .get(state.selected_day)
                .map_or(&[], |g| g.sessions.as_slice());
            let selecting = state.selecting;
            let active_pane_index = state.active_pane_index;
            for i in 0..state.panes.len() {
                let is_active = active_pane_index == Some(i);
                let pane_area = layout[i + layout_offset];
                let ca = draw_conversation_pane(
                    frame,
                    pane_area,
                    &mut state.panes[i],
                    is_active,
                    &state.toast_time,
                    layout_too_narrow,
                    selecting,
                    sessions,
                    &state.original_daily_groups,
                    &state.pins,
                    &state.project_labels,
                    &state.session_titles,
                );
                if is_active {
                    state.layout.conversation_content_area = ca;
                }
            }
        } else {
            state.layout.pane_areas.clear();
        }

        // Footer adapts to the focused pane's mode: in compact mode Enter expands
        // an accordion row and `c` switches to full; in full mode Enter is a no-op
        // (dropped) and `c` switches back to compact; with no pane focused (list
        // view) Enter opens the selection and `c` has nothing to toggle (dropped).
        let active_compact = state
            .active_pane_index
            .and_then(|i| state.panes.get(i))
            .map(|p| p.compact);
        let mut pairs: Vec<(&str, &str)> = vec![("Esc", "back"), ("↑↓", "msg")];
        match active_compact {
            Some(true) => pairs.extend([("Enter", "expand"), ("c", "full")]),
            Some(false) => pairs.push(("c", "compact")),
            None => pairs.push(("Enter", "open")),
        }
        pairs.extend([("/", "search"), ("i", "info"), ("s", "summary")]);
        // `y` copies the focused message; with no pane focused (list view) it's
        // a no-op, so omit it there rather than advertise a dead key.
        if active_compact.is_some() {
            pairs.push(("y", "copy"));
            // Only reachable with a pane focused, and Esc no longer removes
            // panes, so this is the sole hint for dropping one.
            pairs.push(("X", "close-pane"));
        }
        // Day stepping only moves the Day list; Live / Pinned lists ignore it.
        if state.conv_list_mode == crate::ConvListMode::Day {
            pairs.push(("H/L", "day"));
        }
        // Drop least-essential hints (rightmost first) until the bar fits, so a
        // narrow terminal trims gracefully instead of raw-cutting mid-hint. The
        // first two (Esc/back, ↑↓/msg) are always kept; `?` shows the full list.
        while pairs.len() > 2 && help_bar(&pairs).width() > help_area.width as usize {
            pairs.pop();
        }
        let help_line = Paragraph::new(tab_help_bar(state, &pairs));
        frame.render_widget(help_line, help_area);

        if state.show_detail() {
            let fp = state
                .active_pane_index
                .and_then(|i| state.panes.get(i))
                .and_then(|p| p.file_path.clone())
                .or_else(|| crate::get_conv_session_file(state, state.selected_session));
            let session = fp.and_then(|fp| {
                state
                    .original_daily_groups
                    .iter()
                    .find_map(|g| g.sessions.iter().find(|s| s.file_path == fp))
            });
            if let Some(session) = session {
                let pinned = state.pins.is_pinned(&session.file_path);
                let cumulative =
                    compute_session_cumulative(&session.file_path, &state.original_daily_groups);
                let pa = draw_session_detail(
                    frame,
                    area,
                    session,
                    " Space: pin  y: copy resume  s: summary  t: title  C: pane  ↑↓: scroll  i/Esc: close ",
                    pinned,
                    state.session_detail_scroll,
                    &state.project_labels,
                    &state.session_titles,
                    &cumulative,
                    crate::aggregator::day_subagent_spend(session, &state.original_daily_groups),
                    None,
                    state.session_detail_recent.as_deref(),
                    state.resume_dir(&session.file_path).as_deref(),
                );
                state.layout.active_popup_area = Some(pa);
            }
        }
    }

    if state.show_filter_popup() {
        draw_filter_popup(frame, area, state);
    }

    if state.show_title_edit() {
        draw_title_edit_popup(frame, area, state);
    }

    if state.show_project_popup() {
        draw_project_popup(frame, area, state);
    }

    if state.show_summary() {
        draw_summary(frame, chunks[3], state);
    }

    if state.show_help() {
        draw_help_popup(frame, area, state);
    }

    if state.show_project_detail() {
        draw_project_detail_popup(frame, area, state);
    }

    if state.search_mode {
        draw_search_popup(frame, area, state);
    }

    if let Some((sc, sr, ec, er)) = state.text_selection {
        let (start_col, start_row, end_col, end_row) = if (sr, sc) <= (er, ec) {
            (sc, sr, ec, er)
        } else {
            (ec, er, sc, sr)
        };

        let clamp_area = if state.show_conversation {
            state.layout.conversation_content_area.filter(|ca| {
                start_row >= ca.y
                    && start_row < ca.y + ca.height
                    && start_col >= ca.x
                    && start_col < ca.x + ca.width
            })
        } else {
            None
        };

        let buf = frame.buffer_mut();
        let buf_area = buf.area;
        for row in start_row..=end_row {
            if row < buf_area.y || row >= buf_area.y + buf_area.height {
                continue;
            }
            if let Some(ca) = clamp_area
                && (row < ca.y || row >= ca.y + ca.height)
            {
                continue;
            }
            let col_start = if row == start_row {
                start_col
            } else {
                clamp_area.map_or(buf_area.x, |ca| ca.x)
            };
            let col_end = if row == end_row {
                end_col
            } else {
                clamp_area.map_or(buf_area.x + buf_area.width - 1, |ca| ca.x + ca.width - 1)
            };
            for col in col_start..=col_end.min(buf_area.x + buf_area.width - 1) {
                let cell = &mut buf[(col, row)];
                cell.set_bg(theme::SELECTION);
            }
        }
    }

    // Global toast — draws over whatever tab / popup is active so messages
    // like "Press q again to quit", "Pin saved", clipboard errors, snapshot
    // failures, etc. are visible regardless of which view the user is on.
    // Scoping it to a single tab would hide most warnings from users who
    // stay in the other views.
    if let Some(ref msg) = state.toast_message {
        // `+ 2` is the one space padded on each side below. A wider rect paints
        // the badge background over whatever is beside it — usually a panel
        // border, which then reads as part of the message.
        let toast_width = unicode_width::UnicodeWidthStr::width(msg.as_str()) as u16 + 2;
        let toast_width = toast_width.min(area.width);
        let toast_x = area.x + area.width.saturating_sub(toast_width).saturating_sub(2);
        // Render one row above the very bottom (which holds the footer
        // hints). y=0 area = first row (header), so toast_y = area.height-2.
        let toast_y = area.y + area.height.saturating_sub(2);
        let toast_area = Rect {
            x: toast_x,
            y: toast_y,
            width: toast_width,
            height: 1,
        };
        let toast = Paragraph::new(format!(" {msg} "))
            .style(Style::default().fg(theme::TEXT_DARK).bg(theme::SUCCESS));
        frame.render_widget(toast, toast_area);
    }
}

fn draw_header(frame: &mut Frame, area: Rect, state: &AppState) {
    let soft = theme::WARM;
    let dim = theme::LABEL;

    let mut spans = vec![
        Span::styled("  ◈  ", Style::default().fg(theme::PRIMARY)),
        Span::styled(
            "C C S I G H T",
            Style::default().fg(soft).add_modifier(Modifier::BOLD),
        ),
    ];

    // Suppress during the initial load — `0 sessions` next to the splash
    // reads as "no data / load failed". Once loaded, always render it
    // (including 0): a real `0` means the active filter matched nothing,
    // which users mistook for a load failure when the span was hidden. The
    // cache / index spans below stay visible while loading.
    if !state.loading {
        let session_count = crate::aggregator::distinct_user_session_count(&state.daily_groups);
        spans.push(Span::styled(
            format!("  ·  {session_count} sessions"),
            Style::default().fg(dim),
        ));
    }

    if let Some(ref cache) = state.cache_stats
        && cache.cached_files > 0
    {
        // `files`, not `sessions`: one JSONL can span multiple days,
        // so the file count is smaller than the session-day count.
        spans.push(Span::styled(
            format!(
                "  ·  cache {}/{} files",
                cache.cached_files,
                cache.cached_files + cache.parsed_files
            ),
            Style::default().fg(theme::META),
        ));
    }

    if state.index_build_task.is_some() {
        spans.push(Span::styled(
            "  ·  building search index...",
            Style::default().fg(theme::PLACEHOLDER),
        ));
    }

    // Sits beside the cache count because both answer "is what I'm looking at
    // complete". A truncated parse drops the newest entries, so the totals and
    // the last-activity times on screen are understated.
    let truncated = crate::parser::truncated_file_count();
    if truncated > 0 {
        spans.push(Span::styled(
            format!(
                "  ·  ⚠ {truncated} file{} truncated (entry cap)",
                if truncated == 1 { "" } else { "s" }
            ),
            Style::default().fg(theme::WARNING),
        ));
    }

    let title = Paragraph::new(Line::from(spans));
    frame.render_widget(title, area);
}

fn draw_retention_warning(frame: &mut Frame, area: Rect, state: &AppState) {
    if let Some(ref warning) = state.retention_warning {
        let line1 = if warning.is_default {
            "⚠ Log retention period is not set (default: 30 days). Setting a longer period is recommended for ccsight.".to_string()
        } else {
            format!(
                "⚠ Log retention period is set to {} days. A longer period is recommended for ccsight.",
                warning.days
            )
        };
        let line2 = if warning.is_default {
            "  → Add { \"cleanupPeriodDays\": 36500 } to ~/.claude/settings.json"
        } else {
            "  → Increase cleanupPeriodDays in ~/.claude/settings.json (e.g., 36500)"
        };

        let content = vec![
            Line::from(Span::styled(line1, Style::default().fg(theme::WARNING))),
            Line::from(Span::styled(line2, Style::default().fg(theme::LABEL))),
            Line::from(vec![
                Span::styled("  Docs: ", Style::default().fg(theme::LABEL)),
                Span::styled(
                    "https://code.claude.com/docs/en/settings",
                    Style::default().fg(theme::LINK),
                ),
                Span::styled("  |  x to dismiss", Style::default().fg(theme::LABEL)),
            ]),
        ];

        let banner = Paragraph::new(content).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(theme::SEPARATOR)),
        );
        frame.render_widget(banner, area);
    }
}

fn draw_tabs(frame: &mut Frame, area: Rect, state: &mut AppState) {
    let dim = theme::LABEL;

    // Tab label count = latest day in the current view (= today, or the
    // latest day inside an active date filter). Counted the way the Daily
    // tab's own `Sessions (n/N)` header counts, so the two agree on the same
    // glass; a subagent-inclusive badge read as a different day's figure.
    let latest_group = state.daily_groups.iter().max_by_key(|g| g.date);
    let today_sessions = latest_group.map_or(0, |g| g.user_sessions().count());
    let today_tokens: u64 = latest_group.map_or(0, |g| {
        g.sessions
            .iter()
            .flat_map(|s| s.day_tokens_by_model.values())
            .map(super::aggregator::stats::TokenStats::work_tokens)
            .sum()
    });

    // No count until the first live poll lands — a hardcoded "(0)" that
    // flips to the real number a moment later reads as a glitch.
    let live_label = if state.live_last_update.is_some() {
        format!("Live ({})", state.live_active.len())
    } else {
        "Live".to_string()
    };
    // Order: Dashboard (landing / overview) → Live (current activity) →
    // Daily (today drill-down) → Insights (deep analysis).
    let tabs_data = [
        (Tab::Dashboard, "1", "Dashboard".to_string()),
        (Tab::Live, "2", live_label),
        (Tab::Daily, "3", format!("Daily ({today_sessions})")),
        (
            Tab::Insights,
            "4",
            format!("Insights ({})", crate::format_number(today_tokens)),
        ),
    ];

    // Clear and rebuild tab areas
    state.layout.tab_areas.clear();
    let mut current_x = area.x + 1; // Start after initial space

    let mut all_spans = vec![Span::styled(" ", Style::default())];
    for (i, (tab, key, label)) in tabs_data.iter().enumerate() {
        let is_selected = state.tab == *tab;
        let tab_width = if is_selected {
            unicode_width::UnicodeWidthStr::width(label.as_str()) + 2 // " label "
        } else {
            key.len() + 1 + unicode_width::UnicodeWidthStr::width(label.as_str()) + 1 // "N:label "
        };

        // Store clickable area for this tab
        state
            .layout
            .tab_areas
            .push((*tab, Rect::new(current_x, area.y, tab_width as u16, 1)));

        if is_selected {
            all_spans.push(Span::styled(
                format!(" {label} "),
                Style::default()
                    .fg(theme::TEXT_DARK)
                    .bg(theme::PRIMARY)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            all_spans.push(Span::styled(
                format!("{key}:"),
                Style::default().fg(theme::FAINT),
            ));
            all_spans.push(Span::styled(format!("{label} "), Style::default().fg(dim)));
        }

        current_x += tab_width as u16;

        // Inter-tab gap: 2 spaces after every tab except the last so all
        // adjacent tabs share the same rhythm.
        if i + 1 < tabs_data.len() {
            all_spans.push(Span::styled("  ", Style::default()));
            current_x += 2;
        }
    }

    // The chip keeps the `f:` prefix while a filter is on: it replaces the
    // `f:Filter` hint in the same slot, and losing the key exactly when the
    // user wants to change or clear the filter is the worst moment for it.
    let filter_label = if state.period_filter != crate::PeriodFilter::All {
        let range = state.period_filter.date_range_label();
        if range.is_empty() {
            format!(" f:{} ", state.period_filter.label())
        } else {
            format!(" f:{} {} ", state.period_filter.label(), range)
        }
    } else {
        " f:Filter ".to_string()
    };
    let filter_width = unicode_width::UnicodeWidthStr::width(filter_label.as_str()) as u16;

    let project_label = if let Some(ref project) = state.project_filter {
        let short = state.project_label(project);
        format!(" {short} ")
    } else {
        " p:Project ".to_string()
    };
    let project_width = unicode_width::UnicodeWidthStr::width(project_label.as_str()) as u16;

    let pin_count = state.pins.entries().len();
    let pin_label = if pin_count > 0 {
        format!(" *{pin_count} ")
    } else {
        String::new()
    };
    let pin_width = unicode_width::UnicodeWidthStr::width(pin_label.as_str()) as u16;
    let help_label = " ? ";
    let help_width = 3u16;

    let buttons_width = filter_width + project_width + pin_width + help_width + 1;
    if area.width > buttons_width + current_x - area.x {
        let right_x = area.x + area.width - buttons_width;
        let gap = (right_x - current_x) as usize;

        let filter_area = Rect::new(right_x, area.y, filter_width, 1);
        state.layout.filter_popup_area_trigger = Some(filter_area);
        let filter_style = if state.period_filter != crate::PeriodFilter::All {
            Style::default()
                .fg(theme::TEXT_DARK)
                .bg(theme::PRIMARY)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::LABEL)
        };
        all_spans.push(Span::raw(" ".repeat(gap)));
        all_spans.push(Span::styled(filter_label, filter_style));

        let project_area = Rect::new(right_x + filter_width, area.y, project_width, 1);
        state.layout.project_popup_area_trigger = Some(project_area);
        let project_style = if state.project_filter.is_some() {
            Style::default()
                .fg(theme::TEXT_DARK)
                .bg(theme::PRIMARY)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::LABEL)
        };
        all_spans.push(Span::styled(project_label, project_style));

        if pin_count > 0 {
            let pin_area = Rect::new(right_x + filter_width + project_width, area.y, pin_width, 1);
            state.layout.pin_view_trigger = Some(pin_area);
            all_spans.push(Span::styled(pin_label, Style::default().fg(theme::WARNING)));
        } else {
            state.layout.pin_view_trigger = None;
        }

        let help_x = right_x + filter_width + project_width + pin_width;
        let help_area = Rect::new(help_x, area.y, help_width, 1);
        state.layout.help_trigger = Some(help_area);
        all_spans.push(Span::styled(help_label, Style::default().fg(theme::LABEL)));
    } else {
        state.layout.filter_popup_area_trigger = None;
        state.layout.project_popup_area_trigger = None;
        state.layout.pin_view_trigger = None;
        state.layout.help_trigger = None;
    }

    let tab_line = Paragraph::new(Line::from(all_spans));
    frame.render_widget(tab_line, area);
}

/// Render the Live tab — active sessions on top, recently-paused below.
/// `live_view_snapshot_offset > 0` delegates to `draw_live_past` (single
/// "Alive at <date>" list from frozen snapshot).
fn draw_live(frame: &mut Frame, area: Rect, state: &mut AppState) {
    use chrono::Utc;

    if state.live_view_snapshot_offset > 0 {
        draw_live_past(frame, area, state);
        return;
    }

    let chunks = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Min(0),
        ratatui::layout::Constraint::Length(1),
    ])
    .split(area);

    let active_visible: Vec<&crate::infrastructure::live_sessions::LiveSession> =
        state.live_active.iter().collect();
    let paused_visible: Vec<&crate::infrastructure::live_sessions::LiveSession> =
        state.live_paused.iter().collect();
    let active_count = active_visible.len();
    let paused_count = paused_visible.len();

    let now = Utc::now();
    // Assemble the per-row map from the memoized cumulative cache (rebuilt only
    // on data load) — no per-frame re-fold / pricing lookups.
    let meta_map = crate::aggregator::meta_by_path(&state.original_daily_groups);
    // Summary on line 2 sits behind a 5-col indent (rank + marker) plus a
    // small right margin for border legibility.
    let inner_width = chunks[0].width.saturating_sub(2) as usize;
    let max_summary_chars = inner_width.saturating_sub(7).max(20);
    let row_height = 3usize;

    // Active now and Recently paused render as two separate framed panels.
    // Both bodies share the `line 0 = info header, rows at 1 + r*3` shape so
    // the scroll and mouse hit-test math is identical. `live_selected` flows
    // continuously across the boundary: active panel while `< active_count`,
    // paused panel otherwise — j/k crosses frames without a jump.

    // ---- Active panel body ----
    // Status breakdown mirrors `render_live_row` glyph precedence
    // (busy → today → older) so the header counts and row glyphs agree.
    let mut busy_n = 0usize;
    let mut today_n = 0usize;
    let mut older_n = 0usize;
    for s in &active_visible {
        if s.status.as_deref() == Some("busy") {
            busy_n += 1;
        } else {
            let last = live_session_last_activity(state, s, now);
            if crate::infrastructure::live_sessions::is_today(last, now) {
                today_n += 1;
            } else {
                older_n += 1;
            }
        }
    }
    let mut active_lines: Vec<Line> = Vec::new();
    if active_count == 0 {
        let placeholder = if state.live_sessions_task.is_some() {
            "  Loading…".to_string()
        } else {
            "  No active sessions".to_string()
        };
        active_lines.push(Line::from(Span::styled(
            placeholder,
            Style::default().fg(theme::PLACEHOLDER),
        )));
    } else {
        active_lines.push(Line::from(Span::styled(
            format!("  🟢 {busy_n} busy · ◉ {today_n} today · ○ {older_n} older"),
            Style::default().fg(theme::META),
        )));
        for (display_rank, s) in active_visible.iter().enumerate() {
            let selected = display_rank == state.live_selected;
            active_lines.extend(render_live_row(
                s,
                now,
                state,
                true,
                selected,
                max_summary_chars,
                inner_width,
                display_rank,
                &meta_map,
                None,
            ));
        }
    }

    // ---- Paused panel body ----
    // `⟳` count comes from snapshot matches (sessions alive in a past
    // snapshot but not alive now). Split yesterday vs older so the user can
    // tell at a glance whether the pause is a fresh continuation point or
    // older context.
    let today = now.with_timezone(&chrono::Local).date_naive();
    let yesterday = today - chrono::Duration::days(1);
    let mut restorable_yesterday = 0usize;
    let mut restorable_older = 0usize;
    for s in &paused_visible {
        if !s.was_recently_live {
            continue;
        }
        let last = s.jsonl_mtime.or(s.updated_at);
        let bucket_date = last.map(|t| t.with_timezone(&chrono::Local).date_naive());
        match bucket_date {
            Some(d) if d >= yesterday => restorable_yesterday += 1,
            _ => restorable_older += 1,
        }
    }
    let restorable_count = restorable_yesterday + restorable_older;
    let mut paused_lines: Vec<Line> = Vec::new();
    if paused_count == 0 {
        paused_lines.push(Line::from(Span::styled(
            "  No paused sessions",
            Style::default().fg(theme::PLACEHOLDER),
        )));
    } else {
        let mut info_spans = Vec::new();
        if restorable_count > 0 {
            info_spans.push(Span::styled(
                format!(
                    "  ⟳ {restorable_count} restorable (yesterday:{restorable_yesterday} \
                     older:{restorable_older})  "
                ),
                Style::default().fg(theme::ACCENT),
            ));
        }
        info_spans.push(Span::styled(
            "last 24h + prior-run snapshot",
            Style::default().fg(theme::LABEL),
        ));
        paused_lines.push(Line::from(info_spans));
        for (display_rank, s) in paused_visible.iter().enumerate() {
            let selected = active_count + display_rank == state.live_selected;
            paused_lines.extend(render_live_row(
                s,
                now,
                state,
                false,
                selected,
                max_summary_chars,
                inner_width,
                display_rank,
                &meta_map,
                None,
            ));
        }
    }

    // ---- Split the body into two stacked frames (pane-mode aware) ----
    // Full-screen modes give one list the whole body; the other frame gets a
    // zero-height rect (rendered as a no-op, layout area cleared below). Split:
    // active is content-sized capped at 3/5 so paused stays on screen; with no
    // paused it shrinks to its placeholder and active claims the rest.
    let full = chunks[0];
    let hidden = ratatui::layout::Rect {
        x: full.x,
        y: full.y,
        width: full.width,
        height: 0,
    };
    let (active_area, paused_area) = match state.live_pane_mode {
        LivePaneMode::ActiveOnly => (full, hidden),
        LivePaneMode::PausedOnly => (hidden, full),
        LivePaneMode::Split => {
            let frames = if paused_count == 0 {
                let paused_h = (paused_lines.len() as u16 + 2).min(full.height);
                ratatui::layout::Layout::vertical([
                    ratatui::layout::Constraint::Min(0),
                    ratatui::layout::Constraint::Length(paused_h),
                ])
                .split(full)
            } else {
                let active_needed = (active_lines.len() + 2).max(3) as u16;
                let cap = ((full.height as usize * 3 / 5).max(4) as u16).max(3);
                let active_h = active_needed.min(cap).min(full.height);
                ratatui::layout::Layout::vertical([
                    ratatui::layout::Constraint::Length(active_h),
                    ratatui::layout::Constraint::Min(0),
                ])
                .split(full)
            };
            (frames[0], frames[1])
        }
    };

    // `inner_h` is the true viewport — used for the scroll clamp + scrollbar
    // so the last row stays reachable. `snap_h` floors it to one full row for
    // the cursor-follow comparison only, which would otherwise misbehave on a
    // frame too short to hold a 3-line row. Flooring the clamp to `inner_h`
    // instead would understate max-scroll and strand the bottom row.
    let active_inner_h = (active_area.height as usize).saturating_sub(2);
    let active_view_h = active_inner_h.max(1);
    let active_snap_h = active_inner_h.max(row_height);

    // ---- Active frame scroll (cursor-following only while selection here) ----
    if state.live_selected < active_count {
        let first = 1 + state.live_selected * row_height;
        let last = first + row_height - 1;
        let scroll = state.live_scroll;
        state.live_scroll = if state.live_selected == 0 {
            0
        } else if first < scroll {
            first
        } else if last >= scroll + active_snap_h {
            last + 1 - active_snap_h
        } else {
            scroll
        };
    }
    let active_total = active_lines.len();
    state.live_scroll = state
        .live_scroll
        .min(active_total.saturating_sub(active_view_h));

    // ---- Paused frame scroll ----
    let paused_inner_h = (paused_area.height as usize).saturating_sub(2);
    let paused_view_h = paused_inner_h.max(1);
    let paused_snap_h = paused_inner_h.max(row_height);
    if state.live_selected >= active_count {
        let pidx = state.live_selected - active_count;
        let first = 1 + pidx * row_height;
        let last = first + row_height - 1;
        let scroll = state.live_paused_scroll;
        state.live_paused_scroll = if pidx == 0 {
            0
        } else if first < scroll {
            first
        } else if last >= scroll + paused_snap_h {
            last + 1 - paused_snap_h
        } else {
            scroll
        };
    }
    let paused_total = paused_lines.len();
    state.live_paused_scroll = state
        .live_paused_scroll
        .min(paused_total.saturating_sub(paused_view_h));

    // ---- Render active frame ----
    // Advertise `←/→` time-travel in the active title so it's discoverable
    // without reading the footer. Shown only when a snapshot exists; the
    // flag is refreshed per-poll (not re-scanned each render frame).
    let has_history = state.live_has_snapshot_history;
    let active_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER));
    let active_block = if has_history {
        active_block.title(Line::from(vec![
            Span::styled(
                format!(" Active now ({active_count}) "),
                Style::default().fg(theme::PRIMARY),
            ),
            Span::styled(" ← earlier snapshots ", Style::default().fg(theme::LABEL)),
        ]))
    } else {
        active_block.title(Span::styled(
            format!(" Active now ({active_count}) "),
            Style::default().fg(theme::PRIMARY),
        ))
    };
    frame.render_widget(
        ratatui::widgets::Paragraph::new(active_lines)
            .scroll((state.live_scroll as u16, 0))
            .block(active_block),
        active_area,
    );
    draw_scrollbar(
        frame,
        active_area,
        state.live_scroll,
        active_total,
        active_view_h,
    );

    // ---- Render paused frame ----
    let paused_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .title(Span::styled(
            format!(" Recently paused ({paused_count}) "),
            Style::default().fg(theme::PRIMARY),
        ));
    frame.render_widget(
        ratatui::widgets::Paragraph::new(paused_lines)
            .scroll((state.live_paused_scroll as u16, 0))
            .block(paused_block),
        paused_area,
    );
    draw_scrollbar(
        frame,
        paused_area,
        state.live_paused_scroll,
        paused_total,
        paused_view_h,
    );

    // Stash each frame's inner rect + scroll so mouse clicks resolve to a
    // session index. Active = rows 0..active_count; paused = global index
    // active_count + row.
    let active_inner = ratatui::layout::Rect {
        x: active_area.x + 1,
        y: active_area.y + 1,
        width: active_area.width.saturating_sub(2),
        height: active_area.height.saturating_sub(2),
    };
    let paused_inner = ratatui::layout::Rect {
        x: paused_area.x + 1,
        y: paused_area.y + 1,
        width: paused_area.width.saturating_sub(2),
        height: paused_area.height.saturating_sub(2),
    };
    // Only the visible pane(s) accept mouse hits — a hidden (zero-height) frame
    // must not resolve clicks to a row it isn't showing.
    state.layout.live_list_area =
        (active_area.height > 0).then_some((active_inner, state.live_scroll, active_count));
    state.layout.live_paused_list_area =
        (paused_area.height > 0).then_some((paused_inner, state.live_paused_scroll));

    // Vocab matches Daily's session-list footer (:session for ↑↓, :view for
    // Enter, :info for i) so the same keys read the same way across tabs.
    let help_line = tab_help_bar(
        state,
        &[
            ("?", "help"),
            ("q", "quit"),
            ("↑↓", "session"),
            ("i", "info"),
            ("Enter", "view"),
            ("Space", "pin"),
            ("y", "copy-resume"),
            ("t", "title"),
            ("←→", "snapshot"),
            ("v", "layout"),
            ("/", "search"),
            ("m", "pins"),
        ],
    );
    frame.render_widget(ratatui::widgets::Paragraph::new(help_line), chunks[1]);
}

/// Past view: a single section listing the frozen alive set from one
/// snapshot file under `~/.ccsight/live_snapshots/`. Rendering convention
/// mirrors `draw_live` (3-line rows, same column layout) so the user feels
/// the same screen, just time-shifted. No Recently paused section — a
/// snapshot only captures "alive at that poll", not the paused tail.
fn draw_live_past(frame: &mut Frame, area: Rect, state: &mut AppState) {
    use chrono::Utc;

    let chunks = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Min(0),
        ratatui::layout::Constraint::Length(1),
    ])
    .split(area);

    let inner_width = chunks[0].width.saturating_sub(2) as usize;
    let max_summary_chars = inner_width.saturating_sub(7).max(20);
    let now = Utc::now();
    let offset = state.live_view_snapshot_offset;
    let total = state.live_past_snapshot_total.max(offset);
    let meta_map = crate::aggregator::meta_by_path(&state.original_daily_groups);
    let past_visible: Vec<&crate::infrastructure::live_sessions::LiveSession> =
        state.live_past_sessions.iter().collect();
    // Diff vs the current alive set (live_active is preserved across time-travel)
    // so each frozen row shows whether it's still live now and the header
    // summarizes the delta. Matched by session_id.
    let now_active: std::collections::HashSet<&str> = state
        .live_active
        .iter()
        .map(|s| s.session_id.as_str())
        .collect();
    let past_ids: std::collections::HashSet<&str> =
        past_visible.iter().map(|s| s.session_id.as_str()).collect();
    let still_live_n = past_visible
        .iter()
        .filter(|s| now_active.contains(s.session_id.as_str()))
        .count();
    let ended_n = past_visible.len() - still_live_n;
    let new_since_n = now_active
        .iter()
        .filter(|id| !past_ids.contains(*id))
        .count();
    // Header derives its date / wall-clock label from the snapshot's
    // captured_at, not from offset×24h: snapshots within the same day
    // share a date but different times, and only the file knows which.
    let (snap_local_dt, snap_date) = state.live_past_snapshot_meta.map_or_else(
        || {
            // Fallback when meta is missing: derive the date from offset
            // and pin time at midnight so the header still renders. The
            // empty sessions vec triggers the "No snapshot" placeholder
            // below, so the 00:00 fallback is never the headline.
            let d = chrono::Local::now().date_naive() - chrono::Duration::days(offset as i64);
            (
                d.and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_local_timezone(chrono::Local)
                    .unwrap(),
                d,
            )
        },
        |(t, d)| (t.with_timezone(&chrono::Local), d),
    );
    let mut lines: Vec<Line> = Vec::new();
    // Position `(offset/total)` lives in the block title next to the
    // `← older / → newer` nav hints; here show only the alive count to
    // avoid rendering the same N/M twice in one view.
    let title = format!(
        "  Alive at {} {} ({})  ",
        snap_date.format("%Y-%m-%d (%a)"),
        snap_local_dt.format("%H:%M"),
        past_visible.len()
    );
    let age = now - snap_local_dt.with_timezone(&Utc);
    let age_label = if age.num_minutes() < 60 {
        format!("{}m ago", age.num_minutes().max(0))
    } else if age.num_hours() < 24 {
        format!("{}h ago", age.num_hours())
    } else {
        format!("{}d ago", age.num_days())
    };
    let mut header_spans = vec![
        Span::styled(
            title,
            Style::default()
                .fg(theme::PRIMARY)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{age_label} · ages as of then · "),
            Style::default().fg(theme::TIME),
        ),
    ];
    if past_visible.is_empty() {
        header_spans.push(Span::styled(
            "frozen snapshot",
            Style::default().fg(theme::LABEL),
        ));
    } else {
        // Diff vs the current alive set: ● still-live, · ended, + new since.
        header_spans.push(Span::styled("vs now: ", Style::default().fg(theme::LABEL)));
        header_spans.push(Span::styled(
            format!("● {still_live_n} live"),
            Style::default().fg(theme::SUCCESS),
        ));
        // Two spaces, not ` · `: the ended count opens with its own `·` glyph.
        header_spans.push(Span::raw("  "));
        header_spans.push(Span::styled(
            format!("· {ended_n} ended"),
            Style::default().fg(theme::SUBDUED),
        ));
        header_spans.push(Span::styled(" · ", Style::default().fg(theme::LABEL)));
        header_spans.push(Span::styled(
            format!("{new_since_n} new"),
            Style::default().fg(theme::ACCENT),
        ));
    }
    lines.push(Line::from(header_spans));
    if past_visible.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No snapshot for this date (ccsight didn't run, or all sessions outside retention)",
            Style::default().fg(theme::PLACEHOLDER),
        )));
    }
    for (display_rank, s) in past_visible.iter().enumerate() {
        let selected = display_rank == state.live_selected;
        let still_live = now_active.contains(s.session_id.as_str());
        lines.extend(render_live_row(
            s,
            now,
            state,
            true,
            selected,
            max_summary_chars,
            inner_width,
            display_rank,
            &meta_map,
            Some(still_live),
        ));
    }

    // Auto-scroll: single section, 3 lines per row, header on line 0.
    let row_height = 3usize;
    let selected_first_line = 1 + state.live_selected * row_height;
    let selected_last_line = selected_first_line + row_height - 1;
    let visible_h = (chunks[0].height as usize)
        .saturating_sub(2)
        .max(row_height);
    let scroll = state.live_scroll;
    state.live_scroll = if state.live_selected == 0 {
        0
    } else if selected_first_line < scroll {
        selected_first_line
    } else if selected_last_line >= scroll + visible_h {
        selected_last_line + 1 - visible_h
    } else {
        scroll
    };

    let total_lines = lines.len();
    // Clamp to content like `draw_live`: stepping to a snapshot with fewer
    // (or zero) sessions leaves `live_selected` stale, so the cursor-follow
    // above can overshoot and render blank rows without this floor.
    state.live_scroll = state.live_scroll.min(total_lines.saturating_sub(visible_h));
    let title_str = format!(
        " Live · {} {} ({offset}/{total}) ",
        snap_date.format("%Y-%m-%d"),
        snap_local_dt.format("%H:%M")
    );
    // Direction hints: `→ newer` always (can step back toward the live view);
    // `← older` only when an older snapshot exists past this one. `t` jumps
    // back to the live `now` view, not "today" — today snapshots also exist.
    let mut title_spans = vec![Span::styled(title_str, Style::default().fg(theme::ACCENT))];
    if offset < total {
        title_spans.push(Span::styled(" ← older ", Style::default().fg(theme::LABEL)));
    }
    title_spans.push(Span::styled(
        "→ newer · T now ",
        Style::default().fg(theme::LABEL),
    ));
    let body = ratatui::widgets::Paragraph::new(lines)
        .scroll((state.live_scroll as u16, 0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER))
                .title(Line::from(title_spans)),
        );
    frame.render_widget(body, chunks[0]);
    draw_scrollbar(frame, chunks[0], state.live_scroll, total_lines, visible_h);

    let inner_rect = ratatui::layout::Rect {
        x: chunks[0].x + 1,
        y: chunks[0].y + 1,
        width: chunks[0].width.saturating_sub(2),
        height: chunks[0].height.saturating_sub(2),
    };
    // Past view = single section, so active_count == past_visible.len()
    // (every row participates in the same group). No paused frame here.
    state.layout.live_list_area = Some((inner_rect, state.live_scroll, past_visible.len()));
    state.layout.live_paused_list_area = None;

    let help_line = tab_help_bar(
        state,
        &[
            ("?", "help"),
            ("q", "quit"),
            ("↑↓", "session"),
            ("i", "info"),
            ("Enter", "view"),
            ("Space", "pin"),
            ("y", "copy-resume"),
            ("t", "title"),
            ("←→", "snapshot"),
            ("T", "now"),
            ("/", "search"),
        ],
    );
    frame.render_widget(ratatui::widgets::Paragraph::new(help_line), chunks[1]);
}

/// Compact "start → last" range string for a Live row. Today-only ranges
/// drop the date prefix to stay narrow; cross-day ranges keep both dates so
/// the boundary isn't hidden behind a time-only display.
fn format_session_range(
    start: chrono::DateTime<chrono::Utc>,
    last: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let start_local = start.with_timezone(&chrono::Local);
    let last_local = last.with_timezone(&chrono::Local);
    let today = now.with_timezone(&chrono::Local).date_naive();
    let s_date = start_local.date_naive();
    let l_date = last_local.date_naive();
    if s_date == l_date {
        if s_date == today {
            format!(
                "[{}–{}]",
                start_local.format("%H:%M"),
                last_local.format("%H:%M")
            )
        } else {
            format!(
                "[{} {}–{}]",
                s_date.format("%m-%d"),
                start_local.format("%H:%M"),
                last_local.format("%H:%M")
            )
        }
    } else {
        format!(
            "[{} {}–{} {}]",
            s_date.format("%m-%d"),
            start_local.format("%H:%M"),
            l_date.format("%m-%d"),
            last_local.format("%H:%M")
        )
    }
}

/// Resolve a live session's "last activity" timestamp. Prefers the JSONL's
/// last conversation entry (via daily_groups lookup) over filesystem mtime
/// or the pid file's heartbeat, both of which can run ahead when claude
/// rewrites metadata (`ai-title`) or the user resumes without sending a
/// message.
pub(crate) fn live_session_last_activity(
    state: &AppState,
    s: &crate::infrastructure::live_sessions::LiveSession,
    now: chrono::DateTime<chrono::Utc>,
) -> chrono::DateTime<chrono::Utc> {
    let meta_last = s.jsonl_path.as_ref().and_then(|target| {
        state
            .original_daily_groups
            .iter()
            .find_map(|g| g.sessions.iter().find(|sess| &sess.file_path == target))
            .map(|sess| sess.day_last_timestamp)
    });
    meta_last
        .or(s.updated_at)
        .or(s.jsonl_mtime)
        .or(s.started_at)
        .unwrap_or(now)
}

/// Re-order `live_paused` by real recency. The discovery sort key
/// (`jsonl_mtime`) clusters when paused JSONLs are bulk-touched, so it doesn't
/// track last use; this sorts by the same `live_session_last_activity` the age
/// column shows. Restorable (`⟳`) rows stay pinned on top, then last-activity
/// desc, then session_id (render-stable, lint #30).
pub(crate) fn sort_paused_by_recency(state: &mut AppState) {
    let now = chrono::Utc::now();
    let mut paused = std::mem::take(&mut state.live_paused);
    paused.sort_by(|a, b| {
        u8::from(!a.was_recently_live)
            .cmp(&u8::from(!b.was_recently_live))
            .then_with(|| {
                live_session_last_activity(state, b, now)
                    .cmp(&live_session_last_activity(state, a, now))
            })
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    state.live_paused = paused;
}

/// Compact age string for live-session list rows (4-6 chars wide). Used in
/// both the Live tab and the conv-view session list so the same session
/// reads the same length in both surfaces.
fn short_age(
    state: &AppState,
    s: &crate::infrastructure::live_sessions::LiveSession,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let last = live_session_last_activity(state, s, now);
    let secs = (now - last).num_seconds().max(0);
    // Match `render_live_row`: sub-minute resolution would lie about
    // freshness because the underlying poll only fires every few seconds.
    if secs < 60 {
        "now".to_string()
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

/// Build the two-line Live row. Line 1 carries status / age / project
/// (with branch) / tokens / model / short id — mirroring Daily's per-row
/// metadata line. Line 2 is the indented summary so long titles get full
/// width.
fn render_live_row<'a>(
    s: &crate::infrastructure::live_sessions::LiveSession,
    now: chrono::DateTime<chrono::Utc>,
    state: &AppState,
    is_active_section: bool,
    is_selected: bool,
    max_summary_chars: usize,
    row_inner_width: usize,
    row_idx: usize,
    // Representative newest-day meta per path — the caller built it once (refs
    // only, no clone); the lifetime cumulative comes from the memoized
    // `state.live_cumulative` cache. Together this row does two O(1) lookups
    // instead of the map re-walking / cloning every session per draw.
    meta_map: &std::collections::HashMap<&std::path::Path, &crate::aggregator::SessionInfo>,
    // Past view only: `Some(true)` = this frozen session is still in the current
    // alive set, `Some(false)` = it ended since the snapshot. `None` = today view
    // (no diff). Drives the leading glyph and dims ended rows.
    past_diff: Option<bool>,
) -> [Line<'a>; 3] {
    let path = s.jsonl_path.as_deref();
    let session_meta = path.and_then(|p| meta_map.get(p)).copied();
    let cumulative = path.and_then(|p| state.live_cumulative.get(p));
    let last = live_session_last_activity(state, s, now);
    // A frozen row's age is measured back from its snapshot: against the wall
    // clock every row of an old snapshot reads as the same stale span.
    let snapshot_at = past_diff.and_then(|_| state.live_past_snapshot_meta.map(|(t, _)| t));
    let age_ref = snapshot_at.unwrap_or(now);
    let secs = (age_ref - last).num_seconds().max(0);

    // Active section: three tiers — busy / today / older. `is_today` is the
    // single source of truth shared with the sort logic in `discover_live`.
    // Paused section: `⟳` flags rows that were alive when ccsight last saw
    // them (snapshot match), helping post-restart users find the windows
    // they had open before the reboot.
    let (glyph, glyph_color) = if let Some(still_live) = past_diff {
        // Past view: the glyph encodes the diff vs the current alive set, not
        // staleness — `●` still live now, `·` ended since this snapshot.
        if still_live {
            ("● ", theme::SUCCESS)
        } else {
            ("· ", theme::SUBDUED)
        }
    } else if !is_active_section {
        if s.was_recently_live {
            ("⟳ ", theme::ACCENT)
        } else {
            ("⏸ ", theme::LABEL)
        }
    } else if s.status.as_deref() == Some("busy") {
        ("🟢", theme::SUCCESS)
    } else if crate::infrastructure::live_sessions::is_today(last, now) {
        ("◉ ", theme::WARNING)
    } else {
        ("○ ", theme::LABEL)
    };
    // Polling cadence is coarser than 1s; collapse sub-minute to fixed. The
    // age is "time since the last conversation entry" — the transcript's
    // newest, so a session that kept going past its snapshot has no
    // as-of-then age to show.
    let age = if snapshot_at.is_some_and(|t| last > t) {
        "continued".to_string()
    } else if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    };

    let cwd_str = s.cwd.to_string_lossy().to_string();
    let project_label = state.project_label(&cwd_str);

    let title_from_meta =
        session_meta.and_then(|m| resolved_title(&state.session_titles, m).map(String::from));
    // first_user_message is the universal fallback — virtually every
    // session has at least one user prompt, so line 2 reads as "this is
    // what was asked" instead of an opaque "—".
    let first_msg = session_meta.and_then(|m| m.first_user_message.clone());
    // A live session whose JSONL hasn't been written yet (just-spawned,
    // pre-first-turn) has no meta at all. Say so explicitly instead of
    // rendering a bare "—" that reads as a broken/ghost row.
    let no_activity_yet = session_meta.is_none() && s.jsonl_mtime.is_none();
    let title = title_from_meta
        .or_else(|| {
            // Reject `name` values that are just a prefix of the session_id
            // (Claude Code's default placeholder for fresh sessions) — these
            // look like garbage hex strings, not titles.
            s.name.clone().filter(|n| {
                !n.is_empty() && n != &s.session_id && !s.session_id.starts_with(n.as_str())
            })
        })
        .or(first_msg)
        .unwrap_or_else(|| {
            if no_activity_yet {
                "(no activity yet)".to_string()
            } else {
                "—".to_string()
            }
        });

    let branch_short = session_meta.and_then(|m| {
        m.git_branch.as_ref().map(|b| {
            let name = b.split('/').next_back().unwrap_or(b);
            crate::text::truncate_with_ellipsis(name, 12)
        })
    });

    // Live rows are session-centric: cost/tokens are the lifetime total over
    // unfiltered groups, subagents included as on the Daily row. The column
    // renders even at 0 so the model column aligns.
    let tokens_total: u64 = cumulative.map_or(0, |c| {
        c.input_tokens + c.output_tokens + c.subagents.work_tokens
    });
    let tokens_str = crate::format_number(tokens_total);

    let (session_cost, session_unpriced) =
        cumulative.map_or((0.0, false), SessionCumulative::cost_with_subagents);
    let cost_str = format_cost_marked(session_cost, session_unpriced, 0);

    let model_field = session_meta.and_then(|m| m.model.as_ref());
    let model_short = model_field.map(|m| crate::aggregator::normalize_model_name(m));
    let model_clr = model_field.map_or(theme::LABEL, |m| model_color(m));

    // Pin + continued/new glyph pair, identical to Daily's pattern.
    let is_pinned = s
        .jsonl_path
        .as_ref()
        .is_some_and(|p| state.pins.is_pinned(p));
    let is_continued = session_meta.is_some_and(|m| m.is_continued);

    let marker = if is_selected { "▶ " } else { "  " };
    let marker_style = if is_selected {
        Style::default()
            .fg(theme::PRIMARY)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    // Leading rank column — 1-based within each section so paused rows
    // restart from 1 instead of continuing the active count. The selection
    // cursor (`state.live_selected`) still uses a flat global index for
    // j/k navigation; this column is for visual reference only.
    let rank_str = format!("{:>2} ", row_idx + 1);
    let rank_style = if is_selected {
        Style::default()
            .fg(theme::PRIMARY)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::FAINT)
    };
    let mut line1_spans = vec![
        Span::styled(rank_str, rank_style),
        Span::styled(marker, marker_style),
    ];
    if is_pinned {
        line1_spans.push(Span::styled("*", Style::default().fg(theme::WARNING)));
        line1_spans.push(Span::styled(
            if is_continued { "»" } else { "·" },
            Style::default().fg(if is_continued {
                theme::PRIMARY
            } else {
                theme::SUCCESS
            }),
        ));
    } else {
        line1_spans.push(Span::styled(
            if is_continued { " »" } else { " ·" },
            Style::default().fg(if is_continued {
                theme::PRIMARY
            } else {
                theme::SUCCESS
            }),
        ));
    }
    line1_spans.push(Span::styled(glyph, Style::default().fg(glyph_color)));
    line1_spans.push(Span::styled(
        format!(" {age:>9}  "),
        Style::default().fg(theme::TIME),
    ));
    // "Started → last" range. Prefer the JSONL's first entry; for a
    // just-spawned session with no JSONL yet, fall back to pid.json's
    // `started_at` so the row still shows when the session began rather
    // than omitting the column entirely. Paused snapshot rows have
    // neither — they skip the range.
    let range_start = session_meta
        .map(|m| m.session_first_timestamp)
        .or(s.started_at);
    if let Some(start) = range_start {
        // A frozen row's range ends where the snapshot saw it, not at the
        // transcript's newest entry.
        let range_end = snapshot_at.map_or(last, |t| last.min(t));
        let range = format_session_range(start, range_end, age_ref);
        line1_spans.push(Span::styled(
            format!("{range}  "),
            Style::default().fg(theme::TIME),
        ));
    }
    line1_spans.push(Span::styled(
        project_label.clone(),
        Style::default().fg(theme::WARM),
    ));
    if let Some(b) = branch_short {
        line1_spans.push(Span::styled(
            format!("#{b}"),
            Style::default().fg(theme::BRANCH),
        ));
    }
    line1_spans.push(Span::styled(
        format!("  {tokens_str}"),
        Style::default().fg(theme::PRIMARY),
    ));
    line1_spans.push(Span::styled(
        format!(" {cost_str}"),
        cost_style_marked(session_cost, session_unpriced),
    ));
    // Model badge is the lowest-priority trailing element — drop it
    // entirely on rows so tight that even `[i]` would otherwise be clipped.
    let info_btn_width = 3usize;
    let pre_model_width: usize = spans_width(&line1_spans);
    if let Some(ms) = model_short {
        let badge = format!(" [{ms}]");
        let badge_w = unicode_width::UnicodeWidthStr::width(badge.as_str());
        if pre_model_width + badge_w + info_btn_width <= row_inner_width {
            line1_spans.push(Span::styled(badge, Style::default().fg(model_clr)));
        }
    }
    // Reserve 3 cols for `[i]` and ≥1 col of separation; the session_id
    // column flexes to whatever's left so the action button always stays
    // visible even when projects / titles eat the row. When even a single-
    // char id won't fit, drop the id span entirely.
    let prefix_width: usize = spans_width(&line1_spans);
    let id_budget = row_inner_width.saturating_sub(prefix_width + info_btn_width + 3);
    let id_chars = s.session_id.chars().count();
    let id_width = if id_budget == 0 {
        0
    } else if id_chars <= id_budget {
        let display = s.session_id.clone();
        let width = unicode_width::UnicodeWidthStr::width(display.as_str());
        line1_spans.push(Span::styled(
            format!("  {display}"),
            Style::default().fg(theme::SESSION_ID),
        ));
        width + 2
    } else {
        let display = crate::text::truncate_with_ellipsis(&s.session_id, id_budget);
        let width = unicode_width::UnicodeWidthStr::width(display.as_str());
        line1_spans.push(Span::styled(
            format!("  {display}"),
            Style::default().fg(theme::SESSION_ID),
        ));
        width + 2
    };
    let pad = row_inner_width.saturating_sub(prefix_width + id_width + info_btn_width);
    if pad > 0 {
        line1_spans.push(Span::raw(" ".repeat(pad)));
    }
    line1_spans.push(Span::styled("[i]", Style::default().fg(theme::PRIMARY)));

    let mut line1 = Line::from(line1_spans);

    // Daily uses a 2-space indent on line 2 so the summary "hangs" to the
    // left of the metadata column; match that.
    let truncated = truncate_with_ellipsis(&title, max_summary_chars);
    let summary_style = if title == "—" {
        Style::default().fg(theme::FAINT)
    } else {
        Style::default().fg(theme::TEXT_BRIGHT)
    };
    // 5-space indent on line 2 = rank(3) + marker(2) so the summary
    // visually hangs under line 1's first metadata column.
    let mut line2 = Line::from(vec![
        Span::raw("     "),
        Span::styled(truncated, summary_style),
    ]);

    // Line 3: last user message snippet, prefixed with `❯` to read as a
    // quoted user prompt. Hidden behind a "—" when the session has nothing
    // captured yet so the row height stays uniform (auto-scroll math
    // depends on a fixed `row_height`).
    let last_msg = session_meta
        .and_then(|m| m.last_user_message.clone())
        .filter(|m| !m.is_empty());
    let (msg_text, msg_style) = if let Some(m) = last_msg {
        (
            truncate_with_ellipsis(&m, max_summary_chars),
            Style::default().fg(theme::SNIPPET),
        )
    } else {
        ("—".to_string(), Style::default().fg(theme::FAINT))
    };
    let mut line3 = Line::from(vec![
        Span::styled("     ❯ ", Style::default().fg(theme::LABEL)),
        Span::styled(msg_text, msg_style),
    ]);

    // Selection bg (SELECTED_BG, matches Daily). Three layers: per-span patch
    // (BOLD transitions leave holes), padding span (trailing cells use
    // widget style not Line::style), Line::style as fallback.
    if is_selected {
        let bg = theme::SELECTED_BG;
        let sel_style = Style::default().bg(bg);
        let line1_width = spans_width(&line1.spans);
        let line2_width = spans_width(&line2.spans);
        let line3_width = spans_width(&line3.spans);
        for span in &mut line1.spans {
            span.style = span.style.bg(bg);
        }
        for span in &mut line2.spans {
            span.style = span.style.bg(bg);
        }
        for span in &mut line3.spans {
            span.style = span.style.bg(bg);
        }
        for (line, width) in [
            (&mut line1, line1_width),
            (&mut line2, line2_width),
            (&mut line3, line3_width),
        ] {
            let pad = row_inner_width.saturating_sub(width);
            if pad > 0 {
                line.spans.push(Span::styled(" ".repeat(pad), sel_style));
            }
        }
        line1 = line1.style(sel_style);
        line2 = line2.style(sel_style);
        line3 = line3.style(sel_style);
    }

    // Ended-since rows are background context — dim them, except when selected
    // (the selection highlight must stay legible).
    if past_diff == Some(false) && !is_selected {
        for line in [&mut line1, &mut line2, &mut line3] {
            for span in &mut line.spans {
                span.style = span.style.fg(theme::SUBDUED);
            }
        }
    }

    [line1, line2, line3]
}

fn draw_daily(frame: &mut Frame, area: Rect, state: &mut AppState) {
    if state.daily_groups.is_empty() {
        let empty = Paragraph::new("No sessions")
            .style(Style::default().fg(theme::PLACEHOLDER))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme::BORDER))
                    .title(Span::styled(" Daily ", Style::default().fg(theme::PRIMARY))),
            );
        frame.render_widget(empty, area);
        return;
    }

    if state.selected_day >= state.daily_groups.len() {
        state.selected_day = state.daily_groups.len().saturating_sub(1);
    }
    let group = &state.daily_groups[state.selected_day];
    let today = Local::now().date_naive();
    let is_today = group.date == today;

    let all_sessions = &group.sessions;
    let sessions: Vec<_> = all_sessions.iter().filter(|s| !s.is_subagent).collect();

    let show_stats_panel = area.width >= 60;

    let (header_chunk, stats_chunk, sessions_chunk, help_chunk) = if show_stats_panel {
        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(9),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);
        (chunks[0], Some(chunks[1]), chunks[2], chunks[3])
    } else {
        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);
        (chunks[0], None, chunks[1], chunks[2])
    };

    let date_str = group.date.format("%Y-%m-%d (%a)").to_string();
    let mut date_label = if is_today {
        format!("🟢 {date_str} - Today")
    } else {
        date_str
    };
    // The cost surfaces count only billable days, so their day total is lower
    // than this view's. Naming the reason on the day itself explains the gap
    // where the user meets it, without a permanent caption on every other day.
    if group.billable_tokens() == 0 {
        date_label.push_str(" · no billable");
    }

    let left_arrow = if state.selected_day < state.daily_groups.len().saturating_sub(1) {
        "◀ "
    } else {
        "  "
    };
    let right_arrow = if state.selected_day > 0 { " ▶" } else { "  " };

    let nav_text = format!(
        "{}{}{}  ({}/{})",
        left_arrow,
        date_label,
        right_arrow,
        state.selected_day + 1,
        state.daily_groups.len(),
    );

    let nav = Paragraph::new(nav_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER)),
        )
        .centered();

    state.layout.daily_header_area = Some(header_chunk);
    frame.render_widget(nav, header_chunk);

    let continued_count = sessions.iter().filter(|s| s.is_continued).count();
    let new_count = sessions.len() - continued_count;

    let mut hourly_tokens: std::collections::HashMap<u8, u64> = std::collections::HashMap::new();
    let mut project_tokens: std::collections::HashMap<String, u64> =
        std::collections::HashMap::new();
    let mut model_tokens: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut tool_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for s in all_sessions {
        for (hour, tokens) in &s.day_hourly_work_tokens {
            *hourly_tokens.entry(*hour).or_insert(0) += tokens;
        }
        let mut session_total: u64 = 0;
        for (model, ts) in &s.day_tokens_by_model {
            let model_total = ts.work_tokens();
            *model_tokens.entry(model.clone()).or_insert(0) += model_total;
            session_total += model_total;
        }
        for (tool, count) in &s.day_tool_usage {
            *tool_counts.entry(tool.clone()).or_insert(0) += count;
        }
        let proj = state.project_label(&s.project_name);
        *project_tokens.entry(proj).or_insert(0) += session_total;
    }

    let max_hourly_raw = hourly_tokens.values().max().copied().unwrap_or(0);
    let max_hourly = max_hourly_raw.max(1);
    let active_hours: usize = hourly_tokens.values().filter(|&&t| t > 0).count();
    let peak_hour = hourly_tokens
        .iter()
        .max_by_key(|(_, t)| *t)
        .map(|(h, t)| (*h, *t));

    let total_day_tokens: u64 = all_sessions
        .iter()
        .map(|s| {
            s.day_tokens_by_model
                .values()
                .map(super::aggregator::stats::TokenStats::work_tokens)
                .sum::<u64>()
        })
        .sum();
    let visible_session_tokens: u64 = sessions
        .iter()
        .map(|s| {
            s.day_tokens_by_model
                .values()
                .map(super::aggregator::stats::TokenStats::work_tokens)
                .sum::<u64>()
        })
        .sum();
    // The gap between Activity (all sessions) and the Sessions list (filtered)
    // is subagent work — either separate agent-* JSONLs OR spawn-embedded
    // activity attributed to the parent session's model totals but not shown
    // as its own row. Surfacing it keeps the headline number from disagreeing
    // with the rows below.
    let subagent_day_tokens: u64 = total_day_tokens.saturating_sub(visible_session_tokens);

    let calculator = CostCalculator::global();
    let total_day_cost: f64 = all_sessions.iter().map(|s| s.cost(calculator)).sum();
    let day_unpriced = all_sessions
        .iter()
        .any(|s| s.has_unpriced_model(calculator));

    let show_timeline = area.width >= 80;
    let (timeline_area, breakdown_area) = if let Some(stats_area) = stats_chunk {
        if show_timeline {
            let panel =
                Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)])
                    .split(stats_area);
            (Some(panel[0]), Some(panel[1]))
        } else {
            (None, Some(stats_area))
        }
    } else {
        (None, None)
    };

    if let Some(timeline_rect) = timeline_area {
        let bar_chars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let inner_height = timeline_rect.height.saturating_sub(4) as usize;

        let mut timeline_lines: Vec<Line> = Vec::new();

        let has_data = max_hourly_raw > 0;
        let y_label_width = if has_data { 6 } else { 0 };
        for row in (0..inner_height).rev() {
            let threshold = (row as f64 + 0.5) / inner_height as f64;
            let y_label = if !has_data {
                String::new()
            } else if row == inner_height - 1 {
                format!("{:>5} ", crate::format_number(max_hourly))
            } else {
                " ".repeat(y_label_width)
            };
            let mut row_chars = String::new();
            for hour in 0..24u8 {
                let tokens = hourly_tokens.get(&hour).copied().unwrap_or(0);
                let ratio = if max_hourly > 0 {
                    tokens as f64 / max_hourly as f64
                } else {
                    0.0
                };
                if ratio >= threshold {
                    row_chars.push(bar_chars[7]);
                } else if ratio >= threshold - (1.0 / inner_height as f64) && ratio > 0.0 {
                    let sub_level = ((ratio - (threshold - 1.0 / inner_height as f64))
                        * inner_height as f64
                        * 8.0) as usize;
                    row_chars.push(bar_chars[sub_level.min(7)]);
                } else {
                    row_chars.push(' ');
                }
            }
            timeline_lines.push(Line::from(vec![
                Span::styled(y_label, Style::default().fg(theme::META)),
                Span::styled(row_chars, Style::default().fg(theme::PRIMARY)),
            ]));
        }

        let time_label = if timeline_rect.width >= 30 {
            "0     6    12    18   24"
        } else {
            "0  6  12  18  24"
        };
        timeline_lines.push(
            Line::from(Span::styled(time_label, Style::default().fg(theme::TIME)))
                .alignment(ratatui::layout::Alignment::Center),
        );

        let inner_width = timeline_rect.width.saturating_sub(2) as usize;
        if inner_width >= 28 {
            let peak_info = if let Some((hour, _)) = peak_hour {
                format!(" peak:{}-{}h", hour, hour + 1)
            } else {
                String::new()
            };
            // Parens read as a breakdown of the preceding total. A `+N sub`
            // prefix reads as additive and misleads users into summing the
            // two numbers.
            let sub_suffix = if subagent_day_tokens > 0 {
                format!(" ({} sub)", crate::format_number(subagent_day_tokens))
            } else {
                String::new()
            };
            timeline_lines.push(
                Line::from(vec![
                    Span::styled(
                        format!("active:{active_hours}h"),
                        Style::default().fg(theme::TIME),
                    ),
                    Span::styled(peak_info, Style::default().fg(theme::WARM)),
                    Span::styled(" ", Style::default().fg(theme::LABEL)),
                    Span::styled(
                        crate::format_number(total_day_tokens),
                        Style::default().fg(theme::PRIMARY),
                    ),
                    Span::styled(sub_suffix, Style::default().fg(theme::META)),
                    Span::styled(" ", Style::default().fg(theme::LABEL)),
                    Span::styled(
                        format_cost_marked(total_day_cost, day_unpriced, 0),
                        cost_style_marked(total_day_cost, day_unpriced),
                    ),
                ])
                .alignment(ratatui::layout::Alignment::Center),
            );
        } else {
            timeline_lines.push(
                Line::from(vec![
                    Span::styled(
                        format!("{active_hours}h "),
                        Style::default().fg(theme::SUCCESS),
                    ),
                    Span::styled(
                        format_cost_marked(total_day_cost, day_unpriced, 0),
                        cost_style_marked(total_day_cost, day_unpriced),
                    ),
                ])
                .alignment(ratatui::layout::Alignment::Center),
            );
        }

        let timeline = Paragraph::new(timeline_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER))
                .title(Span::styled(
                    " Activity ",
                    Style::default().fg(theme::PRIMARY),
                )),
        );
        frame.render_widget(timeline, timeline_rect);
    }

    let mut sorted_projects: Vec<_> = project_tokens.iter().collect();
    sorted_projects.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let total_project_tokens: u64 = project_tokens.values().sum();
    let mut sorted_models: Vec<_> = model_tokens.iter().collect();
    sorted_models.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));

    let total_model_tokens: u64 = model_tokens.values().sum();

    let mut sorted_tools: Vec<_> = tool_counts.iter().collect();
    sorted_tools.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let total_tool_count: usize = tool_counts.values().sum();

    let mut all_items: Vec<BreakdownItem> = vec![];
    for (proj, tokens) in &sorted_projects {
        let pct = if total_project_tokens > 0 {
            **tokens as f64 / total_project_tokens as f64 * 100.0
        } else {
            0.0
        };
        all_items.push(BreakdownItem::Project((*proj).clone(), **tokens, pct));
    }
    for (model, tokens) in &sorted_models {
        let pct = if total_model_tokens > 0 {
            **tokens as f64 / total_model_tokens as f64 * 100.0
        } else {
            0.0
        };
        all_items.push(BreakdownItem::Model((*model).clone(), **tokens, pct));
    }
    let tools_start_idx = sorted_projects.len() + sorted_models.len();
    for (tool, count) in &sorted_tools {
        let pct = if total_tool_count > 0 {
            **count as f64 / total_tool_count as f64 * 100.0
        } else {
            0.0
        };
        all_items.push(BreakdownItem::Tool((*tool).clone(), **count, pct));
    }

    let total_items = all_items.len();

    fn render_column_items(
        items: &[(String, String, f64)],
        color: Color,
        max_lines: usize,
        col_width: usize,
    ) -> Vec<Line<'static>> {
        items
            .iter()
            .take(max_lines)
            .map(|(name, info, pct)| {
                let bar_len = breakdown_bar_len(*pct);
                let bar = "█".repeat(bar_len);
                let max_name = col_width.saturating_sub(bar_len + info.len() + 3);
                let short = truncate_with_ellipsis(name, max_name);
                Line::from(vec![
                    Span::styled(format!(" {bar}"), Style::default().fg(color)),
                    Span::styled(
                        format!(" {short} {info}"),
                        Style::default().fg(theme::TEXT_BRIGHT),
                    ),
                ])
            })
            .collect()
    }

    /// Per-row colored variant of `render_column_items` — bar color from
    /// each tool key's category. Keeps `agent:Explore` colored as
    /// Subagent in the Daily breakdown rather than uniform Tools.
    fn render_tool_column_items(
        items: &[(String, String, f64)],
        max_lines: usize,
        col_width: usize,
    ) -> Vec<Line<'static>> {
        items
            .iter()
            .take(max_lines)
            .map(|(name, info, pct)| {
                let bar_len = breakdown_bar_len(*pct);
                let bar = "█".repeat(bar_len);
                let max_name = col_width.saturating_sub(bar_len + info.len() + 3);
                let short = truncate_with_ellipsis(name, max_name);
                let color = tool_category_color(name);
                Line::from(vec![
                    Span::styled(format!(" {bar}"), Style::default().fg(color)),
                    Span::styled(
                        format!(" {short} {info}"),
                        Style::default().fg(theme::TEXT_BRIGHT),
                    ),
                ])
            })
            .collect()
    }

    state.layout.breakdown_panel_area = breakdown_area;
    let breakdown_popup_data = if let Some(breakdown_rect) = breakdown_area {
        let outer_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::BORDER))
            .title(Span::styled(
                format!(" Breakdown ({total_items}) [b] "),
                Style::default().fg(theme::PRIMARY),
            ));
        let inner = outer_block.inner(breakdown_rect);
        frame.render_widget(outer_block, breakdown_rect);

        let cols = Layout::horizontal([
            Constraint::Percentage(33),
            Constraint::Percentage(34),
            Constraint::Percentage(33),
        ])
        .split(inner);

        let max_lines = inner.height.saturating_sub(1) as usize;

        let proj_items: Vec<_> = sorted_projects
            .iter()
            .map(|(name, tokens)| {
                let short = state.project_label(name);
                let pct = if total_project_tokens > 0 {
                    **tokens as f64 / total_project_tokens as f64 * 100.0
                } else {
                    0.0
                };
                let label = crate::text::format_pct(**tokens, total_project_tokens);
                (short, label, pct)
            })
            .collect();
        let model_items: Vec<_> = sorted_models
            .iter()
            .map(|(name, tokens)| {
                let short = crate::aggregator::normalize_model_name(name);
                let pct = if total_model_tokens > 0 {
                    **tokens as f64 / total_model_tokens as f64 * 100.0
                } else {
                    0.0
                };
                let label = crate::text::format_pct(**tokens, total_model_tokens);
                (short, label, pct)
            })
            .collect();
        let tool_items: Vec<_> = sorted_tools
            .iter()
            .map(|(name, count)| {
                let pct = if total_tool_count > 0 {
                    **count as f64 / total_tool_count as f64 * 100.0
                } else {
                    0.0
                };
                ((*name).clone(), format!("{count}x"), pct)
            })
            .collect();

        let proj_lines =
            render_column_items(&proj_items, theme::WARM, max_lines, cols[0].width as usize);
        let model_lines = render_column_items(
            &model_items,
            theme::PRIMARY,
            max_lines,
            cols[1].width as usize,
        );
        let tool_lines = render_tool_column_items(&tool_items, max_lines, cols[2].width as usize);

        let proj_title = format!(" Projects({}) ", sorted_projects.len());
        let model_title = format!(" Models({}) ", sorted_models.len());
        let tool_title = format!(" Tools({}) ", sorted_tools.len());

        frame.render_widget(
            Paragraph::new(proj_lines).block(
                Block::default().title(Span::styled(proj_title, Style::default().fg(theme::WARM))),
            ),
            cols[0],
        );
        frame.render_widget(
            Paragraph::new(model_lines).block(Block::default().title(Span::styled(
                model_title,
                Style::default().fg(theme::PRIMARY),
            ))),
            cols[1],
        );
        frame.render_widget(
            Paragraph::new(tool_lines).block(Block::default().title(Span::styled(
                tool_title,
                Style::default().fg(theme::SUCCESS),
            ))),
            cols[2],
        );

        if state.daily_breakdown_focus {
            Some((all_items, sorted_projects.len(), tools_start_idx))
        } else {
            None
        }
    } else {
        None
    };

    let content_width = area.width.saturating_sub(4) as usize;
    let max_summary_len = content_width.saturating_sub(4).max(15);

    let session_calculator = CostCalculator::global();
    // [i] button: 3-cell hit zone right-aligned in the list inner area, mouse-only entry point for session details.
    let inner_width = sessions_chunk.width.saturating_sub(2) as usize;
    let info_btn_width = 3usize;

    let subagents_by_parent = group.subagent_spend_by_parent();
    let items: Vec<ListItem> = sessions
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let start_time = s.day_first_timestamp.with_timezone(&Local).format("%H:%M");
            let end_time = s.day_last_timestamp.with_timezone(&Local).format("%H:%M");
            let time_str = format!("{start_time}–{end_time}");
            // Subagent transcripts are hidden from this list, so their spend
            // rides on the row of the session that launched them.
            let subagents = subagents_by_parent
                .get(s.file_path.as_path())
                .copied()
                .unwrap_or_default();
            let session_tokens: u64 = s
                .day_tokens_by_model
                .values()
                .map(super::aggregator::stats::TokenStats::work_tokens)
                .sum::<u64>()
                + subagents.work_tokens;
            let tokens_str = crate::format_number(session_tokens);
            let session_cost: f64 = s.cost(session_calculator) + subagents.cost;
            let session_unpriced = s.has_unpriced_model(session_calculator) || subagents.unpriced;
            let cost_str = format_cost_marked(session_cost, session_unpriced, 0);

            let model_short = s.model.as_ref().map_or_else(
                || "?".to_string(),
                |m| crate::aggregator::normalize_model_name(m),
            );
            let model_clr = s.model.as_ref().map_or(theme::LABEL, |m| model_color(m));

            let is_selected = i == state.selected_session;
            let is_updating = state.updating_session == Some((state.selected_day, i));
            let now = chrono::Utc::now();
            let is_recent = (now - s.day_last_timestamp).num_minutes() < 5;
            let prefix = if is_updating {
                "🔄"
            } else if is_selected {
                "▶ "
            } else {
                "  "
            };

            let project_short = state.project_label(&s.project_name);

            let title_source = resolved_title(&state.session_titles, s);
            let summary_text = title_source.map(|sum| truncate_with_ellipsis(sum, max_summary_len));

            let time_style = if is_recent {
                Style::default().fg(theme::ACCENT)
            } else {
                Style::default().fg(theme::TIME)
            };

            let branch_short = s.git_branch.as_ref().map(|b| {
                let name = b.split('/').next_back().unwrap_or(b);
                crate::text::truncate_with_ellipsis(name, 12)
            });

            let mut line1_spans = vec![Span::raw(prefix)];
            let pinned = state.pins.is_pinned(&s.file_path);
            if pinned {
                line1_spans.push(Span::styled("*", Style::default().fg(theme::WARNING)));
                line1_spans.push(Span::styled(
                    if s.is_continued { "»" } else { "·" },
                    Style::default().fg(if s.is_continued {
                        theme::PRIMARY
                    } else {
                        theme::SUCCESS
                    }),
                ));
            } else {
                line1_spans.push(Span::styled(
                    if s.is_continued { " »" } else { " ·" },
                    Style::default().fg(if s.is_continued {
                        theme::PRIMARY
                    } else {
                        theme::SUCCESS
                    }),
                ));
            }
            line1_spans.push(Span::styled(format!("{time_str} "), time_style));
            line1_spans.push(Span::styled(
                project_short.clone(),
                Style::default().fg(theme::WARM),
            ));
            if let Some(ref branch) = branch_short {
                line1_spans.push(Span::styled(
                    format!("#{branch}"),
                    Style::default().fg(theme::BRANCH),
                ));
            }
            line1_spans.push(Span::styled(
                format!("  {tokens_str}"),
                Style::default().fg(theme::PRIMARY),
            ));
            line1_spans.push(Span::styled(
                format!(" {cost_str}"),
                cost_style_marked(session_cost, session_unpriced),
            ));
            line1_spans.push(Span::styled(
                format!(" [{model_short}]"),
                Style::default().fg(model_clr),
            ));

            let line1_width: usize = spans_width(&line1_spans);
            let pad = inner_width.saturating_sub(line1_width + info_btn_width);
            if pad > 0 {
                line1_spans.push(Span::raw(" ".repeat(pad)));
            }
            line1_spans.push(Span::styled("[i]", Style::default().fg(theme::PRIMARY)));

            let line1 = Line::from(line1_spans);

            let line2_spans = if let Some(summary) = summary_text {
                vec![
                    Span::raw("  "),
                    Span::styled(summary, Style::default().fg(theme::TEXT_BRIGHT)),
                ]
            } else if let Some(ref title) = s.custom_title {
                vec![
                    Span::raw("  "),
                    // A user-set title reads a step below an AI summary, so
                    // rows still awaiting a summary stand out.
                    Span::styled(title.clone(), Style::default().fg(theme::LABEL)),
                ]
            } else {
                vec![
                    Span::raw("  "),
                    Span::styled("—", Style::default().fg(theme::FAINT)),
                ]
            };
            let line2 = Line::from(line2_spans);
            // ccsight's own generation calls stay listed but set back, so the
            // user's own sessions read first.
            let (line1, line2) = if s.ccsight_generated().is_some() {
                (subdued_line(line1), subdued_line(line2))
            } else {
                (line1, line2)
            };

            let has_summary = s.summary.is_some();
            let item = ListItem::new(vec![line1, line2]);
            if is_updating {
                item.style(Style::default().bg(theme::WARNING).fg(theme::TEXT_DARK))
            } else if is_selected {
                item.style(Style::default().bg(theme::SELECTED_BG))
            } else if !has_summary {
                item.style(Style::default().fg(theme::LABEL))
            } else {
                item
            }
        })
        .collect();

    let item_height = 2;
    let visible_items_count = (sessions_chunk.height.saturating_sub(2) / item_height) as usize;
    let scroll_offset = if state.selected_session >= visible_items_count {
        state.selected_session - visible_items_count + 1
    } else {
        0
    };

    let visible_items: Vec<ListItem> = items
        .into_iter()
        .skip(scroll_offset)
        .take(visible_items_count)
        .collect();

    let list = List::new(visible_items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::BORDER))
            .title(Line::from(vec![
                Span::styled(
                    format!(
                        " Sessions ({}/{}) ",
                        state.selected_session + 1,
                        sessions.len()
                    ),
                    Style::default().fg(theme::PRIMARY),
                ),
                Span::styled(
                    format!("· new: {new_count}"),
                    Style::default()
                        .fg(theme::SUCCESS)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    format!("» continued: {continued_count}"),
                    Style::default()
                        .fg(theme::PRIMARY)
                        .add_modifier(Modifier::BOLD),
                ),
            ])),
    );

    frame.render_widget(list, sessions_chunk);

    draw_scrollbar(
        frame,
        sessions_chunk,
        scroll_offset,
        sessions.len(),
        visible_items_count,
    );

    state.layout.session_list_area = Some((sessions_chunk, scroll_offset, item_height as usize));

    let help_line = Paragraph::new(tab_help_bar(
        state,
        &[
            ("?", "help"),
            ("q", "quit"),
            ("←→", "day"),
            ("↑↓", "session"),
            ("i", "info"),
            ("Enter", "view"),
            ("s", "summary"),
            ("S", "day-summary"),
            ("t", "title"),
            ("b", "breakdown"),
            ("/", "search"),
            ("Space", "pin"),
            ("m", "pins"),
        ],
    ));
    frame.render_widget(help_line, help_chunk);

    if let Some((items, models_start, tools_start)) = breakdown_popup_data {
        draw_breakdown_detail_popup(frame, area, &items, models_start, tools_start, state);
    }
}

fn draw_split_session_list(frame: &mut Frame, area: Rect, state: &mut AppState, is_active: bool) {
    use ratatui::widgets::{List, ListItem};

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    let item_height: usize = 2;

    let border_color = if is_active {
        theme::PRIMARY
    } else {
        theme::BORDER
    };

    struct SessionDisplay {
        file_path: std::path::PathBuf,
        time_or_date: String,
        project_short: String,
        summary: Option<String>,
        is_recent: bool,
        is_pinned: bool,
        is_continued: bool,
    }

    let now = chrono::Utc::now();
    let proj_max_len = area.width.saturating_sub(11) as usize;
    let summary_max_len = area.width.saturating_sub(6) as usize;

    let project_labels = &state.project_labels;
    let label_for = |name: &str| -> String {
        project_labels
            .get(name)
            .cloned()
            .unwrap_or_else(|| shorten_project(name).to_string())
    };
    let (sessions_display, title) = match state.conv_list_mode {
        crate::ConvListMode::Pinned => {
            let pinned: Vec<SessionDisplay> = state
                .pins
                .entries()
                .iter()
                .map(|entry| {
                    state
                        .original_daily_groups
                        .iter()
                        .find_map(|g| {
                            g.sessions
                                .iter()
                                .find(|s| s.file_path == entry.path)
                                .map(|s| SessionDisplay {
                                    file_path: s.file_path.clone(),
                                    time_or_date: g.date.format("%Y-%m-%d").to_string(),
                                    project_short: label_for(&s.project_name),
                                    summary: resolved_title(&state.session_titles, s)
                                        .map(ToString::to_string),
                                    is_recent: false,
                                    is_pinned: true,
                                    is_continued: s.is_continued,
                                })
                        })
                        .unwrap_or_else(|| SessionDisplay {
                            file_path: entry.path.clone(),
                            time_or_date: "????-??-??".to_string(),
                            project_short: "(deleted)".to_string(),
                            summary: None,
                            is_recent: false,
                            is_pinned: true,
                            is_continued: false,
                        })
                })
                .collect();
            let count = pinned.len();
            (pinned, format!(" * Pinned ({count}) "))
        }
        crate::ConvListMode::All => {
            let pins_ref = &state.pins;
            let session_titles = &state.session_titles;
            let all: Vec<SessionDisplay> = state
                .original_daily_groups
                .iter()
                .flat_map(|g| {
                    g.sessions
                        .iter()
                        .filter(|s| !s.is_subagent)
                        .map(move |s| SessionDisplay {
                            file_path: s.file_path.clone(),
                            time_or_date: g.date.format("%Y-%m-%d").to_string(),
                            project_short: label_for(&s.project_name),
                            summary: resolved_title(session_titles, s).map(ToString::to_string),
                            is_recent: false,
                            is_pinned: pins_ref.is_pinned(&s.file_path),
                            is_continued: s.is_continued,
                        })
                })
                .collect();
            let count = all.len();
            (all, format!(" All ({count}) "))
        }
        crate::ConvListMode::Live => {
            // Live list: active sessions first (newest activity), then paused.
            let collect_one = |s: &crate::infrastructure::live_sessions::LiveSession,
                               is_active: bool|
             -> SessionDisplay {
                let cwd_str = s.cwd.to_string_lossy().to_string();
                let summary = s
                    .jsonl_path
                    .as_ref()
                    .and_then(|p| {
                        state.original_daily_groups.iter().find_map(|g| {
                            g.sessions
                                .iter()
                                .find(|sess| sess.file_path == *p)
                                .and_then(|sess| {
                                    resolved_title(&state.session_titles, sess).map(String::from)
                                })
                        })
                    })
                    .or_else(|| s.name.clone());
                let now = chrono::Utc::now();
                let last = live_session_last_activity(state, s, now);
                // Mirror render_live_row: 5-tier glyph (busy / today / older /
                // snapshot-recovered paused / paused) so the conv-pane list
                // tells the same story as the Live tab itself.
                let glyph = if !is_active {
                    if s.was_recently_live { "⟳" } else { "⏸" }
                } else if s.status.as_deref() == Some("busy") {
                    "🟢"
                } else if crate::infrastructure::live_sessions::is_today(last, now) {
                    "◉"
                } else {
                    "○"
                };
                let pinned = s
                    .jsonl_path
                    .as_ref()
                    .is_some_and(|p| state.pins.is_pinned(p));
                // Multi-day flag for the leading `»` / `·` glyph. Same lookup
                // pattern as `render_live_row` so the narrow conv-pane list
                // and the full Live tab agree on the session-span indicator.
                let is_continued = s
                    .jsonl_path
                    .as_ref()
                    .and_then(|target| {
                        state.original_daily_groups.iter().find_map(|g| {
                            g.sessions
                                .iter()
                                .find(|sess| &sess.file_path == target)
                                .map(|sess| sess.is_continued)
                        })
                    })
                    .unwrap_or(false);
                let time_label = format!("{glyph} {}", short_age(state, s, now));
                SessionDisplay {
                    file_path: s.jsonl_path.clone().unwrap_or_default(),
                    time_or_date: time_label,
                    project_short: label_for(&cwd_str),
                    summary,
                    is_recent: is_active && s.status.as_deref() == Some("busy"),
                    is_pinned: pinned,
                    is_continued,
                }
            };
            let active_count = state.live_active.len();
            let paused_count = state.live_paused.len();
            let mut rows: Vec<SessionDisplay> = state
                .live_active
                .iter()
                .map(|s| collect_one(s, true))
                .collect();
            rows.extend(state.live_paused.iter().map(|s| collect_one(s, false)));
            // Show active + paused split explicitly so the conv-pane count
            // doesn't read as a single number that disagrees with the Live tab
            // header (which counts active only).
            let title = if paused_count > 0 {
                format!(" Live ({active_count} + {paused_count}) ")
            } else {
                format!(" Live ({active_count}) ")
            };
            (rows, title)
        }
        crate::ConvListMode::Day => {
            if state.daily_groups.is_empty() {
                let empty = Paragraph::new("No sessions")
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(border_color))
                            .title(Span::styled(
                                " Sessions ",
                                Style::default().fg(theme::PRIMARY),
                            )),
                    )
                    .style(Style::default().fg(theme::LABEL));
                frame.render_widget(empty, area);
                return;
            }
            let group = &state.daily_groups[state.selected_day];
            let sessions: Vec<SessionDisplay> = group
                .sessions
                .iter()
                .filter(|s| !s.is_subagent)
                .map(|s| {
                    let is_recent = (now - s.day_last_timestamp).num_minutes() < 5;
                    SessionDisplay {
                        file_path: s.file_path.clone(),
                        time_or_date: s
                            .day_last_timestamp
                            .with_timezone(&Local)
                            .format("%H:%M")
                            .to_string(),
                        project_short: label_for(&s.project_name),
                        summary: resolved_title(&state.session_titles, s).map(ToString::to_string),
                        is_recent,
                        is_pinned: state.pins.is_pinned(&s.file_path),
                        is_continued: s.is_continued,
                    }
                })
                .collect();
            let date_str = group.date.format("%m-%d").to_string();
            let count = sessions.len();
            (sessions, format!(" {date_str} ({count}) "))
        }
    };

    let items: Vec<ListItem> = sessions_display
        .iter()
        .enumerate()
        .map(|(i, sd)| {
            let is_selected = i == state.selected_session;
            let pane_idx = state
                .panes
                .iter()
                .position(|p| p.file_path.as_deref() == Some(sd.file_path.as_path()));

            let prefix = match (is_selected, pane_idx) {
                (true, Some(idx)) => format!("▶{}", idx + 1),
                (true, None) => "▶ ".to_string(),
                (false, Some(idx)) => format!(" {}", idx + 1),
                (false, None) => "  ".to_string(),
            };

            let proj_display = truncate_with_ellipsis(&sd.project_short, proj_max_len);

            let style = if sd.is_pinned {
                Style::default()
                    .fg(theme::WARNING)
                    .add_modifier(Modifier::BOLD)
            } else if is_selected {
                Style::default()
                    .fg(theme::TEXT_BRIGHT)
                    .add_modifier(Modifier::BOLD)
            } else if pane_idx.is_some() {
                Style::default().fg(theme::WARM)
            } else {
                Style::default().fg(theme::LABEL)
            };

            let time_style = if sd.is_recent {
                Style::default().fg(theme::TIME_RECENT)
            } else {
                Style::default().fg(theme::TIME)
            };

            let state_color = if sd.is_continued {
                theme::PRIMARY
            } else {
                theme::SUCCESS
            };

            let mut line1_spans = vec![Span::styled(prefix, style)];
            if sd.is_pinned {
                line1_spans.push(Span::styled(
                    "*",
                    Style::default()
                        .fg(theme::WARNING)
                        .add_modifier(Modifier::BOLD),
                ));
                line1_spans.push(Span::styled(
                    if sd.is_continued { "»" } else { "·" },
                    Style::default().fg(state_color),
                ));
            } else {
                line1_spans.push(Span::styled(
                    if sd.is_continued { " »" } else { " ·" },
                    Style::default().fg(state_color),
                ));
            }
            line1_spans.push(Span::styled(format!("{} ", sd.time_or_date), time_style));
            line1_spans.push(Span::styled(proj_display, style));
            let line1 = Line::from(line1_spans);

            let (summary_text, summary_color) = sd
                .summary
                .as_deref()
                .map_or(("—", theme::FAINT), |t| (t, theme::TITLE));
            let summary_display = truncate_with_ellipsis(summary_text, summary_max_len);
            let line2 = Line::from(vec![
                Span::raw("   "),
                Span::styled(summary_display, Style::default().fg(summary_color)),
            ]);

            let item = ListItem::new(vec![line1, line2]);
            if is_selected {
                item.style(Style::default().bg(theme::SELECTED_BG))
            } else {
                item
            }
        })
        .collect();

    let help_line = if is_active {
        let mode_label = match state.conv_list_mode {
            crate::ConvListMode::Day => "*",
            crate::ConvListMode::Pinned => "All",
            crate::ConvListMode::All => "Day",
            // Live mode is one-way (entered from Live tab, exited via Esc),
            // so no S-Tab cycle out of it — label is informational only.
            crate::ConvListMode::Live => "Esc",
        };
        if area.width >= 36 {
            let mut spans = vec![
                Span::styled(" ↑↓", Style::default().fg(theme::PRIMARY)),
                Span::styled(":sel ", Style::default().fg(theme::LABEL)),
                Span::styled("Sp", Style::default().fg(theme::PRIMARY)),
                Span::styled(":pin ", Style::default().fg(theme::LABEL)),
                Span::styled("S-Tab", Style::default().fg(theme::PRIMARY)),
                Span::styled(format!(":{mode_label}"), Style::default().fg(theme::LABEL)),
            ];
            if state.conv_list_mode == crate::ConvListMode::Day {
                spans.extend_from_slice(&[
                    Span::styled(" H/L", Style::default().fg(theme::PRIMARY)),
                    Span::styled(":day", Style::default().fg(theme::LABEL)),
                ]);
            }
            Line::from(spans)
        } else {
            Line::from(vec![
                Span::styled(" Sp", Style::default().fg(theme::PRIMARY)),
                Span::styled(":pin ", Style::default().fg(theme::LABEL)),
                Span::styled("S-Tab", Style::default().fg(theme::PRIMARY)),
                Span::styled(format!(":{mode_label}"), Style::default().fg(theme::LABEL)),
            ])
        }
    } else {
        Line::default()
    };
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(title, Style::default().fg(theme::PRIMARY)))
            .title_bottom(help_line),
    );

    let mut list_state =
        ratatui::widgets::ListState::default().with_selected(Some(state.selected_session));
    frame.render_stateful_widget(list, area, &mut list_state);
    state.layout.session_list_area = Some((inner, list_state.offset(), item_height));
}

/// Reposition the viewport so selected message `[start, end)` is visible, but
/// only while it's fully off screen — any partial visibility leaves `scroll`
/// put (decoupled cursor/viewport). Off screen, a message taller than the
/// viewport aligns the edge being read toward (top from above, bottom from
/// below); one that fits snaps to its near edge. Caller clamps to `max_scroll`.
fn reposition_scroll_for_selection(
    scroll: usize,
    visible_height: usize,
    selected_start: usize,
    selected_end: usize,
) -> usize {
    let in_view = selected_start < scroll + visible_height && selected_end > scroll;
    if in_view {
        return scroll;
    }
    let taller_than_view = selected_end.saturating_sub(selected_start) > visible_height;
    if selected_end <= scroll {
        if taller_than_view {
            selected_end.saturating_sub(visible_height)
        } else {
            selected_start
        }
    } else if selected_start >= scroll + visible_height {
        if taller_than_view {
            selected_start
        } else {
            selected_end.saturating_sub(visible_height)
        }
    } else {
        scroll
    }
}

fn scope_day_label(day_first: chrono::DateTime<chrono::Utc>) -> String {
    let day = day_first.with_timezone(&chrono::Local).date_naive();
    if day == chrono::Local::now().date_naive() {
        "today".to_string()
    } else {
        day.format("%m-%d").to_string()
    }
}

fn draw_conversation_pane(
    frame: &mut Frame,
    area: Rect,
    pane: &mut ConversationPane,
    is_active: bool,
    _toast_time: &Option<std::time::Instant>,
    _layout_too_narrow: bool,
    selecting: bool,
    sessions: &[SessionInfo],
    all_groups: &[crate::aggregator::DailyGroup],
    pins_ref: &crate::pins::Pins,
    project_labels: &std::collections::HashMap<String, String>,
    session_titles: &std::collections::HashMap<std::path::PathBuf, String>,
) -> Option<Rect> {
    use ratatui::widgets::Clear;

    frame.render_widget(Clear, area);

    let has_session = pane.file_path.is_some();
    let info_height: u16 = if has_session { 3 } else { 0 };
    let info_area = if has_session {
        Some(Rect {
            x: area.x + 1,
            y: area.y + 1,
            width: area.width.saturating_sub(2),
            height: info_height,
        })
    } else {
        None
    };
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1 + info_height,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2 + info_height),
    };

    let border_color = if is_active {
        theme::PRIMARY
    } else {
        theme::BORDER
    };

    let draw_block = |frame: &mut Frame, scroll_info: &str, pane: &ConversationPane| {
        let msg_count = pane.message_lines.len();
        let msg_indicator = if msg_count > 0 {
            format!(" msg {}/{} ", pane.selected_message + 1, msg_count)
        } else {
            String::new()
        };
        let session = pane.file_path.as_ref().and_then(|fp| {
            sessions.iter().find(|s| &s.file_path == fp).or_else(|| {
                all_groups
                    .iter()
                    .flat_map(|g| g.sessions.iter())
                    .find(|s| &s.file_path == fp)
            })
        });
        let title = if let Some(s) = session {
            let available = area.width.saturating_sub(2) as usize;
            let pin = if pins_ref.is_pinned(&s.file_path) {
                "*"
            } else {
                ""
            };
            let proj_owned = project_labels
                .get(&s.project_name)
                .cloned()
                .unwrap_or_else(|| shorten_project(&s.project_name).to_string());
            let proj = proj_owned.as_str();
            let branch = s
                .git_branch
                .as_ref()
                .map(|b| {
                    let name = b.split('/').next_back().unwrap_or(b);
                    format!("#{}", truncate_with_ellipsis(name, 10))
                })
                .unwrap_or_default();
            let model = s
                .model
                .as_ref()
                .map(|m| format!(" [{}]", crate::aggregator::normalize_model_name(m)))
                .unwrap_or_default();
            let start = s.day_first_timestamp.with_timezone(&chrono::Local);
            let end = s.day_last_timestamp.with_timezone(&chrono::Local);
            let time = format!(" {}–{}", start.format("%H:%M"), end.format("%H:%M"));

            let full = format!(" {pin}{proj}{branch}{time}{model} ");
            let mid = format!(" {pin}{proj}{branch}{model} ");
            let short = format!(" {pin}{proj}{branch} ");
            let minimal = format!(" {pin}{proj} ");

            if unicode_width::UnicodeWidthStr::width(full.as_str()) <= available {
                full
            } else if unicode_width::UnicodeWidthStr::width(mid.as_str()) <= available {
                mid
            } else if unicode_width::UnicodeWidthStr::width(short.as_str()) <= available {
                short
            } else {
                minimal
            }
        } else {
            String::new()
        };
        let bottom_text = if scroll_info.is_empty() {
            msg_indicator.trim().to_string()
        } else if msg_indicator.is_empty() {
            scroll_info.to_string()
        } else {
            format!("{} · {}", msg_indicator.trim(), scroll_info.trim())
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(title, Style::default().fg(theme::PRIMARY)))
            .title_bottom(Line::from(Span::styled(
                bottom_text,
                Style::default().fg(theme::LABEL),
            )))
            .border_style(Style::default().fg(if pane.search_mode {
                theme::WARM
            } else {
                border_color
            }));
        frame.render_widget(block, area);
    };

    if pane.messages.is_empty() {
        if pane.loading {
            let loading = Paragraph::new("Loading…").style(Style::default().fg(theme::PLACEHOLDER));
            frame.render_widget(loading, inner);
            draw_block(frame, "", pane);
        } else if pane.file_path.is_none() {
            let hint = Paragraph::new("Select a session (Enter)")
                .style(Style::default().fg(theme::PLACEHOLDER))
                .alignment(ratatui::layout::Alignment::Center);
            let centered_area = Rect {
                x: inner.x,
                y: inner.y + inner.height / 2,
                width: inner.width,
                height: 1,
            };
            frame.render_widget(hint, centered_area);
            draw_block(frame, "", pane);
        } else {
            let empty =
                Paragraph::new("No messages found").style(Style::default().fg(theme::PLACEHOLDER));
            frame.render_widget(empty, inner);
            draw_block(frame, "", pane);
        }
        return None;
    }

    if pane.last_width != Some(inner.width) {
        pane.rendered = None;
        pane.last_width = Some(inner.width);
    }

    // Matches live on the LOGICAL messages (not rendered lines), so they
    // can be recomputed before layout — required for peek below, which
    // must mutate `expanded` before the render signature is taken.
    if pane.search_mode {
        update_pane_search_matches(pane);
    }
    if pane.pending_search_scroll
        && !pane.messages.is_empty()
        && let Some(day) = pane.search_seed_day.take()
        && let Some(i) = first_match_from_day(&pane.messages, &pane.search_matches, day)
    {
        pane.search_current = i;
    }
    // Peek: auto-expand the row holding the current match (for tool runs
    // that's the group head, not the matched message itself); collapse the
    // previous peek unless the user expanded it themselves.
    if pane.pending_search_scroll
        && let Some(&(m, _)) = pane.search_matches.get(pane.search_current)
    {
        let key = compact_expand_key(&pane.messages, m);
        if pane.peek_expanded != Some(key)
            && let Some(prev) = pane.peek_expanded.take()
        {
            pane.expanded.remove(&prev);
        }
        if pane.compact && !pane.expanded.contains(&key) {
            pane.expanded.insert(key);
            pane.peek_expanded = Some(key);
        }
    }

    let focused_msg_idx = pane
        .message_lines
        .get(pane.selected_message)
        .map(|&(_, idx)| idx);

    // Re-render when any input to the line layout changes: compact mode, the
    // expanded set, and — only in full mode — the focused message (which draws a
    // highlight baked into the lines). Compact mode highlights the cursor at draw
    // time from `message_lines`, so focus changes there need no relayout. Width
    // changes already null `rendered`.
    let render_sig = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        pane.compact.hash(&mut h);
        if !pane.compact {
            focused_msg_idx.hash(&mut h);
        }
        let mut exp: Vec<usize> = pane.expanded.iter().copied().collect();
        exp.sort_unstable();
        exp.hash(&mut h);
        h.finish()
    };
    let needs_rerender = pane.rendered.is_none() || pane.rendered_sig != render_sig;

    if needs_rerender {
        let rendered = if pane.compact {
            render_compact_lines(&pane.messages, inner.width, &pane.expanded)
        } else {
            render_conversation_lines(&pane.messages, inner.width, focused_msg_idx)
        };
        pane.message_lines = rendered.1.clone();
        pane.rendered = Some((rendered.0, rendered.1, rendered.2));
        pane.rendered_sig = render_sig;

        if let Some(ref saved_ts) = pane.focused_timestamp.take()
            && let Some(msg_idx) = pane
                .messages
                .iter()
                .position(|m| m.timestamp.as_ref() == Some(saved_ts))
            && let Some(line_idx) = pane
                .message_lines
                .iter()
                .position(|&(_, idx)| idx == msg_idx)
        {
            pane.selected_message = line_idx;
        }
    }

    let cached = pane.rendered.as_ref()?;

    let search_bar_height = if pane.search_mode { 1 } else { 0 };
    let content_height = inner.height.saturating_sub(search_bar_height);
    let content_area = Rect {
        height: content_height,
        ..inner
    };
    let search_area = Rect {
        y: inner.y + content_height,
        height: search_bar_height,
        ..inner
    };

    let visible_height = content_area.height as usize;
    pane.last_visible_height = Some(visible_height);
    let total_lines = cached.0.len();
    let max_scroll = total_lines.saturating_sub(visible_height);
    let msg_count = pane.message_lines.len();

    if msg_count > 0 && (pane.selected_message == usize::MAX || pane.selected_message >= msg_count)
    {
        pane.selected_message = msg_count - 1;
    }

    // Deferred match jump: center the line holding the current occurrence
    // (only the draw fn knows line layout + viewport height). Waits until
    // matches exist so an async-loading pane keeps the flag armed. Runs
    // BEFORE the selection range is derived so `reposition` sees the match
    // row, not a stale cursor that would drag the viewport back.
    if pane.pending_search_scroll
        && let Some(&(m, k)) = pane.search_matches.get(pane.search_current)
    {
        pane.pending_search_scroll = false;
        if let Some(row) = pane.message_lines.iter().rposition(|&(_, idx)| idx <= m) {
            pane.selected_message = row;
        }
        let query_lower_jump = pane.search_input.text.to_lowercase();
        if let Some((line, _)) = resolve_match_line(
            &cached.0,
            &pane.message_lines,
            total_lines,
            m,
            k,
            &query_lower_jump,
        ) {
            pane.scroll = line.saturating_sub(visible_height / 2).min(max_scroll);
        }
    }

    let selected_msg_idx = pane.selected_message;
    let selected_start = pane
        .message_lines
        .get(selected_msg_idx)
        .map_or(0, |&(line, _)| line);
    let selected_end = pane
        .message_lines
        .get(selected_msg_idx + 1)
        .map_or(total_lines, |&(line, _)| line);

    if pane.scroll == usize::MAX {
        if let Some(&(last_pos, _)) = cached.1.last() {
            pane.scroll = last_pos.min(max_scroll);
        } else {
            pane.scroll = max_scroll;
        }
    } else if pane.scroll > max_scroll {
        pane.scroll = max_scroll;
    }

    if !selecting {
        pane.scroll = reposition_scroll_for_selection(
            pane.scroll,
            visible_height,
            selected_start,
            selected_end,
        );
    }

    pane.scroll = pane.scroll.min(max_scroll);
    let scroll = pane.scroll;

    // Highlights derive from matches, so they die with the search (Esc
    // clears matches) instead of lingering while the bar is closed.
    let query_lower = if pane.search_matches.is_empty() {
        String::new()
    } else {
        pane.search_input.text.to_lowercase()
    };
    // Rendered position of the current occurrence: (line, occ-in-line).
    let current_hl: Option<(usize, usize)> = if query_lower.is_empty() {
        None
    } else {
        pane.search_matches
            .get(pane.search_current)
            .and_then(|&(m, k)| {
                resolve_match_line(
                    &cached.0,
                    &pane.message_lines,
                    total_lines,
                    m,
                    k,
                    &query_lower,
                )
            })
            .and_then(|(line, occ)| occ.map(|o| (line, o)))
    };

    let visible_lines: Vec<Line> = cached
        .0
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_height)
        .map(|(line_idx, line)| {
            let is_selected = line_idx >= selected_start && line_idx < selected_end;

            let mut spans: Vec<Span> = Vec::with_capacity(line.spans.len() + 1);
            if is_selected && line_idx == selected_start {
                spans.push(Span::styled("▶ ", Style::default().fg(theme::PRIMARY)));
            } else {
                spans.push(Span::styled("  ", Style::default()));
            }
            let ranges = if query_lower.is_empty() {
                Vec::new()
            } else {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                query_char_ranges(&text, &query_lower)
            };
            if ranges.is_empty() {
                spans.extend(line.spans.iter().cloned());
            } else {
                let strong = current_hl.and_then(|(l, o)| (l == line_idx).then_some(o));
                spans.extend(apply_bg_ranges(&line.spans, &ranges, strong));
            }
            Line::from(spans)
        })
        .collect();
    let paragraph = Paragraph::new(visible_lines);
    frame.render_widget(paragraph, content_area);

    if search_bar_height > 0 {
        let counter = if pane.search_input.text.is_empty() {
            String::new()
        } else if pane.search_matches.is_empty() {
            "  0/0".to_string()
        } else {
            format!(
                "  {}/{}",
                pane.search_current + 1,
                pane.search_matches.len()
            )
        };
        // Reverse-video query = "selected" (VS Code find widget): the next
        // typed char replaces the restored text wholesale.
        let query_style = if pane.search_select_all {
            Style::default().fg(theme::TEXT_BRIGHT).bg(theme::WARM)
        } else {
            Style::default().fg(theme::WARM)
        };
        let mut spans = pane.search_input.render_spans(
            "/",
            query_style,
            Style::default().fg(theme::TEXT_BRIGHT).bg(theme::PRIMARY),
        );
        spans.push(Span::styled(counter, Style::default().fg(theme::PRIMARY)));
        spans.push(Span::styled(
            "  Enter: next  ⇧Enter: prev  Esc: close",
            Style::default().fg(theme::LABEL),
        ));
        frame.render_widget(Paragraph::new(Line::from(spans)), search_area);
    }

    let can_scroll_up = scroll > 0;
    let can_scroll_down = scroll < max_scroll;
    let scroll_indicator = if can_scroll_up && can_scroll_down {
        "▲▼ "
    } else if can_scroll_up {
        "▲ "
    } else if can_scroll_down {
        "▼ "
    } else {
        ""
    };
    // Named, because the bar beside it is also `n/m`: a message index next to
    // a line offset reads as two of the same thing.
    let scroll_info = format!(
        " {}line {}/{} ",
        scroll_indicator,
        scroll + 1,
        max_scroll.max(1) + 1
    );
    draw_block(frame, &scroll_info, pane);

    let scrollbar_area = Rect {
        y: area.y + info_height,
        height: area.height.saturating_sub(info_height),
        ..area
    };
    draw_scrollbar(frame, scrollbar_area, scroll, total_lines, visible_height);

    if let Some(info_rect) = info_area
        && let Some(session) = pane.file_path.as_ref().and_then(|fp| {
            sessions.iter().find(|s| &s.file_path == fp).or_else(|| {
                all_groups
                    .iter()
                    .flat_map(|g| g.sessions.iter())
                    .find(|s| &s.file_path == fp)
            })
        })
    {
        let calculator = crate::aggregator::CostCalculator::global();
        // Use day_first, not session_first — for continued sessions the
        // session_first can predate the displayed day by weeks, which would
        // turn a single-day range like 00:00–09:47 into a multi-hundred-hour
        // duration that misrepresents activity for the day.
        let duration_mins =
            (session.day_last_timestamp - session.day_first_timestamp).num_minutes();
        let dur = if duration_mins >= 60 {
            format!("{}h{}m", duration_mins / 60, duration_mins % 60)
        } else {
            format!("{duration_mins}m")
        };
        let subagents = crate::aggregator::day_subagent_spend(session, all_groups);
        let work_tokens = session.work_tokens() + subagents.work_tokens;
        let cost: f64 = session.cost(calculator) + subagents.cost;
        let unpriced = session.has_unpriced_model(calculator) || subagents.unpriced;

        let summary = resolved_title(session_titles, session).unwrap_or("—");
        // Reserve 4 cells at the right edge for the [i] button (3) +
        // 1 spacer; keep summary inside the remaining width.
        let info_btn_width: usize = 3;
        let summary_max = info_rect
            .width
            .saturating_sub(2 + info_btn_width as u16 + 1) as usize;
        let summary_display = truncate_with_ellipsis(summary, summary_max);

        let summary_w = unicode_width::UnicodeWidthStr::width(summary_display.as_str()) + 1;
        let pad = (info_rect.width as usize).saturating_sub(summary_w + info_btn_width);
        let line1 = Line::from(vec![
            Span::styled(
                format!(" {summary_display}"),
                Style::default().fg(theme::TEXT_BRIGHT),
            ),
            Span::raw(" ".repeat(pad)),
            Span::styled("[i]", Style::default().fg(theme::PRIMARY)),
        ]);

        // Cowork audit.jsonl files all share the literal stem `audit`, so
        // prefer the canonical `cliSessionId` from sibling metadata when
        // available. Falls through to file_stem for regular Claude Code
        // JSONL paths (no behaviour change there).
        let short_id: String = crate::infrastructure::cowork_session_id(&session.file_path)
            .or_else(|| {
                session
                    .file_path
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .map(std::string::ToString::to_string)
            })
            .unwrap_or_else(|| "-".to_string())
            .chars()
            .take(8)
            .collect();

        // Duration, tokens and cost are this day's slice; naming the day (and
        // the lifetime total when the session spans several) keeps a
        // continued session from reading as if it cost only today's share.
        let day_label = scope_day_label(session.day_first_timestamp);
        let cumulative =
            crate::aggregator::compute_session_cumulative(&session.file_path, all_groups);
        let (all_days_cost, all_days_unpriced) = cumulative.cost_with_subagents();
        let all_days = (cumulative.days > 1).then(|| {
            format!(
                "  Σ{}d {}",
                cumulative.days,
                format_cost_marked(all_days_cost, all_days_unpriced, 0)
            )
        });
        let mut line2_spans: Vec<Span> = vec![
            Span::styled(format!(" {day_label}"), Style::default().fg(theme::ACCENT)),
            Span::styled(format!(" {dur}"), Style::default().fg(theme::TIME)),
            Span::styled(
                format!("  {}", crate::format_number(work_tokens)),
                Style::default().fg(theme::TOKENS),
            ),
            Span::styled(
                format!("  {}", format_cost_marked(cost, unpriced, 0)),
                cost_style_marked(cost, unpriced),
            ),
        ];
        if let Some(all_days) = &all_days {
            // A lower-bound total reads as a warning, as every marked cost does.
            let color = if all_days_unpriced {
                theme::WARNING
            } else {
                theme::META
            };
            line2_spans.push(Span::styled(all_days.clone(), Style::default().fg(color)));
        }
        line2_spans.push(Span::styled(
            format!("  {short_id}"),
            Style::default().fg(theme::SESSION_ID),
        ));
        if !session.day_tool_usage.is_empty() {
            let mut tools: Vec<_> = session.day_tool_usage.iter().collect();
            // Tiebreaker: alphabetical by name (HashMap iteration order
            // is randomized, so tied counts would shuffle per frame).
            tools.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
            // Width budget must measure the SAME string the row renders —
            // the unpriced mark is one column wider than the plain form, and
            // an underestimate lets the tool list overrun the pane.
            let prefix_width = day_label.chars().count()
                + 1
                + all_days.as_deref().map_or(0, |s| s.chars().count())
                + dur.len()
                + crate::format_number(work_tokens).len()
                + format_cost_marked(cost, unpriced, 0).len()
                + short_id.len()
                + 10;
            let max_width = info_rect.width.saturating_sub(2) as usize;
            let mut used = prefix_width;
            line2_spans.push(Span::raw("  "));
            for (name, count) in tools.iter().take(6) {
                let part = format!("{name}({count}) ");
                if used + part.len() > max_width {
                    break;
                }
                used += part.len();
                line2_spans.push(Span::styled(part, Style::default().fg(theme::LABEL)));
            }
        }
        let line2 = Line::from(line2_spans);

        let sep_color = if is_active {
            theme::PRIMARY
        } else {
            theme::BORDER
        };
        let separator = Line::from(Span::styled(
            "─".repeat(info_rect.width as usize),
            Style::default().fg(sep_color),
        ));

        let info_paragraph = Paragraph::new(vec![line1, line2, separator]);
        frame.render_widget(info_paragraph, info_rect);
    }

    Some(content_area)
}

fn render_conversation_lines(
    messages: &[ConversationMessage],
    width: u16,
    focused_msg_idx: Option<usize>,
) -> (Vec<Line<'static>>, Vec<(usize, usize)>, Vec<bool>) {
    use crate::text::wrap_text_with_continuation;

    const MAX_TOTAL_LINES: usize = 10000;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut wrap_flags: Vec<bool> = Vec::new();
    let mut message_positions: Vec<(usize, usize)> = Vec::new();
    let content_width = width.saturating_sub(3) as usize;
    let line_max_width = width.saturating_sub(3) as usize;
    let mut i = 0;

    macro_rules! push_line {
        ($line:expr) => {{
            lines.push($line);
            wrap_flags.push(false);
        }};
    }

    while i < messages.len() {
        // Anchor the rendered window to the end of the conversation: when the
        // buffer overflows the cap, drop earliest messages rather than stopping
        // at the front. Users typically need to see the latest exchange.
        while lines.len() > MAX_TOTAL_LINES && message_positions.len() > 1 {
            let next_start = message_positions[1].0;
            lines.drain(..next_start);
            wrap_flags.drain(..next_start);
            message_positions.remove(0);
            for pos in &mut message_positions {
                pos.0 = pos.0.saturating_sub(next_start);
            }
        }
        let msg = &messages[i];

        if is_tool_only_message(msg) {
            let group_start = i;
            let mut tool_uses: Vec<(&str, &str)> = Vec::new();
            let mut tool_results: Vec<(&str, bool)> = Vec::new();

            while i < messages.len() && is_tool_only_message(&messages[i]) {
                for block in &messages[i].blocks {
                    match block {
                        ConversationBlock::ToolUse {
                            name,
                            input_summary,
                        } => {
                            tool_uses.push((name.as_str(), input_summary.as_str()));
                        }
                        ConversationBlock::ToolResult { content, is_error } => {
                            tool_results.push((content.as_str(), *is_error));
                        }
                        _ => {}
                    }
                }
                i += 1;
            }

            message_positions.push((lines.len(), group_start));

            let is_focused = focused_msg_idx == Some(group_start);
            if is_focused {
                push_line!(Line::from(vec![
                    Span::styled("🔧 ", Style::default()),
                    Span::styled(
                        format!("[Tools: {} calls] ", tool_uses.len()),
                        Style::default().fg(theme::PRIMARY).bold(),
                    ),
                    Span::styled("▼", Style::default().fg(theme::META)),
                ]));
                push_line!(Line::from(""));

                for (idx, (name, summary)) in tool_uses.iter().enumerate() {
                    let result_status = tool_results.get(idx).map_or_else(
                        || Span::raw(""),
                        |(_, is_err)| {
                            if *is_err {
                                Span::styled(" ✗", Style::default().fg(theme::ERROR))
                            } else {
                                Span::styled(" ✓", Style::default().fg(theme::SUCCESS))
                            }
                        },
                    );

                    let display = if summary.is_empty() {
                        format!("  {} {}", name, "")
                    } else {
                        format!("  {name} {}", truncate_with_ellipsis(summary, 50))
                    };
                    push_line!(Line::from(truncate_spans(
                        vec![
                            Span::styled(display, Style::default().fg(theme::PRIMARY)),
                            result_status,
                        ],
                        line_max_width
                    )));
                }
                push_line!(Line::from(""));
            } else {
                let mut tool_order: Vec<(&str, usize)> = Vec::new();
                for (name, _) in &tool_uses {
                    if let Some(entry) = tool_order.iter_mut().find(|(n, _)| n == name) {
                        entry.1 += 1;
                    } else {
                        tool_order.push((name, 1));
                    }
                }
                let summary: Vec<String> = tool_order
                    .iter()
                    .map(|(name, count)| {
                        if *count > 1 {
                            format!("{name}×{count}")
                        } else {
                            name.to_string()
                        }
                    })
                    .collect();

                let has_error = tool_results.iter().any(|(_, is_err)| *is_err);
                let status_icon = if has_error { "⚠" } else { "✓" };
                let status_color = if has_error {
                    theme::WARNING
                } else {
                    theme::SUCCESS
                };

                push_line!(Line::from(truncate_spans(
                    vec![
                        Span::styled("🔧 ", Style::default()),
                        Span::styled(
                            format!("[{}] ", summary.join(", ")),
                            Style::default().fg(theme::PRIMARY),
                        ),
                        Span::styled(status_icon, Style::default().fg(status_color)),
                        Span::styled(" ▶", Style::default().fg(theme::META)),
                    ],
                    line_max_width
                )));
            }

            push_line!(Line::from(Span::styled(
                "─".repeat(line_max_width),
                Style::default().fg(theme::SEPARATOR),
            )));
            continue;
        }

        message_positions.push((lines.len(), i));
        let (role_style, role_icon) = match msg.role.as_str() {
            "user" => (Style::default().fg(theme::SUCCESS).bold(), "👤"),
            "recap" => (Style::default().fg(theme::WARM).bold(), "📝"),
            "meta" => (Style::default().fg(theme::META).bold(), "⚙"),
            _ => (Style::default().fg(theme::LABEL).bold(), "🤖"),
        };

        let mut header_spans = vec![
            Span::raw(role_icon.to_string()),
            Span::raw(" "),
            Span::styled(msg.role.to_uppercase(), role_style),
        ];

        if let Some(ref ts) = msg.timestamp {
            header_spans.push(Span::styled(
                format!("  {ts}"),
                Style::default().fg(theme::TIME),
            ));
        }

        if let Some(ref model) = msg.model {
            header_spans.push(Span::styled(
                format!("  [{}]", crate::aggregator::normalize_model_name(model)),
                Style::default().fg(model_color(model)),
            ));
        }

        // Per-turn response latency = gap from the entry that prompted it: a
        // typed prompt, an injected note or a compaction recap. Goes BEFORE tokens so the eye picks up "how long did
        // this turn take" before parsing through cache numbers. Only
        // meaningful for assistant turns; user turns include the human's
        // thinking time which doesn't belong on the row.
        if msg.role == "assistant"
            && let Some(this_ts) = msg.timestamp_utc
        {
            let prev_user_ts = messages
                .iter()
                .take(i)
                .rev()
                .find(|m| matches!(m.role.as_str(), "user" | "meta" | "recap"))
                .and_then(|m| m.timestamp_utc);
            if let Some(prev_ts) = prev_user_ts {
                let gap = (this_ts - prev_ts).num_seconds();
                if gap > 0 {
                    let label = if gap < 60 {
                        format!("{gap}s")
                    } else if gap < 3600 {
                        format!("{}m{:02}s", gap / 60, gap % 60)
                    } else {
                        format!("{}h{}m", gap / 3600, (gap % 3600) / 60)
                    };
                    header_spans.push(Span::styled(
                        format!("  Δ{label}"),
                        Style::default().fg(theme::TIME),
                    ));
                }
            }
        }

        // Per-turn cost via CostCalculator (1h/5m cache split correct).
        // Skipped when usage / model absent or cost is zero. Position:
        // `seconds → $ → tokens` so cost sits next to its latency.
        if let (Some(usage), Some(model)) = (msg.usage.as_ref(), msg.model.as_deref())
            && let Some(cost) =
                crate::aggregator::CostCalculator::global().calculate_cost(usage, Some(model))
            && cost > 0.0
        {
            // `calculate_cost` returned a figure, so the model is priced.
            header_spans.push(Span::styled(
                format!("  {}", format_cost(cost, 2)),
                cost_style_marked(cost, false),
            ));
        }

        if let Some((input, output)) = msg.tokens
            && (input > 0 || output > 0)
        {
            // Compact (in:1 out:822) reads as "you sent 1 token" which is a
            // misleading impression of Claude Code's prompt-cache-heavy
            // traffic. When there's pane width to spare, append the cache
            // creation / read split so the reader can attribute the cost.
            let mut text = format!(
                "  in:{} out:{}",
                crate::format_number(input),
                crate::format_number(output)
            );
            if line_max_width >= 90
                && let Some(usage) = msg.usage.as_ref()
            {
                if usage.cache_creation_tokens > 0 {
                    text.push_str(&format!(
                        " cw:{}",
                        crate::format_number(usage.cache_creation_tokens)
                    ));
                }
                if usage.cache_read_tokens > 0 {
                    text.push_str(&format!(
                        " cr:{}",
                        crate::format_number(usage.cache_read_tokens)
                    ));
                }
            }
            header_spans.push(Span::styled(text, Style::default().fg(theme::PRIMARY)));
        }

        push_line!(Line::from(truncate_spans(header_spans, line_max_width)));
        push_line!(Line::from(""));

        let mut line_count: usize = 0;
        let max_lines: usize = 100;

        for block in &msg.blocks {
            if line_count >= max_lines {
                push_line!(Line::from(Span::styled(
                    "  ... (truncated)".to_string(),
                    Style::default().fg(theme::PLACEHOLDER),
                )));
                break;
            }

            match block {
                ConversationBlock::Text(text) => {
                    let (hl_lines, hl_flags) = render_text_with_highlighting(text, content_width);
                    for (hl_line, flag) in hl_lines.into_iter().zip(hl_flags) {
                        if line_count >= max_lines {
                            break;
                        }
                        lines.push(hl_line);
                        wrap_flags.push(flag);
                        line_count += 1;
                    }
                }
                ConversationBlock::Thinking(thinking) => {
                    let char_count = thinking.chars().count();
                    let header = format!("💭 Thinking ({char_count} chars)");
                    push_line!(Line::from(Span::styled(
                        header,
                        Style::default()
                            .fg(theme::THINKING)
                            .add_modifier(Modifier::ITALIC),
                    )));
                    line_count += 1;

                    let max_thinking_lines = 8;
                    let truncated: String = thinking.chars().take(500).collect();
                    let display = if char_count > 500 {
                        format!("{truncated}...")
                    } else {
                        truncated
                    };

                    let (wrapped, wflags) = wrap_text_with_continuation(&display, content_width);
                    for (wrapped_line, flag) in
                        wrapped.into_iter().zip(wflags).take(max_thinking_lines)
                    {
                        if line_count >= max_lines {
                            break;
                        }
                        lines.push(Line::from(Span::styled(
                            format!("  {wrapped_line}"),
                            Style::default().fg(theme::LABEL),
                        )));
                        wrap_flags.push(flag);
                        line_count += 1;
                    }
                }
                ConversationBlock::ToolUse {
                    name,
                    input_summary,
                } => {
                    let tool_line = if input_summary.is_empty() {
                        format!("  [Tool] {name}")
                    } else {
                        format!("  [Tool] {name}: {input_summary}")
                    };
                    let (wrapped, wflags) =
                        wrap_text_with_continuation(&tool_line, content_width + 2);
                    for (wrapped_line, flag) in wrapped.into_iter().zip(wflags).take(2) {
                        lines.push(Line::from(Span::styled(
                            wrapped_line,
                            Style::default().fg(theme::PRIMARY),
                        )));
                        wrap_flags.push(flag);
                        line_count += 1;
                    }
                }
                ConversationBlock::ToolResult { content, is_error } => {
                    let result_style = if *is_error {
                        Style::default().fg(theme::ERROR)
                    } else {
                        Style::default().fg(theme::SUCCESS)
                    };
                    let prefix = if *is_error { "  [Error]" } else { "  [Result]" };
                    push_line!(Line::from(Span::styled(prefix.to_string(), result_style)));
                    line_count += 1;

                    let (result_lines, result_flags) =
                        render_tool_result_with_highlighting(content, content_width);
                    for (rl, flag) in result_lines.into_iter().zip(result_flags) {
                        if line_count >= max_lines {
                            break;
                        }
                        lines.push(rl);
                        wrap_flags.push(flag);
                        line_count += 1;
                    }
                }
            }
        }

        push_line!(Line::from(Span::styled(
            "─".repeat(width.saturating_sub(2) as usize),
            Style::default().fg(theme::SEPARATOR),
        )));
        i += 1;
    }

    // Final drain pass: trim the front after the last message has been pushed.
    while lines.len() > MAX_TOTAL_LINES && message_positions.len() > 1 {
        let next_start = message_positions[1].0;
        lines.drain(..next_start);
        wrap_flags.drain(..next_start);
        message_positions.remove(0);
        for pos in &mut message_positions {
            pos.0 = pos.0.saturating_sub(next_start);
        }
    }

    // If messages above the window were dropped, prepend a placeholder so
    // the user can see how many preceding messages are not shown.
    if let Some(&(_, first_kept_msg)) = message_positions.first()
        && first_kept_msg > 0
    {
        lines.insert(
            0,
            Line::from(Span::styled(
                format!("... ({first_kept_msg} earlier messages)"),
                Style::default().fg(theme::PLACEHOLDER),
            )),
        );
        wrap_flags.insert(0, false);
        for pos in &mut message_positions {
            pos.0 += 1;
        }
    }

    (lines, message_positions, wrap_flags)
}

/// One-line gist of a message for the compact view: first non-empty text,
/// else the tool calls, else thinking / tool-result markers. Whitespace is
/// collapsed so the result is always a single line.
fn compact_message_summary(msg: &ConversationMessage) -> String {
    for b in &msg.blocks {
        if let ConversationBlock::Text(t) = b {
            let one = t.split_whitespace().collect::<Vec<_>>().join(" ");
            if !one.is_empty() {
                return one;
            }
        }
    }
    let tools: Vec<String> = msg
        .blocks
        .iter()
        .filter_map(|b| match b {
            ConversationBlock::ToolUse {
                name,
                input_summary,
            } if input_summary.is_empty() => Some(name.clone()),
            ConversationBlock::ToolUse {
                name,
                input_summary,
            } => Some(format!("{name} {input_summary}")),
            _ => None,
        })
        .collect();
    if !tools.is_empty() {
        return format!("⚙ {}", tools.join(" · "));
    }
    if matches!(msg.blocks.first(), Some(ConversationBlock::Thinking(_))) {
        return "💭 thinking".to_string();
    }
    for b in &msg.blocks {
        if let ConversationBlock::ToolResult { content, .. } = b {
            let one = content.split_whitespace().collect::<Vec<_>>().join(" ");
            return format!("↳ {one}");
        }
    }
    String::new()
}

/// Compact conversation render: one summary line per message, the messages in
/// `expanded` shown in full inline (accordion). Same return shape as
/// `render_conversation_lines` so the pane's scroll / selection / search
/// machinery is mode-agnostic. `focused` marks the cursor row.
fn render_compact_lines(
    messages: &[ConversationMessage],
    width: u16,
    expanded: &std::collections::HashSet<usize>,
) -> (Vec<Line<'static>>, Vec<(usize, usize)>, Vec<bool>) {
    use crate::text::wrap_text_with_continuation;
    let inner = width.saturating_sub(1) as usize;
    // Minimal hang-indent for an expanded message's body: enough to set it off
    // from the header row without burning width aligning under the content
    // column (the full alignment wastes ~10 cols, cramping long markdown/code).
    const BODY_INDENT: &str = "  ";
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut positions: Vec<(usize, usize)> = Vec::new();
    let mut wrap_flags: Vec<bool> = Vec::new();

    // Header prefix (caret + time + role) shared by both branches.
    let header_prefix = |is_open: bool, msg: &ConversationMessage| {
        let (role_label, role_style) = if msg.role == "user" {
            ("You", Style::default().fg(theme::SUCCESS).bold())
        } else if msg.role == "recap" {
            // Written by Claude Code when it compacted the context: a summary
            // of everything above, not something the user said.
            ("Recap", Style::default().fg(theme::WARM).bold())
        } else if msg.role == "assistant" {
            ("AI ", Style::default().fg(theme::LABEL).bold())
        } else {
            ("·  ", Style::default().fg(theme::META))
        };
        let time = msg
            .timestamp_utc
            .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
            .or_else(|| msg.timestamp.clone())
            .unwrap_or_default();
        let (caret, caret_style) = if is_open {
            ("▾", Style::default().fg(theme::PRIMARY))
        } else {
            ("▸", Style::default().fg(theme::META))
        };
        let lead = format!("{caret} {time} {role_label} ");
        let prefix_w = unicode_width::UnicodeWidthStr::width(lead.as_str());
        let spans = vec![
            Span::styled(format!("{caret} "), caret_style),
            Span::styled(format!("{time} "), Style::default().fg(theme::TIME)),
            Span::styled(format!("{role_label} "), role_style),
        ];
        (spans, prefix_w)
    };
    let status_span = |any_err: bool| {
        if any_err {
            Span::styled(" ✗", Style::default().fg(theme::ERROR))
        } else {
            Span::styled(" ✓", Style::default().fg(theme::SUCCESS))
        }
    };

    let mut i = 0;
    while i < messages.len() {
        let text_w = inner.saturating_sub(BODY_INDENT.len()).max(8);

        // ---- Tool run: group consecutive tool-only messages into one row ----
        // A long stretch of `⚙ Read · ⚙ Edit · ⚙ Bash …` collapses to a single
        // `⚙ Read×2 · Edit · Bash ✓` line; expanding shows each call's detail.
        if is_tool_only_message(&messages[i]) {
            // A group interleaves uses and results from SEPARATE messages
            // (a result can even lead the group when it answers the previous
            // mixed message's call), so details keep message order — a
            // use↔result pairing by index would misattribute them.
            enum ToolDetail {
                Use(String, String),
                Result(bool, String),
            }
            let group_start = i;
            let mut uses: Vec<(String, String)> = Vec::new();
            let mut details: Vec<ToolDetail> = Vec::new();
            let mut any_err = false;
            let mut n_results = 0usize;
            while i < messages.len() && is_tool_only_message(&messages[i]) {
                for b in &messages[i].blocks {
                    match b {
                        ConversationBlock::ToolUse {
                            name,
                            input_summary,
                        } => {
                            uses.push((name.clone(), input_summary.clone()));
                            details.push(ToolDetail::Use(name.clone(), input_summary.clone()));
                        }
                        ConversationBlock::ToolResult { content, is_error } => {
                            any_err |= *is_error;
                            n_results += 1;
                            details.push(ToolDetail::Result(*is_error, content.clone()));
                        }
                        _ => {}
                    }
                }
                i += 1;
            }
            positions.push((lines.len(), group_start));
            let is_open = expanded.contains(&group_start);
            let (mut row, prefix_w) = header_prefix(is_open, &messages[group_start]);
            let has_status = n_results > 0;

            if uses.len() <= 1 {
                // Single call: show the tool + its argument inline.
                let label = uses.first().map_or_else(
                    || "⚙ (tool)".to_string(),
                    |(n, a)| {
                        if a.is_empty() {
                            format!("⚙ {n}")
                        } else {
                            format!("⚙ {n} {a}")
                        }
                    },
                );
                let budget = inner
                    .saturating_sub(prefix_w + if has_status { 2 } else { 0 })
                    .max(8);
                row.push(Span::styled(
                    truncate_with_ellipsis(&label, budget),
                    Style::default().fg(theme::PRIMARY),
                ));
            } else {
                // Multiple calls: name×count counts, e.g. "Read×2 · Edit · Bash".
                let mut order: Vec<(String, usize)> = Vec::new();
                for (name, _) in &uses {
                    if let Some(e) = order.iter_mut().find(|(n, _)| n == name) {
                        e.1 += 1;
                    } else {
                        order.push((name.clone(), 1));
                    }
                }
                let counts = order
                    .iter()
                    .map(|(n, c)| {
                        if *c > 1 {
                            format!("{n}×{c}")
                        } else {
                            n.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" · ");
                let label = format!("⚙ {} tools · {counts}", uses.len());
                let budget = inner
                    .saturating_sub(prefix_w + if has_status { 2 } else { 0 })
                    .max(8);
                row.push(Span::styled(
                    truncate_with_ellipsis(&label, budget),
                    Style::default().fg(theme::PRIMARY),
                ));
            }
            if has_status {
                row.push(status_span(any_err));
            }
            lines.push(Line::from(row));
            wrap_flags.push(false);

            if is_open {
                // Result content renders too (same `↳` form as expanded
                // single messages) so text inside tool results is visible —
                // and search-highlightable — when peeked.
                for d in &details {
                    match d {
                        ToolDetail::Use(name, arg) => {
                            lines.push(Line::from(Span::styled(
                                format!(
                                    "{BODY_INDENT}⚙ {name}  {}",
                                    truncate_with_ellipsis(
                                        arg,
                                        text_w.saturating_sub(name.len() + 6).max(8)
                                    )
                                ),
                                Style::default().fg(theme::PRIMARY),
                            )));
                        }
                        ToolDetail::Result(err, content) => {
                            let one = content.split_whitespace().collect::<Vec<_>>().join(" ");
                            let short =
                                truncate_with_ellipsis(&one, text_w.saturating_sub(4).max(8));
                            let (icon, c) = if *err {
                                ("✗", theme::ERROR)
                            } else {
                                ("✓", theme::SUCCESS)
                            };
                            lines.push(Line::from(vec![
                                Span::styled(
                                    format!("{BODY_INDENT}↳ "),
                                    Style::default().fg(theme::META),
                                ),
                                Span::styled(short, Style::default().fg(theme::LABEL)),
                                Span::styled(format!(" {icon}"), Style::default().fg(c)),
                            ]));
                        }
                    }
                    wrap_flags.push(false);
                }
                lines.push(Line::from(""));
                wrap_flags.push(false);
            }
            continue;
        }

        // ---- Text / mixed message ----
        let msg = &messages[i];
        positions.push((lines.len(), i));
        let is_open = expanded.contains(&i);
        let (mut row, prefix_w) = header_prefix(is_open, msg);
        let summary = compact_message_summary(msg);
        let budget = inner.saturating_sub(prefix_w).max(8);
        row.push(Span::styled(
            truncate_with_ellipsis(&summary, budget),
            Style::default().fg(theme::SNIPPET),
        ));
        lines.push(Line::from(row));
        wrap_flags.push(false);

        if !is_open {
            i += 1;
            continue;
        }

        for block in &msg.blocks {
            match block {
                ConversationBlock::Text(t) | ConversationBlock::Thinking(t) => {
                    if t.trim().is_empty() {
                        continue;
                    }
                    let dim = matches!(block, ConversationBlock::Thinking(_));
                    let (wrapped, _) = wrap_text_with_continuation(t, text_w);
                    for (wi, wl) in wrapped.iter().enumerate() {
                        let style = if dim {
                            Style::default().fg(theme::THINKING)
                        } else {
                            Style::default()
                        };
                        lines.push(Line::from(Span::styled(
                            format!("{BODY_INDENT}{wl}"),
                            style,
                        )));
                        wrap_flags.push(wi > 0);
                    }
                }
                ConversationBlock::ToolUse {
                    name,
                    input_summary,
                } => {
                    let arg = truncate_with_ellipsis(
                        input_summary,
                        text_w.saturating_sub(name.len() + 6).max(8),
                    );
                    lines.push(Line::from(Span::styled(
                        format!("{BODY_INDENT}⚙ {name}  {arg}"),
                        Style::default().fg(theme::PRIMARY),
                    )));
                    wrap_flags.push(false);
                }
                ConversationBlock::ToolResult { content, is_error } => {
                    let one = content.split_whitespace().collect::<Vec<_>>().join(" ");
                    let short = truncate_with_ellipsis(&one, text_w.saturating_sub(4).max(8));
                    let (icon, c) = if *is_error {
                        ("✗", theme::ERROR)
                    } else {
                        ("✓", theme::SUCCESS)
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("{BODY_INDENT}↳ "), Style::default().fg(theme::META)),
                        Span::styled(short, Style::default().fg(theme::LABEL)),
                        Span::styled(format!(" {icon}"), Style::default().fg(c)),
                    ]));
                    wrap_flags.push(false);
                }
            }
        }
        lines.push(Line::from(""));
        wrap_flags.push(false);
        i += 1;
    }
    (lines, positions, wrap_flags)
}

fn draw_summary(frame: &mut Frame, area: Rect, state: &mut AppState) {
    use ratatui::widgets::{Clear, Wrap};

    frame.render_widget(Clear, area);

    let target_info = match &state.summary_type {
        Some(SummaryType::Session(session)) => {
            let time = session
                .day_first_timestamp
                .with_timezone(&Local)
                .format("%H:%M");
            format!("{} @ {}", session.project_name, time)
        }
        Some(SummaryType::Day(day)) => day.date.format("%Y-%m-%d").to_string(),
        None => String::new(),
    };

    // `t` (resume title) only applies to a session summary; a day summary has no
    // single .jsonl to title, so it's omitted from the day-summary footer.
    let is_session = matches!(state.summary_type, Some(SummaryType::Session(_)));
    let actions = if is_session {
        "q: close  ↑↓: scroll  r: regenerate  t: write title"
    } else {
        "q: close  ↑↓: scroll  r: regenerate"
    };
    let title = if state.generating_summary {
        format!(" Generating: {target_info} ")
    } else if target_info.is_empty() {
        " Summary ".to_string()
    } else {
        format!(" {target_info} ")
    };

    // Keys on the bottom border, where every other popup puts them; the title
    // carried them here and the bottom border sat empty.
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            title,
            Style::default().fg(theme::PRIMARY).bold(),
        ))
        .border_style(Style::default().fg(theme::PRIMARY));
    if !state.generating_summary {
        block = block.title_bottom(Span::styled(
            format!(" {actions} "),
            Style::default().fg(theme::LABEL),
        ));
    }

    let inner = block.inner(area);
    state.layout.summary_popup_area = Some(area);
    frame.render_widget(block, area);

    if state.generating_summary {
        let f = state.animation_frame;
        let slow = f / 2;

        let star_chars = ['·', '✢', '✳', '✶', '✻', '✽'];

        let messages = [
            "Analyzing conversation",
            "Summarizing insights",
            "Distilling patterns",
            "Synthesizing context",
            "Processing thoughts",
        ];
        let msg_idx = (slow / 30) % messages.len();
        let msg = messages[msg_idx];

        let make_starfield = |width: usize, offset: usize| -> Vec<Span> {
            (0..width)
                .map(|i| {
                    let seed = (i * 17 + offset) % 97;
                    let twinkle = (slow + seed) % 48;
                    let (ch, intensity) = if seed.is_multiple_of(7) {
                        let char_idx = (twinkle / 8) % star_chars.len();
                        match twinkle {
                            0..=8 => (star_chars[char_idx], 1.0),
                            9..=20 => (star_chars[(char_idx + 1) % star_chars.len()], 0.7),
                            21..=32 => ('·', 0.4),
                            _ => (' ', 0.0),
                        }
                    } else {
                        (' ', 0.0)
                    };
                    let color = theme::primary_with_intensity(intensity);
                    Span::styled(ch.to_string(), Style::default().fg(color))
                })
                .collect()
        };

        let spinner_idx = (slow / 4) % star_chars.len();
        let spinner: Vec<Span> = (0..6)
            .map(|i| {
                let frame_idx = (spinner_idx + 6 - i) % star_chars.len();
                let intensity = 1.0 - (i as f32 * 0.15);
                let color = theme::primary_with_intensity(intensity as f64);
                Span::styled(
                    format!(" {}", star_chars[frame_idx]),
                    Style::default().fg(color),
                )
            })
            .collect();

        let wave_msg: Vec<Span> = msg
            .chars()
            .enumerate()
            .map(|(i, c)| {
                let wave = ((slow as f32 * 0.12 + i as f32 * 0.25).sin() * 0.25 + 0.75) as f64;
                let color = theme::primary_with_intensity(wave);
                Span::styled(c.to_string(), Style::default().fg(color))
            })
            .collect();

        let dots_phase = (slow / 6) % 4;
        let dots_spans: Vec<Span> = (0..3)
            .map(|i| {
                let visible = i < dots_phase;
                let intensity = if visible { 0.6 } else { 0.15 };
                let color = theme::primary_with_intensity(intensity);
                Span::styled(".", Style::default().fg(color))
            })
            .collect();

        let width = inner.width as usize;
        let center_y = inner.height / 2;

        let lines: Vec<Line> = vec![
            Line::from(make_starfield(width, 0)),
            Line::from(make_starfield(width, 33)),
            Line::from(vec![]),
            Line::from(spinner).alignment(ratatui::layout::Alignment::Center),
            Line::from(vec![]),
            {
                let mut msg_line = wave_msg;
                msg_line.extend(dots_spans);
                Line::from(msg_line).alignment(ratatui::layout::Alignment::Center)
            },
            Line::from(vec![]),
            Line::from(make_starfield(width, 66)),
            Line::from(make_starfield(width, 99)),
        ];

        let total_lines = lines.len() as u16;
        let start_y = center_y.saturating_sub(total_lines / 2);

        let text_area = Rect {
            x: inner.x,
            y: inner.y.saturating_add(start_y),
            width: inner.width,
            height: inner.height.saturating_sub(start_y),
        };

        let loading = Paragraph::new(lines);
        frame.render_widget(loading, text_area);
        return;
    }

    let padded_inner = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(1),
        height: inner.height,
    };

    // Clamp first in its own borrow scope so the content is never cloned
    // per frame just to satisfy the later `set_summary_scroll` call.
    let lines = summary_lines(&state.summary_content);
    let max_scroll = {
        let paragraph = Paragraph::new(lines.clone()).wrap(Wrap { trim: false });
        paragraph
            .line_count(padded_inner.width)
            .saturating_sub(padded_inner.height as usize)
    };
    state.set_summary_scroll(state.summary_scroll().min(max_scroll));

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((state.summary_scroll() as u16, 0));
    frame.render_widget(paragraph, padded_inner);
}

/// The AI summary prompt asks for markdown, so the popup has to render it —
/// `## Goal` and `**none**` shown as source put the markup in front of the
/// reader. Handles the three shapes that prompt produces: `##` headings,
/// `**bold**` runs, and `-` bullets.
fn summary_lines(text: &str) -> Vec<Line<'static>> {
    text.lines().map(summary_line).collect()
}

fn summary_line(raw: &str) -> Line<'static> {
    let trimmed = raw.trim_start();
    let indent = &raw[..raw.len() - trimmed.len()];

    if let Some(rest) = trimmed
        .strip_prefix("## ")
        .or_else(|| trimmed.strip_prefix("# "))
    {
        return Line::from(Span::styled(
            format!("{indent}{}", rest.trim()),
            Style::default().fg(theme::PRIMARY).bold(),
        ));
    }
    // `-` and `*` both appear; a bullet glyph reads as a list without the
    // marker competing with the `**bold**` runs on the same line.
    let (prefix, body) = match trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        Some(rest) => (format!("{indent}• "), rest),
        None => (indent.to_string(), trimmed),
    };

    let mut spans = vec![Span::styled(prefix, Style::default().fg(theme::LABEL))];
    spans.extend(bold_runs(body));
    Line::from(spans)
}

/// Splits on `**` pairs. An unmatched `**` stays literal rather than turning
/// the rest of the line bold — a truncated generation must not restyle it all.
fn bold_runs(body: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find("**") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("**") else { break };
        if open > 0 {
            spans.push(Span::raw(rest[..open].to_string()));
        }
        spans.push(Span::styled(
            after[..close].to_string(),
            Style::default().fg(theme::TEXT_BRIGHT).bold(),
        ));
        rest = &after[close + 2..];
    }
    if !rest.is_empty() {
        spans.push(Span::raw(rest.to_string()));
    }
    spans
}

/// Bar length for the compact Breakdown columns. A non-zero share always gets
/// at least one block: rounding a small share down to zero leaves a blank
/// where every other row has a bar, which reads as a broken column rather than
/// as "small". Shared so the two column renderers agree.
fn breakdown_bar_len(pct: f64) -> usize {
    const MAX_BLOCKS: f64 = 3.0;
    let n = (pct / 100.0 * MAX_BLOCKS).round() as usize;
    if pct > 0.0 { n.max(1) } else { 0 }
}

/// Shared geometry for the major detail popups (Session / Dashboard / Insights)
/// so they present at one consistent size instead of three ad-hoc rectangles.
/// Centered in `area`, clamped to fit smaller terminals; the row count fits the
/// stats plus a recent-conversation preview without immediate scrolling.
pub(super) fn detail_popup_area(area: Rect) -> Rect {
    // Width clears the Insights `Sessions` row, whose rate figures plus the
    // subagent suffix are the longest line any of the three popups compose.
    // `insights_detail_metrics_fit_the_popup_width` pins it.
    let width = 104u16.min(area.width.saturating_sub(4));
    let height = 36u16.min(area.height.saturating_sub(4));
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

/// `+N / -M lines`, added and removed coloured like a diff. One home so the
/// Session / Insights / Project popups cannot drift in format.
fn churn_line(
    label: &'static str,
    churn: crate::domain::LineChurn,
    label_style: Style,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(label, label_style),
        Span::styled(
            format!("+{}", crate::format_number(u64::from(churn.added))),
            Style::default().fg(theme::SUCCESS),
        ),
        Span::styled(" / ", label_style),
        Span::styled(
            format!("-{}", crate::format_number(u64::from(churn.removed))),
            Style::default().fg(theme::WARNING),
        ),
        Span::styled(" lines", label_style),
    ])
}

/// The Tokens row's share run by subagents, which the row's own figures leave
/// out; empty when the session launched none.
fn subagent_tokens_span(
    subagents: crate::aggregator::SubagentSpend,
    label_style: Style,
) -> Span<'static> {
    if subagents.count == 0 {
        return Span::raw("");
    }
    Span::styled(
        format!(
            "  + subagents work:{}",
            crate::format_number(subagents.work_tokens)
        ),
        label_style,
    )
}

/// The popup's Cost row: the session plus the subagents it launched, split
/// out only when there are any.
fn cost_line(
    total: f64,
    total_unpriced: bool,
    own: f64,
    own_unpriced: bool,
    subagents: crate::aggregator::SubagentSpend,
    label_style: Style,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled("  Cost      ", label_style),
        Span::styled(
            format_cost_marked(total, total_unpriced, 2),
            cost_style_marked(total, total_unpriced),
        ),
    ];
    if subagents.count > 0 {
        spans.push(Span::styled(
            format!(
                "  (session {} + subagents {})",
                format_cost_marked(own, own_unpriced, 2),
                format_cost_marked(subagents.cost, subagents.unpriced, 2)
            ),
            label_style,
        ));
    }
    Line::from(spans)
}

fn draw_session_detail(
    frame: &mut Frame,
    area: Rect,
    session: &crate::aggregator::SessionInfo,
    footer: &str,
    is_pinned: bool,
    scroll: usize,
    project_labels: &std::collections::HashMap<String, String>,
    session_titles: &std::collections::HashMap<std::path::PathBuf, String>,
    cumulative: &SessionCumulative,
    day_subagents: crate::aggregator::SubagentSpend,
    // `(pid, status, started_at)` to append as a "Process" row. Set only
    // when the popup is opened from the Live tab; daily-tab opens pass None.
    live_extra: Option<&(u32, String, String)>,
    // Last few `(role, one-line text)` messages for the "Recent conversation"
    // section. `None` = still loading (shows "Loading…").
    recent: Option<&[(String, String)]>,
    // Verified `claude -r` cd target (`AppState::resume_dir`); `None` renders
    // an honest "unknown" line instead of a guessed path.
    resume_dir: Option<&std::path::Path>,
) -> Rect {
    use ratatui::widgets::Clear;

    // Shared size with the Dashboard / Insights detail popups (see
    // `detail_popup_area`) so the three present identically.
    let popup_area = detail_popup_area(area);
    let popup_width = popup_area.width;

    frame.render_widget(Clear, popup_area);

    // Use day_* bounds so Time / Duration reflect the selected day's slice
    // (other fields in this popup — tokens, cost, tools — are also day-only).
    let session_start = session.day_first_timestamp.with_timezone(&chrono::Local);
    let end = session.day_last_timestamp.with_timezone(&chrono::Local);
    let duration_mins = (session.day_last_timestamp - session.day_first_timestamp).num_minutes();
    let duration_str = if duration_mins >= 60 {
        format!("{}h{}m", duration_mins / 60, duration_mins % 60)
    } else {
        format!("{duration_mins}m")
    };

    let cache_write: u64 = session
        .day_tokens_by_model
        .values()
        .map(|t| t.cache_creation_tokens)
        .sum();
    let cache_read: u64 = session
        .day_tokens_by_model
        .values()
        .map(|t| t.cache_read_tokens)
        .sum();
    let work_tokens = session.work_tokens();
    let total_tokens = work_tokens + cache_write + cache_read;

    let calculator = crate::aggregator::CostCalculator::global();
    let own_cost: f64 = session.cost(calculator);
    let own_unpriced = session.has_unpriced_model(calculator);
    let cost = own_cost + day_subagents.cost;
    let unpriced = own_unpriced || day_subagents.unpriced;

    let model_name = session.model.as_ref().map_or_else(
        || "?".to_string(),
        |m| {
            let normalized = crate::aggregator::normalize_model_name(m);
            if normalized == "Other" {
                m.clone()
            } else {
                normalized
            }
        },
    );
    let model_clr = session
        .model
        .as_ref()
        .map_or(theme::LABEL, |m| model_color(m));

    let label_style = Style::default().fg(theme::LABEL);

    let mut lines: Vec<Line> = Vec::new();

    // Header: project#branch [Model]  tokens $cost
    let mut header = vec![Span::raw("  ")];
    if is_pinned {
        header.push(Span::styled("* ", Style::default().fg(theme::WARNING)));
    }
    let marker = if session.is_continued { "» " } else { "" };
    if !marker.is_empty() {
        header.push(Span::styled(marker, Style::default().fg(theme::PRIMARY)));
    }
    let project_label = project_labels
        .get(&session.project_name)
        .cloned()
        .unwrap_or_else(|| shorten_project(&session.project_name).to_string());
    header.push(Span::styled(
        project_label,
        Style::default().fg(theme::PROJECT).bold(),
    ));
    if let Some(ref branch) = session.git_branch {
        let short = branch.split('/').next_back().unwrap_or(branch);
        header.push(Span::styled(
            format!("#{short}"),
            Style::default().fg(theme::BRANCH),
        ));
    }
    header.push(Span::styled(
        format!("  [{model_name}]"),
        Style::default().fg(model_clr),
    ));
    // No token figure on the header: it would duplicate the labelled value
    // in the `[Today]` block below and read ambiguous whenever the session
    // spans more days than the displayed value.
    lines.push(Line::from(header));

    let summary_text = resolved_title(session_titles, session).unwrap_or("—");
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(summary_text, Style::default().fg(theme::TEXT_BRIGHT)),
    ]));

    // Recent conversation: last few user/assistant text messages, one line
    // each, oldest-first (newest sits next to the stats below). `None` = the
    // background load hasn't returned yet; an empty result skips the section.
    let show_recent = recent.is_none_or(|r| !r.is_empty());
    if show_recent {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Recent conversation",
            Style::default().fg(theme::PRIMARY).bold(),
        )));
        match recent {
            None => lines.push(Line::from(Span::styled("    Loading…", label_style))),
            Some(msgs) => {
                let avail = popup_width.saturating_sub(2).saturating_sub(6) as usize;
                for (role, text) in msgs {
                    let is_user = role == "user";
                    let glyph_style = if is_user {
                        Style::default().fg(theme::PRIMARY)
                    } else {
                        Style::default().fg(theme::LABEL)
                    };
                    let text_style = Style::default().fg(theme::SNIPPET);
                    let glyph = if is_user { "❯" } else { "⬡" };
                    lines.push(Line::from(vec![
                        Span::styled(format!("    {glyph} "), glyph_style),
                        Span::styled(truncate_with_ellipsis(text, avail), text_style),
                    ]));
                }
            }
        }
    }
    lines.push(Line::from(""));

    // Time
    let time_display = if session_start.date_naive() == end.date_naive() {
        format!(
            "{}–{}  {}",
            session_start.format("%Y-%m-%d %H:%M"),
            end.format("%H:%M"),
            duration_str,
        )
    } else {
        format!(
            "{}–{}  {}",
            session_start.format("%Y-%m-%d %H:%M"),
            end.format("%Y-%m-%d %H:%M"),
            duration_str,
        )
    };
    lines.push(Line::from(vec![
        Span::styled("  Time      ", label_style),
        Span::styled(time_display, Style::default().fg(theme::VALUE)),
    ]));

    // Multi-day span line: only render when the session is active on more
    // than one day, so single-day sessions stay compact.
    if cumulative.days > 1
        && let Some(earliest) = cumulative.earliest_start
    {
        let started = earliest.with_timezone(&chrono::Local).format("%Y-%m-%d");
        let total_h = cumulative.total_work_mins / 60;
        let total_m = cumulative.total_work_mins % 60;
        let total_dur = if total_h > 0 {
            format!("{total_h}h{total_m}m")
        } else {
            format!("{total_m}m")
        };
        lines.push(Line::from(vec![
            Span::styled("  Session   ", label_style),
            Span::styled(
                format!(
                    "started {started} · {days} days · {total_dur} total",
                    days = cumulative.days
                ),
                Style::default().fg(theme::VALUE),
            ),
        ]));
    }

    // Live-process metadata (pid / status / started-at) appears alongside
    // the time / location / resume block — it's runtime context, naturally
    // grouped with "where / what / how to re-attach" rather than buried
    // under the stats and Tools list.
    if let Some((pid, status, started)) = live_extra {
        lines.push(Line::from(vec![
            Span::styled("  Process   ", label_style),
            Span::styled(format!("pid {pid}"), Style::default().fg(theme::PRIMARY)),
            Span::styled(format!("  status: {status}"), label_style),
            if !started.is_empty() {
                Span::styled(format!("  started {started}"), label_style)
            } else {
                Span::raw("")
            },
        ]));
    }

    // Directory + ID + Resume command sit immediately under the session
    // header so the copy-pasteable resume command is reachable without
    // scrolling past the variable-length Tools list below.
    let dir_label = "  Directory ";
    let dir_value = session.project_name.as_str();
    let dir_inner_w = popup_width.saturating_sub(2) as usize;
    let dir_avail = dir_inner_w.saturating_sub(dir_label.chars().count());
    if dir_value.chars().count() <= dir_avail {
        lines.push(Line::from(vec![
            Span::styled(dir_label, label_style),
            Span::styled(dir_value, Style::default().fg(theme::VALUE)),
        ]));
    } else {
        let cont_indent: String = std::iter::repeat_n(' ', dir_label.chars().count()).collect();
        let mut chunks: Vec<String> = Vec::new();
        let mut current = String::new();
        let mut current_w = 0usize;
        for ch in dir_value.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_w + cw > dir_avail && !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
                current_w = 0;
            }
            current.push(ch);
            current_w += cw;
        }
        if !current.is_empty() {
            chunks.push(current);
        }
        for (i, chunk) in chunks.iter().enumerate() {
            let prefix = if i == 0 {
                dir_label.to_string()
            } else {
                cont_indent.clone()
            };
            lines.push(Line::from(vec![
                Span::styled(prefix, label_style),
                Span::styled(chunk.clone(), Style::default().fg(theme::VALUE)),
            ]));
        }
    }

    // Session ID + resume command. Cowork audit.jsonl files all share the
    // file stem `audit`, so prefer `cliSessionId` from sibling metadata —
    // and skip the `claude -r` resume command entirely since Cowork sessions
    // run in a sandbox VM that the local CLI cannot attach to.
    let is_cowork = crate::infrastructure::is_cowork_audit_path(&session.file_path);
    let session_id: String = crate::infrastructure::cowork_session_id(&session.file_path)
        .or_else(|| {
            session
                .file_path
                .file_stem()
                .and_then(|n| n.to_str())
                .map(std::string::ToString::to_string)
        })
        .unwrap_or_else(|| "-".to_string());
    lines.push(Line::from(vec![
        Span::styled("  ID        ", label_style),
        Span::styled(session_id.clone(), Style::default().fg(theme::VALUE)),
    ]));
    lines.push(Line::from(vec![Span::styled("  Resume    ", label_style)]));
    let acc_style = Style::default().fg(theme::ACCENT);
    if is_cowork {
        lines.push(Line::from(vec![Span::styled(
            "    (Cowork — re-open from Claude Desktop)",
            Style::default().fg(theme::LABEL),
        )]));
    } else if let Some(dir) = resume_dir {
        // The same verified dir the `y` copy uses — display and clipboard
        // can never disagree on the target.
        let resume_cmd = crate::shell::resume_command(&dir.to_string_lossy(), &session_id);
        let inner_w = popup_width.saturating_sub(2) as usize;
        let avail = inner_w.saturating_sub(4);
        if resume_cmd.chars().count() <= avail {
            lines.push(Line::from(vec![Span::styled(
                format!("    {resume_cmd}"),
                acc_style,
            )]));
        } else {
            let parts: Vec<&str> = resume_cmd.split(" && ").collect();
            for (i, part) in parts.iter().enumerate() {
                let suffix = if i + 1 < parts.len() { " && \\" } else { "" };
                let prefix = if i == 0 { "    " } else { "      " };
                lines.push(Line::from(vec![Span::styled(
                    format!("{prefix}{part}{suffix}"),
                    acc_style,
                )]));
            }
        }
    } else {
        // No witness verified the storage dir (or it was deleted) — say so
        // rather than print a cd that silently fails to find the session.
        lines.push(Line::from(vec![Span::styled(
            "    (resume dir unknown — use the `claude --resume` picker)",
            Style::default().fg(theme::LABEL),
        )]));
    }
    lines.push(Line::from(""));

    let multi_day = cumulative.days > 1;
    let section_style = Style::default()
        .fg(theme::PRIMARY)
        .add_modifier(Modifier::BOLD);

    // Multi-day: split per-day vs lifetime. Header carries the slice's
    // actual date (past-day Daily / Live overrides where latest activity
    // isn't today would lie if hardcoded "[Today]"). Single-day skips.
    if multi_day {
        let slice_date = session
            .day_first_timestamp
            .with_timezone(&Local)
            .date_naive();
        let today = Local::now().date_naive();
        let day_label = if slice_date == today {
            "  [Today]".to_string()
        } else {
            slice_date.format("  [%Y-%m-%d (%a)]").to_string()
        };
        lines.push(Line::from(Span::styled(day_label, section_style)));
    }
    lines.push(Line::from(vec![
        Span::styled("  Turns     ", label_style),
        Span::styled(
            turns_text(
                session.day_prompts,
                session.day_assistant_msgs,
                session.day_user_msgs,
            ),
            Style::default().fg(theme::PRIMARY),
        ),
    ]));
    lines.push(churn_line("  Edits     ", session.line_churn, label_style));
    // Surface `work` (= input + output) alongside `total` so the popup's
    // headline number lines up with the session row's token column. The
    // row shows work tokens because cache reads inflate the total by 100x+
    // on heavy cache hit; pairing the two lets the reader see both the
    // "conversation size" and the cache-inclusive grand total at a glance.
    lines.push(Line::from(vec![
        Span::styled("  Tokens    ", label_style),
        Span::styled(
            format!("work:{}", crate::format_number(work_tokens)),
            Style::default().fg(theme::PRIMARY),
        ),
        Span::styled(
            format!("  total:{}", crate::format_number(total_tokens)),
            label_style,
        ),
        Span::styled(
            format!(
                "  in:{} out:{} cw:{} cr:{}",
                crate::format_number(session.day_input_tokens),
                crate::format_number(session.day_output_tokens),
                crate::format_number(cache_write),
                crate::format_number(cache_read),
            ),
            label_style,
        ),
        subagent_tokens_span(day_subagents, label_style),
    ]));
    lines.push(cost_line(
        cost,
        unpriced,
        own_cost,
        own_unpriced,
        day_subagents,
        label_style,
    ));

    if multi_day {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("  [All days]", section_style)));
        lines.push(Line::from(vec![
            Span::styled("  Turns     ", label_style),
            Span::styled(
                turns_text(
                    cumulative.prompts,
                    cumulative.assistant_msgs,
                    cumulative.user_msgs,
                ),
                Style::default().fg(theme::PRIMARY),
            ),
        ]));
        lines.push(churn_line(
            "  Edits     ",
            cumulative.line_churn,
            label_style,
        ));
        let cum_work = cumulative.input_tokens + cumulative.output_tokens;
        let cum_total = cum_work + cumulative.cache_creation + cumulative.cache_read;
        lines.push(Line::from(vec![
            Span::styled("  Tokens    ", label_style),
            Span::styled(
                format!("work:{}", crate::format_number(cum_work)),
                Style::default().fg(theme::PRIMARY),
            ),
            Span::styled(
                format!("  total:{}", crate::format_number(cum_total)),
                label_style,
            ),
            Span::styled(
                format!(
                    "  in:{} out:{} cw:{} cr:{}",
                    crate::format_number(cumulative.input_tokens),
                    crate::format_number(cumulative.output_tokens),
                    crate::format_number(cumulative.cache_creation),
                    crate::format_number(cumulative.cache_read),
                ),
                label_style,
            ),
            subagent_tokens_span(cumulative.subagents, label_style),
        ]));
        let (total, total_unpriced) = cumulative.cost_with_subagents();
        lines.push(cost_line(
            total,
            total_unpriced,
            cumulative.cost,
            cumulative.unpriced,
            cumulative.subagents,
            label_style,
        ));
    }

    lines.push(Line::from(""));

    // Model breakdown
    if session.day_tokens_by_model.len() > 1 {
        lines.push(Line::from(Span::styled(
            "  Models",
            Style::default().fg(theme::PRIMARY).bold(),
        )));
        let mut models: Vec<_> = session.day_tokens_by_model.iter().collect();
        models.sort_by_key(|m| std::cmp::Reverse(m.1.work_tokens()));
        for (model, tokens) in &models {
            let normalized = crate::aggregator::normalize_model_name(model);
            let clr = model_color(model);
            // `None` here is the per-model unpriced signal — render "$?" so
            // the row can't read as a free model next to the marked Cost row.
            let model_cost = calculator.calculate_cost(tokens, Some(model));
            let unknown = model_cost.is_none();
            let model_cost = model_cost.unwrap_or(0.0);
            lines.push(Line::from(vec![
                Span::styled(format!("    {normalized:<16}"), Style::default().fg(clr)),
                Span::styled(
                    format!("{:>6}", crate::format_number(tokens.work_tokens())),
                    Style::default().fg(theme::PRIMARY),
                ),
                Span::styled(
                    format!("  {}", format_cost_marked(model_cost, unknown, 0)),
                    cost_style_marked(model_cost, unknown),
                ),
            ]));
        }
        lines.push(Line::from(""));
    }

    // Tools
    let mut tools: Vec<_> = session
        .day_tool_usage
        .iter()
        .filter(|(name, count)| !name.is_empty() && **count > 0)
        .collect();
    // Tiebreaker on name keeps tied-count tools in stable alphabetical order
    // across frames (HashMap iteration is randomized per instance).
    tools.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    if !tools.is_empty() {
        let tool_strs: Vec<String> = tools
            .iter()
            .take(8)
            .map(|(name, count)| format!("{name}({count})"))
            .collect();
        let inner_width = popup_width.saturating_sub(6) as usize;
        // Build content lines first so an empty / all-whitespace render leaves
        // no orphan "Tools" header behind. Skip leading whitespace-only flushes
        // (an oversize first tool string would otherwise push "    " alone).
        let mut content_lines: Vec<Line> = Vec::new();
        let mut current_line = String::from("    ");
        for (i, tool_str) in tool_strs.iter().enumerate() {
            let sep = if i > 0 { "  " } else { "" };
            if current_line.len() + sep.len() + tool_str.len() > inner_width {
                if !current_line.trim().is_empty() {
                    content_lines.push(Line::from(Span::styled(current_line.clone(), label_style)));
                }
                current_line = format!("    {tool_str}");
            } else {
                current_line = format!("{current_line}{sep}{tool_str}");
            }
        }
        if !current_line.trim().is_empty() {
            content_lines.push(Line::from(Span::styled(current_line, label_style)));
        }
        if !content_lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "  Tools",
                Style::default().fg(theme::PRIMARY).bold(),
            )));
            lines.extend(content_lines);
            lines.push(Line::from(""));
        }
    }

    let block = popup_block(" Session Detail ").title_bottom(Line::from(Span::styled(
        footer,
        Style::default().fg(theme::LABEL),
    )));

    // Clamp scroll to keep at least one line of body content visible. Inner height excludes
    // the top + bottom borders.
    let inner_height = popup_area.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(inner_height);
    let actual_scroll = scroll.min(max_scroll);
    let paragraph = Paragraph::new(lines)
        .scroll((actual_scroll as u16, 0))
        .block(block);
    frame.render_widget(paragraph, popup_area);
    popup_area
}

fn draw_detail_popup(frame: &mut Frame, area: Rect, state: &mut AppState) {
    // Live tab pushes a SessionInfo into `session_detail_override` so the
    // popup can render sessions that fall outside the active filter.
    let session = if let Some(ref s) = state.session_detail_override {
        s.clone()
    } else {
        let Some(group) = state.daily_groups.get(state.selected_day) else {
            return;
        };
        let sessions: Vec<_> = group.user_sessions().collect();
        let Some(session) = sessions.get(state.selected_session) else {
            return;
        };
        (*session).clone()
    };
    let pinned = state.pins.is_pinned(&session.file_path);
    // Aggregate across all days (using original groups so cumulative is
    // independent of the active period/project filter).
    let cumulative = compute_session_cumulative(&session.file_path, &state.original_daily_groups);
    let live_extra = state.session_detail_live_extra.clone();
    let popup_area = draw_session_detail(
        frame,
        area,
        &session,
        " Space: pin  y: copy resume  s: summary  t: title  C: pane  ↑↓: scroll  i/Esc: close ",
        pinned,
        state.session_detail_scroll,
        &state.project_labels,
        &state.session_titles,
        &cumulative,
        crate::aggregator::day_subagent_spend(&session, &state.original_daily_groups),
        live_extra.as_ref(),
        state.session_detail_recent.as_deref(),
        state.resume_dir(&session.file_path).as_deref(),
    );
    state.layout.active_popup_area = Some(popup_area);
}

// The canonical per-session lifetime aggregate lives in the aggregator (single
// source of truth shared with MCP); the TUI just indexes it.
use crate::aggregator::{SessionCumulative, compute_session_cumulative};

fn draw_breakdown_detail_popup(
    frame: &mut Frame,
    area: Rect,
    items: &[BreakdownItem],
    models_start_idx: usize,
    tools_start_idx: usize,
    state: &mut AppState,
) {
    use ratatui::widgets::Clear;

    let popup_width = 80.min(area.width.saturating_sub(4));
    let item_count = items.len() as u16;
    let popup_height = (item_count + 4).min(area.height.saturating_sub(4)).min(30);

    let popup_area = Rect {
        x: area.width.saturating_sub(popup_width) / 2,
        y: area.height.saturating_sub(popup_height) / 2,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_area);

    let (visible_height, max_scroll, scroll) =
        calc_scroll(popup_height, items.len(), state.daily_breakdown_scroll, 3);
    state.daily_breakdown_max_scroll = max_scroll;

    let mut lines: Vec<Line> = vec![];
    for (i, item) in items.iter().enumerate().skip(scroll).take(visible_height) {
        let (label, bar_color, name, info, pct) = match item {
            BreakdownItem::Project(name, tokens, pct) => {
                let label =
                    if i == 0 || (i > 0 && !matches!(&items[i - 1], BreakdownItem::Project(..))) {
                        "Projects  "
                    } else {
                        "          "
                    };
                (
                    label,
                    theme::WARM,
                    name.clone(),
                    crate::format_number(*tokens),
                    *pct,
                )
            }
            BreakdownItem::Model(name, tokens, pct) => {
                let label = if i == models_start_idx {
                    "Models    "
                } else {
                    "          "
                };
                let short = crate::aggregator::normalize_model_name(name);
                (
                    label,
                    theme::PRIMARY,
                    short,
                    crate::format_number(*tokens),
                    *pct,
                )
            }
            BreakdownItem::Tool(name, count, pct) => {
                let label = if i == tools_start_idx {
                    "Tools     "
                } else {
                    "          "
                };
                (label, theme::SUCCESS, name.clone(), count.to_string(), *pct)
            }
        };

        let bar_len = (pct / 100.0 * 8.0).round().min(8.0) as usize;
        let bar = crate::text::hbar(bar_len.max(1), 8);

        // pct already carries the share scaled to hundreds; reuse the
        // canonical formatter so the sub-one bucket renders consistently.
        let pct_str = crate::text::format_pct_f64(pct, 100.0);
        let display_text = format!(" {name} ({info}) {pct_str}");

        lines.push(Line::from(vec![
            Span::styled(format!(" {label}"), Style::default().fg(theme::LABEL)),
            Span::styled(bar, Style::default().fg(bar_color)),
            Span::styled(display_text, Style::default().fg(theme::TEXT_BRIGHT)),
        ]));
    }

    let total_items = items.len();
    let can_scroll_up = scroll > 0;
    let can_scroll_down = scroll + visible_height < total_items;
    let scroll_indicator = match (can_scroll_up, can_scroll_down) {
        (true, true) => " ▲▼ ",
        (true, false) => " ▲ ",
        (false, true) => " ▼ ",
        (false, false) => "",
    };

    let popup = Paragraph::new(lines).block(
        popup_block(&format!(" Breakdown ({total_items}) ")).title_bottom(Line::from(vec![
            Span::styled(
                " ↑↓: scroll  b/Esc: close ",
                Style::default().fg(theme::LABEL),
            ),
            Span::styled(scroll_indicator, Style::default().fg(theme::WARNING)),
        ])),
    );

    frame.render_widget(popup, popup_area);
}

fn draw_filter_popup(frame: &mut Frame, area: Rect, state: &mut crate::AppState) {
    use ratatui::widgets::Clear;

    let total_items = crate::PeriodFilter::ALL_VARIANTS.len() + 1;
    // Input-mode adds: blank + input row, plus an error row when invalid.
    let extra_lines: u16 = if state.filter_input_mode() {
        if state.filter_input_error() { 3 } else { 2 }
    } else {
        0
    };
    // Widened to fit the format hint:
    // `YYYY · YYYY-MM · YYYY-MM-DD · YYYY-MM-DD..YYYY-MM-DD` (~58 chars).
    let popup_width = 60.min(area.width.saturating_sub(4));
    let popup_height = (total_items as u16 + 4 + extra_lines).min(area.height.saturating_sub(4));

    let popup_area = Rect {
        x: area.width.saturating_sub(popup_width) / 2,
        y: area.height.saturating_sub(popup_height) / 2,
        width: popup_width,
        height: popup_height,
    };

    state.layout.filter_popup_area = Some(popup_area);
    frame.render_widget(Clear, popup_area);

    let mut lines: Vec<Line> = Vec::new();
    for (i, variant) in crate::PeriodFilter::ALL_VARIANTS.iter().enumerate() {
        let is_selected = i == state.filter_popup_selected() && !state.filter_input_mode();
        let is_current = *variant == state.period_filter;

        let marker = if is_current { "●" } else { " " };
        let range_label = variant.date_range_label();
        let text = if range_label.is_empty() {
            format!(" {} {}", marker, variant.label())
        } else {
            format!(" {} {} {}", marker, variant.label(), range_label)
        };

        let style = if is_selected {
            Style::default()
                .fg(theme::TEXT_BRIGHT)
                .bg(theme::SELECTED_BG)
                .add_modifier(Modifier::BOLD)
        } else if is_current {
            Style::default().fg(theme::PRIMARY)
        } else {
            Style::default().fg(theme::VALUE)
        };

        lines.push(Line::from(Span::styled(text, style)));
    }

    let custom_idx = crate::PeriodFilter::ALL_VARIANTS.len();
    let is_custom_selected =
        state.filter_popup_selected() == custom_idx && !state.filter_input_mode();
    let is_custom_current = matches!(state.period_filter, crate::PeriodFilter::Custom(_, _));
    let custom_marker = if is_custom_current { "●" } else { " " };
    let custom_label = if is_custom_current {
        format!(
            " {} Custom {}",
            custom_marker,
            state.period_filter.date_range_label()
        )
    } else {
        format!(" {custom_marker} Custom...")
    };
    let custom_style = if is_custom_selected {
        Style::default()
            .fg(theme::TEXT_BRIGHT)
            .bg(theme::SELECTED_BG)
            .add_modifier(Modifier::BOLD)
    } else if is_custom_current {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::VALUE)
    };
    lines.push(Line::from(Span::styled(custom_label, custom_style)));

    if let Some(filter_input) = state.filter_input().filter(|_| state.filter_input_mode()) {
        lines.push(Line::from(""));
        let input_color = if state.filter_input_error() {
            theme::ERROR
        } else {
            theme::TEXT_BRIGHT
        };
        let mut spans = vec![Span::styled("   ", Style::default().fg(theme::LABEL))];
        spans.extend(filter_input.render_spans(
            "> ",
            Style::default().fg(input_color),
            Style::default().fg(theme::TEXT_BRIGHT).bg(theme::PRIMARY),
        ));
        lines.push(Line::from(spans));
        if state.filter_input_error() {
            lines.push(Line::from(Span::styled(
                "   ⚠ Invalid format. Esc: back to presets",
                Style::default().fg(theme::ERROR),
            )));
        }
    }

    let footer = if state.filter_input_mode() {
        " YYYY · YYYY-MM · YYYY-MM-DD · YYYY-MM-DD..YYYY-MM-DD "
    } else {
        " ↑↓: nav  Enter: apply  0-9: type a date  Esc: close "
    };

    let popup = Paragraph::new(lines).block(
        popup_block(" Filter Period ")
            .title_bottom(Line::from(footer).style(Style::default().fg(theme::LABEL))),
    );

    frame.render_widget(popup, popup_area);
}

fn draw_title_edit_popup(frame: &mut Frame, area: Rect, state: &mut crate::AppState) {
    use ratatui::widgets::Clear;

    let popup_width = 72u16.min(area.width.saturating_sub(4));
    let popup_height = 5u16.min(area.height.saturating_sub(4));
    let popup_area = Rect {
        x: area.width.saturating_sub(popup_width) / 2,
        y: area.height.saturating_sub(popup_height) / 2,
        width: popup_width,
        height: popup_height,
    };
    // Without this, `handle_mouse_click` sees no popup area and treats a
    // click INSIDE the editor as outside → dismisses and drops the edit.
    state.layout.active_popup_area = Some(popup_area);
    frame.render_widget(Clear, popup_area);

    let Some(title_input) = state.title_input() else {
        return;
    };
    let mut input_spans = vec![Span::raw("  ")];
    input_spans.extend(title_input.render_spans(
        "> ",
        Style::default().fg(theme::TEXT_BRIGHT),
        Style::default().fg(theme::TEXT_BRIGHT).bg(theme::PRIMARY),
    ));
    let lines = vec![Line::from(input_spans)];

    let popup = Paragraph::new(lines).block(
        popup_block(" Edit session title ").title_bottom(
            Line::from(" Enter: save  ·  ^R: AI generate  ·  Esc: cancel ")
                .style(Style::default().fg(theme::LABEL)),
        ),
    );
    frame.render_widget(popup, popup_area);
}

fn draw_project_popup(frame: &mut Frame, area: Rect, state: &mut crate::AppState) {
    use ratatui::widgets::Clear;

    let total = state.project_list.len() + 1;
    let max_visible: usize = 20;
    let visible = total.min(max_visible);
    let popup_width = 60u16.min(area.width.saturating_sub(4));
    let popup_height = (visible as u16 + 3)
        .min(area.height.saturating_sub(4))
        .min(23);

    let popup_area = Rect {
        x: area.width.saturating_sub(popup_width) / 2,
        y: area.height.saturating_sub(popup_height) / 2,
        width: popup_width,
        height: popup_height,
    };

    state.layout.project_popup_area = Some(popup_area);
    frame.render_widget(Clear, popup_area);

    // Inner content rows = popup_height - 2 (top + bottom border). `title_bottom`
    // is rendered onto the bottom border line and does not consume an extra row.
    let inner_height = popup_height.saturating_sub(2) as usize;
    let sel = state.project_popup_selected();
    let mut scroll_val = state.project_popup_scroll();
    if sel < scroll_val {
        scroll_val = sel;
    } else if inner_height > 0 && sel >= scroll_val + inner_height {
        scroll_val = sel + 1 - inner_height;
    }
    state.set_project_popup_scroll(scroll_val);

    // Detect basename collisions so we can disambiguate two projects that
    // share a final path segment. Colliding entries get the parent directory
    // appended in dim brackets.
    let mut basename_counts: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    for (name, _, _) in &state.project_list {
        let base = name.rsplit(['/', '\\']).next().unwrap_or(name.as_str());
        *basename_counts.entry(base).or_insert(0) += 1;
    }

    // Row order follows the Projects panel sort mode. `project_list_sorted`
    // is the single source of truth so panel and popup agree on rank.
    let sorted = state.project_list_sorted();

    let mut lines: Vec<Line> = Vec::new();
    for i in scroll_val..(scroll_val + inner_height).min(total) {
        let is_selected = i == sel;
        if i == 0 {
            let is_current = state.project_filter.is_none();
            let marker = if is_current { "\u{25cf}" } else { " " };
            let text = format!(" {marker} All");
            let style = if is_selected {
                Style::default()
                    .fg(theme::TEXT_BRIGHT)
                    .bg(theme::SELECTED_BG)
                    .add_modifier(Modifier::BOLD)
            } else if is_current {
                Style::default().fg(theme::PRIMARY)
            } else {
                Style::default().fg(theme::PROJECT)
            };
            lines.push(Line::from(Span::styled(text, style)));
        } else if let Some((name, tokens, last_date)) = sorted.get(i - 1).copied() {
            let is_current = state.project_filter.as_ref() == Some(name);
            let marker = if is_current { "\u{25cf}" } else { " " };
            let basename = name.rsplit(['/', '\\']).next().unwrap_or(name.as_str());
            // When two projects share a basename, append the immediate parent
            // directory in parentheses to disambiguate.
            let short_owned: String = if basename_counts.get(basename).copied().unwrap_or(0) > 1 {
                let parent = name
                    .rsplit_once(['/', '\\'])
                    .map_or("", |(p, _)| p)
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or("");
                if parent.is_empty() {
                    basename.to_string()
                } else {
                    format!("{basename} ({parent})")
                }
            } else {
                basename.to_string()
            };
            let short = short_owned.as_str();
            let token_str = crate::format_number(*tokens);
            let date_str = last_date.format("%Y-%m-%d").to_string();
            let suffix = format!("{token_str}  {date_str}");
            let inner_width = popup_width.saturating_sub(2) as usize;
            let prefix_len = 3 + 2; // " X " marker + "  " separator before suffix
            let max_name_len = inner_width.saturating_sub(prefix_len + suffix.len());
            let short_width = unicode_width::UnicodeWidthStr::width(short);
            let display_name: String = if short_width > max_name_len {
                truncate_with_ellipsis(short, max_name_len)
            } else {
                short.to_string()
            };
            let display_name_width = unicode_width::UnicodeWidthStr::width(display_name.as_str());
            let pad = max_name_len.saturating_sub(display_name_width);
            let text = format!(
                " {} {}{:pad$}  {}",
                marker,
                display_name,
                "",
                suffix,
                pad = pad
            );
            let style = if is_selected {
                Style::default()
                    .fg(theme::TEXT_BRIGHT)
                    .bg(theme::SELECTED_BG)
                    .add_modifier(Modifier::BOLD)
            } else if is_current {
                Style::default().fg(theme::PRIMARY)
            } else {
                Style::default().fg(theme::PROJECT)
            };
            lines.push(Line::from(Span::styled(text, style)));
        }
    }

    let footer = " ↑↓: nav  Enter: apply  Esc: close ";

    let popup = Paragraph::new(lines).block(
        popup_block(" Filter Project ")
            .title_bottom(Line::from(footer).style(Style::default().fg(theme::LABEL))),
    );

    frame.render_widget(popup, popup_area);
}

/// Per-day metric value, used by trends to build sparklines and the
/// trailing-window averages shown alongside them.
pub(crate) struct DailyTrendValue {
    /// Numerator (e.g., cache_read tokens).
    pub num: f64,
    /// Denominator (e.g., input + cache_read tokens). Skipped when 0.
    pub den: f64,
}

/// How a calendar day with no activity enters a trend series. A per-day rate
/// (`$/day`, `Sessions/day`) must count it as zero, or the trailing average
/// silently becomes a per-active-day figure and disagrees with every other
/// per-day number. A per-session rate must skip it — a day with no sessions
/// has no ratio to contribute.
#[derive(Clone, Copy)]
pub(crate) enum MissingDay {
    Zero,
    Skip,
}

pub(crate) fn metric_per_day(
    state: &AppState,
    today: chrono::NaiveDate,
    days: usize,
    missing: MissingDay,
    mut sample: impl FnMut(&crate::aggregator::DailyGroup) -> DailyTrendValue,
) -> Vec<(chrono::NaiveDate, Option<f64>)> {
    let by_date: std::collections::HashMap<chrono::NaiveDate, &crate::aggregator::DailyGroup> =
        state.daily_groups.iter().map(|g| (g.date, g)).collect();
    (0..days)
        .rev()
        .map(|offset| {
            let date = today - chrono::Duration::days(offset as i64);
            // A zero denominator is "not computable", never zero — it stays
            // `None` regardless of how absent days are treated.
            let value = match by_date.get(&date) {
                Some(g) => {
                    let v = sample(g);
                    (v.den > 0.0).then(|| v.num / v.den)
                }
                None => match missing {
                    MissingDay::Zero => Some(0.0),
                    MissingDay::Skip => None,
                },
            };
            (date, value)
        })
        .collect()
}

pub(crate) fn tokens_per_session_value(group: &crate::aggregator::DailyGroup) -> DailyTrendValue {
    let mut tokens = 0u64;
    let mut sess = 0u64;
    for s in &group.sessions {
        if s.is_subagent {
            continue;
        }
        tokens += s.work_tokens();
        sess += 1;
    }
    DailyTrendValue {
        num: tokens as f64,
        den: sess as f64,
    }
}

pub(crate) fn sessions_per_day_value(group: &crate::aggregator::DailyGroup) -> DailyTrendValue {
    let sess = group.sessions.iter().filter(|s| !s.is_subagent).count();
    // Den=1 so the sample IS the session count; metric_per_day will return
    // `Some(sess)` only when `den > 0`, which is always true here.
    DailyTrendValue {
        num: sess as f64,
        den: 1.0,
    }
}

/// Returns (recent_avg, baseline_avg) where recent = last 7 samples and
/// baseline = all 30 samples. Both averages drop missing days (None) so
/// gaps don't drag values toward zero.
pub(crate) fn summarise_series(
    samples: &[(chrono::NaiveDate, Option<f64>)],
) -> (Option<f64>, Option<f64>) {
    fn avg(values: &[f64]) -> Option<f64> {
        if values.is_empty() {
            None
        } else {
            Some(values.iter().sum::<f64>() / values.len() as f64)
        }
    }
    let recent: Vec<f64> = samples
        .iter()
        .rev()
        .take(7)
        .filter_map(|(_, v)| *v)
        .collect();
    let baseline: Vec<f64> = samples.iter().filter_map(|(_, v)| *v).collect();
    (avg(&recent), avg(&baseline))
}

/// Render the recent-vs-baseline delta as a percent change. Uniform across
/// every metric in the popup so absolute units (%, tokens, sess) don't leak
/// into the delta column.
pub(crate) fn fmt_delta_pct(recent: Option<f64>, baseline: Option<f64>) -> String {
    match (recent, baseline) {
        (Some(r), Some(b)) if b.abs() > f64::EPSILON => {
            let pct = ((r - b) / b) * 100.0;
            if pct.abs() < 5.0 {
                "≈".to_string()
            } else if pct > 0.0 {
                format!("↑+{pct:.0}%")
            } else {
                format!("↓{pct:.0}%")
            }
        }
        _ => "—".to_string(),
    }
}

/// Per-project drilldown popup. Aggregates data for the single project
/// identified by `state.project_detail_path`: total tokens / cost / sessions,
/// 30-day trend sparklines, model + tool composition, and the most recent
/// sessions. Opened from the Projects detail popup (panel 1) via Enter.
fn draw_project_detail_popup(frame: &mut Frame, area: Rect, state: &mut AppState) {
    use ratatui::widgets::Clear;

    let today = chrono::Local::now().date_naive();
    let path = state.project_detail_path().to_string();
    if path.is_empty() {
        state.active_popup = crate::ActivePopup::None;
        return;
    }

    let popup_width = 90.min(area.width.saturating_sub(4));
    let popup_height = 30.min(area.height.saturating_sub(4));
    let popup_area = Rect {
        x: area.width.saturating_sub(popup_width) / 2,
        y: area.height.saturating_sub(popup_height) / 2,
        width: popup_width,
        height: popup_height,
    };
    state.layout.active_popup_area = Some(popup_area);
    frame.render_widget(Clear, popup_area);

    // Aggregate everything in one pass over the daily groups so we don't walk
    // `state.daily_groups` three times.
    let calculator = crate::aggregator::CostCalculator::global();
    let mut total_tokens: u64 = 0;
    let mut total_cost: f64 = 0.0;
    let mut total_turns: u64 = 0;
    let mut project_churn = crate::domain::LineChurn::default();
    // Dedup by file_path: one logical session can span multiple days, and
    // `daily_groups` carries one entry per (session, day). The Projects panel
    // counts unique files, so this view must too — otherwise the same number
    // disagrees across surfaces.
    let mut unique_sessions: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    let mut session_day_count: usize = 0;
    let mut first_seen: Option<chrono::NaiveDate> = None;
    let mut last_seen: Option<chrono::NaiveDate> = None;
    let mut model_cost: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    let mut tool_count: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut daily_tokens: std::collections::HashMap<chrono::NaiveDate, u64> =
        std::collections::HashMap::new();
    let mut daily_cost: std::collections::HashMap<chrono::NaiveDate, f64> =
        std::collections::HashMap::new();
    let mut active_days_set: std::collections::HashSet<chrono::NaiveDate> =
        std::collections::HashSet::new();
    // (date, project-label, tokens, cost, model, session_file_path, branch, duration_mins)
    let mut sessions_rev: Vec<(
        chrono::NaiveDate,
        u64,
        f64,
        bool, // unpriced — cost is a lower bound, mark it
        Option<String>,
        std::path::PathBuf,
        Option<String>,
        i64,
    )> = Vec::new();
    let mut project_unpriced = false;
    let mut unpriced_models: std::collections::HashSet<String> = std::collections::HashSet::new();
    for group in &state.daily_groups {
        let mut by_parent = None;
        for session in &group.sessions {
            if session.project_name != path {
                continue;
            }
            // Money, tokens and models include subagents, as the Projects
            // panel and Overview do; session counts and turns stay the user's.
            let sess_tokens = session.work_tokens();
            let sess_cost: f64 = session.cost(calculator);
            let sess_unpriced = session.has_unpriced_model(calculator);
            total_tokens += sess_tokens;
            total_cost += sess_cost;
            project_unpriced |= sess_unpriced;
            for (model, tokens) in &session.day_tokens_by_model {
                let c = calculator.calculate_cost(tokens, Some(model));
                let normalized = crate::aggregator::normalize_model_name(model);
                if c.is_none() {
                    unpriced_models.insert(normalized.clone());
                }
                *model_cost.entry(normalized).or_insert(0.0) += c.unwrap_or(0.0);
            }
            *daily_tokens.entry(group.date).or_insert(0) += sess_tokens;
            *daily_cost.entry(group.date).or_insert(0.0) += sess_cost;
            if session.is_subagent {
                continue;
            }
            unique_sessions.insert(session.file_path.clone());
            session_day_count += 1;
            active_days_set.insert(group.date);
            total_turns += session.day_prompts;
            project_churn.add(session.line_churn);
            for (tool, count) in &session.day_tool_usage {
                *tool_count.entry(tool.clone()).or_insert(0) += count;
            }
            first_seen = Some(first_seen.map_or(group.date, |d| d.min(group.date)));
            last_seen = Some(last_seen.map_or(group.date, |d| d.max(group.date)));
            let duration = (session.day_last_timestamp - session.day_first_timestamp)
                .num_minutes()
                .max(1);
            // The row folds in its subagents so it matches the Daily row.
            let subagents = by_parent
                .get_or_insert_with(|| group.subagent_spend_by_parent())
                .get(session.file_path.as_path())
                .copied()
                .unwrap_or_default();
            sessions_rev.push((
                group.date,
                sess_tokens + subagents.work_tokens,
                sess_cost + subagents.cost,
                sess_unpriced || subagents.unpriced,
                session.model.clone(),
                session.file_path.clone(),
                session.git_branch.clone(),
                duration,
            ));
        }
    }
    // Sort sessions by date desc (newest first).
    sessions_rev.sort_by_key(|s| std::cmp::Reverse(s.0));

    let label = state.project_label(&path);
    let label_dim = Style::default().fg(theme::LABEL);
    let primary_bold = Style::default().fg(theme::PRIMARY).bold();

    // No leading blank: the title is rendered on the top border line, so a
    // `Line::from("")` here would push every section down one row from the
    // convention used by Dashboard / Insights / Tools detail popups.
    let mut content: Vec<Line> = Vec::new();

    // Summary header. All four totals are lifetime (across every day the
    // project has been active), made explicit by the section title.
    let active_days = active_days_set.len();
    let span = match (first_seen, last_seen) {
        (Some(f), Some(l)) if f == l => f.format("%Y-%m-%d").to_string(),
        (Some(f), Some(l)) => format!("{}..{}", f.format("%Y-%m-%d"), l.format("%Y-%m-%d")),
        _ => "—".to_string(),
    };
    content.push(Line::from(Span::styled(
        " Lifetime totals",
        Style::default().fg(theme::PRIMARY).bold(),
    )));
    // Sessions = unique files (matches Projects panel "N ses"). The dim
    // suffix exposes "session-days" so users who notice both numbers in the
    // sparkline / Recent rows know which one they're looking at. Suppressed
    // when no session spans multiple days — the parenthetical would just
    // restate the headline.
    let unique_session_count = unique_sessions.len();
    let session_day_suffix = if session_day_count > unique_session_count {
        format!(" ({session_day_count} session-days)")
    } else {
        String::new()
    };
    // Active days moved to the Range line so the top row stays under the
    // popup's inner width (88 cols) even when `(N session-days)` is shown.
    content.push(Line::from(vec![
        Span::styled("  Cost  ", label_dim),
        Span::styled(
            format_cost_marked(total_cost, project_unpriced, 2),
            cost_total_style(project_unpriced),
        ),
        Span::styled("   Tokens ", label_dim),
        Span::styled(crate::format_number(total_tokens), primary_bold),
        Span::styled("   Sessions ", label_dim),
        Span::styled(format!("{unique_session_count}"), primary_bold),
        Span::styled(session_day_suffix, label_dim),
        Span::styled("   Prompts ", label_dim),
        Span::styled(crate::format_number(total_turns), primary_bold),
    ]));
    content.push(Line::from(vec![
        Span::styled("  Range ", label_dim),
        Span::styled(span, Style::default().fg(theme::VALUE)),
        Span::styled("   Active ", label_dim),
        Span::styled(format!("{active_days}d"), primary_bold),
    ]));
    content.push(churn_line("  Edits ", project_churn, label_dim));
    content.push(Line::from(""));

    // 30-day sparklines: daily token volume and daily cost. Adapts to popup
    // width so the leftmost column stays under the label even on narrow terms.
    let spark_days = (popup_width as usize).saturating_sub(18).clamp(7, 30);
    let tokens_spark = crate::ui::dashboard::render_spark_line(today, spark_days, |d| {
        daily_tokens.get(&d).copied().unwrap_or(0) as f64
    });
    let cost_spark = crate::ui::dashboard::render_spark_line(today, spark_days, |d| {
        daily_cost.get(&d).copied().unwrap_or(0.0)
    });
    content.push(Line::from(Span::styled(
        format!(" Trailing {spark_days} days (one cell per day)"),
        Style::default().fg(theme::PRIMARY).bold(),
    )));
    content.push(Line::from(vec![
        Span::styled("  Tokens   ", label_dim),
        Span::styled(tokens_spark, Style::default().fg(theme::PRIMARY)),
    ]));
    content.push(Line::from(vec![
        Span::styled("  $/day    ", label_dim),
        Span::styled(cost_spark, Style::default().fg(theme::PRIMARY)),
    ]));
    content.push(Line::from(""));

    // Top models by cost share (cumulative across all days). Unpriced models
    // must keep the section alive (their share is unknown, not zero) — hiding
    // it would erase the only clue that the project's cost is a lower bound.
    if total_cost > 0.0 || !unpriced_models.is_empty() {
        let mut models: Vec<(String, f64)> = model_cost.into_iter().collect();
        models.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        content.push(Line::from(Span::styled(
            " Models — by cost share",
            Style::default().fg(theme::PRIMARY).bold(),
        )));
        for (name, cost) in models.iter().take(5) {
            let unknown = unpriced_models.contains(name);
            let pct_str = if unknown {
                "—".to_string()
            } else {
                crate::text::format_pct_f64(*cost, total_cost)
            };
            let clr = model_color(name);
            let bar_w = 18usize;
            let filled = if total_cost > 0.0 {
                ((cost / total_cost) * bar_w as f64).round() as usize
            } else {
                0
            };
            let bar = format!(
                "{}{}",
                "█".repeat(filled.min(bar_w)),
                "░".repeat(bar_w.saturating_sub(filled))
            );
            content.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(bar, Style::default().fg(clr)),
                Span::styled(format!("  {name:<14}"), Style::default().fg(clr)),
                Span::styled(
                    format!("{} ", format_cost_marked(*cost, unknown, 0)),
                    cost_total_style(unknown),
                ),
                Span::styled(pct_str, label_dim),
            ]));
        }
        content.push(Line::from(""));
    }

    // Top tools (inline).
    if !tool_count.is_empty() {
        let mut tools: Vec<(String, usize)> = tool_count
            .into_iter()
            .filter(|(name, c)| !name.is_empty() && *c > 0)
            .collect();
        // Tiebreaker: alphabetical by tool name. Without this, tools with
        // equal call counts shuffle between frames because they come from
        // a HashMap whose iteration order is randomized per instance.
        tools.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let inner_w = popup_width.saturating_sub(4) as usize;
        let mut current = String::from("  ");
        let mut tool_lines: Vec<Line> = Vec::new();
        for (name, count) in tools.iter().take(12) {
            let short = crate::aggregator::format_tool_short(name);
            let chunk = format!("{short}({count})  ");
            if current.len() + chunk.len() > inner_w && current.trim() != "" {
                tool_lines.push(Line::from(Span::styled(
                    std::mem::take(&mut current),
                    label_dim,
                )));
                current = format!("  {chunk}");
            } else {
                current.push_str(&chunk);
            }
        }
        if !current.trim().is_empty() {
            tool_lines.push(Line::from(Span::styled(current, label_dim)));
        }
        if !tool_lines.is_empty() {
            content.push(Line::from(Span::styled(
                " Tools — by call count",
                Style::default().fg(theme::PRIMARY).bold(),
            )));
            content.extend(tool_lines);
            content.push(Line::from(""));
        }
    }

    // Recent sessions. Column header matches the row layout below so a
    // reader doesn't have to decode each row's positional meaning.
    content.push(Line::from(Span::styled(
        " Recent sessions — newest first",
        Style::default().fg(theme::PRIMARY).bold(),
    )));
    content.push(Line::from(vec![Span::styled(
        "      date       branch   duration   tokens     cost      model",
        Style::default().fg(theme::LABEL),
    )]));
    for (i, (date, tokens, cost, unpriced, model, file_path, branch, duration_mins)) in
        sessions_rev.iter().take(10).enumerate()
    {
        let model_short = model.as_ref().map_or_else(
            || "?".to_string(),
            |m| crate::aggregator::normalize_model_name(m),
        );
        let branch_short = branch
            .as_ref()
            .map(|b| {
                let name = b.split('/').next_back().unwrap_or(b);
                format!(" #{name}")
            })
            .unwrap_or_default();
        let dur_str = if *duration_mins >= 60 {
            format!("{}h{}m", duration_mins / 60, duration_mins % 60)
        } else {
            format!("{duration_mins}m")
        };
        content.push(Line::from(vec![
            Span::styled(format!("  {:>2}. ", i + 1), label_dim),
            Span::styled(
                date.format("%Y-%m-%d").to_string(),
                Style::default().fg(theme::TIME),
            ),
            Span::styled(branch_short, Style::default().fg(theme::BRANCH)),
            Span::styled(format!("  {dur_str:>6}  "), label_dim),
            Span::styled(
                format!("{:>9}", crate::format_number(*tokens)),
                Style::default().fg(theme::PRIMARY),
            ),
            Span::styled(
                format!("  {}", format_cost_marked(*cost, *unpriced, 2)),
                cost_style_marked(*cost, *unpriced),
            ),
            Span::styled(
                format!("  [{model_short}]"),
                Style::default().fg(model_color(&model_short)),
            ),
        ]));
        // Session UUID below the summary row so it's available for `claude -r`
        // without crowding the metadata line.
        let session_id = file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?");
        content.push(Line::from(Span::styled(
            format!("      {session_id}"),
            Style::default().fg(theme::SESSION_ID),
        )));
    }
    if sessions_rev.len() > 10 {
        content.push(Line::from(Span::styled(
            format!("  +{} earlier sessions", sessions_rev.len() - 10),
            label_dim,
        )));
    }

    let visible_height = popup_height.saturating_sub(2) as usize;
    let max_scroll = content.len().saturating_sub(visible_height);
    let scroll = state.project_detail_scroll().min(max_scroll);
    state.set_project_detail_scroll(scroll);

    // Footer aligned with the other detail popups (CLAUDE.md "Footer /
    // title_bottom Format"): `key: action` with colon+space, `▲▼` for the
    // can-scroll indicator (not `↑↓` — those are reserved for keybind
    // hints), and a position chip `[N/M project-label]` mirroring
    // `insights_detail`'s `[1/4] Metrics`.
    let can_scroll_up = scroll > 0;
    let can_scroll_down = scroll + visible_height < content.len();
    let scroll_indicator = if can_scroll_up && can_scroll_down {
        " ▲▼ "
    } else if can_scroll_up {
        " ▲ "
    } else if can_scroll_down {
        " ▼ "
    } else {
        ""
    };
    let project_total = state.stats.project_stats.len();
    let project_pos = state.dashboard_scroll[1] + 1;
    let position_chip = if project_total > 0 {
        format!(" [{project_pos}/{project_total} {label}] ")
    } else {
        String::new()
    };

    let title = format!(" Project · {label} ");
    let block = popup_block(&title).title_bottom(Line::from(vec![
        Span::styled(
            " ↑↓: scroll  Enter/q: back ",
            Style::default().fg(theme::LABEL),
        ),
        Span::styled(scroll_indicator, Style::default().fg(theme::WARNING)),
        Span::styled(position_chip, Style::default().fg(theme::PRIMARY)),
    ]));
    let paragraph = ratatui::widgets::Paragraph::new(content)
        .scroll((scroll as u16, 0))
        .block(block);
    frame.render_widget(paragraph, popup_area);
}

fn draw_help_popup(frame: &mut Frame, area: Rect, state: &mut AppState) {
    use ratatui::widgets::Clear;

    let popup_width = 76.min(area.width.saturating_sub(4));
    let popup_height = area.height.saturating_sub(4).min(44);

    let popup_area = Rect {
        x: area.width.saturating_sub(popup_width) / 2,
        y: area.height.saturating_sub(popup_height) / 2,
        width: popup_width,
        height: popup_height,
    };

    state.layout.active_popup_area = Some(popup_area);
    frame.render_widget(Clear, popup_area);

    let mut content = vec![
        Line::from(Span::styled(
            "  Global",
            Style::default().fg(theme::HEADER).bold(),
        )),
        Line::from("  Tab / 1-4     Switch tabs (Dashboard/Live/Daily/Insights)"),
        Line::from("  /             Search sessions (inline filters supported)"),
        Line::from("                ↑↓ navigate results · ↑ recalls history while empty"),
        Line::from("                filter:live|paused|busy|today|week|month"),
        Line::from("                filter:date:Y-M-D"),
        Line::from("                project:NAME · branch:NAME · model:NAME"),
        Line::from("  f             Open period filter"),
        Line::from("  p             Open project filter"),
        Line::from("  m             Open pinned-sessions view (header shows `*N` count)"),
        Line::from("                J / K in the pin list reorders the focused entry"),
        Line::from("  ?             Show this help"),
        Line::from("  q             Quit (press twice to confirm)"),
        Line::from("  Popups        d/u page · g/G top/end scroll everywhere"),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Dashboard ", Style::default().fg(theme::HEADER).bold()),
            Span::styled("(Tab 1)", Style::default().fg(theme::LABEL)),
        ]),
        Line::from("  ←/→ h/l       Switch panels"),
        Line::from("  ↑/↓ j/k       Scroll panel content"),
        Line::from("  Enter         Expand panel detail"),
        Line::from("  Projects detail: j/k move cursor, click row to select,"),
        Line::from("                   Enter / double-click → per-project popup"),
        Line::from("                   s toggles sort (recent ↔ tokens)"),
        Line::from("  Models/Languages: s toggles sort (recent ↔ tokens)"),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Ecosystem detail popup",
            Style::default().fg(theme::HEADER).bold(),
        )]),
        Line::from("  ←/→ h/l Tab   Switch section (Tools/Skills/Commands/Subagents)"),
        Line::from("  1-4           Jump to section"),
        Line::from("  ↑/↓ j/k       Scroll within section"),
        Line::from("  PgUp/PgDn u/d Page scroll (10 lines)"),
        Line::from("  Home/End g/G  Jump to top / bottom"),
        Line::from("  Enter/Space   (Tools) Expand/collapse MCP server"),
        Line::from("  o / c         (Tools) Open all / close all MCP servers"),
        Line::from("  s             Toggle sort (recent ↔ calls), all sections"),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Live ", Style::default().fg(theme::HEADER).bold()),
            Span::styled("(Tab 2)", Style::default().fg(theme::LABEL)),
        ]),
        Line::from("  ↑/↓ j/k       Select session (busy → today → older → paused)"),
        Line::from("  Enter         Open conversation in pane"),
        Line::from("  i             Session details (includes pid / status)"),
        Line::from("  Space         Pin / unpin (same as Daily)"),
        Line::from("  y             Copy `cd ... && claude -r UUID` to clipboard"),
        Line::from("  ←/→ h/l       Time-travel: step back/forward through frozen snapshots"),
        Line::from("  t             Edit session title"),
        Line::from("  T             Jump back to the live now view"),
        Line::from("  v             Cycle pane layout (split / active / paused)"),
        Line::from("  /             Search, pre-filtered to filter:live"),
        Line::from("  Glyphs        🟢 busy · ◉ today · ○ older"),
        Line::from("                ⏸ paused · ⟳ alive in prior run"),
        Line::from("                past snapshot: ● still live now · · ended since"),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Daily ", Style::default().fg(theme::HEADER).bold()),
            Span::styled("(Tab 3)", Style::default().fg(theme::LABEL)),
        ]),
        Line::from("  ←/→ h/l       Navigate days"),
        Line::from("  ↑/↓ j/k       Select session (or scroll breakdown)"),
        Line::from("  b             Toggle breakdown focus"),
        Line::from("  t             Edit session title"),
        Line::from("  T             Jump to today"),
        Line::from("  i / [i] click Session details"),
        Line::from("  Enter         Open conversation (⇧Enter / C: in a new pane)"),
        Line::from("  Space         Pin / unpin session"),
        Line::from("  s / S         Session / Day summary (AI)"),
        Line::from("                in popup: r regenerate · t write resume title"),
        Line::from("  Glyphs        · started this day · » continued from an earlier day"),
        Line::from("                * pinned  (same marks on Live and the conversation list)"),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Insights ", Style::default().fg(theme::HEADER).bold()),
            Span::styled("(Tab 4)", Style::default().fg(theme::LABEL)),
        ]),
        Line::from("  ←/→ h/l       Switch panel"),
        Line::from("  Enter / i     Open detail popup (scroll and ←/→ live there)"),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Conversation ", Style::default().fg(theme::HEADER).bold()),
            Span::styled("(from Daily / Live)", Style::default().fg(theme::LABEL)),
        ]),
        Line::from("  ↑/↓ j/k       Select message"),
        Line::from("  Enter         Expand / collapse message (compact view)"),
        Line::from("  c             Toggle compact ↔ full transcript"),
        Line::from("  d/u           Scroll page (20 lines)"),
        Line::from("  i             Session details"),
        Line::from("  s             Session summary (AI)"),
        Line::from("  y             Copy message to clipboard"),
        Line::from("  /             Search in conversation (reopen restores the last query)"),
        Line::from("  Enter/\u{21e7}Enter  Next / previous match \u{b7} Esc ends the search"),
        Line::from("  n/N           Jump to next / previous message"),
        Line::from("  g/G           Top / Bottom"),
        Line::from("  C             Open another session in a new pane"),
        Line::from("  0-4           Focus list (0) or pane 1-4 · Tab/h/l cycle focus"),
        Line::from("  ⇧Tab          Cycle list mode (Day / Pinned / All)"),
        Line::from("  H/L           Previous / next day (Day list mode)"),
        Line::from("  X             Close the focused pane · C opens one"),
        Line::from("  Q             Close all panes and leave · q/Esc step out one level"),
        Line::from(""),
        Line::from(Span::styled(
            "  CLI Options ",
            Style::default().fg(theme::HEADER).bold(),
        )),
    ];
    let inner_w = popup_width.saturating_sub(4) as usize;
    let flag_col = 16;
    // Wrapped onto the description column rather than cut: a flag's help text
    // is the whole content of its row, and these are the only rows in the
    // popup long enough to reach the edge.
    let desc_w = inner_w.saturating_sub(flag_col + 3).max(16);
    for (flag, help) in crate::cli_help_lines() {
        let (wrapped, _) = crate::text::wrap_text_with_continuation(&help, desc_w);
        let parts = if wrapped.is_empty() {
            vec![String::new()]
        } else {
            wrapped
        };
        for (i, part) in parts.iter().enumerate() {
            let head = if i == 0 { flag.as_str() } else { "" };
            content.push(Line::from(format!("  {head:<flag_col$} {part}")));
        }
    }
    content.extend_from_slice(&[
        Line::from(""),
        Line::from(Span::styled(
            "  Note ",
            Style::default().fg(theme::HEADER).bold(),
        )),
        Line::from("  Tokens = input + output (excludes cache); also called \"work tokens\""),
        Line::from("  Costs  = estimated from API pricing"),
        Line::from("  $/MTok work   = cost ÷ work tokens (input+output)"),
        Line::from("  $/MTok billed = cost ÷ all billed tokens (input+output+cache)"),
        Line::from("  Cache TTL: 5m (1.25× base) vs 1h (2× base);"),
        Line::from("             higher 5m share = cheaper writes"),
        Line::from("  Cache  = ~/.ccsight/cache.json"),
        Line::from("  Pins   = ~/.ccsight/pins.json"),
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::PRIMARY))
        .title(Span::styled(
            " Help [↑↓: scroll] ",
            Style::default().fg(theme::PRIMARY).bold(),
        ));
    let inner = block.inner(popup_area);
    let total_lines = content.len() as u16;
    let max_scroll = total_lines.saturating_sub(inner.height);
    state.set_help_scroll(state.help_scroll().min(max_scroll));

    let popup = Paragraph::new(content)
        .block(block)
        .scroll((state.help_scroll(), 0));

    frame.render_widget(popup, popup_area);

    draw_scrollbar(
        frame,
        popup_area,
        state.help_scroll() as usize,
        total_lines as usize,
        inner.height as usize,
    );
}

fn draw_search_popup(frame: &mut Frame, area: Rect, state: &mut crate::AppState) {
    use ratatui::widgets::Clear;

    let popup_width = (area.width as f32 * 0.8) as u16;
    let popup_height = 24.min(area.height.saturating_sub(4));

    let popup_area = Rect {
        x: area.width.saturating_sub(popup_width) / 2,
        y: 3,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_area);

    let inner = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).split(popup_area);
    state.layout.search_results_area = Some(inner[1]);

    // Pre-parse the input so the title can echo back the recognised
    // filters as bracketed chips. Treats the title as feedback: the
    // user can tell at a glance whether their `filter:` syntax was
    // recognised (chip appears) or got mis-typed (chip absent).
    let (parsed_filters, free_text) = crate::search::parse_search_query(&state.search_input.text);
    let chip_style = Style::default()
        .bg(theme::PRIMARY)
        .fg(theme::TEXT_DARK)
        .add_modifier(Modifier::BOLD);
    let mut title_spans: Vec<Span<'static>> = Vec::new();
    let title: String = if state.searching {
        " Search [Searching...] ".to_string()
    } else if state.search_index.is_none() && state.index_build_task.is_some() {
        " Search [Indexing...] ".to_string()
    } else if state.search_input.text.is_empty() {
        " Search (Esc: cancel, Enter: select) ".to_string()
    } else {
        // Results are deduped to one row per session inside the search
        // pipeline, so the headline is a straight session count.
        let hits = state.search_results.len();
        let headline = format!(" Search · {hits} sessions ");
        title_spans.push(Span::styled(
            headline.clone(),
            Style::default().fg(theme::PRIMARY).bold(),
        ));
        let mut push_chip = |label: String| {
            title_spans.push(Span::styled(format!(" {label} "), chip_style));
            title_spans.push(Span::raw(" "));
        };
        if let Some(s) = parsed_filters.state {
            push_chip(
                match s {
                    crate::search::SessionStateFilter::Live => "live",
                    crate::search::SessionStateFilter::Paused => "paused",
                    crate::search::SessionStateFilter::Busy => "busy",
                }
                .to_string(),
            );
        }
        if let Some(p) = parsed_filters.period {
            let label = match p {
                crate::search::PeriodFilter::Today => "today".to_string(),
                crate::search::PeriodFilter::Week => "week".to_string(),
                crate::search::PeriodFilter::Month => "month".to_string(),
                crate::search::PeriodFilter::On(d) => d.format("%Y-%m-%d").to_string(),
            };
            push_chip(label);
        }
        if let Some(p) = &parsed_filters.project {
            push_chip(format!("project:{p}"));
        }
        if let Some(m) = &parsed_filters.model {
            push_chip(format!("model:{m}"));
        }
        if let Some(b) = &parsed_filters.branch {
            push_chip(format!("branch:{b}"));
        }
        headline
    };

    // Always-visible filter syntax hint on the input's bottom border —
    // discoverable even after the empty-state help disappears.
    let title_line = if title_spans.is_empty() {
        ratatui::text::Line::from(Span::styled(title, Style::default().fg(theme::PRIMARY)))
    } else {
        ratatui::text::Line::from(title_spans)
    };
    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::PRIMARY))
        .title(title_line);

    // Reverse-video query = "selected" (VS Code find widget): the next typed
    // char replaces the restored text wholesale. Mirrors the pane search bar.
    let query_style = if state.search_select_all {
        Style::default().fg(theme::TEXT_BRIGHT).bg(theme::WARM)
    } else {
        Style::default().fg(theme::TEXT_BRIGHT)
    };
    let input_line = Line::from(state.search_input.render_spans(
        "/",
        query_style,
        Style::default().fg(theme::TEXT_BRIGHT).bg(theme::PRIMARY),
    ));
    let input = Paragraph::new(input_line).block(input_block);
    frame.render_widget(input, inner[0]);

    // Standing key hint: j/k are literal input here (unlike every list
    // view), and ↑ is context-dependent — without this line both read as
    // broken keys.
    let results_block = Block::default()
        .borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM)
        .border_style(Style::default().fg(theme::PRIMARY))
        .title_bottom(Line::from(Span::styled(
            " ↑↓: results  ↑(empty): history  Enter: open  Esc: close ",
            Style::default().fg(theme::LABEL),
        )));

    if state.search_results.is_empty() {
        let no_results = if state.search_input.text.is_empty() {
            "Type to search projects, summaries, branches, dates, content...\n\
             Filters: filter:live|paused|busy|today|week|month  filter:date:Y-M-D\n\
                      project:NAME · branch:NAME · model:NAME"
        } else if state.searching {
            "Searching content..."
        } else if state.loading {
            // Nothing is searchable before the first load lands.
            "Loading sessions — results appear once they are read"
        } else if state.search_index.is_none() && state.index_build_task.is_some() {
            // The title says `[Indexing...]` on the same condition; "No
            // results" here would claim an answer the index has not given.
            "Indexing — content matches appear once the index is ready"
        } else {
            "No results found"
        };
        let text = Paragraph::new(no_results)
            .style(Style::default().fg(theme::PLACEHOLDER))
            .block(results_block);
        frame.render_widget(text, inner[1]);
    } else {
        let item_height = 2usize;
        let visible_items = inner[1].height.saturating_sub(2) as usize / item_height;
        let start = if state.search_selected >= visible_items {
            state.search_selected - visible_items + 1
        } else {
            0
        };

        let inner_w = inner[1].width.saturating_sub(2) as usize;
        let items: Vec<ListItem> = state
            .search_results
            .iter()
            .enumerate()
            .skip(start)
            .take(visible_items)
            .map(|(i, result)| {
                let group = &state.daily_groups[result.day_idx];
                let session = group
                    .user_sessions()
                    .nth(result.session_idx)
                    .unwrap_or_else(|| &group.sessions[0]);
                let date_str = group.date.format("%Y-%m-%d").to_string();
                let project = state.project_label(&session.project_name);
                let branch = session
                    .git_branch
                    .as_ref()
                    .map(|b| format!("#{}", b.split('/').next_back().unwrap_or(b)))
                    .unwrap_or_default();

                // Untitled sessions promote their opening user message so
                // line 1 never renders as a blank left column.
                let summary = resolved_title(&state.session_titles, session)
                    .or(session.first_user_message.as_deref())
                    .unwrap_or("");
                // Line-2 snippet is suppressed when it would just echo the
                // title on line 1: a [sum] hit that matched at offset 0 gets
                // no leading ellipsis from extract_snippet, so the snippet
                // equals the title's prefix character-for-character.
                let snippet_text = match result.snippet.as_deref() {
                    Some(s) => {
                        let echoes_title =
                            matches!(result.match_type, search::SearchMatchType::Summary)
                                && !s.starts_with('…')
                                && !s.starts_with("...");
                        if echoes_title { "" } else { s }
                    }
                    None => "",
                };

                let selected = i == state.search_selected;
                let sel_style = Style::default()
                    .bg(theme::SELECTED_BG)
                    .fg(theme::TEXT_BRIGHT);
                let pinned = state.pins.is_pinned(&session.file_path);
                let model_short = session
                    .model
                    .as_deref()
                    .map_or_else(|| "?".to_string(), crate::aggregator::normalize_model_name);

                // Content-forward layout: the title leads line 1 (bright,
                // left) so titles scan down a shared edge, with
                // `project#branch · date` right-aligned and dim; line 2 is the
                // matched snippet, query bolded. Tokens / cost live in the
                // Session Detail popup (`i`), off the scan list.
                use unicode_width::UnicodeWidthStr;
                // `▶` = selection (as in Daily/Live lists); `▸` stays
                // reserved for collapse carets so one glyph keeps one job.
                let marker = if selected {
                    "▶ "
                } else if pinned {
                    "* "
                } else {
                    "  "
                };
                let right = if branch.is_empty() {
                    format!("{project} · {date_str}")
                } else {
                    format!("{project}{branch} · {date_str}")
                };
                let title_avail = inner_w.saturating_sub(2 + right.width() + 2);
                let title = truncate_with_ellipsis(summary, title_avail);
                let gap = title_avail.saturating_sub(title.width()) + 2;

                let marker_style = if selected {
                    sel_style
                } else if pinned {
                    Style::default().fg(theme::WARNING)
                } else {
                    Style::default().fg(theme::SEPARATOR)
                };
                // Query occurrences bold everywhere they appear — title,
                // snippet, or preview — because quick-path rows (project /
                // branch / summary matches) carry no snippet at all.
                let title_base = if selected {
                    sel_style
                } else {
                    Style::default().fg(theme::TEXT_BRIGHT)
                };
                let hit_style = |base: Style| base.add_modifier(Modifier::BOLD);
                let mut line1_spans = vec![Span::styled(marker.to_string(), marker_style)];
                line1_spans.extend(highlight_terms(
                    &title,
                    &free_text,
                    title_base,
                    hit_style(title_base),
                ));
                line1_spans.push(Span::styled(
                    " ".repeat(gap),
                    if selected {
                        sel_style
                    } else {
                        Style::default()
                    },
                ));
                line1_spans.push(Span::styled(
                    right,
                    if selected {
                        sel_style
                    } else {
                        Style::default().fg(theme::LABEL)
                    },
                ));
                let line1 = Line::from(line1_spans);

                // Line 2 is the row's context, dim and indented: the matched
                // snippet (query bolded) for a content hit, else the session's
                // opening user message as a preview of what it was about, else
                // the model as a last resort so the row keeps its height.
                let base2 = if selected {
                    sel_style
                } else {
                    Style::default().fg(theme::SNIPPET)
                };
                let line2 = if !snippet_text.is_empty() {
                    let snip = truncate_with_ellipsis(snippet_text, inner_w.saturating_sub(4));
                    let hit = if selected {
                        hit_style(sel_style)
                    } else {
                        hit_style(Style::default().fg(theme::ACCENT))
                    };
                    let mut spans = vec![Span::styled("    ".to_string(), base2)];
                    spans.extend(highlight_terms(&snip, &free_text, base2, hit));
                    Line::from(spans)
                } else {
                    let preview = session
                        .first_user_message
                        .as_deref()
                        .filter(|m| *m != summary)
                        .unwrap_or(&model_short);
                    let preview = truncate_with_ellipsis(preview, inner_w.saturating_sub(4));
                    let pv_base = if selected {
                        sel_style
                    } else {
                        Style::default().fg(theme::SNIPPET)
                    };
                    let mut spans = vec![Span::styled("    ".to_string(), pv_base)];
                    spans.extend(highlight_terms(
                        &preview,
                        &free_text,
                        pv_base,
                        hit_style(pv_base),
                    ));
                    Line::from(spans)
                };
                ListItem::new(vec![line1, line2])
            })
            .collect();

        let list = List::new(items).block(results_block);
        frame.render_widget(list, inner[1]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod span_width {
        use super::super::{Span, spans_width, truncate_spans};
        use proptest::prelude::*;
        use ratatui::style::Style;

        fn spans(parts: &[&str]) -> Vec<Span<'static>> {
            parts
                .iter()
                .map(|p| Span::styled(p.to_string(), Style::default()))
                .collect()
        }

        // `truncate_spans` is the shared last line of defence for composed rows,
        // so the bound it promises has to hold for any mix of single-width,
        // double-width, and zero-width content.
        proptest! {
            #[test]
            fn truncate_spans_never_exceeds_max_width(
                parts in proptest::collection::vec(
                    proptest::string::string_regex("[a-z ]{0,6}|[ぁ-ん一-龥]{0,4}|[🎯✓]{0,3}").unwrap(),
                    0..6,
                ),
                max_width in 0usize..40,
            ) {
                let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
                let out = truncate_spans(spans(&refs), max_width);
                prop_assert!(
                    spans_width(&out) <= max_width,
                    "{:?} at {} → {}", parts, max_width, spans_width(&out)
                );
            }

            #[test]
            fn truncate_spans_is_identity_when_everything_fits(
                parts in proptest::collection::vec(
                    proptest::string::string_regex("[a-z]{1,6}").unwrap(),
                    1..5,
                ),
            ) {
                let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
                let full = spans(&refs);
                let want = spans_width(&full);
                prop_assert_eq!(spans_width(&truncate_spans(full, want)), want);
            }
        }

        // A double-width char cannot be half-drawn, so it is dropped whole
        // rather than counted as one column.
        #[test]
        fn truncate_spans_drops_a_wide_char_that_would_straddle_the_edge() {
            let out = truncate_spans(spans(&["あ"]), 1);
            assert_eq!(spans_width(&out), 0);
            assert_eq!(spans_width(&truncate_spans(spans(&["あ"]), 2)), 2);
        }
    }

    #[test]
    fn search_mode_replaces_the_tab_bar_with_the_search_keys() {
        let mut state = create_test_state();
        let pairs = [("q", "quit"), ("←→", "panel")];
        let normal: String = tab_help_bar(&state, &pairs)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(normal.contains("q:quit"));

        state.search_mode = true;
        let searching: String = tab_help_bar(&state, &pairs)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        // Every printable key goes into the query, so none of the tab's own
        // keys may be advertised.
        assert!(!searching.contains("q:quit"), "got {searching:?}");
        assert!(searching.contains("Esc:back"), "got {searching:?}");
    }

    // While the index is still being built an empty result list is not an
    // answer; the body must say so, as the title's `[Indexing...]` does.
    #[test]
    fn search_body_says_indexing_rather_than_no_results_while_the_index_builds() {
        let mut state = create_test_state();
        state.search_mode = true;
        state.search_input.set("needle".to_string());
        state.search_results.clear();
        state.searching = false;
        state.search_index = None;
        let (_tx, rx) = std::sync::mpsc::channel();
        state.index_build_task = Some(rx);
        let text = render_to_text(&mut state, 140, 45);
        assert!(text.contains("Indexing"), "{text}");
        assert!(
            !text.contains("No results found"),
            "claimed an answer mid-build"
        );

        state.index_build_task = None;
        let text = render_to_text(&mut state, 140, 45);
        assert!(
            text.contains("No results found"),
            "a finished empty search says so"
        );

        state.loading = true;
        let text = render_to_text(&mut state, 140, 45);
        assert!(text.contains("Loading sessions"), "{text}");
    }

    // `this mo` sums subagents; the model shares under it must be taken over
    // the same sessions, or a model used mostly through subagents reads small.
    #[test]
    fn daily_costs_top_models_include_subagent_spend() {
        use crate::test_helpers::helpers::{
            make_daily_group, make_session_with_tokens, make_test_app_state,
        };
        let today = chrono::Local::now().date_naive();
        let user = make_session_with_tokens("proj", 1_000, 1_000, "claude-opus-5");
        let mut sub = make_session_with_tokens("proj", 4_000_000, 4_000_000, "claude-sonnet-4-6");
        sub.is_subagent = true;
        sub.file_path = std::path::PathBuf::from("/tmp/test/sub.jsonl");
        let mut state = make_test_app_state(vec![make_daily_group(today, vec![user, sub])]);
        state.loading = false;
        state.daily_costs = vec![(today, 100.0)];
        state.dashboard_panel = 0;
        state.active_popup = crate::ActivePopup::DashboardDetail;
        let text = render_to_text(&mut state, 140, 45);
        let line = text
            .lines()
            .find(|l| l.contains("Top models"))
            .expect("the Top models line renders");
        let sonnet = line.find("Sonnet").expect("subagent-only model missing");
        let opus = line.find("Opus").unwrap_or(usize::MAX);
        assert!(
            sonnet < opus,
            "the larger, subagent-only model must lead: {line}"
        );
    }

    #[test]
    fn an_unpriced_model_marks_only_the_months_it_was_used_in() {
        use crate::test_helpers::helpers::{
            make_daily_group, make_session_with_tokens, make_test_app_state,
        };
        let today = chrono::Local::now().date_naive();
        let old = today - chrono::Duration::days(70);
        let mut state = make_test_app_state(vec![
            make_daily_group(
                today,
                vec![make_session_with_tokens(
                    "proj",
                    1_000_000,
                    0,
                    "claude-opus-4-6",
                )],
            ),
            make_daily_group(
                old,
                vec![make_session_with_tokens(
                    "proj",
                    1_000,
                    0,
                    "claude-future-x",
                )],
            ),
        ]);
        state.loading = false;
        state.daily_costs = vec![(today, 5.0), (old, 0.0)];
        state
            .models_without_pricing
            .insert("claude-future-x".to_string());
        state.dashboard_panel = 0;
        state.active_popup = crate::ActivePopup::DashboardDetail;
        let text = render_to_text(&mut state, 140, 45);
        let line = text
            .lines()
            .find(|l| l.contains("this mo:"))
            .expect("the month summary renders");
        assert!(!line.contains('*'), "this month is fully priced: {line}");
    }

    #[test]
    fn month_unpriced_looks_only_at_the_named_month() {
        use crate::test_helpers::helpers::{make_daily_group, make_session_with_tokens};
        let day = |m: u32| chrono::NaiveDate::from_ymd_opt(2026, m, 10).unwrap(); // lint-ok: date-literal
        let groups = vec![
            make_daily_group(
                day(2),
                vec![make_session_with_tokens("p", 1, 0, "claude-opus-4-6")],
            ),
            make_daily_group(
                day(1),
                vec![make_session_with_tokens("p", 1, 0, "claude-future-x")],
            ),
        ];
        assert!(month_unpriced(&groups, "2026-01"));
        assert!(!month_unpriced(&groups, "2026-02"));
        assert!(!month_unpriced(&groups, "not-a-month"));
    }

    #[test]
    fn the_today_vs_average_popup_chart_grows_with_the_terminal() {
        // The popup's chart rows sit between the y-axis `│` and the `└` axis.
        let chart_rows = |w: u16, h: u16| {
            let mut state = create_test_state();
            state.tab = crate::Tab::Insights;
            state.insights_panel = 1;
            state.active_popup = crate::ActivePopup::InsightsDetail { scroll: 0 };
            let text = render_popup_text(&mut state, w, h);
            let axis = text
                .lines()
                .position(|l| l.contains("└──"))
                .expect("x axis");
            let top = text
                .lines()
                .position(|l| l.contains("Today vs Average"))
                .expect("title");
            axis - top - 1
        };
        let small = chart_rows(80, 24);
        let large = chart_rows(140, 45);
        assert!(small >= 8, "keeps the floor: {small}");
        assert!(
            large > small + 10,
            "fills the taller popup: {small} → {large}"
        );
    }

    #[test]
    fn a_rename_of_a_ccsight_generated_session_shows_the_new_name() {
        let mut s = crate::test_helpers::helpers::make_session("p", None, None);
        let (prompt, label) = crate::summary::GENERATED_PROMPTS[0];
        s.first_user_message = Some(format!("{prompt} …"));
        let own = s.display_title().map(str::to_string);
        let mut titles = std::collections::HashMap::new();
        if let Some(t) = &own {
            titles.insert(s.file_path.clone(), t.clone());
        }
        assert_eq!(
            resolved_title(&titles, &s),
            Some(label),
            "unrenamed keeps its label"
        );
        titles.insert(s.file_path.clone(), "my name".to_string());
        assert_eq!(
            resolved_title(&titles, &s),
            Some("my name"),
            "the rename was hidden"
        );
    }

    #[test]
    fn splash_progress_line_is_empty_before_the_walk_publishes_a_total() {
        assert!(progress_spans(0, 0).is_empty());
        assert!(progress_spans(12, 0).is_empty());
    }

    #[test]
    fn splash_progress_line_reports_walked_over_total() {
        let spans = progress_spans(1234, 6135);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "1234 / 6135 files");
    }

    #[test]
    fn bar_intensity_floors_at_thirty_percent_and_caps_at_one() {
        assert_eq!(
            theme::bar_intensity(0.0),
            0.3,
            "zero ratio floors, not black"
        );
        assert_eq!(theme::bar_intensity(1.0), 1.0);
        assert_eq!(theme::bar_intensity(2.0), 1.0, "ratio past 1.0 still caps");
        assert!((theme::bar_intensity(0.5) - 0.65).abs() < 1e-9);
    }

    #[test]
    fn test_calc_scroll_basic() {
        let (visible, max_scroll, scroll) = calc_scroll(10, 20, 0, 2);
        assert_eq!(visible, 8);
        assert_eq!(max_scroll, 12);
        assert_eq!(scroll, 0);
    }

    #[test]
    fn test_calc_scroll_with_scroll_offset() {
        let (visible, max_scroll, scroll) = calc_scroll(10, 20, 5, 2);
        assert_eq!(visible, 8);
        assert_eq!(max_scroll, 12);
        assert_eq!(scroll, 5);
    }

    #[test]
    fn test_calc_scroll_clamps_to_max() {
        let (visible, max_scroll, scroll) = calc_scroll(10, 20, 100, 2);
        assert_eq!(visible, 8);
        assert_eq!(max_scroll, 12);
        assert_eq!(scroll, 12);
    }

    #[test]
    fn test_calc_scroll_items_fit_in_view() {
        let (visible, max_scroll, scroll) = calc_scroll(10, 5, 0, 2);
        assert_eq!(visible, 8);
        assert_eq!(max_scroll, 0);
        assert_eq!(scroll, 0);
    }

    #[test]
    fn test_calc_scroll_different_header() {
        let (visible, max_scroll, scroll) = calc_scroll(10, 20, 0, 4);
        assert_eq!(visible, 6);
        assert_eq!(max_scroll, 14);
        assert_eq!(scroll, 0);
    }

    #[test]
    fn test_calc_scroll_zero_height() {
        let (visible, max_scroll, scroll) = calc_scroll(0, 10, 0, 2);
        assert_eq!(visible, 0);
        assert_eq!(max_scroll, 10);
        assert_eq!(scroll, 0);
    }

    #[test]
    fn format_session_range_today_drops_date() {
        use chrono::{Local, TimeZone};
        let now_local = Local
            .with_ymd_and_hms(2026, 5, 21, 18, 0, 0)
            .unwrap()
            .with_timezone(&chrono::Utc);
        let start = Local
            .with_ymd_and_hms(2026, 5, 21, 12, 30, 0)
            .unwrap()
            .with_timezone(&chrono::Utc);
        let last = Local
            .with_ymd_and_hms(2026, 5, 21, 15, 45, 0)
            .unwrap()
            .with_timezone(&chrono::Utc);
        let s = super::format_session_range(start, last, now_local);
        assert_eq!(s, "[12:30–15:45]");
    }

    #[test]
    fn format_session_range_past_same_day_keeps_one_date() {
        use chrono::{Local, TimeZone};
        let now_local = Local
            .with_ymd_and_hms(2026, 5, 21, 18, 0, 0)
            .unwrap()
            .with_timezone(&chrono::Utc);
        let start = Local
            .with_ymd_and_hms(2026, 5, 19, 9, 0, 0)
            .unwrap()
            .with_timezone(&chrono::Utc);
        let last = Local
            .with_ymd_and_hms(2026, 5, 19, 11, 30, 0)
            .unwrap()
            .with_timezone(&chrono::Utc);
        let s = super::format_session_range(start, last, now_local);
        assert_eq!(s, "[05-19 09:00–11:30]");
    }

    #[test]
    fn live_session_last_activity_prefers_meta_last_over_pid_heartbeat() {
        use chrono::TimeZone;
        use std::path::PathBuf;
        // pid file's `updated_at` is recent (process re-activated) while
        // JSONL's last conversation entry predates it by days. The age
        // column must reflect the conversation, not the heartbeat.
        let jsonl = PathBuf::from("/tmp/test-83297480.jsonl");
        let conv_last = chrono::Utc
            .with_ymd_and_hms(2026, 5, 19, 10, 50, 0)
            .unwrap();
        let heartbeat = chrono::Utc.with_ymd_and_hms(2026, 5, 21, 8, 32, 0).unwrap();

        let mut state = create_test_state();
        state.original_daily_groups.clear();
        let mut sess = crate::test_helpers::helpers::make_session("tmp", None, Some("main"));
        sess.file_path = jsonl.clone();
        sess.day_last_timestamp = conv_last;
        state
            .original_daily_groups
            .push(crate::aggregator::DailyGroup {
                date: chrono::NaiveDate::from_ymd_opt(2026, 5, 19).unwrap(), // lint-ok: date-literal
                sessions: vec![sess],
            });

        let live = crate::infrastructure::live_sessions::LiveSession {
            session_id: "83297480".to_string(),
            jsonl_path: Some(jsonl),
            cwd: PathBuf::from("/tmp"),
            name: None,
            status: None,
            pid: 0,
            started_at: None,
            updated_at: Some(heartbeat),
            jsonl_mtime: Some(heartbeat),
            is_live: true,
            was_recently_live: false,
        };

        let now = chrono::Utc.with_ymd_and_hms(2026, 5, 22, 0, 0, 0).unwrap();
        let resolved = super::live_session_last_activity(&state, &live, now);
        assert_eq!(
            resolved, conv_last,
            "meta.day_last_timestamp must beat updated_at / jsonl_mtime"
        );
    }

    #[test]
    fn format_session_range_cross_day_shows_both_dates() {
        use chrono::{Local, TimeZone};
        let now_local = Local
            .with_ymd_and_hms(2026, 5, 21, 18, 0, 0)
            .unwrap()
            .with_timezone(&chrono::Utc);
        let start = Local
            .with_ymd_and_hms(2026, 5, 20, 20, 0, 0)
            .unwrap()
            .with_timezone(&chrono::Utc);
        let last = Local
            .with_ymd_and_hms(2026, 5, 21, 10, 0, 0)
            .unwrap()
            .with_timezone(&chrono::Utc);
        let s = super::format_session_range(start, last, now_local);
        assert_eq!(s, "[05-20 20:00–05-21 10:00]");
    }

    #[test]
    fn test_shorten_model_name_opus() {
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-opus-4-5-20251101"),
            "Opus 4.5"
        );
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-opus-4-1-20250805"),
            "Opus 4.1"
        );
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-opus-4-20250514"),
            "Opus 4"
        );
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-3-opus-20240229"),
            "Opus 3"
        );
    }

    #[test]
    fn test_shorten_model_name_sonnet() {
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-sonnet-4-5-20250929"),
            "Sonnet 4.5"
        );
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-sonnet-4-20250514"),
            "Sonnet 4"
        );
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-3-5-sonnet-20241022"),
            "Sonnet 3.5"
        );
    }

    #[test]
    fn test_shorten_model_name_haiku() {
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-haiku-4-5-20251001"),
            "Haiku 4.5"
        );
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-3-5-haiku-20241022"),
            "Haiku 3.5"
        );
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-3-haiku-20240307"),
            "Haiku 3"
        );
    }

    #[test]
    fn test_shorten_model_name_fallback_keeps_raw() {
        // Unknown family models keep their raw name so the UI can list
        // them individually with a "no pricing" badge.
        assert_eq!(
            crate::aggregator::normalize_model_name("unknown"),
            "unknown"
        );
        assert_eq!(
            crate::aggregator::normalize_model_name("some-future-model"),
            "some-future-model"
        );
        // Empty input still falls back to literal "unknown".
        assert_eq!(crate::aggregator::normalize_model_name(""), "unknown");
    }

    #[test]
    fn test_shorten_model_name_new_versions() {
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-opus-4-6-20260101"),
            "Opus 4.6"
        );
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-sonnet-5-20260101"),
            "Sonnet 5"
        );
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-haiku-5-1-20260101"),
            "Haiku 5.1"
        );
    }

    fn conv_msg(role: &str, blocks: Vec<ConversationBlock>) -> ConversationMessage {
        ConversationMessage {
            role: role.to_string(),
            blocks,
            timestamp: Some("10:00".to_string()),
            model: None,
            tokens: None,
            timestamp_utc: None,
            usage: None,
        }
    }

    // `H/L` steps the Day list only; advertising it over a Live or Pinned list
    // names a key that does nothing there.
    #[test]
    fn the_conversation_footer_offers_day_stepping_only_over_the_day_list() {
        let mut state = create_test_state();
        state.show_conversation = true;
        state.conv_list_mode = crate::ConvListMode::Day;
        let footer = |state: &mut AppState| {
            render_to_text(state, 140, 45)
                .lines()
                .last()
                .unwrap_or_default()
                .to_string()
        };
        assert!(footer(&mut state).contains("H/L:day"));
        state.conv_list_mode = crate::ConvListMode::Live;
        assert!(
            !footer(&mut state).contains("H/L:day"),
            "dead key advertised"
        );
    }

    #[test]
    fn the_pane_header_names_its_day_and_adds_the_all_days_total_when_continued() {
        use crate::test_helpers::helpers::{make_daily_group, make_session, make_test_app_state};
        let today = chrono::Local::now().date_naive();
        let mut newest = make_session("proj", Some("continued"), Some("main"));
        newest.day_first_timestamp = chrono::Utc::now();
        newest.day_last_timestamp = newest.day_first_timestamp;
        let mut older = newest.clone();
        older.day_first_timestamp -= chrono::Duration::days(1);
        let single = make_session("proj", Some("single"), Some("topic"));
        let groups = vec![
            make_daily_group(today, vec![newest.clone(), single.clone()]),
            make_daily_group(today - chrono::Duration::days(1), vec![older]),
        ];
        let header = |path: &std::path::Path| {
            let mut state = make_test_app_state(groups.clone());
            state.show_conversation = true;
            state.active_pane_index = Some(0);
            state.panes = vec![crate::ConversationPane {
                file_path: Some(path.to_path_buf()),
                messages: std::sync::Arc::new(vec![conv_msg(
                    "user",
                    vec![ConversationBlock::Text("hello".into())],
                )]),
                ..Default::default()
            }];
            render_to_text(&mut state, 140, 45)
        };
        let text = header(&newest.file_path);
        assert!(text.contains(" today "), "day scope missing: {text}");
        assert!(text.contains("Σ2d"), "all-days total missing: {text}");
        assert!(
            !header(&single.file_path).contains('Σ'),
            "single-day session"
        );
    }

    #[test]
    fn a_seeded_search_starts_on_the_day_the_hit_was_written() {
        let mut state = create_test_state();
        state.show_conversation = true;
        state.active_pane_index = Some(0);
        let now = chrono::Utc::now();
        let at = |days_back: i64, text: &str| {
            let mut m = conv_msg("user", vec![ConversationBlock::Text(text.into())]);
            m.timestamp_utc = Some(now - chrono::Duration::days(days_back));
            m
        };
        let mut pane = crate::ConversationPane {
            messages: std::sync::Arc::new(vec![
                at(5, "old target"),
                at(5, "old target again"),
                at(0, "fresh target"),
            ]),
            ..Default::default()
        };
        pane.search_mode = true;
        pane.search_input.set("target".to_string());
        pane.pending_search_scroll = true;
        pane.search_seed_day = Some(now.with_timezone(&chrono::Local).date_naive());
        state.panes = vec![pane];
        let _ = render_to_text(&mut state, 120, 35);
        let pane = &state.panes[0];
        assert_eq!(pane.search_matches[pane.search_current].0, 2);
        assert_eq!(pane.search_seed_day, None, "the seed applies once");
    }

    #[test]
    fn reply_latency_counts_from_an_injected_note_not_the_last_typed_prompt() {
        let mut state = create_test_state();
        state.show_conversation = true;
        state.active_pane_index = Some(0);
        let t0 = chrono::Utc::now() - chrono::Duration::days(1);
        let at = |role: &str, secs: i64, text: &str| {
            let mut m = conv_msg(role, vec![ConversationBlock::Text(text.into())]);
            m.timestamp_utc = Some(t0 + chrono::Duration::seconds(secs));
            m
        };
        let resumed = 23 * 3600;
        let mut pane = crate::ConversationPane {
            messages: std::sync::Arc::new(vec![
                at("user", 0, "typed"),
                at("meta", resumed, "Continue from where you left off."),
                at("assistant", resumed + 5, "reply"),
            ]),
            ..Default::default()
        };
        pane.compact = false;
        state.panes = vec![pane];
        let text = render_to_text(&mut state, 140, 45);
        assert!(text.contains("Δ5s"), "{text}");
        assert!(!text.contains("Δ23h"), "{text}");
    }

    #[test]
    fn seeded_async_load_peeks_and_centers_match() {
        let mut state = create_test_state();
        state.show_conversation = true;
        state.active_pane_index = Some(0);
        // Simulate the popup-seed path: pane exists but messages arrive later.
        let mut pane = crate::ConversationPane::default();
        pane.search_mode = true;
        pane.search_input.set("target".to_string());
        pane.pending_search_scroll = true;
        pane.scroll = usize::MAX;
        state.panes = vec![pane];
        // Draw once BEFORE messages arrive (loading frame).
        let _ = render_to_text(&mut state, 120, 35);
        // Messages land (async load completion) — mirror poll_pane_loads'
        // first-load branch exactly. A long tail after the match ensures a
        // stale end-of-transcript cursor can't drag the viewport back.
        let mut msgs = vec![
            conv_msg("user", vec![ConversationBlock::Text("intro".into())]),
            conv_msg(
                "assistant",
                vec![ConversationBlock::Text("has target inside".into())],
            ),
        ];
        for i in 0..60 {
            msgs.push(conv_msg(
                "user",
                vec![ConversationBlock::Text(format!("tail {i}"))],
            ));
        }
        state.panes[0].messages = std::sync::Arc::new(msgs);
        state.panes[0].rendered = None;
        state.panes[0].search_matches.clear();
        state.panes[0].search_current = 0;
        state.panes[0].search_saved_scroll = None;
        state.panes[0].scroll = usize::MAX;
        state.panes[0].selected_message = usize::MAX;
        let _ = render_to_text(&mut state, 120, 35);
        let pane = &state.panes[0];
        assert_eq!(
            pane.peek_expanded,
            Some(1),
            "peek after async load; matches={:?} pending={}",
            pane.search_matches,
            pane.pending_search_scroll
        );
        assert_eq!(
            pane.message_lines
                .get(pane.selected_message)
                .map(|&(_, m)| m),
            Some(1),
            "cursor lands on the matched message"
        );
        assert!(
            pane.scroll < 30,
            "viewport centered on the match near the top, not the tail: scroll={}",
            pane.scroll
        );
    }

    // The whole surface × size matrix must render without panicking:
    // small areas are where unsaturated layout arithmetic hides (lints
    // #5/#22 catch the grep-able shapes; this covers everything else).
    // Sizes: the two documented test sizes, the narrow reference, and a
    // pathological minimum.
    #[test]
    fn every_tab_and_popup_renders_at_every_size() {
        let sizes: [(u16, u16); 4] = [(140, 45), (120, 35), (60, 20), (40, 10)];
        let tabs = [
            crate::Tab::Dashboard,
            crate::Tab::Live,
            crate::Tab::Daily,
            crate::Tab::Insights,
        ];
        let popups: Vec<crate::ActivePopup> = vec![
            crate::ActivePopup::None,
            crate::ActivePopup::Help { scroll: 0 },
            crate::ActivePopup::Detail,
            crate::ActivePopup::Summary { scroll: 0 },
            crate::ActivePopup::InsightsDetail { scroll: 0 },
            crate::ActivePopup::DashboardDetail,
            crate::ActivePopup::ProjectDetail {
                path: "~/proj".to_string(),
                scroll: 0,
            },
            crate::ActivePopup::FilterPopup {
                selected: 0,
                input_mode: false,
                input: crate::TextInput::default(),
                input_error: false,
            },
            crate::ActivePopup::ProjectPopup {
                selected: 0,
                scroll: 0,
            },
            crate::ActivePopup::TitleEdit {
                input: crate::TextInput::default(),
                path: std::path::PathBuf::from("/tmp/x.jsonl"),
                return_to: crate::TitleEditReturn::Root,
            },
        ];
        for &(w, h) in &sizes {
            for tab in tabs {
                for popup in &popups {
                    let mut state = create_test_state();
                    state.tab = tab;
                    state.active_popup = popup.clone();
                    let _ = render_to_text(&mut state, w, h);
                }
            }
        }
    }

    #[test]
    fn title_edit_popup_registers_its_click_area() {
        // `handle_mouse_click` treats a missing `active_popup_area` as
        // "clicked outside" and dismisses — an unregistered editor would
        // die on ANY click, dropping the typed text.
        let mut state = create_test_state();
        state.active_popup = crate::ActivePopup::TitleEdit {
            input: crate::TextInput::default(),
            path: std::path::PathBuf::from("/tmp/x.jsonl"),
            return_to: crate::TitleEditReturn::Root,
        };
        let _ = render_to_text(&mut state, 120, 35);
        assert!(
            state.layout.active_popup_area.is_some(),
            "title editor must register its popup area for click hit-testing"
        );
    }

    #[test]
    fn pane_search_peek_opens_tool_group_head_for_inner_match() {
        // Tool-only messages fold into one compact row keyed by the run's
        // FIRST message; a match inside a later member must expand the head
        // or the row never opens.
        let mut state = create_test_state();
        state.show_conversation = true;
        state.active_pane_index = Some(0);
        let msgs = vec![
            conv_msg("user", vec![ConversationBlock::Text("intro".into())]),
            conv_msg(
                "assistant",
                vec![ConversationBlock::ToolUse {
                    name: "Read".into(),
                    input_summary: "src/lib.rs".into(),
                }],
            ),
            conv_msg(
                "user",
                vec![ConversationBlock::ToolResult {
                    content: "command not found: expected binary".into(),
                    is_error: true,
                }],
            ),
            conv_msg("assistant", vec![ConversationBlock::Text("done".into())]),
        ];
        let mut pane = crate::ConversationPane {
            messages: std::sync::Arc::new(msgs),
            ..Default::default()
        };
        pane.search_mode = true;
        pane.search_input.set("expected binary".to_string());
        pane.pending_search_scroll = true;
        state.panes = vec![pane];
        let _ = render_to_text(&mut state, 120, 35);
        let pane = &state.panes[0];
        assert_eq!(
            pane.search_matches.as_slice(),
            &[(2, 0)],
            "match sits on the second member of the tool run"
        );
        assert_eq!(
            pane.peek_expanded,
            Some(1),
            "peek keys the tool-run head, not the matched member"
        );
        assert!(pane.expanded.contains(&1));
        // The result CONTENT renders in the expanded group (`↳ …`), so the
        // matched text is visible and the current-match highlight has a
        // rendered occurrence to land on.
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("command not found: expected binary"),
            "tool-result content must render when the group is peeked: {text}"
        );
    }

    #[test]
    fn pane_search_peek_expands_only_current_match_message() {
        let mut state = create_test_state();
        state.show_conversation = true;
        state.active_pane_index = Some(0);
        let msgs = vec![
            conv_msg("user", vec![ConversationBlock::Text("intro line".into())]),
            conv_msg(
                "assistant",
                vec![ConversationBlock::Text(format!(
                    "{} target here",
                    "filler ".repeat(30)
                ))],
            ),
            conv_msg(
                "user",
                vec![ConversationBlock::Text("closing target".into())],
            ),
        ];
        let mut pane = crate::ConversationPane {
            messages: std::sync::Arc::new(msgs),
            ..Default::default()
        };
        pane.search_mode = true;
        pane.search_input.set("target".to_string());
        pane.pending_search_scroll = true;
        state.panes = vec![pane];

        let _ = render_to_text(&mut state, 120, 35);
        {
            let pane = &state.panes[0];
            assert_eq!(pane.peek_expanded, Some(1), "current match auto-expands");
            assert!(pane.expanded.contains(&1));
            assert_eq!(
                pane.message_lines
                    .get(pane.selected_message)
                    .map(|&(_, m)| m),
                Some(1),
                "cursor lands on the matched message"
            );
        }

        // Next match (message 2): the peek migrates, message 1 folds back.
        state.panes[0].search_current = 1;
        state.panes[0].pending_search_scroll = true;
        let _ = render_to_text(&mut state, 120, 35);
        let pane = &state.panes[0];
        assert_eq!(pane.peek_expanded, Some(2));
        assert!(!pane.expanded.contains(&1), "auto-peek folds back on move");
        assert!(pane.expanded.contains(&2));
    }

    #[test]
    fn pane_search_bar_carries_counter_and_key_hints() {
        let mut state = create_test_state();
        state.show_conversation = true;
        state.active_pane_index = Some(0);
        let msgs = vec![
            conv_msg("user", vec![ConversationBlock::Text("beta one".into())]),
            conv_msg(
                "assistant",
                vec![ConversationBlock::Text("beta two beta".into())],
            ),
        ];
        let mut pane = crate::ConversationPane {
            messages: std::sync::Arc::new(msgs),
            ..Default::default()
        };
        pane.search_mode = true;
        pane.search_input.set("beta".to_string());
        state.panes = vec![pane];
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("1/3"),
            "occurrence counter in the bar: {text}"
        );
        assert!(text.contains("Esc: close"), "key hints in the bar: {text}");
    }

    #[test]
    fn compact_lines_one_per_message_until_expanded() {
        let long = "The 5m and 1h cache write split was being dropped in the \
                    per-model fold so calculate_cost reported zero cache-write cost"
            .to_string();
        let messages = vec![
            conv_msg("user", vec![ConversationBlock::Text("fix the bug".into())]),
            conv_msg("assistant", vec![ConversationBlock::Text(long.clone())]),
            conv_msg(
                "assistant",
                vec![ConversationBlock::ToolUse {
                    name: "Edit".into(),
                    input_summary: "aggregator/stats.rs".into(),
                }],
            ),
        ];
        let expanded = std::collections::HashSet::new();

        // Collapsed: exactly one line per message (3) + nothing else.
        let (lines, positions, _) = render_compact_lines(&messages, 80, &expanded);
        assert_eq!(positions.len(), 3, "one position per message");
        assert_eq!(lines.len(), 3, "collapsed compact = one line each");
        let row0: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            row0.starts_with('▸'),
            "collapsed caret leads the row: {row0:?}"
        );
        assert!(row0.contains("You") && row0.contains("fix the bug"));
        // The long assistant message is truncated to one line (ends with …).
        let row1: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            row1.contains('…'),
            "long message collapsed + truncated: {row1:?}"
        );
        let row2: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(row2.contains("⚙ Edit"), "tool message summary: {row2:?}");

        // Expand message 1 → its full text wraps over several extra lines.
        let mut expanded = std::collections::HashSet::new();
        expanded.insert(1);
        let (lines2, _, _) = render_compact_lines(&messages, 80, &expanded);
        assert!(
            lines2.len() > 3,
            "expanded message must add body lines: {} lines",
            lines2.len()
        );
        let joined: String = lines2
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            joined.contains("calculate_cost reported zero"),
            "expanded body must show the full text"
        );
    }

    #[test]
    fn reposition_scroll_aligns_by_message_height_and_read_direction() {
        use super::reposition_scroll_for_selection as r;
        let vh = 10;
        // Already partly on screen → never moves (decoupled cursor/viewport).
        assert_eq!(r(5, vh, 3, 8), 5, "fully in view stays put");
        assert_eq!(
            r(10, vh, 5, 30),
            10,
            "tall msg straddling the fold stays put"
        );
        // Entirely below the fold.
        assert_eq!(
            r(0, vh, 20, 60),
            20,
            "tall below → top-align (read from start)"
        );
        assert_eq!(
            r(0, vh, 20, 25),
            15,
            "short below → bottom-align just into view"
        );
        // Entirely above the fold.
        assert_eq!(
            r(50, vh, 0, 40),
            30,
            "tall above → bottom-align (resume upward)"
        );
        assert_eq!(r(50, vh, 40, 45), 40, "short above → top-align");
    }

    #[test]
    fn compact_folds_tool_result_into_use_row() {
        // A tool-use message followed by its result message must render as ONE
        // row (⚙ + status icon), not two — the result line is folded away.
        let messages = vec![
            conv_msg(
                "assistant",
                vec![ConversationBlock::ToolUse {
                    name: "Edit".into(),
                    input_summary: "src/state.rs".into(),
                }],
            ),
            conv_msg(
                "user",
                vec![ConversationBlock::ToolResult {
                    content: "The file has been updated successfully.".into(),
                    is_error: false,
                }],
            ),
        ];
        let expanded = std::collections::HashSet::new();
        let (lines, positions, _) = render_compact_lines(&messages, 80, &expanded);
        assert_eq!(positions.len(), 1, "result message folded out of the list");
        assert_eq!(lines.len(), 1, "use + result collapse to one row");
        let row: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(row.contains("⚙ Edit") && row.contains('✓'), "row: {row:?}");
        assert!(
            !row.contains("updated successfully"),
            "result content must not appear on the collapsed row: {row:?}"
        );
    }

    #[test]
    fn compact_groups_consecutive_tool_calls() {
        // A run of tool calls collapses to one counted row; expanding shows
        // each call. An error anywhere flips the group status to ✗.
        let tool = |name: &str, arg: &str| {
            conv_msg(
                "assistant",
                vec![ConversationBlock::ToolUse {
                    name: name.into(),
                    input_summary: arg.into(),
                }],
            )
        };
        let result = |err: bool| {
            conv_msg(
                "user",
                vec![ConversationBlock::ToolResult {
                    content: "out".into(),
                    is_error: err,
                }],
            )
        };
        let messages = vec![
            tool("Read", "a.rs"),
            result(false),
            tool("Read", "b.rs"),
            result(false),
            tool("Edit", "a.rs"),
            result(true),
        ];
        let expanded = std::collections::HashSet::new();
        let (lines, positions, _) = render_compact_lines(&messages, 100, &expanded);
        assert_eq!(positions.len(), 1, "the whole run is one row");
        assert_eq!(lines.len(), 1);
        let row: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            row.contains("3 tools") && row.contains("Read×2"),
            "row: {row:?}"
        );
        assert!(
            row.contains("Edit") && row.contains('✗'),
            "any error → ✗: {row:?}"
        );

        // Expand → one detail line per call (3) + header + blank.
        let mut expanded = std::collections::HashSet::new();
        expanded.insert(0);
        let (lines2, _, _) = render_compact_lines(&messages, 100, &expanded);
        let detail = lines2
            .iter()
            .filter(|l| {
                l.spans
                    .iter()
                    .any(|s| s.content.contains("⚙") && s.content.contains(".rs"))
            })
            .count();
        assert_eq!(detail, 3, "expanded group shows each call");
    }

    // message judgment function tests
    #[test]
    fn test_is_tool_only_message_true() {
        let msg = ConversationMessage {
            role: "assistant".to_string(),
            blocks: vec![
                ConversationBlock::ToolUse {
                    name: "Read".to_string(),
                    input_summary: "/path".to_string(),
                },
                ConversationBlock::ToolResult {
                    content: "content".to_string(),
                    is_error: false,
                },
            ],
            timestamp: None,
            model: None,
            tokens: None,
            timestamp_utc: None,
            usage: None,
        };
        assert!(is_tool_only_message(&msg));
    }

    #[test]
    fn test_is_tool_only_message_false() {
        let msg = ConversationMessage {
            role: "assistant".to_string(),
            blocks: vec![
                ConversationBlock::Text("Hello".to_string()),
                ConversationBlock::ToolUse {
                    name: "Read".to_string(),
                    input_summary: "/path".to_string(),
                },
            ],
            timestamp: None,
            model: None,
            tokens: None,
            timestamp_utc: None,
            usage: None,
        };
        assert!(!is_tool_only_message(&msg));
    }

    #[test]
    fn test_is_thinking_only_message_true() {
        let msg = ConversationMessage {
            role: "assistant".to_string(),
            blocks: vec![ConversationBlock::Thinking("thinking...".to_string())],
            timestamp: None,
            model: None,
            tokens: None,
            timestamp_utc: None,
            usage: None,
        };
        assert!(is_thinking_only_message(&msg));
    }

    #[test]
    fn test_extract_message_text_from_text_block() {
        let msg = ConversationMessage {
            role: "assistant".to_string(),
            blocks: vec![ConversationBlock::Text("Hello world".to_string())],
            timestamp: None,
            model: None,
            tokens: None,
            timestamp_utc: None,
            usage: None,
        };
        assert_eq!(extract_message_text(&msg), "Hello world");
    }

    #[test]
    fn test_extract_message_text_from_tool_result() {
        let msg = ConversationMessage {
            role: "user".to_string(),
            blocks: vec![ConversationBlock::ToolResult {
                content: "result content".to_string(),
                is_error: false,
            }],
            timestamp: None,
            model: None,
            tokens: None,
            timestamp_utc: None,
            usage: None,
        };
        assert_eq!(extract_message_text(&msg), "result content");
    }

    #[test]
    fn test_extract_message_text_error_result() {
        let msg = ConversationMessage {
            role: "user".to_string(),
            blocks: vec![ConversationBlock::ToolResult {
                content: "error message".to_string(),
                is_error: true,
            }],
            timestamp: None,
            model: None,
            tokens: None,
            timestamp_utc: None,
            usage: None,
        };
        assert_eq!(extract_message_text(&msg), "[Error] error message");
    }

    // theme function tests
    #[test]
    fn test_model_color_opus() {
        assert_eq!(model_color("claude-opus-4-5"), theme::MODEL_OPUS);
        assert_eq!(model_color("opus"), theme::MODEL_OPUS);
    }

    #[test]
    fn text_roles_keep_their_brightness_order() {
        let luma = |c: Color| match c {
            Color::Rgb(r, g, b) => u32::from(r) + u32::from(g) + u32::from(b), // lint-ok: raw-rgb — a pattern, not a color
            _ => panic!("theme colors are RGB"),
        };
        // Unread filler < secondary figures < labels < values < titles: a value
        // that renders darker than its own label reads as the less important.
        let ladder = [
            theme::FAINT,
            theme::META,
            theme::LABEL,
            theme::VALUE,
            theme::TITLE,
        ];
        for pair in ladder.windows(2) {
            assert!(
                luma(pair[0]) < luma(pair[1]),
                "{:?} !< {:?}",
                pair[0],
                pair[1]
            );
        }
        assert_eq!(theme::TIME, theme::META);
        assert_eq!(theme::SESSION_ID, theme::META);
        assert_eq!(theme::PLACEHOLDER, theme::LABEL);
    }

    #[test]
    fn model_color_accepts_normalized_display_names() {
        // The project-detail popup keys its model map by `normalize_model_name`,
        // so the colour lookup sees "Opus 5", not the raw id.
        for raw in ["claude-opus-5", "claude-sonnet-4-6", "claude-haiku-4-5"] {
            let display = crate::aggregator::normalize_model_name(raw);
            assert_eq!(
                model_color(&display),
                model_color(raw),
                "{display} must colour like {raw}"
            );
            assert_ne!(
                model_color(&display),
                theme::LABEL,
                "{display} fell through to the unpriced/unknown colour"
            );
        }
    }

    #[test]
    fn test_model_color_sonnet() {
        assert_eq!(model_color("claude-sonnet-4"), theme::MODEL_SONNET);
        assert_eq!(model_color("sonnet"), theme::MODEL_SONNET);
        for m in [
            "claude-fable-5-1",
            "Fable 5",
            "claude-mythos-5-1",
            "Mythos 5",
        ] {
            assert_eq!(model_color(m), theme::MODEL_FABLE, "{m}");
        }
    }

    #[test]
    fn test_model_color_haiku() {
        assert_eq!(model_color("claude-haiku-4"), theme::MODEL_HAIKU);
        assert_eq!(model_color("haiku"), theme::MODEL_HAIKU);
    }

    #[test]
    fn test_model_color_unknown() {
        assert_eq!(model_color("unknown-model"), theme::LABEL);
    }

    #[test]
    fn test_cost_style_critical() {
        assert_eq!(cost_style(500.0).fg, Some(theme::CRITICAL));
    }

    #[test]
    fn test_cost_style_danger() {
        assert_eq!(cost_style(200.0).fg, Some(theme::DANGER));
    }

    #[test]
    fn test_cost_style_error() {
        assert_eq!(cost_style(80.0).fg, Some(theme::ERROR));
    }

    #[test]
    fn test_cost_style_warning() {
        assert_eq!(cost_style(30.0).fg, Some(theme::WARNING));
    }

    #[test]
    fn test_cost_style_success() {
        assert_eq!(cost_style(10.0).fg, Some(theme::SUCCESS));
    }

    // The badge and the Daily tab's own `Sessions (n/N)` header describe the
    // same day, so a subagent-inclusive badge reads as a different day's
    // figure. Rendered, not computed from the fixture: the point is what the
    // tab bar prints.
    #[test]
    fn daily_tab_badge_counts_the_same_sessions_the_daily_header_does() {
        use crate::test_helpers::helpers::{make_daily_group, make_session_with_tokens};
        let today = chrono::Local::now().date_naive();
        let user = make_session_with_tokens("proj", 1, 1, "claude-opus-5");
        let mut subs = vec![user];
        for i in 0..4 {
            let mut sub = make_session_with_tokens("proj", 1, 1, "claude-opus-5");
            sub.is_subagent = true;
            sub.file_path = std::path::PathBuf::from(format!("/s/sub{i}.jsonl"));
            subs.push(sub);
        }
        let mut state =
            crate::test_helpers::helpers::make_test_app_state(vec![make_daily_group(today, subs)]);

        let text = render_to_text(&mut state, 140, 45);
        let bar = text.lines().nth(1).unwrap_or_default().to_string();
        assert!(
            bar.contains("Daily (1)"),
            "badge should show the 1 user session, not the 5 rows: {bar:?}"
        );
    }

    // The chip occupies the `f:Filter` slot, so it has to carry the key.
    #[test]
    fn the_filter_chip_keeps_the_key_while_a_filter_is_active() {
        let mut state = create_test_state();
        let today = chrono::Local::now().date_naive();
        for filter in [
            crate::PeriodFilter::All,
            crate::PeriodFilter::Today,
            crate::PeriodFilter::Custom(today, Some(today)),
        ] {
            state.period_filter = filter;
            let text = render_to_text(&mut state, 140, 45);
            let bar = text.lines().nth(1).unwrap_or_default().to_string();
            assert!(
                bar.contains("f:"),
                "no way back to the filter popup with {filter:?}: {bar:?}"
            );
        }
    }

    // The AI summary prompt asks for markdown, so the popup renders it. Showing
    // the source put `##` and `**` in front of the reader.
    #[test]
    fn summary_lines_render_the_markdown_the_prompt_asks_for() {
        let text = "## Goal\nship it\n- first item\n* second item\n**none** recorded\nplain";
        let lines = super::summary_lines(text);
        let flat: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect();

        assert_eq!(flat[0], "Goal", "the `##` marker is consumed");
        assert_eq!(flat[1], "ship it");
        assert_eq!(flat[2], "• first item", "`-` becomes a bullet");
        assert_eq!(flat[3], "• second item", "`*` becomes a bullet too");
        assert_eq!(flat[4], "none recorded", "the `**` markers are consumed");
        assert_eq!(flat[5], "plain");

        // The heading and the bold run have to actually be bold, or consuming
        // the markers would just delete the emphasis.
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD)),
            "heading is not bold"
        );
        assert!(
            lines[4]
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD)),
            "bold run is not bold"
        );
    }

    // A generation cut mid-emphasis must not restyle the rest of the line.
    #[test]
    fn summary_lines_leave_an_unmatched_marker_literal() {
        let lines = super::summary_lines("**cut off here");
        let flat: String = lines[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(flat, "**cut off here");
        assert!(
            !lines[0]
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD)),
            "an unmatched marker must not bold anything"
        );
    }

    // Four zeroes read the same whether the tool is broken or the machine has
    // never run Claude Code. The guidance must appear only in the empty case —
    // a caption on every launch would be noise for everyone else.
    #[test]
    fn the_overview_names_the_search_path_only_when_nothing_was_found() {
        use crate::test_helpers::helpers::{make_daily_group, make_session_with_tokens};

        let mut empty = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        empty.loading = false;
        let text = render_to_text(&mut empty, 140, 45);
        assert!(text.contains("No Claude Code sessions found"), "{text}");
        assert!(
            text.contains("~/.claude/projects"),
            "the path has to be named"
        );
        // The panel's inner height is two; a third line would be clipped away.
        assert_eq!(
            super::dashboard::empty_dashboard_lines().len(),
            2,
            "the Overview panel only has two inner rows"
        );

        let today = chrono::Local::now().date_naive();
        let mut filled = crate::test_helpers::helpers::make_test_app_state(vec![make_daily_group(
            today,
            vec![make_session_with_tokens("proj", 10, 10, "claude-opus-5")],
        )]);
        filled.loading = false;
        let text = render_to_text(&mut filled, 140, 45);
        assert!(
            !text.contains("No Claude Code sessions found"),
            "guidance leaked into a populated dashboard"
        );
        assert!(text.contains("active days"), "the cards still render");

        // A filter matching nothing empties the view, not the disk.
        filled.daily_groups.clear();
        let text = render_to_text(&mut filled, 140, 45);
        assert!(
            !text.contains("No Claude Code sessions found"),
            "an empty filter result was reported as a missing install"
        );
    }

    #[test]
    fn breakdown_bar_never_vanishes_for_a_non_zero_share() {
        use super::breakdown_bar_len;
        assert_eq!(breakdown_bar_len(0.0), 0, "a zero share draws nothing");
        for pct in [0.01f64, 1.0, 13.0, 16.6] {
            assert!(
                breakdown_bar_len(pct) >= 1,
                "{pct}% rounded away to a blank row"
            );
        }
        assert_eq!(breakdown_bar_len(100.0), 3, "a full share fills the column");
    }

    // Key dispatch is exclusive, so a tab footer advertising `←→` / `Enter`
    // while a modal is up names keys that cannot fire. Every blocking variant
    // has to collapse the bar, and every non-blocking one has to leave it.
    #[test]
    fn tab_footer_collapses_for_every_blocking_popup() {
        use crate::ActivePopup;
        let pairs = [("?", "help"), ("q", "quit"), ("←→", "panel")];
        let full = help_bar(&pairs).spans.len();
        let mut state = create_test_state();

        // Every blocking variant, so making one non-blocking by mistake fails
        // here instead of shipping a footer that lies on that popup.
        for popup in [
            ActivePopup::Help { scroll: 0 },
            ActivePopup::ProjectDetail {
                path: "p".into(),
                scroll: 0,
            },
            ActivePopup::Summary { scroll: 0 },
            ActivePopup::Detail,
            ActivePopup::DashboardDetail,
            ActivePopup::InsightsDetail { scroll: 0 },
            ActivePopup::TitleEdit {
                input: crate::state::TextInput::default(),
                path: std::path::PathBuf::from("p"),
                return_to: crate::state::TitleEditReturn::Root,
            },
        ] {
            state.active_popup = popup;
            let bar = tab_help_bar(&state, &pairs);
            let text: String = bar.spans.iter().map(|s| s.content.to_string()).collect();
            assert!(
                bar.spans.len() < full && text.contains("Esc"),
                "blocking popup kept the tab bar: {text:?}"
            );
        }

        for popup in [
            ActivePopup::None,
            ActivePopup::ProjectPopup {
                selected: 0,
                scroll: 0,
            },
        ] {
            state.active_popup = popup;
            assert_eq!(
                tab_help_bar(&state, &pairs).spans.len(),
                full,
                "non-blocking popup collapsed the tab bar"
            );
        }
    }

    fn create_test_state() -> crate::AppState {
        use crate::aggregator::{DailyGroup, SessionInfo, TokenStats};

        let today = chrono::Local::now().date_naive();
        let first_date = today - chrono::Duration::days(9); // 10 calendar days

        let mut hourly_work = std::collections::HashMap::new();
        hourly_work.insert(10u8, 3000u64);
        hourly_work.insert(11u8, 3000u64);

        let mut day_tokens_by_model = std::collections::HashMap::new();
        day_tokens_by_model.insert(
            "claude-sonnet-4-20250514".to_string(),
            crate::aggregator::ModelTokens {
                input_tokens: 4000,
                output_tokens: 800,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                cache_creation_5m_tokens: 0,
                cache_creation_1h_tokens: 0,
                non_standard_speed: false,
            },
        );

        let mut day_tool_usage = std::collections::HashMap::new();
        day_tool_usage.insert("Bash".to_string(), 5);

        let mut day_language_usage = std::collections::HashMap::new();
        day_language_usage.insert("Rust".to_string(), 10);
        day_language_usage.insert("TypeScript".to_string(), 5);

        let mut day_extension_usage = std::collections::HashMap::new();
        day_extension_usage.insert("rs".to_string(), 10);
        day_extension_usage.insert("ts".to_string(), 3);
        day_extension_usage.insert("tsx".to_string(), 2);

        let session = SessionInfo {
            verified_cwd: None,
            file_path: std::path::PathBuf::from("/tmp/test.jsonl"),
            project_name: "test-project".to_string(),
            git_branch: None,
            session_first_timestamp: chrono::Utc::now() - chrono::Duration::hours(1),
            model: Some("claude-sonnet-4-20250514".to_string()),
            day_input_tokens: 5000,
            day_output_tokens: 1000,
            day_user_msgs: 0,
            day_prompts: 0,
            day_assistant_msgs: 0,
            day_tokens_by_model,
            day_hourly_activity: std::collections::HashMap::new(),
            day_hourly_work_tokens: hourly_work,
            day_tool_usage,
            day_language_usage,
            day_extension_usage,
            day_first_timestamp: chrono::Utc::now() - chrono::Duration::hours(1),
            day_last_timestamp: chrono::Utc::now(),
            summary: None,
            custom_title: None,
            ai_title: None,
            last_user_message: None,
            first_user_message: None,
            is_subagent: false,
            parent_path: None,
            is_continued: false,
            line_churn: Default::default(),
        };

        let group = DailyGroup {
            date: today,
            sessions: vec![session],
        };

        let past_group = DailyGroup {
            date: first_date,
            sessions: vec![],
        };

        let mut stats = crate::aggregator::Stats::default();
        stats.total_tokens = TokenStats {
            input_tokens: 50000,
            output_tokens: 10000,
            cache_creation_tokens: 0,
            cache_read_tokens: 40000,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            non_standard_speed: false,
        };
        stats.tool_success_count = 90;
        stats.tool_error_count = 10;
        stats.total_sessions_count = 10;
        stats.tool_usage.insert("Bash".to_string(), 50);
        stats.tool_usage.insert("Read".to_string(), 30);
        stats.language_usage.insert("Rust".to_string(), 120);
        stats.language_usage.insert("TypeScript".to_string(), 85);
        stats.language_usage.insert("Other".to_string(), 30);
        stats.extension_usage.insert("rs".to_string(), 120);
        stats.extension_usage.insert("ts".to_string(), 60);
        stats.extension_usage.insert("tsx".to_string(), 25);
        stats.extension_usage.insert("example".to_string(), 15);
        stats.extension_usage.insert("xyz".to_string(), 10);
        stats.extension_usage.insert("abc".to_string(), 5);

        let mut aggregated_model_tokens = std::collections::HashMap::new();
        aggregated_model_tokens.insert(
            "Sonnet 4".to_string(),
            TokenStats {
                input_tokens: 40000,
                output_tokens: 10000,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                cache_creation_5m_tokens: 0,
                cache_creation_1h_tokens: 0,
                non_standard_speed: false,
            },
        );

        let daily_groups = vec![past_group, group];
        let daily_costs = vec![(today, 10.0), (first_date, 5.0)];
        let model_costs = vec![("Sonnet 4".to_string(), 100.0)];

        // Start from the shared fixture (every zero/empty/None field) and
        // override only the ~16 fields this Insights-render test cares about.
        // `make_test_app_state` is the single exhaustive non-production
        // AppState literal; new_initial remains the compiler-enforced field
        // parity site (state.rs invariant).
        let mut state = crate::test_helpers::helpers::make_test_app_state(daily_groups.clone());
        state.tab = crate::Tab::Insights;
        state.stats = stats.clone();
        state.total_cost = 100.0;
        state.model_costs = model_costs.clone();
        state.aggregated_model_tokens = aggregated_model_tokens.clone();
        state.daily_costs = daily_costs.clone();
        state.file_count = 10;
        state.data_limit = 50;
        state.project_list = vec![
            ("~/projects/app-a".to_string(), 50000, today),
            (
                "~/projects/other-project".to_string(),
                20000,
                today - chrono::Duration::days(3),
            ),
        ];
        state.original_daily_costs = daily_costs;
        state.original_stats = stats;
        state.original_total_cost = 100.0;
        state.original_model_costs = model_costs;
        state.original_aggregated_model_tokens = aggregated_model_tokens;
        state
    }

    fn render_to_text(state: &mut crate::AppState, width: u16, height: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, state)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    // Golden footers: the exact bottom-bar text per tab, in the documented
    // `key:action` single-space format. Written against the hand-rolled span
    // lists BEFORE the help_bar consolidation so the refactor is provably
    // byte-identical; afterwards they pin the helper's spacing rules.
    #[test]
    fn tab_footers_render_exact_golden_strings() {
        let cases: [(crate::Tab, &str); 3] = [
            (
                crate::Tab::Dashboard,
                " ?:help q:quit ←→:panel ↑↓:scroll Enter:detail /:search m:pins",
            ),
            (
                crate::Tab::Daily,
                " ?:help q:quit ←→:day ↑↓:session i:info Enter:view s:summary S:day-summary t:title b:breakdown /:search Space:pin m:pins",
            ),
            (
                crate::Tab::Insights,
                " ?:help q:quit ←→:panel Enter:detail /:search m:pins",
            ),
        ];
        for (tab, golden) in cases {
            let mut state = create_test_state();
            state.tab = tab;
            let text = render_to_text(&mut state, 140, 45);
            assert!(
                text.contains(golden),
                "footer golden mismatch for tab — expected {golden:?} in:\n{}",
                text.lines().rev().take(3).collect::<Vec<_>>().join("\n")
            );
        }
    }

    // The Live tab count is unknown until the first live poll returns; a
    // hardcoded 0 that flips to the real number a moment later reads as a
    // glitch, so the badge must stay hidden until the poll lands.
    #[test]
    fn live_tab_count_hidden_until_first_poll() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Dashboard;
        let text = render_to_text(&mut state, 140, 45);
        assert!(
            !text.contains("Live ("),
            "no count before the first live poll: {text}"
        );

        let mut state = create_test_state();
        state.tab = crate::Tab::Dashboard;
        state.live_last_update = Some(std::time::Instant::now());
        let text = render_to_text(&mut state, 140, 45);
        assert!(
            text.contains("Live (0)"),
            "count appears once the poll has landed: {text}"
        );
    }

    // A float cost can carry a tiny negative value; unclamped it renders
    // as the nonsensical "-0.00" in the block header segment.
    // A 6-digit language count in a 5-wide cell would be cut to its first
    // digits and read as a much smaller number; K/M formatting keeps the
    // magnitude visible (and the % column consistent with the count).
    #[test]
    fn languages_panel_formats_large_counts() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Dashboard;
        state.stats.language_usage.insert("C".to_string(), 132_500);
        let text = render_to_text(&mut state, 140, 45);
        assert!(
            !text.contains("13250"),
            "raw-cut 6-digit count must not render: {text}"
        );
        assert!(
            text.contains("132K") || text.contains("133K"),
            "count renders K-formatted: {text}"
        );
    }

    // The Activity panel's bottom border carries a left date label and a
    // right-aligned intensity legend; the legend must drop (not overlap the
    // date) when the panel can't fit both.
    #[test]
    fn activity_legend_shown_wide_dropped_narrow() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Dashboard;
        let wide = render_to_text(&mut state, 140, 45);
        assert!(wide.contains("Less"), "intensity legend at 140 cols");

        // 30 cols leaves the panel narrower than date + legend; the date
        // keeps the border line and the legend must drop, not overlap it.
        let mut state = create_test_state();
        state.tab = crate::Tab::Dashboard;
        let narrow = render_to_text(&mut state, 30, 20);
        assert!(
            !narrow.contains("Less"),
            "legend must drop when date + legend can't both fit"
        );
    }

    #[test]
    fn format_cost_marked_surfaces_unknown_pricing() {
        // Fully unknown → "$?", partially priced → lower bound + "*",
        // fully priced → plain format_cost.
        assert_eq!(format_cost_marked(0.0, true, 0), "$?");
        assert_eq!(format_cost_marked(5.0, true, 0), "$5.0*");
        assert_eq!(format_cost_marked(5.0, false, 0), "$5.0");
    }

    // A session whose model has no pricing entry must show "$?" in the Daily
    // list — a silent $0 reads as "this session was free", which is wrong.
    #[test]
    fn daily_row_long_title_truncates_with_single_ellipsis() {
        use crate::test_helpers::helpers::{
            make_daily_group, make_session_with_tokens, make_test_app_state,
        };
        let date = chrono::NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(); // lint-ok: date-literal
        let mut session = make_session_with_tokens("~/proj", 1000, 500, "claude-sonnet-4-20250514");
        // Wider than any summary column so the truncation path must fire.
        session.custom_title = Some("word ".repeat(60));
        let mut state = make_test_app_state(vec![make_daily_group(date, vec![session])]);
        state.tab = crate::Tab::Daily;
        let text = render_to_text(&mut state, 140, 45);
        assert!(text.contains('…'), "long title must end with an ellipsis");
        assert!(
            !text.contains("…."),
            "ellipsis must never be followed by extra dots"
        );
    }

    #[test]
    fn daily_row_unknown_model_cost_shows_question_not_zero() {
        use crate::test_helpers::helpers::{
            make_daily_group, make_session_with_tokens, make_test_app_state,
        };
        let date = chrono::NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(); // lint-ok: date-literal
        let session = make_session_with_tokens("~/proj", 1000, 500, "mystery-model-x");
        let mut state = make_test_app_state(vec![make_daily_group(date, vec![session])]);
        state.tab = crate::Tab::Daily;
        let text = render_to_text(&mut state, 140, 45);
        assert!(
            text.contains("$?"),
            "unknown pricing must render $?, not $0"
        );
    }

    /// Render, then return only the text WITHIN the active popup's rect
    /// (`state.layout.active_popup_area`, set by `draw`). Scoping `contains()` to the
    /// popup avoids the false-positive class where a match lands on the panel
    /// BEHIND the popup (e.g. a stripped name that also appears in a background
    /// list). Empty string when no popup is open.
    fn render_popup_text(state: &mut crate::AppState, width: u16, height: u16) -> String {
        let buffer = render_buffer(state, width, height);
        let Some(rect) = state.layout.active_popup_area else {
            return String::new();
        };
        let mut text = String::new();
        let x1 = (rect.x + rect.width).min(buffer.area.width);
        let y1 = (rect.y + rect.height).min(buffer.area.height);
        for y in rect.y..y1 {
            for x in rect.x..x1 {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    fn render_buffer(
        state: &mut crate::AppState,
        width: u16,
        height: u16,
    ) -> ratatui::buffer::Buffer {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, state)).unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn test_draw_insights_renders_without_panic() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        let text = render_to_text(&mut state, 120, 35);
        assert!(text.contains("/day"), "should contain /day metric");
    }

    #[test]
    fn show_conversation_clears_stale_tab_bar_triggers() {
        // Regression: draw_tabs is skipped in conv view, so any trigger Rect
        // captured by the previous (non-conv) frame is stale and would shadow
        // widgets placed at the same coords by the conv layout. draw() must
        // clear those triggers before drawing the conv view.
        use ratatui::layout::Rect;
        let mut state = create_test_state();
        state.show_conversation = true;
        state.layout.help_trigger = Some(Rect {
            x: 1,
            y: 1,
            width: 3,
            height: 1,
        });
        state.layout.filter_popup_area_trigger = Some(Rect {
            x: 1,
            y: 1,
            width: 5,
            height: 1,
        });
        state.layout.project_popup_area_trigger = Some(Rect {
            x: 1,
            y: 1,
            width: 5,
            height: 1,
        });
        state.layout.pin_view_trigger = Some(Rect {
            x: 1,
            y: 1,
            width: 3,
            height: 1,
        });
        let _ = render_to_text(&mut state, 140, 45);
        assert!(
            state.layout.help_trigger.is_none(),
            "help_trigger must clear"
        );
        assert!(state.layout.filter_popup_area_trigger.is_none());
        assert!(state.layout.project_popup_area_trigger.is_none());
        assert!(state.layout.pin_view_trigger.is_none());
    }

    #[test]
    fn test_draw_insights_uses_calendar_days() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        let text = render_to_text(&mut state, 120, 35);

        // total_work_tokens = 50000 + 10000 = 60000
        // calendar_days = 10
        // tokens_per_day = 60000 / 10 = 6000 = "6.00K"
        // If active_days were used: 60000 / 2 = 30000 = "30.0K"
        assert!(
            text.contains("6.00K/day"),
            "tokens/day should use calendar_days (10), got buffer:\n{text}"
        );
        assert!(
            !text.contains("30.0K/day"),
            "should NOT use active_days (2) for tokens/day"
        );
    }

    #[test]
    fn test_draw_dashboard_renders_without_panic() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Dashboard;
        render_to_text(&mut state, 120, 35);
    }

    #[test]
    fn test_languages_compact_panel_expands_unknown_exts_no_other_bucket() {
        // The fixture seeds `language_usage["Other"] = 30` plus `.example`,
        // `.xyz`, `.abc` in `extension_usage`. The compact Languages panel
        // must show those extensions individually (per the categorization
        // rule) and never render the literal "Other" row.
        let mut state = create_test_state();
        state.tab = crate::Tab::Dashboard;
        state.dashboard_panel = 4;
        let text = render_to_text(&mut state, 140, 45);
        assert!(
            !text.contains(" Other "),
            "compact Languages panel must not render an 'Other' catch-all row"
        );
        // At least one of the unknown extensions should surface as a row.
        assert!(
            text.contains("example") || text.contains("xyz") || text.contains("abc"),
            "unknown extensions should be visible individually in the Languages panel"
        );
    }

    #[test]
    fn test_insights_weekly_monthly_no_bottom_padding() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        let buffer = render_buffer(&mut state, 120, 35);

        let help_row = buffer.area.height.saturating_sub(1);
        let panel_last_row = help_row.saturating_sub(1);
        let mut has_border = false;
        for x in 0..buffer.area.width {
            let sym = buffer[(x, panel_last_row)].symbol();
            if sym == "─" || sym == "┘" || sym == "└" || sym == "┴" {
                has_border = true;
                break;
            }
        }
        assert!(
            has_border,
            "last row before help should be panel bottom border, not empty space"
        );
    }

    #[test]
    fn test_draw_daily_renders_without_panic() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Daily;
        render_to_text(&mut state, 120, 35);
    }

    #[test]
    fn test_draw_insights_detail_popup_renders() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        state.active_popup = crate::ActivePopup::InsightsDetail { scroll: 0 };

        let panel_markers = [
            (0, "Cache Hit Rate"),
            (1, "/day"),
            (2, "Monday"),
            (3, "avg"),
        ];
        for (panel, expected) in panel_markers {
            state.insights_panel = panel;
            let text = render_to_text(&mut state, 120, 35);
            assert!(
                text.contains(expected),
                "insights detail panel {panel} should contain '{expected}', got:\n{text}"
            );
        }
    }

    #[test]
    fn test_draw_dashboard_detail_popup_renders() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Dashboard;
        state.active_popup = crate::ActivePopup::DashboardDetail;

        let panel_markers = [
            (0, "Daily Costs"),
            (1, "close"), // Projects: dynamic title, verify popup footer
            (2, "Model Tokens"),
            (3, "Ecosystem"),
            (4, "Languages"),
            (5, "Daily Activity"),
            (6, "Hourly Average"),
        ];
        for (panel, expected) in panel_markers {
            state.dashboard_panel = panel;
            let text = render_to_text(&mut state, 120, 35);
            assert!(
                text.contains(expected),
                "dashboard detail panel {panel} should contain '{expected}', got:\n{text}"
            );
        }
    }

    #[test]
    fn test_tools_detail_popup_tab_labels_use_official_names() {
        // Regression: tab labels must be Tools / Skills / Subagents / Commands
        // (Built-in + MCP are merged under "Tools" since both are tools the
        // assistant calls).
        let mut state = create_test_state();
        // Inject one of each category to populate sections (generic placeholders only).
        state.stats.tool_usage.insert("Bash".to_string(), 10);
        state
            .stats
            .tool_usage
            .insert("mcp__server1__action".to_string(), 4);
        state
            .stats
            .tool_usage
            .insert("skill:my-skill".to_string(), 3);
        state.stats.tool_usage.insert("agent:type-a".to_string(), 2);
        // Zero-count sections drop their jump-digit prefix, so every
        // category needs at least one call for the order assertion below.
        state
            .stats
            .tool_usage
            .insert("command:my-cmd".to_string(), 1);
        state.tab = crate::Tab::Dashboard;
        state.active_popup = crate::ActivePopup::DashboardDetail;
        state.dashboard_panel = 3;

        let text = render_to_text(&mut state, 140, 35);
        assert!(
            text.contains("Tools"),
            "should show Tools tab. Got:\n{text}"
        );
        assert!(
            text.contains("Skills"),
            "should show Skills tab. Got:\n{text}"
        );
        assert!(
            text.contains("Subagents"),
            "should show Subagents tab. Got:\n{text}"
        );
        // Inactive tab shortcut prefixes pin the section ORDER (Tools →
        // Skills → Commands → Subagents); an `||` here would let a swapped
        // pair slip through unnoticed.
        assert!(
            text.contains("2:Skills")
                && text.contains("3:Commands")
                && text.contains("4:Subagents"),
            "inactive tabs must show ordered shortcut prefixes. Got:\n{text}"
        );
    }

    #[test]
    fn test_tools_detail_tab_click_areas_align_with_drawn_row() {
        // The tab click Rects (`tools_detail_tab_areas`) must sit on the SAME
        // buffer row where the tab labels are drawn — TestBackend asserts text
        // only, so a y-offset in the hit-test would pass every render test yet
        // make clicking a tab silently miss. Lock the alignment here.
        let mut state = create_test_state();
        state.stats.tool_usage.insert("Bash".to_string(), 10);
        state
            .stats
            .tool_usage
            .insert("skill:my-skill".to_string(), 3);
        state.tab = crate::Tab::Dashboard;
        state.active_popup = crate::ActivePopup::DashboardDetail;
        state.dashboard_panel = 3;

        // Render populates `tools_detail_tab_areas` as a side effect.
        let text = render_to_text(&mut state, 140, 35);
        let lines: Vec<&str> = text.lines().collect();
        let drawn_row = lines
            .iter()
            .position(|l| l.contains("Tools") && l.contains("Subagents"))
            .expect("tab bar line should be in the rendered buffer");

        assert!(
            !state.layout.tools_detail_tab_areas.is_empty(),
            "tab click areas should be recorded after render"
        );
        for (idx, rect) in &state.layout.tools_detail_tab_areas {
            assert_eq!(
                rect.y as usize, drawn_row,
                "tab {idx} click Rect (y={}) must align with the drawn tab row {drawn_row}",
                rect.y
            );
        }
    }

    #[test]
    fn test_ecosystem_recency_sort_does_not_overflow_section_bars() {
        // Regression: recency sort orders section items by last-used, so the
        // first item is NOT the max-count one. The bar denominator must be the
        // true max, else a later higher-count row gives ratio>1 and panics in
        // the bar `repeat`. Render every section in recency mode.
        let mut state = create_test_state();
        // Items where the MOST-RECENT key has FEWER calls than an older one.
        // Recency sort puts the low-count recent item first; the old bar
        // denominator (first item) then made the older high-count item's
        // ratio > 1 and overflowed the bar repeat.
        let recent = chrono::Utc::now();
        let old = recent - chrono::Duration::days(100);
        // `s1` = most-recent + low count, `s2` = older + high count.
        for (k, c, ts) in [
            ("skill:s1", 2usize, recent),
            ("skill:s2", 500, old),
            ("command:c1", 3, recent),
            ("command:c2", 700, old),
            ("agent:s1", 1, recent),
            ("agent:s2", 900, old),
        ] {
            state.stats.tool_usage.insert(k.to_string(), c);
            state.tool_last_used.insert(k.to_string(), ts);
        }
        state.tab = crate::Tab::Dashboard;
        state.active_popup = crate::ActivePopup::DashboardDetail;
        state.dashboard_panel = 3;
        state.dashboard_ecosystem_sort = crate::state::RankSort::Recency;
        // Render each section (0=Tools,1=Skills,2=Commands,3=Subagents) — none
        // may panic. (render_to_text panics propagate and fail the test.)
        for section in 0..4 {
            state.tools_detail_section = section;
            let text = render_to_text(&mut state, 140, 35);
            assert!(
                text.contains("Ecosystem"),
                "section {section} should render"
            );
        }
    }

    #[test]
    fn test_projects_panel_long_name_truncates_with_ellipsis() {
        // A project label wider than the name column must shrink to `…`, not get
        // raw-cut at the panel border (the compact Projects panel mirrors Models).
        let long = "this-is-an-extremely-long-project-directory-name-that-overflows-everything";
        let mut state = create_test_state();
        state.stats.project_stats.clear();
        state.stats.project_stats.insert(
            long.to_string(),
            crate::aggregator::ProjectStats {
                sessions: 1,
                tokens: 1000,
                work_tokens: 1000,
            },
        );
        state
            .project_labels
            .insert(long.to_string(), long.to_string());
        state.tab = crate::Tab::Dashboard;
        let text = render_to_text(&mut state, 70, 24);
        assert!(
            text.contains('…'),
            "long project name should ellipsis-truncate:\n{text}"
        );
        assert!(!text.contains(long), "full long name must not render uncut");
    }

    #[test]
    fn test_ecosystem_line_count_includes_mcp_servers_divider() {
        // Tools tab body with Built-in + MCP = 5 lines (summary + 2 group
        // rows + ratio + divider). No mcp_status ⇒ no stale legend.
        // Missing divider clips the last server row.
        let mut state = create_test_state();
        state.stats.tool_usage.clear();
        state.mcp_status.clear();
        state.configured_resources = Default::default();
        state.stats.tool_usage.insert("Bash".to_string(), 5);
        state
            .stats
            .tool_usage
            .insert("mcp__server1__do".to_string(), 3);
        state.tools_detail_section = 0;
        assert_eq!(crate::ui::dashboard::tool_usage_line_count(&state), 5);
    }

    #[test]
    fn test_costs_popup_oldest_day_reachable_under_month_dividers() {
        // daily_costs spanning more days than fit on one screen. 40 consecutive
        // days always cross at least one month boundary, so the body inserts a
        // divider row: a day-based scroll clamp would push the oldest day below
        // the visible fold; the line-based clamp must keep it reachable at max
        // scroll. Dates are relative to today so the fixture never rots.
        let mut state = create_test_state();
        let today = chrono::Local::now().date_naive();
        let dc: Vec<(chrono::NaiveDate, f64)> = (0..40)
            .map(|k| (today - chrono::Duration::days(k), 5.0))
            .collect();
        let oldest = today - chrono::Duration::days(39);
        state.daily_costs = dc.clone();
        state.original_daily_costs = dc;
        assert!(
            crate::ui::dashboard::active_days_body_line_count(&state, today) >= 41,
            "40 day rows + at least one month divider"
        );
        state.tab = crate::Tab::Dashboard;
        state.dashboard_panel = 0;
        state.active_popup = crate::ActivePopup::DashboardDetail;
        // Past the end — the popup clamps to the last full line-based page.
        state.dashboard_scroll[0] = 9999;
        let text = render_to_text(&mut state, 140, 35);
        assert!(
            text.contains(&oldest.to_string()),
            "oldest day must render at max scroll:\n{text}"
        );
    }

    #[test]
    fn test_ecosystem_popup_sort_label_reflects_mode() {
        let mut state = create_test_state();
        state.stats.tool_usage.insert("Bash".to_string(), 10);
        state
            .stats
            .tool_usage
            .insert("mcp__server1__tool".to_string(), 4);
        state.tab = crate::Tab::Dashboard;
        state.active_popup = crate::ActivePopup::DashboardDetail;
        state.dashboard_panel = 3;

        state.dashboard_ecosystem_sort = crate::state::RankSort::Recency;
        let text = render_to_text(&mut state, 140, 35);
        assert!(
            text.contains("Ecosystem · recent"),
            "recency mode should label the title 'recent'. Got:\n{text}"
        );

        state.dashboard_ecosystem_sort = crate::state::RankSort::Tokens;
        let text = render_to_text(&mut state, 140, 35);
        assert!(
            text.contains("Ecosystem · calls"),
            "magnitude mode should label the title 'calls'. Got:\n{text}"
        );
    }

    #[test]
    fn test_mcp_server_row_areas_align_under_scroll() {
        // The Tools-section body is post-sliced (`lines[headers..].skip(scroll)`),
        // not rendered via Paragraph.scroll, so a server row's click Rect must
        // be recorded at its SCROLLED screen row — and only for rows in view.
        // TestBackend asserts text only, so a `+scroll` drift passes every
        // render test yet sends clicks to the wrong server.
        let mut state = create_test_state();
        state.stats.tool_usage.insert("Bash".to_string(), 100);
        for n in 0..20 {
            state
                .stats
                .tool_usage
                .insert(format!("mcp__srv{n:02}__tool"), 60 - n);
        }
        state.tab = crate::Tab::Dashboard;
        state.active_popup = crate::ActivePopup::DashboardDetail;
        state.dashboard_panel = 3;
        state.tools_detail_section = 0; // Tools tab (Built-in + MCP)
        state.dashboard_scroll[3] = 4; // force a scrolled viewport

        let buffer = render_buffer(&mut state, 140, 24);
        assert!(
            state.dashboard_scroll[3] > 0,
            "the body must actually be scrolled to exercise the offset"
        );
        assert!(
            !state.layout.mcp_server_row_areas.is_empty(),
            "server rows should be recorded after render"
        );
        for (idx, rect) in &state.layout.mcp_server_row_areas {
            let row_text: String = (0..buffer.area.width)
                .map(|x| buffer[(x, rect.y)].symbol())
                .collect();
            assert!(
                row_text.contains('▶') || row_text.contains('▼'),
                "server {idx} click Rect at y={} must land on a drawn server \
                 row (▶/▼), got: {row_text:?}",
                rect.y
            );
        }
    }

    #[test]
    fn test_tools_detail_popup_active_section_switches() {
        let mut state = create_test_state();
        state.stats.tool_usage.insert("Bash".to_string(), 10);
        state
            .stats
            .tool_usage
            .insert("skill:my-skill".to_string(), 3);
        state.tab = crate::Tab::Dashboard;
        state.active_popup = crate::ActivePopup::DashboardDetail;
        state.dashboard_panel = 3;

        // Active = 0 (Tools): body should show the synthetic Built-in group row
        // (collapsed by default — individual tool names appear only when expanded).
        // `render_popup_text` scopes to the popup so background tool rows can't
        // satisfy these assertions.
        state.tools_detail_section = 0;
        let text = render_popup_text(&mut state, 140, 35);
        assert!(
            text.contains("Built-in"),
            "Tools body should show the Built-in group row. Got:\n{text}"
        );

        // Expanding the Built-in group should reveal Bash.
        state.mcp_expanded_servers.insert("Built-in".to_string());
        let text = render_popup_text(&mut state, 140, 35);
        assert!(
            text.contains("Bash"),
            "expanded Built-in group should show Bash. Got:\n{text}"
        );

        // Active = 1 (Skills; Tools merge collapsed Built-in+MCP into idx 0).
        // The Skills section shows the stripped name (`format_tool_short` drops
        // the `skill:` prefix). Scope to the popup rect: the raw `skill:my-skill`
        // key also renders in the Ecosystem panel BEHIND the popup, so a
        // whole-frame assertion would false-positive on it.
        state.tools_detail_section = 1;
        let text = render_popup_text(&mut state, 140, 35);
        assert!(
            text.contains("my-skill") && !text.contains("skill:my-skill"),
            "Skills popup body should show the stripped name `my-skill`. Got:\n{text}"
        );
    }

    #[test]
    fn test_tools_detail_non_mcp_tab_scroll_skips_first_row() {
        // Skills/Commands/Subagents tabs had headers=5 (off-by-one) which
        // pinned the first body row, so j-scroll only moved rows 2+ out
        // of view while row "1." stayed sticky. With headers=4 the first
        // row is part of the body and scrolls normally.
        let mut state = create_test_state();
        // Inject 60 distinct skills so the body has more rows than the
        // popup viewport can show. With fewer rows the `max_scroll` clamp
        // in `draw_dashboard_detail_popup` would silently zero `scroll`
        // and the test wouldn't actually exercise the body slice.
        for i in 0..60 {
            state
                .stats
                .tool_usage
                .insert(format!("skill:my-skill{i:02}"), 1);
        }
        state.tab = crate::Tab::Dashboard;
        state.active_popup = crate::ActivePopup::DashboardDetail;
        state.dashboard_panel = 3;
        state.tools_detail_section = 1;
        state.dashboard_scroll[3] = 0;
        let text_top = render_to_text(&mut state, 140, 24);
        assert!(
            text_top.contains("1. my-skill00"),
            "row 1 should appear at scroll=0. Got:\n{text_top}"
        );
        // Scroll past the first 2 rows — row 1 / row 2 must drop off,
        // row 3 must remain.
        state.dashboard_scroll[3] = 2;
        let text_scrolled = render_to_text(&mut state, 140, 24);
        assert!(
            !text_scrolled.contains("1. my-skill00"),
            "row 1 must scroll off with dashboard_scroll[3]=2. Got:\n{text_scrolled}"
        );
        assert!(
            text_scrolled.contains("3. my-skill02"),
            "row 3 must remain visible after scrolling past 2. Got:\n{text_scrolled}"
        );
    }

    #[test]
    fn test_tools_detail_popup_empty_section_falls_back() {
        // If user sets active to a section with zero items, render should fall back
        // to the first non-empty section (Tools).
        let mut state = create_test_state();
        state.stats.tool_usage.insert("Bash".to_string(), 5);
        state.tab = crate::Tab::Dashboard;
        state.active_popup = crate::ActivePopup::DashboardDetail;
        state.dashboard_panel = 3;
        state.tools_detail_section = 2; // Subagents (empty)

        let text = render_to_text(&mut state, 140, 35);
        // Should not panic and should show the Built-in group row from the Tools fallback.
        assert!(
            text.contains("Built-in"),
            "should fall back to Tools and show Built-in row. Got:\n{text}"
        );
    }

    #[test]
    fn test_insights_metrics_shows_usage_row_absolute_counts() {
        // Metrics row 4 shows absolute usage counts per category; a
        // cross-category `%` would compare incommensurable invocation
        // semantics, so the test guards against re-introducing one.
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        state.stats.total_session_days = 100;
        state.stats.sessions_using_skills = 20;
        state.stats.sessions_using_subagents = 30;
        state.stats.sessions_using_mcp = 10;

        let text = render_to_text(&mut state, 140, 35);
        assert!(
            text.contains("Sessions using"),
            "should contain 'Sessions using' label. Got:\n{text}"
        );
        assert!(
            text.contains("MCP"),
            "should contain MCP category. Got:\n{text}"
        );
        assert!(text.contains("Skills"), "should contain Skills category");
        assert!(
            text.contains("Subagents"),
            "should contain Subagents category"
        );
        // Absolute counts should be present.
        assert!(text.contains("20") && text.contains("30") && text.contains("10"));
        // No cross-category percentage should appear in this row.
        // (We can't grep loose "%" because other rows use it — just verify "20%" absent.)
        assert!(
            !text.contains("20%") || !text.contains("30%"),
            "row should not carry cross-category %. Got:\n{text}"
        );
    }

    #[test]
    fn test_insights_metrics_row_renders_when_no_sessions() {
        // Regression: zero session_days must not panic (no div-by-zero from the old % calc).
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        state.stats.total_session_days = 0;
        state.stats.sessions_using_skills = 0;

        let text = render_to_text(&mut state, 140, 35);
        assert!(text.contains("Sessions using"));
    }

    #[test]
    fn test_dashboard_tools_panel_shows_all_categories_one_line_each() {
        // Regression: the Dashboard Tools preview must render exactly 1 line per non-
        // empty category (Tools, Skills, Subagents) so all rows are visible at once
        // even in narrow panel slots. Built-in and MCP are merged under "Tools"
        // since both are tools the assistant calls. Each row ends with a `▶` marker
        // indicating it is clickable to open the detail popup at that section.
        let mut state = create_test_state();
        state.stats.tool_usage.insert("Bash".to_string(), 100);
        state
            .stats
            .tool_usage
            .insert("mcp__server1__action".to_string(), 50);
        state
            .stats
            .tool_usage
            .insert("skill:my-skill".to_string(), 30);
        state
            .stats
            .tool_usage
            .insert("agent:type-a".to_string(), 20);
        state.tab = crate::Tab::Dashboard;

        let text = render_to_text(&mut state, 100, 35);
        for label in ["Tools", "Skills", "Subagents"] {
            assert!(
                text.contains(label),
                "Tools panel should show '{label}' row. Got:\n{text}"
            );
        }
        assert!(
            text.contains("▶"),
            "each category row should end with '▶' marker. Got:\n{text}"
        );
    }

    #[test]
    fn test_ecosystem_panel_title_renamed() {
        // `Ecosystem` is the canonical panel title — it covers built-in
        // tools plus Skills / Subagents / MCP servers, so a bare "Tools"
        // label would understate scope.
        let mut state = create_test_state();
        state.stats.tool_usage.insert("Bash".to_string(), 5);
        state.tab = crate::Tab::Dashboard;
        let text = render_to_text(&mut state, 140, 45);
        assert!(
            text.contains("Ecosystem"),
            "panel title should read 'Ecosystem'. Got:\n{text}"
        );
    }

    #[test]
    fn test_ecosystem_panel_health_alerts_pricing_gap() {
        // When the cost calculator flags some models as untracked, the
        // dashboard must surface that with a "pricing gap" health alert in the
        // Ecosystem panel so users notice silently-undercounted spend.
        let mut state = create_test_state();
        state.stats.tool_usage.insert("Bash".to_string(), 5);
        state
            .models_without_pricing
            .insert("Some Future Model".to_string());
        state.tab = crate::Tab::Dashboard;
        let text = render_to_text(&mut state, 140, 45);
        assert!(
            text.contains("pricing gap"),
            "Ecosystem panel should surface pricing-gap alert. Got:\n{text}"
        );
    }

    #[test]
    fn test_ecosystem_panel_nominal_when_clean() {
        // No alerts -> the bottom tier collapses to a single positive line so the
        // panel still feels balanced instead of empty.
        let mut state = create_test_state();
        state.stats.tool_usage.insert("Bash".to_string(), 5);
        state.models_without_pricing.clear();
        state.mcp_status.clear();
        state.retention_warning = None;
        state.tab = crate::Tab::Dashboard;
        let text = render_to_text(&mut state, 140, 45);
        assert!(
            text.contains("all systems nominal"),
            "no alerts should render the nominal line. Got:\n{text}"
        );
    }

    #[test]
    fn test_ecosystem_panel_tier1_only_when_short() {
        // At a small terminal height the bottom row's panels get squeezed; the
        // Ecosystem panel must drop to category summaries only and not bleed
        // top-tools or alert lines into adjacent panels.
        let mut state = create_test_state();
        state.stats.tool_usage.insert("Bash".to_string(), 5);
        state
            .stats
            .tool_usage
            .insert("mcp__server1__action".to_string(), 3);
        state.tab = crate::Tab::Dashboard;
        // 100x27 → bottom-bottom-row panels get ~6 rows tall → inner ~4 rows,
        // exactly enough for Tier 1 categories but no room for Tier 2/3.
        let text = render_to_text(&mut state, 100, 27);
        // "Top tools" header is the easy probe — when Tier 2 is dropped it must
        // not appear. Categories should still be present.
        assert!(
            !text.contains("Top tools"),
            "Tier 2 'Top tools' header must be dropped at narrow heights. Got:\n{text}"
        );
        assert!(
            text.contains("Tools"),
            "Tier 1 categories must remain even at narrow heights. Got:\n{text}"
        );
    }

    #[test]
    fn test_mcp_tab_collapsed_by_default_shows_arrow_but_no_tools() {
        // Regression: with `mcp_expanded_servers` empty the MCP tab renders a collapsed
        // "▶ server …" row and no sub-row for any of the server's tools.
        let mut state = create_test_state();
        state.stats.tool_usage.insert("Bash".to_string(), 1);
        state
            .stats
            .tool_usage
            .insert("mcp__server1__action1".to_string(), 3);
        state
            .stats
            .tool_usage
            .insert("mcp__server1__action2".to_string(), 2);
        state.tab = crate::Tab::Dashboard;
        state.active_popup = crate::ActivePopup::DashboardDetail;
        state.dashboard_panel = 3;
        state.tools_detail_section = 0;
        assert!(state.mcp_expanded_servers.is_empty());

        let text = render_to_text(&mut state, 140, 35);
        assert!(
            text.contains("▶ "),
            "collapsed server row should display the right-pointing arrow. Got:\n{text}"
        );
        assert!(
            text.contains("server1"),
            "server row should appear. Got:\n{text}"
        );
        // Tool sub-rows (indented `       1. action1` lines) must NOT appear while
        // the server is collapsed. Bare matches on "action1" are too broad — the
        // dashboard Ecosystem panel may legitimately surface tool names in its
        // top-tools section behind the popup, which is unrelated to this regression.
        assert!(
            !text.contains("       1. action1") && !text.contains("       1. action2"),
            "expanded sub-rows must be hidden while the server is collapsed. Got:\n{text}"
        );
    }

    #[test]
    fn test_mcp_tab_expand_shows_tool_rows_with_within_server_pct() {
        // Regression: expanding a server via `mcp_expanded_servers` injects per-tool rows
        // below the server row. % displayed on tool rows must be within-server (a tool
        // that accounts for 60% of its server's calls reads "60%", not "60% of grand total").
        let mut state = create_test_state();
        state
            .stats
            .tool_usage
            .insert("mcp__server1__action1".to_string(), 60); // 60% of server1
        state
            .stats
            .tool_usage
            .insert("mcp__server1__action2".to_string(), 40); // 40% of server1
        state
            .stats
            .tool_sessions
            .insert("mcp__server1__action1".to_string(), 3);
        state
            .stats
            .tool_sessions
            .insert("mcp__server1__action2".to_string(), 2);
        state.tab = crate::Tab::Dashboard;
        state.active_popup = crate::ActivePopup::DashboardDetail;
        state.dashboard_panel = 3;
        state.tools_detail_section = 0;
        state.mcp_expanded_servers.insert("server1".to_string());

        let text = render_to_text(&mut state, 140, 35);
        assert!(
            text.contains("▼ "),
            "expanded server row should display the down-pointing arrow. Got:\n{text}"
        );
        assert!(
            text.contains("action1"),
            "top-ranked tool should appear in expanded sub-rows. Got:\n{text}"
        );
        assert!(
            text.contains("action2"),
            "second tool should also appear when expanded. Got:\n{text}"
        );
        // 60% is within-server for action1; guards against a future refactor that
        // mistakenly uses grand-total as the denominator.
        assert!(
            text.contains("60%"),
            "tool row should show within-server percentage (action1 = 60%). Got:\n{text}"
        );
    }

    #[test]
    fn test_tools_detail_popup_renders_plugin_mcp_form_aggregated_by_server() {
        // Plugin-form MCP keys must aggregate at SERVER level
        // (`<org>/<server>`), not per tool. Built-in renders as a synthetic
        // group alongside MCP, so the header says "groups" not "servers".
        let mut state = create_test_state();
        state.stats.tool_usage.insert("Bash".to_string(), 1);
        state
            .stats
            .tool_usage
            .insert("mcp__plugin_orgA_serverB__action1".to_string(), 3);
        state
            .stats
            .tool_usage
            .insert("mcp__plugin_orgA_serverB__action2".to_string(), 2);
        state
            .stats
            .tool_sessions
            .insert("mcp__plugin_orgA_serverB__action1".to_string(), 1);
        state
            .stats
            .tool_sessions
            .insert("mcp__plugin_orgA_serverB__action2".to_string(), 1);
        state
            .stats
            .mcp_server_sessions
            .insert("orgA/serverB".to_string(), 1);
        state.tab = crate::Tab::Dashboard;
        state.active_popup = crate::ActivePopup::DashboardDetail;
        state.dashboard_panel = 3;
        state.tools_detail_section = 0; // Tools tab (Built-in + MCP merged)

        let text = render_to_text(&mut state, 140, 35);
        assert!(
            text.contains("orgA/serverB"),
            "Tools section should show the plugin server name. Got:\n{text}"
        );
        // After server aggregation the body shows "2 tools" for this single server
        // (two distinct actions from the same server collapse into one row).
        assert!(
            text.contains("2 tools"),
            "Tools section should report per-server tool count. Got:\n{text}"
        );
        // Header reports "2 groups" (Built-in synthetic + 1 MCP server).
        assert!(
            text.contains("2 groups"),
            "Tools section header should list group count. Got:\n{text}"
        );
    }

    #[test]
    fn test_insights_metrics_usage_counts_plugin_mcp_sessions() {
        // Metrics usage row's MCP count must include sessions that used
        // only plugin-form MCP tools. The assertion uses the absolute
        // count rather than a cross-category percentage.
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        state.stats.total_session_days = 100;
        state.stats.sessions_using_mcp = 50;
        state.stats.sessions_using_skills = 0;
        state.stats.sessions_using_subagents = 0;
        state
            .stats
            .tool_sessions
            .insert("mcp__plugin_orgA_serverB__action".to_string(), 50);

        let text = render_to_text(&mut state, 140, 35);
        // The absolute MCP session count must appear near the MCP label.
        assert!(
            text.contains("MCP") && text.contains("50"),
            "should show MCP 50 sessions. Got:\n{text}"
        );
    }

    #[test]
    fn test_resume_copy_preserves_newlines_in_static_popup_selection() {
        // Copying a multi-line block from a static popup (Session Detail)
        // must preserve `\<newline>` continuation. Popup path:
        // `extract_selected_text_from_buffer(conv_area=Some, wrap_flags=None)`.
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        // Popup inner area (arbitrary position).
        let inner = Rect::new(10, 5, 70, 4);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 100, 20));
        // Write the rendered lines into the popup's inner rows.
        let line1 = "    cd /path && \\";
        let line2 = "      claude -r abc-123";
        let row1 = inner.y;
        let row2 = inner.y + 1;
        for (i, ch) in line1.chars().enumerate() {
            buffer[(inner.x + i as u16, row1)].set_char(ch);
        }
        for (i, ch) in line2.chars().enumerate() {
            buffer[(inner.x + i as u16, row2)].set_char(ch);
        }

        // Select both rows fully.
        let sel = (inner.x, row1, inner.x + inner.width.saturating_sub(1), row2);
        let text = crate::extract_selected_text_from_buffer(&sel, &buffer, Some(inner), None, 0, 0);

        assert!(
            text.contains("\\\n"),
            "resume copy should preserve `\\<newline>` between lines. Got:\n{text:?}"
        );
        assert!(
            text.contains("    cd /path && \\"),
            "first line should be preserved verbatim (leading indent + trailing backslash). Got:\n{text:?}"
        );
        assert!(
            text.contains("      claude -r abc-123"),
            "second line should be preserved verbatim. Got:\n{text:?}"
        );
    }

    #[test]
    fn test_overview_flags_cost_when_pricing_gap_exists() {
        // Regression: when any model lacks pricing, the Overview cost figure must carry
        // a `*` marker and a caption warning so the silent-$0 risk is visible without
        // digging into the Models detail popup.
        let mut state = create_test_state();
        state.tab = crate::Tab::Dashboard;
        state
            .models_without_pricing
            .insert("claude-future-experimental-x".to_string());

        let text = render_to_text(&mut state, 140, 35);
        assert!(
            text.contains("models lack pricing"),
            "Overview should surface the pricing-gap warning. Got:\n{text}"
        );
    }

    #[test]
    fn test_overview_clean_when_no_pricing_gap() {
        // Regression: without unknown-pricing models, the Overview must NOT show the `*`
        // marker nor the warning caption (to avoid warning fatigue).
        let mut state = create_test_state();
        state.tab = crate::Tab::Dashboard;
        assert!(state.models_without_pricing.is_empty());

        let text = render_to_text(&mut state, 140, 35);
        assert!(
            !text.contains("models lack pricing"),
            "Overview should not show a warning when all models have pricing. Got:\n{text}"
        );
    }

    #[test]
    fn test_insights_metrics_shows_pricing_gap_row() {
        // Regression: Insights Metrics block must add the `⚠ Pricing gap` row when any
        // model in the current view lacks pricing. Guards against the row being dropped
        // if the layout gets refactored.
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        state
            .models_without_pricing
            .insert("claude-future-experimental-x".to_string());

        let text = render_to_text(&mut state, 140, 35);
        assert!(
            text.contains("Pricing gap"),
            "Insights Metrics should show 'Pricing gap' row. Got:\n{text}"
        );
        assert!(
            text.contains("1 models"),
            "Pricing gap row should show model count. Got:\n{text}"
        );
    }

    #[test]
    fn a_recent_sort_scales_model_bars_by_the_largest_model() {
        use crate::test_helpers::helpers::{
            make_daily_group, make_session_with_tokens, make_test_app_state,
        };
        let today = chrono::Local::now().date_naive();
        // The most recent model is the smallest, so it sorts first under Recency.
        let groups = vec![
            make_daily_group(
                today,
                vec![make_session_with_tokens("p", 100, 0, "claude-sonnet-4-6")],
            ),
            make_daily_group(
                today - chrono::Duration::days(3),
                vec![make_session_with_tokens("p", 10_000, 0, "claude-opus-4-6")],
            ),
        ];
        let mut state = make_test_app_state(groups);
        for (model, input) in [("Sonnet 4.6", 100), ("Opus 4.6", 10_000)] {
            state.aggregated_model_tokens.insert(
                model.to_string(),
                crate::aggregator::stats::TokenStats {
                    input_tokens: input,
                    ..Default::default()
                },
            );
        }
        state.dashboard_models_sort = crate::RankSort::Recency;
        state.tab = crate::Tab::Dashboard;
        state.active_popup = crate::ActivePopup::DashboardDetail;
        state.dashboard_panel = 2;
        let text = render_to_text(&mut state, 140, 45);
        let full = "█".repeat(15);
        let full_bars = text.lines().filter(|l| l.contains(&full)).count();
        assert_eq!(
            full_bars, 1,
            "only the largest model fills its bar:\n{text}"
        );
    }

    #[test]
    fn a_recent_sort_scales_project_bars_by_the_largest_project() {
        use crate::test_helpers::helpers::{make_daily_group, make_session, make_test_app_state};
        let today = chrono::Local::now().date_naive();
        let groups = vec![
            make_daily_group(today, vec![make_session("small", None, None)]),
            make_daily_group(
                today - chrono::Duration::days(3),
                vec![make_session("big", None, None)],
            ),
        ];
        let mut state = make_test_app_state(groups);
        state.stats.project_stats.clear();
        for (name, work) in [("small", 100), ("big", 10_000)] {
            state.stats.project_stats.insert(
                name.to_string(),
                crate::aggregator::stats::ProjectStats {
                    sessions: 1,
                    tokens: work,
                    work_tokens: work,
                },
            );
        }
        // Recency ranks by `project_list`; the smaller project is the newer one.
        state.project_list = vec![
            ("small".to_string(), 100, today),
            ("big".to_string(), 10_000, today - chrono::Duration::days(3)),
        ];
        state.dashboard_projects_sort = crate::RankSort::Recency;
        state.tab = crate::Tab::Dashboard;
        state.active_popup = crate::ActivePopup::DashboardDetail;
        state.dashboard_panel = 1;
        let text = render_to_text(&mut state, 140, 45);
        let full = "█".repeat(12);
        let full_bars = text.lines().filter(|l| l.contains(&full)).count();
        assert_eq!(
            full_bars, 1,
            "only the largest project fills its bar:\n{text}"
        );
    }

    #[test]
    fn test_models_detail_unknown_model_shows_warning_badge() {
        // Regression: unknown model families (no pricing entry) must show a "no pricing"
        // warning so the user is not silently undercharged in the cost summary.
        let mut state = create_test_state();
        state.tab = crate::Tab::Dashboard;
        state.active_popup = crate::ActivePopup::DashboardDetail;
        state.dashboard_panel = 2; // Models panel

        // Inject the unknown model into both:
        // 1. aggregated_model_tokens — drives the Models detail popup item list
        // 2. one session's day_tokens_by_model — drives the first/last-used dates
        let unknown_model = "claude-future-experimental-x".to_string();
        let tokens = crate::aggregator::stats::TokenStats {
            input_tokens: 1000,
            output_tokens: 500,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            non_standard_speed: false,
        };
        state
            .aggregated_model_tokens
            .insert(unknown_model.clone(), tokens.clone());
        // The Models detail popup decides "unknown" via `models_without_pricing`.
        state.models_without_pricing.insert(unknown_model.clone());
        if let Some(group) = state.daily_groups.get_mut(0)
            && let Some(session) = group.sessions.get_mut(0)
        {
            session.day_tokens_by_model.insert(unknown_model, tokens);
        }

        let text = render_to_text(&mut state, 140, 35);
        assert!(
            text.contains("no pricing"),
            "Models detail should show 'no pricing' badge for unknown model. Got:\n{text}"
        );
    }

    #[test]
    fn test_session_count_uses_ses_suffix_not_s() {
        // Regression: `Xs` suffix is confusable with seconds. Must be `X ses`.
        let mut state = create_test_state();
        state.tab = crate::Tab::Dashboard;
        state.active_popup = crate::ActivePopup::DashboardDetail;
        state.dashboard_panel = 1; // Projects

        let text = render_to_text(&mut state, 140, 35);
        // Must contain " ses" (space prefix) — never " 357s" raw form.
        if text.contains("ses") {
            // OK if Projects detail rendered with at least one row.
            assert!(
                !text.contains("357s") && !text.contains("100s"),
                "should NOT use raw `Ns` suffix. Got:\n{text}"
            );
        }
    }

    #[test]
    fn test_draw_help_overlay_renders() {
        let mut state = create_test_state();
        state.active_popup = crate::ActivePopup::Help { scroll: 0 };
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("Switch tabs"),
            "help overlay should contain keybinding text 'Switch tabs'"
        );
        assert!(
            text.contains("Quit (press twice"),
            "help overlay should contain the quit line"
        );
    }

    #[test]
    fn test_draw_narrow_terminal() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Dashboard;
        render_to_text(&mut state, 60, 20);

        state.tab = crate::Tab::Insights;
        render_to_text(&mut state, 60, 20);

        state.tab = crate::Tab::Daily;
        render_to_text(&mut state, 60, 20);
    }

    #[test]
    fn test_draw_minimal_terminal() {
        let mut state = create_test_state();
        for tab in [
            crate::Tab::Dashboard,
            crate::Tab::Daily,
            crate::Tab::Insights,
        ] {
            state.tab = tab;
            render_to_text(&mut state, 40, 10);
        }
    }

    #[test]
    fn test_draw_loading_state() {
        let mut state = create_test_state();
        state.loading = true;
        // Loading screen shows animated logo, not a tab view
        // Just verify it renders without panic
        render_to_text(&mut state, 120, 35);
    }

    #[test]
    fn test_draw_error_state() {
        let mut state = create_test_state();
        state.error = Some("Test error message".to_string());
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("Test error message"),
            "error state should display the error message"
        );
    }

    #[test]
    fn test_draw_empty_data() {
        let mut state = create_test_state();
        state.daily_groups.clear();
        state.daily_costs.clear();
        state.total_cost = 0.0;
        state.stats = crate::aggregator::Stats::default();

        for tab in [
            crate::Tab::Dashboard,
            crate::Tab::Daily,
            crate::Tab::Insights,
        ] {
            state.tab = tab;
            render_to_text(&mut state, 120, 35);
        }
    }

    #[test]
    fn test_insights_metrics_values() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        let text = render_to_text(&mut state, 120, 35);

        // cache_hit_rate = 40000 / (50000 + 40000) * 100 = 44.4%
        assert!(text.contains("44.4% cache"), "should show Cache Hit Rate");
        // tool_success_rate = 90 / (90+10) * 100 = 90.0%
        assert!(
            text.contains("90.0% success"),
            "should show tool success rate 90.0%"
        );
        // avg_cost_per_day = 100.0 / 10 = $10.00
        assert!(
            text.contains("$10.00/day cost"),
            "should show $10.00/day cost"
        );
        // tokens_per_day = 60000 / 10 = 6000
        assert!(
            text.contains("6.00K/day tokens"),
            "should show 6.00K/day tokens"
        );
    }

    #[test]
    fn test_cost_style_boundary_values() {
        // Each cutoff uses a strict `>` comparison, so the cutoff itself
        // belongs to the lower tier. Spot-check every band edge.
        assert_eq!(cost_style(20.0).fg, Some(theme::SUCCESS));
        assert_eq!(cost_style(20.01).fg, Some(theme::WARNING));
        assert_eq!(cost_style(60.0).fg, Some(theme::WARNING));
        assert_eq!(cost_style(60.01).fg, Some(theme::ERROR));
        assert_eq!(cost_style(100.0).fg, Some(theme::ERROR));
        assert_eq!(cost_style(100.01).fg, Some(theme::DANGER));
        assert_eq!(cost_style(300.0).fg, Some(theme::DANGER));
        assert_eq!(cost_style(300.01).fg, Some(theme::CRITICAL));
    }

    #[test]
    fn test_draw_insights_single_day_data() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        // Only today's data, so calendar_days = 1
        let today = chrono::Local::now().date_naive();
        state.daily_groups.retain(|g| g.date == today);

        let text = render_to_text(&mut state, 120, 35);
        // calendar_days = 1, tokens_per_day = 60000 / 1 = 60000 = "60.0K"
        assert!(
            text.contains("60.0K/day"),
            "with single day, tokens_per_day should be 60.0K, got:\n{text}"
        );
    }

    #[test]
    fn test_draw_insights_subagent_excluded() {
        use crate::aggregator::SessionInfo;

        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;

        let today = chrono::Local::now().date_naive();
        let subagent_session = SessionInfo {
            verified_cwd: None,
            file_path: std::path::PathBuf::from("/tmp/agent-test.jsonl"),
            project_name: "test-project".to_string(),
            git_branch: None,
            session_first_timestamp: chrono::Utc::now(),
            model: Some("claude-haiku-4-5-20250514".to_string()),
            day_input_tokens: 100000,
            day_output_tokens: 100000,
            day_user_msgs: 0,
            day_prompts: 0,
            day_assistant_msgs: 0,
            day_tokens_by_model: std::collections::HashMap::new(),
            day_hourly_activity: std::collections::HashMap::new(),
            day_hourly_work_tokens: std::collections::HashMap::new(),
            day_tool_usage: std::collections::HashMap::new(),
            day_language_usage: std::collections::HashMap::new(),
            day_extension_usage: std::collections::HashMap::new(),
            day_first_timestamp: chrono::Utc::now(),
            day_last_timestamp: chrono::Utc::now(),
            summary: None,
            custom_title: None,
            ai_title: None,
            last_user_message: None,
            first_user_message: None,
            is_subagent: true,
            parent_path: None,
            is_continued: false,
            line_churn: Default::default(),
        };

        // Add subagent session to today's group
        if let Some(group) = state.daily_groups.iter_mut().find(|g| g.date == today) {
            group.sessions.push(subagent_session);
        }

        let text = render_to_text(&mut state, 120, 35);
        // tokens_per_day should still be 6.00K (subagent excluded from count)
        // total_sessions in draw_insights counts only non-subagent sessions
        assert!(
            text.contains("1 sessions"),
            "subagent sessions should be excluded from session count, got:\n{text}"
        );
    }

    #[test]
    fn test_draw_empty_data_all_panels() {
        let mut state = create_test_state();
        state.daily_groups.clear();
        state.daily_costs.clear();
        state.total_cost = 0.0;
        state.stats = crate::aggregator::Stats::default();

        state.tab = crate::Tab::Insights;
        for panel in 0..4 {
            state.insights_panel = panel;
            render_to_text(&mut state, 120, 35);
        }

        state.active_popup = crate::ActivePopup::InsightsDetail { scroll: 0 };
        for panel in 0..4 {
            state.insights_panel = panel;
            render_to_text(&mut state, 120, 35);
        }
    }

    #[test]
    fn test_insights_detail_popup_calendar_days_consistency() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        state.active_popup = crate::ActivePopup::InsightsDetail { scroll: 0 };
        state.insights_panel = 0;

        let text = render_to_text(&mut state, 120, 35);
        // Popup-specific text: "/day   tokens" (with extra spaces) is unique to detail popup
        // Main view uses "6.00K/day tokens" (no extra spaces)
        assert!(
            text.contains("Cache Hit Rate"),
            "detail popup panel 0 should be visible"
        );
        assert!(
            text.contains("/day"),
            "detail popup should show /day metric"
        );
        // Verify calendar_days is used: total is "$100.00 / 10d"
        assert!(
            text.contains("10 days"),
            "detail popup should show 10 calendar days"
        );
    }

    #[test]
    fn insights_detail_popup_states_which_divisor_the_per_day_figures_use() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        state.active_popup = crate::ActivePopup::InsightsDetail { scroll: 0 };
        state.insights_panel = 0;

        let text = render_to_text(&mut state, 140, 45);
        assert!(
            text.contains("divide by all 10 days, not the 2 active ones"),
            "the popup must name the divisor; a bare $/day reads as either average"
        );
    }

    #[test]
    fn metric_per_day_zero_fills_or_skips_days_with_no_activity() {
        let state = create_test_state();
        let today = chrono::Local::now().date_naive();
        // The fixture is active on `today` and on `today - 9`; the eight days
        // between are absent, so a 10-day window exercises both arms.
        let sample = |g: &crate::aggregator::DailyGroup| DailyTrendValue {
            num: g.sessions.len() as f64,
            den: 1.0,
        };
        let zero = metric_per_day(&state, today, 10, MissingDay::Zero, sample);
        let skip = metric_per_day(&state, today, 10, MissingDay::Skip, sample);
        assert_eq!(zero.iter().filter(|(_, v)| v.is_some()).count(), 10);
        assert_eq!(skip.iter().filter(|(_, v)| v.is_some()).count(), 2);

        // The divisor difference is the whole point: the same samples average
        // over 10 days one way and over 2 the other.
        let (_, zero_baseline) = summarise_series(&zero);
        let (_, skip_baseline) = summarise_series(&skip);
        let sum: f64 = zero.iter().filter_map(|(_, v)| *v).sum();
        assert!((zero_baseline.unwrap() - sum / 10.0).abs() < f64::EPSILON);
        assert!((skip_baseline.unwrap() - sum / 2.0).abs() < f64::EPSILON);
    }

    /// Churn reaches all three popups it is exposed in. A render assertion
    /// because there is no other user-visible surface: wiring the aggregator
    /// without a panel would leave the number unreachable.
    #[test]
    fn churn_renders_in_session_insights_and_project_popups() {
        let mut state = create_test_state();
        for session in state
            .daily_groups
            .iter_mut()
            .flat_map(|g| g.sessions.iter_mut())
        {
            session.line_churn = crate::domain::LineChurn {
                added: 12,
                removed: 7,
            };
        }
        state.stats.line_churn = crate::domain::LineChurn {
            added: 1200,
            removed: 700,
        };

        state.tab = crate::Tab::Insights;
        state.active_popup = crate::ActivePopup::InsightsDetail { scroll: 0 };
        state.insights_panel = 0;
        let text = render_to_text(&mut state, 140, 45);
        assert!(
            text.contains("+1.20K / -700 lines"),
            "insights detail: {text}"
        );

        state.tab = crate::Tab::Daily;
        // The fixture's first group has no sessions; the popup needs one that
        // does or it renders nothing at all.
        state.selected_day = state.daily_groups.len() - 1;
        state.selected_session = 0;
        state.active_popup = crate::ActivePopup::Detail;
        let text = render_to_text(&mut state, 140, 45);
        assert!(text.contains("+12 / -7 lines"), "session detail: {text}");

        state.tab = crate::Tab::Dashboard;
        state.active_popup = crate::ActivePopup::ProjectDetail {
            path: "test-project".to_string(),
            scroll: 0,
        };
        let text = render_to_text(&mut state, 140, 45);
        assert!(text.contains("lines"), "project detail: {text}");
    }

    /// The Ecosystem panel's compact "Top:" row is built by appending, so
    /// nothing bounds it unless the builder stops at the panel edge — a long
    /// tool name or a six-digit count used to run past the border.
    #[test]
    fn ecosystem_compact_top_row_stays_inside_the_panel() {
        use unicode_width::UnicodeWidthStr;
        let mut state = create_test_state();
        for (name, count) in [
            ("BashOutputStream", 715_000usize),
            ("ReadFileContents", 351_000),
            ("EditManyFilesAtOnce", 240_000),
            ("WriteWholeFile", 190_000),
            ("GrepAcrossRepo", 120_000),
        ] {
            state.stats.tool_usage.insert(name.to_string(), count);
        }
        state.tab = crate::Tab::Dashboard;
        // Narrow enough that the Ecosystem panel takes the compressed form.
        for width in [100u16, 120, 140] {
            let text = render_to_text(&mut state, width, 30);
            for line in text.lines() {
                assert!(
                    UnicodeWidthStr::width(line) <= width as usize,
                    "row overflows at {width}: {line}"
                );
            }
        }
    }

    #[test]
    fn test_insights_popup_on_non_insights_tab() {
        // show_insights_detail is checked outside of tab guard in draw()
        // Verify it doesn't panic on Dashboard or Daily tabs
        let mut state = create_test_state();
        state.active_popup = crate::ActivePopup::InsightsDetail { scroll: 0 };
        state.insights_panel = 0;

        state.tab = crate::Tab::Dashboard;
        render_to_text(&mut state, 120, 35);

        state.tab = crate::Tab::Daily;
        render_to_text(&mut state, 120, 35);
    }

    #[test]
    fn test_model_efficiency_in_detail_popup() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Dashboard;
        state.active_popup = crate::ActivePopup::DashboardDetail;
        state.dashboard_panel = 2;
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("/MTok"),
            "Dashboard detail popup should contain /MTok, got:\n{text}"
        );
        assert!(
            text.contains("rate/MTok"),
            "Dashboard detail popup should contain documented rate, got:\n{text}"
        );
    }

    #[test]
    fn test_monthly_actual_in_insights() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("this mo:"),
            "Insights Monthly panel should label current month, got:\n{text}"
        );
    }

    #[test]
    fn test_monthly_actual_in_insights_detail() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        state.active_popup = crate::ActivePopup::InsightsDetail { scroll: 0 };
        state.insights_panel = 3;
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("this mo:"),
            "Insights detail popup panel 3 should label current month, got:\n{text}"
        );
    }

    #[test]
    fn test_period_filter_label() {
        use crate::PeriodFilter;
        assert_eq!(PeriodFilter::All.label(), "All");
        assert_eq!(PeriodFilter::Today.label(), "Today");
        assert_eq!(PeriodFilter::Last7d.label(), "7d");
        assert_eq!(PeriodFilter::Last30d.label(), "30d");
        assert_eq!(PeriodFilter::ThisMonth.label(), "This Month");
        assert_eq!(PeriodFilter::LastMonth.label(), "Last Month");
        assert_eq!(PeriodFilter::Last90d.label(), "90d");
    }

    #[test]
    fn test_period_filter_date_range() {
        use crate::PeriodFilter;
        use chrono::Datelike;

        let (start, end) = PeriodFilter::All.date_range();
        assert!(start.is_none());
        assert!(end.is_none());

        let (start, end) = PeriodFilter::Today.date_range();
        let today = chrono::Local::now().date_naive();
        assert_eq!(start, Some(today));
        assert!(end.is_none());

        let (start, end) = PeriodFilter::LastMonth.date_range();
        assert!(start.is_some());
        assert!(end.is_some());
        let s = start.unwrap();
        let e = end.unwrap();
        assert_eq!(s.day(), 1);
        assert!(e < today);
        assert_eq!(e.month(), s.month());
    }

    #[test]
    fn test_period_filter_all_variants_count() {
        assert_eq!(crate::PeriodFilter::ALL_VARIANTS.len(), 8);
    }

    #[test]
    fn test_apply_filter_7d_excludes_old_data() {
        let mut state = create_test_state();
        let today = chrono::Local::now().date_naive();
        let old_date = today - chrono::Duration::days(9);

        assert_eq!(state.daily_groups.len(), 2);
        assert!(state.daily_groups.iter().any(|g| g.date == old_date));

        state.period_filter = crate::PeriodFilter::Last7d;
        state.apply_filter();

        assert!(!state.daily_groups.iter().any(|g| g.date == old_date));
        assert!(state.daily_groups.iter().any(|g| g.date == today));
    }

    #[test]
    fn test_apply_filter_all_restores_data() {
        let mut state = create_test_state();
        let original_len = state.daily_groups.len();

        state.period_filter = crate::PeriodFilter::Last7d;
        state.apply_filter();
        assert!(state.daily_groups.len() < original_len);

        state.period_filter = crate::PeriodFilter::All;
        state.apply_filter();
        assert_eq!(state.daily_groups.len(), original_len);
    }

    #[test]
    fn test_filter_header_shows_label() {
        let mut state = create_test_state();
        state.period_filter = crate::PeriodFilter::Last7d;
        state.apply_filter();
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("7d"),
            "Header should show filter label '7d' when filter is active, got:\n{text}"
        );
    }

    #[test]
    fn test_filtered_rendering_no_panic() {
        let mut state = create_test_state();
        for filter in crate::PeriodFilter::ALL_VARIANTS {
            state.period_filter = filter;
            state.apply_filter();
            for tab in [
                crate::Tab::Dashboard,
                crate::Tab::Daily,
                crate::Tab::Insights,
            ] {
                state.tab = tab;
                render_to_text(&mut state, 120, 35);
            }
        }
    }

    #[test]
    fn test_filter_popup_renders() {
        let mut state = create_test_state();
        state.active_popup = crate::ActivePopup::FilterPopup {
            selected: 2,
            input_mode: false,
            input: crate::TextInput::default(),
            input_error: false,
        };
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("Filter Period"),
            "Filter popup should show title"
        );
        assert!(text.contains("All"), "Filter popup should show All option");
        assert!(
            text.contains("Today"),
            "Filter popup should show Today option"
        );
    }

    #[test]
    fn test_help_popup_shows_filter_keybind() {
        let mut state = create_test_state();
        state.active_popup = crate::ActivePopup::Help { scroll: 0 };
        let text = render_to_text(&mut state, 120, 40);
        assert!(
            text.contains("period filter"),
            "Help popup should mention period filter, got:\n{text}"
        );
    }

    #[test]
    fn test_project_popup_renders() {
        let mut state = create_test_state();
        state.active_popup = crate::ActivePopup::ProjectPopup {
            selected: 1,
            scroll: 0,
        };
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("Filter Project"),
            "Project popup should show title, got:\n{text}"
        );
        assert!(
            text.contains("app-a"),
            "Project popup should show project name, got:\n{text}"
        );
    }

    #[test]
    fn test_project_filter_header() {
        let mut state = create_test_state();
        state.project_filter = Some("~/projects/app-a".to_string());
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("app-a"),
            "Header should show project name when filter is active, got:\n{text}"
        );
    }

    #[test]
    fn test_session_detail_popup_past_day_label_shows_date_not_today() {
        // Multi-day session whose slice is from 2 days ago. Pre-fix the
        // popup always labelled the per-day block "[Today]", which lied on
        // both Daily-tab past-day opens and Live opens whose latest activity
        // wasn't today. The label must instead carry the slice's actual
        // date (with weekday) per the project's full-date format rule.
        use crate::aggregator::{DailyGroup, ModelTokens, SessionInfo};
        let today = chrono::Local::now().date_naive();
        let past_date = today - chrono::Duration::days(2);
        let past_first = past_date.and_hms_opt(10, 0, 0).unwrap().and_utc();
        let past_last = past_first + chrono::Duration::hours(1);
        let yesterday_first = (today - chrono::Duration::days(1))
            .and_hms_opt(10, 0, 0)
            .unwrap()
            .and_utc();
        let yesterday_last = yesterday_first + chrono::Duration::hours(1);
        let mut tokens = std::collections::HashMap::new();
        tokens.insert(
            "claude-sonnet-4-20250514".to_string(),
            ModelTokens {
                input_tokens: 1000,
                output_tokens: 200,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                cache_creation_5m_tokens: 0,
                cache_creation_1h_tokens: 0,
                non_standard_speed: false,
            },
        );
        let past_slice = SessionInfo {
            verified_cwd: None,
            file_path: std::path::PathBuf::from("/tmp/past.jsonl"),
            project_name: "x".to_string(),
            git_branch: None,
            session_first_timestamp: past_first,
            model: Some("claude-sonnet-4-20250514".to_string()),
            day_input_tokens: 1000,
            day_output_tokens: 200,
            day_user_msgs: 1,
            day_prompts: 1,
            day_assistant_msgs: 1,
            day_tokens_by_model: tokens.clone(),
            day_hourly_activity: Default::default(),
            day_hourly_work_tokens: Default::default(),
            day_tool_usage: Default::default(),
            day_language_usage: Default::default(),
            day_extension_usage: Default::default(),
            day_first_timestamp: past_first,
            day_last_timestamp: past_last,
            summary: None,
            custom_title: None,
            ai_title: None,
            last_user_message: None,
            first_user_message: None,
            is_subagent: false,
            parent_path: None,
            is_continued: false,
            line_churn: Default::default(),
        };
        // Second slice the next day so cumulative.days > 1 → multi_day
        // path triggers the per-day [date] header.
        let later_slice = SessionInfo {
            day_first_timestamp: yesterday_first,
            day_last_timestamp: yesterday_last,
            ..past_slice.clone()
        };
        let groups = vec![
            DailyGroup {
                date: today - chrono::Duration::days(1),
                sessions: vec![later_slice],
            },
            DailyGroup {
                date: past_date,
                sessions: vec![past_slice.clone()],
            },
        ];
        let mut state = crate::test_helpers::helpers::make_test_app_state(groups.clone());
        state.original_daily_groups = groups;
        // Park the cursor on the past-day group and request its detail.
        state.tab = crate::Tab::Daily;
        state.selected_day = 1;
        state.selected_session = 0;
        state.active_popup = crate::ActivePopup::Detail;

        let text = render_popup_text(&mut state, 140, 40);
        let expected = past_date.format("[%Y-%m-%d (%a)]").to_string();
        assert!(
            text.contains(&expected),
            "past-day popup header must contain {expected:?} — got:\n{text}"
        );
        // The hardcoded "[Today]" header must not leak into a past-day view.
        assert!(
            !text.contains("[Today]"),
            "past-day popup must not label its slice as [Today] — got:\n{text}"
        );
    }

    // A mixed-model session (one priced, one unpriced) must mark BOTH the
    // Cost row (lower-bound "*") and the unpriced model's breakdown row
    // ("$?") — an unmarked $0 next to a marked total reads as a free model.
    #[test]
    fn session_detail_marks_unpriced_model_in_cost_and_breakdown() {
        use crate::test_helpers::helpers::{
            make_daily_group, make_session_with_tokens, make_test_app_state,
        };
        let date = chrono::NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(); // lint-ok: date-literal
        let mut session = make_session_with_tokens("~/proj", 1000, 500, "claude-sonnet-4-6");
        session.day_tokens_by_model.insert(
            "mystery-model-x".to_string(),
            crate::aggregator::TokenStats {
                input_tokens: 200,
                output_tokens: 100,
                ..Default::default()
            },
        );
        let mut state = make_test_app_state(vec![make_daily_group(date, vec![session])]);
        state.tab = crate::Tab::Daily;
        state.selected_day = 0;
        state.selected_session = 0;
        state.active_popup = crate::ActivePopup::Detail;

        let text = render_popup_text(&mut state, 140, 40);
        assert!(
            text.contains("$?"),
            "unpriced model's breakdown row must show $? — got:\n{text}"
        );
        assert!(
            text.contains('*'),
            "mixed-model Cost row must carry the lower-bound mark — got:\n{text}"
        );
    }

    #[test]
    fn test_session_detail_recent_conversation_renders_and_loading_state() {
        use crate::test_helpers::helpers::{make_daily_group, make_session, make_test_app_state};
        let date = chrono::Local::now().date_naive();
        let groups = vec![make_daily_group(
            date,
            vec![make_session("~/proj", None, Some("main"))],
        )];
        let mut state = make_test_app_state(groups.clone());
        state.original_daily_groups = groups;
        state.tab = crate::Tab::Daily;
        state.selected_day = 0;
        state.selected_session = 0;
        state.active_popup = crate::ActivePopup::Detail;

        // Loaded → section renders the messages with role glyphs. Scope to
        // the popup rect so background panels can't satisfy the assertions.
        state.session_detail_recent = Some(vec![
            (
                "user".to_string(),
                "how should the preview look".to_string(),
            ),
            ("assistant".to_string(), "one line per message".to_string()),
        ]);
        let text = render_popup_text(&mut state, 140, 40);
        assert!(
            text.contains("Recent conversation"),
            "section header: {text}"
        );
        assert!(
            text.contains("how should the preview look"),
            "user message text: {text}"
        );
        assert!(
            text.contains('❯') && text.contains('⬡'),
            "user/assistant role glyphs: {text}"
        );
        // Footer exposes both the summary gateway and the direct title write.
        assert!(
            text.contains("s: summary") && text.contains("t: title"),
            "session detail footer must show s: summary and t: title: {text}"
        );

        // Still loading (background task not yet returned) → placeholder.
        state.session_detail_recent = None;
        let text = render_popup_text(&mut state, 140, 40);
        assert!(
            text.contains("Recent conversation") && text.contains("Loading"),
            "loading placeholder: {text}"
        );
    }

    // ── Live tab render tests ───────────────────────────────────────────
    // Live discovery needs real disk state, so tests inject pre-built
    // `LiveSession` records into `live_active` / `live_paused` directly.

    fn live_session_fixture(
        id: &str,
        cwd: &str,
        status: Option<&str>,
        updated_at_secs_ago: i64,
    ) -> crate::infrastructure::live_sessions::LiveSession {
        use chrono::Utc;
        use std::path::PathBuf;
        crate::infrastructure::live_sessions::LiveSession {
            session_id: id.to_string(),
            jsonl_path: Some(PathBuf::from(format!("/tmp/{id}.jsonl"))),
            cwd: PathBuf::from(cwd),
            name: None,
            status: status.map(str::to_string),
            pid: 0,
            started_at: None,
            updated_at: Some(Utc::now() - chrono::Duration::seconds(updated_at_secs_ago)),
            jsonl_mtime: None,
            is_live: status.is_some(),
            was_recently_live: false,
        }
    }

    #[test]
    fn test_live_tab_renders_with_busy_today_older_glyphs() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Live;
        // 30s ago = busy. 1h ago = still today (local calendar). 48h ago =
        // definitely older. Avoid 24h offset which races the midnight boundary.
        let mut today_row = live_session_fixture("bbbb-today", "/Users/me/repo-today", None, 3600);
        today_row.is_live = true;
        let mut older_row = live_session_fixture("cccc-older", "/Users/me/repo-older", None, 0);
        older_row.is_live = true;
        older_row.updated_at = Some(chrono::Utc::now() - chrono::Duration::hours(48));
        state.live_active = vec![
            live_session_fixture("aaaa-busy", "/Users/me/repo-busy", Some("busy"), 30),
            today_row,
            older_row,
        ];
        let text = render_to_text(&mut state, 140, 50);
        assert!(text.contains("Active now (3)"), "section header: {text}");
        assert!(text.contains("🟢"), "busy glyph missing: {text}");
        assert!(text.contains("◉"), "today glyph missing: {text}");
        assert!(text.contains("○"), "older glyph missing: {text}");
    }

    #[test]
    fn test_live_tab_fresh_session_no_jsonl_shows_started_range_and_no_activity() {
        // A just-spawned session whose JSONL hasn't been written yet has no
        // SessionInfo in daily_groups. Instead of a bare "—" row, ccsight
        // must render the started-at range (from pid.json) and an explicit
        // "(no activity yet)" label so the row isn't an opaque ghost.
        let mut state = create_test_state();
        state.tab = crate::Tab::Live;
        let mut fresh = live_session_fixture("fresh-no-jsonl", "/Users/me/work", Some("idle"), 0);
        // No JSONL on disk → no meta. started_at is what pid.json supplies.
        fresh.jsonl_path = Some(std::path::PathBuf::from("/tmp/does-not-exist.jsonl"));
        fresh.jsonl_mtime = None;
        fresh.started_at = Some(chrono::Utc::now() - chrono::Duration::minutes(4));
        fresh.is_live = true;
        state.live_active = vec![fresh];
        let text = render_to_text(&mut state, 140, 50);
        assert!(
            text.contains("(no activity yet)"),
            "fresh session should show '(no activity yet)': {text}"
        );
        // The range column must render rather than being omitted (as it
        // would when both meta and started_at are absent). The `–` en-dash
        // separating start–last is the range's signature — `[` alone also
        // matches unrelated TUI chrome.
        assert!(
            text.contains('–'),
            "started-at range (start–last) should render for a fresh session: {text}"
        );
    }

    #[test]
    fn test_live_tab_renders_snapshot_recovered_paused_with_glyph() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Live;
        let mut paused = live_session_fixture("recovered", "/Users/me/r", None, 24 * 3600);
        paused.was_recently_live = true;
        state.live_paused = vec![paused];
        let text = render_to_text(&mut state, 140, 50);
        assert!(
            text.contains("Recently paused (1)"),
            "paused header: {text}"
        );
        assert!(
            text.contains("⟳"),
            "snapshot-recovered glyph missing: {text}"
        );
    }

    #[test]
    fn test_live_tab_row_has_three_lines_per_session() {
        // metadata + title + ❯ message — even if title/message are "—" the
        // row should still occupy 3 visual lines so auto-scroll math
        // (row idx → body line 1 + idx * 3) stays consistent.
        let mut state = create_test_state();
        state.tab = crate::Tab::Live;
        state.live_active = vec![live_session_fixture("only-row", "/tmp", Some("busy"), 0)];
        let text = render_to_text(&mut state, 140, 50);
        let lines: Vec<&str> = text.lines().collect();
        // "Active now" is the frame title (border row). Body line 0 is the
        // status breakdown, then the 3-line row: rank, title, ❯ message.
        let header_idx = lines
            .iter()
            .position(|l| l.contains("Active now"))
            .expect("Active now header");
        assert!(
            lines[header_idx + 2].contains(" 1 "),
            "rank row: {:?}",
            lines[header_idx + 2]
        );
        assert!(
            lines[header_idx + 4].contains('❯'),
            "third row should contain ❯ marker: {:?}",
            lines[header_idx + 4]
        );
    }

    #[test]
    fn test_live_tab_empty_paused_frame_is_minimal_height() {
        // With no paused sessions the paused frame must shrink to its 1-line
        // placeholder at the very bottom — not reserve the 3/5 cap's worth of
        // empty space — so the active list gets the rest of the tab.
        let mut state = create_test_state();
        state.tab = crate::Tab::Live;
        state.live_active = vec![live_session_fixture("only-row", "/tmp", Some("busy"), 0)];
        state.live_paused = Vec::new();
        let text = render_to_text(&mut state, 140, 50);
        let lines: Vec<&str> = text.lines().collect();
        let paused_row = lines
            .iter()
            .position(|l| l.contains("Recently paused (0)"))
            .expect("paused frame title");
        // Body is 49 rows (1 footer); a minimal 3-row paused frame sits at the
        // bottom, so its title lands within the last few lines.
        assert!(
            paused_row >= lines.len() - 5,
            "empty paused frame must be minimal at the bottom — title at row {paused_row} of {}",
            lines.len()
        );
        assert!(
            text.contains("No paused sessions"),
            "placeholder must still render inside the minimal frame:\n{text}"
        );
    }

    #[test]
    fn test_live_tab_two_frame_scroll_crossing_and_cap() {
        // The headline split feature: with more content than fits, the active
        // frame is capped (paused stays visible) and j/k flows continuously
        // across the boundary while each frame scrolls independently.
        let mut state = create_test_state();
        state.tab = crate::Tab::Live;
        state.live_active = (0..12)
            .map(|i| live_session_fixture(&format!("act{i}"), "/tmp/a", Some("busy"), 0))
            .collect();
        state.live_paused = (0..8)
            .map(|i| live_session_fixture(&format!("pause{i}"), "/tmp/p", None, 3600))
            .collect();

        // Last active row: active content exceeds the cap, so cursor-follow
        // must scroll the active frame.
        state.live_selected = 11;
        let _ = render_to_text(&mut state, 140, 40);
        assert!(
            state.live_scroll > 0,
            "active frame must scroll to reveal the last active row; got {}",
            state.live_scroll
        );

        // Cross into the first paused row: BOTH frames stay on screen (cap
        // works), paused snaps to its top, and the active frame keeps its
        // scroll (independent per-frame viewport).
        state.live_selected = 12;
        let prev_active_scroll = state.live_scroll;
        let text = render_to_text(&mut state, 140, 40);
        assert!(
            text.contains("Active now (12)") && text.contains("Recently paused (8)"),
            "both frames must remain visible under the cap: {text}"
        );
        assert_eq!(
            state.live_paused_scroll, 0,
            "first paused row snaps the paused frame to its top"
        );
        assert_eq!(
            state.live_scroll, prev_active_scroll,
            "active frame keeps its scroll while the cursor is in the paused frame"
        );

        // Last paused row: the paused frame scrolls to reveal it.
        state.live_selected = 19;
        let _ = render_to_text(&mut state, 140, 40);
        assert!(
            state.live_paused_scroll > 0,
            "paused frame must scroll to reveal the last paused row; got {}",
            state.live_paused_scroll
        );
    }

    #[test]
    fn test_live_tab_footer_has_required_keys() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Live;
        let text = render_to_text(&mut state, 140, 50);
        for key in [
            "?:help",
            "q:quit",
            "↑↓:session",
            "i:info",
            "Enter:view",
            "Space:pin",
            "y:copy-resume",
            "←→:snapshot",
            "m:pins",
        ] {
            assert!(text.contains(key), "Live footer missing {key:?}: {text}");
        }
    }

    #[test]
    fn resolved_title_prefers_cache_over_slice() {
        // Rename updates only the single-source cache, so it must win over the
        // slice's own title; a cache miss falls back to the slice for sessions
        // not yet aggregated into the map.
        let mut s = crate::test_helpers::helpers::make_session_with_tokens("p", 1, 1, "m");
        s.custom_title = Some("slice-old".to_string());
        let mut titles = std::collections::HashMap::new();
        assert_eq!(
            resolved_title(&titles, &s),
            Some("slice-old"),
            "miss -> slice"
        );
        titles.insert(s.file_path.clone(), "cache-new".to_string());
        assert_eq!(
            resolved_title(&titles, &s),
            Some("cache-new"),
            "hit -> cache"
        );
    }

    #[test]
    fn test_title_edit_popup_renders_prefilled() {
        // ASCII title: TestBackend pads a wide (CJK) glyph with a space in its
        // continuation cell, so an ASCII string round-trips cleanly here; the
        // real terminal renders either correctly.
        let mut state = create_test_state();
        let mut input = crate::TextInput::default();
        input.set("renamed-session-title".to_string());
        state.active_popup = crate::ActivePopup::TitleEdit {
            input,
            path: std::path::PathBuf::from("/tmp/x.jsonl"),
            return_to: crate::TitleEditReturn::Root,
        };
        let text = render_to_text(&mut state, 120, 35);
        assert!(text.contains("Edit session title"), "title bar: {text}");
        assert!(
            text.contains("renamed-session-title"),
            "prefilled text: {text}"
        );
        assert!(
            text.contains("^R") && text.contains("AI"),
            "footer explains ^R → AI: {text}"
        );
    }

    #[test]
    fn test_live_pane_mode_full_screens_active_or_paused() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Live;
        state.live_active = vec![live_session_fixture("act", "/Users/me/a", Some("busy"), 0)];
        state.live_paused = vec![live_session_fixture("pau", "/Users/me/b", None, 3600)];

        // Split: both frames visible.
        state.live_pane_mode = crate::LivePaneMode::Split;
        let text = render_to_text(&mut state, 140, 45);
        assert!(
            text.contains("Active now") && text.contains("Recently paused"),
            "split shows both frames: {text}"
        );

        // Active-only: paused frame hidden (zero height → no title).
        state.live_pane_mode = crate::LivePaneMode::ActiveOnly;
        state.live_selected = 0;
        let text = render_to_text(&mut state, 140, 45);
        assert!(
            text.contains("Active now"),
            "active-only keeps active: {text}"
        );
        assert!(
            !text.contains("Recently paused"),
            "active-only hides paused: {text}"
        );

        // Paused-only: active frame hidden; cursor moves into the paused range.
        state.live_pane_mode = crate::LivePaneMode::PausedOnly;
        state.live_selected = state.live_active.len();
        let text = render_to_text(&mut state, 140, 45);
        assert!(
            text.contains("Recently paused"),
            "paused-only keeps paused: {text}"
        );
        assert!(
            !text.contains("Active now"),
            "paused-only hides active: {text}"
        );
    }

    #[test]
    fn test_live_row_cost_and_tokens_are_session_lifetime_total() {
        // A live row's cost/tokens must be the session's full lifetime total
        // summed across every day it appears, not just the most-recent day's
        // slice. Two days for the same file_path: latest = 3.80K work tokens,
        // older = 1.20K → lifetime 5.00K must render (and 3.80K must not).
        use crate::test_helpers::helpers::{
            make_daily_group, make_session_with_tokens, make_test_app_state,
        };
        let path = std::path::PathBuf::from("/tmp/multi.jsonl");
        let today = chrono::Local::now().date_naive();
        let older_date = today - chrono::Duration::days(2);

        let mut recent = make_session_with_tokens("~/proj", 3000, 800, "claude-sonnet-4-6");
        recent.file_path = path.clone();
        let mut older = make_session_with_tokens("~/proj", 1000, 200, "claude-sonnet-4-6");
        older.file_path = path.clone();

        let groups = vec![
            make_daily_group(today, vec![recent]),
            make_daily_group(older_date, vec![older]),
        ];
        let mut state = make_test_app_state(groups.clone());
        state.original_daily_groups = groups;
        state.tab = crate::Tab::Live;

        let mut row = live_session_fixture("multi", "/Users/me/repo", Some("busy"), 30);
        row.is_live = true;
        state.live_active = vec![row];

        let text = render_to_text(&mut state, 140, 50);
        // Scope to the live row itself ("just now" age) — the Insights tab
        // label also carries a token figure and must not satisfy these.
        let row_line = text.lines().find(|l| l.contains("just now")).unwrap_or("");
        assert!(
            row_line.contains("5.00K"),
            "Live tokens must be the lifetime total 1.20K+3.80K=5.00K, got:\n{text}"
        );
        assert!(
            !row_line.contains("3.80K"),
            "Live must not show only the latest day's tokens (3.80K), got:\n{text}"
        );
    }

    #[test]
    fn paused_sorted_by_last_activity_with_restorable_pinned() {
        use crate::aggregator::DailyGroup;
        use chrono::{Duration, Utc};
        // Three paused sessions with distinct last-activity (day_last_timestamp
        // in groups). All jsonl_mtime are None so only last-activity (and the
        // restorable pin) can order them. The OLDEST is restorable → it must
        // pin to the top despite being least recent; the rest go newest-first.
        let now = Utc::now();
        let mk_session = |id: &str, last: chrono::DateTime<Utc>| {
            let mut s = crate::test_helpers::helpers::make_session("~/proj", None, Some("main"));
            s.file_path = std::path::PathBuf::from(format!("/tmp/{id}.jsonl"));
            s.day_last_timestamp = last;
            s
        };
        let groups = vec![DailyGroup {
            date: now.date_naive(),
            sessions: vec![
                mk_session("p-old", now - Duration::days(3)),
                mk_session("p-new", now - Duration::hours(1)),
                mk_session("p-mid", now - Duration::days(1)),
            ],
        }];
        let mut state = crate::test_helpers::helpers::make_test_app_state(groups.clone());
        state.original_daily_groups = groups;

        let mut old = live_session_fixture("p-old", "/Users/me/r", None, 0);
        old.was_recently_live = true;
        let mid = live_session_fixture("p-mid", "/Users/me/r", None, 0);
        let new = live_session_fixture("p-new", "/Users/me/r", None, 0);
        // Inject deliberately out of order to prove the sort reorders them.
        state.live_paused = vec![mid, old, new];

        sort_paused_by_recency(&mut state);
        let order: Vec<&str> = state
            .live_paused
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        assert_eq!(
            order,
            vec!["p-old", "p-new", "p-mid"],
            "restorable pinned first, then last-activity desc"
        );
    }

    #[test]
    fn test_live_tab_past_view_renders_snapshot_header_and_today_hint() {
        // Park the view on the most-recent past snapshot (offset = 1)
        // with synthetic meta so the header carries a captured_at clock
        // and the (offset/total) position indicator. Verifies the new
        // multi-snapshot semantics — header MUST include the wall-clock
        // (HH:MM) time and the position fraction.
        let mut state = create_test_state();
        state.tab = crate::Tab::Live;
        state.live_view_snapshot_offset = 1;
        state.live_past_snapshot_total = 1;
        let captured_at = chrono::Utc::now() - chrono::Duration::hours(2);
        let snap_date = captured_at.with_timezone(&chrono::Local).date_naive();
        state.live_past_snapshot_meta = Some((captured_at, snap_date));
        state.live_past_sessions = vec![live_session_fixture(
            "frozen-1",
            "/Users/me/work/project",
            None,
            3600,
        )];
        let text = render_to_text(&mut state, 140, 50);
        // Header includes "Alive at YYYY-MM-DD (Xxx) HH:MM (1/1) (1)".
        let snap_local = captured_at.with_timezone(&chrono::Local);
        let expected_header_prefix = format!("Alive at {}", snap_local.format("%Y-%m-%d (%a)"));
        assert!(
            text.contains(&expected_header_prefix),
            "past header missing {expected_header_prefix:?}: {text}"
        );
        assert!(
            text.contains("(1/1)"),
            "past header should show (offset/total): {text}"
        );
        assert!(text.contains("T:now"), "past footer missing T:now: {text}");
        assert!(
            text.contains("t:title"),
            "past footer missing t:title: {text}"
        );
        assert!(
            text.contains("←→:snapshot"),
            "past footer missing ←→:snapshot: {text}"
        );
        // Title bar advertises the time-travel directions so `←/→` is
        // discoverable rather than hidden in the footer.
        assert!(
            text.contains("newer") && text.contains("T now"),
            "past title should hint `→ newer · T now`: {text}"
        );
        // Today-only "Recently paused" section must NOT appear in past view.
        assert!(
            !text.contains("Recently paused"),
            "past view must not render Recently paused: {text}"
        );
    }

    #[test]
    fn test_live_past_view_shows_diff_vs_current_active() {
        // Past snapshot froze two sessions; the current alive set keeps one
        // ("still"), drops the other ("gone"), and adds one ("fresh"). The past
        // view must summarize 1 live / 1 ended / 1 new and mark still-live rows.
        let mut state = create_test_state();
        state.tab = crate::Tab::Live;
        state.live_view_snapshot_offset = 1;
        state.live_past_snapshot_total = 1;
        let captured_at = chrono::Utc::now() - chrono::Duration::hours(2);
        let snap_date = captured_at.with_timezone(&chrono::Local).date_naive();
        state.live_past_snapshot_meta = Some((captured_at, snap_date));
        state.live_past_sessions = vec![
            live_session_fixture("still", "/Users/me/a", None, 3600),
            live_session_fixture("gone", "/Users/me/b", None, 3600),
        ];
        state.live_active = vec![
            live_session_fixture("still", "/Users/me/a", Some("busy"), 0),
            live_session_fixture("fresh", "/Users/me/c", Some("busy"), 0),
        ];
        let text = render_to_text(&mut state, 140, 50);
        assert!(text.contains("vs now:"), "missing diff summary: {text}");
        // Each count carries its row glyph, so the header doubles as the legend.
        assert!(text.contains("● 1 live"), "expected 1 still-live: {text}");
        assert!(text.contains("· 1 ended"), "expected 1 ended: {text}");
        assert!(text.contains("1 new"), "expected 1 new-since: {text}");
        // Still-live rows carry the `●` diff glyph (unambiguous — the ended `·`
        // collides with the continued-session marker, so assert on `●`).
        assert!(text.contains("●"), "still-live glyph ● missing: {text}");
    }

    #[test]
    fn past_view_row_ages_count_back_from_the_snapshot() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Live;
        state.live_view_snapshot_offset = 1;
        state.live_past_snapshot_total = 1;
        let captured_at = chrono::Utc::now() - chrono::Duration::hours(2);
        let snap_date = captured_at.with_timezone(&chrono::Local).date_naive();
        state.live_past_snapshot_meta = Some((captured_at, snap_date));
        state.live_past_sessions = vec![
            live_session_fixture("idle", "/Users/me/a", None, 3 * 3600 + 120),
            live_session_fixture("kept-going", "/Users/me/b", None, 1800),
        ];
        let text = render_to_text(&mut state, 140, 50);
        assert!(
            text.contains("1h ago"),
            "idle row counts from the snapshot: {text}"
        );
        assert!(!text.contains("3h ago"), "wall-clock age leaked: {text}");
        assert!(text.contains("continued"), "post-snapshot activity: {text}");
    }

    #[test]
    fn the_daily_session_header_names_the_row_glyphs_it_counts() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Daily;
        let text = render_to_text(&mut state, 140, 45);
        assert!(text.contains("· new: "), "{text}");
        assert!(text.contains("» continued: "), "{text}");
    }

    #[test]
    fn a_session_row_and_its_detail_carry_the_subagents_it_launched() {
        use crate::test_helpers::helpers::{make_daily_group, make_session_with_tokens};
        let mut parent = make_session_with_tokens("proj", 1_000_000, 0, "claude-opus-4-6");
        parent.file_path = std::path::PathBuf::from("/p/slug/pid.jsonl");
        let mut sub = make_session_with_tokens("proj", 1_000_000, 0, "claude-opus-4-6");
        sub.file_path = std::path::PathBuf::from("/p/slug/pid/subagents/agent-a.jsonl");
        sub.is_subagent = true;
        sub.parent_path = Some(parent.file_path.clone());
        let calculator = crate::aggregator::CostCalculator::global();
        let own = parent.cost(calculator);
        let groups = vec![make_daily_group(
            chrono::Local::now().date_naive(),
            vec![parent, sub],
        )];
        let mut state = crate::test_helpers::helpers::make_test_app_state(groups.clone());
        state.original_daily_groups = groups;
        state.tab = crate::Tab::Daily;
        let row = render_to_text(&mut state, 140, 45);
        let total = format_cost(2.0 * own, 0);
        assert!(
            row.contains(&format!(" proj  2.00M {total} ")),
            "row sums the subagent's tokens and cost: {row}"
        );

        state.active_popup = crate::ActivePopup::ProjectDetail {
            path: "proj".to_string(),
            scroll: 0,
        };
        let project = render_popup_text(&mut state, 140, 45);
        assert!(
            project.contains(&format!(
                "Cost  {}   Tokens 2.00M",
                format_cost(2.0 * own, 2)
            )),
            "project total carries the subagent: {project}"
        );

        state.active_popup = crate::ActivePopup::Detail;
        let popup = render_popup_text(&mut state, 140, 45);
        let split = format!(
            "(session {} + subagents {})",
            format_cost(own, 2),
            format_cost(own, 2)
        );
        assert!(popup.contains(&split), "{split}: {popup}");
        assert!(popup.contains("+ subagents work:1.00M"), "{popup}");
        assert!(popup.contains(&format_cost(2.0 * own, 2)), "{popup}");
    }

    #[test]
    fn test_live_tab_empty_state_renders_cleanly() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Live;
        // No live_active, no live_paused — should render the "No active"
        // and "No paused" placeholders rather than panic / blank.
        let text = render_to_text(&mut state, 140, 50);
        assert!(
            text.contains("Active now (0)"),
            "empty active header: {text}"
        );
        assert!(
            text.contains("No active sessions"),
            "empty placeholder: {text}"
        );
        assert!(
            text.contains("Recently paused (0)"),
            "empty paused header: {text}"
        );
    }

    #[test]
    fn test_search_popup_is_content_forward() {
        let mut state = create_test_state();
        // daily_groups = [past (empty), today (1 session)]; index the today one.
        let day = state.daily_groups.len() - 1;
        {
            let s = &mut state.daily_groups[day].sessions[0];
            s.summary = Some("scheduler lock rework".to_string());
            s.git_branch = Some("feature/smp-scheduler".to_string());
            s.first_user_message = Some("opening line about the run queue".to_string());
        }
        // Rebuild the title cache so `resolved_title` sees the new summary.
        state.session_titles = crate::aggregator::meta_by_path(&state.daily_groups)
            .into_iter()
            .filter_map(|(p, m)| m.display_title().map(|t| (p.to_path_buf(), t.to_string())))
            .collect();
        state.search_mode = true;
        state.search_input.set("scheduler".to_string());
        state.search_results = vec![crate::search::SearchResult {
            day_idx: day,
            session_idx: 0,
            snippet: Some("…run-queue locks in scheduler order but the…".to_string()),
            match_type: crate::search::SearchMatchType::Content,
            session_path: Some("/tmp/test.jsonl".to_string()),
            message_date: None,
        }];

        let text = render_to_text(&mut state, 140, 45);
        let row = text
            .lines()
            .find(|l| l.contains("scheduler lock rework"))
            .expect("title should lead a result row");
        let title_col = row.find("scheduler lock rework").unwrap();
        let meta_col = row
            .find("test-project#smp-scheduler")
            .expect("meta on line 1");
        // Content-forward: title left of the right-aligned project#branch meta.
        assert!(title_col < meta_col, "title must lead the metadata: {row}");
        assert!(row.contains("· 2026"), "date present in meta: {row}");
        // Line 2 carries the matched snippet, and the query term inside it
        // renders BOLD (the visual cue for WHY the row matched).
        assert!(
            text.contains("run-queue locks in scheduler order"),
            "snippet on line 2: {text}"
        );
        let buffer = render_buffer(&mut state, 140, 45);
        let mut bold_hit = false;
        for y in 0..45u16 {
            for x in 0..131u16 {
                let run: String = (x..(x + 9).min(139))
                    .map(|xx| buffer[(xx, y)].symbol())
                    .collect();
                if run == "scheduler"
                    && buffer[(x, y)].style().add_modifier.contains(Modifier::BOLD)
                {
                    bold_hit = true;
                }
            }
        }
        assert!(bold_hit, "query term must render bold somewhere");
    }

    #[test]
    fn test_search_quick_path_rows_bold_title_and_preview() {
        // Branch/summary matches carry no snippet; the query must still
        // render bold in the title and the opening-message preview.
        let mut state = create_test_state();
        let day = state.daily_groups.len() - 1;
        {
            let s = &mut state.daily_groups[day].sessions[0];
            s.summary = Some("scheduler lock rework".to_string());
            s.first_user_message = Some("the scheduler wakeup path races".to_string());
        }
        state.session_titles = crate::aggregator::meta_by_path(&state.daily_groups)
            .into_iter()
            .filter_map(|(p, m)| m.display_title().map(|t| (p.to_path_buf(), t.to_string())))
            .collect();
        state.search_mode = true;
        state.search_input.set("scheduler".to_string());
        state.search_results = vec![crate::search::SearchResult {
            day_idx: day,
            session_idx: 0,
            snippet: None,
            match_type: crate::search::SearchMatchType::GitBranch,
            session_path: Some("/tmp/test.jsonl".to_string()),
            message_date: None,
        }];

        let buffer = render_buffer(&mut state, 140, 45);
        let mut bold_rows: Vec<u16> = Vec::new();
        for y in 0..45u16 {
            for x in 0..131u16 {
                let run: String = (x..(x + 9).min(139))
                    .map(|xx| buffer[(xx, y)].symbol())
                    .collect();
                if run == "scheduler"
                    && buffer[(x, y)].style().add_modifier.contains(Modifier::BOLD)
                {
                    bold_rows.push(y);
                }
            }
        }
        assert!(
            bold_rows.len() >= 2,
            "bold in both title and preview rows, got rows {bold_rows:?}"
        );
    }

    #[test]
    fn highlight_terms_survives_non_ascii_text() {
        let base = Style::default();
        let hit = Style::default().add_modifier(Modifier::BOLD);
        // Em dash before the match — lowercase byte offsets differ from
        // original offsets, so the map-back must stay on char boundaries.
        let spans = highlight_terms("locks — the Scheduler path", "scheduler", base, hit);
        let bolded: Vec<&str> = spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(
            bolded,
            vec!["Scheduler"],
            "case-insensitive match survives —"
        );

        // CJK text around an ASCII term.
        let spans = highlight_terms("スケジューラの scheduler を直す", "scheduler", base, hit);
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect::<String>();
        assert_eq!(
            joined, "スケジューラの scheduler を直す",
            "text survives intact"
        );
        assert!(
            spans
                .iter()
                .any(|s| s.content == "scheduler" && s.style.add_modifier.contains(Modifier::BOLD)),
            "term bolded amid CJK"
        );

        // Multiple terms, multiple occurrences.
        let spans = highlight_terms("dma race in the dma engine", "dma race", base, hit);
        let bold_count = spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            .count();
        assert_eq!(bold_count, 3, "both dma hits and the race hit");
    }

    // ── Property tests: rendering ──────────────────────────────────────────────
    // Nested module to keep proptest's prelude out of the main test namespace.
    mod render_prop {
        use super::render_to_text;
        use crate::test_helpers::helpers::{
            make_daily_group, make_session_with_tokens, make_test_app_state,
        };
        use proptest::prelude::*;

        fn arb_groups() -> impl Strategy<Value = Vec<crate::aggregator::DailyGroup>> {
            proptest::collection::vec(
                (
                    0i64..40,
                    proptest::collection::vec((0u64..200_000, 0u64..200_000), 0..4),
                ),
                0..5,
            )
            .prop_map(|days| {
                let base = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(); // lint-ok: date-literal
                days.into_iter()
                    .enumerate()
                    .map(|(i, (off, sessions))| {
                        let date = base + chrono::Duration::days(off + i as i64);
                        let sess = sessions
                            .into_iter()
                            .map(|(inp, out)| {
                                make_session_with_tokens("~/proj", inp, out, "claude-sonnet-4-6")
                            })
                            .collect();
                        make_daily_group(date, sess)
                    })
                    .collect()
            })
        }

        /// Every overlay, so the property reaches popup inner-width math —
        /// where this codebase's width panics and truncation bugs live. Index
        /// rather than a `prop_oneof!` of values: the variants carry input
        /// buffers and paths that add nothing to the layout arithmetic.
        fn popup_at(i: usize) -> crate::ActivePopup {
            use crate::ActivePopup as P;
            match i {
                1 => P::Help { scroll: 0 },
                2 => P::ProjectDetail {
                    path: "~/proj".to_string(),
                    scroll: 0,
                },
                3 => P::Summary { scroll: 0 },
                4 => P::Detail,
                5 => P::DashboardDetail,
                6 => P::InsightsDetail { scroll: 0 },
                7 => P::FilterPopup {
                    selected: 0,
                    input_mode: false,
                    input: crate::TextInput::default(),
                    input_error: false,
                },
                8 => P::ProjectPopup {
                    selected: 0,
                    scroll: 0,
                },
                9 => P::TitleEdit {
                    input: crate::TextInput::default(),
                    path: std::path::PathBuf::from("~/proj/s.jsonl"),
                    return_to: crate::TitleEditReturn::default(),
                },
                _ => P::None,
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(128))]

            // Rendering any tab or overlay over arbitrary data at any terminal
            // size must not panic — exercises the saturating_sub layout math.
            #[test]
            fn render_never_panics(
                groups in arb_groups(),
                tab_i in 0usize..crate::Tab::ALL.len(),
                popup_i in 0usize..10,
                conv in any::<bool>(),
                pane_count in 0usize..=crate::state::MAX_PANES,
                msgs_per_pane in 0usize..3,
                // Half the samples land in the cramped range. Uniform 1..200
                // spends most cases on widths no layout guard reacts to, so a
                // popup's inner-width math is barely sampled at gate case counts.
                w in prop_oneof![1u16..24, 24u16..200],
                h in prop_oneof![1u16..16, 16u16..80],
            ) {
                let mut state = make_test_app_state(groups);
                state.tab = crate::Tab::ALL[tab_i];
                state.active_popup = popup_at(popup_i);
                // The conversation view has layout arithmetic the tab views
                // never reach (session-list width, per-pane splitting), gated
                // on a non-empty pane list. Vary the flag and the pane count
                // independently: deriving one from the other skips either the
                // tab views or the empty-pane guard.
                state.show_conversation = conv;
                state.panes = (0..pane_count)
                    .map(|_| {
                        let mut pane = crate::ConversationPane::default();
                        // An empty pane short-circuits before the per-message
                        // wrap / scroll arithmetic, so seed a few messages.
                        pane.messages = std::sync::Arc::new(
                            (0..msgs_per_pane).map(make_message).collect(),
                        );
                        pane
                    })
                    .collect();
                state.active_pane_index = (pane_count > 0).then_some(0);
                let _ = render_to_text(&mut state, w, h);
            }
        }

        fn make_message(i: usize) -> crate::ConversationMessage {
            crate::ConversationMessage {
                role: if i.is_multiple_of(2) {
                    "user"
                } else {
                    "assistant"
                }
                .to_string(),
                blocks: vec![crate::ConversationBlock::Text(format!(
                    "message {i} with enough words to wrap at a narrow width"
                ))],
                timestamp: None,
                timestamp_utc: None,
                model: None,
                tokens: None,
                usage: None,
            }
        }
    }
}
