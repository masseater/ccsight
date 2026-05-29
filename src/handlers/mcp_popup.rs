//! MCP detail-popup helpers — server enumeration, cursor row math, scroll
//! clamping. Used by the keyboard handler (cursor nav) and the popup renderer
//! (rendered server-list order).

use crate::AppState;
use crate::aggregator::{ToolCategory, classify_tool};

/// Synthetic group name for built-in tools when they are rendered alongside MCP
/// servers in the Tools detail popup (active==0). Treated as a single
/// expandable group with the same row layout as an MCP server.
pub(crate) const BUILTIN_GROUP_NAME: &str = "Built-in";

/// Returns the sorted list of group names shown in the Tools detail Tools tab.
/// Includes the synthetic Built-in group (when there are built-in tools), every
/// MCP server seen in logs, AND every configured-but-never-used MCP server
/// (stale-never) so the cursor can visit them just like used servers. Sort
/// order matches the popup rendering: calls desc, name asc (tiebreak).
pub(crate) fn collect_mcp_servers(state: &AppState) -> Vec<String> {
    use std::collections::HashMap;
    let mut per_server: HashMap<String, usize> = HashMap::new();
    let mut builtin_total: usize = 0;
    for (key, count) in &state.stats.tool_usage {
        match classify_tool(key) {
            ToolCategory::Mcp { server } => {
                *per_server.entry(server).or_insert(0) += count;
            }
            ToolCategory::BuiltIn => {
                builtin_total += count;
            }
            _ => {}
        }
    }
    // Surface configured-but-unused servers as zero-call rows so they share
    // the same cursor / sort space as used ones in the popup. Without this
    // the popup body could show 42 rows while the cursor could only navigate
    // 31, leaving stale-never rows un-selectable.
    for status in &state.mcp_status {
        if status.configured {
            per_server.entry(status.name.clone()).or_insert(0);
        }
    }
    let mut rows: Vec<(String, usize)> = per_server.into_iter().collect();
    if builtin_total > 0 {
        rows.push((BUILTIN_GROUP_NAME.to_string(), builtin_total));
    }
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows.into_iter().map(|(n, _)| n).collect()
}

/// Number of distinct tools attributed to the given group in `tool_usage`.
/// For the synthetic Built-in group, counts every BuiltIn-classified tool.
pub(crate) fn mcp_tool_count(state: &AppState, server: &str) -> usize {
    if server == BUILTIN_GROUP_NAME {
        return state
            .stats
            .tool_usage
            .keys()
            .filter(|k| matches!(classify_tool(k), ToolCategory::BuiltIn))
            .count();
    }
    state
        .stats
        .tool_usage
        .keys()
        .filter(|k| matches!(classify_tool(k), ToolCategory::Mcp { server: ref s } if s == server))
        .count()
}

/// Row span (1 header row + optional expanded tool rows) of a given server in the MCP
/// tab rendering. Used to keep the cursor visible while navigating with j/k.
fn row_span_of_server(state: &AppState, server: &str) -> usize {
    if !state.mcp_expanded_servers.contains(server) {
        return 1;
    }
    1 + mcp_tool_count(state, server)
}

/// Logical row offset (0-origin) of the current MCP cursor within the tab body, counting
/// every server header + each expanded tool row above it.
fn mcp_cursor_row(state: &AppState, servers: &[String]) -> usize {
    let mut row = 0usize;
    for (i, server) in servers.iter().enumerate() {
        if i == state.mcp_selected_server {
            if let Some(t) = state.mcp_selected_tool {
                row += 1 + t;
            }
            return row;
        }
        row += row_span_of_server(state, server);
    }
    row
}

/// Number of body rows rendered before the first group row in the Tools tab.
///
/// Body layout (must mirror `draw_dashboard_detail_popup`'s active==0 branch):
///   [summary]               — 1 row, always
///   [stale legend]          — 1 row, present iff any stale MCP server
///   [group rows...]         — start here (Built-in synthetic + MCP servers)
///
/// **Coord-unity rule**: `dashboard_scroll[3]` (popup scroll) and the
/// `body_cursor` returned by `mcp_cursor_row(...) + mcp_pre_server_offset(...)`
/// MUST agree on what counts as "row 0". Mixing "header-included" scroll
/// with "header-excluded" cursor causes off-by-N drift where the cursor is
/// mathematically in view but rendered outside the slice. Whenever a header
/// row is added or removed in the draw fn, this offset must be updated.
fn mcp_pre_server_offset(state: &AppState) -> usize {
    // Delegate the "stale" definition to `McpServerStatus::is_underutilized`
    // — the canonical source-of-truth — so this offset, the legend, the
    // per-row ⚠ marker, and the Tier 3 alert all share one threshold.
    // Reimplementing the check inline (e.g. `> 30`) drifts off-by-one
    // against the legend (`>= 30`) and produces mismatched displays.
    let now = chrono::Utc::now();
    let any_stale = state.mcp_status.iter().any(|s| s.is_underutilized(now, 30));
    1 + usize::from(any_stale)
}

/// Keep the MCP cursor visible by clamping `dashboard_scroll[3]` so the cursor row sits
/// inside the popup viewport. Visible body height matches the actual rendered body slice
/// in `draw_dashboard_detail_popup` (popup inner = `height - 2`, minus 4 pinned header
/// rows: blank + Total + tab bar + separator). `dashboard_scroll[3]` and `body_cursor`
/// share the same coord system: body[0..pre_server_offset] = summary (+ optional legend),
/// body[pre_server_offset..] = servers/tools.
pub(crate) fn adjust_mcp_scroll(state: &mut AppState, servers: &[String]) {
    // Pinned header rows above the scrollable body: blank + Total + tab bar + separator.
    // The summary (and optional legend) lives in body[0..] and scrolls with the rest.
    const HEADER_ROWS: usize = 4;
    let visible_body = state.active_popup_area.map_or(20, |a| {
        (a.height as usize).saturating_sub(2 + HEADER_ROWS).max(1)
    });
    // mcp_cursor_row returns server-relative (server 0 = row 0); add the pre-server
    // offset (summary + optional legend) so body_cursor matches the rendered slice.
    let body_cursor = mcp_cursor_row(state, servers) + mcp_pre_server_offset(state);
    let scroll = &mut state.dashboard_scroll[3];
    if body_cursor < *scroll {
        *scroll = body_cursor;
    } else if body_cursor >= *scroll + visible_body {
        *scroll = body_cursor + 1 - visible_body;
    }
}
