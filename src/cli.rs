use std::io::IsTerminal;

use crate::aggregator::{CostCalculator, DailyGrouper};
use crate::infrastructure::FileDiscovery;

// ANSI escape sequences. Emitted only when stdout is a TTY so pipes (e.g.
// `ccsight --daily | column -t`) stay clean. Cost tier coloring mirrors
// the TUI's `cost_style` (5-tier scale, same boundaries) so the daily $
// reads the same in CLI and TUI.
const C_RESET: &str = "\x1b[0m";
const C_BOLD: &str = "\x1b[1m";
const C_DIM: &str = "\x1b[2m";
const C_CYAN: &str = "\x1b[36m";
const C_GREEN: &str = "\x1b[32m"; // SUCCESS — lowest cost tier
const C_YELLOW: &str = "\x1b[33m"; // WARNING
const C_BR_RED: &str = "\x1b[91m"; // ERROR
const C_RED: &str = "\x1b[31m"; // DANGER
const C_BR_MAGENTA: &str = "\x1b[95m"; // CRITICAL — highest tier

/// CLI mirror of `ui::cost_style`. Boundaries and tier-to-color mapping
/// must stay in lockstep with the TUI — when the TUI rebalances tiers,
/// update both. Tiers from low to high spend: SUCCESS / WARNING / ERROR
/// / DANGER / CRITICAL.
fn cost_color(amount: f64) -> &'static str {
    let c = amount.max(0.0);
    if c > 300.0 {
        C_BR_MAGENTA
    } else if c > 100.0 {
        C_RED
    } else if c > 60.0 {
        C_BR_RED
    } else if c > 20.0 {
        C_YELLOW
    } else {
        C_GREEN
    }
}

pub fn show_daily_costs(limit: usize) {
    let files = match FileDiscovery::find_jsonl_files_with_limit(limit) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error finding files: {e}");
            return;
        }
    };
    if files.is_empty() {
        println!("No session files found");
        return;
    }

    let mut cache = crate::infrastructure::Cache::load().ok();
    let daily_groups = DailyGrouper::group_by_date_with_shared_cache(&files, &mut cache);
    let calculator = CostCalculator::global();

    // Detect whether stdout is being piped — if so, drop color and the
    // decorative `----` rules, and print raw integers (no K/M/B suffix)
    // so downstream awk / jq / spreadsheet importers don't have to parse
    // a magnitude suffix. The header stays so the columns are still
    // self-labelled.
    let is_tty = std::io::stdout().is_terminal();
    let use_color = is_tty;
    let c = |code: &'static str| -> &'static str { if use_color { code } else { "" } };

    println!(
        "{bold}{:>12} {:>12} {:>12} {:>12} {:>12} {:>10}{reset}",
        "Date",
        "Input",
        "Output",
        "CacheW",
        "CacheR",
        "Cost",
        bold = c(C_BOLD),
        reset = c(C_RESET),
    );
    if is_tty {
        println!(
            "{dim}{}{reset}",
            "-".repeat(74),
            dim = c(C_DIM),
            reset = c(C_RESET)
        );
    }

    let mut total_input: u64 = 0;
    let mut total_output: u64 = 0;
    let mut total_cache_w: u64 = 0;
    let mut total_cache_r: u64 = 0;
    let mut total_cost: f64 = 0.0;

    for group in daily_groups.iter().rev() {
        let mut day_input: u64 = 0;
        let mut day_output: u64 = 0;
        let mut day_cache_w: u64 = 0;
        let mut day_cache_r: u64 = 0;
        let mut day_cost: f64 = 0.0;

        // Include subagents so `--daily` total matches the TUI Overview /
        // Costs panel. Subagent dispatch is real Anthropic spend tied to the
        // day it ran on.
        for session in &group.sessions {
            for (model, tokens) in &session.day_tokens_by_model {
                day_input += tokens.input_tokens;
                day_output += tokens.output_tokens;
                day_cache_w += tokens.cache_creation_tokens;
                day_cache_r += tokens.cache_read_tokens;

                day_cost += calculator
                    .calculate_cost(tokens, Some(model.as_str()))
                    .unwrap_or(0.0);
            }
        }

        // Skip days with no billable activity. They live in `daily_groups`
        // because the JSONLs contain timestamped non-billing entries (API
        // errors, canceled prompts, system / file-snapshot rows) but they
        // render as noise in a cost summary.
        if day_input == 0 && day_output == 0 && day_cache_w == 0 && day_cache_r == 0 {
            continue;
        }

        total_input += day_input;
        total_output += day_output;
        total_cache_w += day_cache_w;
        total_cache_r += day_cache_r;
        total_cost += day_cost;

        let reset = c(C_RESET);
        // Cost cell is tier-colored (matches TUI Costs panel). Bold lifts
        // the bottom-line number above the surrounding row.
        let cost_col = c(cost_color(day_cost));
        let fmt = |n: u64| {
            if is_tty {
                crate::format_number(n)
            } else {
                n.to_string()
            }
        };
        println!(
            "{cyan}{:>12}{reset} {:>12} {:>12} {dim}{:>12} {:>12}{reset} {bold}{cost_col}{:>10}{reset}",
            group.date.format("%Y-%m-%d"),
            fmt(day_input),
            fmt(day_output),
            fmt(day_cache_w),
            fmt(day_cache_r),
            format!("${:.2}", day_cost),
            cyan = c(C_CYAN),
            dim = c(C_DIM),
            bold = c(C_BOLD),
        );
    }

    if is_tty {
        println!(
            "{dim}{}{reset}",
            "-".repeat(74),
            dim = c(C_DIM),
            reset = c(C_RESET)
        );
    }
    let total_cost_col = c(cost_color(total_cost));
    let fmt = |n: u64| {
        if is_tty {
            crate::format_number(n)
        } else {
            n.to_string()
        }
    };
    println!(
        "{bold}{:>12}{reset} {bold}{:>12} {:>12} {:>12} {:>12}{reset} {bold}{total_cost_col}{:>10}{reset}",
        "Total",
        fmt(total_input),
        fmt(total_output),
        fmt(total_cache_w),
        fmt(total_cache_r),
        format!("${:.2}", total_cost),
        bold = c(C_BOLD),
        reset = c(C_RESET),
    );
}

