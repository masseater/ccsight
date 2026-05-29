use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table},
};

use std::collections::HashMap;

use chrono::NaiveDate;

use super::theme;
use super::{calc_scroll, cost_style, format_cost};
use crate::AppState;
use crate::aggregator::CostCalculator;

fn resample_sparkline(
    data: &[(NaiveDate, u64)],
    width: usize,
    global_first: NaiveDate,
    global_last: NaiveDate,
) -> Vec<u64> {
    if width == 0 {
        return Vec::new();
    }
    let total_days = (global_last - global_first).num_days().max(1) as usize;
    let mut buckets = vec![0u64; width];
    for &(date, val) in data {
        let day_offset = (date - global_first).num_days().max(0) as usize;
        let bucket = (day_offset * width / total_days.max(1)).min(width.saturating_sub(1));
        buckets[bucket] += val;
    }
    buckets
}

fn render_sparkline(values: &[u64], global_max: u64, color: Color) -> Vec<Span<'static>> {
    const SPARK_CHARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max_val = global_max.max(1);
    let spark: String = values
        .iter()
        .map(|&v| {
            if v == 0 {
                ' '
            } else {
                let idx = ((v as f64 / max_val as f64) * 7.0) as usize;
                SPARK_CHARS[idx.min(7)]
            }
        })
        .collect();
    vec![
        Span::raw("       "),
        Span::styled(spark, Style::default().fg(color)),
    ]
}

pub(super) fn draw_dashboard(frame: &mut Frame, area: Rect, state: &mut AppState) {
    let chunks = Layout::vertical([
        Constraint::Length(4),  // Stats cards (with border)
        Constraint::Length(10), // Heatmap + Hourly pattern (fixed to match week rows)
        Constraint::Fill(1),    // Bottom section (scales with terminal)
        Constraint::Length(1),  // Help
    ])
    .split(area);

    draw_stats_cards(frame, chunks[0], state);

    let heatmap_row =
        Layout::horizontal([Constraint::Min(40), Constraint::Length(30)]).split(chunks[1]);

    draw_heatmap(
        frame,
        heatmap_row[0],
        state,
        state.dashboard_panel == 5,
        state.dashboard_scroll[5],
    );
    draw_hourly_pattern(
        frame,
        heatmap_row[1],
        state,
        state.dashboard_panel == 6,
        state.dashboard_scroll[6],
    );

    let bottom_rows = Layout::vertical([Constraint::Fill(1), Constraint::Fill(1)]).split(chunks[2]);

    let top_row = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(bottom_rows[0]);

    let bottom_row = Layout::horizontal([
        Constraint::Percentage(33),
        Constraint::Percentage(34),
        Constraint::Percentage(33),
    ])
    .split(bottom_rows[1]);

    // Store panel areas for click detection
    state.dashboard_panel_areas = vec![
        top_row[0],     // 0: Recent Costs
        top_row[1],     // 1: Top Projects
        bottom_row[0],  // 2: Model Tokens
        bottom_row[1],  // 3: Tool Usage
        bottom_row[2],  // 4: Languages
        heatmap_row[0], // 5: Heatmap
        heatmap_row[1], // 6: Hourly Pattern
    ];

    draw_recent_costs(
        frame,
        top_row[0],
        state,
        state.dashboard_panel == 0,
        state.dashboard_scroll[0],
    );
    draw_top_projects(
        frame,
        top_row[1],
        state,
        state.dashboard_panel == 1,
        state.dashboard_scroll[1],
    );
    draw_model_tokens(
        frame,
        bottom_row[0],
        state,
        state.dashboard_panel == 2,
        state.dashboard_scroll[2],
    );
    let tools_selected = state.dashboard_panel == 3;
    let tools_scroll = state.dashboard_scroll[3];
    draw_tool_usage(frame, bottom_row[1], state, tools_selected, tools_scroll);
    draw_languages(
        frame,
        bottom_row[2],
        state,
        state.dashboard_panel == 4,
        state.dashboard_scroll[4],
    );

    // Footer span format: `<key>:<action>` blocks separated by a single
    // trailing space inside the action text (so two adjacent blocks read
    // as `:help q:quit ...` with one space between them). The `key` runs
    // in PRIMARY color, the `:action` part in DIM. New global keybinds
    // (e.g. `f:filter`, `p:project`, `/:search`) need to be added to ALL
    // `help_spans` constructions across `ui/*.rs` — there is no shared
    // helper because each tab takes a different selection.
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

fn draw_stats_cards(frame: &mut Frame, area: Rect, state: &AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .title(Span::styled(
            " Overview ",
            Style::default().fg(theme::PRIMARY),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::horizontal([
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
    ])
    .split(inner);

    let session_count: usize = state
        .daily_groups
        .iter()
        .map(|g| g.user_sessions().count())
        .sum();
    let sessions_card = Paragraph::new(vec![
        Line::from(Span::styled(
            format!("{session_count}"),
            Style::default()
                .fg(theme::SUCCESS)
                .add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center),
        Line::from(Span::styled("sessions", Style::default().fg(theme::DIM)))
            .alignment(Alignment::Center),
    ])
    .block(Block::default().borders(Borders::NONE));

    let active_days = state.active_days();
    let days_card = Paragraph::new(vec![
        Line::from(Span::styled(
            format!("{active_days}"),
            Style::default()
                .fg(theme::WARM)
                .add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center),
        Line::from(Span::styled(
            "active days",
            Style::default().fg(theme::DIM),
        ))
        .alignment(Alignment::Center),
    ])
    .block(Block::default().borders(Borders::NONE));

    let tokens_card = Paragraph::new(vec![
        Line::from(Span::styled(
            crate::format_number(state.stats.total_tokens.work_tokens()),
            Style::default()
                .fg(theme::PRIMARY)
                .add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center),
        Line::from(Span::styled("tokens", Style::default().fg(theme::DIM)))
            .alignment(Alignment::Center),
    ])
    .block(Block::default().borders(Borders::NONE));

    // If any model lacks pricing, flag the cost figure with `*` and a warning caption so
    // users are not misled by a silently under-counted total. Pricing entries live in
    // `pricing.rs`; unknown models are detected during stats aggregation and collected
    // in `state.models_without_pricing`.
    let pricing_gap_count = state.models_without_pricing.len();
    let cost_text = if pricing_gap_count == 0 {
        super::format_cost(state.total_cost, 2)
    } else {
        format!("{}*", super::format_cost(state.total_cost, 2))
    };
    // Caption text picks a length that fits the per-card width. The card occupies
    // roughly inner.width / 4 — at 60×20 that's ~14 chars, so "total cost (API est.)"
    // (21 chars) gets clipped to "total cost (AP". Use a short label below 22.
    let card_width = (inner.width / 4) as usize;
    let cost_caption = if card_width >= 22 {
        "total cost (API est.)"
    } else {
        "cost (est.)"
    };
    let caption = if pricing_gap_count == 0 {
        Line::from(Span::styled(cost_caption, Style::default().fg(theme::DIM)))
            .alignment(Alignment::Center)
    } else {
        Line::from(vec![
            Span::styled(
                "* ",
                Style::default()
                    .fg(theme::WARNING)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if card_width >= 22 {
                    format!("{pricing_gap_count} models lack pricing")
                } else {
                    format!("{pricing_gap_count} no-price")
                },
                Style::default().fg(theme::WARNING),
            ),
        ])
        .alignment(Alignment::Center)
    };
    let cost_card = Paragraph::new(vec![
        Line::from(Span::styled(
            cost_text,
            Style::default()
                .fg(if pricing_gap_count == 0 {
                    theme::WARM
                } else {
                    theme::WARNING
                })
                .add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center),
        caption,
    ])
    .block(Block::default().borders(Borders::NONE));

    frame.render_widget(sessions_card, chunks[0]);
    frame.render_widget(days_card, chunks[1]);
    frame.render_widget(tokens_card, chunks[2]);
    frame.render_widget(cost_card, chunks[3]);
}

fn draw_heatmap(frame: &mut Frame, area: Rect, state: &AppState, selected: bool, scroll: usize) {
    use chrono::{Datelike, Duration, Local};

    let today = Local::now().date_naive();
    let available_width = area.width.saturating_sub(2) as usize;
    let max_weeks_for_width = available_width.saturating_sub(4) / 2;
    let weeks = max_weeks_for_width.clamp(13, 52);

    let daily_work: std::collections::HashMap<chrono::NaiveDate, u64> = state
        .daily_groups
        .iter()
        .map(|group| {
            let tokens: u64 = group
                .sessions
                .iter()
                .filter(|s| !s.is_subagent)
                .map(crate::aggregator::SessionInfo::work_tokens)
                .sum();
            (group.date, tokens)
        })
        .collect();

    let oldest_date = daily_work.keys().min().copied();
    let max_scroll_weeks = if let Some(oldest) = oldest_date {
        let days_from_oldest = (today - oldest).num_days().max(0) as usize;
        days_from_oldest / 7
    } else {
        0
    };
    let scroll = scroll.min(max_scroll_weeks);

    let scroll_weeks = scroll as i64;
    let today_weekday = today.weekday().num_days_from_sunday() as i64;
    let last_saturday =
        today + Duration::days(6 - today_weekday) - Duration::days(scroll_weeks * 7);
    let adjusted_start = last_saturday - Duration::days((weeks * 7 - 1) as i64);
    let display_end = last_saturday;

    let max_tokens = daily_work.values().max().copied().unwrap_or(1);
    let get_color = |tokens: u64| -> Color {
        if tokens == 0 {
            theme::HEATMAP_EMPTY
        } else {
            let ratio = tokens as f64 / max_tokens as f64;
            if ratio < 0.15 {
                theme::HEATMAP_LOW
            } else if ratio < 0.35 {
                theme::HEATMAP_MID
            } else if ratio < 0.65 {
                theme::HEATMAP_HIGH
            } else {
                theme::PRIMARY
            }
        }
    };

    let month_name = |m: u32| -> &'static str {
        match m {
            1 => "Jan",
            2 => "Feb",
            3 => "Mar",
            4 => "Apr",
            5 => "May",
            6 => "Jun",
            7 => "Jul",
            8 => "Aug",
            9 => "Sep",
            10 => "Oct",
            11 => "Nov",
            12 => "Dec",
            _ => "",
        }
    };

    let mut lines: Vec<Line> = Vec::new();

    let content_width = 4 + weeks * 2;
    let padding = if available_width > content_width {
        (available_width - content_width) / 2
    } else {
        0
    };
    let pad_str = " ".repeat(padding);

    let mut month_row: Vec<Span> = vec![Span::raw(format!("{pad_str}    "))];
    let mut prev_month = 0u32;
    let mut prev_year = 0i32;
    let mut used_chars = 0usize;
    for week in 0..weeks {
        let expected_pos = week * 2;
        let week_start = adjusted_start + Duration::days((week * 7) as i64);
        let month = week_start.month();
        let year = week_start.year();
        if month != prev_month {
            let label = if year != prev_year || week == 0 {
                format!("{}/{}", year % 100, month_name(month))
            } else {
                month_name(month).to_string()
            };
            // Always insert at least 1 space between adjacent labels so a
            // month that starts the same column where the previous label
            // ended doesn't render as one run-on token. After the first
            // label, a 0-gap means visual collision.
            let raw_gap = expected_pos.saturating_sub(used_chars);
            let gap = if used_chars == 0 {
                raw_gap
            } else {
                raw_gap.max(1)
            };
            if gap > 0 {
                month_row.push(Span::raw(" ".repeat(gap)));
                used_chars += gap;
            }
            month_row.push(Span::styled(
                label.clone(),
                Style::default().fg(theme::LABEL_SUBTLE),
            ));
            used_chars += label.len();
            prev_month = month;
            prev_year = year;
        }
    }
    // If the last visible cell falls in a month whose label was never pushed
    // (the trailing partial week starts in the previous month), right-align
    // a synthetic label so the user can still tell which month the rightmost
    // cells belong to.
    let trailing = display_end.min(today);
    if trailing.month() != prev_month {
        let label = if trailing.year() != prev_year {
            format!("{}/{}", trailing.year() % 100, month_name(trailing.month()))
        } else {
            month_name(trailing.month()).to_string()
        };
        let cells_width = weeks * 2;
        let target_end = cells_width;
        let label_start = target_end.saturating_sub(label.len());
        if label_start > used_chars {
            let gap = label_start - used_chars;
            month_row.push(Span::raw(" ".repeat(gap)));
            month_row.push(Span::styled(
                label,
                Style::default().fg(theme::LABEL_SUBTLE),
            ));
        }
    }
    lines.push(Line::from(month_row));

    let day_labels = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    for (day_idx, day_label) in day_labels.iter().enumerate() {
        let label = match day_idx {
            1 | 3 | 5 => *day_label,
            _ => "",
        };
        let mut row_spans: Vec<Span> = vec![Span::styled(
            format!("{pad_str}{label:<4}"),
            Style::default().fg(theme::LABEL_SUBTLE),
        )];

        for week in 0..weeks {
            let date = adjusted_start + Duration::days((week * 7 + day_idx) as i64);
            if date <= display_end && date <= today {
                let tokens = daily_work.get(&date).copied().unwrap_or(0);
                let color = get_color(tokens);
                row_spans.push(Span::styled("■ ", Style::default().fg(color)));
            } else {
                row_spans.push(Span::raw("  "));
            }
        }
        lines.push(Line::from(row_spans));
    }

    let start_str = adjusted_start.format("%m-%d").to_string();
    let end_str = display_end.min(today).format("%m-%d").to_string();
    let legend_bottom = Line::from(vec![
        Span::styled(
            format!(" {start_str} - {end_str}  Less "),
            Style::default().fg(theme::LABEL_SUBTLE),
        ),
        Span::styled("■ ", Style::default().fg(theme::HEATMAP_EMPTY)),
        Span::styled("■ ", Style::default().fg(theme::HEATMAP_LOW)),
        Span::styled("■ ", Style::default().fg(theme::HEATMAP_MID)),
        Span::styled("■ ", Style::default().fg(theme::HEATMAP_HIGH)),
        Span::styled("■ ", Style::default().fg(theme::PRIMARY)),
        Span::styled(" More ", Style::default().fg(theme::LABEL_SUBTLE)),
    ]);

    let border_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::BORDER)
    };

    let title_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::DIM)
    };

    let actual_end = display_end.min(today);
    // ISO 8601 separator (`-`) for YY-MM, matching the Monthly graph labels
    // and the project-wide date-style rule (no locale-dependent slashes in
    // numeric date forms).
    let marker = if selected { '◈' } else { '◇' };
    let title = format!(
        " {marker} Activity {}-{} - {}-{}",
        adjusted_start.format("%y"),
        adjusted_start.format("%m"),
        actual_end.format("%y"),
        actual_end.format("%m"),
    );

    let heatmap = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(title, title_style))
            .title_bottom(legend_bottom),
    );

    frame.render_widget(heatmap, area);
}

fn draw_hourly_pattern(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    selected: bool,
    _scroll: usize,
) {
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
    let num_days = state.daily_groups.len().max(1) as u64;

    let hourly_avg: std::collections::HashMap<u8, u64> = hourly_total
        .iter()
        .map(|(h, t)| (*h, *t / num_days))
        .collect();

    let max_tokens = hourly_avg.values().max().copied().unwrap_or(1);
    let total_avg: u64 = hourly_avg.values().sum();

    let bar_chars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let inner_height = area.height.saturating_sub(3) as usize;

    let mut lines: Vec<Line> = Vec::new();

    // Bar graph (vertical style in rows)
    for row in (0..inner_height).rev() {
        let threshold = (row as f64 + 0.5) / inner_height as f64;
        let mut row_chars = String::new();
        for hour in 0..24u8 {
            let tokens = hourly_avg.get(&hour).copied().unwrap_or(0);
            let ratio = tokens as f64 / max_tokens as f64;
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
        lines.push(Line::from(Span::styled(
            row_chars,
            Style::default().fg(theme::PRIMARY),
        )));
    }

    // Hour labels — `24` marks the right edge (= midnight next day = end of
    // the final bar's hour window), giving a natural 0..24 axis.
    lines.push(Line::from(Span::styled(
        "0     6    12    18   24",
        Style::default().fg(theme::DIM),
    )));

    let peak_entry = hourly_avg.iter().max_by_key(|(_, t)| *t);
    let peak_title = if let Some((h, t)) = peak_entry {
        format!(" Peak: {}-{}h ({}) ", h, h + 1, crate::format_number(*t))
    } else {
        format!(" Peak: - ({}/day) ", crate::format_number(total_avg))
    };

    let border_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::BORDER)
    };
    let title_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::DIM)
    };
    let prefix = if selected { "◈" } else { "◇" };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(vec![Span::styled(
            format!(" {prefix} Hourly avg "),
            title_style,
        )]))
        .title_bottom(Line::from(Span::styled(
            peak_title,
            Style::default().fg(theme::DIM),
        )));

    let paragraph = Paragraph::new(lines).centered().block(block);
    frame.render_widget(paragraph, area);
}

