# CLAUDE.md

See [README.md](README.md) for usage. Press `?` in TUI for key bindings.

## Build & Development

```bash
cargo build --release        # Build
cargo run                    # Run TUI
cargo fmt                    # Format (gate: `cargo fmt -- --check`)
cargo test                   # Run tests
cargo clippy -- -D warnings  # Lint (warnings are errors)
bash scripts/lint.sh         # Project lint (UI patterns, safety)
```

The `scripts/hooks/` pre-commit (per commit) and pre-push (before push) hooks
run `cargo fmt --check` + test + clippy + lint; CI runs the same gate.
`build.rs` arms them on first build via `core.hooksPath` (opt out:
`CCSIGHT_NO_HOOK_INSTALL=1`; manual: `install-hooks.sh`). pre-push exists
because `git cherry-pick` skips pre-commit.

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

| Module | Non-obvious invariant |
|--------|------------------------|
| `aggregator/mod.rs` | `extract_session_model` walks entries reverse — mid-session `/model` switches must reflect in the badge. |
| `aggregator/pricing.rs` | `models_without_pricing` surfaces unpriced models (`*` mark) instead of silently rendering $0. |
| `aggregator/tool_category.rs` | UI section order: **Tools → Skills → Commands → Subagents**. `format_tool_short` strips category prefix (section already conveys it). |
| `infrastructure/mcp_config.rs` | `is_underutilized(now, 30)` is the canonical "stale" predicate. Servers absent from current config are **inactive**, not stale. |
| `infrastructure/live_sessions.rs` | `is_today()` is the single source of truth shared between sort tier and row glyph (busy / today / older). |
| `infrastructure/live_snapshots.rs` | Per-poll alive-set history at `~/.ccsight/live_snapshots/<YYYY-MM-DD>-<HHMM>.json`. Diff-detected + 30-min debounce: a poll that sees no change is a no-op; same-window changes overwrite the latest file; cross-window changes create a new one. Drives the Live tab's `⟳ restorable` flag and `←/→ h/l` time-travel. Singular `LiveSnapshot` struct here = one historical record; the diagnostic singleton (latest alive-set + `last_refresh_at`) lives in `live_diagnostic.rs`. |
| `infrastructure/live_diagnostic.rs` | Singleton `~/.ccsight/live_snapshot.json`. Diagnostic only: stores the last poll's alive metadata so external readers (MCP, debug scripts) can answer "when did the TUI last refresh". The Live tab's restorable logic does NOT read this file. |
| `infrastructure/cache.rs` | `CachedFileStats` is single-shape (no partial flag). Both `Stats::aggregate_with_shared_cache` (TUI) and `DailyGrouper::group_by_date_with_shared_cache` (`--daily`) write fully-populated entries derived from the same entry-walk — anything else gets re-parsed on the other reader's next pass. Bump `CACHE_VERSION` when adding a field. |
| `shell.rs` | `posix_shell_quote` is the only sanctioned interpolation for `cd ... && claude -r ...`. Lint #28 enforces. |
| `state.rs` | Adding a field is compiler-checked at every `AppState { ... }` literal site (3+ places). |

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

**Real data never becomes a literal**: lint #17's denylist is *reactive* — it
only matches terms already listed, so a fresh project / directory / glob /
domain name copied out of `~/.claude` or the local filesystem **while
investigating real data** slips straight through (a real project directory
name once reached a test fixture this way). Whenever observed data informs a
test fixture, comment, doc example, or smoke-test path, abstract every real
identifier to a generic placeholder *before* it lands in committed source:
`config/` not a real dir, `multi-word-dir` not a real repo, `~/proj` not a
real path, `**/*.{a,b}` not a real glob. Then add the term to
`.lint-forbidden-terms` (git-ignored) so the reactive net also catches a
relapse — and note that this doc, README, and source are all scanned, so the
denylisted term itself must never appear here either.

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

Comments state invariants verifiable against the code in front of the
reader. No history, incident numbers, captured values (%, $, K/M, dates,
versions), `e.g.`-with-real-values, or "earlier/previously/was renamed"
phrasings — those belong in the commit message. Lint #38 catches the
common past-state words. `mod tests` docstrings are exempt (fixture
arithmetic is part of the assertion).

- ❌ `// regression in vA.B.C where two surfaces disagreed on the count.`
- ❌ `// e.g. "94%" once crushed the bars to "0–1%".`
- ❌ `// Earlier this used `builtin.len() + mcp_servers.len()`.`
- ❌ `// Tools panel was renamed "Ecosystem" because it covers Skills too.`
- ✅ `// Truncating mid-number drops the K/M suffix and reads much smaller.`
- ✅ `// `Ecosystem` covers Built-in + MCP + Skills + Subagents in one panel.`

### Length (lint #41)

Per contiguous block: `//` / `///` ≤ 5 lines, `//!` ≤ 20 lines.
Overflow → trim WHY to the invariant, refactor so less explanation is
needed, or move narrative into the commit message / PR description. Two
adjacent blocks split by a blank line are NOT a fix — usually one is
redundant. Escape hatch `// lint-ok: long-comment` on the first line,
only when every line is load-bearing.

## Tests — three layers

| Layer | When | How |
|-------|------|-----|
| Unit (`#[test]`) | Pure logic (aggregation, formatting, classifiers). | `cargo test`; lives next to the code. |
| Render (`TestBackend`) | UI text / layout / row regression. | `render_to_text(&mut state, w, h)` in `ui/mod.rs::tests`. Use `create_test_state()`. Test 120x35, 140x45 (popup math), 60x20 (narrow). |
| Smoke (tmux) | Real-data, async paths (index build, summary), cross-popup key sequences. | `scripts/smoke.sh`; capture with `tmux capture-pane -p`. |

Pick the lowest layer that exercises what you changed.

**`cargo clippy -- -D warnings` (CI gate) does NOT check `#[cfg(test)]` code.**
Unused vars and dead_code in test blocks slip through. After touching test
files run `cargo clippy --release --tests -- -D warnings` locally before
committing.

## Verifying logic changes

Green unit tests are not proof that a fix lands in production. For changes
that depend on on-disk state (snapshot, cache, ~/.claude files), open the
actual files and verify the fix engages:

- `~/.ccsight/live_snapshot.json` — diagnostic singleton; dump with `python3 -c "import json; ..."`
- `~/.ccsight/live_snapshots/<YYYY-MM-DD>-<HHMM>.json` — alive-set history; `ls` to see frozen poll moments, `cat` to inspect a specific snapshot
- `~/.claude/sessions/*.json` — `ls -la` + `cat <pid>.json`, check `kill -0`
- `~/.claude/projects/*/<id>.jsonl` — `wc -l`, `head -1` / `tail -1`

The conversation history has several incidents where a fix passed all unit
tests but did nothing on real data because the test fixtures didn't match
the on-disk shape. When in doubt, dump the actual file before claiming the
fix worked.

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
