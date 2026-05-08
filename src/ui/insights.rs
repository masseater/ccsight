use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::AppState;
use super::theme;
use super::cost_style;

/// Trailing-N-months `(YYYY-MM, cost)` pairs ending at today, oldest first.
/// Months absent from `monthly_costs` are filled with `0.0` so the chart
/// range agrees with the heatmap (which shows a fixed trailing window) and
/// the inline / popup `Monthly` views share the same denominator.
fn trailing_n_months(
    monthly_costs: &std::collections::BTreeMap<String, f64>,
    n: usize,
) -> Vec<(String, f64)> {
    use chrono::Local;
    trailing_n_months_at(monthly_costs, n, Local::now().date_naive())
}

fn trailing_n_months_at(
    monthly_costs: &std::collections::BTreeMap<String, f64>,
    n: usize,
    now: chrono::NaiveDate,
) -> Vec<(String, f64)> {
    use chrono::Datelike;
    let mut result: Vec<(String, f64)> = Vec::with_capacity(n);
    for i in (0..n).rev() {
        let total_months_back = i as i32;
        let mut year = now.year();
        let mut month = now.month() as i32 - total_months_back;
        while month <= 0 {
            month += 12;
            year -= 1;
        }
        let key = format!("{year}-{month:02}");
        let cost = monthly_costs.get(&key).copied().unwrap_or(0.0);
        result.push((key, cost));
    }
    result
}

