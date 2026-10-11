use crate::aggregator::DailyGroup;

/// Eagerly trigger language detection on a background thread so the first call from
/// the UI thread (when the user hits `s` to summarize) doesn't pay the
/// `defaults read NSGlobalDomain AppleLanguages` cost. Idempotent: subsequent calls
/// are no-ops thanks to the underlying `OnceLock`.
pub fn prefetch_user_language() {
    std::thread::spawn(|| {
        let _ = detect_user_language();
    });
}

fn detect_user_language() -> String {
    use std::sync::OnceLock;
    static CACHED: OnceLock<String> = OnceLock::new();
    return CACHED.get_or_init(detect_user_language_inner).clone();

    fn detect_user_language_inner() -> String {
        if let Some(home) = crate::infrastructure::home_dir() {
            let settings_path = home.join(".claude").join("settings.json");
            if let Ok(content) = std::fs::read_to_string(&settings_path)
                && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
                && let Some(lang) = json.get("language").and_then(|v| v.as_str())
                && !lang.is_empty()
            {
                return lang.to_string();
            }
        }
        #[cfg(target_os = "macos")]
        {
            if let Ok(output) = std::process::Command::new("defaults")
                .args(["read", "NSGlobalDomain", "AppleLanguages"])
                .output()
            {
                let text = String::from_utf8_lossy(&output.stdout);
                for line in text.lines() {
                    let trimmed = line.trim().trim_matches(|c| c == '"' || c == ',');
                    if !trimmed.is_empty() && !trimmed.starts_with('(') && !trimmed.starts_with(')')
                    {
                        return trimmed.to_string();
                    }
                }
            }
        }
        std::env::var("LANG")
            .ok()
            .and_then(|lang| {
                let code = lang.split('.').next()?;
                if code.is_empty() {
                    None
                } else {
                    Some(code.to_string())
                }
            })
            .unwrap_or_else(|| "en_US".to_string())
    }
}

pub fn generate_day_summary(group: &DailyGroup) -> String {
    generate_day_summary_internal(group, true)
}

pub fn regenerate_day_summary(group: &DailyGroup) -> String {
    generate_day_summary_internal(group, false)
}

