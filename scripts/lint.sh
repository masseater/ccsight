#!/bin/bash
# Project linter for ccsight.
# Checks for common pattern violations across all source files. Each rule is a
# self-contained block below numbered #1..#N. To add a new rule: copy an existing
# block, bump the number, and document its intent in the header comment.
#
# Severity: most rules ERROR (block commit); a few stay WARNING when the violation
# is sometimes intentional and depends on review context.
#
# File sets are DISCOVERED dynamically via `find` so future refactors and new
# files (e.g., splitting `src/ui/mod.rs` into `src/ui/popups.rs`) get covered
# automatically without editing this script. Hardcoded paths are limited to the
# very few files whose location is structural (e.g., `src/main.rs`).
#
# Rule index (by number → topic):
#   1   Title spans use Span::styled         (UI files)
#   2   Borders need .border_style           (UI files)
#   3   Date format: %Y-%m-%d / %m-%d        (all Rust files)
#   4   Cost precision (no .4+) + use format_cost (all Rust files)
#   5   No raw u16 subtract on area dims     (UI files)
#   6   sessions iter without subagent filter (main.rs, warning)
#   7   Scroll indicator uses ▲▼ not ↑↓      (UI files)
#   8   Use shorten_project() helper         (UI + summary)
#   9   Use spawn_load_conversation()        (main.rs)
#   10  Use weekday_occurrence_count()       (UI files)
#   11  Use draw_scrollbar() helper          (UI files)
#   12  No legacy conversation_* fields      (main + UI)
#   13  Use ConversationPane::load_from()    (main.rs)
#   14  Avoid raw sessions[idx] index        (warning, allowlists session_indices ctx)
#   15  TextInput / char_indices for UTF-8   (main.rs)
#   16  Generic placeholder identifiers      (mcp__/skill:/agent: literals)
#   17  Denylist of real-world terms         (.lint-forbidden-terms, git-ignored)
#   18  Popup inner height = popup_height-2  (UI files; title_bottom shares border)
#   19  Stale-MCP threshold via is_underutilized (no inline `num_days >= 30`)
#   20  No stderr writes outside cli.rs / panic hook (corrupts TUI)
#   21  Generic placeholder `command:` literals (mirrors #16 for command prefix)
#   22  Saturating sub on inner/popup height too (extends #5 beyond `area.*`)
#   23  No captured numeric values in comments (per CLAUDE.md "Comments")
#   24  Use state.project_label() in render paths
#   25  No literal calendar dates (use offsets or `// lint-ok: date-literal`)
#   26  `try_recv()` must use `match` with Disconnected arm
#   27  No `from_timestamp(0, 0).unwrap()` — use UNIX_EPOCH
#   28  Shell commands via `format!` must use `posix_shell_quote`
#   29  Atomic tmp+rename must `sync_all` before rename
#   30  HashMap-sourced single-key sort needs `.then_with(...)` tiebreaker
#   31  No leading `Line::from("")` in popup content vecs (title sits on border)

set -e

ERRORS=0

# All Rust sources under src/. Adapts to additions/renames/splits.
ALL_RUST_FILES=$(find src -name '*.rs' -type f 2>/dev/null | sort | tr '\n' ' ')
# UI subset — checks that target ratatui rendering patterns.
UI_FILES=$(find src/ui -name '*.rs' -type f 2>/dev/null | sort | tr '\n' ' ')
# Top-level docs that may also contain text we want to scrub for leaks.
DOC_FILES=$(ls CLAUDE.md README.md 2>/dev/null | tr '\n' ' ')

# Single-file targets kept by name because their role is structural (entry point,
# CLI summary helpers). If these are renamed the lints simply skip — no false
# negatives, but the hint comments here will need updating.
MAIN_FILE="src/main.rs"
SUMMARY_FILE="src/summary.rs"

# 1. Plain string titles (should be Span::styled)
PLAIN_TITLES=$(grep -n '\.title("' $UI_FILES 2>/dev/null || true)
if [ -n "$PLAIN_TITLES" ]; then
    echo "ERROR: Plain string titles found (use Span::styled with theme color):"
    echo "$PLAIN_TITLES"
    ERRORS=$((ERRORS + 1))
fi

PLAIN_FORMAT_TITLES=$(grep -n '\.title(format!' $UI_FILES 2>/dev/null || true)
if [ -n "$PLAIN_FORMAT_TITLES" ]; then
    echo "ERROR: format! titles without Span::styled found:"
    echo "$PLAIN_FORMAT_TITLES"
    ERRORS=$((ERRORS + 1))
fi