fn draw_top_projects(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    selected: bool,
    scroll: usize,
) {
    let mut projects: Vec<_> = state.stats.project_stats.iter().collect();
    // Sort goes through `state.sort_projects` (single source of truth) so
    // the panel draw, detail popup, keyboard Enter, and mouse double-click
    // all index into the same list. Alphabetical tiebreaker (lint #30) is
    // inside the helper.
    state.sort_projects(&mut projects);

    let total_tokens: u64 = projects.iter().map(|(_, s)| s.work_tokens).sum();
    let (visible_height, _, scroll) = calc_scroll(area.height, projects.len(), scroll, 2);

    let rows: Vec<Row> = projects
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_height)
        .map(|(i, (name, stats))| {
            let pct_str = crate::text::format_pct(stats.work_tokens, total_tokens);
            let dir_name = state.project_label(name);
            let tokens_str = crate::format_number(stats.work_tokens);

            Row::new(vec![
                Cell::from(format!("{}.", i + 1)).style(Style::default().fg(theme::DIM)),
                Cell::from(dir_name),
                Cell::from(tokens_str).style(Style::default().fg(theme::PRIMARY)),
                Cell::from(pct_str).style(Style::default().fg(theme::MUTED)),
            ])
        })
        .collect();

    let border_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::BORDER)
    };

    let title_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::DIM)
    };

    // When there are no projects (e.g. empty filter result), show `0/0`
    // instead of the misleading `1/1` that the previous `.max(1)` produced.
    let total = projects.len();
    let pos = if total == 0 { 0 } else { scroll + 1 };
    let sort_label = state.dashboard_projects_sort.label();
    let title = if selected {
        format!(" ◈ Projects {pos}/{total} · {sort_label} (s) ")
    } else {
        format!(" ◇ Projects {pos}/{total} · {sort_label}")
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(6),
            Constraint::Length(4),
        ],
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(title, title_style)),
    );

    frame.render_widget(table, area);
}

fn draw_model_tokens(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    selected: bool,
    scroll: usize,
) {
    let mut models: Vec<_> = state
        .aggregated_model_tokens
        .iter()
        .map(|(name, ts)| {
            let work_tokens = ts.input_tokens + ts.output_tokens;
            let cost = state
                .model_costs
                .iter()
                .find(|(n, _)| n == name)
                .map_or(0.0, |(_, c)| *c);
            (name.clone(), work_tokens, cost)
        })
        .collect();
    // Sort goes through `state.sort_models` so the panel and the detail
    // popup share one comparator. Alphabetical tiebreaker keeps tied rows
    // stable across frames (HashMap iteration is randomized).
    state.sort_models(&mut models);

    let total_tokens: u64 = models.iter().map(|(_, t, _)| *t).sum();

    let (visible_height, _, scroll) = calc_scroll(area.height, models.len(), scroll, 2);

    // Name column gets whatever Min(8) leaves after the fixed columns
    // (rank 3, tokens 6, pct 4) + 3 inter-column gaps + 2 borders.
    // pct is `NN%` — matches the Languages / Projects panels for visual
    // consistency across the Dashboard grid.
    let name_w = (area.width as usize)
        .saturating_sub(3 + 6 + 4 + 3 + 2)
        .max(4);
    let rows: Vec<Row> = models
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_height)
        .map(|(i, (name, tokens, _cost))| {
            let rank = format!("{}.", i + 1);
            let pct = crate::text::format_pct(*tokens, total_tokens);
            Row::new(vec![
                Cell::from(rank).style(Style::default().fg(theme::DIM)),
                Cell::from(super::truncate_with_ellipsis(name, name_w)),
                Cell::from(crate::format_number(*tokens))
                    .style(Style::default().fg(theme::PRIMARY)),
                Cell::from(pct).style(Style::default().fg(theme::DIM)),
            ])
        })
        .collect();

    let border_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::BORDER)
    };

    let title_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::DIM)
    };

    let sort_label = state.dashboard_models_sort.label();
    let pos = if models.is_empty() { 0 } else { scroll + 1 };
    let total = models.len();
    let title = if selected {
        format!(" ◈ Models {pos}/{total} · {sort_label} (s) ")
    } else {
        format!(" ◇ Models {pos}/{total} · {sort_label}")
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(6),
            Constraint::Length(4),
        ],
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(title, title_style)),
    );

    frame.render_widget(table, area);
}

fn draw_tool_usage(
    frame: &mut Frame,
    area: Rect,
    state: &mut AppState,
    selected: bool,
    _scroll: usize,
) {
    // "Ecosystem & Health" panel — three tiers of progressive disclosure:
    //   Tier 1 (always): per-category summary (Built-in / MCP / Skills / Subagents).
    //   The popup detail merges Built-in + MCP under one "Tools" tab; the preview
    //   keeps them as separate rows for a per-category readout at a glance.
    //   Tier 2 (inner ≥ 8): cross-category top tools
    //   Tier 3 (inner ≥ 11): system-health alerts (pricing gap / stale MCP / retention)
    // Lower tiers are dropped for narrow panels so the layout never overflows.
    // Click areas are still recorded for the Tier 1 rows so Enter / mouse opens the
    // Tools detail popup at the matching section, preserving existing UX.
    use crate::aggregator::{ToolCategory, classify_tool, mcp_server_of};

    let tools: Vec<_> = state
        .stats
        .tool_usage
        .iter()
        .filter(|(name, _)| !name.is_empty())
        .collect();

    let (mut builtin, mut skill, mut agent, mut mcp): (
        Vec<usize>,
        Vec<usize>,
        Vec<usize>,
        Vec<usize>,
    ) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut mcp_keys: Vec<&String> = Vec::new();
    let mut all_tools: Vec<(&str, usize)> = Vec::new();
    let mut command: Vec<usize> = Vec::new();
    let mut skill_used_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut command_used_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut agent_used_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (name, count) in &tools {
        all_tools.push((name.as_str(), **count));
        match classify_tool(name) {
            ToolCategory::BuiltIn => builtin.push(**count),
            ToolCategory::Skill { .. } => {
                skill.push(**count);
                skill_used_keys.insert((*name).clone());
            }
            ToolCategory::Agent { .. } => {
                agent.push(**count);
                agent_used_keys.insert((*name).clone());
            }
            ToolCategory::Command { .. } => {
                command.push(**count);
                command_used_keys.insert((*name).clone());
            }
            ToolCategory::Mcp { .. } => {
                mcp.push(**count);
                mcp_keys.push(*name);
            }
        }
    }

    // MCP unique-count is server-level (matches detail popup semantics).
    // Includes both servers seen in logs AND configured-but-never-used servers
    // so the preview group count matches what the Tools popup body shows.
    let mut mcp_servers: std::collections::HashSet<String> = std::collections::HashSet::new();
    for key in &mcp_keys {
        if let Some(server) = mcp_server_of(key) {
            mcp_servers.insert(server);
        }
    }
    for status in &state.mcp_status {
        if status.configured {
            mcp_servers.insert(status.name.clone());
        }
    }

    let builtin_total: usize = builtin.iter().sum();
    let skill_total: usize = skill.iter().sum();
    let agent_total: usize = agent.iter().sum();
    let mcp_total: usize = mcp.iter().sum();
    let command_total: usize = command.iter().sum();
    let grand_total = builtin_total + skill_total + agent_total + mcp_total + command_total;

    // Inner area starts at (area.x + 1, area.y + 1) due to the bordered Block.
    let inner_x = area.x + 1;
    let inner_y = area.y + 1;
    let inner_width = area.width.saturating_sub(2);
    let inner_height = area.height.saturating_sub(2) as usize;

    // ── Tier 1: per-category summary lines ───────────────────────────────
    // Each entry: (section_index, label, uniq_count, uniq_unit, total_calls, color)
    // Order matches the popup tabs: Tools → Skills → Commands → Subagents.
    // Built-in and MCP are merged into "Tools" mirroring the popup body.
    // `tools_uniq` counts groups the same way the popup does: Built-in is
    // one expandable group (not 31 individual tools) plus one row per MCP
    // server. Counting individual built-in tools would make the same word
    // "groups" mean two different things across views.
    let tools_uniq = usize::from(!builtin.is_empty()) + mcp_servers.len();
    let tools_total_calls = builtin_total + mcp_total;
    // Skills / Commands / Subagents preview counts include configured-but-
    // unused entries so they match the popup body's row count exactly.
    // Counting only entries with calls would make the same word "uniq"
    // disagree between the preview and the popup, matching the regression
    // pattern that `tools_uniq` also avoids.
    let extra_skill = state
        .configured_resources
        .skills
        .iter()
        .filter(|n| !skill_used_keys.contains(&format!("skill:{n}")))
        .count();
    let extra_command = state
        .configured_resources
        .commands
        .iter()
        .filter(|n| !command_used_keys.contains(&format!("command:{n}")))
        .count();
    let extra_agent = state
        .configured_resources
        .agents
        .iter()
        .filter(|n| !agent_used_keys.contains(&format!("agent:{n}")))
        .count();
    let skills_uniq = skill.len() + extra_skill;
    let commands_uniq = command.len() + extra_command;
    let subagents_uniq = agent.len() + extra_agent;
    // Colors come from `theme::CAT_*` (single source of truth) — the same
    // Tools color flows from the preview row into the popup tab so the
    // transition feels visually contiguous instead of like switching
    // categories.
    let categories: [(usize, &str, usize, &str, usize, Color); 4] = [
        (
            0,
            "Tools",
            tools_uniq,
            "groups",
            tools_total_calls,
            theme::CAT_TOOLS,
        ),
        (
            1,
            "Skills",
            skills_uniq,
            "uniq",
            skill_total,
            theme::CAT_SKILLS,
        ),
        (
            2,
            "Commands",
            commands_uniq,
            "uniq",
            command_total,
            theme::CAT_COMMANDS,
        ),
        (
            3,
            "Subagents",
            subagents_uniq,
            "uniq",
            agent_total,
            theme::CAT_SUBAGENTS,
        ),
    ];

    // Stale MCP servers (>30d) get inlined on the MCP row whenever there are any,
    // regardless of which tiers are visible. Tier 3 may also list them as a
    // standalone alert; the duplication is intentional — tying the warning to its
    // category line makes "which thing is stale" visually unambiguous.
    let now = chrono::Utc::now();
    let stale_mcp_count: usize = state
        .mcp_status
        .iter()
        .filter(|s| s.is_underutilized(now, 30))
        .count();

    // Health alerts are computed up-front so we know how many lines Tier 3 needs.
    let pricing_gap = state.models_without_pricing.len();
    let retention_warn = state.retention_warning.as_ref();
    let mut alerts: Vec<(String, Color)> = Vec::new();
    if pricing_gap > 0 {
        alerts.push((
            format!("! pricing gap: {pricing_gap} models ($0 計上)"),
            theme::WARNING,
        ));
    }
    if stale_mcp_count > 0 {
        alerts.push((
            format!("! {stale_mcp_count} stale MCP servers (>30d)"),
            theme::WARNING,
        ));
    }
    if let Some(warn) = retention_warn {
        let prefix = if warn.is_default {
            "default"
        } else {
            "configured"
        };
        alerts.push((
            format!("! retention {}d {prefix} — risk of loss", warn.days),
            theme::WARNING,
        ));
    }

    // Tier visibility based on inner_height. Order: Tier 1 first, then add 2/3 if
    // we still have room. Each tier reserves a separator line above it.
    let category_rows: Vec<(usize, &str, usize, &str, usize, Color)> = categories
        .into_iter()
        .filter(|(_, _, uniq, _, _, _)| *uniq > 0)
        .collect();
    let tier1_lines = category_rows.len();

    // How many lines would Tier 2 need at full size? 1 header + up to 3 tools.
    let mut top_tools: Vec<(&str, usize)> = all_tools.clone();
    top_tools.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    top_tools.truncate(3);
    let tier2_full = if top_tools.is_empty() {
        0
    } else {
        1 + top_tools.len()
    };
    let tier2_compressed = if top_tools.is_empty() { 0 } else { 1 };
    // Tier 3: alerts.len() rows; if no alerts, 1 row "all systems nominal".
    let tier3_full = alerts.len().max(1);

    let want_full = tier1_lines + 1 + tier2_full + 1 + tier3_full; // separators included
    let want_med = tier1_lines + 1 + tier2_full;
    let want_compressed = tier1_lines + 1 + tier2_compressed;

    let (show_tier2, show_tier3, compress_tier2) = if inner_height >= want_full {
        (true, true, false)
    } else if inner_height >= want_med {
        (true, false, false)
    } else if inner_height >= want_compressed {
        (true, false, true)
    } else {
        (false, false, false)
    };

    let mut lines: Vec<Line> = Vec::new();

    // Tier 1
    for (row_offset, (section_idx, label, uniq_count, uniq_unit, total, color)) in
        category_rows.iter().enumerate()
    {
        let prefix_label = format!(" {label:<10}");
        let total_str = crate::format_number(*total as u64);
        let middle_full = format!(" {uniq_count} {uniq_unit} · {total_str}");
        let arrow_pos = (inner_width as usize).saturating_sub(2);
        // Inline stale flag on the Tools row only if it leaves room for the
        // trailing `▶`. The Tier 3 health alert below carries the same info
        // for narrow panels.
        let mcp_stale_inline = *label == "Tools" && stale_mcp_count > 0;
        let stale_suffix = if mcp_stale_inline {
            format!("   {stale_mcp_count} stale")
        } else {
            String::new()
        };
        let prefix_len = prefix_label.chars().count();
        let middle_avail = arrow_pos.saturating_sub(prefix_len);
        // When the full "{N groups · {total}}" doesn't fit before the trailing
        // ▶, drop the group/uniq prefix and show just the total. Truncating
        // mid-number would mislead the reader: a clipped magnitude suffix
        // (e.g. losing the `K`/`M`) reads as a much smaller value than it is.
        let middle_base = if middle_full.chars().count() <= middle_avail {
            middle_full
        } else {
            format!(" {total_str}")
        };
        let middle = if mcp_stale_inline
            && middle_base.chars().count() + stale_suffix.chars().count() <= middle_avail
        {
            format!("{middle_base}{stale_suffix}")
        } else {
            middle_base
        };
        let used = prefix_label.chars().count() + middle.chars().count();
        let pad = arrow_pos.saturating_sub(used);
        // Arrow always uses the row's canonical category color so the
        // four rows look like one consistent "click to expand" affordance.
        // Stale signaling lives on the inline `N stale` suffix and the
        // Tier 3 alert; recoloring the arrow on stale > 0 would break the
        // visual rhythm with the other three rows for redundant info.
        let arrow_color = *color;
        lines.push(Line::from(vec![
            Span::styled(
                prefix_label,
                Style::default().fg(*color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(middle, Style::default().fg(theme::DIM)),
            Span::raw(" ".repeat(pad)),
            Span::styled("▶", Style::default().fg(arrow_color)),
        ]));
        state.tools_panel_category_areas.push((
            *section_idx,
            ratatui::layout::Rect::new(inner_x, inner_y + row_offset as u16, inner_width, 1),
        ));
    }

    let sep_width = inner_width.saturating_sub(2) as usize;
    let make_sep = || {
        Line::from(vec![Span::styled(
            format!(" {}", "─".repeat(sep_width)),
            Style::default().fg(theme::SEPARATOR),
        )])
    };

    // Tier 2
    if show_tier2 && !top_tools.is_empty() {
        lines.push(make_sep());
        if compress_tier2 {
            // Single-line "Top: A 20K · B 17K · C 16K" form for tight panels.
            let mut compact = String::from(" Top:");
            for (i, (name, count)) in top_tools.iter().enumerate() {
                if i > 0 {
                    compact.push_str(" ·");
                }
                let short_name = (*name).chars().take(10).collect::<String>();
                compact.push_str(&format!(
                    " {short_name} {}",
                    crate::format_number(*count as u64)
                ));
            }
            lines.push(Line::from(vec![Span::styled(
                compact,
                Style::default().fg(theme::LABEL_MUTED),
            )]));
        } else {
            lines.push(Line::from(vec![Span::styled(
                " Top tools",
                Style::default().fg(theme::DIM),
            )]));
            let name_w = (inner_width as usize).saturating_sub(15);
            for (name, count) in &top_tools {
                let pct = crate::text::format_pct(*count as u64, grand_total as u64);
                let display: String = (*name).chars().take(name_w).collect();
                let pad = name_w.saturating_sub(display.chars().count());
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        format!("{display}{}", " ".repeat(pad)),
                        Style::default().fg(theme::TEXT_BRIGHT),
                    ),
                    Span::styled(
                        format!(" {:>5}", crate::format_number(*count as u64)),
                        Style::default().fg(theme::LABEL_MUTED),
                    ),
                    Span::styled(format!(" {pct:>3}"), Style::default().fg(theme::DIM)),
                ]));
            }
        }
    }

    // Tier 3
    if show_tier3 {
        lines.push(make_sep());
        if alerts.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                " ✓ all systems nominal",
                Style::default().fg(theme::SUCCESS),
            )]));
        } else {
            for (msg, color) in &alerts {
                lines.push(Line::from(vec![Span::styled(
                    format!(" {msg}"),
                    Style::default().fg(*color),
                )]));
            }
        }
    }

    let border_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::BORDER)
    };
    let title_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::DIM)
    };
    let marker = if selected { '◈' } else { '◇' };
    // Header shows only call count to avoid the dual-meaning trap of "uniq":
    // each Tier 1 row already prints its own per-category uniq with the
    // correct semantics (groups for Tools, uniq for Skills/Commands/Subagents
    // — each including configured-but-unused). A panel-level total mixed the
    // two granularities and contradicted the row sum.
    let title = format!(
        " {marker} Ecosystem  {} calls ",
        crate::format_number(grand_total as u64)
    );

    let paragraph = ratatui::widgets::Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(title, title_style)),
    );

    frame.render_widget(paragraph, area);
}