fn generate_day_summary_internal(group: &DailyGroup, use_cache: bool) -> String {
    use crate::infrastructure::Cache;
    use std::collections::HashMap;

    if use_cache
        && let Ok(cache) = Cache::load()
        && let Some(cached) = cache.get_day_summary(&group.date)
    {
        return format!("(cached)\n\n{cached}");
    }

    let sessions: Vec<_> = group.user_sessions().collect();
    if sessions.is_empty() {
        return "No sessions to summarize.".to_string();
    }

    let calculator = crate::aggregator::CostCalculator::global();

    let mut all_user_requests: Vec<(String, String)> = Vec::new();
    let mut all_files: Vec<String> = Vec::new();
    let mut all_tools: HashMap<String, usize> = HashMap::new();
    let mut project_metrics: HashMap<String, (usize, u64, f64)> = HashMap::new();
    let mut hourly_tokens: HashMap<u8, u64> = HashMap::new();
    let mut lang_counts: HashMap<String, usize> = HashMap::new();
    let mut total_tokens: u64 = 0;
    let mut total_cost: f64 = 0.0;
    let mut earliest_start = None::<chrono::DateTime<chrono::Utc>>;
    let mut latest_end = None::<chrono::DateTime<chrono::Utc>>;

    for session in &sessions {
        let (user_requests, files, tools) =
            extract_session_details_for_date(&session.file_path, Some(group.date));

        let project_short = crate::ui::shorten_project(&session.project_name).to_string();

        for req in user_requests.into_iter().take(5) {
            all_user_requests.push((project_short.clone(), req));
        }
        for file in files {
            if !all_files.contains(&file) {
                all_files.push(file);
            }
        }
        for (tool, count) in tools {
            *all_tools.entry(tool).or_insert(0) += count;
        }

        let session_tokens: u64 = session
            .day_tokens_by_model
            .values()
            .map(crate::aggregator::TokenStats::work_tokens)
            .sum();
        total_tokens += session_tokens;

        let session_cost: f64 = session
            .day_tokens_by_model
            .iter()
            .map(|(model, tokens)| {
                calculator
                    .calculate_cost(tokens, Some(model))
                    .unwrap_or(0.0)
            })
            .sum();
        total_cost += session_cost;

        let pm = project_metrics
            .entry(session.project_name.clone())
            .or_default();
        pm.0 += 1;
        pm.1 += session_tokens;
        pm.2 += session_cost;

        for (hour, tokens) in &session.day_hourly_work_tokens {
            *hourly_tokens.entry(*hour).or_default() += tokens;
        }
        for (lang, count) in &session.day_language_usage {
            *lang_counts.entry(lang.clone()).or_default() += count;
        }

        if earliest_start.is_none() || Some(session.day_first_timestamp) < earliest_start {
            earliest_start = Some(session.day_first_timestamp);
        }
        if latest_end.is_none() || Some(session.day_last_timestamp) > latest_end {
            latest_end = Some(session.day_last_timestamp);
        }
    }

    let time_range = match (earliest_start, latest_end) {
        (Some(s), Some(e)) => format!(
            "{}–{}",
            s.with_timezone(&chrono::Local).format("%H:%M"),
            e.with_timezone(&chrono::Local).format("%H:%M")
        ),
        _ => "-".to_string(),
    };

    let mut context = format!(
        "# Work Summary for {}\n\n\
        ## Overview\n\
        - Sessions: {}\n\
        - Total cost: ${:.2} (API est.)\n\
        - Time range: {}\n\
        - Total tokens: {}\n\n",
        group.date.format("%Y-%m-%d (%a)"),
        sessions.len(),
        total_cost,
        time_range,
        crate::format_number(total_tokens),
    );

    context.push_str("## Session Details:\n");
    for (i, session) in sessions.iter().enumerate() {
        let start_time = session.day_first_timestamp.with_timezone(&chrono::Local);
        let end_time = session.day_last_timestamp.with_timezone(&chrono::Local);
        let duration_mins =
            (session.day_last_timestamp - session.day_first_timestamp).num_minutes();
        let duration_str = if duration_mins >= 60 {
            format!("{}h{}m", duration_mins / 60, duration_mins % 60)
        } else {
            format!("{}m", duration_mins.max(1))
        };

        let session_tokens: u64 = session
            .day_tokens_by_model
            .values()
            .map(crate::aggregator::TokenStats::work_tokens)
            .sum();

        let model = session.model.as_deref().unwrap_or("unknown");
        let branch = session.git_branch.as_deref().unwrap_or("-");
        let cost: f64 = session.cost(calculator);

        let project_short = crate::ui::shorten_project(&session.project_name);

        context.push_str(&format!(
            "\n### Session {}: {} ({}–{}, {}, ${:.2})\n",
            i + 1,
            project_short,
            start_time.format("%H:%M"),
            end_time.format("%H:%M"),
            duration_str,
            cost
        ));
        context.push_str(&format!(
            "- Model: {}, Tokens: {}, Branch: {}\n",
            model,
            crate::format_number(session_tokens),
            branch
        ));

        let (requests, files, tools) =
            extract_session_details_for_date(&session.file_path, Some(group.date));

        if !requests.is_empty() {
            context.push_str("- User requests:\n");
            for req in requests.iter().take(5) {
                let short: String = req.chars().take(150).collect();
                context.push_str(&format!("  - {short}\n"));
            }
        }

        if !files.is_empty() {
            context.push_str(&format!("- Modified files ({}): ", files.len()));
            let file_list: Vec<_> = files
                .iter()
                .take(5)
                .map(|f| {
                    std::path::Path::new(f)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(f)
                })
                .collect();
            context.push_str(&format!("{}\n", file_list.join(", ")));
        }

        if !tools.is_empty() {
            let mut sorted_tools: Vec<_> = tools.iter().collect();
            sorted_tools.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
            let top_tools: Vec<_> = sorted_tools
                .iter()
                .take(5)
                .map(|(t, c)| format!("{t}({c})"))
                .collect();
            context.push_str(&format!("- Tools: {}\n", top_tools.join(", ")));
        }
    }

    context.push_str("\n## Cost by Project:\n");
    let mut sorted_projects: Vec<_> = project_metrics.iter().collect();
    sorted_projects.sort_by(|a, b| {
        b.1.2
            .partial_cmp(&a.1.2)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (name, (sess, tokens, cost)) in &sorted_projects {
        let short = crate::ui::shorten_project(name);
        context.push_str(&format!(
            "- {short}: ${:.2} ({sess} sessions, {} tokens)\n",
            cost,
            crate::format_number(*tokens)
        ));
    }

    if !hourly_tokens.is_empty() {
        context.push_str("\n## Hourly Activity (tokens):\n");
        let mut hours: Vec<_> = hourly_tokens.iter().collect();
        hours.sort_by_key(|&(h, _)| *h);
        let peak_hour = hours.iter().max_by_key(|&(_, t)| **t).map(|(h, _)| **h);
        for (hour, tokens) in &hours {
            let peak = if peak_hour == Some(**hour) {
                " (peak)"
            } else {
                ""
            };
            context.push_str(&format!(
                "- {:02}: {}{peak}\n",
                hour,
                crate::format_number(**tokens)
            ));
        }
    }

    if !lang_counts.is_empty() {
        context.push_str("\n## Languages:\n");
        let mut langs: Vec<_> = lang_counts.iter().collect();
        langs.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        for (lang, count) in langs.iter().take(10) {
            context.push_str(&format!("- {lang}: {count}\n"));
        }
    }

    if !all_tools.is_empty() {
        context.push_str("\n## Tool Usage:\n");
        let mut sorted_tools: Vec<_> = all_tools.iter().collect();
        sorted_tools.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        for (tool, count) in sorted_tools.iter().take(15) {
            context.push_str(&format!("- {tool}: {count}x\n"));
        }
    }

    if !all_files.is_empty() {
        context.push_str(&format!(
            "\n## Files Touched ({} files):\n",
            all_files.len()
        ));
        for file in all_files.iter().take(30) {
            context.push_str(&format!("- {file}\n"));
        }
        if all_files.len() > 30 {
            context.push_str(&format!("... {} more files\n", all_files.len() - 30));
        }
    }

    if !all_user_requests.is_empty() {
        context.push_str("\n## All User Requests:\n");
        for (i, (project, req)) in all_user_requests.iter().take(30).enumerate() {
            let short: String = req.chars().take(150).collect();
            context.push_str(&format!("{}. [{}] {}\n", i + 1, project, short));
        }
        if all_user_requests.len() > 30 {
            context.push_str(&format!(
                "... {} more requests\n",
                all_user_requests.len() - 30
            ));
        }
    }

    let output_lang = detect_user_language();
    let prompt = format!(
        "Below is a detailed Claude Code work log for one day.\n\
        Create a technically accurate daily report.\n\
        \n\
        Output language: {output_lang}\n\
        \n\
        Rules:\n\
        - Include specific filenames, feature names, and implementation details\n\
        - Include cost ($) for each session and project\n\
        - Only describe facts from the log, do not speculate or hallucinate\n\
        - Use the exact project/file names from the log\n\
        \n\
        Format:\n\
        ## {{date}} Daily Report\n\
        \n\
        {{sessions}} sessions / total cost ${{cost}} / {{time_range}}\n\
        \n\
        ## Work by Session\n\
        (Each session: project name, cost, time range, bullet points of work done)\n\
        \n\
        ## Work by Project\n\
        (Per project: cost, session count, key tasks)\n\
        \n\
        ## Technical Highlights\n\
        (Technical achievements, problems solved)\n\
        \n\
        ## Patterns\n\
        (Languages, tools, hourly activity patterns)\n\n\
        ---\n{context}\n---"
    );

    let result = match spawn_claude(&prompt).and_then(std::process::Child::wait_with_output) {
        Ok(output) => {
            if output.status.success() {
                let out = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if out.is_empty() {
                    "Error: claude returned empty output".to_string()
                } else {
                    out
                }
            } else {
                format!("Error: {}", String::from_utf8_lossy(&output.stderr).trim())
            }
        }
        Err(e) => format!("Failed to run claude: {e}"),
    };

    if !result.starts_with("Error")
        && !result.starts_with("Failed")
        && let Ok(mut cache) = Cache::load()
    {
        cache.set_day_summary(&group.date, result.clone());
        let _ = cache.save();
    }

    result
}

/// `claude -p --model <SUMMARY_MODEL>` with the prompt fed on stdin rather
/// than argv: Windows caps a whole command line at 32K chars, which a busy
/// day's summary context can exceed. A writer thread feeds stdin so a large
/// prompt can't deadlock against the child filling its stdout pipe.
fn spawn_claude(prompt: &str) -> std::io::Result<std::process::Child> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = std::process::Command::new(claude_program())
        .args(["-p", "--model", crate::SUMMARY_MODEL])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let prompt = prompt.to_owned();
        std::thread::spawn(move || {
            let _write = stdin.write_all(prompt.as_bytes());
        });
    }
    Ok(child)
}