# 1b. .title(variable) without Span::styled (multi-line aware)
PLAIN_VAR_TITLES=$(python3 -c "
for fname in '$UI_FILES'.split():
    with open(fname) as f:
        lines = f.read().split('\n')
    for i, line in enumerate(lines):
        stripped = line.strip()
        if stripped.startswith('.title(') and 'Span::styled' not in stripped and 'Line::from' not in stripped:
            content = stripped[7:].rstrip(')')  .rstrip(',')
            # Variables whose name ends in _line are by convention pre-built
            # ratatui Lines (carrying their own per-span styling), so a bare
            # title(x_line) call is the right call. Skip them.
            if content and not content.startswith('\"') and not content.startswith('Span') and not content.startswith('Line') and not content.endswith('_line'):
                print(f'  {fname}:L{i+1}: {stripped}')
" 2>/dev/null || true)
if [ -n "$PLAIN_VAR_TITLES" ]; then
    echo "ERROR: .title(variable) without Span::styled found:"
    echo "$PLAIN_VAR_TITLES"
    ERRORS=$((ERRORS + 1))
fi

# 2. borders(Borders::ALL) without border_style (multi-line aware)
MISSING_BORDER=$(python3 -c "
for fname in '$UI_FILES'.split():
    with open(fname) as f:
        lines = f.read().split('\n')
    for i, line in enumerate(lines):
        if 'borders(Borders::ALL)' in line:
            window = '\n'.join(lines[i:i+8])
            if 'border_style' not in window:
                print(f'  {fname}:L{i+1}: {line.strip()}')
" 2>/dev/null || true)
if [ -n "$MISSING_BORDER" ]; then
    echo "ERROR: borders(Borders::ALL) without border_style:"
    echo "$MISSING_BORDER"
    ERRORS=$((ERRORS + 1))
fi

# 3. Wrong date format (%y/ with 2-digit year, %m/%d instead of %m-%d, %b locale-dependent month).
# Scan ALL_RUST_FILES so the rule is consistent regardless of where date strings
# live (tests, helpers, MCP tool args). Previously only UI + main.rs were covered.
WRONG_DATES=$(grep -n '%y/' $ALL_RUST_FILES 2>/dev/null || true)
WRONG_SLASH=$(grep -n '%m/%d' $ALL_RUST_FILES 2>/dev/null || true)
WRONG_LOCALE=$(grep -n '%b' $ALL_RUST_FILES 2>/dev/null || true)
if [ -n "$WRONG_DATES" ] || [ -n "$WRONG_SLASH" ] || [ -n "$WRONG_LOCALE" ]; then
    echo "ERROR: Non-standard date format found (use %Y-%m-%d or %m-%d):"
    [ -n "$WRONG_DATES" ] && echo "$WRONG_DATES"
    [ -n "$WRONG_SLASH" ] && echo "$WRONG_SLASH"
    [ -n "$WRONG_LOCALE" ] && echo "$WRONG_LOCALE"
    ERRORS=$((ERRORS + 1))
fi

# 4. Cost with .4+ precision (never use .4 or higher)
WRONG_COST=$(grep -nE '\{[a-z_]*:\.[4-9]\}' $UI_FILES 2>/dev/null || true)
if [ -n "$WRONG_COST" ]; then
    echo "ERROR: Cost with .4+ precision found (use .0 or .2):"
    echo "$WRONG_COST"
    ERRORS=$((ERRORS + 1))
fi

# 4b. Direct cost formatting without format_cost() (use format_cost to prevent $-0.00).
# Scan ALL_RUST_FILES so non-UI code (e.g. summary text, MCP responses) also
# routes through `format_cost`. Skip false positives where the value is already
# clamped via `.max(0.0)` or wrapped in `format_cost`.
DIRECT_COST=$(grep -nE 'format!\(".*\$\{.*cost.*:\.[0-9]\}' $ALL_RUST_FILES 2>/dev/null | grep -v 'format_cost\|max(0.0)\|\.max(' || true)
if [ -n "$DIRECT_COST" ]; then
    echo "ERROR: Direct cost formatting (use format_cost() to prevent \$-0.00):"
    echo "$DIRECT_COST"
    ERRORS=$((ERRORS + 1))
fi

# 5. Raw u16 subtraction on area dimensions for popup sizing (use saturating_sub).
# Catches `area.height - N` / `area.width - N` anywhere in UI code, not just popup-sized
# variables. The previous regex only matched `(popup|inner)…area.height - N` and the
# `.min(...)` form, missing plain `area.height - 4` followed by `;` or `,`. Excludes
# saturating_sub itself so the fixed form doesn't trigger.
RAW_SUB=$(python3 -c "
import re
pat = re.compile(r'\barea\.(height|width)\s*-\s*\d')
for fname in '$UI_FILES'.split():
    with open(fname) as f:
        for i, line in enumerate(f.readlines()):
            stripped = line.strip()
            if stripped.startswith('//'):
                continue
            if pat.search(line) and 'saturating_sub' not in line:
                print(f'  {fname}:L{i+1}: {stripped}')
" 2>/dev/null || true)
if [ -n "$RAW_SUB" ]; then
    echo "ERROR: Raw subtraction on area dimensions (use saturating_sub):"
    echo "$RAW_SUB"
    ERRORS=$((ERRORS + 1))
fi

# 6. sessions.iter().enumerate() without subagent filter (main.rs)
UNFILTERED=$(python3 -c "
import re
with open('$MAIN_FILE') as f:
    content = f.read()
    lines = content.split('\n')
issues = []
for i, line in enumerate(lines):
    if '.sessions' in line and 'enumerate' in line and 'iter' in line:
        window = '\n'.join(lines[max(0,i-2):i+3])
        if 'is_subagent' not in window and 'filter' not in window:
            issues.append(f'  L{i+1}: {line.strip()}')
for issue in issues:
    print(issue)
" 2>/dev/null || true)
if [ -n "$UNFILTERED" ]; then
    echo "WARNING: sessions.iter().enumerate() without subagent filter:"
    echo "$UNFILTERED"
    echo "  (Verify this is intentional - selected_session uses filtered indices)"
fi

# 7. Scroll indicator using ↑↓ instead of ▲▼ (in scroll state indicators, not keybind help)
WRONG_SCROLL=$(python3 -c "
for fname in '$UI_FILES'.split():
    with open(fname) as f:
        lines = f.read().split('\n')
    for i, line in enumerate(lines):
        if '↑↓' in line and 'scroll_indicator' in line:
            print(f'  {fname}:L{i+1}: {line.strip()}')
" 2>/dev/null || true)
if [ -n "$WRONG_SCROLL" ]; then
    echo "ERROR: Scroll indicator using ↑↓ instead of ▲▼:"
    echo "$WRONG_SCROLL"
    ERRORS=$((ERRORS + 1))
fi

# 8. Direct project name shortening instead of shorten_project()
DIRECT_PROJECT=$(grep -n 'Path::new.*project_name.*file_name()' $UI_FILES "$SUMMARY_FILE" 2>/dev/null | grep -v 'shorten_project' || true)
if [ -n "$DIRECT_PROJECT" ]; then
    echo "ERROR: Direct project name shortening (use shorten_project()):"
    echo "$DIRECT_PROJECT"
    ERRORS=$((ERRORS + 1))
fi

# 9. Direct ui::load_conversation in main.rs (use spawn_load_conversation)
DIRECT_LOAD=$(python3 -c "
with open('$MAIN_FILE') as f:
    lines = f.readlines()
helper_lines = set()
for i, line in enumerate(lines):
    if 'fn spawn_load_conversation' in line:
        for j in range(max(0,i-1), min(len(lines), i+10)):
            helper_lines.add(j)
for i, line in enumerate(lines):
    if 'ui::load_conversation' in line and i not in helper_lines:
        print(f'  L{i+1}: {line.strip()}')
" 2>/dev/null || true)
if [ -n "$DIRECT_LOAD" ]; then
    echo "ERROR: Direct ui::load_conversation call (use spawn_load_conversation()):"
    echo "$DIRECT_LOAD"
    ERRORS=$((ERRORS + 1))
fi

# 10. Inline weekday occurrence count (use aggregator::buckets::aggregate_weekday_avg /
# weekday_occurrence_count helpers). Scans ALL Rust files since the helper moved
# from `src/ui/` to `src/aggregator/buckets.rs`; keeping the lint UI-only made
# it silently no-op against the helper's actual host file.
INLINE_WEEKDAY=$(python3 -c "
for fname in '$ALL_RUST_FILES'.split():
    with open(fname) as f:
        lines = f.readlines()
    helper_lines = set()
    for i, line in enumerate(lines):
        if 'fn weekday_occurrence_count' in line or 'fn aggregate_weekday_avg' in line:
            for j in range(max(0,i-1), min(len(lines), i+25)):
                helper_lines.add(j)
    for i, line in enumerate(lines):
        if i not in helper_lines and ('calendar_days' in line and ('/ 7' in line or '% 7' in line)):
            print(f'  {fname}:L{i+1}: {line.strip()}')
" 2>/dev/null || true)
if [ -n "$INLINE_WEEKDAY" ]; then
    echo "ERROR: Inline weekday count calculation (use aggregator::buckets helpers):"
    echo "$INLINE_WEEKDAY"
    ERRORS=$((ERRORS + 1))
fi

# 11. Direct ratatui Scrollbar widget (use draw_scrollbar())
DIRECT_SCROLLBAR=$(grep -n 'Scrollbar::new\|ScrollbarState::new' $UI_FILES 2>/dev/null || true)
if [ -n "$DIRECT_SCROLLBAR" ]; then
    echo "ERROR: Direct ratatui Scrollbar widget (use draw_scrollbar()):"
    echo "$DIRECT_SCROLLBAR"
    ERRORS=$((ERRORS + 1))
fi

# 12. Legacy conversation_* fields on AppState (use panes instead)
LEGACY_CONV=$(grep -n 'state\.conversation_messages\|state\.conversation_scroll\|state\.conversation_rendered\|state\.conversation_file_path\|state\.conversation_loading\|state\.conversation_load_task\|state\.conv_search_mode\|state\.conv_search_query\|state\.conv_search_matches\|state\.conv_search_current\|state\.conv_search_saved_scroll\|state\.selected_conversation_message\|state\.conversation_message_lines\|state\.conversation_last_modified\|state\.conversation_reload_check\|state\.last_conversation_width' "$MAIN_FILE" $UI_FILES 2>/dev/null || true)
if [ -n "$LEGACY_CONV" ]; then
    echo "ERROR: Legacy conversation_* fields found (use panes instead):"
    echo "$LEGACY_CONV"
    ERRORS=$((ERRORS + 1))
fi

# 13. Inline pane initialization (use ConversationPane::load_from or open_conversation_in_pane)
INLINE_PANE=$(python3 -c "
with open('$MAIN_FILE') as f:
    lines = f.readlines()
helper_lines = set()
for i, line in enumerate(lines):
    if 'fn load_from' in line or 'fn open_conversation_in_pane' in line:
        for j in range(max(0,i-1), min(len(lines), i+25)):
            helper_lines.add(j)
for i, line in enumerate(lines):
    if 'load_task' in line and 'spawn_load_conversation' in line and i not in helper_lines:
        window = ''.join(lines[max(0,i-15):i+2])
        if 'needs_reload' not in window and 'reload_check' not in window:
            print(f'  L{i+1}: {line.strip()}')
" 2>/dev/null || true)
if [ -n "$INLINE_PANE" ]; then
    echo "ERROR: Inline pane initialization (use ConversationPane::load_from()):"
    echo "$INLINE_PANE"
    ERRORS=$((ERRORS + 1))
fi

# 14. Direct sessions[idx] index access (use .iter().filter().nth() or .get())
# Allowlist: when the surrounding lines build a `session_indices` list via
# `.filter(!is_subagent)`, the resulting `idx` is already a real session position,
# so `group.sessions[idx]` is safe and intentional. Look for that context in a
# 10-line window above the suspected access.
DIRECT_SESSION_IDX=$(python3 -c "
import re
for fname in '$UI_FILES'.split() + ['$MAIN_FILE']:
    with open(fname) as f:
        lines = f.readlines()
    for i, line in enumerate(lines):
        if re.search(r'\.sessions\[[a-z_]+\]', line.strip()):
            window = ''.join(lines[max(0, i-10):i+1])
            if 'session_indices' in window or 'actual_idx' in window:
                continue
            print(f'  {fname}:L{i+1}: {line.strip()}')
" 2>/dev/null || true)
if [ -n "$DIRECT_SESSION_IDX" ]; then
    echo "WARNING: Direct sessions[idx] access (prefer .iter().filter().nth() or .get()):"
    echo "$DIRECT_SESSION_IDX"
fi

# 15. String::remove/insert with cursor (needs char_indices for UTF-8 safety)
UNSAFE_STRING_OP=$(grep -n '\.remove(.*cursor\|\.insert(.*cursor' "$MAIN_FILE" 2>/dev/null | grep -v 'char_indices\|byte_pos' || true)
if [ -n "$UNSAFE_STRING_OP" ]; then
    echo "ERROR: String::remove/insert with cursor without char_indices (UTF-8 unsafe):"
    echo "$UNSAFE_STRING_OP"
    ERRORS=$((ERRORS + 1))
fi

# 16. Real-world identifiers inside `mcp__…`, `skill:…`, `agent:…` literals
# Enforced via SHAPE-based detection — never list real names here, the list itself leaks.
# Approved placeholder shapes (a token-plus-digit or a short single-letter form):
#   mcp__server<digits>__<word>
#   mcp__plugin_<placeholder>_<placeholder>__<word>
#   skill:s<digits>   skill:my-<lowercase-word>   skill:my   skill:my-skill
#   agent:t<digits>   agent:type-<a-z>   agent:ns:type-<a-z>
# Where <placeholder> is `serverN` (any digits or single uppercase) or `orgA`/`serverB` style.
# Anything that does NOT match these shapes is flagged.
DOMAIN_HITS=$(python3 - <<'PYEOF' 2>/dev/null || true
import re, sys, pathlib

# Discover Rust sources under src/ dynamically — picks up any new files added
# in refactors without needing this list updated.
files = sorted(str(p) for p in pathlib.Path("src").rglob("*.rs"))

# Allowed identifier shapes (kept deliberately simple/generic).
allowed_id = re.compile(
    r"^("
    r"server\d+|"            # server1, server2, ...
    r"server[A-Z]|"          # serverA, serverB, ...
    r"org[A-Z]|"             # orgA, orgB, ...
    r"plugin_[a-zA-Z0-9]+_[a-zA-Z0-9]+|"  # plugin_orgA_serverB
    r"my-?[a-z-]*|"          # my, my-skill, my-tool
    r"type-[a-z]|"           # type-a, type-b
    r"ns:type-[a-z]|"        # ns:type-a
    r"s\d+|t\d+|"            # s1, s2, t1, t2
    r"action|"               # placeholder method name used in tests
    r"unknown"               # explicit fallback marker
    r")$"
)

# Patterns that locate identifier positions in source/test strings.
# We only check inside Rust string literals (between double quotes).
mcp_pat   = re.compile(r'mcp__([a-zA-Z0-9_-]+)__([a-zA-Z0-9_-]+)')
skill_pat = re.compile(r'skill:([a-zA-Z0-9_:-]+)')
agent_pat = re.compile(r'agent:([a-zA-Z0-9_:-]+)')

def is_allowed(token: str) -> bool:
    return bool(allowed_id.match(token))

violations = []
for path in files:
    if not pathlib.Path(path).exists():
        continue
    for lineno, line in enumerate(pathlib.Path(path).read_text().splitlines(), 1):
        # Only inspect content that appears inside string literals.
        if '"' not in line:
            continue
        for m in mcp_pat.finditer(line):
            server, _action = m.group(1), m.group(2)
            if server.startswith("plugin_"):
                # plugin_X_Y form: validate both X and Y
                parts = server.split("_", 2)
                if len(parts) >= 3:
                    if not (is_allowed(parts[1]) and is_allowed(parts[2])):
                        violations.append(f"  {path}:L{lineno}: mcp__{server}__…")
                else:
                    violations.append(f"  {path}:L{lineno}: mcp__{server}__…")
            else:
                if not is_allowed(server):
                    violations.append(f"  {path}:L{lineno}: mcp__{server}__…")
        for m in skill_pat.finditer(line):
            tok = m.group(1)
            # Allow nested form like skill:my-skill or skill:s1
            head = tok.split(":")[0]
            if not is_allowed(tok) and not is_allowed(head):
                violations.append(f"  {path}:L{lineno}: skill:{tok}")
        for m in agent_pat.finditer(line):
            tok = m.group(1)
            if not is_allowed(tok):
                # Try first segment for ns:type-a
                head = tok.split(":")[0]
                if not is_allowed(head) and not is_allowed(tok):
                    violations.append(f"  {path}:L{lineno}: agent:{tok}")

if violations:
    print("\n".join(violations))
PYEOF
)
if [ -n "$DOMAIN_HITS" ]; then
    echo "ERROR: Real-world identifiers found inside mcp__/skill:/agent: literals."
    echo "       Use generic placeholders only (e.g. server1, orgA, my-skill, type-a)."
    echo "$DOMAIN_HITS"
    ERRORS=$((ERRORS + 1))
fi

# 17. Local denylist: substring search for terms in `.lint-forbidden-terms`.
# This file is git-ignored (never committed) and lists real org/product/MCP/skill
# identifiers the developer wants to keep out of public source. The example file
# `.lint-forbidden-terms.example` documents the format and is committed.
DENYLIST_FILE=".lint-forbidden-terms"
if [ -f "$DENYLIST_FILE" ]; then
    # Strip comments + blank lines + leading/trailing whitespace, then collect terms.
    DENY_TERMS=$(grep -vE '^\s*(#|$)' "$DENYLIST_FILE" | sed -E 's/^[[:space:]]+//;s/[[:space:]]+$//' | grep -v '^$' || true)
    if [ -n "$DENY_TERMS" ]; then
        # Build a single ERE alternation. Escape regex meta-characters per term, then
        # wrap each with non-alphanumeric (or string-edge) boundaries so substrings of
        # innocent words don't fire (e.g., `esa` should match `mcp__esa__action` but
        # not `resampled`). Treats `-`, `_`, `:` etc. as word separators.
        ESCAPED=$(printf '%s\n' "$DENY_TERMS" | sed -E 's/[][\\.|^$*+?(){}/-]/\\&/g')
        WRAPPED=$(printf '%s\n' "$ESCAPED" | sed -E 's/^(.*)$/(^|[^A-Za-z0-9])\1([^A-Za-z0-9]|$)/')
        DENY_PATTERN=$(printf '%s\n' "$WRAPPED" | tr '\n' '|' | sed 's/|$//')
        if [ -n "$DENY_PATTERN" ]; then
            # Scan all Rust sources + top-level docs. ALL_RUST_FILES / DOC_FILES are
            # discovered above so newly added files are automatically covered.
            DENY_HITS=$(grep -nE "$DENY_PATTERN" \
                $ALL_RUST_FILES $DOC_FILES 2>/dev/null \
                | grep -v '^Binary' || true)
            if [ -n "$DENY_HITS" ]; then
                echo "ERROR: Forbidden term(s) from .lint-forbidden-terms found in committed files."
                echo "       Replace with a generic placeholder. The denylist file itself is"
                echo "       git-ignored — never commit it."
                echo "$DENY_HITS"
                ERRORS=$((ERRORS + 1))
            fi
        fi
    fi
fi

# 18. Popup inner height over-subtraction.
# ratatui `Block.title_bottom` is rendered onto the bottom border line itself, so the
# inner content area of a bordered popup is exactly `popup_height - 2` (top + bottom
# border). Subtracting 3 or 4 leaves a permanent dead band of empty rows under the
# content and makes cursor-tracking math drift (the rendered viewport disagrees with
# what `scroll + visible_height` claims). Allow `- 2` only; flag anything larger.
POPUP_OVERSUB=$(grep -nE '(popup|inner)[A-Za-z_]*_?height\s*\.\s*saturating_sub\s*\(\s*[3-9][0-9]*\s*\)' $UI_FILES 2>/dev/null \
    | grep -v '^[^:]*:[^:]*:\s*//' || true)
if [ -n "$POPUP_OVERSUB" ]; then
    echo "ERROR: popup_height.saturating_sub(N) with N > 2 (use -2; title_bottom shares the border line):"
    echo "$POPUP_OVERSUB"
    ERRORS=$((ERRORS + 1))
fi

# 19. Stale-MCP threshold via is_underutilized.
# `McpServerStatus::is_underutilized(now, 30)` is the source-of-truth for the
# "stale" predicate (configured AND idle ≥30d OR never used). Re-implementing
# the threshold inline (`num_days() >= 30` / `> 30`) caused an off-by-one drift
# between the Dashboard preview "N stale" count and the popup body's per-row
# `⚠` marker. Allowlist: the function definition itself + retention-warning
# threshold in `file_discovery.rs` (different semantics).
INLINE_STALE=$(python3 -c "
import re
files = '$ALL_RUST_FILES'.split()
pat = re.compile(r'num_days\(\)\s*[><]=?\s*30\b')
for fname in files:
    if fname.endswith('mcp_config.rs') or fname.endswith('file_discovery.rs'):
        continue  # source-of-truth + retention warning
    with open(fname) as f:
        for i, line in enumerate(f):
            stripped = line.strip()
            if stripped.startswith('//'):
                continue
            if pat.search(line):
                print(f'  {fname}:L{i+1}: {stripped}')
" 2>/dev/null || true)
if [ -n "$INLINE_STALE" ]; then
    echo "ERROR: Inline stale threshold (use McpServerStatus::is_underutilized(now, 30)):"
    echo "$INLINE_STALE"
    ERRORS=$((ERRORS + 1))
fi

# 20. stderr writes (eprintln!, writeln!(io::stderr())) outside allowed paths.
# The TUI takes over stdout AND stderr inside ratatui's alternate screen; any
# stray write to stderr corrupts the rendering. Allowed: `cli.rs` (--daily mode
# never enters TUI), `main.rs` panic-hook restoration (already disables raw
# mode), and `mcp.rs` (stdio MCP server doesn't render TUI either).
STDERR_HITS=$(grep -nE 'eprintln!|writeln!\(\s*io::stderr|\.stderr\(\)' \
    $ALL_RUST_FILES 2>/dev/null \
    | grep -vE '^src/cli\.rs:|^src/main\.rs:|^src/mcp\.rs:' \
    | grep -v '^[^:]*:[^:]*:\s*//' || true)
if [ -n "$STDERR_HITS" ]; then
    echo "ERROR: stderr write outside cli.rs / main.rs / mcp.rs (corrupts TUI):"
    echo "$STDERR_HITS"
    ERRORS=$((ERRORS + 1))
fi

# 21. command: literal placeholder shape (mirrors #16 for the command: prefix).
# Same rationale as #16 — real command names leak product / org info. Allowed:
# `command:my-cmd` / `command:plugin:my-cmd` / `command:c<digits>`. Anything
# else (e.g. `command:setup-real-product`) gets flagged.
COMMAND_HITS=$(python3 - <<'PYEOF' 2>/dev/null || true
import re, pathlib
allowed = re.compile(
    r"^("
    r"my-?[a-z-]*|"
    r"c\d+|"
    r"plugin:my-?[a-z-]*|"
    r"plugin:c\d+|"
    r"my-cmd|"
    r"unknown"
    r")$"
)
cmd_pat = re.compile(r'command:([a-zA-Z0-9_:-]+)')
violations = []
for path in sorted(str(p) for p in pathlib.Path("src").rglob("*.rs")):
    for lineno, line in enumerate(pathlib.Path(path).read_text().splitlines(), 1):
        if '"' not in line:
            continue
        for m in cmd_pat.finditer(line):
            tok = m.group(1)
            if not allowed.match(tok):
                violations.append(f"  {path}:L{lineno}: command:{tok}")
if violations:
    print("\n".join(violations))
PYEOF
)
if [ -n "$COMMAND_HITS" ]; then
    echo "ERROR: Real-world identifier inside command: literal."
    echo "       Use generic placeholders only (e.g. my-cmd, plugin:my-cmd, c1)."
    echo "$COMMAND_HITS"
    ERRORS=$((ERRORS + 1))
fi

# 23. Captured numeric values in `//` comments (per CLAUDE.md "Comments" rule).
# Catches percentages (`94%`, `100.0%`), magnitude suffixes (`76.2K`, `4.46M`),
# and dollar amounts in narrative comments — they look like illustrations but
# are stale snapshots of user data the moment they're written.
# Exemptions:
#   * `mod tests` blocks — test docstrings document fixture arithmetic that
#     IS the contract and won't drift independently of the assertions.
#   * `src/aggregator/pricing.rs` — rate constants are self-documented inline.
#   * `silent-$0` / `silent $0` — term-of-art for the "model lacks pricing"
#     bug class (like "silent NaN").
NUMERIC_COMMENTS=$(python3 -c "
import re
pct = re.compile(r'(?<![A-Za-z0-9_.])\d+(?:\.\d+)?%')
mag = re.compile(r'(?<![A-Za-z0-9_.])\d+\.\d+[KMGB]\b')
dol = re.compile(r'\\\$\d+(?:\.\d+)?')
silent_zero = re.compile(r'silent[ -]?\\\$0')
for fname in '$ALL_RUST_FILES'.split():
    if fname.endswith('pricing.rs'):
        continue
    with open(fname) as f:
        lines = f.read().split('\n')
    in_tests = False
    test_depth = 0
    for i, line in enumerate(lines):
        stripped = line.strip()
        # Track 'mod tests {' nesting by brace count once entered.
        if not in_tests and re.match(r'mod\s+tests\s*\{', stripped):
            in_tests = True
            test_depth = line.count('{') - line.count('}')
            continue
        if in_tests:
            test_depth += line.count('{') - line.count('}')
            if test_depth <= 0:
                in_tests = False
            continue
        # Find comment start; skip lines without // (avoid false matches in code).
        idx = line.find('//')
        if idx < 0:
            continue
        comment = line[idx:]
        # Strip 'silent-\$0' style term so the dollar regex doesn't trip on it.
        scrubbed = silent_zero.sub('', comment)
        hits = []
        if pct.search(scrubbed):
            hits.append('%')
        if mag.search(scrubbed):
            hits.append('K/M/G/B')
        if dol.search(scrubbed):
            hits.append('\$')
        if hits:
            print(f'  {fname}:L{i+1} [{\",\".join(hits)}]: {stripped}')
" 2>/dev/null || true)
if [ -n "$NUMERIC_COMMENTS" ]; then
    echo "ERROR: Captured numeric values in comments (CLAUDE.md \"Comments\"):"
    echo "$NUMERIC_COMMENTS"
    ERRORS=$((ERRORS + 1))
fi

# 22. Raw subtraction on inner / popup dimensions.
# Extends #5 beyond bare `area.*`. Variables named `inner.height`,
# `popup_area.height`, `popup_height`, etc. carry the same u16-underflow risk
# when subtracted with a literal — use `saturating_sub`.
RAW_SUB_INNER=$(python3 -c "
import re
pat = re.compile(r'\b(inner|popup_?area|popup)[a-zA-Z_]*\.(height|width)\s*-\s*\d')
for fname in '$UI_FILES'.split():
    with open(fname) as f:
        for i, line in enumerate(f.readlines()):
            stripped = line.strip()
            if stripped.startswith('//'):
                continue
            if pat.search(line) and 'saturating_sub' not in line:
                print(f'  {fname}:L{i+1}: {stripped}')
" 2>/dev/null || true)
if [ -n "$RAW_SUB_INNER" ]; then
    echo "ERROR: Raw subtraction on inner/popup dimensions (use saturating_sub):"
    echo "$RAW_SUB_INNER"
    ERRORS=$((ERRORS + 1))
fi

# 24. Direct `shorten_project()` in render paths instead of `state.project_label()`.
# `shorten_project` returns just the basename — when two projects share a basename
# (`/work/dev/foo` vs `/other/area/foo`), it loses the disambiguating context.
# Render paths must go through `state.project_label`, which prepends the parent
# dir on collision. Allowed call sites:
#   - the `shorten_project` definition itself (mod.rs:172)
#   - `unwrap_or_else(|| shorten_project(...))` fallback when `project_labels` is missing
#   - `summary.rs` (AI prompt generation, single session, no list to disambiguate against)
SHORTEN_DIRECT=$(python3 -c "
import re
pat = re.compile(r'\bshorten_project\s*\(')
for fname in 'src/ui/dashboard.rs src/ui/insights.rs src/ui/mod.rs'.split():
    try:
        with open(fname) as f:
            content = f.read()
    except FileNotFoundError:
        continue
    for i, line in enumerate(content.splitlines()):
        stripped = line.strip()
        if stripped.startswith('//') or stripped.startswith('///'):
            continue
        if not pat.search(line):
            continue
        # Allow the definition itself.
        if 'fn shorten_project' in line:
            continue
        # Allow the fallback inside an unwrap_or_else closure (typically '|| shorten_project(...)').
        if 'unwrap_or_else' in line or '||' in line.split('shorten_project')[0]:
            continue
        print(f'  {fname}:L{i+1}: {stripped}')
" 2>/dev/null || true)
if [ -n "$SHORTEN_DIRECT" ]; then
    echo "ERROR: Direct shorten_project() in render path (use state.project_label() so two projects with the same basename stay distinguishable):"
    echo "$SHORTEN_DIRECT"
    ERRORS=$((ERRORS + 1))
fi

# 25. Literal calendar dates in source (per "no captured values" rule).
# Production code MUST NOT call `chrono::Local::now()` / `Utc::now()` outside
# the single top-level draw / event-loop entry point; date-dependent helpers
# take `today: NaiveDate` (or equivalent) as a parameter so they are pure
# functions of their input. Tests then supply a fixed date and stay
# deterministic across runs, timezones, and calendar boundaries.
#
# Reading the clock inside tests via `chrono::Local::now()` is BANNED — it
# couples the test to wall-clock state and silently changes behaviour at
# DST / month / year edges. Use a literal date with the `// lint-ok:
# date-literal` marker; the marker doubles as documentation that the value
# is an intentional, hermetic fixture.
DATE_LITERALS=$(python3 -c "
import os
import re

# Match any ISO date substring 'YYYY-MM-DD' OR from_ymd[_opt](YEAR, ...).
# The substring form catches timestamps in test JSON like 'YYYY-MM-DDTHH:MM:SSZ'
# and bare dates in comments, not just quoted-string literals.
pat = re.compile(r'(\b20[0-9]{2}-[01][0-9]-[0-3][0-9]\b)|(from_ymd(_opt)?\s*\(\s*20[0-9]{2}\b)')

for root, dirs, files in os.walk('src'):
    for fname in files:
        if not fname.endswith('.rs'):
            continue
        path = os.path.join(root, fname)
        try:
            with open(path) as f:
                content = f.read()
        except FileNotFoundError:
            continue
        for i, line in enumerate(content.splitlines()):
            if not pat.search(line):
                continue
            # Format-spec strings (\"%Y-%m-%d\") are not literal dates.
            if '%Y-%m-%d' in line:
                continue
            # Per-line opt-out marker.
            if 'lint-ok: date-literal' in line:
                continue
            print(f'  {path}:L{i+1}: {line.strip()}')
" 2>/dev/null || true)
if [ -n "$DATE_LITERALS" ]; then
    echo "ERROR: Literal calendar dates in source (use chrono::Local::now() + offsets or add '// lint-ok: date-literal' for genuinely fixed parser fixtures):"
    echo "$DATE_LITERALS"
    ERRORS=$((ERRORS + 1))
fi

# 26. `let Ok(...) = rx.try_recv()` (background-task receivers).
# Holds an mpsc::Receiver via `let Ok(x) = ...` silently swallows the
# `Disconnected` arm — if the spawned thread panics before sending, the
# receiver stays in state forever and the task never re-spawns. Use
# `match rx.try_recv()` with all three arms explicit. Allowed inside
# `search_task` / `conversation_load_task` etc. where the task is
# legitimately one-shot AND the slot is overwritten on the next user
# action; mark the line with `// lint-ok: one-shot-task`.
TRY_RECV_LET_OK=$(grep -rn 'let Ok(.*) = .*\.try_recv()' src/main.rs src/handlers/ 2>/dev/null | \
    grep -v 'lint-ok: one-shot-task' || true)
if [ -n "$TRY_RECV_LET_OK" ]; then
    echo "ERROR: \`let Ok(...) = rx.try_recv()\` swallows \`Disconnected\` (Live polling / data reload / index build all freeze if the worker thread panics). Use \`match rx.try_recv()\` with three arms, or add \`// lint-ok: one-shot-task\` for receivers that are intentionally fire-and-forget:"
    echo "$TRY_RECV_LET_OK"
    ERRORS=$((ERRORS + 1))
fi

# 27. `DateTime::from_timestamp(0, 0).unwrap()` — old chrono API.
# Use `DateTime::<Utc>::UNIX_EPOCH` (chrono ≥ 0.4.26): infallible, no
# `unwrap`, no risk of silent breakage when chrono's API shifts.
FROM_TIMESTAMP_ZERO=$(grep -rn 'from_timestamp(0, 0)\.unwrap()' src/ 2>/dev/null || true)
if [ -n "$FROM_TIMESTAMP_ZERO" ]; then
    echo "ERROR: \`DateTime::from_timestamp(0, 0).unwrap()\` — replace with \`DateTime::<Utc>::UNIX_EPOCH\`:"
    echo "$FROM_TIMESTAMP_ZERO"
    ERRORS=$((ERRORS + 1))
fi

# 28. Shell command strings must route through `posix_shell_quote`.
# Building a shell command via `format!("cd {} && ...")` from user-derived
# data (cwd, session_id read from JSON on disk) is a clipboard injection
# vector. The single quoting helper lives in `src/handlers/keyboard.rs`.
SHELL_FORMAT=$(grep -rn 'format!("cd {' src/ 2>/dev/null | \
    grep -v 'posix_shell_quote' | grep -v 'lint-ok: shell-quote' | \
    grep -v 'src/shell.rs' || true)
if [ -n "$SHELL_FORMAT" ]; then
    echo "ERROR: shell command built via \`format!\` without \`posix_shell_quote\` — clipboard injection risk:"
    echo "$SHELL_FORMAT"
    ERRORS=$((ERRORS + 1))
fi

# 29. Atomic file writes (`tmp + rename`) must flush before rename.
# Writing the tmp via `fs::write` and renaming on success is the standard
# atomic pattern, but the data isn't durable across a power loss until
# `sync_all` is called. Enforce sync by checking that any function
# performing `fs::rename(&tmp, &path)` also references `sync_all`. The
# allowlist `lint-ok: best-effort-tmp` covers genuinely transient writes
# (e.g. lock files that get recreated).
RENAME_NO_FSYNC=$(python3 -c "
import re, os
pat_rename = re.compile(r'fs::rename\(.*tmp.*,.*path')
pat_sync = re.compile(r'sync_all')
for root, dirs, files in os.walk('src/infrastructure'):
    for fname in files:
        if not fname.endswith('.rs'):
            continue
        path = os.path.join(root, fname)
        with open(path) as f:
            content = f.read()
        if 'lint-ok: best-effort-tmp' in content:
            continue
        if pat_rename.search(content) and not pat_sync.search(content):
            print(f'  {path}: fs::rename(tmp, path) without sync_all elsewhere in file')
" 2>/dev/null || true)
if [ -n "$RENAME_NO_FSYNC" ]; then
    echo "ERROR: tmp+rename atomic write without \`sync_all\` — data not durable on power loss. Call \`f.sync_all()\` before rename (see cache.rs::save), or annotate with \`// lint-ok: best-effort-tmp\`:"
    echo "$RENAME_NO_FSYNC"
    ERRORS=$((ERRORS + 1))
fi

# 30. Single-key sort on HashMap-sourced collections (non-deterministic ties).
# Sorting a Vec by a single value (e.g. `sort_by(|a, b| b.1.cmp(a.1))` or
# `sort_by_key(|t| Reverse(t.1))`) is non-deterministic for tied values
# when the source is a HashMap — HashMap iteration order is randomized
# per instance, so two items with the same count shuffle between frames
# in the UI. Always add a tiebreaker (typically alphabetical on name)
# via `.then_with(|| a.0.cmp(b.0))` so the final order is stable.
# Allowlist `// lint-ok: deterministic-source` for cases sourced from a
# Vec / BTreeMap (already ordered) or where the values are unique.
NONDET_SORT=$(grep -rn 'sort_by(|a, b| b\.1\.cmp(a\.1));' src/ 2>/dev/null | \
    grep -v 'then_with' | grep -v 'lint-ok: deterministic-source' || true)
NONDET_SORT_KEY=$(grep -rn 'sort_by_key(|.| std::cmp::Reverse(.\.1))' src/ 2>/dev/null | \
    grep -v 'then_with' | grep -v 'lint-ok: deterministic-source' || true)
if [ -n "$NONDET_SORT$NONDET_SORT_KEY" ]; then
    echo "ERROR: single-key sort without tiebreaker. If the source is a HashMap, tied values shuffle between frames. Add \`.then_with(|| a.0.cmp(b.0))\` for stable ordering, or annotate with \`// lint-ok: deterministic-source\`:"
    [ -n "$NONDET_SORT" ] && echo "$NONDET_SORT"
    [ -n "$NONDET_SORT_KEY" ] && echo "$NONDET_SORT_KEY"
    ERRORS=$((ERRORS + 1))
fi

# 31. Popup content vecs must not start with a blank `Line::from("")`.
# The popup title is rendered onto the top border line itself, so any
# leading empty Line shifts every section down one row and visually
# desyncs the popup from its siblings (Dashboard / Insights / Tools
# detail popups all begin with substantive content). This has bitten
# the per-project popup, Live preview, Help popup, and Insights detail
# at different times — same diagnosis every time: someone added an
# initial blank "for breathing room" without seeing it doubled up.
#
# Pattern: `vec![Line::from("")` followed by `,` or `]` (single- or
# multi-element vec where blank is the first item).
# Exempt: `vec![Line::from(""); N]` (deliberate N-row padding — the
# `;` distinguishes the repeat form).
LEADING_BLANK_LINE=$(python3 -c "
import os, re
# Match: vec![Line::from(\"\")<sep>  where <sep> is comma or close-bracket
# (not semicolon — that is the repeat form vec![Line::from(\"\"); N]).
pat = re.compile(r'vec!\[Line::from\(\"\"\)\s*[,\]]')
hits = []
for root, _, files in os.walk('src/ui'):
    for fname in files:
        if not fname.endswith('.rs'):
            continue
        path = os.path.join(root, fname)
        with open(path) as f:
            for i, line in enumerate(f, 1):
                if 'lint-ok: leading-blank' in line:
                    continue
                if pat.search(line):
                    hits.append(f'  {path}:L{i}: {line.rstrip()}')
print('\n'.join(hits))
" 2>/dev/null || true)
if [ -n "$LEADING_BLANK_LINE" ]; then
    echo "ERROR: popup content starts with \`Line::from(\"\")\`. The title sits on the top border; a leading blank desyncs this popup from its siblings. Drop the empty Line (use \`Vec::new()\` and push content), or annotate with \`// lint-ok: leading-blank\`:"
    echo "$LEADING_BLANK_LINE"
    ERRORS=$((ERRORS + 1))
fi

# 32. Cross-surface rate-unit consistency: `/day` (not `/d`) in format strings.
# Dashboard and Insights compute per-day rates; the displayed suffix has to
# agree across surfaces or the same metric reads differently between tabs.
# Pattern: `/d` immediately followed by a string-terminator or whitespace
# inside a string literal (catches `"avg ${X}/d"`, `"X/d "`, `"X/d)"`).
# Annotate legitimate uses with `// lint-ok: rate-suffix-d`.
RATE_SUFFIX_D=$(python3 -c "
import os, re
# /d preceded by a value-like character (digit, } from interpolation,
# K/M/B unit suffix) and followed by a string terminator. Filters out
# paths like '~/projects/d' and key abbrevs like 'u/d Page scroll'.
pat = re.compile(r'[\\d\\}KMBkT]/d[\"\\s\\),\\]]')
hits = []
for root, _, files in os.walk('src'):
    for fname in files:
        if not fname.endswith('.rs'):
            continue
        path = os.path.join(root, fname)
        with open(path) as f:
            for i, line in enumerate(f, 1):
                if 'lint-ok: rate-suffix-d' in line:
                    continue
                stripped = line.lstrip()
                if stripped.startswith('//') or stripped.startswith('///'):
                    continue
                if pat.search(line):
                    hits.append(f'  {path}:L{i}: {line.rstrip()}')
print('\n'.join(hits))
" 2>/dev/null || true)
if [ -n "$RATE_SUFFIX_D" ]; then
    echo "ERROR: per-day rate suffix \`/d\` (use \`/day\` for cross-surface consistency):"
    echo "$RATE_SUFFIX_D"
    ERRORS=$((ERRORS + 1))
fi

# 33. Session unit: `/ses` (3-char) — `/sess` (4-char) breaks alignment with
# the Insights metrics card's existing column width budget.
SESS_SUFFIX=$(python3 -c "
import os, re
pat = re.compile(r'/sess\\b')
hits = []
for root, _, files in os.walk('src/ui'):
    for fname in files:
        if not fname.endswith('.rs'):
            continue
        path = os.path.join(root, fname)
        with open(path) as f:
            for i, line in enumerate(f, 1):
                stripped = line.lstrip()
                if stripped.startswith('//') or stripped.startswith('///'):
                    continue
                if pat.search(line):
                    hits.append(f'  {path}:L{i}: {line.rstrip()}')
print('\n'.join(hits))
" 2>/dev/null || true)
if [ -n "$SESS_SUFFIX" ]; then
    echo "ERROR: session-rate suffix \`/sess\` (use \`/ses\`):"
    echo "$SESS_SUFFIX"
    ERRORS=$((ERRORS + 1))
fi

# 34. Compact date with weekday needs a separator between date and \`(%a)\`.
# Daily header uses \`%Y-%m-%d (%a)\` (space + paren); Dashboard \`%m-%d(%a)\`
# (no space) reads as a typo and breaks visual rhythm. Use either form with
# the space — short (\`%m-%d (%a)\`) or full (\`%Y-%m-%d (%a)\`).
WEEKDAY_NO_SPACE=$(python3 -c "
import os, re
pat = re.compile(r'%[Yymd][^\"]*%[md]\(%a\)')
hits = []
for root, _, files in os.walk('src'):
    for fname in files:
        if not fname.endswith('.rs'):
            continue
        path = os.path.join(root, fname)
        with open(path) as f:
            for i, line in enumerate(f, 1):
                stripped = line.lstrip()
                if stripped.startswith('//') or stripped.startswith('///'):
                    continue
                if pat.search(line):
                    hits.append(f'  {path}:L{i}: {line.rstrip()}')
print('\n'.join(hits))
" 2>/dev/null || true)
if [ -n "$WEEKDAY_NO_SPACE" ]; then
    echo "ERROR: date+weekday with no separator (use \"... (%a)\" with a space):"
    echo "$WEEKDAY_NO_SPACE"
    ERRORS=$((ERRORS + 1))
fi

# 35. Short date format: slash form (\`%m/%d\`) drifts from the dash form
# used everywhere else (\`%m-%d\`). Keep one form so date columns line up
# the same way on every surface.
SLASH_DATE=$(python3 -c "
import os, re
pat = re.compile(r'%-?m/%-?d')
hits = []
for root, _, files in os.walk('src'):
    for fname in files:
        if not fname.endswith('.rs'):
            continue
        path = os.path.join(root, fname)
        with open(path) as f:
            for i, line in enumerate(f, 1):
                stripped = line.lstrip()
                if stripped.startswith('//') or stripped.startswith('///'):
                    continue
                if pat.search(line):
                    hits.append(f'  {path}:L{i}: {line.rstrip()}')
print('\n'.join(hits))
" 2>/dev/null || true)
if [ -n "$SLASH_DATE" ]; then
    echo "ERROR: short date uses slash form (use \"%m-%d\" / \"%Y-%m-%d\"):"
    echo "$SLASH_DATE"
    ERRORS=$((ERRORS + 1))
fi

# 36. Cost precision: \`\${:.1}\` / \`\${:.3+}\` violates the Cost Precision
# rule (only \`:.0\` for compact, \`:.2\` for detailed). 1-decimal cost is
# visually neither — read confusion with timestamps in dense rows.
COST_PRECISION=$(python3 -c "
import os, re
pat = re.compile(r'\\\$\{:\.([13456789])\}')
hits = []
for root, _, files in os.walk('src/ui'):
    for fname in files:
        if not fname.endswith('.rs'):
            continue
        path = os.path.join(root, fname)
        with open(path) as f:
            for i, line in enumerate(f, 1):
                stripped = line.lstrip()
                if stripped.startswith('//') or stripped.startswith('///'):
                    continue
                if pat.search(line):
                    hits.append(f'  {path}:L{i}: {line.rstrip()}')
print('\n'.join(hits))
" 2>/dev/null || true)
if [ -n "$COST_PRECISION" ]; then
    echo "ERROR: cost precision other than :.0 or :.2 (per Cost Precision rule):"
    echo "$COST_PRECISION"
    ERRORS=$((ERRORS + 1))
fi

# 37. Percentage formatting: direct \`{pct:.0}%\` on a computed ratio bypasses
# the \`<1%\` indicator that \`text::format_pct\` / \`format_pct_f64\` provide,
# so genuinely small non-zero shares render as \`0%\` and become
# indistinguishable from true zeroes (Projects, Models, Tools detail).
# Use the helpers instead; the only exception is for signed deltas
# (month-over-month \`↑+N%\` / \`↓-N%\`) and the Hourly Avg 1-decimal column.
PCT_DIRECT=$(python3 -c "
import os, re
# Match \`{var:.0}%\` literal in format strings (any var name).
pat = re.compile(r'\{[a-zA-Z_][a-zA-Z0-9_]*:\.\0%\}|format!\(.*\"[^\"]*\{[a-zA-Z_][a-zA-Z0-9_]*:\.0\}%')
# Allowed surrounding text — signed delta or 1-decimal sites.
allow_re = re.compile(r'↑\+\{|↓\{|:\.1\}%|>4\.1\}%')
hits = []
for root, _, files in os.walk('src'):
    for fname in files:
        if not fname.endswith('.rs'):
            continue
        path = os.path.join(root, fname)
        with open(path) as f:
            for i, line in enumerate(f, 1):
                stripped = line.lstrip()
                if stripped.startswith('//'):
                    continue
                if '{pct:.0}%' in line and not allow_re.search(line):
                    hits.append(f'  {path}:L{i}: {line.rstrip()}')
print('\n'.join(hits))
" 2>/dev/null || true)
if [ -n "$PCT_DIRECT" ]; then
    echo "ERROR: direct \`{pct:.0}%\` format bypasses <1% indicator — use crate::text::format_pct or format_pct_f64:"
    echo "$PCT_DIRECT"
    ERRORS=$((ERRORS + 1))
fi

# 38. Past-state descriptions in comments: phrasings like "earlier this
# used X" / "previously the popup hid Y" / "was renamed from Z" force the
# reader to know a prior state the file no longer reflects. Rephrase the
# rule positively, in terms a reader can verify against current code. Past
# context belongs in the commit message / PR description.
# Test docstrings inside `mod tests` and `#[cfg(test)]` blocks are exempt.
PAST_STATE=$(python3 -c "
import os, re
# Phrases that almost always describe what the code USED to do.
# Word-boundaries keep \`once per call\` / \`at most once\` valid.
pat = re.compile(
    r'\b(earlier(\s+(this|the|it|we|they|a|an|version))?|'
    r'previously|'
    r'used\s+to\s+(be|have|return|use|read|store|live|do|call|render|use|exist)|'
    r'was\s+(renamed|deprecated|removed|replaced|moved)|'
    r'pre-v\d|in\s+v\d)\b',
    re.IGNORECASE,
)
hits = []
for root, _, files in os.walk('src'):
    for fname in files:
        if not fname.endswith('.rs'):
            continue
        path = os.path.join(root, fname)
        with open(path) as f:
            for i, line in enumerate(f, 1):
                stripped = line.strip()
                # Only \`//\` and \`///\` comments. Docstrings (\`///\`) describing
                # fixture arithmetic inside test blocks are exempt from this
                # rule; regular \`//\` comments are not — they describe runtime
                # behavior that the file should keep documenting honestly.
                if not stripped.startswith('//'):
                    continue
                if pat.search(line):
                    hits.append(f'  {path}:L{i}: {stripped[:140]}')
print('\n'.join(hits))
" 2>/dev/null || true)
if [ -n "$PAST_STATE" ]; then
    echo "ERROR: past-state phrasing in comment — rephrase as a present-tense invariant (see CLAUDE.md Comments):"
    echo "$PAST_STATE"
    ERRORS=$((ERRORS + 1))
fi

# 39. Direct `${...:.0}` format on a cost value bypasses `format_cost`'s
# two-sig-fig rule (sub-$1 → 2 decimals, $1-$10 → 1 decimal). Display
# callers must route cost values through `format_cost(_, 0)` so sig-figs
# stay consistent across surfaces. The one site inside `format_cost`'s
# body is the helper itself and is exempt. `src/cli.rs` keeps the precise
# 2-decimal form for stdout, also exempt.
COST_DIRECT=$(python3 -c "
import os, re
pat = re.compile(r'format!\(\"\\\$\\{[^\"]*\.0[\"}]')
hits = []
for root, _, files in os.walk('src'):
    if root.startswith(os.path.join('src', 'cli')) or root == 'src/cli':
        continue
    for fname in files:
        if not fname.endswith('.rs'):
            continue
        if fname == 'cli.rs':
            continue
        path = os.path.join(root, fname)
        with open(path) as f:
            for i, line in enumerate(f, 1):
                # The implementation of format_cost itself is the only
                # legitimate site for the raw \`\${c:.0}\` form.
                if 'fn format_cost' in line or '0 => format!' in line:
                    continue
                if pat.search(line):
                    hits.append(f'  {path}:L{i}: {line.rstrip()[:140]}')
print('\n'.join(hits))
" 2>/dev/null || true)
if [ -n "$COST_DIRECT" ]; then
    echo "ERROR: direct \`format!(\"\${...:.0}\", ...)\` bypasses format_cost sig-figs — route through crate::ui::format_cost(_, 0):"
    echo "$COST_DIRECT"
    ERRORS=$((ERRORS + 1))
fi

if [ $ERRORS -eq 0 ]; then
    echo "Lint: OK"
else
    echo ""
    echo "Lint: $ERRORS issue(s) found"
    exit 1
fi