/// Computes total line count of the Tools detail popup body (excluding headers).
/// Used for scroll bounds to match actual rendered lines in `draw_dashboard_detail_popup` panel 3.
pub(crate) fn tool_usage_line_count(state: &AppState) -> usize {
    use crate::aggregator::{ToolCategory, classify_tool};
    use std::collections::HashSet;
    let tools: Vec<(&String, &usize)> = state.stats.tool_usage.iter().collect();
    // Index order matches the tab order: 0=Tools, 1=Skills, 2=Commands, 3=Subagents.
    // Each tab counts the rendered ROW count (not raw tool key count) so the
    // scroll bar stops at the actual end of content. Configured-but-unused
    // entries from `configured_resources` add zero-call rows.
    let mut has_builtin = false;
    let mut builtin_calls = 0usize;
    let mut mcp_calls = 0usize;
    let mut mcp_servers: HashSet<String> = HashSet::new();
    let mut skill_keys: HashSet<String> = HashSet::new();
    let mut command_keys: HashSet<String> = HashSet::new();
    let mut agent_keys: HashSet<String> = HashSet::new();
    for t in &tools {
        match classify_tool(t.0) {
            ToolCategory::BuiltIn => {
                has_builtin = true;
                builtin_calls += t.1;
            }
            ToolCategory::Mcp { server } => {
                mcp_servers.insert(server);
                mcp_calls += t.1;
            }
            ToolCategory::Skill { .. } => {
                skill_keys.insert((*t.0).clone());
            }
            ToolCategory::Agent { .. } => {
                agent_keys.insert((*t.0).clone());
            }
            ToolCategory::Command { .. } => {
                command_keys.insert((*t.0).clone());
            }
        }
    }
    for status in &state.mcp_status {
        if status.configured {
            mcp_servers.insert(status.name.clone());
        }
    }
    for n in &state.configured_resources.skills {
        skill_keys.insert(format!("skill:{n}"));
    }
    for n in &state.configured_resources.commands {
        command_keys.insert(format!("command:{n}"));
    }
    for n in &state.configured_resources.agents {
        agent_keys.insert(format!("agent:{n}"));
    }
    let counts = [
        usize::from(has_builtin) + mcp_servers.len(),
        skill_keys.len(),
        command_keys.len(),
        agent_keys.len(),
    ];
    let mut active = state.tools_detail_section.min(counts.len() - 1);
    if counts[active] == 0 {
        active = counts.iter().position(|&c| c > 0).unwrap_or(0);
    }
    if counts[active] == 0 {
        return 0;
    }
    // Tools tab body has up to two extra header rows beyond the group rows:
    // the stale legend (when any MCP server is underutilized) and any
    // expanded server's tool sub-rows. Without counting these, j-scroll and
    // the scrollbar's `N/M` indicator stop short of the real end.
    let extra = if active == 0 {
        let now = chrono::Utc::now();
        let any_stale = state.mcp_status.iter().any(|s| s.is_underutilized(now, 30));
        // Each expanded server contributes (tool_count) sub-rows. Built-in
        // counts as a server-like group with `mcp_tool_count("Built-in")`
        // tools (i.e. all built-in keys) when expanded.
        let expanded_rows: usize = state
            .mcp_expanded_servers
            .iter()
            .map(|name| crate::mcp_tool_count(state, name))
            .sum();
        // The Built-in vs MCP ratio line renders only when both subgroups
        // have nonzero calls. Counted here so the scrollbar reaches every
        // body line.
        let has_ratio = builtin_calls > 0 && mcp_calls > 0;
        usize::from(any_stale) + usize::from(has_ratio) + expanded_rows
    } else {
        0
    };
    // Tools tab pins 1 header line ("groups · calls") in the body slice, so
    // its scrollable item count is `1 + groups + extra`. The other tabs pin
    // their `N uniq · M calls` line as a fixed header (not part of the body
    // slice) — return just the row count so pagination matches what the
    // user actually scrolls through.
    if active == 0 {
        1 + counts[active] + extra
    } else {
        counts[active]
    }
}

fn draw_languages(frame: &mut Frame, area: Rect, state: &AppState, selected: bool, scroll: usize) {
    // Known languages with their aggregate counts, excluding the literal
    // "Other" bucket. Unknown extensions are surfaced individually below
    // (rule: architecture.categorization — no catch-all grouping).
    let mut entries: Vec<(String, usize, bool)> = state
        .stats
        .language_usage
        .iter()
        .filter(|(name, _)| !name.is_empty() && name.as_str() != "Other")
        .map(|(name, count)| (name.clone(), *count, false))
        .collect();
    for (ext, count) in &state.stats.extension_usage {
        if crate::aggregator::language::for_extension(ext) == "Other" {
            entries.push((ext.clone(), *count, true));
        }
    }
    // Tiebreaker on name keeps tied-count rows stable across frames.
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let total_usage: usize = entries.iter().map(|(_, c, _)| *c).sum();
    let (visible_height, _, scroll) = calc_scroll(area.height, entries.len(), scroll, 2);

    // Same Min(8) name column as Models; trailing fixed columns are wider here
    // (rank 4, count 6, pct 4) plus 3 inter-column gaps and 2 borders.
    let name_w = (area.width as usize)
        .saturating_sub(4 + 6 + 4 + 3 + 2)
        .max(4);
    let rows: Vec<Row> = entries
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_height)
        .map(|(i, (name, count, is_unknown))| {
            let rank = format!("{}.", i + 1);
            let pct_str = crate::text::format_pct(*count as u64, total_usage as u64);
            // Unknown extensions render dimmer so they're visually
            // distinguishable from the named language categories.
            let name_style = if *is_unknown {
                Style::default().fg(theme::DIM)
            } else {
                Style::default()
            };
            Row::new(vec![
                Cell::from(rank).style(Style::default().fg(theme::DIM)),
                Cell::from(super::truncate_with_ellipsis(name, name_w)).style(name_style),
                Cell::from(count.to_string()).style(Style::default().fg(theme::PRIMARY)),
                Cell::from(pct_str).style(Style::default().fg(theme::MUTED)),
            ])
        })
        .collect();

    let border_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::BORDER)
    };

    let title_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::DIM)
    };

    let title = if selected {
        format!(
            " ◈ Languages {}/{}",
            if entries.is_empty() { 0 } else { scroll + 1 },
            entries.len()
        )
    } else {
        format!(
            " ◇ Languages {}/{}",
            if entries.is_empty() { 0 } else { scroll + 1 },
            entries.len()
        )
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(5),
            Constraint::Length(4),
        ],
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(title, title_style)),
    );

    frame.render_widget(table, area);
}

fn draw_recent_costs(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    selected: bool,
    scroll: usize,
) {
    let (visible_height, _, scroll) = calc_scroll(area.height, state.daily_costs.len(), scroll, 2);

    // Drop the cost decimals when the panel is too narrow to fit
    // `$NNN.NN` — otherwise ratatui silently clips the trailing decimals
    // and leaves the user reading a wrong value. `${:.0}` keeps the
    // magnitude intact.
    let cost_precision = if area.width >= 70 { 2 } else { 0 };

    let rows: Vec<Row> = state
        .daily_costs
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_height)
        .map(|(i, (date, cost))| {
            let date_str = date.format("%m-%d (%a)").to_string();
            let cost_display = cost.max(0.0);
            let cost_str = super::format_cost(cost_display, cost_precision);
            let tokens: u64 =
                state
                    .daily_groups
                    .iter()
                    .find(|g| &g.date == date)
                    .map_or(0, |group| {
                        group
                            .sessions
                            .iter()
                            .filter(|s| !s.is_subagent)
                            .map(crate::aggregator::SessionInfo::work_tokens)
                            .sum()
                    });
            Row::new(vec![
                Cell::from(format!("{}.", i + 1)).style(Style::default().fg(theme::DIM)),
                Cell::from(date_str),
                Cell::from(crate::format_number(tokens)).style(Style::default().fg(theme::DIM)),
                Cell::from(cost_str).style(cost_style(*cost)),
            ])
        })
        .collect();

    let border_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::BORDER)
    };

    let title_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::DIM)
    };

    // Empty filter result → show `0/0`, not `1/1`. See Projects panel rationale.
    let total = state.daily_costs.len();
    let pos = if total == 0 { 0 } else { scroll + 1 };
    let title = if selected {
        format!(" ◈ Costs {pos}/{total}")
    } else {
        format!(" ◇ Costs {pos}/{total}")
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Min(11),
            Constraint::Length(6),
            Constraint::Length(8),
        ],
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(title, title_style)),
    );

    frame.render_widget(table, area);
}

/// Number of leading rows that `activity_header_lines` adds above the
/// scrollable daily/weekly breakdown. Used to shrink the body's
/// `take(visible_height)` and the scroll-clamp `items_per_screen` so the
/// fixed header always stays visible while the body still tracks position.
pub(crate) const ACTIVITY_HEADER_ROWS: usize = 5;

/// Block characters used by every header sparkline so trend rows across
/// popups share the same visual vocabulary. Index 0 (`·`) is the dedicated
/// "no activity" glyph — visually distinct from the `▁..█` non-zero ramp so
/// zero days never get mistaken for the lowest non-zero bucket.
pub(crate) const SPARK_CHARS: [char; 9] = ['·', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Render a trailing-N-day sparkline of `value_for_date(d)` ending at `today`.
/// The bucket is normalised against the window's max so visual scale is
/// independent of the absolute magnitude. Days with zero value render as `░`.
pub(crate) fn render_spark_line<F>(
    today: chrono::NaiveDate,
    days: usize,
    mut value_for: F,
) -> String
where
    F: FnMut(chrono::NaiveDate) -> f64,
{
    let values: Vec<f64> = (0..days)
        .rev()
        .map(|offset| {
            let date = today - chrono::Duration::days(offset as i64);
            value_for(date).max(0.0)
        })
        .collect();
    let max = values.iter().copied().fold(0.0f64, f64::max).max(1e-9);
    values
        .iter()
        .map(|v| {
            if *v <= 0.0 {
                SPARK_CHARS[0]
            } else {
                let ratio = v / max;
                let idx = ((ratio * (SPARK_CHARS.len() - 1) as f64).round() as usize)
                    .clamp(1, SPARK_CHARS.len() - 1);
                SPARK_CHARS[idx]
            }
        })
        .collect()
}

/// Mirror of `ACTIVITY_HEADER_ROWS` for the Costs detail popup. Header carries
/// this-month spend / forecast / MoM delta + top-model composition, then a separator.
pub(crate) const COSTS_HEADER_ROWS: usize = 5;

/// Header block above the Daily Costs breakdown: month-to-date spend,
/// end-of-month forecast (run-rate × remaining days), MoM delta, and the
/// top model contributors for the current month. Outlier callouts are
/// surfaced inline as the user scrolls (not in the header) so the
/// composition row stays readable.
/// `today` is injected so the header math (this-month / forecast / MoM) is
/// testable without reading the wall clock; the top-level draw function
/// passes `chrono::Local::now().date_naive()` at the only production site.
pub(crate) fn costs_header_lines(state: &AppState, today: chrono::NaiveDate) -> Vec<Line<'static>> {
    use chrono::{Datelike, NaiveDate};

    let month_start = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today);
    let prev_month_end = month_start - chrono::Duration::days(1);
    let prev_month_start =
        NaiveDate::from_ymd_opt(prev_month_end.year(), prev_month_end.month(), 1)
            .unwrap_or(prev_month_end);

    let days_in_month = {
        let next_month_start = if today.month() == 12 {
            NaiveDate::from_ymd_opt(today.year() + 1, 1, 1)
        } else {
            NaiveDate::from_ymd_opt(today.year(), today.month() + 1, 1)
        }
        .unwrap_or(today);
        (next_month_start - month_start).num_days() as u32
    };
    let day_of_month = today.day();

    let mut mtd_cost = 0.0f64;
    let mut prev_month_cost = 0.0f64;
    for (date, cost) in &state.daily_costs {
        if date.year() == today.year() && date.month() == today.month() {
            mtd_cost += cost;
        } else if date.year() == prev_month_start.year() && date.month() == prev_month_start.month()
        {
            prev_month_cost += cost;
        }
    }

    // Run-rate forecast: average over elapsed days × total days in month.
    let run_rate = if day_of_month > 0 {
        mtd_cost / day_of_month as f64
    } else {
        0.0
    };
    let forecast = run_rate * days_in_month as f64;
    let days_remaining = days_in_month.saturating_sub(day_of_month);

    // Surface the previous month's actual total verbatim so the reader can
    // compare it against `forecast` directly. A pre-computed percentage of
    // (forecast / prev - 1) reads like an actual month-over-month delta but
    // is forecast-projected, which misleads readers about completed activity.
    let delta_text = if prev_month_cost > 0.0 {
        format!("→ prev {}", super::format_cost(prev_month_cost, 0))
    } else {
        String::from("→ prev —")
    };

    let summary = vec![
        Span::styled("  this mo: ", Style::default().fg(theme::DIM)),
        Span::styled(
            super::format_cost(mtd_cost, 0),
            Style::default()
                .fg(theme::PRIMARY)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · forecast: ", Style::default().fg(theme::DIM)),
        Span::styled(super::format_cost(forecast, 0), cost_style(forecast)),
        Span::styled(
            format!(
                " ({days_remaining}d left @ {}/day)  ",
                super::format_cost(run_rate, 0)
            ),
            Style::default().fg(theme::DIM),
        ),
        Span::styled(delta_text, Style::default().fg(theme::DIM)),
    ];

    // Top model composition for the current month.
    let calculator = crate::aggregator::CostCalculator::global();
    let mut model_cost_mtd: std::collections::HashMap<String, f64> =
        std::collections::HashMap::new();
    for group in &state.daily_groups {
        if group.date.year() != today.year() || group.date.month() != today.month() {
            continue;
        }
        for session in &group.sessions {
            if session.is_subagent {
                continue;
            }
            for (model, tokens) in &session.day_tokens_by_model {
                let c = calculator
                    .calculate_cost(tokens, Some(model))
                    .unwrap_or(0.0);
                let normalized = crate::aggregator::normalize_model_name(model);
                *model_cost_mtd.entry(normalized).or_insert(0.0) += c;
            }
        }
    }
    let mut model_costs: Vec<(String, f64)> = model_cost_mtd.into_iter().collect();
    model_costs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut model_line: Vec<Span> = vec![Span::styled(
        "  Top models  ",
        Style::default().fg(theme::DIM),
    )];
    if mtd_cost > 0.0 && !model_costs.is_empty() {
        for (i, (name, cost)) in model_costs.iter().take(3).enumerate() {
            let pct_str = crate::text::format_pct_f64(*cost, mtd_cost);
            if i > 0 {
                model_line.push(Span::styled(" · ", Style::default().fg(theme::DIM)));
            }
            model_line.push(Span::styled(
                name.clone(),
                Style::default().fg(theme::LABEL_MUTED),
            ));
            model_line.push(Span::styled(
                format!(" {} {pct_str}", super::format_cost(*cost, 0)),
                Style::default().fg(theme::PRIMARY),
            ));
        }
    } else {
        model_line.push(Span::styled(
            "no spend this month yet",
            Style::default().fg(theme::DIM),
        ));
    }

    // Top project composition (symmetric to Top models). Lets users see "where
    // the money went" by codebase as well as by model. Projects without an
    // entry in `state.project_labels` fall back to the bare project_name.
    let mut project_cost_mtd: std::collections::HashMap<String, f64> =
        std::collections::HashMap::new();
    for group in &state.daily_groups {
        if group.date.year() != today.year() || group.date.month() != today.month() {
            continue;
        }
        for session in &group.sessions {
            if session.is_subagent {
                continue;
            }
            let cost: f64 = session
                .day_tokens_by_model
                .iter()
                .map(|(m, t)| calculator.calculate_cost(t, Some(m)).unwrap_or(0.0))
                .sum();
            *project_cost_mtd
                .entry(session.project_name.clone())
                .or_insert(0.0) += cost;
        }
    }
    let mut project_costs: Vec<(String, f64)> = project_cost_mtd.into_iter().collect();
    project_costs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut project_line: Vec<Span> = vec![Span::styled(
        "  Top projects ",
        Style::default().fg(theme::DIM),
    )];
    if mtd_cost > 0.0 && !project_costs.is_empty() {
        for (i, (name, cost)) in project_costs.iter().take(3).enumerate() {
            let pct_str = crate::text::format_pct_f64(*cost, mtd_cost);
            if i > 0 {
                project_line.push(Span::styled(" · ", Style::default().fg(theme::DIM)));
            }
            let label = state.project_label(name);
            project_line.push(Span::styled(label, Style::default().fg(theme::LABEL_MUTED)));
            project_line.push(Span::styled(
                format!(" {} {pct_str}", super::format_cost(*cost, 0)),
                Style::default().fg(theme::PRIMARY),
            ));
        }
    } else {
        project_line.push(Span::styled(
            "no spend this month yet",
            Style::default().fg(theme::DIM),
        ));
    }

    // Daily $/day sparkline over the trailing 30 days.
    let cost_by_date: std::collections::HashMap<NaiveDate, f64> =
        state.daily_costs.iter().copied().collect();
    let spark = render_spark_line(today, 30, |d| cost_by_date.get(&d).copied().unwrap_or(0.0));
    let trend_line = vec![
        Span::styled("  Daily $   ", Style::default().fg(theme::DIM)),
        Span::styled(spark, Style::default().fg(theme::PRIMARY)),
        Span::styled("  past 30 days", Style::default().fg(theme::DIM)),
    ];

    vec![
        Line::from(summary),
        Line::from(model_line),
        Line::from(project_line),
        Line::from(trend_line),
        // Single blank acts as the section break; the previous truncated
        // `────` separator was visually noisy and ate a row for no gain.
        Line::from(""),
    ]
}