/// The `claude` executable, resolved with PATHEXT on Windows: std's own
/// lookup only tries `claude.exe` and misses the npm `claude.cmd` shim.
/// Falls back to bare `claude` (the spawn error then surfaces as-is).
fn claude_program() -> std::ffi::OsString {
    which::which("claude").map_or_else(|_| "claude".into(), std::path::PathBuf::into_os_string)
}

pub fn extract_session_details(
    file_path: &std::path::Path,
) -> (
    Vec<String>,
    Vec<String>,
    std::collections::HashMap<String, usize>,
) {
    extract_session_details_for_date(file_path, None)
}

pub fn extract_session_details_for_date(
    file_path: &std::path::Path,
    filter_date: Option<chrono::NaiveDate>,
) -> (
    Vec<String>,
    Vec<String>,
    std::collections::HashMap<String, usize>,
) {
    use crate::domain::Role;
    use crate::parser::JsonlParser;
    use chrono::Local;
    use std::collections::HashMap;

    let Ok(entries) = JsonlParser::parse_file(file_path) else {
        return (vec![], vec![], HashMap::new());
    };

    let mut user_requests: Vec<String> = Vec::new();
    let mut files_modified: Vec<String> = Vec::new();
    let mut tool_counts: HashMap<String, usize> = HashMap::new();

    for entry in &entries {
        if let Some(date) = filter_date {
            if let Some(ts) = entry.timestamp {
                let entry_date = ts.with_timezone(&Local).date_naive();
                if entry_date != date {
                    continue;
                }
            } else {
                continue;
            }
        }

        if let Some(ref message) = entry.message {
            if message.role == Role::User {
                let text = message.content.extract_text();
                if text.chars().count() > 10 {
                    let truncated: String = text.chars().take(300).collect();
                    user_requests.push(truncated);
                }
            }

            for (tool_name, file_path) in message.content.extract_tool_calls() {
                *tool_counts.entry(tool_name.clone()).or_insert(0) += 1;

                if let Some(path) = file_path
                    && matches!(tool_name.as_str(), "Edit" | "Write" | "Read")
                {
                    let short_path = path.rsplit(['/', '\\']).next().unwrap_or(&path).to_string();
                    if !files_modified.contains(&short_path) {
                        files_modified.push(short_path);
                    }
                }
            }
        }
    }

    (user_requests, files_modified, tool_counts)
}

