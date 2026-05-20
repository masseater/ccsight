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

    let cache = crate::infrastructure::Cache::load().ok();
    let daily_groups = DailyGrouper::group_by_date_with_shared_cache(&files, &cache);
    let calculator = CostCalculator::global();

    let use_color = std::io::stdout().is_terminal();
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
    println!(
        "{dim}{}{reset}",
        "-".repeat(74),
        dim = c(C_DIM),
        reset = c(C_RESET)
    );

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

        total_input += day_input;
        total_output += day_output;
        total_cache_w += day_cache_w;
        total_cache_r += day_cache_r;
        total_cost += day_cost;

        // Zero-activity day renders fully dim so the eye skips it.
        let zero_day = day_input == 0 && day_output == 0 && day_cost == 0.0;
        let row_dim = if zero_day { c(C_DIM) } else { "" };
        let reset = c(C_RESET);
        // Cost cell is tier-colored (matches TUI Costs panel). Bold lifts
        // the bottom-line number above the surrounding row.
        let cost_col = if zero_day {
            ""
        } else {
            c(cost_color(day_cost))
        };
        println!(
            "{row_dim}{cyan}{:>12}{reset}{row_dim} {:>12} {:>12} {dim}{:>12} {:>12}{reset}{row_dim} {bold}{cost_col}{:>10}{reset}{row_dim}{reset}",
            group.date.format("%Y-%m-%d"),
            crate::format_number(day_input),
            crate::format_number(day_output),
            crate::format_number(day_cache_w),
            crate::format_number(day_cache_r),
            format!("${:.2}", day_cost),
            cyan = if zero_day { "" } else { c(C_CYAN) },
            dim = if zero_day { "" } else { c(C_DIM) },
            bold = if zero_day { "" } else { c(C_BOLD) },
        );
    }

    println!(
        "{dim}{}{reset}",
        "-".repeat(74),
        dim = c(C_DIM),
        reset = c(C_RESET)
    );
    let total_cost_col = c(cost_color(total_cost));
    println!(
        "{bold}{:>12}{reset} {bold}{:>12} {:>12} {:>12} {:>12}{reset} {bold}{total_cost_col}{:>10}{reset}",
        "Total",
        crate::format_number(total_input),
        crate::format_number(total_output),
        crate::format_number(total_cache_w),
        crate::format_number(total_cache_r),
        format!("${:.2}", total_cost),
        bold = c(C_BOLD),
        reset = c(C_RESET),
    );
}