/// Header block above the Daily/Weekly Activity breakdown: streak, this-week
/// summary, week-over-week delta, day-of-week distribution. Always reflects
/// the calendar week of `today`, independent of the daily/weekly toggle.
///
/// `today` is injected by the caller so this function stays time-pure: tests
/// can pin a specific calendar day and exercise month/year/week boundaries
/// without depending on the wall clock.
pub(crate) fn activity_header_lines(
    state: &AppState,
    today: chrono::NaiveDate,
) -> Vec<Line<'static>> {
    use chrono::{Datelike, NaiveDate};

    fn week_start(d: NaiveDate) -> NaiveDate {
        let dow = d.weekday().num_days_from_monday();
        d - chrono::Duration::days(dow as i64)
    }

    let this_week_start = week_start(today);
    let prev_week_start = this_week_start - chrono::Duration::days(7);

    // Streak: walk backwards from today, counting days that had work tokens.
    let active_dates: std::collections::HashSet<NaiveDate> = state
        .daily_groups
        .iter()
        .filter(|g| {
            g.sessions
                .iter()
                .any(|s| !s.is_subagent && s.work_tokens() > 0)
        })
        .map(|g| g.date)
        .collect();
    let mut streak = 0u64;
    let mut check = today;
    while active_dates.contains(&check) {
        streak += 1;
        check -= chrono::Duration::days(1);
    }

    let mut this_tokens = 0u64;
    let mut this_sess = 0usize;
    let mut this_days = 0usize;
    let mut prev_tokens = 0u64;
    let mut dow_tokens = [0u64; 7];
    let mut this_project_tokens: HashMap<String, u64> = HashMap::new();
    let mut this_tool_count: HashMap<String, usize> = HashMap::new();
    for group in &state.daily_groups {
        let tokens: u64 = group
            .sessions
            .iter()
            .filter(|s| !s.is_subagent)
            .map(crate::aggregator::SessionInfo::work_tokens)
            .sum();
        let sess = group.sessions.iter().filter(|s| !s.is_subagent).count();
        let ws = week_start(group.date);
        if ws == this_week_start {
            this_tokens += tokens;
            this_sess += sess;
            if tokens > 0 {
                this_days += 1;
            }
            for s in group.sessions.iter().filter(|s| !s.is_subagent) {
                *this_project_tokens
                    .entry(s.project_name.clone())
                    .or_insert(0) += s.work_tokens();
                for (tool, count) in &s.day_tool_usage {
                    *this_tool_count.entry(tool.clone()).or_insert(0) += count;
                }
            }
        } else if ws == prev_week_start {
            prev_tokens += tokens;
        }
        let dow = group.date.weekday().num_days_from_monday() as usize;
        if dow < 7 {
            dow_tokens[dow] += tokens;
        }
    }

    // Surface the previous week's actual total verbatim so the reader can
    // compare it against `this_tokens` directly. A pre-computed `↓N% WoW`
    // reads like an actual week-over-week delta but compares a partial
    // current week against a full prior week, which biases the number low.
    let (delta_text, delta_color) = if prev_tokens > 0 {
        (
            format!("→ prev {} tok", crate::format_number(prev_tokens)),
            theme::DIM,
        )
    } else {
        ("→ prev —".to_string(), theme::DIM)
    };

    let summary = vec![
        Span::styled("  Streak ", Style::default().fg(theme::DIM)),
        Span::styled(
            format!("{streak}d"),
            Style::default()
                .fg(theme::PRIMARY)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · This week ", Style::default().fg(theme::DIM)),
        Span::styled(
            crate::format_number(this_tokens),
            Style::default().fg(theme::PRIMARY),
        ),
        Span::styled(
            format!(" tok · {this_sess} sess · {this_days}/7d  "),
            Style::default().fg(theme::DIM),
        ),
        Span::styled(delta_text, Style::default().fg(delta_color)),
    ];

    let dow_max = dow_tokens.iter().copied().max().unwrap_or(1).max(1);
    let dow_names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let mut dow_line: Vec<Span> = vec![Span::styled(
        "  Day-of-week (all-time)  ",
        Style::default().fg(theme::DIM),
    )];
    for (i, name) in dow_names.iter().enumerate() {
        let bar = if dow_tokens[i] == 0 {
            SPARK_CHARS[0]
        } else {
            let ratio = dow_tokens[i] as f64 / dow_max as f64;
            let idx = ((ratio * (SPARK_CHARS.len() - 1) as f64).round() as usize)
                .clamp(1, SPARK_CHARS.len() - 1);
            SPARK_CHARS[idx]
        };
        dow_line.push(Span::styled(
            format!("{name} "),
            Style::default().fg(theme::LABEL_MUTED),
        ));
        dow_line.push(Span::styled(
            format!("{bar}  "),
            Style::default().fg(theme::PRIMARY),
        ));
    }

    // Daily token sparkline over the trailing 30 days. Builds a date→tokens
    // lookup once so the sparkline closure stays O(window) rather than
    // O(window × daily_groups).
    let mut daily_tokens: std::collections::HashMap<NaiveDate, u64> =
        std::collections::HashMap::new();
    for group in &state.daily_groups {
        let tokens: u64 = group
            .sessions
            .iter()
            .filter(|s| !s.is_subagent)
            .map(crate::aggregator::SessionInfo::work_tokens)
            .sum();
        daily_tokens.insert(group.date, tokens);
    }
    let spark = render_spark_line(today, 30, |d| {
        daily_tokens.get(&d).copied().unwrap_or(0) as f64
    });
    let trend_line = vec![
        Span::styled("  Daily tokens ", Style::default().fg(theme::DIM)),
        Span::styled(spark, Style::default().fg(theme::PRIMARY)),
        Span::styled("  past 30 days", Style::default().fg(theme::DIM)),
    ];

    // Top contributors this week: highest project by tokens and highest
    // tool by call count. Skips the row entirely when the current week has
    // no activity so empty weeks don't claim header real estate.
    let top_project = this_project_tokens
        .iter()
        .max_by_key(|(_, v)| **v)
        .map(|(name, tokens)| (name.clone(), *tokens));
    let top_tool = this_tool_count
        .iter()
        .filter(|(name, _)| !name.is_empty())
        .max_by_key(|(_, v)| **v)
        .map(|(name, count)| (name.clone(), *count));
    let mut top_line: Vec<Span> = vec![Span::styled(
        "  This week  ",
        Style::default().fg(theme::DIM),
    )];
    if let Some((name, tokens)) = top_project {
        let label = state.project_label(&name);
        top_line.push(Span::styled(label, Style::default().fg(theme::WARM)));
        top_line.push(Span::styled(
            format!(" {} tok", crate::format_number(tokens)),
            Style::default().fg(theme::PRIMARY),
        ));
    } else {
        top_line.push(Span::styled(
            "no activity yet",
            Style::default().fg(theme::DIM),
        ));
    }
    if let Some((name, count)) = top_tool {
        top_line.push(Span::styled(" · ", Style::default().fg(theme::DIM)));
        let short = crate::aggregator::format_tool_short(&name);
        top_line.push(Span::styled(short, Style::default().fg(theme::SUCCESS)));
        top_line.push(Span::styled(
            format!(" {count}×"),
            Style::default().fg(theme::PRIMARY),
        ));
    }

    vec![
        Line::from(summary),
        Line::from(dow_line),
        Line::from(top_line),
        Line::from(trend_line),
        // Single blank between header and the daily/weekly breakdown — see
        // `costs_header_lines` for the same reasoning.
        Line::from(""),
    ]
}

pub(crate) struct WeeklyBucket {
    pub label: String,
    pub start_label: String,
    pub end_label: String,
    pub tokens: u64,
    pub cost: f64,
    pub active_days: usize,
}

/// ISO-week aggregation over `state.daily_groups`. Output is newest-week-first
/// to match the surrounding popup conventions (Daily Activity also lists
/// today at the top). Cost is summed from `daily_costs` so it stays in sync
/// with the Costs panel.
pub(crate) fn weekly_activity(state: &AppState) -> Vec<WeeklyBucket> {
    use chrono::{Datelike, NaiveDate};

    fn week_start(d: NaiveDate) -> NaiveDate {
        let dow = d.weekday().num_days_from_monday();
        d - chrono::Duration::days(dow as i64)
    }

    let cost_by_date: HashMap<NaiveDate, f64> = state.daily_costs.iter().copied().collect();

    let mut buckets: HashMap<NaiveDate, WeeklyBucket> = HashMap::new();
    for group in &state.daily_groups {
        let start = week_start(group.date);
        let iso = group.date.iso_week();
        let tokens: u64 = group
            .sessions
            .iter()
            .filter(|s| !s.is_subagent)
            .map(crate::aggregator::SessionInfo::work_tokens)
            .sum();
        let cost = cost_by_date.get(&group.date).copied().unwrap_or(0.0);
        let entry = buckets.entry(start).or_insert_with(|| {
            let end = start + chrono::Duration::days(6);
            WeeklyBucket {
                label: format!("{}-W{:02}", iso.year(), iso.week()),
                start_label: start.format("%m-%d").to_string(),
                end_label: end.format("%m-%d").to_string(),
                tokens: 0,
                cost: 0.0,
                active_days: 0,
            }
        });
        entry.tokens += tokens;
        entry.cost += cost;
        entry.active_days += 1;
    }

    let mut out: Vec<WeeklyBucket> = buckets.into_values().collect();
    // Newest week first — match Daily Activity's reverse-chronological order.
    out.sort_by_key(|w| {
        let parts: Vec<&str> = w.label.split("-W").collect();
        let year: i32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
        let week: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        std::cmp::Reverse((year, week))
    });
    out
}

/// Append a month-summary divider line for the popup body. Same layout for
/// the Daily Costs and Daily Activity detail popups: `── YY-MM  Nd  $cost
/// tokens  avg $X/day ──...`. Width-aware trailing fill.
fn push_month_divider_line(
    lines: &mut Vec<Line<'static>>,
    year: i32,
    month: u32,
    days: usize,
    cost: f64,
    tokens: u64,
    content_width: usize,
) {
    let avg_per_day = if days > 0 { cost / days as f64 } else { 0.0 };
    let label = format!(
        "{:02}-{:02}  {}d  ${:.0}  {}  avg ${:.0}/day",
        year % 100,
        month,
        days,
        cost.max(0.0),
        crate::format_number(tokens),
        avg_per_day.max(0.0),
    );
    let label_w = unicode_width::UnicodeWidthStr::width(label.as_str());
    let trail = "─".repeat(content_width.saturating_sub(2 + 3 + 1 + label_w + 1));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("── ", Style::default().fg(theme::SEPARATOR)),
        Span::styled(label, Style::default().fg(theme::LABEL_MUTED)),
        Span::styled(format!(" {trail}"), Style::default().fg(theme::SEPARATOR)),
    ]));
}