pub fn generate_session_summary(
    session: &crate::aggregator::SessionInfo,
    filter_date: Option<chrono::NaiveDate>,
) -> String {
    generate_session_summary_internal(session, true, filter_date)
}

pub fn regenerate_session_summary(
    session: &crate::aggregator::SessionInfo,
    filter_date: Option<chrono::NaiveDate>,
) -> String {
    generate_session_summary_internal(session, false, filter_date)
}

fn summary_cache_key(
    session: &crate::aggregator::SessionInfo,
    filter_date: Option<chrono::NaiveDate>,
) -> std::path::PathBuf {
    match filter_date {
        Some(date) => {
            let mut key = session.file_path.clone();
            let name = key
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            key.set_file_name(format!("{name}_{date}"));
            key
        }
        None => session.file_path.clone(),
    }
}

fn generate_session_summary_internal(
    session: &crate::aggregator::SessionInfo,
    use_cache: bool,
    filter_date: Option<chrono::NaiveDate>,
) -> String {
    use crate::infrastructure::Cache;

    let cache_key = summary_cache_key(session, filter_date);

    if use_cache
        && let Ok(cache) = Cache::load()
        && let Some(cached) = cache.get_session_summary(&cache_key)
    {
        return format!("(cached)\n\n{cached}");
    }

    let (user_requests, files_modified, tool_counts) =
        extract_session_details_for_date(&session.file_path, filter_date);

    if user_requests.is_empty() {
        return "No conversation to summarize.".to_string();
    }

    let mut context = String::new();

    context.push_str(&format!("## Project: {}\n", session.project_name));
    if let Some(ref branch) = session.git_branch {
        context.push_str(&format!("## Branch: {branch}\n"));
    }
    if let Some(date) = filter_date {
        context.push_str(&format!("## Date: {date} (this day's activity only)\n"));
    }
    context.push('\n');

    context.push_str("## User Requests:\n");
    for (i, req) in user_requests.iter().take(10).enumerate() {
        context.push_str(&format!("{}. {}\n", i + 1, req));
    }
    if user_requests.len() > 10 {
        context.push_str(&format!("... {} more requests\n", user_requests.len() - 10));
    }

    if !files_modified.is_empty() {
        context.push_str("\n## Files Touched:\n");
        for file in files_modified.iter().take(20) {
            context.push_str(&format!("- {file}\n"));
        }
    }

    if !tool_counts.is_empty() {
        context.push_str("\n## Tools Used:\n");
        let mut sorted_tools: Vec<_> = tool_counts.iter().collect();
        sorted_tools.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        for (tool, count) in sorted_tools.iter().take(10) {
            context.push_str(&format!("- {tool}: {count}x\n"));
        }
    }

    if let Some(ref existing_summary) = session.summary {
        context.push_str(&format!("\n## Existing Summary:\n{existing_summary}\n"));
    }

    let output_lang = detect_user_language();
    let prompt = format!(
        "Below is a detailed Claude Code work session.\n\
        Summarize specifically what was accomplished.\n\
        \n\
        Output language: {output_lang}\n\
        \n\
        Rules:\n\
        - Include technical details, specific code changes, and implementation content\n\
        - Only describe facts from the log, do not speculate\n\
        \n\
        Format:\n\
        ## Goal\n\
        (What was the objective, 1-2 sentences)\n\
        \n\
        ## Work Done\n\
        (Specific technical tasks in bullet points)\n\
        \n\
        ## Code Changes\n\
        (Key file changes and their content)\n\
        \n\
        ## Results\n\
        (What was completed, problems solved, improvements made)\n\n\
        ---\n{context}\n---"
    );

    let result = match spawn_claude(&prompt).and_then(std::process::Child::wait_with_output) {
        Ok(output) => {
            if output.status.success() {
                let out = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if out.is_empty() {
                    "Error: claude returned empty output".to_string()
                } else {
                    out
                }
            } else {
                format!("Error: {}", String::from_utf8_lossy(&output.stderr).trim())
            }
        }
        Err(e) => format!("Failed to run claude: {e}"),
    };

    if !result.starts_with("Error")
        && !result.starts_with("Failed")
        && let Ok(mut cache) = Cache::load()
    {
        cache.set_session_summary(&cache_key, result.clone());
        let _ = cache.save();
    }

    result
}

