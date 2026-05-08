//! Time-bucket aggregations (monthly cost / monthly tokens / weekday avg)
//! shared by both compact Insights panels and their detail popups.

use std::collections::{BTreeMap, HashMap};

use chrono::{Datelike, NaiveDate, Weekday};

use crate::aggregator::{DailyGroup, SessionInfo};

/// Aggregate per-month cost (key = "YYYY-MM") from `state.daily_costs`.
/// Used by both the compact Insights panel and its detail popup.
pub(crate) fn aggregate_monthly_costs(
    daily_costs: &[(NaiveDate, f64)],
) -> BTreeMap<String, f64> {
    let mut map = BTreeMap::new();
    for (date, cost) in daily_costs {
        let key = format!("{}-{:02}", date.year(), date.month());
        *map.entry(key).or_insert(0.0) += cost;
    }
    map
}

/// Aggregate per-month work-token totals (key = "YYYY-MM"), excluding subagent
/// sessions to match the cost path's accounting. Used by the Monthly detail popup.
pub(crate) fn aggregate_monthly_tokens(
    daily_groups: &[DailyGroup],
) -> BTreeMap<String, u64> {
    let mut map = BTreeMap::new();
    for group in daily_groups {
        let key = format!("{}-{:02}", group.date.year(), group.date.month());
        let tokens: u64 = group
            .sessions
            .iter()
            .filter(|s| !s.is_subagent)
            .map(SessionInfo::work_tokens)
            .sum();
        *map.entry(key).or_insert(0) += tokens;
    }
    map
}

/// Per-weekday work-token average (token sum / occurrences-of-that-weekday).
/// Skips subagent sessions. Returned map always has every `Weekday` key.
pub(crate) fn aggregate_weekday_avg(
    daily_groups: &[DailyGroup],
    calendar_days: usize,
    first_date: NaiveDate,
) -> HashMap<Weekday, u64> {
    const ALL_WEEKDAYS: [Weekday; 7] = [
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
        Weekday::Sat,
        Weekday::Sun,
    ];
    // Include subagent sessions to match the Costs panel and Insights
    // `tokens/day` accounting, so all `K/day` values agree.
    let mut work: HashMap<Weekday, u64> = HashMap::new();
    for group in daily_groups {
        let tokens: u64 = group
            .sessions
            .iter()
            .map(SessionInfo::work_tokens)
            .sum();
        *work.entry(group.date.weekday()).or_insert(0) += tokens;
    }
    let mut avg = HashMap::with_capacity(7);
    for wd in ALL_WEEKDAYS {
        let count = weekday_occurrence_count(calendar_days, first_date, wd);
        avg.insert(wd, work.get(&wd).copied().unwrap_or(0) / count as u64);
    }
    avg
}

/// How many times a given weekday appears across `calendar_days` starting at
/// `first_date`. Used by [`aggregate_weekday_avg`] (and the Insights weekly
/// detail's "Most active" caption) to convert raw weekday tokens into an
/// average per occurrence. Always returns at least 1 to avoid div-by-zero.
pub(crate) fn weekday_occurrence_count(
    calendar_days: usize,
    first_date: NaiveDate,
    weekday: Weekday,
) -> u32 {
    let full_weeks = calendar_days as u32 / 7;
    let remainder = calendar_days as u32 % 7;
    let extra = if remainder > 0 {
        let first_wd = first_date.weekday().num_days_from_monday();
        let target_wd = weekday.num_days_from_monday();
        let offset = (target_wd + 7 - first_wd) % 7;
        if offset < remainder { 1 } else { 0 }
    } else {
        0
    };
    (full_weeks + extra).max(1)
}
