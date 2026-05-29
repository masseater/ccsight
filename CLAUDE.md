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
bash scripts/install-hooks.sh   # One-time pre-commit hook setup
```

The pre-commit hook runs `cargo fmt -- --check`, `cargo test`, `cargo clippy`,
and `bash scripts/lint.sh` on every commit. CI runs the same set.

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
| `infrastructure/live_snapshot.rs` | `last_refresh_at` anchors the `⟳` cluster; **must be frozen at boot in `AppState::prior_run_last_refresh`** — re-deriving mid-run silently drops markers. |
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
versions, dates), or any phrasing that requires the reader to know what
the code used to be. The file outlives the incident; "earlier this used
X" / "previously the popup hid Y" / "this was renamed from Z" reads as a
factual claim about a state the reader can't see and has no way to
verify. Same for `e.g.`-style examples that quote real values: they read
as illustrations but date as snapshots.

- ❌ `// regression in vA.B.C where two surfaces disagreed on the count.`
- ❌ `// e.g. "94%" once crushed the bars to "0–1%".`
- ❌ `// Earlier this used `builtin.len() + mcp_servers.len()`.`
- ❌ `// Tools panel was renamed "Ecosystem" because it covers Skills too.`
- ✅ `// Truncating mid-number drops the K/M suffix and reads much smaller.`
- ✅ `// `Ecosystem` covers Built-in + MCP + Skills + Subagents in one panel.`

State the rule in terms a future reader can verify against the code in
front of them. Past incidents, captured values, and rename history go in
the commit message or PR description. Lint #38 detects the most common
past-state phrasings (`earlier`, `previously`, `used to be`, `was
renamed`, `pre-vN`). Test docstrings inside `mod tests` are exempt — they
describe fixture arithmetic that is part of the assertion contract.

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

- `~/.ccsight/live_snapshot.json` — dump with `python3 -c "import json; ..."`
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