// JSONL title regeneration / append.
// Writes the result back into the session's JSONL as a `type: "custom-title"`
// row (the field the resume picker shows and `display_title` ranks first), so
// external tools and ccsight on next reload see the updated label.

/// Context-compaction recaps, oldest first — each a `user` message that
/// summarises everything before it, so the earliest and latest bracket what a
/// long session set out to do and where it got to. Re-reads the file rather
/// than widening `extract_session_details`'s shared tuple for three callers
/// that have no use for them.
fn extract_compaction_recaps(file_path: &std::path::Path) -> Vec<String> {
    use crate::parser::JsonlParser;
    let Ok(entries) = JsonlParser::parse_file(file_path) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter(|e| e.is_compact_summary)
        .filter_map(|e| e.message.as_ref())
        .map(|m| crate::text::clean_user_message_preview(&m.content.extract_text()))
        .filter(|t| !t.is_empty())
        .collect()
}

/// Earliest + latest recap: the pair brackets a long session. One recap alone
/// pulls the title toward whichever end it came from.
fn bracket_recaps(recaps: &[String]) -> Vec<&String> {
    match recaps.len() {
        0 => Vec::new(),
        1 => vec![&recaps[0]],
        _ => vec![&recaps[0], &recaps[recaps.len() - 1]],
    }
}

/// Requests as the title prompt should see them: the same noise the
/// conversation preview strips, consecutive repeats collapsed, and anything
/// left too short to name a topic dropped. Falls back to the raw list rather
/// than handing the prompt nothing.
fn title_request_candidates(reqs: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in reqs {
        let cleaned = crate::text::clean_user_message_preview(raw);
        if cleaned.chars().count() <= 10 || out.last().is_some_and(|p| *p == cleaned) {
            continue;
        }
        out.push(cleaned);
    }
    if out.is_empty() { reqs.to_vec() } else { out }
}

/// Rows handed to the title prompt.
const TITLE_SAMPLES: usize = 14;

/// Evenly spaced across the WHOLE list, first and last included. A head-plus-
/// tail sample made the title describe the latest request: a long session's
/// opening rows are the caveat / slash-command text Claude Code injects, so
/// only the tail carried signal and the body was never seen.
fn sample_title_requests(reqs: &[String]) -> Vec<&String> {
    if reqs.len() <= TITLE_SAMPLES {
        return reqs.iter().collect();
    }
    let last = reqs.len() - 1;
    (0..TITLE_SAMPLES)
        .map(|i| &reqs[i * last / (TITLE_SAMPLES - 1)])
        .collect()
}

/// Chars of each compaction recap fed to the prompt. A recap opens with its
/// "Primary Request and Intent" section, so the head carries the purpose; a
/// larger budget pulls the title toward whatever that section detailed.
const RECAP_BUDGET: usize = 1500;

/// Build the title-generation prompt context. Recaps lead — they summarise the
/// whole session, which no sample of requests can. File names are generic and
/// add little to a title, so they are omitted.
fn build_title_context(
    project: &str,
    summary: Option<&str>,
    recaps: &[String],
    user_requests: &[String],
) -> String {
    let mut context = format!("Project: {project}\n");
    if let Some(summary) = summary {
        let recap: String = summary.chars().take(300).collect();
        context.push_str(&format!("\nSession recap:\n{recap}\n"));
    }
    let bracketed = bracket_recaps(recaps);
    for (i, recap) in bracketed.iter().enumerate() {
        let label = if bracketed.len() > 1 && i == 0 {
            "earliest"
        } else {
            "latest"
        };
        let head: String = recap.chars().take(RECAP_BUDGET).collect();
        context.push_str(&format!(
            "\nSession recap ({label} compaction summary):\n{head}\n"
        ));
    }
    let candidates = title_request_candidates(user_requests);
    context.push_str("\nUser requests (sampled across the whole session, in order):\n");
    for req in sample_title_requests(&candidates) {
        let truncated: String = req.chars().take(100).collect();
        context.push_str(&format!("- {truncated}\n"));
    }
    context
}

/// Openings of the prompts ccsight hands `claude -p`, with the label its
/// session shows. Those calls leave transcripts like any session; matching a
/// session's first prompt against these names it as ccsight's own work.
pub(crate) const GENERATED_PROMPTS: &[(&str, &str)] = &[
    (
        "Generate a VERY SHORT title (max 60 chars) for this Claude Code session",
        "ccsight · title generation",
    ),
    (
        "Below is a detailed Claude Code work session.",
        "ccsight · session summary",
    ),
    (
        "Below is a detailed Claude Code work log for one day.",
        "ccsight · day summary",
    ),
];

