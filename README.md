# ccsight

Claude Code session analytics TUI. Browse what's running now, drill into a day,
reconcile spend across projects and models, and search every past conversation
— all from the terminal, against your local `~/.claude/` logs.

## Features

- **Dashboard** — cost (with 5m vs 1h cache-write TTL separated), daily / monthly trends, top projects, models, tools, languages, activity heatmap, hourly pattern
- **Live** — currently-running and recently-paused sessions, copy-resume command, post-reboot recovery via local snapshots, time-travel through past snapshots with `←/→`
- **Daily View** — per-day sessions + activity graph + project / model / tool breakdown + conversation viewer
- **Insights** — metrics (cost, tokens, cache, tool success, subagent overhead), today vs average, weekly / monthly trends — each with detail popups
- **Conversation** — multi-pane, syntax highlighting, in-pane search, per-turn latency / cost / token breakdown
- **Search** — full-text (tantivy) + inline filter tokens + persistent history (`↑/↓`)
- **Pin** — mark sessions, reorder with `Shift+J/K`, browse across dates
- **MCP Server** — `--mcp` exposes `stats`, `sessions`, `search`, `live_sessions` to other Claude clients
- **Caching** — on-disk cache + incremental full-text index for fast startup

## Installation

Pick **one** of the following:

Homebrew:

```bash
brew install esorae/tap/ccsight
```

Cargo (crates.io):

```bash
cargo install ccsight
```

From source:

```bash
cargo install --path .
```

> **macOS**: If downloading the binary directly from GitHub Releases, run `xattr -d com.apple.quarantine ccsight` to clear the Gatekeeper flag. Homebrew and `cargo install` are not affected.

## Usage

```bash
ccsight                    # Run TUI
ccsight --daily            # Print daily cost table to stdout (date / tokens / cost)
ccsight --weekly           # Same shape, aggregated by ISO week (Mon-Sun)
ccsight --monthly          # Same shape, aggregated by calendar month
ccsight --mcp              # Run as MCP server (stdio)
ccsight --clear-cache      # Drop the JSON cache + tantivy index, rebuild on next run
ccsight --limit 50         # Load only the 50 most recent sessions (faster startup)
```

Run `ccsight --help` for the full flag list. Press `?` in the TUI for key bindings.

### Search filters

Press `/` inside the TUI to open the search popup. Plain queries match
projects, summaries, branches, dates, and conversation content. Add any
of these tokens to narrow the result set further:

| Token | Effect |
|-------|--------|
| `filter:live` / `filter:paused` / `filter:busy` | Limit to sessions in the current Live / paused / busy poll |
| `filter:today` / `filter:week` / `filter:month` | Calendar window (local timezone) |
| `filter:date:YYYY-MM-DD` | Exact date |
| `project:NAME` / `branch:NAME` / `model:NAME` | Substring match (case-insensitive) |

Tokens can be combined freely with each other and with free text:

```text
/filter:today project:ccsight             # today's ccsight sessions
/filter:live branch:main                  # currently-running on main branch
/filter:month model:opus mcp setup        # last 30 days, Opus, containing "mcp setup"
/filter:date:YYYY-MM-DD release           # specific date narrowed by content
```

The Live tab pre-fills `filter:live ` for `/`, and the search popup chips up recognised tokens so you can confirm parsing.

## MCP Server

`ccsight --mcp` runs as a stdio MCP server exposing four tools:

- `stats` — aggregated cost / token / model metrics + per-MCP-server adoption snapshot
- `sessions` — list & detail (with `conversation_query` for in-session search)
- `search` — full-text search across all sessions (tantivy)
- `live_sessions` — currently-running and recently-disconnected sessions

The first three accept `date_from` / `date_to` (`YYYY-MM-DD`, local timezone); `live_sessions` always reports the current poll.

Register with Claude Code (simplest, user-scoped — works in every project):

```bash
claude mcp add --scope user ccsight -- ccsight --mcp
```

For Claude Desktop or hand-rolled configs, add this block under `mcpServers` in `~/.claude.json` or `~/Library/Application Support/Claude/claude_desktop_config.json`, then restart the host:

```json
{ "mcpServers": { "ccsight": { "command": "ccsight", "args": ["--mcp"] } } }
```

## Data Source

Reads inputs from these locations:

- **`~/.claude/projects/<project>/<session>.jsonl`** — session logs (conversation
  history, tool calls, token usage). Discovered recursively at startup.
- **`~/.claude.json`** + enabled-plugin `.mcp.json` — Claude Code's MCP config.
  Used to classify each MCP server as **active** (used in last 30d), **stale**
  (configured but idle ≥30d, including never used), or **inactive** (in logs
  but no longer in any config).
- **`~/.claude/{skills,commands,agents}/`** + enabled-plugin paths — installed
  Skills / Commands / Subagents. Surfaced as zero-call rows in the Tools popup
  for entries you've installed but never invoked.
- **`~/Library/Application Support/Claude/local-agent-mode-sessions/`** *(macOS only)*
  — Claude Desktop "Cowork" sessions. Read via a side-channel format; if a
  release breaks it, individual sessions silent-skip rather than crashing.

State written by ccsight (cache + index are removed by `--clear-cache`; pins
and the live history are kept):

- `~/.ccsight/cache.json` — parsed-session JSON cache (incremental)
- `~/.ccsight/index/` — tantivy full-text index segments
- `~/.ccsight/pins.json` — pinned-session list
- `~/.ccsight/live_snapshot.json` — latest alive-session poll (diagnostic only)
- `~/.ccsight/live_snapshots/<date>-<HHMM>.json` — alive-set history; powers
  `⟳ restorable` and `←/→ h/l` time-travel. 7-day retention

## License

[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE)