/// Aggregate `daily_groups` into buckets keyed by a label string (e.g. ISO
/// week or year-month). Returns the labelled rows in insertion order so the
/// caller's date sort propagates through.
fn aggregate_buckets<F>(
    daily_groups: &[crate::aggregator::DailyGroup],
    label: F,
) -> Vec<(String, u64, u64, u64, u64, f64)>
where
    F: Fn(chrono::NaiveDate) -> String,
{
    use std::collections::BTreeMap;
    let calculator = CostCalculator::global();
    // Use BTreeMap keyed on the label; since labels are derived from
    // dates and dates are processed newest-first, we sort labels asc
    // later for chronological output (matches --daily).
    let mut acc: BTreeMap<String, (u64, u64, u64, u64, f64)> = BTreeMap::new();
    for group in daily_groups {
        let key = label(group.date);
        let entry = acc.entry(key).or_insert((0, 0, 0, 0, 0.0));
        for session in &group.sessions {
            for (model, tokens) in &session.day_tokens_by_model {
                entry.0 += tokens.input_tokens;
                entry.1 += tokens.output_tokens;
                entry.2 += tokens.cache_creation_tokens;
                entry.3 += tokens.cache_read_tokens;
                entry.4 += calculator
                    .calculate_cost(tokens, Some(model.as_str()))
                    .unwrap_or(0.0);
            }
        }
    }
    acc.into_iter()
        .map(|(k, (a, b, c, d, e))| (k, a, b, c, d, e))
        .collect()
}