pub(super) fn draw_dashboard_detail_popup(frame: &mut Frame, area: Rect, state: &mut AppState) {
    // Single time-reading site for this popup. Header builders (`activity_header_lines`,
    // `costs_header_lines`) stay time-pure and take `today` as a parameter so they
    // can be tested deterministically at any calendar boundary.
    let today = chrono::Local::now().date_naive();
    let popup_width = 90.min(area.width.saturating_sub(4));
    let popup_height = area.height.saturating_sub(4).min(30);
    let content_width = popup_width.saturating_sub(4) as usize;

    let popup_area = Rect {
        x: area.width.saturating_sub(popup_width) / 2,
        y: area.height.saturating_sub(popup_height) / 2,
        width: popup_width,
        height: popup_height,
    };

    state.active_popup_area = Some(popup_area);
    frame.render_widget(Clear, popup_area);

    let scroll = state.dashboard_scroll[state.dashboard_panel];
    // Inner content rows = popup_height - 2 (top + bottom border). `title_bottom`
    // is rendered onto the bottom border itself and does not consume an extra row.
    let visible_height = popup_height.saturating_sub(2) as usize;

    fn truncate(s: &str, max_len: usize) -> String {
        use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
        if UnicodeWidthStr::width(s) <= max_len {
            s.to_string()
        } else {
            let mut width = 0;
            let mut result = String::new();
            for ch in s.chars() {
                let ch_w = UnicodeWidthChar::width(ch).unwrap_or(0);
                if width + ch_w > max_len.saturating_sub(1) {
                    break;
                }
                result.push(ch);
                width += ch_w;
            }
            format!("{result}…")
        }
    }

    let total_items = match state.dashboard_panel {
        0 => state.daily_costs.len(),
        1 => state.stats.project_stats.len(),
        2 => state.model_costs.len(),
        3 => tool_usage_line_count(state),
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
                weekly_activity(state).len()
            } else {
                // Match the row count after the zero-billable-token filter
                // applied below so the scroll indicator (1-N/M) is correct.
                state.active_days()
            }
        }
        6 => 24,
        _ => 0,
    };
    let items_per_screen = match state.dashboard_panel {
        // Panel 0 reserves the top rows for the this-month/forecast/composition
        // header; panel 5 likewise reserves rows for streak/dow header.
        0 => visible_height.saturating_sub(COSTS_HEADER_ROWS),
        1 => visible_height / 3,
        2 => visible_height / 5,
        4 => visible_height / 2, // Languages: main row + extensions line
        5 => visible_height.saturating_sub(ACTIVITY_HEADER_ROWS),
        _ => visible_height,
    };

    // Clamp scroll so it can't park past the last full page (mirrors the
    // `calc_scroll` rule on the compact panels).
    let max_scroll = total_items.saturating_sub(items_per_screen);
    let scroll = scroll.min(max_scroll);
    state.dashboard_scroll[state.dashboard_panel] = scroll;

    let can_scroll_up = scroll > 0;
    let can_scroll_down = scroll + items_per_screen < total_items;

    let (title, content) = match state.dashboard_panel {
        0 => {
            use chrono::Datelike;
            let title = " Daily Costs ".to_string();
            let mut lines: Vec<Line> = costs_header_lines(state, today);
            let body_height = visible_height.saturating_sub(COSTS_HEADER_ROWS);
            let max_cost = state
                .daily_costs
                .iter()
                .map(|(_, c)| *c)
                .fold(0.0f64, f64::max);
            let bar_width = 12usize;

            // Per-month aggregates so each divider can carry the month's
            // active-day count, total cost, total tokens and average cost
            // per active day. Tokens come from `daily_groups`; costs come
            // from `daily_costs` so the divider stays in sync with the
            // panel rows themselves.
            let mut monthly_cost: std::collections::HashMap<(i32, u32), (f64, usize)> =
                std::collections::HashMap::new();
            for (date, cost) in &state.daily_costs {
                let entry = monthly_cost
                    .entry((date.year(), date.month()))
                    .or_insert((0.0, 0));
                entry.0 += cost;
                entry.1 += 1;
            }
            let mut monthly_tok: std::collections::HashMap<(i32, u32), u64> =
                std::collections::HashMap::new();
            for group in &state.daily_groups {
                let tokens: u64 = group
                    .sessions
                    .iter()
                    .filter(|s| !s.is_subagent)
                    .map(crate::aggregator::SessionInfo::work_tokens)
                    .sum();
                *monthly_tok
                    .entry((group.date.year(), group.date.month()))
                    .or_insert(0) += tokens;
            }
            let cw = content_width;
            let mut prev_month: Option<(i32, u32)> = None;
            for (i, (date, cost)) in state
                .daily_costs
                .iter()
                .enumerate()
                .skip(scroll)
                .take(body_height)
            {
                let key = (date.year(), date.month());
                if let Some(pm) = prev_month
                    && pm != key
                {
                    let (c, d) = monthly_cost.get(&pm).copied().unwrap_or((0.0, 0));
                    let t = monthly_tok.get(&pm).copied().unwrap_or(0);
                    push_month_divider_line(&mut lines, pm.0, pm.1, d, c, t, cw);
                }
                prev_month = Some(key);

                let cost_display = cost.max(0.0);
                let ratio = if max_cost > 0.0 {
                    *cost / max_cost
                } else {
                    0.0
                };
                let filled = (ratio * bar_width as f64).round() as usize;
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled));
                let intensity = (ratio * 0.7 + 0.3).min(1.0);
                let bar_color = theme::primary_with_intensity(intensity);

                let tokens: u64 =
                    state
                        .daily_groups
                        .iter()
                        .find(|g| &g.date == date)
                        .map_or(0, |group| {
                            group
                                .sessions
                                .iter()
                                .filter(|s| !s.is_subagent)
                                .map(crate::aggregator::SessionInfo::work_tokens)
                                .sum()
                        });

                lines.push(Line::from(vec![
                    Span::styled(format!("  {:>3}. ", i + 1), Style::default().fg(theme::DIM)),
                    Span::styled(
                        format!("{} ({})", date, date.format("%a")),
                        Style::default().fg(theme::LABEL_MUTED),
                    ),
                    Span::raw(" "),
                    Span::styled(bar, Style::default().fg(bar_color)),
                    Span::styled(
                        format!(" {}", super::format_cost(cost_display, 2)),
                        cost_style(*cost),
                    ),
                    Span::styled(
                        format!(" ({})", crate::format_number(tokens)),
                        Style::default().fg(theme::DIM),
                    ),
                ]));
            }
            (title, lines)
        }
        1 => {
            let mut projects: Vec<_> = state.stats.project_stats.iter().collect();
            state.sort_projects(&mut projects);
            // `scroll` is the cursor (j/k moves it; Enter opens this row).
            // The actual viewport offset lives in `dashboard_viewport[1]`
            // and only adjusts when the cursor leaves the visible window.
            let cursor = scroll;
            let selected_path = projects.get(cursor).map_or("", |(name, _)| name.as_str());
            let sort_label = state.dashboard_projects_sort.label();
            let title = format!(" ◈ {selected_path}  · {sort_label} (s) ");
            let mut lines: Vec<Line> = Vec::new();
            let max_tokens = projects.first().map_or(1, |(_, s)| s.work_tokens);
            let bar_width = 12usize;
            let name_width = content_width.saturating_sub(8);
            let spark_width = content_width.saturating_sub(10);

            let today = chrono::Local::now().date_naive();
            let global_first = state.daily_groups.last().map_or(today, |g| g.date);
            let global_last = state.daily_groups.first().map_or(today, |g| g.date);

            let mut project_daily_tokens: HashMap<String, Vec<(NaiveDate, u64)>> = HashMap::new();
            for group in state.daily_groups.iter().rev() {
                for session in &group.sessions {
                    let work = session.work_tokens();
                    let daily = project_daily_tokens
                        .entry(session.project_name.clone())
                        .or_default();
                    if let Some(entry) = daily.last_mut().filter(|e| e.0 == group.date) {
                        entry.1 += work;
                    } else {
                        daily.push((group.date, work));
                    }
                }
            }
            let all_resampled: HashMap<String, Vec<u64>> = project_daily_tokens
                .iter()
                .map(|(name, daily)| {
                    (
                        name.clone(),
                        resample_sparkline(daily, spark_width, global_first, global_last),
                    )
                })
                .collect();
            let global_spark_max: u64 = all_resampled
                .values()
                .flat_map(|v| v.iter())
                .copied()
                .max()
                .unwrap_or(1);

            let calculator = CostCalculator::global();
            let mut project_cost: HashMap<String, f64> = HashMap::new();
            let mut project_last_date: HashMap<String, NaiveDate> = HashMap::new();
            let mut project_first_date: HashMap<String, NaiveDate> = HashMap::new();
            let mut project_active_days: HashMap<String, usize> = HashMap::new();
            for group in &state.daily_groups {
                for session in &group.sessions {
                    for (model, tokens) in &session.day_tokens_by_model {
                        let cost = calculator
                            .calculate_cost(tokens, Some(model))
                            .unwrap_or(0.0);
                        *project_cost
                            .entry(session.project_name.clone())
                            .or_default() += cost;
                    }
                    let last = project_last_date
                        .entry(session.project_name.clone())
                        .or_insert(group.date);
                    if group.date > *last {
                        *last = group.date;
                    }
                    let first = project_first_date
                        .entry(session.project_name.clone())
                        .or_insert(group.date);
                    if group.date < *first {
                        *first = group.date;
                    }
                }
            }
            for (name, daily) in &project_daily_tokens {
                project_active_days.insert(name.clone(), daily.len());
            }

            let items_visible = visible_height / 3;
            // Edge-triggered viewport: scroll only when cursor leaves view.
            let prev_viewport = state.dashboard_viewport[1];
            let viewport = if cursor < prev_viewport {
                cursor
            } else if cursor >= prev_viewport + items_visible {
                cursor + 1 - items_visible
            } else {
                prev_viewport
            };
            state.dashboard_viewport[1] = viewport;

            // Body lines start at popup_area.y + 1 (top border). The title
            // line lives on the border itself, not inside content.
            let body_y0 = popup_area.y + 1;
            let inner_x = popup_area.x + 1;
            let row_width = popup_area.width.saturating_sub(2);
            for (i, (name, stats)) in projects
                .iter()
                .enumerate()
                .skip(viewport)
                .take(items_visible)
            {
                let ratio = stats.work_tokens as f64 / max_tokens as f64;
                let filled = (ratio * bar_width as f64).round() as usize;
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled));
                let intensity = (ratio * 0.7 + 0.3).min(1.0);
                let bar_color = Color::Rgb(
                    (140.0 + 78.0 * intensity) as u8,
                    (100.0 + 68.0 * intensity) as u8,
                    (180.0 + 75.0 * intensity) as u8,
                );
                // Show basename only; the absolute path is in the popup title.
                let basename = name.rsplit('/').next().unwrap_or(name.as_str());
                let display_name = truncate(basename, name_width);
                let cost = project_cost.get(name.as_str()).copied().unwrap_or(0.0);
                let active_days = project_active_days.get(name.as_str()).copied().unwrap_or(0);
                let date_range = match (
                    project_first_date.get(name.as_str()),
                    project_last_date.get(name.as_str()),
                ) {
                    (Some(f), Some(l)) if f == l => f.format("%Y-%m-%d").to_string(),
                    (Some(f), Some(l)) => {
                        format!("{}..{}", f.format("%Y-%m-%d"), l.format("%Y-%m-%d"))
                    }
                    _ => String::new(),
                };

                let is_selected = i == cursor;
                let marker = if is_selected { "▶ " } else { "  " };
                let name_style = if is_selected {
                    Style::default()
                        .fg(theme::TEXT_BRIGHT)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::SECONDARY)
                };
                let row_start = lines.len();
                lines.push(Line::from(vec![
                    Span::styled(marker, Style::default().fg(theme::PRIMARY)),
                    Span::styled(format!("{:>3}. ", i + 1), Style::default().fg(theme::DIM)),
                    Span::styled(display_name, name_style),
                ]));
                lines.push(Line::from(vec![
                    Span::raw("       "),
                    Span::styled(bar, Style::default().fg(bar_color)),
                    Span::styled(
                        format!(" {}", crate::format_number(stats.work_tokens)),
                        Style::default().fg(theme::LABEL_MUTED),
                    ),
                    Span::styled(
                        format!("  {} ses", stats.sessions),
                        Style::default().fg(theme::DIM),
                    ),
                    Span::styled(
                        format!("  {}", super::format_cost(cost, 0)),
                        super::cost_style(cost),
                    ),
                    Span::styled(format!("  {active_days}d"), Style::default().fg(theme::DIM)),
                    Span::styled(format!("  {date_range}"), Style::default().fg(theme::DIM)),
                ]));
                if let Some(values) = all_resampled.get(name.as_str())
                    && values.iter().any(|&v| v > 0)
                {
                    lines.push(Line::from(render_sparkline(
                        values,
                        global_spark_max,
                        bar_color,
                    )));
                }
                let row_end = lines.len();
                let row_height = (row_end - row_start) as u16;
                state.project_detail_row_areas.push((
                    i,
                    Rect::new(inner_x, body_y0 + row_start as u16, row_width, row_height),
                ));
                // Full-row bg highlight on every line in the selected row,
                // mirroring the Live tab pattern: patch each span's bg, then
                // pad the line out to row_width so the bg reaches the right
                // border. `Paragraph` styles trailing area with the widget
                // style, not the line style, so explicit padding is needed.
                if is_selected {
                    let bg = theme::FAINT;
                    let sel_style = Style::default().bg(bg);
                    for line in lines[row_start..row_end].iter_mut() {
                        let width: usize = line
                            .spans
                            .iter()
                            .map(|sp| unicode_width::UnicodeWidthStr::width(sp.content.as_ref()))
                            .sum();
                        for span in &mut line.spans {
                            span.style = span.style.bg(bg);
                        }
                        let pad = (row_width as usize).saturating_sub(width);
                        if pad > 0 {
                            line.spans.push(Span::styled(" ".repeat(pad), sel_style));
                        }
                        *line = std::mem::take(line).style(sel_style);
                    }
                }
            }
            (title, lines)
        }
        2 => {
            let sort_label = state.dashboard_models_sort.label();
            let title = format!(" Model Tokens  · {sort_label} (s) ");
            let mut lines: Vec<Line> = Vec::new();
            let calculator = CostCalculator::global();

            let mut model_last_used: HashMap<String, NaiveDate> = HashMap::new();
            let mut model_first_used: HashMap<String, NaiveDate> = HashMap::new();
            let mut model_daily_tokens: HashMap<String, Vec<(NaiveDate, u64)>> = HashMap::new();
            for group in state.daily_groups.iter().rev() {
                for session in group.user_sessions() {
                    for (model_name, ts) in &session.day_tokens_by_model {
                        let normalized = crate::aggregator::normalize_model_name(model_name);
                        let last = model_last_used
                            .entry(normalized.clone())
                            .or_insert(group.date);
                        if group.date > *last {
                            *last = group.date;
                        }
                        let first = model_first_used
                            .entry(normalized.clone())
                            .or_insert(group.date);
                        if group.date < *first {
                            *first = group.date;
                        }
                        let daily = model_daily_tokens.entry(normalized).or_default();
                        let work = ts.input_tokens + ts.output_tokens;
                        if let Some(entry) = daily.last_mut().filter(|e| e.0 == group.date) {
                            entry.1 += work;
                        } else {
                            daily.push((group.date, work));
                        }
                    }
                }
            }
            let spark_width = content_width.saturating_sub(10);
            let today = chrono::Local::now().date_naive();
            let global_first = state.daily_groups.last().map_or(today, |g| g.date);
            let global_last = state.daily_groups.first().map_or(today, |g| g.date);
            let all_resampled: HashMap<String, Vec<u64>> = model_daily_tokens
                .iter()
                .map(|(name, daily)| {
                    (
                        name.clone(),
                        resample_sparkline(daily, spark_width, global_first, global_last),
                    )
                })
                .collect();
            let global_spark_max: u64 = all_resampled
                .values()
                .flat_map(|v| v.iter())
                .copied()
                .max()
                .unwrap_or(1);

            let mut models: Vec<(String, u64, (crate::aggregator::TokenStats, f64))> = state
                .aggregated_model_tokens
                .iter()
                .map(|(name, ts)| {
                    let work_tokens = ts.input_tokens + ts.output_tokens;
                    let cost = state
                        .model_costs
                        .iter()
                        .find(|(n, _)| n == name)
                        .map_or(0.0, |(_, c)| *c);
                    (name.clone(), work_tokens, (ts.clone(), cost))
                })
                .collect();
            // Sort via shared helper so cursor index lines up with the
            // panel and the popup never disagree on order.
            state.sort_models(&mut models);

            let total_tokens: u64 = models.iter().map(|(_, t, _)| *t).sum();
            let max_tokens = models.first().map_or(1, |(_, t, _)| *t);
            let bar_width = 15usize;
            let name_width = content_width.saturating_sub(50);
            let items_visible = visible_height / 3;

            for (i, (model, work_tokens, (ts, cost))) in
                models.iter().enumerate().skip(scroll).take(items_visible)
            {
                let ratio = if max_tokens > 0 {
                    *work_tokens as f64 / max_tokens as f64
                } else {
                    0.0
                };
                let pct = crate::text::format_pct(*work_tokens, total_tokens);
                let filled = (ratio * bar_width as f64).round() as usize;
                let unknown = state.models_without_pricing.contains(model);
                let bar = if unknown {
                    format!("{}{}", "░".repeat(filled), " ".repeat(bar_width - filled))
                } else {
                    format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled))
                };
                let bar_color = if unknown {
                    theme::WARNING
                } else {
                    let intensity = (ratio * 0.7 + 0.3).min(1.0);
                    Color::Rgb(
                        (100.0 + 118.0 * intensity) as u8,
                        (140.0 + 78.0 * intensity) as u8,
                        (200.0 + 55.0 * intensity) as u8,
                    )
                };

                let display_name = truncate(model, name_width);
                let token_info = if unknown {
                    format!(
                        "in:{} out:{} cache:{}  $? (pricing undefined)",
                        crate::format_number(ts.input_tokens),
                        crate::format_number(ts.output_tokens),
                        crate::format_number(ts.cache_creation_tokens + ts.cache_read_tokens),
                    )
                } else {
                    // Two effective rates for the same cost:
                    //   work   = $/MTok charged against input + output only (no cache).
                    //            Compares directly to the base rate on the next line;
                    //            large gap from base means heavy cache pulled-down.
                    //   billed = $/MTok across every token Anthropic billed (cache
                    //            included). Lower number, useful for "what does each
                    //            request feel like" rather than the headline rate.
                    let work_tokens = ts.input_tokens + ts.output_tokens;
                    let total_tokens =
                        work_tokens + ts.cache_creation_tokens + ts.cache_read_tokens;
                    let rates = match (work_tokens, total_tokens) {
                        (0, 0) => String::new(),
                        (0, _) => format!(
                            "  ${:.2}/MTok billed",
                            cost / (total_tokens as f64 / 1_000_000.0),
                        ),
                        (_, _) => format!(
                            "  ${:.2}/MTok work · ${:.2}/MTok billed",
                            cost / (work_tokens as f64 / 1_000_000.0),
                            cost / (total_tokens as f64 / 1_000_000.0),
                        ),
                    };
                    format!(
                        "in:{} out:{} cache:{}  ${:.2}{}",
                        crate::format_number(ts.input_tokens),
                        crate::format_number(ts.output_tokens),
                        crate::format_number(ts.cache_creation_tokens + ts.cache_read_tokens),
                        cost,
                        rates,
                    )
                };

                let mut name_spans = vec![
                    Span::styled(format!("  {:>3}. ", i + 1), Style::default().fg(theme::DIM)),
                    Span::styled(
                        format!("{display_name:<name_width$}"),
                        Style::default().fg(if unknown {
                            theme::WARNING
                        } else {
                            theme::PRIMARY
                        }),
                    ),
                ];
                if unknown {
                    name_spans.push(Span::styled(
                        " ⚠ no pricing",
                        Style::default()
                            .fg(theme::WARNING)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
                lines.push(Line::from(name_spans));
                let date_range = match (
                    model_first_used.get(model.as_str()),
                    model_last_used.get(model.as_str()),
                ) {
                    (Some(f), Some(l)) if f == l => format!(" {}", f.format("%Y-%m-%d")),
                    (Some(f), Some(l)) => {
                        format!(" {}..{}", f.format("%Y-%m-%d"), l.format("%Y-%m-%d"))
                    }
                    _ => String::new(),
                };
                lines.push(Line::from(vec![
                    Span::raw("       "),
                    Span::styled(bar, Style::default().fg(bar_color)),
                    Span::styled(
                        format!(" {}", crate::format_number(*work_tokens)),
                        Style::default().fg(theme::PRIMARY),
                    ),
                    Span::styled(format!(" {pct:>3}"), Style::default().fg(bar_color)),
                    Span::styled(date_range, Style::default().fg(theme::DIM)),
                ]));
                let info_color = if unknown {
                    theme::WARNING
                } else {
                    theme::LABEL_MUTED
                };
                lines.push(Line::from(vec![
                    Span::raw("       "),
                    Span::styled(token_info, Style::default().fg(info_color)),
                ]));
                if let Some(p) = calculator.get_pricing_by_display_name(model) {
                    lines.push(Line::from(vec![
                        Span::raw("       "),
                        Span::styled(
                            format!(
                                "rate/MTok: in:${} out:${} cw_5m:${} cw_1h:${} cache_r:${}",
                                p.input_cost_per_mtok,
                                p.output_cost_per_mtok,
                                p.cache_write_5m_cost_per_mtok,
                                p.cache_write_1h_cost_per_mtok,
                                p.cache_read_cost_per_mtok,
                            ),
                            Style::default().fg(theme::DIM),
                        ),
                    ]));
                }
                if let Some(values) = all_resampled.get(model)
                    && values.iter().any(|&v| v > 0)
                {
                    lines.push(Line::from(render_sparkline(
                        values,
                        global_spark_max,
                        bar_color,
                    )));
                }
            }
            (title, lines)
        }
        3 => {
            use crate::aggregator::{ToolCategory, classify_tool, format_tool_short};

            let title = " Ecosystem ".to_string();
            let mut lines: Vec<Line> = Vec::new();
            let tools: Vec<_> = state.stats.tool_usage.iter().collect();

            let (mut builtin, mut skill, mut agent, mut mcp, mut command): (
                Vec<&(&String, &usize)>,
                Vec<&(&String, &usize)>,
                Vec<&(&String, &usize)>,
                Vec<&(&String, &usize)>,
                Vec<&(&String, &usize)>,
            ) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
            for t in &tools {
                match classify_tool(t.0) {
                    ToolCategory::BuiltIn => builtin.push(t),
                    ToolCategory::Skill { .. } => skill.push(t),
                    ToolCategory::Agent { .. } => agent.push(t),
                    ToolCategory::Mcp { .. } => mcp.push(t),
                    ToolCategory::Command { .. } => command.push(t),
                }
            }
            // Stable tiebreak by name — see draw_tool_usage comment for rationale.
            builtin.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
            skill.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
            agent.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
            mcp.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
            command.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));

            let builtin_total: usize = builtin.iter().map(|(_, c)| **c).sum();
            let skill_total: usize = skill.iter().map(|(_, c)| **c).sum();
            let agent_total: usize = agent.iter().map(|(_, c)| **c).sum();
            let mcp_total: usize = mcp.iter().map(|(_, c)| **c).sum();
            let command_total: usize = command.iter().map(|(_, c)| **c).sum();
            let tools_total = builtin_total + mcp_total;
            let grand_total = tools_total + skill_total + agent_total + command_total;
            // grand_total retained for the Total line; no cross-category % derived from it.

            lines.push(Line::from(vec![Span::styled(
                format!("  Total: {grand_total} calls"),
                Style::default().fg(theme::LABEL_MUTED),
            )]));

            let bar_width = 12usize;
            // Trailing columns: bar (12) + count + pct + ses + ` · Nd ago` (~12).
            let name_width = content_width.saturating_sub(52);

            struct Section<'a> {
                items: Vec<&'a (&'a String, &'a usize)>,
                total: usize,
                color: Color,
                /// (R_base, G_base, B_base, R_amp, G_amp, B_amp): each channel = base + amp * intensity.
                bar_rgb_table: [f64; 6],
                format_name: fn(&str) -> String,
            }

            // Color base+amp tables per category (identical curve, only hues differ).
            const BUILTIN_RGB: [f64; 6] = [150.0, 180.0, 100.0, 68.0, 38.0, 55.0];
            const SKILL_RGB: [f64; 6] = [200.0, 160.0, 80.0, 40.0, 50.0, 40.0];
            const AGENT_RGB: [f64; 6] = [130.0, 170.0, 200.0, 55.0, 50.0, 40.0];
            const MCP_RGB: [f64; 6] = [100.0, 150.0, 180.0, 55.0, 68.0, 38.0];
            const COMMAND_RGB: [f64; 6] = [180.0, 130.0, 200.0, 55.0, 60.0, 50.0];

            fn rgb_from_table(table: &[f64; 6], ratio: f64) -> (u8, u8, u8) {
                let intensity = (ratio * 0.7 + 0.3).min(1.0);
                (
                    (table[0] + table[3] * intensity) as u8,
                    (table[1] + table[4] * intensity) as u8,
                    (table[2] + table[5] * intensity) as u8,
                )
            }
            fn identity_name(s: &str) -> String {
                s.to_string()
            }
            fn short_name(s: &str) -> String {
                format_tool_short(s)
            }

            // Order: Tools (Built-in + MCP) → Skills → Commands → Subagents.
            // Commands sits between Skills and Subagents because slash commands
            // are user-invoked primitives (closer to Skills semantically) while
            // Subagents are dispatcher-tier meta-tools shown last.
            // `mcp` is rendered specially inside the Tools body and is not
            // represented here as its own Section.
            let sections = [
                Section {
                    items: builtin,
                    total: builtin_total,
                    color: theme::CAT_TOOLS,
                    bar_rgb_table: BUILTIN_RGB,
                    format_name: identity_name,
                },
                Section {
                    items: skill,
                    total: skill_total,
                    color: theme::CAT_SKILLS,
                    bar_rgb_table: SKILL_RGB,
                    format_name: short_name,
                },
                Section {
                    items: command,
                    total: command_total,
                    color: theme::CAT_COMMANDS,
                    bar_rgb_table: COMMAND_RGB,
                    format_name: short_name,
                },
                Section {
                    items: agent,
                    total: agent_total,
                    color: theme::CAT_SUBAGENTS,
                    bar_rgb_table: AGENT_RGB,
                    format_name: short_name,
                },
            ];
            // The Tools tab is "active for nav" if either built-in OR mcp has rows.
            let tools_tab_has_rows = !sections[0].items.is_empty() || !mcp.is_empty();

            let short_labels = ["Tools", "Skills", "Commands", "Subagents"];
            let short_totals = [tools_total, skill_total, command_total, agent_total];
            // Per-tab "has rows" — Tools tab (idx 0) is non-empty if either
            // built-in OR mcp has rows; other tabs follow `sections[i].items`.
            let tab_has_rows = |i: usize| -> bool {
                if i == 0 {
                    tools_tab_has_rows
                } else {
                    !sections[i].items.is_empty()
                }
            };
            let active = state.tools_detail_section.min(sections.len() - 1);
            let active = if !tab_has_rows(active) {
                (0..sections.len()).find(|&i| tab_has_rows(i)).unwrap_or(0)
            } else {
                active
            };

            // Build tab spans in main-tabs style:
            //   active: reversed color block (TEXT_DARK on PRIMARY, BOLD)
            //   inactive: `N:` prefix in FAINT + label in section color (DIM for empty)
            //   click areas recorded per-tab (absolute coords within popup)
            let mut tab_spans: Vec<Span> = vec![Span::raw("  ")];
            // Tab bar line index within `lines`: currently 0=blank, 1=Total, 2=tab_bar.
            // Absolute y = popup_area.y + 1 (top border) + line_index.
            let tab_bar_y = popup_area.y + 1 + 2;
            let mut cursor_x = popup_area.x + 1 + 2; // inner content x (+leading "  ")

            for (i, sec) in sections.iter().enumerate() {
                let is_active = i == active;
                let empty = !tab_has_rows(i);
                // Absolute count only — a cross-category percentage here would
                // misleadingly suggest Built-in and Skills can be compared on the same
                // axis when their call-count semantics are very different (dispatcher vs
                // primitive). Keep call count for scale, drop %.
                let label_core = format!(
                    "{} {}",
                    short_labels[i],
                    crate::format_number(short_totals[i] as u64),
                );

                let tab_width: u16;
                if is_active {
                    // ` label ` reversed block
                    let text = format!(" {label_core} ");
                    tab_width = unicode_width::UnicodeWidthStr::width(text.as_str()) as u16;
                    tab_spans.push(Span::styled(
                        text,
                        Style::default()
                            .fg(theme::TEXT_DARK)
                            .bg(theme::PRIMARY)
                            .add_modifier(Modifier::BOLD),
                    ));
                } else if empty {
                    // Dim, no shortcut prefix
                    let text = label_core.clone();
                    tab_width = unicode_width::UnicodeWidthStr::width(text.as_str()) as u16;
                    tab_spans.push(Span::styled(text, Style::default().fg(theme::DIM)));
                } else {
                    // `N:` prefix (FAINT) + label (section color)
                    let prefix = format!("{}:", i + 1);
                    let prefix_w = unicode_width::UnicodeWidthStr::width(prefix.as_str()) as u16;
                    let label_w = unicode_width::UnicodeWidthStr::width(label_core.as_str()) as u16;
                    tab_width = prefix_w + label_w;
                    tab_spans.push(Span::styled(prefix, Style::default().fg(theme::FAINT)));
                    tab_spans.push(Span::styled(label_core, Style::default().fg(sec.color)));
                }

                // Record click area for non-empty sections (empty is not clickable)
                if !empty {
                    state
                        .tools_detail_tab_areas
                        .push((i, Rect::new(cursor_x, tab_bar_y, tab_width, 1)));
                }
                cursor_x += tab_width;

                // Gap between tabs (2 spaces)
                if i + 1 < sections.len() {
                    tab_spans.push(Span::raw("  "));
                    cursor_x += 2;
                }
            }
            lines.push(Line::from(tab_spans));
            // Separator line under tabs
            let sep_width = (popup_area.width as usize).saturating_sub(4);
            lines.push(Line::from(vec![Span::styled(
                format!("  {}", "─".repeat(sep_width)),
                Style::default().fg(theme::SEPARATOR),
            )]));

            // Active section body only.
            // Tab indices: 0=Tools (Built-in + MCP) → 1=Skills → 2=Subagents → 3=Commands.
            // The Tools tab renders two subsections back-to-back: Built-in
            // (flat list) followed by MCP servers (server-grouped, expandable).
            if !tab_has_rows(active) {
                lines.push(Line::from(vec![Span::styled(
                    "  (no data in this section)",
                    Style::default().fg(theme::DIM),
                )]));
            } else if active == 0 {
                // Tools tab: unified list of tool groups. "Built-in" is a
                // synthetic group containing all built-in tools and is rendered
                // with the same expandable layout as MCP servers (▶/▼ toggle).
                struct GroupAgg<'a> {
                    calls: usize,
                    last_used: Option<chrono::DateTime<chrono::Utc>>,
                    /// `true` when the row is either Built-in (always installed)
                    /// or an MCP server still present in `~/.claude.json`.
                    /// `false` means logs reference an MCP server that has been
                    /// removed from the current config.
                    configured: bool,
                    tools: Vec<(&'a String, usize)>, // (full_key, call_count)
                    /// `true` for the synthetic Built-in row — the renderer
                    /// suppresses the "stale" marker and inactive dim styling
                    /// for this row, and tool-name expansion uses raw built-in
                    /// names rather than `format_tool_short` (which would no-op
                    /// but the intent is clearer when stated explicitly).
                    is_builtin: bool,
                }
                impl<'a> Default for GroupAgg<'a> {
                    fn default() -> Self {
                        Self {
                            calls: 0,
                            last_used: None,
                            configured: true,
                            tools: Vec::new(),
                            is_builtin: false,
                        }
                    }
                }
                let mut servers: std::collections::HashMap<String, GroupAgg> =
                    std::collections::HashMap::new();
                // Aggregate MCP tools per server.
                for (key, count) in &mcp {
                    if let ToolCategory::Mcp { server } = classify_tool(key) {
                        let entry = servers.entry(server).or_default();
                        entry.calls += **count;
                        entry.tools.push((*key, **count));
                    }
                }
                for entry in servers.values_mut() {
                    entry.configured = false;
                }
                for status in &state.mcp_status {
                    if let Some(entry) = servers.get_mut(&status.name) {
                        entry.last_used = status.last_used;
                        entry.configured = status.configured;
                    } else if status.configured {
                        // Configured server with no log entries (stale-never)
                        // is surfaced as a 0-call row so the visible list
                        // matches the legend's "N stale" — counting only
                        // servers present in `tool_usage` would hide
                        // "configured but unused" servers from the same view.
                        servers.insert(
                            status.name.clone(),
                            GroupAgg {
                                calls: 0,
                                last_used: status.last_used,
                                configured: true,
                                tools: Vec::new(),
                                is_builtin: false,
                            },
                        );
                    }
                }
                let mut rows: Vec<(String, GroupAgg)> = servers.into_iter().collect();

                // Synthetic "Built-in" row when there are any built-in tools.
                // It participates in the same cursor / sort / expansion logic
                // as an MCP server, reading its items off `sections[0]` (the
                // canonical home for built-in tools).
                let builtin_items = &sections[0].items;
                if !builtin_items.is_empty() {
                    let mut bi = GroupAgg {
                        calls: builtin_total,
                        configured: true,
                        is_builtin: true,
                        ..Default::default()
                    };
                    bi.last_used = builtin_items
                        .iter()
                        .filter_map(|(k, _)| state.tool_last_used.get(*k).copied())
                        .max();
                    for (k, c) in builtin_items {
                        bi.tools.push((*k, **c));
                    }
                    rows.push((
                        crate::handlers::mcp_popup::BUILTIN_GROUP_NAME.to_string(),
                        bi,
                    ));
                }
                rows.sort_by(|a, b| b.1.calls.cmp(&a.1.calls).then_with(|| a.0.cmp(&b.0)));

                let now_for_legend = chrono::Utc::now();
                // Stale count uses the same source-of-truth as the Dashboard
                // Ecosystem preview: `mcp_status` + `is_underutilized`
                // (>=30d OR never-used, configured only). Counting off
                // `rows` with a `>30` threshold would exclude configured
                // servers absent from logs and produce an off-by-one
                // against the preview. Built-in is excluded by construction
                // (not a member of `mcp_status`).
                let stale_count = state
                    .mcp_status
                    .iter()
                    .filter(|s| s.is_underutilized(now_for_legend, 30))
                    .count();
                let total_calls = builtin_total + mcp_total;
                lines.push(Line::from(vec![Span::styled(
                    format!(
                        "  {} groups · {} calls",
                        rows.len(),
                        crate::format_number(total_calls as u64)
                    ),
                    Style::default().fg(theme::DIM),
                )]));
                if total_calls > 0 && builtin_total > 0 && mcp_total > 0 {
                    let bi_pct = builtin_total as f64 / total_calls as f64 * 100.0;
                    let mcp_pct = mcp_total as f64 / total_calls as f64 * 100.0;
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            format!(
                                "Built-in {} ({:.0}%)",
                                crate::format_number(builtin_total as u64),
                                bi_pct
                            ),
                            Style::default().fg(theme::CAT_BUILTIN),
                        ),
                        Span::styled(" · ", Style::default().fg(theme::DIM)),
                        Span::styled(
                            format!(
                                "MCP {} ({:.0}%)",
                                crate::format_number(mcp_total as u64),
                                mcp_pct
                            ),
                            Style::default().fg(theme::CAT_MCP),
                        ),
                    ]));
                }
                if stale_count > 0 {
                    lines.push(Line::from(vec![Span::styled(
                        format!("  ⚠ {stale_count} stale (>30d)"),
                        Style::default().fg(theme::DIM),
                    )]));
                }

                // Per-category bar scale — same reason as `pct_denom` below.
                // A single shared `max_calls` would make every MCP server's bar
                // render as a flat empty trough since Built-in dwarfs them.
                let builtin_max = rows
                    .iter()
                    .filter(|(_, a)| a.is_builtin)
                    .map(|(_, a)| a.calls)
                    .max()
                    .unwrap_or(1)
                    .max(1);
                let mcp_max = rows
                    .iter()
                    .filter(|(_, a)| !a.is_builtin)
                    .map(|(_, a)| a.calls)
                    .max()
                    .unwrap_or(1)
                    .max(1);
                let now = chrono::Utc::now();
                // Name width is narrower here because the row carries more trailing
                // columns than other tabs (expand arrow + rank + tools + ses + last).
                let group_name_width = content_width.saturating_sub(62);
                let inner_x = popup_area.x + 1;
                let base_y = popup_area.y + 1;
                let selected_idx = state.mcp_selected_server.min(rows.len().saturating_sub(1));

                for (i, (group, agg)) in rows.iter().enumerate() {
                    // Separator between Built-in and the MCP block: the two
                    // categories use different `%` denominators (Built-in vs
                    // grand total, MCP rows vs `mcp_total`) so a divider with
                    // a `(% within MCP)` hint keeps the column readable.
                    if i > 0
                        && rows[i - 1].1.is_builtin
                        && !agg.is_builtin
                        && let Some(line_width) = popup_area.width.checked_sub(4)
                    {
                        let label = " MCP servers (% within MCP) ";
                        let label_len = label.chars().count();
                        let dash_total = (line_width as usize).saturating_sub(label_len);
                        let left = dash_total / 2;
                        let right = dash_total - left;
                        lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled("─".repeat(left), Style::default().fg(theme::SEPARATOR)),
                            Span::styled(label, Style::default().fg(theme::CAT_MCP)),
                            Span::styled("─".repeat(right), Style::default().fg(theme::SEPARATOR)),
                        ]));
                    }
                    let bar_max = if agg.is_builtin { builtin_max } else { mcp_max };
                    let ratio = agg.calls as f64 / bar_max as f64;
                    // MCP servers compare against `mcp_total` (per-category) so
                    // ranking detail isn't squashed by Built-in dominance. The
                    // Built-in row shows its share of the grand total instead,
                    // matching the `Built-in N (X%)` summary line above.
                    let pct_denom = if agg.is_builtin {
                        builtin_total + mcp_total
                    } else {
                        mcp_total
                    };
                    let pct_str = crate::text::format_pct(agg.calls as u64, pct_denom as u64);
                    let filled = (ratio * bar_width as f64).round() as usize;
                    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled));
                    // Built-in row gets the Built-in palette so it stays visually
                    // distinct from MCP servers in the same list. Both colors
                    // come from `theme::CAT_*` aliases so the canonical
                    // category palette is enforced from one place.
                    let row_color = if agg.is_builtin {
                        theme::CAT_BUILTIN
                    } else {
                        theme::CAT_MCP
                    };
                    let row_rgb_table = if agg.is_builtin { BUILTIN_RGB } else { MCP_RGB };
                    let (r, g, b) = rgb_from_table(&row_rgb_table, ratio);
                    let bar_color = Color::Rgb(r, g, b);
                    let display_name = truncate(group, group_name_width);
                    let sessions = if agg.is_builtin {
                        // Sum of distinct sessions across built-in tool keys is
                        // misleading (sessions overlap), so prefer the per-tool
                        // max as a coarse "how active the group is" signal.
                        agg.tools
                            .iter()
                            .map(|(k, _)| state.stats.tool_sessions.get(*k).copied().unwrap_or(0))
                            .max()
                            .unwrap_or(0)
                    } else {
                        state
                            .stats
                            .mcp_server_sessions
                            .get(group)
                            .copied()
                            .unwrap_or(0)
                    };
                    let days_since = agg.last_used.map(|ts| (now - ts).num_days());
                    let last_used_str = match days_since {
                        Some(0) => "today".to_string(),
                        Some(1) => "1d ago".to_string(),
                        Some(d) => format!("{d}d ago"),
                        None => "never".to_string(),
                    };
                    let is_inactive = !agg.is_builtin && !agg.configured;
                    // Per-row stale flag must match the legend / preview count:
                    // configured + (never used OR >=30d idle). Threshold matches
                    // `McpServerStatus::is_underutilized` exactly so per-row `⚠`
                    // and the header "N stale" stay in sync.
                    let is_stale =
                        !agg.is_builtin && agg.configured && days_since.is_none_or(|d| d >= 30);
                    let expanded = state.mcp_expanded_servers.contains(group);
                    let is_selected = i == selected_idx;
                    let arrow = if expanded { "▼" } else { "▶" };
                    let server_row_y = base_y + lines.len() as u16;
                    state.mcp_server_row_areas.push((
                        i,
                        Rect::new(inner_x, server_row_y, popup_area.width.saturating_sub(2), 1),
                    ));
                    let name_style = if is_selected {
                        Style::default()
                            .fg(theme::TEXT_DARK)
                            .bg(theme::PRIMARY)
                            .add_modifier(Modifier::BOLD)
                    } else if is_inactive {
                        Style::default().fg(theme::LABEL_SUBTLE)
                    } else {
                        Style::default().fg(row_color)
                    };
                    let tool_count = agg.tools.len();
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!(" {arrow} "),
                            Style::default().fg(if expanded { row_color } else { theme::DIM }),
                        ),
                        Span::styled(format!("{:>2}. ", i + 1), Style::default().fg(theme::DIM)),
                        Span::styled(format!("{display_name:<group_name_width$}"), name_style),
                        Span::raw(" "),
                        Span::styled(bar, Style::default().fg(bar_color)),
                        Span::styled(
                            format!(" {:>5}", crate::format_number(agg.calls as u64)),
                            Style::default().fg(theme::LABEL_MUTED),
                        ),
                        Span::styled(format!(" {pct_str:>3}"), Style::default().fg(theme::DIM)),
                        Span::styled(
                            format!(
                                " · {tool_count} tool{}",
                                if tool_count == 1 { "" } else { "s" }
                            ),
                            Style::default().fg(theme::DIM),
                        ),
                        Span::styled(
                            format!(" · {sessions:>3} ses"),
                            Style::default().fg(theme::DIM),
                        ),
                        Span::styled(
                            format!(" · {last_used_str}"),
                            Style::default().fg(if is_stale { theme::WARNING } else { theme::DIM }),
                        ),
                        Span::styled(
                            if is_stale { " ⚠" } else { "" },
                            Style::default().fg(theme::WARNING),
                        ),
                    ]));

                    if expanded && !agg.tools.is_empty() {
                        let mut tools_sorted = agg.tools.clone();
                        tools_sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
                        let tool_max = tools_sorted.first().map_or(1, |(_, c)| *c).max(1);
                        let tool_bar_width = 6usize;
                        let selected_tool_idx = if is_selected {
                            state.mcp_selected_tool
                        } else {
                            None
                        };
                        for (ti, (full_key, tcalls)) in tools_sorted.iter().enumerate() {
                            // Built-in tool keys are already short (e.g. "Bash");
                            // MCP keys get the `mcp__server__action` prefix stripped.
                            let display = if agg.is_builtin {
                                (*full_key).clone()
                            } else {
                                crate::aggregator::format_tool_short(full_key)
                            };
                            let short = display
                                .rsplit_once(':')
                                .map_or_else(|| display.clone(), |(_, t)| t.to_string());
                            let t_ratio = *tcalls as f64 / tool_max as f64;
                            let t_pct = crate::text::format_pct(*tcalls as u64, agg.calls as u64);
                            let filled_t = (t_ratio * tool_bar_width as f64).round() as usize;
                            let t_bar = format!(
                                "{}{}",
                                "█".repeat(filled_t),
                                "░".repeat(tool_bar_width.saturating_sub(filled_t))
                            );
                            let t_ses = state
                                .stats
                                .tool_sessions
                                .get(*full_key)
                                .copied()
                                .unwrap_or(0);
                            let tool_name_width = content_width.saturating_sub(40);
                            let t_name = truncate(&short, tool_name_width);
                            let tool_selected = selected_tool_idx == Some(ti);
                            let t_name_style = if tool_selected {
                                Style::default()
                                    .fg(theme::TEXT_DARK)
                                    .bg(theme::PRIMARY)
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(row_color)
                            };
                            lines.push(Line::from(vec![
                                Span::raw("       "),
                                Span::styled(
                                    format!("{:>2}. ", ti + 1),
                                    Style::default().fg(theme::DIM),
                                ),
                                Span::styled(format!("{t_name:<tool_name_width$}"), t_name_style),
                                Span::raw(" "),
                                Span::styled(t_bar, Style::default().fg(bar_color)),
                                Span::styled(
                                    format!(" {:>5}", crate::format_number(*tcalls as u64)),
                                    Style::default().fg(theme::LABEL_MUTED),
                                ),
                                Span::styled(
                                    format!(" {t_pct:>3}"),
                                    Style::default().fg(theme::DIM),
                                ),
                                Span::styled(
                                    format!(" · {t_ses:>3} ses"),
                                    Style::default().fg(theme::DIM),
                                ),
                            ]));
                        }
                    }
                }
            } else {
                let sec = &sections[active];
                // Build the merged display list: every entry actually used
                // (call > 0) plus configured-but-unused entries from the
                // matching `configured_resources` set (rendered as 0-call
                // rows with `never` last-used). Mirrors how the Tools tab
                // surfaces stale-never MCP servers — without this, an
                // installed-but-never-tried Skill / Command / Agent stays
                // invisible in the popup it ought to show up in.
                let (storage_prefix, configured_set): (&str, &std::collections::HashSet<String>) =
                    match active {
                        1 => ("skill:", &state.configured_resources.skills),
                        2 => ("command:", &state.configured_resources.commands),
                        3 => ("agent:", &state.configured_resources.agents),
                        _ => ("", &state.configured_resources.skills),
                    };
                let used_keys: std::collections::HashSet<String> =
                    sec.items.iter().map(|(k, _)| (*k).clone()).collect();
                let mut unused_keys: Vec<String> = configured_set
                    .iter()
                    .filter_map(|name| {
                        let key = format!("{storage_prefix}{name}");
                        if used_keys.contains(&key) {
                            None
                        } else {
                            Some(key)
                        }
                    })
                    .collect();
                unused_keys.sort();

                let display_uniq = sec.items.len() + unused_keys.len();
                lines.push(Line::from(vec![Span::styled(
                    format!(
                        "  {} uniq · {} calls",
                        display_uniq,
                        crate::format_number(sec.total as u64)
                    ),
                    Style::default().fg(theme::DIM),
                )]));
                let max_usage = sec.items.first().map_or(1, |(_, c)| **c);
                let now = chrono::Utc::now();
                let mut row_idx = 0usize;
                for (name, count) in sec.items.iter() {
                    let ratio = **count as f64 / max_usage as f64;
                    let pct = crate::text::format_pct(**count as u64, sec.total as u64);
                    let filled = (ratio * bar_width as f64).round() as usize;
                    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled));
                    let (r, g, b) = rgb_from_table(&sec.bar_rgb_table, ratio);
                    let bar_color = Color::Rgb(r, g, b);
                    let display_raw = (sec.format_name)(name);
                    let display_name = truncate(&display_raw, name_width);
                    let ses = state.stats.tool_sessions.get(*name).copied().unwrap_or(0);
                    let last_used_str = match state
                        .tool_last_used
                        .get(*name)
                        .map(|ts| (now - *ts).num_days())
                    {
                        Some(0) => "today".to_string(),
                        Some(1) => "1d ago".to_string(),
                        Some(d) => format!("{d}d ago"),
                        None => "never".to_string(),
                    };
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("  {:>3}. ", row_idx + 1),
                            Style::default().fg(theme::DIM),
                        ),
                        Span::styled(
                            format!("{display_name:<name_width$}"),
                            Style::default().fg(sec.color),
                        ),
                        Span::raw(" "),
                        Span::styled(bar, Style::default().fg(bar_color)),
                        Span::styled(
                            format!(" {count:>4}"),
                            Style::default().fg(theme::LABEL_MUTED),
                        ),
                        Span::styled(format!(" {pct:>3}"), Style::default().fg(theme::DIM)),
                        Span::styled(format!(" · {ses:>3} ses"), Style::default().fg(theme::DIM)),
                        Span::styled(
                            format!(" · {last_used_str}"),
                            Style::default().fg(theme::DIM),
                        ),
                    ]));
                    row_idx += 1;
                }
                // Configured-but-unused rows.
                let bar_empty = "░".repeat(bar_width);
                for key in &unused_keys {
                    let display_raw = (sec.format_name)(key);
                    let display_name = truncate(&display_raw, name_width);
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("  {:>3}. ", row_idx + 1),
                            Style::default().fg(theme::DIM),
                        ),
                        Span::styled(
                            format!("{display_name:<name_width$}"),
                            Style::default().fg(theme::LABEL_SUBTLE),
                        ),
                        Span::raw(" "),
                        Span::styled(bar_empty.clone(), Style::default().fg(theme::SEPARATOR)),
                        Span::styled("    0", Style::default().fg(theme::DIM)),
                        Span::styled("  0%", Style::default().fg(theme::DIM)),
                        Span::styled("   ·   0 ses", Style::default().fg(theme::DIM)),
                        Span::styled(" · never", Style::default().fg(theme::DIM)),
                    ]));
                    row_idx += 1;
                }
            }

            // Pinned headers: blank + Total + tab bar + separator (4). The
            // Skills/Commands/Subagents tabs add a fifth pinned line — the
            // `N uniq · M calls` summary — so the user-visible row count and
            // the pagination footer agree (body row count == total_items).
            let headers = if active == 0 { 4 } else { 5 };
            let total_len = lines.len();
            if total_len > headers {
                let body_start = headers;
                let body_end = total_len;
                let body: Vec<Line> = lines[body_start..body_end]
                    .iter()
                    .skip(scroll)
                    .take(visible_height.saturating_sub(headers))
                    .cloned()
                    .collect();
                let mut new_lines = lines[..headers].to_vec();
                new_lines.extend(body);
                lines = new_lines;
            }

            (title, lines)
        }
        4 => {
            let title = " Languages ".to_string();
            let mut lines: Vec<Line> = vec![
                Line::from(Span::styled(
                    "  File types touched by tool calls (Read, Edit, Write, etc.)",
                    Style::default().fg(theme::DIM),
                )),
                Line::from(""),
            ];

            let mut ext_by_lang: std::collections::HashMap<&str, Vec<(&String, &usize)>> =
                std::collections::HashMap::new();
            let mut other_exts: Vec<(&String, &usize)> = Vec::new();
            for (ext, count) in &state.stats.extension_usage {
                let lang = crate::aggregator::language::for_extension(ext);
                if lang == "Other" {
                    other_exts.push((ext, count));
                } else {
                    ext_by_lang.entry(lang).or_default().push((ext, count));
                }
            }
            for exts in ext_by_lang.values_mut() {
                exts.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
            }
            other_exts.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));

            let mut known_langs: Vec<_> = state
                .stats
                .language_usage
                .iter()
                .filter(|(lang, _)| lang.as_str() != "Other")
                .collect();
            known_langs.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));

            let total_usage: usize = state.stats.language_usage.values().sum();
            let max_count = known_langs
                .first()
                .map_or(1, |(_, c)| **c)
                .max(other_exts.first().map_or(0, |(_, c)| **c));
            let bar_width = 15usize;
            let name_width = content_width.saturating_sub(40);

            enum LangItem<'a> {
                Known(&'a str, usize),
                Unknown(&'a str, usize),
            }
            let mut items: Vec<LangItem> = Vec::new();
            for (lang, count) in &known_langs {
                items.push(LangItem::Known(lang.as_str(), **count));
            }
            for (ext, count) in &other_exts {
                items.push(LangItem::Unknown(ext.as_str(), **count));
            }
            items.sort_by(|a, b| {
                let ca = match a {
                    LangItem::Known(_, c) | LangItem::Unknown(_, c) => *c,
                };
                let cb = match b {
                    LangItem::Known(_, c) | LangItem::Unknown(_, c) => *c,
                };
                cb.cmp(&ca)
            });

            for (rank, item) in items.iter().enumerate().skip(scroll).take(visible_height) {
                let (display_name, count, is_known) = match item {
                    LangItem::Known(lang, c) => ((*lang).to_string(), *c, true),
                    LangItem::Unknown(ext, c) => (format!(".{ext}"), *c, false),
                };
                let ratio = count as f64 / max_count as f64;
                let filled = (ratio * bar_width as f64).round() as usize;
                let intensity = if is_known {
                    (ratio * 0.7 + 0.3).min(1.0)
                } else {
                    (ratio * 0.4 + 0.2).min(0.8)
                };
                let bar_color = Color::Rgb(
                    (40.0 + 46.0 * intensity) as u8,
                    (80.0 + 85.0 * intensity) as u8,
                    (90.0 + 90.0 * intensity) as u8,
                );
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled));
                let pct_str = crate::text::format_pct(count as u64, total_usage as u64);
                let name_label = truncate(&display_name, name_width);
                let name_color = if is_known {
                    theme::LABEL_MUTED
                } else {
                    theme::DIM
                };

                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {:>3}. ", rank + 1),
                        Style::default().fg(theme::DIM),
                    ),
                    Span::styled(
                        format!("{name_label:<name_width$}"),
                        Style::default().fg(name_color),
                    ),
                    Span::raw(" "),
                    Span::styled(bar, Style::default().fg(bar_color)),
                    Span::styled(
                        format!(" {count:>5}"),
                        Style::default().fg(theme::TEXT_BRIGHT),
                    ),
                    Span::styled(format!(" {pct_str:>4}"), Style::default().fg(theme::DIM)),
                ]));

                if is_known
                    && let LangItem::Known(lang, lang_count) = item
                    && let Some(exts) = ext_by_lang.get(lang)
                {
                    // Surface the residual: `language_usage[lang]`
                    // counts hits without a file extension too (see
                    // `extract_language_from_tool_input`); without
                    // this tail the breakdown wouldn't add up to
                    // the row header.
                    let ext_sum: usize = exts.iter().map(|(_, c)| **c).sum();
                    let no_ext = lang_count.saturating_sub(ext_sum);
                    let indent = "        ";
                    let max_line_width = content_width.saturating_sub(2);
                    let mut current_line = String::from(indent);
                    let mut wrote_any = false;
                    let push_part = |part: String,
                                     current_line: &mut String,
                                     wrote_any: &mut bool,
                                     lines: &mut Vec<Line>| {
                        let needed = if current_line.len() == indent.len() {
                            current_line.len() + part.len()
                        } else {
                            current_line.len() + 2 + part.len()
                        };
                        if needed > max_line_width && current_line.len() > indent.len() {
                            lines.push(Line::from(Span::styled(
                                current_line.clone(),
                                Style::default().fg(theme::DIM),
                            )));
                            *current_line = format!("{indent}{part}");
                        } else {
                            if *wrote_any {
                                current_line.push_str("  ");
                            }
                            current_line.push_str(&part);
                        }
                        *wrote_any = true;
                    };
                    for (ext, c) in exts.iter() {
                        push_part(
                            format!(".{ext}({c})"),
                            &mut current_line,
                            &mut wrote_any,
                            &mut lines,
                        );
                    }
                    if no_ext > 0 {
                        push_part(
                            format!("(no-ext: {no_ext})"),
                            &mut current_line,
                            &mut wrote_any,
                            &mut lines,
                        );
                    }
                    if current_line.len() > indent.len() {
                        lines.push(Line::from(Span::styled(
                            current_line,
                            Style::default().fg(theme::DIM),
                        )));
                    }
                }
            }

            if known_langs.is_empty() && other_exts.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  No language data available",
                    Style::default().fg(theme::DIM),
                )));
            }
            (title, lines)
        }
        5 if state.activity_view_weekly => {
            let title = " Weekly Activity ".to_string();
            let mut lines: Vec<Line> = activity_header_lines(state, today);
            let body_height = visible_height.saturating_sub(ACTIVITY_HEADER_ROWS);
            let weekly = weekly_activity(state);
            let max_tokens = weekly.iter().map(|w| w.tokens).max().unwrap_or(1);
            let bar_width = 15usize;
            for (i, w) in weekly.iter().enumerate().skip(scroll).take(body_height) {
                let ratio = w.tokens as f64 / max_tokens as f64;
                let filled = (ratio * bar_width as f64).round() as usize;
                let intensity = (ratio * 0.7 + 0.3).min(1.0);
                let bar_color = Color::Rgb(
                    (80.0 + 100.0 * intensity) as u8,
                    (160.0 + 58.0 * intensity) as u8,
                    (180.0 + 75.0 * intensity) as u8,
                );
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled));
                lines.push(Line::from(vec![
                    Span::styled(format!("  {:>3}. ", i + 1), Style::default().fg(theme::DIM)),
                    Span::styled(
                        format!("{} ({}–{})", w.label, w.start_label, w.end_label),
                        Style::default().fg(theme::LABEL_MUTED),
                    ),
                    Span::raw(" "),
                    Span::styled(bar, Style::default().fg(bar_color)),
                    Span::styled(
                        format!(" {:>9}", crate::format_number(w.tokens)),
                        Style::default().fg(theme::PRIMARY),
                    ),
                    Span::styled(
                        format!(" {:>6}", format_cost(w.cost, 0)),
                        cost_style(w.cost),
                    ),
                    Span::styled(
                        format!(" {}d", w.active_days),
                        Style::default().fg(theme::DIM),
                    ),
                ]));
            }
            (title, lines)
        }
        5 => {
            use chrono::Datelike;
            let title = " Daily Activity ".to_string();
            let mut lines: Vec<Line> = activity_header_lines(state, today);
            let body_height = visible_height.saturating_sub(ACTIVITY_HEADER_ROWS);
            // Skip zero-billable-token days so the row count matches the
            // Overview headline (`active_days`) and the Costs panel ratio.
            // The same `daily_costs` set defines "active" everywhere.
            let active_dates: std::collections::HashSet<chrono::NaiveDate> =
                state.daily_costs.iter().map(|(d, _)| *d).collect();
            // Per-day breakdown: work total + the four billable buckets so
            // users can spot what dominated a day at a glance (output-heavy
            // refactor vs cache-read-heavy resume, etc.).
            let daily: Vec<(chrono::NaiveDate, u64, u64, u64, u64, u64)> = state
                .daily_groups
                .iter()
                .filter(|group| active_dates.contains(&group.date))
                .map(|group| {
                    let mut work = 0u64;
                    let mut input = 0u64;
                    let mut output = 0u64;
                    let mut cw = 0u64;
                    let mut cr = 0u64;
                    for s in group.sessions.iter().filter(|s| !s.is_subagent) {
                        work += s.work_tokens();
                        input += s.day_input_tokens;
                        output += s.day_output_tokens;
                        for ts in s.day_tokens_by_model.values() {
                            cw += ts.cache_creation_tokens;
                            cr += ts.cache_read_tokens;
                        }
                    }
                    (group.date, work, input, output, cw, cr)
                })
                .collect();
            let max_tokens = daily.iter().map(|(_, w, ..)| *w).max().unwrap_or(1);
            let bar_width = 15usize;

            // Per-month totals (active days, work tokens, cost) so each
            // divider row can summarise the month that just finished
            // scrolling past. Costs come from `state.daily_costs` and stay
            // in sync with the Costs panel by construction.
            let mut monthly_tok: std::collections::HashMap<(i32, u32), (u64, usize)> =
                std::collections::HashMap::new();
            for (date, work, ..) in &daily {
                let entry = monthly_tok
                    .entry((date.year(), date.month()))
                    .or_insert((0, 0));
                entry.0 += *work;
                entry.1 += 1;
            }
            let mut monthly_cost: std::collections::HashMap<(i32, u32), f64> =
                std::collections::HashMap::new();
            for (date, cost) in &state.daily_costs {
                *monthly_cost
                    .entry((date.year(), date.month()))
                    .or_insert(0.0) += cost;
            }
            let mut prev_month: Option<(i32, u32)> = None;
            for (i, (date, work, input, output, cw, cr)) in
                daily.iter().enumerate().skip(scroll).take(body_height)
            {
                let key = (date.year(), date.month());
                if let Some(pm) = prev_month
                    && pm != key
                {
                    let (t, d) = monthly_tok.get(&pm).copied().unwrap_or((0, 0));
                    let c = monthly_cost.get(&pm).copied().unwrap_or(0.0);
                    push_month_divider_line(&mut lines, pm.0, pm.1, d, c, t, content_width);
                }
                prev_month = Some(key);

                let ratio = *work as f64 / max_tokens as f64;
                let filled = (ratio * bar_width as f64).round() as usize;
                let intensity = (ratio * 0.7 + 0.3).min(1.0);
                let bar_color = Color::Rgb(
                    (80.0 + 100.0 * intensity) as u8,
                    (160.0 + 58.0 * intensity) as u8,
                    (180.0 + 75.0 * intensity) as u8,
                );
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled));

                lines.push(Line::from(vec![
                    Span::styled(format!("  {:>3}. ", i + 1), Style::default().fg(theme::DIM)),
                    Span::styled(
                        format!("{} ({})", date, date.format("%a")),
                        Style::default().fg(theme::LABEL_MUTED),
                    ),
                    Span::raw(" "),
                    Span::styled(bar, Style::default().fg(bar_color)),
                    Span::styled(
                        format!(" {:>7}", crate::format_number(*work)),
                        Style::default().fg(theme::PRIMARY),
                    ),
                    Span::styled(
                        format!(
                            "  in {:>5} out {:>5} cw {:>5} cr {:>5}",
                            crate::format_number(*input),
                            crate::format_number(*output),
                            crate::format_number(*cw),
                            crate::format_number(*cr),
                        ),
                        Style::default().fg(theme::DIM),
                    ),
                ]));
            }
            (title, lines)
        }
        6 => {
            let title = " Hourly Average ".to_string();
            // No leading blank line — 24 hours need every row we can get to
            // fit on lower-height terminals.
            let mut lines: Vec<Line> = Vec::new();
            let mut hourly_total: std::collections::HashMap<u8, u64> =
                std::collections::HashMap::new();
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
            let num_days = state.daily_groups.len().max(1) as u64;

            let hourly_avg: std::collections::HashMap<u8, u64> = hourly_total
                .iter()
                .map(|(h, t)| (*h, *t / num_days))
                .collect();

            let max_tokens = hourly_avg.values().max().copied().unwrap_or(1);
            let total_avg: u64 = hourly_avg.values().sum();
            let bar_width = 15usize;

            for hour in (0..24u8).skip(scroll).take(visible_height) {
                let tokens = hourly_avg.get(&hour).copied().unwrap_or(0);
                let ratio = tokens as f64 / max_tokens as f64;
                let filled = (ratio * bar_width as f64).round() as usize;
                let intensity = (ratio * 0.7 + 0.3).min(1.0);
                let bar_color = theme::primary_with_intensity(intensity);
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled));
                let pct = if total_avg > 0 {
                    tokens as f64 / total_avg as f64 * 100.0
                } else {
                    0.0
                };

                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {:>2}:00-{:>2}:00 ", hour, hour + 1),
                        Style::default().fg(theme::DIM),
                    ),
                    Span::styled(bar, Style::default().fg(bar_color)),
                    Span::styled(
                        format!(" {:>9}", crate::format_number(tokens)),
                        Style::default().fg(theme::PRIMARY),
                    ),
                    Span::styled(format!(" {pct:>4.1}%"), Style::default().fg(theme::DIM)),
                ]));
            }
            (title, lines)
        }
        _ => {
            let title = " Unknown ".to_string();
            let lines = vec![Line::from("  No detail view available")];
            (title, lines)
        }
    };

    let scroll_indicator = if can_scroll_up && can_scroll_down {
        " ▲▼ "
    } else if can_scroll_up {
        " ▲ "
    } else if can_scroll_down {
        " ▼ "
    } else {
        ""
    };

    let position_info = if total_items > 0 {
        // Use `items_per_screen` (not `visible_height`) — multi-line items
        // like Projects (3 lines) and Models (5 lines) otherwise inflate
        // the visible count to the raw row count.
        format!(
            " {}-{}/{} ",
            scroll + 1,
            (scroll + items_per_screen).min(total_items),
            total_items
        )
    } else {
        String::new()
    };

    let popup = Paragraph::new(content).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::PRIMARY))
            .title(Span::styled(
                title,
                Style::default().fg(theme::PRIMARY).bold(),
            ))
            .title_bottom(Line::from(vec![
                Span::styled(
                    if state.dashboard_panel == 3 && state.tools_detail_section == 1 {
                        " ←→: section  ↑↓: nav  Enter: expand  o/c: all  q: close "
                    } else if state.dashboard_panel == 3 {
                        " ←→: section  ↑↓: scroll  q: close "
                    } else if state.dashboard_panel == 5 {
                        if state.activity_view_weekly {
                            " ↑↓: scroll  w: daily  q: close "
                        } else {
                            " ↑↓: scroll  w: weekly  q: close "
                        }
                    } else {
                        " ↑↓: scroll  q: close "
                    },
                    Style::default().fg(theme::DIM),
                ),
                Span::styled(scroll_indicator, Style::default().fg(theme::WARNING)),
                Span::styled(position_info, Style::default().fg(theme::DIM)),
            ])),
    );

    frame.render_widget(popup, popup_area);
}