/// The title instruction. Asking for the project name without saying where it
/// goes produced `<project>: topic` in most generations — a label rather than a
/// title, so the phrasing puts the name inside the phrase.
fn title_prompt(context: &str) -> String {
    format!(
        "Generate a VERY SHORT title (max 60 chars) for this Claude Code session,\n\
        in the same language as the session. Any recap below summarises the whole\n\
        session and is the strongest signal; the requests are sampled evenly across\n\
        the session in order. Name what the session as a whole was about — NOT just\n\
        the latest request. Name the project inside the phrase itself, so the whole\n\
        title reads as one continuous noun phrase.\n\
        Reply with ONLY the title, nothing else.\n\n\
        ---\n{context}\n---"
    )
}

pub(crate) fn regenerate_jsonl_title(
    session: &crate::aggregator::SessionInfo,
) -> Result<String, String> {
    let (user_requests, _, _) = extract_session_details(&session.file_path);

    if user_requests.is_empty() {
        return Err("No conversation to summarize".to_string());
    }

    let recaps = extract_compaction_recaps(&session.file_path);
    let context = build_title_context(
        &session.project_name,
        session.summary.as_deref(),
        &recaps,
        &user_requests,
    );

    let prompt = title_prompt(&context);

    use std::io::Read;
    use std::thread;
    use std::time::{Duration, Instant};

    let mut child = spawn_claude(&prompt).map_err(|e| format!("Failed to spawn claude: {e}"))?;

    let timeout = Duration::from_secs(60);
    let start = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("Timeout: claude command took too long".to_string());
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("Error waiting for claude: {e}")),
        }
    }

    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }

    let summary = stdout.trim().to_string();
    if summary.is_empty() && !stderr.is_empty() {
        return Err(format!("claude error: {}", stderr.trim()));
    }

    // `truncate_with_ellipsis` is width-aware and no-ops when it fits.
    Ok(crate::text::truncate_with_ellipsis(&summary, 80))
}