fn print_bucket_rows(
    rows: Vec<(String, u64, u64, u64, u64, f64)>,
    label_header: &str,
    label_w: usize,
) {
    let is_tty = std::io::stdout().is_terminal();
    let use_color = is_tty;
    let c = |code: &'static str| -> &'static str { if use_color { code } else { "" } };
    let fmt = |n: u64| {
        if is_tty {
            crate::format_number(n)
        } else {
            n.to_string()
        }
    };
    // Total table width = label + 5 numeric columns + 5 separators.
    let total_w = label_w + 12 * 4 + 10 + 5;

    println!(
        "{bold}{:>label_w$} {:>12} {:>12} {:>12} {:>12} {:>10}{reset}",
        label_header,
        "Input",
        "Output",
        "CacheW",
        "CacheR",
        "Cost",
        bold = c(C_BOLD),
        reset = c(C_RESET),
    );
    if is_tty {
        println!(
            "{dim}{}{reset}",
            "-".repeat(total_w),
            dim = c(C_DIM),
            reset = c(C_RESET)
        );
    }

    let mut total = (0u64, 0u64, 0u64, 0u64, 0.0f64);
    for (label, input, output, cw, cr, cost) in &rows {
        if *input == 0 && *output == 0 && *cw == 0 && *cr == 0 {
            continue;
        }
        total.0 += input;
        total.1 += output;
        total.2 += cw;
        total.3 += cr;
        total.4 += cost;
        let cost_col = c(cost_color(*cost));
        let reset = c(C_RESET);
        println!(
            "{cyan}{:>label_w$}{reset} {:>12} {:>12} {dim}{:>12} {:>12}{reset} {bold}{cost_col}{:>10}{reset}",
            label,
            fmt(*input),
            fmt(*output),
            fmt(*cw),
            fmt(*cr),
            format!("${:.2}", cost),
            cyan = c(C_CYAN),
            dim = c(C_DIM),
            bold = c(C_BOLD),
        );
    }
    if is_tty {
        println!(
            "{dim}{}{reset}",
            "-".repeat(total_w),
            dim = c(C_DIM),
            reset = c(C_RESET)
        );
    }
    let total_cost_col = c(cost_color(total.4));
    println!(
        "{bold}{:>label_w$}{reset} {bold}{:>12} {:>12} {:>12} {:>12}{reset} {bold}{total_cost_col}{:>10}{reset}",
        "Total",
        fmt(total.0),
        fmt(total.1),
        fmt(total.2),
        fmt(total.3),
        format!("${:.2}", total.4),
        bold = c(C_BOLD),
        reset = c(C_RESET),
    );
}

/// Group days into ISO weeks (Mon-Sun). The label spells out the date
/// range so users don't have to mentally convert `2026-W22` into "what
/// dates did that cover" — format: `Wnn  mm-dd–mm-dd` (en dash range
/// inside the same year, matching the Daily detail popup's weekly view).
pub fn show_weekly_costs(limit: usize) {
    use chrono::{Datelike, Duration};
    let files = match FileDiscovery::find_jsonl_files_with_limit(limit) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error finding files: {e}");
            return;
        }
    };
    if files.is_empty() {
        println!("No session files found");
        return;
    }
    let mut cache = crate::infrastructure::Cache::load().ok();
    let daily_groups = DailyGrouper::group_by_date_with_shared_cache(&files, &mut cache);
    let rows = aggregate_buckets(&daily_groups, |d| {
        let monday = d - Duration::days(d.weekday().num_days_from_monday() as i64);
        let sunday = monday + Duration::days(6);
        // ISO 8601-1:2019 §5.5.4 time-interval notation: two endpoints
        // joined by `/`, with the end abbreviated to whatever differs
        // from the start. Same year + month → only the day differs.
        let end_fmt = if monday.year() == sunday.year() {
            if monday.month() == sunday.month() {
                "%d"
            } else {
                "%m-%d"
            }
        } else {
            "%Y-%m-%d"
        };
        format!("{}/{}", monday.format("%Y-%m-%d"), sunday.format(end_fmt))
    });
    print_bucket_rows(rows, "Week", 22);
}

/// Group days into calendar months. Label `YYYY-MM`.
pub fn show_monthly_costs(limit: usize) {
    use chrono::Datelike;
    let files = match FileDiscovery::find_jsonl_files_with_limit(limit) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error finding files: {e}");
            return;
        }
    };
    if files.is_empty() {
        println!("No session files found");
        return;
    }
    let mut cache = crate::infrastructure::Cache::load().ok();
    let daily_groups = DailyGrouper::group_by_date_with_shared_cache(&files, &mut cache);
    let rows = aggregate_buckets(&daily_groups, |d| format!("{}-{:02}", d.year(), d.month()));
    print_bucket_rows(rows, "Month", 12);
}