pub(super) fn draw_insights(frame: &mut Frame, area: Rect, state: &mut AppState) {
    use chrono::{Datelike, Local, Timelike, Weekday};

    // Metrics block: 2 KPI card rows + 1 ecosystem row (MCP active · stale · Used breakdown).
    // Grows by 1 when any model lacks pricing so we can surface the "silent $0" risk
    // prominently. Without that conditional row users only see the warning deep in the
    // Models detail popup.
    let metrics_height: u16 = if state.models_without_pricing.is_empty() {
        5
    } else {
        6
    };
    let chunks = Layout::vertical([
        Constraint::Length(metrics_height), // Unified metrics (2 cards + ecosystem [+ Pricing gap])
        Constraint::Fill(2),                // Today vs Avg (main, scales with terminal)
        Constraint::Min(9),                 // Bottom section (weekly + monthly)
        Constraint::Length(1),              // Help
    ])
    .split(area);

    let selected_panel = state.insights_panel;

    // Unified metrics block
    let metrics_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if selected_panel == 0 {
            theme::PRIMARY
        } else {
            theme::BORDER
        }))
        .title(Span::styled(
            " Metrics ",
            Style::default().fg(theme::PRIMARY),
        ));
    let metrics_inner = metrics_block.inner(chunks[0]);
    frame.render_widget(metrics_block, chunks[0]);

    // Calculate all metrics
    let total_sessions: usize = state
        .daily_groups
        .iter()
        .map(|g| g.user_sessions().count())
        .sum();
    let today = chrono::Local::now().date_naive();
    let first_date = state.daily_groups.iter().map(|g| g.date).min();
    let calendar_days = match first_date {
        Some(first) => (today - first).num_days() as usize + 1,
        _ => 1,
    };
    let avg_cost_per_day = state.total_cost / calendar_days as f64;

    let cache_read = state.stats.total_tokens.cache_read_tokens;
    let input_tokens = state.stats.total_tokens.input_tokens;
    let cache_hit_rate = if input_tokens + cache_read > 0 {
        cache_read as f64 / (input_tokens + cache_read) as f64 * 100.0
    } else {
        0.0
    };

    let total_tool_calls = state.stats.tool_success_count + state.stats.tool_error_count;
    let tool_success_rate = if total_tool_calls > 0 {
        state.stats.tool_success_count as f64 / total_tool_calls as f64 * 100.0
    } else {
        0.0
    };

    let completion_rate = if state.stats.total_sessions_count > 0 {
        state.stats.sessions_with_summary as f64 / state.stats.total_sessions_count as f64 * 100.0
    } else {
        0.0
    };

    let total_work_tokens = state.stats.total_tokens.work_tokens();
    let tokens_per_session = if total_sessions > 0 {
        total_work_tokens / total_sessions as u64
    } else {
        0
    };
    let tokens_per_day = total_work_tokens / calendar_days as u64;

    // Row layout: 2 metric cards rows + MCP activation + Adoption [+ Pricing gap]
    // Pricing gap row is rendered only when at least one model lacks pricing (see
    // `metrics_height` above). Extra `Length(1)` below is harmless if unused.
    let row_constraints: Vec<Constraint> = (0..metrics_height.saturating_sub(2))
        .map(|_| Constraint::Length(1))
        .collect();
    let row_chunks = Layout::vertical(row_constraints).split(metrics_inner);
    let row1_chunks = Layout::horizontal([
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
    ])
    .split(row_chunks[0]);
    let row2_chunks = Layout::horizontal([
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
    ])
    .split(row_chunks[1]);

    let row1_items: [(String, &str, ratatui::style::Color); 4] = [
        (format!("{cache_hit_rate:.1}%"), "cache", theme::SUCCESS),
        (
            format!("{tool_success_rate:.1}%"),
            "success",
            if tool_success_rate >= 90.0 {
                theme::SUCCESS
            } else {
                theme::WARNING
            },
        ),
        (
            // Match the cache / success neighbours and the popup body — all
            // three rate metrics print with one decimal so the row reads as
            // a uniform set rather than mixing `:.0` and `:.1` precisions.
            format!("{completion_rate:.1}%"),
            "summary",
            if completion_rate >= 80.0 {
                theme::SUCCESS
            } else {
                theme::WARNING
            },
        ),
        (
            format!("${:.1}/day", avg_cost_per_day.max(0.0)),
            "cost",
            theme::SECONDARY,
        ),
    ];

    // Each card occupies ~metrics_inner.width / 4 columns. At 60×20 that's ~14
    // chars, so labels longer than ~16 must shorten to avoid clipping.
    let card_width = (metrics_inner.width / 4) as usize;
    let label_density = if card_width >= 16 { "density" } else { "den" };
    let row2_items: [(String, &str, ratatui::style::Color); 4] = [
        (
            format!("{}/day", crate::format_number(tokens_per_day)),
            "tokens",
            theme::PRIMARY,
        ),
        (
            format!("{}/ses", crate::format_number(tokens_per_session)),
            label_density,
            theme::SECONDARY,
        ),
        (
            format!("{}", state.stats.tool_usage.len()),
            "tools",
            theme::SECONDARY,
        ),
        (format!("{total_sessions}"), "sessions", theme::PRIMARY),
    ];

    for (i, (value, label, color)) in row1_items.iter().enumerate() {
        let card = Paragraph::new(Line::from(vec![
            Span::styled(value.as_str(), Style::default().fg(*color).bold()),
            Span::styled(format!(" {label}"), Style::default().fg(theme::DIM)),
        ]))
        .centered();
        frame.render_widget(card, row1_chunks[i]);
    }

    for (i, (value, label, color)) in row2_items.iter().enumerate() {
        let card = Paragraph::new(Line::from(vec![
            Span::styled(value.as_str(), Style::default().fg(*color).bold()),
            Span::styled(format!(" {label}"), Style::default().fg(theme::DIM)),
        ]))
        .centered();
        frame.render_widget(card, row2_chunks[i]);
    }

    // Row 3: Ecosystem — `N/M MCP active · K stale  ·  Used: MCP X · Skills Y · Subagents Z`.
    // Per-server stale list with day-since-use lives in the Metrics detail popup (`i`).
    // The Used counts are absolute (not %) because the three categories have different
    // invocation semantics (1 Skill = context load; 1 Subagent = child session; 1 MCP
    // call = external request) so cross-category percentages would mislead.
    let now = chrono::Utc::now();
    let configured_count = state.mcp_status.iter().filter(|s| s.configured).count();
    let active_count = state
        .mcp_status
        .iter()
        .filter(|s| s.configured && !s.is_underutilized(now, 30))
        .count();
    let stale_count = state
        .mcp_status
        .iter()
        .filter(|s| s.is_underutilized(now, 30))
        .count();
    // At narrow widths the full Ecosystem line overflows on the right, hiding
    // the Subagent count. Below ~80 cols we drop label words ("active", "stale",
    // "Used:", category names) and keep just the numbers + minimal markers, which
    // stays scannable while fitting in 60 cols.
    let row_width = row_chunks[2].width as usize;
    let mut eco_spans = vec![
        Span::styled(
            format!("{active_count}/{configured_count}"),
            Style::default().fg(theme::PRIMARY).bold(),
        ),
        Span::styled(
            if row_width >= 80 { " MCP active" } else { " MCP" },
            Style::default().fg(theme::DIM),
        ),
    ];
    if stale_count > 0 {
        eco_spans.push(Span::styled("  ·  ", Style::default().fg(theme::DIM)));
        eco_spans.push(Span::styled(
            if row_width >= 80 {
                format!("{stale_count} stale")
            } else {
                format!("{stale_count}⚠")
            },
            Style::default().fg(theme::WARNING),
        ));
    }
    // Numbers here are session counts (`sessions_using_*`). The Dashboard
    // Ecosystem panel shows call counts under similar labels, so spell the
    // unit out to keep the two views distinguishable.
    let used_prefix = if row_width >= 80 { "    Sessions using:  " } else { "  ses: " };
    let mcp_label = if row_width >= 80 { "MCP " } else { "M" };
    let skill_label = if row_width >= 80 { "Skills " } else { "Sk" };
    let command_label = if row_width >= 80 { "Commands " } else { "Cm" };
    let agent_label = if row_width >= 80 { "Subagents " } else { "Sb" };
    let sep = if row_width >= 80 { "  ·  " } else { " · " };
    // Order matches the popup tabs: MCP → Skills → Commands → Subagents.
    eco_spans.extend([
        Span::styled(used_prefix, Style::default().fg(theme::DIM)),
        Span::styled(mcp_label, Style::default().fg(theme::CAT_MCP)),
        Span::styled(
            format!("{}", state.stats.sessions_using_mcp),
            Style::default().fg(theme::CAT_MCP).bold(),
        ),
        Span::styled(sep, Style::default().fg(theme::DIM)),
        Span::styled(skill_label, Style::default().fg(theme::CAT_SKILLS)),
        Span::styled(
            format!("{}", state.stats.sessions_using_skills),
            Style::default().fg(theme::CAT_SKILLS).bold(),
        ),
        Span::styled(sep, Style::default().fg(theme::DIM)),
        Span::styled(command_label, Style::default().fg(theme::CAT_COMMANDS)),
        Span::styled(
            format!("{}", state.stats.sessions_using_commands),
            Style::default().fg(theme::CAT_COMMANDS).bold(),
        ),
        Span::styled(sep, Style::default().fg(theme::DIM)),
        Span::styled(agent_label, Style::default().fg(theme::CAT_SUBAGENTS)),
        Span::styled(
            format!("{}", state.stats.sessions_using_subagents),
            Style::default().fg(theme::CAT_SUBAGENTS).bold(),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(Line::from(eco_spans)).centered(),
        row_chunks[2],
    );

    // Row 4 (conditional): Pricing gap warning — shown only when one or more models
    // used in the view lack a pricing entry. Without this row the silent-$0 condition
    // would only be visible in the Models detail popup.
    if !state.models_without_pricing.is_empty()
        && let Some(gap_row) = row_chunks.get(3)
    {
        let count = state.models_without_pricing.len();
        let mut names: Vec<&String> = state.models_without_pricing.iter().collect();
        names.sort();
        let preview: String = names
            .iter()
            .take(3)
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let suffix = if names.len() > 3 {
            format!(" +{} more", names.len() - 3)
        } else {
            String::new()
        };
        let gap_spans = vec![
            Span::styled(
                "⚠ Pricing gap  ",
                Style::default().fg(theme::WARNING).bold(),
            ),
            Span::styled(
                format!("{count} models"),
                Style::default().fg(theme::WARNING),
            ),
            Span::styled("  ·  ", Style::default().fg(theme::DIM)),
            Span::styled(preview, Style::default().fg(theme::DIM)),
            Span::styled(suffix, Style::default().fg(theme::DIM)),
        ];
        frame.render_widget(
            Paragraph::new(Line::from(gap_spans)).centered(),
            *gap_row,
        );
    }

    // Today vs Average - Cumulative graph
    let today = Local::now().date_naive();
    let current_hour = Local::now().hour() as u8;

    let mut hourly_total: std::collections::HashMap<u8, u64> = std::collections::HashMap::new();
    for group in &state.daily_groups {
        for session in &group.sessions {
            if session.is_subagent {
                continue;
            }
            for (hour, tokens) in &session.day_hourly_work_tokens {
                *hourly_total.entry(*hour).or_insert(0) += tokens;
            }
        }
    }
    let hourly_avg: std::collections::HashMap<u8, u64> = hourly_total
        .iter()
        .map(|(h, t)| (*h, *t / calendar_days as u64))
        .collect();

    let mut today_hourly: std::collections::HashMap<u8, u64> = std::collections::HashMap::new();
    if let Some(today_group) = state.daily_groups.iter().find(|g| g.date == today) {
        for session in today_group.user_sessions() {
            for (hour, tokens) in &session.day_hourly_work_tokens {
                *today_hourly.entry(*hour).or_insert(0) += tokens;
            }
        }
    }

    // Calculate cumulative values
    let mut today_cumulative = [0u64; 24];
    let mut avg_cumulative = [0u64; 24];
    let mut running_today = 0u64;
    let mut running_avg = 0u64;
    for hour in 0..24u8 {
        running_today += today_hourly.get(&hour).copied().unwrap_or(0);
        running_avg += hourly_avg.get(&hour).copied().unwrap_or(0);
        today_cumulative[hour as usize] = running_today;
        avg_cumulative[hour as usize] = running_avg;
    }

    let today_total = running_today;
    let avg_total = running_avg;
    let max_cumulative = today_total.max(avg_total).max(1);

    let graph_height = chunks[1].height.saturating_sub(4).max(1) as usize;
    let graph_width = chunks[1].width.saturating_sub(12).max(1) as usize;

    let mut graph_lines: Vec<Line> = Vec::new();

    // Y-axis labels and graph rows - line chart style
    let is_top_row = |r: usize| r == graph_height - 1;
    for row in (0..graph_height).rev() {
        let threshold_low = row as f64 / graph_height as f64 * max_cumulative as f64;
        let threshold_high = (row as f64 + 1.0) / graph_height as f64 * max_cumulative as f64;

        let y_label = if row == graph_height - 1 {
            crate::format_number(max_cumulative)
        } else if row == graph_height / 2 {
            crate::format_number(max_cumulative / 2)
        } else if row == 0 {
            "0".to_string()
        } else {
            String::new()
        };

        let mut row_spans: Vec<Span> = Vec::new();
        for col in 0..graph_width {
            let hour = (col * 24 / graph_width).min(23) as u8;
            let is_future = hour > current_hour;
            let today_val = today_cumulative[hour as usize] as f64;
            let avg_val = avg_cumulative[hour as usize] as f64;

            let today_in_row = !is_future
                && today_val >= threshold_low
                && (today_val < threshold_high || is_top_row(row));
            let avg_in_row =
                avg_val >= threshold_low && (avg_val < threshold_high || is_top_row(row));
            let today_below = !is_future && today_val >= threshold_high && !is_top_row(row);
            let avg_below = avg_val >= threshold_high && !is_top_row(row);

            let (ch, color) = if today_in_row && avg_in_row {
                ('●', theme::WARNING)
            } else if today_in_row {
                ('●', theme::SUCCESS)
            } else if avg_in_row {
                ('○', theme::LABEL_MUTED)
            } else if today_below && avg_below {
                ('│', theme::SEPARATOR)
            } else if today_below {
                ('│', theme::HEATMAP_LOW)
            } else if avg_below {
                ('┆', theme::FAINT)
            } else {
                (' ', theme::DIM)
            };
            row_spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
        }

        let mut line_spans = vec![
            Span::styled(
                format!("{y_label:>5} "),
                Style::default().fg(theme::LABEL_MUTED),
            ),
            Span::raw("│"),
        ];
        line_spans.extend(row_spans);
        graph_lines.push(Line::from(line_spans));
    }

    // X-axis
    let mut x_axis = String::new();
    x_axis.push_str("      └");
    for _ in 0..graph_width {
        x_axis.push('─');
    }
    graph_lines.push(Line::from(Span::styled(
        x_axis,
        Style::default().fg(theme::DIM),
    )));

    // Hour labels
    let mut hour_labels = "       ".to_string();
    let step = graph_width / 6;
    for i in 0..=6 {
        let hour = i * 4;
        let pos = i * step;
        while hour_labels.len() < 7 + pos {
            hour_labels.push(' ');
        }
        hour_labels.push_str(&format!("{hour:<4}"));
    }
    graph_lines.push(Line::from(Span::styled(
        hour_labels,
        Style::default().fg(theme::LABEL_MUTED),
    )));

    // Current time marker with progress
    let current_pos = (current_hour as usize * graph_width / 24) + 7;
    let day_progress = ((current_hour as f32 + 1.0) / 24.0 * 100.0) as u8;
    let mut marker_spans: Vec<Span> = Vec::new();
    marker_spans.push(Span::raw(" ".repeat(current_pos)));
    marker_spans.push(Span::styled(
        "▲",
        Style::default()
            .fg(theme::PRIMARY)
            .add_modifier(Modifier::BOLD),
    ));
    marker_spans.push(Span::styled(
        format!(" now {current_hour}:00 "),
        Style::default()
            .fg(theme::PRIMARY)
            .add_modifier(Modifier::BOLD),
    ));
    marker_spans.push(Span::styled(
        format!("[{day_progress}%]"),
        Style::default().fg(theme::SUCCESS),
    ));
    graph_lines.push(Line::from(marker_spans));

    let diff_pct = if avg_total > 0 {
        (today_total as f64 / avg_total as f64 * 100.0) as i32
    } else {
        0
    };
    let today_cost = state
        .daily_costs
        .iter()
        .find(|(d, _)| *d == today)
        .map_or(0.0, |(_, c)| *c);

    let today_block = Paragraph::new(graph_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if selected_panel == 1 {
                theme::PRIMARY
            } else {
                theme::BORDER
            }))
            .title(Span::styled(
                " Today vs Average (cumulative) ",
                Style::default().fg(theme::PRIMARY),
            ))
            .title_bottom(Line::from(vec![
                Span::styled(" ●", Style::default().fg(theme::SUCCESS)),
                Span::styled("Today", Style::default().fg(theme::SUCCESS)),
                Span::styled("  ○", Style::default().fg(theme::TEXT_BRIGHT)),
                Span::styled("full-day avg", Style::default().fg(theme::TEXT_BRIGHT)),
                Span::styled(
                    format!("  │ {}", crate::format_number(today_total)),
                    Style::default()
                        .fg(theme::SUCCESS)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" / {} ", crate::format_number(avg_total)),
                    Style::default().fg(theme::TEXT_BRIGHT),
                ),
                Span::styled(
                    format!("({diff_pct}%) "),
                    Style::default().fg(if diff_pct > 100 {
                        theme::WARNING
                    } else {
                        theme::SUCCESS
                    }),
                ),
                Span::styled(
                    format!(" ${:.2} ", today_cost.max(0.0)),
                    cost_style(today_cost),
                ),
            ])),
    );
    frame.render_widget(today_block, chunks[1]);

    // Bottom section: Weekly and Monthly side by side
    let bottom_chunks =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(chunks[2]);

    state.insights_panel_areas = vec![
        chunks[0],        // 0: Metrics
        chunks[1],        // 1: Today vs Average
        bottom_chunks[0], // 2: Weekly
        bottom_chunks[1], // 3: Monthly
    ];

    // Monthly trend (compact)
    let monthly_costs = super::aggregate_monthly_costs(&state.daily_costs);
    // Trailing 12 months including zero-cost months so the range mirrors the
    // dashboard heatmap's trailing-year window. Avg divides by 12 (the
    // window size, not just months that happen to have data).
    let trailing_window = trailing_n_months(&monthly_costs, 12);
    let avg_monthly_all: f64 = if trailing_window.is_empty() {
        0.0
    } else {
        trailing_window.iter().map(|(_, c)| *c).sum::<f64>() / trailing_window.len() as f64
    };
    let col_width = 7usize;
    let max_months = ((bottom_chunks[1].width.saturating_sub(3)) as usize / col_width).max(1);
    // Show the most recent `max_months` of the trailing window so a narrow
    // panel keeps the right edge anchored at "this month".
    let months: Vec<(String, f64)> = trailing_window
        .iter()
        .rev()
        .take(max_months)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let months_view: Vec<(&String, &f64)> =
        months.iter().map(|(k, v)| (k, v)).collect();
    let max_monthly = months_view
        .iter()
        .map(|(_, c)| **c)
        .fold(0.0f64, f64::max)
        .max(1.0);

    let mut monthly_lines: Vec<Line> = Vec::new();
    let bar_height = 4usize;

    // Render each month as a stack of cells, one per row. Each cell uses a
    // partial Unicode block based on how much of that row the bar fills, so
    // months with small-but-nonzero cost still produce a visible stub instead
    // of disappearing entirely (the previous threshold-only render dropped
    // anything under 1/8 of the tallest month).
    for row in (0..bar_height).rev() {
        let mut row_spans: Vec<Span> = vec![Span::raw(" ")];
        for (_, cost) in &months_view {
            let ratio = **cost / max_monthly;
            let intensity = (ratio * 0.7 + 0.3).min(1.0);
            let color = theme::primary_with_intensity(intensity);
            // Fractional fill of THIS row: how much of the bar reaches into it.
            let frac = (ratio * bar_height as f64) - row as f64;
            let bar: &str = if frac >= 1.0 {
                "██"
            } else if frac >= 0.75 {
                "▆▆"
            } else if frac >= 0.5 {
                "▄▄"
            } else if frac >= 0.25 {
                "▂▂"
            } else if frac > 0.0 && row == 0 {
                // Floor: any non-zero cost shows at least a 1/8 stub on row 0.
                "▁▁"
            } else {
                "  "
            };
            row_spans.push(Span::styled(
                format!("{bar:^col_width$}"),
                Style::default().fg(color),
            ));
        }
        monthly_lines.push(Line::from(row_spans));
    }

    let avg_monthly = avg_monthly_all;

    let mut label_spans: Vec<Span> = vec![Span::raw(" ")];
    let mut cost_spans: Vec<Span> = vec![Span::raw(" ")];
    let mut diff_spans: Vec<Span> = vec![Span::raw(" ")];
    for (month, cost) in &months_view {
        // `month` is "YYYY-MM"; show as "YY-MM" (ISO 8601 separator) so year
        // transitions (e.g. 25-12 → 26-01) are unambiguous and the format
        // matches the project's date-style rule (no locale-dependent `/`).
        let short_month = month.split_once('-').map_or_else(
            || "??".to_string(),
            |(y, m)| format!("{}-{m}", y.get(2..).unwrap_or(y)),
        );
        label_spans.push(Span::styled(
            format!("{short_month:^col_width$}"),
            Style::default().fg(theme::LABEL_MUTED),
        ));
        cost_spans.push(Span::styled(
            format!(
                "{:^width$}",
                format!("${:.0}", cost.max(0.0)),
                width = col_width
            ),
            Style::default().fg(theme::WARM),
        ));

        let diff_str = if avg_monthly > 0.0 {
            let pct = ((**cost - avg_monthly) / avg_monthly * 100.0) as i32;
            if pct >= 0 {
                format!("+{pct}%")
            } else {
                format!("{pct}%")
            }
        } else {
            "-".to_string()
        };
        let diff_color = if **cost > avg_monthly {
            theme::WARNING
        } else {
            theme::SUCCESS
        };
        diff_spans.push(Span::styled(
            format!("{diff_str:^col_width$}"),
            Style::default().fg(diff_color),
        ));
    }
    monthly_lines.push(Line::from(label_spans));
    monthly_lines.push(Line::from(cost_spans));
    monthly_lines.push(Line::from(diff_spans));

    let forecast_spans = {
        let now = Local::now();
        let current_month_key = format!("{}-{:02}", now.year(), now.month());
        let days_elapsed = now.day() as f64;
        let days_in_month = if now.month() == 12 {
            chrono::NaiveDate::from_ymd_opt(now.year() + 1, 1, 1)
        } else {
            chrono::NaiveDate::from_ymd_opt(now.year(), now.month() + 1, 1)
        }
        .and_then(|d| d.pred_opt())
        .map_or(30.0, |d| d.day() as f64);

        if let Some(current_cost) = monthly_costs.get(&current_month_key) {
            if days_elapsed > 0.0 {
                let forecast = current_cost / days_elapsed * days_in_month;
                let mut spans = vec![
                    Span::styled("this mo: ", Style::default().fg(theme::DIM)),
                    Span::styled(
                        format!("${:.0}", current_cost.max(0.0)),
                        Style::default().fg(theme::PRIMARY),
                    ),
                ];
                // Projection only adds information when more than one day
                // remains; at month-end forecast collapses to current_cost.
                if days_elapsed < days_in_month {
                    spans.push(Span::styled("  ·  proj ", Style::default().fg(theme::DIM)));
                    spans.push(Span::styled(
                        format!("${:.0}", forecast.max(0.0)),
                        Style::default().fg(theme::WARM),
                    ));
                }
                spans
            } else {
                vec![]
            }
        } else {
            vec![]
        }
    };

    let mut bottom_spans = vec![
        Span::styled(" avg: ", Style::default().fg(theme::DIM)),
        Span::styled(
            format!("${:.0}/mo", avg_monthly.max(0.0)),
            Style::default().fg(theme::PRIMARY),
        ),
    ];
    if !forecast_spans.is_empty() {
        bottom_spans.push(Span::styled(" | ", Style::default().fg(theme::DIM)));
        bottom_spans.extend(forecast_spans);
    }
    bottom_spans.push(Span::raw(" "));

    let monthly_block = Paragraph::new(monthly_lines).centered().block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if selected_panel == 3 {
                theme::PRIMARY
            } else {
                theme::BORDER
            }))
            .title(Span::styled(
                " Monthly ",
                Style::default().fg(theme::PRIMARY),
            ))
            .title_bottom(Line::from(bottom_spans)),
    );
    frame.render_widget(monthly_block, bottom_chunks[1]);

    // Weekly activity (all-time average by weekday)
    let weekdays = [
        (Weekday::Mon, "Mo"),
        (Weekday::Tue, "Tu"),
        (Weekday::Wed, "We"),
        (Weekday::Thu, "Th"),
        (Weekday::Fri, "Fr"),
        (Weekday::Sat, "Sa"),
        (Weekday::Sun, "Su"),
    ];
    let today_weekday = today.weekday();
    let first_date = state.daily_groups.last().map_or(today, |g| g.date);
    let weekday_avg =
        super::aggregate_weekday_avg(&state.daily_groups, calendar_days, first_date);
    let max_weekly = weekday_avg.values().max().copied().unwrap_or(1);
    let total_weekly: u64 = weekday_avg.values().sum();

    let mut weekly_lines: Vec<Line> = Vec::new();
    let bar_width = 8usize;

    for (weekday, label) in &weekdays {
        let avg_tokens = weekday_avg.get(weekday).copied().unwrap_or(0);
        let ratio = avg_tokens as f64 / max_weekly as f64;
        let filled = (ratio * bar_width as f64).round() as usize;
        let pct = if total_weekly > 0 {
            (avg_tokens as f64 / total_weekly as f64 * 100.0) as u32
        } else {
            0
        };
        let intensity = (ratio * 0.7 + 0.3).min(1.0);
        let bar_color = theme::primary_with_intensity(intensity);
        let marker = if *weekday == today_weekday {
            "▶"
        } else {
            " "
        };

        weekly_lines.push(Line::from(vec![
            Span::styled(
                format!("{marker}{label} "),
                Style::default().fg(if *weekday == today_weekday {
                    theme::PRIMARY
                } else {
                    theme::LABEL_MUTED
                }),
            ),
            Span::styled("█".repeat(filled), Style::default().fg(bar_color)),
            Span::styled(
                "░".repeat(bar_width - filled),
                Style::default().fg(theme::SEPARATOR),
            ),
            Span::styled(
                format!(" {:>5}", crate::format_number(avg_tokens)),
                Style::default().fg(theme::PRIMARY),
            ),
            Span::styled(format!(" {pct:>2}%"), Style::default().fg(theme::DIM)),
        ]));
    }

    // Use `total_tokens / calendar_days` so this footer agrees with the
    // Insights metrics `tokens/day`. Summing seven integer-divided per-weekday
    // averages drifts because each weekday occurrence count is an int and
    // the per-weekday quotient floors away that fractional remainder.
    let avg_daily_tokens = state.stats.total_tokens.work_tokens() / calendar_days as u64;
    let weekly_block = Paragraph::new(weekly_lines).centered().block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if selected_panel == 2 {
                theme::PRIMARY
            } else {
                theme::BORDER
            }))
            .title(Span::styled(
                " Weekly avg ",
                Style::default().fg(theme::PRIMARY),
            ))
            .title_bottom(Line::from(vec![
                Span::styled(" avg: ", Style::default().fg(theme::DIM)),
                Span::styled(
                    format!("{}/day ", crate::format_number(avg_daily_tokens)),
                    Style::default().fg(theme::PRIMARY),
                ),
            ])),
    );
    frame.render_widget(weekly_block, bottom_chunks[0]);

    let help_spans = vec![
        Span::styled(" ?", Style::default().fg(theme::PRIMARY)),
        Span::styled(":help ", Style::default().fg(theme::DIM)),
        Span::styled("q", Style::default().fg(theme::PRIMARY)),
        Span::styled(":quit ", Style::default().fg(theme::DIM)),
        Span::styled("←→", Style::default().fg(theme::PRIMARY)),
        Span::styled(":panel ", Style::default().fg(theme::DIM)),
        Span::styled("↑↓", Style::default().fg(theme::PRIMARY)),
        Span::styled(":scroll ", Style::default().fg(theme::DIM)),
        Span::styled("Enter", Style::default().fg(theme::PRIMARY)),
        Span::styled(":detail ", Style::default().fg(theme::DIM)),
        Span::styled("/", Style::default().fg(theme::PRIMARY)),
        Span::styled(":search ", Style::default().fg(theme::DIM)),
        Span::styled("m", Style::default().fg(theme::PRIMARY)),
        Span::styled(":pins", Style::default().fg(theme::DIM)),
    ];
    let help_line = Paragraph::new(Line::from(help_spans));
    frame.render_widget(help_line, chunks[3]);
}