#[cfg(test)]
mod tests {
    use super::push_month_divider_line;
    use ratatui::text::Line;

    fn line_text(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn divider_formats_year_month_days_cost_tokens_avg() {
        let mut lines: Vec<Line<'static>> = Vec::new();
        push_month_divider_line(&mut lines, 2026, 5, 3, 706.0, 2_240_000, 80);
        assert_eq!(lines.len(), 1);
        let text = line_text(&lines[0]);
        assert!(text.contains("26-05"), "expected YY-MM, got: {text}");
        assert!(text.contains("3d"), "expected 3d, got: {text}");
        assert!(text.contains("$706"));
        assert!(text.contains("2.24M"));
        // 706 / 3 = 235.33 → ${:.0} → 235
        assert!(text.contains("avg $235/day"), "got: {text}");
    }

    #[test]
    fn divider_handles_zero_days_without_div_by_zero() {
        let mut lines: Vec<Line<'static>> = Vec::new();
        push_month_divider_line(&mut lines, 2026, 5, 0, 0.0, 0, 80);
        let text = line_text(&lines[0]);
        assert!(text.contains("0d"));
        assert!(text.contains("avg $0/day"));
    }

    #[test]
    fn divider_clamps_negative_cost_to_zero() {
        let mut lines: Vec<Line<'static>> = Vec::new();
        push_month_divider_line(&mut lines, 2026, 5, 1, -50.0, 1000, 80);
        let text = line_text(&lines[0]);
        assert!(
            text.contains("$0"),
            "negative cost should clamp to 0, got: {text}"
        );
    }

    #[test]
    fn spark_line_renders_zero_as_empty_bucket() {
        use chrono::NaiveDate;
        let today = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(); // lint-ok: date-literal
        let line = super::render_spark_line(today, 5, |_| 0.0);
        // All five days should be the empty-bucket glyph.
        assert_eq!(line, "·····");
    }

    #[test]
    fn spark_line_normalises_to_window_max() {
        use chrono::NaiveDate;
        let today = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(); // lint-ok: date-literal
        // Two days: one with the peak value, one with half. The peak must
        // render as `█` and the half-value strictly below it.
        let _half = today - chrono::Duration::days(1);
        let line = super::render_spark_line(today, 2, |d| if d == today { 100.0 } else { 50.0 });
        let chars: Vec<char> = line.chars().collect();
        assert_eq!(chars.len(), 2);
        assert_eq!(chars[1], '█', "peak day renders as full block");
        assert!(chars[0] < chars[1], "half-value renders strictly shorter");
    }

    #[test]
    fn weekly_activity_buckets_by_iso_week_newest_first() {
        use crate::test_helpers::helpers::{
            make_daily_group, make_session_with_tokens, make_test_app_state,
        };
        use chrono::NaiveDate;

        // Fixed Monday + 4 days spanning two ISO weeks. Aggregation is a
        // pure function of `state.daily_groups`, so the reference date is
        // immaterial — pick one for determinism.
        let prev_mon = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(); // lint-ok: date-literal
        let prev_w_a = prev_mon;
        let prev_w_b = prev_mon + chrono::Duration::days(1);
        let this_mon = prev_mon + chrono::Duration::days(7);
        let this_w_a = this_mon;
        let this_w_b = this_mon + chrono::Duration::days(2);

        let mk = |date, tokens: u64| {
            make_daily_group(
                date,
                vec![make_session_with_tokens("p", tokens, 0, "claude-opus")],
            )
        };
        let mut state = make_test_app_state(vec![
            mk(prev_w_a, 100),
            mk(prev_w_b, 200),
            mk(this_w_a, 1000),
            mk(this_w_b, 500),
        ]);
        state.daily_costs = vec![
            (prev_w_a, 1.0),
            (prev_w_b, 2.0),
            (this_w_a, 10.0),
            (this_w_b, 5.0),
        ];

        let weeks = super::weekly_activity(&state);
        assert_eq!(weeks.len(), 2, "two ISO weeks expected");
        assert_eq!(weeks[0].tokens, 1500, "newer week first");
        assert!((weeks[0].cost - 15.0).abs() < 1e-9);
        assert_eq!(weeks[0].active_days, 2);
        assert_eq!(weeks[1].tokens, 300);
        assert!((weeks[1].cost - 3.0).abs() < 1e-9);
        assert_eq!(weeks[1].active_days, 2);
        assert_ne!(weeks[0].label, weeks[1].label);
    }
}
