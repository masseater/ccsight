# CLAUDE.md

See [README.md](README.md) for usage. Press `?` in TUI for key bindings.

## Build & Development

```bash
cargo build --release        # Build
cargo run                    # Run TUI
cargo test                   # Run tests
cargo clippy -- -D warnings  # Lint (warnings are errors)
bash scripts/lint.sh         # Project lint (UI patterns, safety)
bash scripts/install-hooks.sh   # One-time pre-commit hook setup
```

The pre-commit hook runs the three checks above on every commit.

## Smoke test (tmux)

The TUI can't launch from a non-interactive shell — use tmux at **140x45** (the
size popup-width math assumes; smaller terminals truncate rightmost columns).
Wait `sleep 5` after launch for data + tantivy index, `sleep 1` between keys.
A reference scenario covering Tools popup, search → conv pane, filter / project
popups, and Insights detail lives in `scripts/smoke.sh`. Watch for: rightmost
column truncation, popup borders missing `border_style`, `▼/▶` arrow inversion,
"Searching..." stuck after Enter (index not committed — see lint #18).

## Monkey test

When asked to "broadly inspect ccsight" or after large refactors, use the smoke
script as a starting point but go beyond — visit every popup and tab, compare
the same number across surfaces, and flag anything that disagrees. Past
sessions caught regressions matching these patterns:

- **Same number, different views**: TUI Overview vs Costs panel vs `--daily`
  CLI total. Dashboard preview's stale count vs the popup's `⚠ N stale`. The
  Insights metrics row vs the detail popup's "Usage by category".
- **Same word, different meanings**: "groups" once meant 31 in the popup and
  61 in the preview. "stale" once mixed `>30` and `>=30` thresholds.
- **Tab/section indices**: Help text says `1-N`; verify each digit jumps to
  the named section. Cycle with Tab/h/l and ensure the order matches.
- **Configured but unused**: every category (Skills / Commands / Subagents /
  MCP) should surface installed-but-never-invoked entries. If a tab only
  shows entries from `tool_usage`, it's missing them.
- **Empty / loading state**: searching while the index hasn't committed must
  show "Searching...", not "No results". Same for data reload.
- **Truncation & ellipsis**: long names should end with `…`, never raw cut.
- **Mid-session mutability**: model badges, custom titles, branches must
  reflect the LAST value, not the first (see `extract_session_model`).
- **Scroll bounds**: scrollbar `N/M` should reach M, not stop short. j/k past
  the visible end should not produce blank rows.

When you find a discrepancy, prefer surfacing all of them in one report
before fixing — fixes often fan out into multiple sites that share a value.

## Architecture

`ls src/` shows the file tree; this section only covers modules whose purpose
isn't obvious from the filename.

| Module | Why it exists |
|--------|---------------|
| `aggregator/mod.rs` | `extract_session_model` walks entries reverse so mid-session `/model` switches are reflected in the badge. |
| `aggregator/pricing.rs` | `models_without_pricing` set — models absent from the pricing table are surfaced (Overview `*`, Insights, MCP `stats.pricing_gap`) instead of silently $0. |
| `aggregator/tool_category.rs` | Classifies every tool key into `BuiltIn` / `Mcp` / `Skill` / `Agent` / `Command`. UI order: **Tools (Built-in + MCP) → Skills → Commands → Subagents**. `format_tool_short` strips the `skill:` / `agent:` / `command:` prefix (sections already convey category). |
| `infrastructure/mcp_config.rs` | Joins `~/.claude.json` + plugin `.mcp.json` with observed `tool_usage`. `is_underutilized(now, 30)` is the canonical "stale" predicate. Servers in logs but absent from current config are **inactive**, not stale. |
| `infrastructure/resource_config.rs` | `discover_configured_resources()` walks `~/.claude/{skills,commands,agents}/` plus enabled plugin paths so the Tools popup can show 0-call rows for installed-but-unused entries. |
| `infrastructure/cowork_source.rs` | Claude Desktop Cowork audit logs (Anthropic-private format — undocumented, may break between Claude Desktop releases). Discovery is silent on parse failure so per-file errors don't crash the TUI. |
| `infrastructure/search_index.rs` | tantivy ngram(2,3) index at `~/.ccsight/index/`. `update_or_build()` handles fresh / incremental / reuse. |
| `infrastructure/state_dir.rs` | Single source of truth for on-disk state paths under `~/.ccsight/`. `migrate_legacy_state_dirs()` is called at startup to relocate pre-1.1 paths. |
| `infrastructure/live_sessions.rs` | Discovers active Claude Code processes via `~/.claude/sessions/<pid>.json` + PID liveness, plus paused sessions via JSONL mtime. `WARM_THRESHOLD_SECS` is the single source of truth shared between the sort tier and the row glyph. |
| `infrastructure/live_snapshot.rs` | Persists "session_id last seen alive" so that after a host reboot (every Claude Code process killed) `recover_missing()` can bring those rows back into the paused list with the `⟳` glyph. |
| `handlers/mcp_popup.rs` | Tools-tab cursor row math. Coord origin must match the body slicing in `dashboard.rs::draw_dashboard_detail_popup` (see doc comment). |
| `shell.rs` | `posix_shell_quote` — the only sanctioned way to embed user-derived strings into `cd ... && claude -r ...` resume commands. Lint #28 enforces. |
| `state.rs` | `AppState`, `ConversationPane`, `TextInput`. Adding a field is compiler-checked at every `AppState { ... }` literal. |
| `test_helpers.rs` | `#[cfg(test)]` fixtures. `create_test_state()` returns an `AppState` with deterministic token / cost values. |

## Rules

**Lint-enforced** (single source of truth: `scripts/lint.sh`):

```bash
rg '^# [0-9]+\.' scripts/lint.sh   # current rule list
```

Add new rules by copying an existing block and bumping the number.

**When to add a lint rule** — only patterns likely to recur. Use these gates:

- *Plausible regression*: a future contributor (or AI) would naturally reach
  for the wrong pattern (it autocompletes, it's the obvious helper, etc.).
- *Silent failure*: the wrong pattern doesn't produce a compile/test error,
  and the bug surfaces only later — wrong number on a rare popup, broken
  visual at a rare width, regression after some other change.
- *Single canonical pattern*: there's one right way to do it that's
  expressible as a substring / regex check.
- *Low false-positive rate*: legitimate exceptions are few and easily
  allowlisted.

If a pattern fails any gate (one-shot mistake, compiler/test catches it,
many legitimate variants, or noisy in practice), prefer a doc comment next
to the canonical site or trust review — don't add a lint.

**Not lint-enforceable** — these live as doc comments next to the code they
govern, so they're seen at the moment of violation:

- Popup overlay guards (4 sites) → `main.rs::dismiss_overlay`
- TextInput / UTF-8 safety → `state.rs::TextInput`
- Scroll / cursor coord unity → `handlers::mcp_popup::mcp_pre_server_offset`
- Session-representative value → `aggregator::extract_session_model`
- Footer span format → first `let help_spans = vec![ ... ]` in `ui/dashboard.rs`

**Compiler-enforced** (no rule needed):
- AppState init parity — every `AppState { ... }` literal lists every field.
- Real-world identifier shapes — fully covered by lint #16 / #17 / #21.

**Suppressing warnings (`#[allow(...)]`)**: never apply on a guess. Before
adding `#[allow(dead_code)]` / `#[allow(clippy::...)]` / `#[allow(unused)]`,
identify *why* the compiler / clippy flagged the symbol — read the macro
source, the upstream API change, or the surrounding code path. Outcome is
one of: (a) **remove the symbol** (truly unused after an API migration);
(b) **wire it up** (actually needed but a usage site is missing);
(c) **keep `allow` with a code comment** that names the macro / generated
code / FFI boundary that reads the symbol invisibly to the compiler. Bare
`#[allow]` without that justification silently buries real bugs (case (a)
or (b)) under a "looks clean" diff.

## Commits

Match the existing voice — skim `git log --oneline -30` before writing one.

Subject only — no bullet body. Lowercase, no period, target ~80 chars (up
to ~100 OK). Optional area prefix (`fix:`, `theme:`, `docs:`, `lint #N:`,
or a feature/file context like `Tools popup:`). Join multiple small
changes with `;` `+` `,`. Pitch at the *what changed* level — concrete
enough that a reader can imagine the diff, but no internal field names,
version numbers, or implementation details that age out.

Always include a `Co-Authored-By` trailer for Claude-assisted commits; omit only for pure-human commits.

## Comments

Comments explain invariants, not history. Don't write incidents, prior bug
numbers, captured numbers (percentages, $ amounts, K/M magnitudes,
versions, dates), or "earlier this was X" — the file outlives the
incident and the comment turns into a lie. Same for `e.g.`-style examples
that quote real values: they read as illustrations but date as snapshots.

- ❌ `// regression in vA.B.C where two surfaces disagreed on the count.`
- ❌ `// e.g. "94%" once crushed the bars to "0–1%".`
- ✅ `// Truncating mid-number drops the K/M suffix and reads much smaller.`

State the rule in terms a future reader can verify against the code in
front of them. Past incidents and captured values go in the commit message
or PR description. Test docstrings inside `mod tests` are exempt — they
describe fixture arithmetic that is part of the assertion contract.

## Tests — three layers

| Layer | When | How |
|-------|------|-----|
| Unit (`#[test]`) | Pure logic (aggregation, formatting, classifiers). | `cargo test`; lives next to the code. |
| Render (`TestBackend`) | UI text / layout / row regression. | `render_to_text(&mut state, w, h)` in `ui/mod.rs::tests`. Use `create_test_state()`. Test 120x35, 140x45 (popup math), 60x20 (narrow). |
| Smoke (tmux) | Real-data, async paths (index build, summary), cross-popup key sequences. | `scripts/smoke.sh`; capture with `tmux capture-pane -p`. |

Pick the lowest layer that exercises what you changed.

## Tools detail popup

Tab order: **0=Tools, 1=Skills, 2=Commands, 3=Subagents** (`tools_detail_section`).
The Tools tab merges Built-in tools and MCP servers into one expandable list;
the other three each render their own category. Every tab also surfaces
"configured but never used" entries as zero-call rows (`░░░ 0 0% · never` in
`LABEL_SUBTLE`) sourced from `state.mcp_status` (Tools) or
`state.configured_resources` (Skills / Commands / Subagents).

Cursor / scroll math has to agree across four sites:

1. Body layout in `dashboard.rs::draw_dashboard_detail_popup` active==0
2. Group enumeration in `mcp_popup::collect_mcp_servers`
3. `mcp_pre_server_offset` (header row count)
4. `dashboard::tool_usage_line_count` (scroll bound = rendered rows)

Stale = configured & `is_underutilized(now, 30)`. This single predicate powers
the preview legend, the popup `⚠`, and the Tier 3 alert — never re-implement
the threshold inline (lint #19).

## Key Patterns

- **Search state**: `[Normal] → / → [Search] → Enter → [Preview] → Esc → [Search] → Esc → [Normal]`. Preview saves tab/position via `search_saved_state`.
- **Pane search**: VS Code style — Enter/Shift+Enter = next/prev, Esc closes the bar (n/N still work).
- **Async**: Background threads for data load, summary, index build via `mpsc::channel`. UI must show "Searching..." (not "No results") while a loading flag is set. Every `Receiver` must be polled with `match` covering `Disconnected` — `let Ok(...) = rx.try_recv()` silently swallows worker-thread panics and freezes the task forever (lint #26).
- **MCP tools** (`mcp.rs`): `stats` (+ per-server `mcp_servers` snapshot), `sessions`, `search`. All share `date_from` / `date_to` (`YYYY-MM-DD`, local timezone).
- **Costs include subagents**: Overview, Costs panel, and `--daily` CLI all sum over `group.sessions` so totals match.
- **stderr is forbidden** — writes corrupt the TUI rendering. Lint #20 enforces.
- **Atomic file writes**: every `tmp + rename` write under `~/.ccsight/` must `sync_all` before the rename, otherwise the data isn't durable across power loss. Lint #29 enforces.
- **Shell command escaping**: any string composed for the user to paste into a shell (`cd ... && claude -r ...`) must route both interpolations through `crate::shell::posix_shell_quote`. The cwd / session_id come from on-disk JSON. Lint #28 enforces.
- **Cursor vs viewport**: scrollable lists with a selection (Daily, Live, Projects detail, MCP server detail) decouple the cursor from the viewport — viewport adjusts only when the cursor leaves the visible window (Vim's `scrolloff` pattern). Scroll-only views collapse the two into one value. New panels with a selection cursor must follow the decoupled pattern.
- **Render-stable sort**: any `Vec` built from a `HashMap` and sorted on a single key produces non-deterministic order for tied values (HashMap iteration is randomized per instance). Always add a tiebreaker (typically alphabetical on name) via `.then_with(|| a.0.cmp(b.0))` so rows don't shuffle between frames. Lint #30 enforces.