// Appends a `type: "custom-title"` row keyed by `sessionId` — the same shape
// Claude Code's `/rename` writes and the resume picker reads. Append-only and
// last-wins, so the latest row is the active title (matching `display_title`'s
// reverse-walk read). Unlike `summary`, no leafUuid scan is needed.
pub(crate) fn update_jsonl_custom_title(
    file_path: &std::path::Path,
    session_id: &str,
    title: &str,
) -> Result<(), String> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let new_entry = serde_json::json!({
        "type": "custom-title",
        "customTitle": title,
        "sessionId": session_id,
    });

    let mut file = OpenOptions::new()
        .append(true)
        .open(file_path)
        .map_err(|e| format!("Failed to open file for append: {e}"))?;

    let json_str =
        serde_json::to_string(&new_entry).map_err(|e| format!("JSON serialization error: {e}"))?;
    writeln!(file, "{json_str}").map_err(|e| format!("Write error: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    #[test]
    fn title_context_leads_with_recap_and_omits_files() {
        let reqs = vec!["fix the parser".to_string(), "add a test".to_string()];
        let ctx = build_title_context("myproj", Some("Refactored the JSONL parser"), &[], &reqs);
        assert!(ctx.contains("Project: myproj"));
        assert!(ctx.contains("Session recap:\nRefactored the JSONL parser"));
        assert!(ctx.contains("- fix the parser"));
        // Files are intentionally not fed to the title prompt.
        assert!(!ctx.contains("Files modified"));
        // Recap precedes the user requests so the model weights it first.
        assert!(ctx.find("Session recap").unwrap() < ctx.find("User requests").unwrap());
    }

    #[test]
    fn title_context_without_summary_falls_back_to_requests() {
        let reqs: Vec<String> = (0..8).map(|i| format!("request number {i}")).collect();
        let ctx = build_title_context("p", None, &[], &reqs);
        assert!(!ctx.contains("Session recap"));
        assert_eq!(
            ctx.matches("- request number").count(),
            8,
            "short list passes whole"
        );
    }

    // Asking for the project name without saying where it goes produced
    // `<project>: topic` — a label, not a title. Both halves of the fix (name
    // it, and put it inside the phrase) have to survive prompt edits.
    #[test]
    fn the_title_prompt_opens_with_its_generated_prompt_marker() {
        // Drift here stops ccsight recognising its own title sessions.
        let (opening, _) = GENERATED_PROMPTS[0];
        assert!(title_prompt("ctx").starts_with(opening));
    }

    #[test]
    fn title_prompt_places_the_project_inside_the_phrase() {
        let prompt = title_prompt("Project: p\n");
        assert!(
            prompt.contains("Name the project inside the phrase"),
            "{prompt}"
        );
        assert!(prompt.contains("one continuous noun phrase"), "{prompt}");
        assert!(
            prompt.contains("NOT just"),
            "whole-session instruction is gone"
        );
        assert!(
            prompt.ends_with("---"),
            "context must be delimited: {prompt}"
        );
    }

    // The compaction recap is the only input that summarises the whole session;
    // one alone pulls the title toward whichever end it came from.
    #[test]
    fn bracket_recaps_takes_the_ends() {
        let none: Vec<String> = Vec::new();
        assert!(bracket_recaps(&none).is_empty());

        let one = vec!["only".to_string()];
        assert_eq!(bracket_recaps(&one), vec![&one[0]]);

        let many: Vec<String> = (0..5).map(|i| format!("r{i}")).collect();
        assert_eq!(bracket_recaps(&many), vec![&many[0], &many[4]]);
    }

    // The recap is the strongest signal, so it has to reach the prompt labelled
    // and ahead of the sampled requests.
    #[test]
    fn title_context_carries_both_recap_ends_before_the_requests() {
        let recaps: Vec<String> = vec![
            "opening intent alpha".to_string(),
            "middle beta".to_string(),
            "final state gamma".to_string(),
        ];
        let reqs = vec!["do the thing".to_string()];
        let ctx = build_title_context("proj", None, &recaps, &reqs);

        assert!(ctx.contains("earliest compaction summary"), "{ctx}");
        assert!(ctx.contains("latest compaction summary"), "{ctx}");
        assert!(ctx.contains("opening intent alpha"));
        assert!(ctx.contains("final state gamma"));
        assert!(!ctx.contains("middle beta"), "only the ends are fed");
        assert!(
            ctx.find("compaction summary").unwrap() < ctx.find("User requests").unwrap(),
            "recap must precede the requests"
        );
    }

    // A head-plus-tail sample made the title describe the latest request: the
    // opening rows of a long session are injected caveat / slash-command text,
    // so the body of the work never reached the prompt.
    #[test]
    fn sample_title_requests_spans_the_whole_session() {
        let reqs: Vec<String> = (0..4).map(|i| i.to_string()).collect();
        assert_eq!(
            sample_title_requests(&reqs).len(),
            4,
            "a list under the sample size passes through unchanged"
        );

        let long: Vec<String> = (0..1000).map(|i| format!("r{i}")).collect();
        let picked: Vec<usize> = sample_title_requests(&long)
            .iter()
            .map(|r| r.trim_start_matches('r').parse::<usize>().unwrap())
            .collect();
        assert_eq!(picked.len(), TITLE_SAMPLES);
        assert_eq!(picked[0], 0, "keeps the opening");
        assert_eq!(*picked.last().unwrap(), 999, "keeps the latest");
        assert!(
            picked.windows(2).all(|w| w[0] < w[1]),
            "in order: {picked:?}"
        );
        // The middle has to be represented, not skipped over.
        let mid = picked.iter().filter(|&&i| (300..700).contains(&i)).count();
        assert!(mid >= 4, "middle of the session is missing: {picked:?}");
    }

    // The title prompt sees what the conversation preview shows, not the raw
    // injected text — the opening rows are otherwise pure noise.
    #[test]
    fn title_candidates_drop_noise_and_consecutive_repeats() {
        let reqs = vec![
            "<command-name>/clear</command-name> <command-message>clear</command-message>"
                .to_string(),
            "investigate the parser cap".to_string(),
            "investigate the parser cap".to_string(),
            "ok".to_string(),
            "measure the memory ceiling".to_string(),
        ];
        let out = title_request_candidates(&reqs);
        assert_eq!(
            out,
            vec![
                "investigate the parser cap".to_string(),
                "measure the memory ceiling".to_string()
            ],
            "noise, the repeat and the ack all drop"
        );

        // Never hand the prompt an empty list.
        let all_noise = vec!["<command-name>/x</command-name>".to_string()];
        assert_eq!(title_request_candidates(&all_noise), all_noise);
    }

    use std::sync::atomic::{AtomicU64, Ordering};
    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempFile(PathBuf);
    impl TempFile {
        fn new(prefix: &str) -> Self {
            let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!("{prefix}_{id}.jsonl")))
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
        fn write_jsonl(&self, entries: &[serde_json::Value]) {
            let mut f = std::fs::File::create(&self.0).unwrap();
            for e in entries {
                writeln!(f, "{}", serde_json::to_string(e).unwrap()).unwrap();
            }
        }
    }
    impl Drop for TempFile {
        fn drop(&mut self) {
            std::fs::remove_file(&self.0).ok();
        }
    }

    #[test]
    fn update_jsonl_custom_title_appends_a_custom_title_row() {
        let tmp = TempFile::new("ccsight_custom_title");
        tmp.write_jsonl(&[serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": "hello"}
        })]);

        update_jsonl_custom_title(tmp.path(), "sess-123", "セッションの要約").unwrap();

        let lines: Vec<String> = std::fs::read_to_string(tmp.path())
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        // Append-only: the original user row is preserved, title row added last.
        assert_eq!(lines.len(), 2, "append must not rewrite existing rows");
        let row: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(row["type"], "custom-title");
        assert_eq!(row["customTitle"], "セッションの要約");
        assert_eq!(row["sessionId"], "sess-123");
        // Exactly the 3-key shape Claude Code's /rename writes — no extra fields.
        assert_eq!(row.as_object().unwrap().len(), 3);
    }

    #[test]
    fn test_extract_session_details_user_requests() {
        let tmp = TempFile::new("ccsight_summary_test_requests.jsonl");
        tmp.write_jsonl(&[
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-03-20T10:00:00Z",
                "message": {"role": "user", "content": "Please fix the authentication flow in the login page"}
            }),
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-03-20T10:01:00Z",
                "message": {"role": "assistant", "content": "I'll fix that now."}
            }),
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-03-20T10:05:00Z",
                "message": {"role": "user", "content": "Now add unit tests for the auth module"}
            }),
        ]);

        let (requests, _files, _tools) = extract_session_details(tmp.path());
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("authentication flow"));
        assert!(requests[1].contains("unit tests"));
    }

    #[test]
    fn test_extract_session_details_short_messages_skipped() {
        let tmp = TempFile::new("ccsight_summary_test_short.jsonl");
        tmp.write_jsonl(&[
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-03-20T10:00:00Z",
                "message": {"role": "user", "content": "ok"}
            }),
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-03-20T10:01:00Z",
                "message": {"role": "user", "content": "yes please"}
            }),
        ]);

        let (requests, _files, _tools) = extract_session_details(tmp.path());
        assert!(
            requests.is_empty(),
            "short messages (<=10 chars) should be skipped"
        );
    }

    #[test]
    fn test_extract_session_details_tool_usage() {
        let tmp = TempFile::new("ccsight_summary_test_tools.jsonl");
        tmp.write_jsonl(&[
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-03-20T10:00:00Z",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "tool_use", "id": "1", "name": "Read", "input": {"file_path": "/src/main.rs"}},
                        {"type": "tool_use", "id": "2", "name": "Edit", "input": {"file_path": "/src/lib.rs"}},
                        {"type": "tool_use", "id": "3", "name": "Read", "input": {"file_path": "/src/test.rs"}}
                    ]
                }
            }),
        ]);

        let (_requests, files, tools) = extract_session_details(tmp.path());
        assert_eq!(*tools.get("Read").unwrap_or(&0), 2);
        assert_eq!(*tools.get("Edit").unwrap_or(&0), 1);
        assert!(files.contains(&"main.rs".to_string()));
        assert!(files.contains(&"lib.rs".to_string()));
        assert!(files.contains(&"test.rs".to_string()));
    }

    #[test]
    fn test_extract_session_details_empty_file() {
        let tmp = TempFile::new("ccsight_summary_test_empty.jsonl");
        tmp.write_jsonl(&[]);

        let (requests, files, tools) = extract_session_details(tmp.path());
        assert!(requests.is_empty());
        assert!(files.is_empty());
        assert!(tools.is_empty());
    }

    #[test]
    fn test_extract_session_details_for_date_filters_by_date() {
        let tmp = TempFile::new("ccsight_summary_test_date.jsonl");
        tmp.write_jsonl(&[
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-03-20T10:00:00Z",
                "message": {"role": "user", "content": "Work on the first day of the sprint planning"}
            }),
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-03-21T10:00:00Z",
                "message": {"role": "user", "content": "Continue with the second day implementation tasks"}
            }),
        ]);

        let date = chrono::NaiveDate::from_ymd_opt(2026, 3, 20).unwrap(); // lint-ok: date-literal
        let (requests, _files, _tools) = extract_session_details_for_date(tmp.path(), Some(date));
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("first day"));
    }

    #[test]
    fn test_extract_session_details_for_date_none_returns_all() {
        let tmp = TempFile::new("ccsight_summary_test_date_none.jsonl");
        tmp.write_jsonl(&[
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-03-20T10:00:00Z",
                "message": {"role": "user", "content": "Work on the first day of the sprint planning"}
            }),
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-03-21T10:00:00Z",
                "message": {"role": "user", "content": "Continue with the second day implementation tasks"}
            }),
        ]);

        let (requests, _files, _tools) = extract_session_details_for_date(tmp.path(), None);
        assert_eq!(requests.len(), 2);
    }

    #[test]
    fn test_extract_session_details_nonexistent_file() {
        let (requests, files, tools) =
            extract_session_details(std::path::Path::new("/tmp/nonexistent_ccsight_test.jsonl"));
        assert!(requests.is_empty());
        assert!(files.is_empty());
        assert!(tools.is_empty());
    }
}
