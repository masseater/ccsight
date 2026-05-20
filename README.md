# ccsight

A Rust TUI for viewing Claude Code session logs with statistics, cost analysis, and conversation browsing.

## Features

- **Dashboard**: Costs, tokens, projects, models, tools, languages, heatmap, hourly patterns
- **Live**: Currently-running and recently-paused sessions with status glyphs, copy-resume command, snapshot recovery so you can find what you had open after a host reboot
- **Daily View**: Day-by-day sessions with activity graph, breakdown, conversation viewer
- **Insights**: Metrics, today vs average, weekly/monthly trends with detail popups
- **Conversation**: Multi-pane browsing with syntax highlighting, search, copy
- **Search**: Full-text search across all sessions (tantivy, multilingual)
- **Pin**: Mark important sessions, browse across dates
- **MCP Server**: query ccsight from other Claude clients (`--mcp`)
- **Caching**: Fast startup via on-disk cache and full-text index

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
ccsight --mcp              # Run as MCP server (stdio)
ccsight --clear-cache      # Drop the JSON cache + tantivy index, rebuild on next run
ccsight --limit 50         # Load only the 50 most recent sessions (faster startup)
```

Run `ccsight --help` for the full flag list. Press `?` in the TUI for key bindings.

## MCP Server

`ccsight --mcp` runs as a stdio MCP server exposing three tools:

- `stats` — aggregated cost / token / model metrics, plus a per-MCP-server adoption snapshot
- `sessions` — list & detail (with `conversation_query` for in-session search)
- `search` — full-text search across all sessions (tantivy)

All tools accept `date_from` / `date_to` (`YYYY-MM-DD`, local timezone).

### Register with Claude Code

The simplest way — Claude Code's CLI registers the MCP server in your user-scoped
config so it's available across every project:

```bash
claude mcp add --scope user ccsight -- ccsight --mcp
```

### Manual config (Claude Code / Claude Desktop)

Alternatively, add an entry under `mcpServers` in `~/.claude.json` (Claude Code)
or `~/Library/Application Support/Claude/claude_desktop_config.json` (Claude Desktop):

```json
{
  "mcpServers": {
    "ccsight": {
      "command": "ccsight",
      "args": ["--mcp"]
    }
  }
}
```

After saving, restart the host. The tools then appear under the `ccsight` MCP server.

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
  — Claude Desktop "Cowork" tab session logs (`audit.jsonl`) plus per-session
  metadata. Ingested alongside regular Claude Code logs. The format is
  undocumented and may change between releases; if a future update breaks it,
  individual sessions just stop appearing rather than crashing the TUI.

State written by ccsight (cache + index are removed by `--clear-cache`; pins
and the live snapshot are kept):

- `~/.ccsight/cache.json` — parsed-session JSON cache (incremental)
- `~/.ccsight/index/` — tantivy full-text index segments
- `~/.ccsight/pins.json` — pinned-session list
- `~/.ccsight/live_snapshot.json` — record of sessions seen alive, used by the
  Live tab to flag "I had this open before the reboot" entries with a `⟳` glyph

Pre-1.1 versions wrote to `~/.cache/ccsight/` and `~/.config/ccsight/`; ccsight
migrates those automatically on first launch.

## License

[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE)