pub(super) fn draw_insights_detail_popup(frame: &mut Frame, area: Rect, state: &mut AppState) {
    let popup_width = 80.min(area.width.saturating_sub(4));
    let popup_height = area.height.saturating_sub(4).min(40);

    let popup_area = Rect {
        x: area.x + (area.width.saturating_sub(popup_width)) / 2,
        y: area.y + (area.height.saturating_sub(popup_height)) / 2,
        width: popup_width,
        height: popup_height,
    };

    state.active_popup_area = Some(popup_area);
    frame.render_widget(Clear, popup_area);

    let total_sessions: usize = state
        .daily_groups
        .iter()
        .map(|g| g.user_sessions().count())
        .sum();
    let today = chrono::Local::now().date_naive();
    let first_date = state.daily_groups.iter().map(|g| g.date).min();
    let calendar_days = match first_date {
        Some(first) => (today - first).num_days() as usize + 1,
        _ => 1,
    };
    let active_days = state.daily_groups.len().max(1);

    let cache_read = state.stats.total_tokens.cache_read_tokens;
    let input_tokens = state.stats.total_tokens.input_tokens;
    let cache_hit_rate = if input_tokens + cache_read > 0 {
        cache_read as f64 / (input_tokens + cache_read) as f64 * 100.0
    } else {
        0.0
    };

    let total_tool_calls = state.stats.tool_success_count + state.stats.tool_error_count;
    let tool_success_rate = if total_tool_calls > 0 {
        state.stats.tool_success_count as f64 / total_tool_calls as f64 * 100.0
    } else {
        0.0
    };

    let completion_rate = if state.stats.total_sessions_count > 0 {
        state.stats.sessions_with_summary as f64 / state.stats.total_sessions_count as f64 * 100.0
    } else {
        0.0
    };

    let avg_cost_per_day = state.total_cost / calendar_days as f64;

    let total_work_tokens = state.stats.total_tokens.work_tokens();
    let tokens_per_session = if total_sessions > 0 {
        total_work_tokens / total_sessions as u64
    } else {
        0
    };
    let tokens_per_day = total_work_tokens / calendar_days as u64;


    // Title-cased to match the Dashboard popup convention (`Daily Costs`,
    // `Languages`, `Hourly Average`, etc.) — lowercase abbreviations were
    // an inconsistency that made these popups look like internal/debug labels.
    let panel_labels = ["Metrics", "Today vs Average", "Weekly", "Monthly"];
    let current_panel = state.insights_panel.min(3);
    let panel_label = panel_labels[current_panel];

    let inner_width = popup_width.saturating_sub(4) as usize;

    let mut lines: Vec<Line> = vec![Line::from("")];

    match current_panel {
        0 => {
            let w = inner_width.saturating_sub(2);
            let sep = Line::from(Span::styled("─".repeat(w), Style::default().fg(theme::BORDER)));
            let bar_w = (w / 3).min(16);

            // Cost & sessions overview
            lines.push(Line::from(vec![
                Span::styled(" Total Cost  ", Style::default().fg(theme::DIM)),
                Span::styled(super::format_cost(state.total_cost, 2), cost_style(state.total_cost)),
                Span::styled(
                    format!("  ({calendar_days} days, {active_days} active)"),
                    Style::default().fg(theme::DIM),
                ),
            ]));
            let total_duration_mins: i64 = state.daily_groups.iter()
                .flat_map(crate::aggregator::DailyGroup::user_sessions)
                .map(|s| (s.day_last_timestamp - s.day_first_timestamp).num_minutes().max(1))
                .sum();
            let avg_duration_mins = if total_sessions > 0 { total_duration_mins / total_sessions as i64 } else { 0 };
            let avg_dur_str = if avg_duration_mins >= 60 {
                format!("{}h{}m", avg_duration_mins / 60, avg_duration_mins % 60)
            } else {
                format!("{avg_duration_mins}m")
            };
            lines.push(Line::from(vec![
                Span::styled(" Sessions    ", Style::default().fg(theme::DIM)),
                Span::styled(format!("{total_sessions}"), Style::default().fg(theme::SUCCESS).bold()),
                Span::styled(
                    format!("  ${:.1}/day  {}/day  {}/ses  {avg_dur_str}/ses",
                        avg_cost_per_day.max(0.0),
                        crate::format_number(tokens_per_day),
                        crate::format_number(tokens_per_session)),
                    Style::default().fg(theme::DIM),
                ),
            ]));
            lines.push(sep.clone());

            // Rates with descriptive labels
            let rates: [(f64, &str, ratatui::style::Color); 3] = [
                (cache_hit_rate, "Cache Hit Rate   ", theme::SUCCESS),
                (tool_success_rate, "Tool Success Rate",
                    if tool_success_rate >= 90.0 { theme::SUCCESS } else { theme::WARNING }),
                (completion_rate, "Has Summary      ",
                    if completion_rate >= 80.0 { theme::SUCCESS } else { theme::WARNING }),
            ];
            for (rate, label, color) in &rates {
                let filled = (*rate / 100.0 * bar_w as f64).round() as usize;
                lines.push(Line::from(vec![
                    Span::styled(format!(" {label} "), Style::default().fg(theme::DIM)),
                    Span::styled(format!("{rate:>5.1}%"), Style::default().fg(*color).bold()),
                    Span::raw(" "),
                    Span::styled("█".repeat(filled.min(bar_w)), Style::default().fg(*color)),
                    Span::styled("░".repeat(bar_w.saturating_sub(filled)), Style::default().fg(theme::SEPARATOR)),
                ]));
            }
            lines.push(sep.clone());

            // Tokens (2 lines, key-value style)
            let output = state.stats.total_tokens.output_tokens;
            let cache_write = state.stats.total_tokens.cache_creation_tokens;
            lines.push(Line::from(vec![
                Span::styled(" Input  ", Style::default().fg(theme::DIM)),
                Span::styled(format!("{:<10}", crate::format_number(input_tokens)), Style::default().fg(theme::TEXT_BRIGHT)),
                Span::styled("  Output  ", Style::default().fg(theme::DIM)),
                Span::styled(crate::format_number(output), Style::default().fg(theme::TEXT_BRIGHT)),
            ]));
            lines.push(Line::from(vec![
                Span::styled(" CacheR ", Style::default().fg(theme::DIM)),
                Span::styled(format!("{:<10}", crate::format_number(cache_read)), Style::default().fg(theme::TEXT_BRIGHT)),
                Span::styled("  CacheW  ", Style::default().fg(theme::DIM)),
                Span::styled(crate::format_number(cache_write), Style::default().fg(theme::TEXT_BRIGHT)),
            ]));
            lines.push(sep.clone());

            // Models (blue gradient, matching dashboard detail). Percent
            // labelled `$%` so it doesn't get confused with the Dashboard
            // Models panel's token-share `tok%` column.
            if !state.model_costs.is_empty() {
                let name_w = 12;
                let bar_max = w.saturating_sub(name_w + 20);
                let total_cost: f64 = state.model_costs.iter().map(|(_, c)| *c).sum();
                let max_cost = state.model_costs.iter().map(|(_, c)| *c).fold(0.0f64, f64::max).max(0.01);
                for (model, cost) in state.model_costs.iter().take(5) {
                    let ratio = *cost / max_cost;
                    let pct = if total_cost > 0.0 { (*cost / total_cost * 100.0) as u32 } else { 0 };
                    let filled = (ratio * bar_max as f64).round() as usize;
                    let intensity = (ratio * 0.7 + 0.3).min(1.0);
                    let bar_color = ratatui::style::Color::Rgb(
                        (100.0 + 118.0 * intensity) as u8,
                        (140.0 + 78.0 * intensity) as u8,
                        (200.0 + 55.0 * intensity) as u8,
                    );
                    let name: String = model.chars().take(name_w).collect();
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {name:<name_w$}"), Style::default().fg(theme::PRIMARY)),
                        Span::styled("█".repeat(filled.min(bar_max)), Style::default().fg(bar_color)),
                        Span::styled("░".repeat(bar_max.saturating_sub(filled)), Style::default().fg(theme::SEPARATOR)),
                        Span::styled(format!(" {:>6}", super::format_cost(*cost, 0)), Style::default().fg(theme::WARM)),
                        Span::styled(format!(" {pct:>2}% $"), Style::default().fg(theme::DIM)),
                    ]));
                }
                lines.push(sep.clone());
            }

            // Projects (purple gradient, matching dashboard detail)
            if !state.stats.project_stats.is_empty() {
                let name_w = w.saturating_sub(22).min(24);
                let bar_max = 12;
                let mut projects: Vec<_> = state.stats.project_stats.iter().collect();
                projects.sort_by_key(|p| std::cmp::Reverse(p.1.work_tokens));
                let max_tokens = projects.first().map_or(1, |(_, s)| s.work_tokens);
                for (name, ps) in projects.iter().take(5) {
                    let short = state.project_label(name);
                    let display: String = short.chars().take(name_w).collect();
                    let ratio = ps.work_tokens as f64 / max_tokens as f64;
                    let filled = (ratio * bar_max as f64).round() as usize;
                    let intensity = (ratio * 0.7 + 0.3).min(1.0);
                    let bar_color = ratatui::style::Color::Rgb(
                        (140.0 + 78.0 * intensity) as u8,
                        (100.0 + 68.0 * intensity) as u8,
                        (180.0 + 75.0 * intensity) as u8,
                    );
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {display:<name_w$}"), Style::default().fg(theme::SECONDARY)),
                        Span::styled("█".repeat(filled.min(bar_max)), Style::default().fg(bar_color)),
                        Span::styled("░".repeat(bar_max.saturating_sub(filled)), Style::default().fg(theme::SEPARATOR)),
                        Span::styled(format!(" {:>4} ses", ps.sessions), Style::default().fg(theme::DIM)),
                        Span::styled(format!(" {}", crate::format_number(ps.work_tokens)), Style::default().fg(theme::LABEL_MUTED)),
                    ]));
                }
                lines.push(sep.clone());
            }

            // Tools (Top N per section: Built-in / Skill / Agent / MCP)
            if !state.stats.tool_usage.is_empty() {
                use crate::aggregator::{classify_tool, format_tool_short, ToolCategory};

                let tools: Vec<_> = state.stats.tool_usage.iter().collect();
                let (mut builtin, mut skill, mut agent, mut mcp, mut command): (
                    Vec<&(&String, &usize)>,
                    Vec<&(&String, &usize)>,
                    Vec<&(&String, &usize)>,
                    Vec<&(&String, &usize)>,
                    Vec<&(&String, &usize)>,
                ) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
                for t in tools.iter() {
                    match classify_tool(t.0) {
                        ToolCategory::BuiltIn => builtin.push(t),
                        ToolCategory::Skill { .. } => skill.push(t),
                        ToolCategory::Agent { .. } => agent.push(t),
                        ToolCategory::Mcp { .. } => mcp.push(t),
                        ToolCategory::Command { .. } => command.push(t),
                    }
                }
                // Stable tiebreak by name so equal-count rows don't shuffle on redraw
                // (HashMap iteration order is non-deterministic across rebuilds).
                builtin.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
                skill.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
                agent.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
                mcp.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
                command.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));

                // Bar widths normalize against the per-category top entry, so
                // the `%` column has to use the **same** denominator (category
                // total). Sharing a grand-total denominator across categories
                // would let the bar render full while the `%` rounds to zero
                // for categories that are small relative to the dominant one.
                let builtin_total: usize = builtin.iter().map(|(_, c)| **c).sum();
                let mcp_total: usize = mcp.iter().map(|(_, c)| **c).sum();
                let skill_total: usize = skill.iter().map(|(_, c)| **c).sum();
                let agent_total: usize = agent.iter().map(|(_, c)| **c).sum();
                let command_total: usize = command.iter().map(|(_, c)| **c).sum();
                let name_w = 18;
                let bar_max = w.saturating_sub(name_w + 12);

                let render = |tool_name: String,
                              count: usize,
                              max: usize,
                              category_total: usize,
                              color: ratatui::style::Color,
                              lines: &mut Vec<Line>| {
                    let ratio = count as f64 / max as f64;
                    let pct = if category_total > 0 {
                        (count as f64 / category_total as f64 * 100.0) as u32
                    } else {
                        0
                    };
                    let filled = (ratio * bar_max as f64).round() as usize;
                    let intensity = (ratio * 0.7 + 0.3).min(1.0);
                    let bar_color = ratatui::style::Color::Rgb(
                        (150.0 + 68.0 * intensity) as u8,
                        (180.0 + 38.0 * intensity) as u8,
                        (100.0 + 55.0 * intensity) as u8,
                    );
                    let name: String = {
                        use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
                        if UnicodeWidthStr::width(tool_name.as_str()) <= name_w {
                            tool_name.clone()
                        } else {
                            let mut width = 0usize;
                            let mut result = String::new();
                            for ch in tool_name.chars() {
                                let ch_w = UnicodeWidthChar::width(ch).unwrap_or(0);
                                if width + ch_w > name_w.saturating_sub(1) {
                                    break;
                                }
                                result.push(ch);
                                width += ch_w;
                            }
                            result.push('…');
                            result
                        }
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {name:<name_w$}"), Style::default().fg(color)),
                        Span::styled("█".repeat(filled.min(bar_max)), Style::default().fg(bar_color)),
                        Span::styled("░".repeat(bar_max.saturating_sub(filled)), Style::default().fg(theme::SEPARATOR)),
                        Span::styled(format!(" {count:>5}"), Style::default().fg(theme::LABEL_MUTED)),
                        Span::styled(format!(" {pct:>2}%"), Style::default().fg(theme::DIM)),
                    ]));
                };

                let has_meta = !skill.is_empty() || !agent.is_empty() || !mcp.is_empty();
                // Header makes the segmentation explicit so the % column is
                // readable: each row's percentage is against its own category
                // total, not against the grand total across categories.
                lines.push(Line::from(vec![Span::styled(
                    " Top tools (% per category)",
                    Style::default().fg(theme::DIM),
                )]));
                if !has_meta {
                    // Fallback: top 6 built-in
                    let max_tool = builtin.first().map_or(1, |(_, c)| **c).max(1);
                    for entry in builtin.iter().take(6) {
                        let (tool, count) = entry;
                        render((*tool).clone(), **count, max_tool, builtin_total, theme::CAT_BUILTIN, &mut lines);
                    }
                } else {
                    // Order matches Tools popup tabs: Built-in → MCP → Skills →
                    // Commands → Subagents. Built-in and MCP are SEPARATE blocks
                    // here even though the popup merges them under "Tools": the
                    // raw call counts are wildly imbalanced (built-in 5–10× MCP)
                    // and a merged top-N would silently push every MCP tool out
                    // of view. Splitting "top 3 each" guarantees both surfaces
                    // get scanned at a glance.
                    let max_builtin = builtin.first().map_or(1, |(_, c)| **c).max(1);
                    let max_mcp = mcp.first().map_or(1, |(_, c)| **c).max(1);
                    let max_skill = skill.first().map_or(1, |(_, c)| **c).max(1);
                    let max_agent = agent.first().map_or(1, |(_, c)| **c).max(1);
                    let max_command = command.first().map_or(1, |(_, c)| **c).max(1);
                    for entry in builtin.iter().take(3) {
                        let (tool, count) = entry;
                        render((*tool).clone(), **count, max_builtin, builtin_total, theme::CAT_BUILTIN, &mut lines);
                    }
                    for entry in mcp.iter().take(3) {
                        let (tool, count) = entry;
                        render(format_tool_short(tool), **count, max_mcp, mcp_total, theme::CAT_MCP, &mut lines);
                    }
                    for entry in skill.iter().take(3) {
                        let (tool, count) = entry;
                        render(format_tool_short(tool), **count, max_skill, skill_total, theme::CAT_SKILLS, &mut lines);
                    }
                    for entry in command.iter().take(3) {
                        let (tool, count) = entry;
                        render(format_tool_short(tool), **count, max_command, command_total, theme::CAT_COMMANDS, &mut lines);
                    }
                    for entry in agent.iter().take(3) {
                        let (tool, count) = entry;
                        render(format_tool_short(tool), **count, max_agent, agent_total, theme::CAT_SUBAGENTS, &mut lines);
                    }
                }
                lines.push(sep.clone());
            }

            // Per-category usage — absolute session-day counts only. Cross-category %
            // was removed because the three categories have incommensurable invocation
            // semantics (see note in the Metrics row). Each row shows how many sessions
            // used at least one entry from that category, plus the top-used entry.
            {
                let total = state.stats.total_session_days;
                let skills = state.stats.sessions_using_skills;
                let subagents = state.stats.sessions_using_subagents;
                let mcp_used = state.stats.sessions_using_mcp;
                let commands_used = state.stats.sessions_using_commands;
                lines.push(Line::from(vec![
                    Span::styled(" Usage by category  ", Style::default().fg(theme::DIM)),
                    Span::styled(
                        format!("of {total} session-days"),
                        Style::default().fg(theme::DIM),
                    ),
                ]));
                // Order matches popup tabs: MCP → Skills → Commands → Subagents.
                let rows: [(&str, usize, ratatui::style::Color); 4] = [
                    ("MCP tools", mcp_used, theme::CAT_MCP),
                    ("Skills", skills, theme::CAT_SKILLS),
                    ("Commands", commands_used, theme::CAT_COMMANDS),
                    ("Subagents", subagents, theme::CAT_SUBAGENTS),
                ];
                for (name, used, color) in rows {
                    let prefix = match name {
                        "Skills" => "skill:",
                        "Commands" => "command:",
                        "Subagents" => "agent:",
                        _ => "mcp__",
                    };
                    let top = state
                        .stats
                        .tool_sessions
                        .iter()
                        .filter(|(k, _)| k.starts_with(prefix))
                        .max_by_key(|(_, c)| **c);
                    let top_text = top.map_or(String::new(), |(k, c)| {
                        let display = crate::aggregator::format_tool_short(k);
                        let label = display
                            .strip_prefix("skill:")
                            .or_else(|| display.strip_prefix("agent:"))
                            .or_else(|| display.strip_prefix("command:"))
                            .map_or_else(|| display.clone(), str::to_string);
                        format!("  ·  top: {label} ({c} ses)")
                    });
                    lines.push(Line::from(vec![
                        Span::styled("   ", Style::default()),
                        Span::styled(format!("{name:<11}"), Style::default().fg(color)),
                        Span::styled(
                            format!("{used:>4} ses"),
                            Style::default().fg(color).bold(),
                        ),
                        Span::styled(top_text, Style::default().fg(theme::DIM)),
                    ]));
                }
                lines.push(sep.clone());
            }

            // MCP Activation (configured vs active, underutilized list)
            if !state.mcp_status.is_empty() {
                let now = chrono::Utc::now();
                let configured_count = state.mcp_status.iter().filter(|s| s.configured).count();
                let mut active: Vec<_> = state
                    .mcp_status
                    .iter()
                    .filter(|s| s.configured && !s.is_underutilized(now, 30))
                    .collect();
                // Tiebreak on server name so equal-call servers don't reshuffle on redraw.
                active.sort_by(|a, b| {
                    b.total_calls
                        .cmp(&a.total_calls)
                        .then_with(|| a.name.cmp(&b.name))
                });
                let mut underutilized: Vec<_> = state
                    .mcp_status
                    .iter()
                    .filter(|s| s.is_underutilized(now, 30))
                    .collect();
                underutilized.sort_by(|a, b| {
                    let ad = a.days_since_last_use(now).unwrap_or(i64::MAX);
                    let bd = b.days_since_last_use(now).unwrap_or(i64::MAX);
                    bd.cmp(&ad)
                });

                lines.push(Line::from(vec![
                    Span::styled(" MCP Activation  ", Style::default().fg(theme::DIM)),
                    Span::styled(
                        format!("{}/{configured_count}", active.len()),
                        Style::default().fg(theme::PRIMARY).bold(),
                    ),
                    Span::styled(" active", Style::default().fg(theme::DIM)),
                ]));
                for s in &active {
                    let detail = match s.days_since_last_use(now) {
                        None => format!("{} calls", s.total_calls),
                        Some(0) => format!("{} calls, today", s.total_calls),
                        Some(1) => format!("{} calls, yesterday", s.total_calls),
                        Some(d) => format!("{} calls, {d}d ago", s.total_calls),
                    };
                    lines.push(Line::from(vec![
                        Span::styled("   ", Style::default()),
                        Span::styled(
                            format!("{:<20}", s.name),
                            Style::default().fg(theme::SUCCESS),
                        ),
                        Span::styled(detail, Style::default().fg(theme::DIM)),
                    ]));
                }
                for s in &underutilized {
                    let detail = match s.days_since_last_use(now) {
                        None => "never used".to_string(),
                        Some(0) => "last used today".to_string(),
                        Some(1) => "last used yesterday".to_string(),
                        Some(d) => format!("last used {d}d ago"),
                    };
                    lines.push(Line::from(vec![
                        Span::styled("   ", Style::default()),
                        Span::styled(
                            format!("{:<20}", s.name),
                            Style::default().fg(theme::WARNING),
                        ),
                        Span::styled(detail, Style::default().fg(theme::DIM)),
                    ]));
                }
                lines.push(sep.clone());
            }

            // Languages (teal, matching dashboard languages panel)
            if !state.stats.language_usage.is_empty() {
                let mut langs: Vec<_> = state.stats.language_usage.iter().collect();
                langs.sort_by(|a, b| b.1.cmp(a.1));
                let max_lang = langs.first().map_or(1, |(_, c)| **c).max(1);
                let total_lang: usize = langs.iter().map(|(_, c)| **c).sum();
                let name_w = 12;
                let bar_max = w.saturating_sub(name_w + 12);
                for (lang, count) in langs.iter().take(6) {
                    let ratio = **count as f64 / max_lang as f64;
                    let pct = if total_lang > 0 { (**count as f64 / total_lang as f64 * 100.0) as u32 } else { 0 };
                    let filled = (ratio * bar_max as f64).round() as usize;
                    let intensity = (ratio * 0.7 + 0.3).min(1.0);
                    let bar_color = ratatui::style::Color::Rgb(
                        (40.0 + 46.0 * intensity) as u8,
                        (80.0 + 85.0 * intensity) as u8,
                        (90.0 + 90.0 * intensity) as u8,
                    );
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {name:<name_w$}", name = lang.chars().take(name_w).collect::<String>()), Style::default().fg(theme::LABEL_MUTED)),
                        Span::styled("█".repeat(filled.min(bar_max)), Style::default().fg(bar_color)),
                        Span::styled("░".repeat(bar_max.saturating_sub(filled)), Style::default().fg(theme::SEPARATOR)),
                        Span::styled(format!(" {count:>5}"), Style::default().fg(theme::LABEL_MUTED)),
                        Span::styled(format!(" {pct:>2}%"), Style::default().fg(theme::DIM)),
                    ]));
                }
            }
        }
        1 => {
            use chrono::{Local, Timelike};
            let today = Local::now().date_naive();
            let current_hour = Local::now().hour() as u8;

            let mut hourly_total: std::collections::HashMap<u8, u64> =
                std::collections::HashMap::new();
            for group in &state.daily_groups {
                for session in group.user_sessions() {
                    for (hour, tokens) in &session.day_hourly_work_tokens {
                        *hourly_total.entry(*hour).or_insert(0) += tokens;
                    }
                }
            }
            let hourly_avg: std::collections::HashMap<u8, u64> = hourly_total
                .iter()
                .map(|(h, t)| (*h, *t / calendar_days as u64))
                .collect();

            let mut today_hourly: std::collections::HashMap<u8, u64> =
                std::collections::HashMap::new();
            if let Some(today_group) = state.daily_groups.iter().find(|g| g.date == today) {
                for session in today_group.user_sessions() {
                    for (hour, tokens) in &session.day_hourly_work_tokens {
                        *today_hourly.entry(*hour).or_insert(0) += tokens;
                    }
                }
            }

            // Match the inline panel: today_total = today's running cumulative
            // (zero past current_hour), avg_total = full-day average. This
            // keeps popup `today/avg` percentage in step with the inline
            // footer instead of inventing a same-hour avg that ends up
            // labelled "avg" twice (once in the chart, once in the caption).
            let today_total: u64 = today_hourly.values().sum();
            let full_day_avg: u64 = hourly_avg.values().sum();
            let avg_total = full_day_avg;
            let today_cost = state
                .daily_costs
                .iter()
                .find(|(d, _)| *d == today)
                .map_or(0.0, |(_, c)| *c);

            let diff_pct = if avg_total > 0 {
                (today_total as f64 / avg_total as f64 * 100.0) as i32
            } else {
                0
            };

            let mut today_cumulative = [0u64; 24];
            let mut avg_cumulative = [0u64; 24];
            let mut running_today = 0u64;
            let mut running_avg = 0u64;
            for hour in 0..24u8 {
                running_today += today_hourly.get(&hour).copied().unwrap_or(0);
                running_avg += hourly_avg.get(&hour).copied().unwrap_or(0);
                today_cumulative[hour as usize] = running_today;
                avg_cumulative[hour as usize] = running_avg;
            }
            let max_cumulative = running_today.max(running_avg).max(1);

            let graph_height = 8usize;
            let graph_width = inner_width.saturating_sub(10);

            let is_top_row = |r: usize| r == graph_height - 1;
            for row in (0..graph_height).rev() {
                let threshold_low = row as f64 / graph_height as f64 * max_cumulative as f64;
                let threshold_high =
                    (row as f64 + 1.0) / graph_height as f64 * max_cumulative as f64;

                let y_label = if row == graph_height - 1 {
                    crate::format_number(max_cumulative)
                } else if row == graph_height / 2 {
                    crate::format_number(max_cumulative / 2)
                } else if row == 0 {
                    "0".to_string()
                } else {
                    String::new()
                };

                let mut row_spans: Vec<Span> = Vec::new();
                for col in 0..graph_width {
                    let hour = (col * 24 / graph_width).min(23) as u8;
                    let is_future = hour > current_hour;
                    let today_val = today_cumulative[hour as usize] as f64;
                    let avg_val = avg_cumulative[hour as usize] as f64;

                    let today_in_row = !is_future
                        && today_val >= threshold_low
                        && (today_val < threshold_high || is_top_row(row));
                    let avg_in_row =
                        avg_val >= threshold_low && (avg_val < threshold_high || is_top_row(row));
                    let today_below = !is_future && today_val >= threshold_high && !is_top_row(row);
                    let avg_below = avg_val >= threshold_high && !is_top_row(row);

                    let (ch, color) = if today_in_row && avg_in_row {
                        ('●', theme::WARNING)
                    } else if today_in_row {
                        ('●', theme::SUCCESS)
                    } else if avg_in_row {
                        ('○', theme::LABEL_MUTED)
                    } else if today_below && avg_below {
                        ('│', theme::SEPARATOR)
                    } else if today_below {
                        ('│', theme::HEATMAP_LOW)
                    } else if avg_below {
                        ('┆', theme::FAINT)
                    } else {
                        (' ', theme::DIM)
                    };
                    row_spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
                }

                let mut line_spans = vec![
                    Span::styled(
                        format!(" {y_label:>5} "),
                        Style::default().fg(theme::LABEL_MUTED),
                    ),
                    Span::raw("│"),
                ];
                line_spans.extend(row_spans);
                lines.push(Line::from(line_spans));
            }

            let mut x_axis = String::new();
            x_axis.push_str("       └");
            for _ in 0..graph_width {
                x_axis.push('─');
            }
            lines.push(Line::from(Span::styled(
                x_axis,
                Style::default().fg(theme::DIM),
            )));

            let mut hour_labels = "        ".to_string();
            let step = graph_width / 6;
            for i in 0..=6 {
                let h = i * 4;
                let pos = i * step;
                while hour_labels.len() < 8 + pos {
                    hour_labels.push(' ');
                }
                hour_labels.push_str(&format!("{h:<4}"));
            }
            lines.push(Line::from(Span::styled(
                hour_labels,
                Style::default().fg(theme::LABEL_MUTED),
            )));

            let diff_color = if diff_pct > 100 {
                theme::WARNING
            } else {
                theme::SUCCESS
            };
            lines.push(Line::from(vec![
                Span::styled(" ●", Style::default().fg(theme::SUCCESS)),
                Span::styled(
                    format!("{} today  ", crate::format_number(today_total)),
                    Style::default().fg(theme::SUCCESS).bold(),
                ),
                Span::styled("○", Style::default().fg(theme::TEXT_BRIGHT)),
                Span::styled(
                    format!("{} full-day avg ", crate::format_number(avg_total)),
                    Style::default().fg(theme::TEXT_BRIGHT),
                ),
                Span::styled(format!("({diff_pct}%)"), Style::default().fg(diff_color)),
                Span::styled("  ", Style::default()),
                Span::styled(
                    format!("${:.2}", today_cost.max(0.0)),
                    cost_style(today_cost),
                ),
            ]));
        }
        2 => {
            use chrono::{Datelike, Weekday};
            let weekdays = [
                (Weekday::Mon, "Monday"),
                (Weekday::Tue, "Tuesday"),
                (Weekday::Wed, "Wednesday"),
                (Weekday::Thu, "Thursday"),
                (Weekday::Fri, "Friday"),
                (Weekday::Sat, "Saturday"),
                (Weekday::Sun, "Sunday"),
            ];

            let today_date = chrono::Local::now().date_naive();
            let today_weekday = today_date.weekday();
            let first_date = state
                .daily_groups
                .last()
                .map_or(today_date, |g| g.date);
            let weekday_avg =
                super::aggregate_weekday_avg(&state.daily_groups, calendar_days, first_date);
            let (max_day, max_avg) = weekday_avg
                .iter()
                .max_by_key(|(_, v)| **v)
                .map_or((Weekday::Mon, 0u64), |(k, v)| (*k, *v));
            let max_weekly = max_avg.max(1);
            let total_weekly: u64 = weekday_avg.values().sum();
            let bar_width = inner_width.saturating_sub(28);

            for (wd, label) in &weekdays {
                let avg = weekday_avg.get(wd).copied().unwrap_or(0);
                let ratio = avg as f64 / max_weekly as f64;
                let filled = (ratio * bar_width as f64).round() as usize;
                let pct = if total_weekly > 0 {
                    (avg as f64 / total_weekly as f64 * 100.0) as u32
                } else {
                    0
                };
                let is_today = *wd == today_weekday;
                let marker = if is_today { "▶" } else { " " };
                let intensity = (ratio * 0.7 + 0.3).min(1.0);
                let bar_color = theme::primary_with_intensity(intensity);

                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {marker}{label:<9} "),
                        Style::default().fg(if is_today {
                            theme::PRIMARY
                        } else {
                            theme::LABEL_MUTED
                        }),
                    ),
                    Span::styled(
                        "█".repeat(filled.min(bar_width)),
                        Style::default().fg(bar_color),
                    ),
                    Span::styled(
                        "░".repeat(bar_width.saturating_sub(filled)),
                        Style::default().fg(theme::SEPARATOR),
                    ),
                    Span::styled(
                        format!(" {:>5}/d", crate::format_number(avg)),
                        Style::default().fg(theme::PRIMARY),
                    ),
                    Span::styled(format!(" {pct:>2}%"), Style::default().fg(theme::DIM)),
                ]));
            }

            lines.push(Line::from(""));
            let avg_daily_tokens =
                state.stats.total_tokens.work_tokens() / calendar_days as u64;
            if max_avg > 0 {
                let max_label = weekdays
                    .iter()
                    .find(|(wd, _)| *wd == max_day)
                    .map_or("?", |(_, l)| *l);
                lines.push(Line::from(vec![
                    Span::styled(" Most active: ", Style::default().fg(theme::DIM)),
                    Span::styled(max_label, Style::default().fg(theme::SUCCESS).bold()),
                    Span::styled("  avg: ", Style::default().fg(theme::DIM)),
                    Span::styled(
                        format!("{}/day", crate::format_number(avg_daily_tokens)),
                        Style::default().fg(theme::PRIMARY),
                    ),
                ]));
            } else {
                lines.push(Line::from(Span::styled(
                    " No activity recorded",
                    Style::default().fg(theme::DIM),
                )));
            }
        }
        3 => {
            use chrono::Datelike;
            let monthly_costs = super::aggregate_monthly_costs(&state.daily_costs);
            // Per-month token totals lets users compare cost vs throughput
            // month-to-month. Subagent sessions are excluded to match the cost
            // path's accounting upstream.
            let monthly_tokens = super::aggregate_monthly_tokens(&state.daily_groups);

            // Trailing window covers max(data months, 12) so the popup keeps
            // the inline panel's 12-month minimum (with zero months filled
            // when data is sparse) and still surfaces older history when the
            // user has more than 12 months of data.
            let window_size = monthly_costs.len().max(12);
            let trailing_window = trailing_n_months(&monthly_costs, window_size);
            let all_months: Vec<(&String, &f64)> =
                trailing_window.iter().rev().map(|(k, v)| (k, v)).collect();
            let total_months = all_months.len();
            // Stretch the column width when there are few enough months to fit
            // the whole history without scrolling. Default 8 leaves a ragged
            // gap on the right when total fits in the popup; this picks the
            // largest column width (capped at 10) that lets every month show.
            let avail = inner_width.saturating_sub(2);
            // Pick the widest column that still fits every month — falls back
            // to the default 8 only when the popup is too narrow to fit them
            // all. Floor of 6 keeps the labels (`YY-MM`) readable.
            let col_width = if total_months > 0 {
                (avail / total_months.max(1)).clamp(6, 10)
            } else {
                8
            };
            let visible_months = (avail / col_width).max(1);
            let max_scroll = total_months.saturating_sub(visible_months);
            if state.insights_detail_scroll > max_scroll {
                state.insights_detail_scroll = max_scroll;
            }
            let skip = total_months
                .saturating_sub(visible_months)
                .saturating_sub(state.insights_detail_scroll);
            let months: Vec<_> = all_months
                .into_iter()
                .rev()
                .skip(skip)
                .take(visible_months)
                .collect();
            // Avg over the same trailing window as the inline panel (12 mo
            // minimum, including zero-cost months) so the row delta agrees.
            let avg_monthly = if total_months == 0 {
                0.0
            } else {
                trailing_window.iter().map(|(_, c)| *c).sum::<f64>() / total_months as f64
            };
            let max_monthly = months
                .iter()
                .map(|(_, c)| **c)
                .fold(0.0f64, f64::max)
                .max(1.0);

            let bar_height = 6usize;

            for row in (0..bar_height).rev() {
                let threshold = (row as f64 + 0.5) / bar_height as f64;
                let mut row_spans: Vec<Span> = vec![Span::raw("  ")];
                for (_, cost) in &months {
                    let ratio = **cost / max_monthly;
                    let intensity = (ratio * 0.7 + 0.3).min(1.0);
                    let color = theme::primary_with_intensity(intensity);
                    let bar = if ratio >= threshold { "██" } else { "  " };
                    row_spans.push(Span::styled(
                        format!("{bar:^col_width$}"),
                        Style::default().fg(color),
                    ));
                }
                lines.push(Line::from(row_spans));
            }

            let mut label_spans: Vec<Span> = vec![Span::raw("  ")];
            let mut cost_spans: Vec<Span> = vec![Span::raw("  ")];
            let mut tok_spans: Vec<Span> = vec![Span::raw("  ")];
            let mut diff_spans: Vec<Span> = vec![Span::raw("  ")];
            for (month, cost) in &months {
                // `month` is "YYYY-MM"; show as "YY-MM" (ISO 8601 separator).
                let short_month = month.split_once('-').map_or_else(
                    || "??".to_string(),
                    |(y, m)| format!("{}-{m}", y.get(2..).unwrap_or(y)),
                );
                label_spans.push(Span::styled(
                    format!("{short_month:^col_width$}"),
                    Style::default().fg(theme::LABEL_MUTED),
                ));
                cost_spans.push(Span::styled(
                    format!(
                        "{:^width$}",
                        format!("${:.0}", cost.max(0.0)),
                        width = col_width
                    ),
                    Style::default().fg(theme::WARM),
                ));
                let tokens = monthly_tokens.get(*month).copied().unwrap_or(0);
                tok_spans.push(Span::styled(
                    format!(
                        "{:^width$}",
                        crate::format_number(tokens),
                        width = col_width
                    ),
                    Style::default().fg(theme::PRIMARY),
                ));

                let diff_str = if avg_monthly > 0.0 {
                    let pct = ((**cost - avg_monthly) / avg_monthly * 100.0) as i32;
                    if pct >= 0 {
                        format!("+{pct}%")
                    } else {
                        format!("{pct}%")
                    }
                } else {
                    "-".to_string()
                };
                let diff_color = if **cost > avg_monthly {
                    theme::WARNING
                } else {
                    theme::SUCCESS
                };
                diff_spans.push(Span::styled(
                    format!("{diff_str:^col_width$}"),
                    Style::default().fg(diff_color),
                ));
            }
            lines.push(Line::from(label_spans));
            lines.push(Line::from(cost_spans));
            lines.push(Line::from(tok_spans));
            lines.push(Line::from(diff_spans));

            lines.push(Line::from(""));
            let avg_monthly_tokens: u64 = if months.is_empty() {
                0
            } else {
                months
                    .iter()
                    .map(|(m, _)| monthly_tokens.get(*m).copied().unwrap_or(0))
                    .sum::<u64>()
                    / months.len() as u64
            };
            let mut summary_spans = vec![
                Span::styled("  avg: ", Style::default().fg(theme::DIM)),
                Span::styled(
                    format!("${:.0}/mo", avg_monthly.max(0.0)),
                    Style::default().fg(theme::PRIMARY),
                ),
                Span::styled(
                    format!("  {}/mo", crate::format_number(avg_monthly_tokens)),
                    Style::default().fg(theme::PRIMARY),
                ),
            ];
            {
                let now = chrono::Local::now();
                let current_month_key = format!("{}-{:02}", now.year(), now.month());
                let days_elapsed = now.day() as f64;
                let days_in_month = if now.month() == 12 {
                    chrono::NaiveDate::from_ymd_opt(now.year() + 1, 1, 1)
                } else {
                    chrono::NaiveDate::from_ymd_opt(now.year(), now.month() + 1, 1)
                }
                .and_then(|d| d.pred_opt())
                .map_or(30.0, |d| d.day() as f64);

                if let Some(current_cost) = monthly_costs.get(&current_month_key)
                    && days_elapsed > 0.0 {
                        let forecast = current_cost / days_elapsed * days_in_month;
                        summary_spans.push(Span::styled(" | ", Style::default().fg(theme::DIM)));
                        summary_spans.push(Span::styled("this mo: ", Style::default().fg(theme::DIM)));
                        summary_spans.push(Span::styled(
                            format!("${:.0}", current_cost.max(0.0)),
                            Style::default().fg(theme::PRIMARY),
                        ));
                        if days_elapsed < days_in_month {
                            summary_spans.push(Span::styled(
                                "  ·  proj ",
                                Style::default().fg(theme::DIM),
                            ));
                            summary_spans.push(Span::styled(
                                format!("${:.0}", forecast.max(0.0)),
                                Style::default().fg(theme::WARM),
                            ));
                        }
                    }
            }
            summary_spans.push(Span::styled(" | ", Style::default().fg(theme::DIM)));
            summary_spans.push(Span::styled("total: ", Style::default().fg(theme::DIM)));
            summary_spans.push(Span::styled(
                super::format_cost(state.total_cost, 2),
                Style::default().fg(theme::PRIMARY),
            ));
            lines.push(Line::from(summary_spans));
            if total_months > visible_months {
                lines.push(Line::from(vec![
                    Span::styled("  ↑↓: scroll months  ", Style::default().fg(theme::DIM)),
                    Span::styled(
                        format!(
                            "{}-{} of {}",
                            skip + 1,
                            (skip + visible_months).min(total_months),
                            total_months
                        ),
                        Style::default().fg(theme::LABEL_MUTED),
                    ),
                ]));
            }
        }
        _ => {}
    }

    let visible_height = popup_height.saturating_sub(2) as usize;
    if lines.len() < visible_height {
        let pad = (visible_height - lines.len()) / 2;
        let mut padded = vec![Line::from(""); pad];
        padded.append(&mut lines);
        lines = padded;
    }
    let max_scroll = lines.len().saturating_sub(visible_height);
    state.insights_detail_scroll = state.insights_detail_scroll.min(max_scroll);

    let popup = Paragraph::new(lines)
        .scroll((state.insights_detail_scroll as u16, 0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::PRIMARY))
                .title(Span::styled(
                    format!(" {panel_label} "),
                    Style::default().fg(theme::PRIMARY).bold(),
                ))
                .title_bottom(Line::from(vec![
                    Span::styled(" ←→:switch  i/q:close ", Style::default().fg(theme::DIM)),
                    Span::styled(
                        format!("[{}/4] {} ", current_panel + 1, panel_label),
                        Style::default().fg(theme::PRIMARY),
                    ),
                ])),
        );

    frame.render_widget(popup, popup_area);
}

#[cfg(test)]
mod tests {
    use super::trailing_n_months_at;
    use chrono::NaiveDate;
    use std::collections::BTreeMap;

    #[test]
    fn trailing_window_crosses_year_boundary() {
        let costs: BTreeMap<String, f64> = [
            ("2025-12".to_string(), 100.0),
            ("2026-01".to_string(), 200.0),
        ]
        .into_iter()
        .collect();
        let now = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
        let window = trailing_n_months_at(&costs, 12, now);

        assert_eq!(window.len(), 12);
        assert_eq!(window.first().unwrap().0, "2025-02");
        assert_eq!(window.last().unwrap().0, "2026-01");
        let dec_2025 = window.iter().find(|(k, _)| k == "2025-12").unwrap();
        assert_eq!(dec_2025.1, 100.0);
        let jan_2026 = window.iter().find(|(k, _)| k == "2026-01").unwrap();
        assert_eq!(jan_2026.1, 200.0);
    }

    #[test]
    fn trailing_window_fills_zero_for_missing_months() {
        let costs: BTreeMap<String, f64> =
            [("2026-03".to_string(), 50.0)].into_iter().collect();
        let now = NaiveDate::from_ymd_opt(2026, 5, 1).unwrap();
        let window = trailing_n_months_at(&costs, 12, now);

        assert_eq!(window.len(), 12);
        let zeros = window.iter().filter(|(_, c)| *c == 0.0).count();
        assert_eq!(zeros, 11);
        let with_data = window.iter().find(|(_, c)| *c > 0.0).unwrap();
        assert_eq!(with_data.0, "2026-03");
        assert_eq!(with_data.1, 50.0);
    }
}
