#!/bin/bash
# CI smoke test for ccsight TUI.
#
# Designed for headless CI (no real `~/.claude/` data). Sets up a minimal
# fixture, launches ccsight under tmux, captures a screenshot after the
# initial render, and asserts the basic UI shell drew without panicking.
# Use `scripts/smoke.sh` for richer scenarios with real local data.

set -euo pipefail

# Pre-flight: tmux must be available.
if ! command -v tmux >/dev/null 2>&1; then
    echo "smoke-ci: tmux not installed — skipping"
    exit 0
fi

# Build release binary (no-op if already built).
cargo build --release --locked

# Isolated HOME so we don't touch the contributor's real ~/.claude/.
ISOLATED_HOME="$(mktemp -d)"
trap 'rm -rf "$ISOLATED_HOME"' EXIT
mkdir -p "$ISOLATED_HOME/.claude/projects/-tmp-fixture"

# Minimal JSONL fixture: one summary entry, one user message, one
# assistant message. Enough that the aggregator + UI have data to render.
cat >"$ISOLATED_HOME/.claude/projects/-tmp-fixture/00000000-0000-0000-0000-000000000001.jsonl" <<'JSONL'
{"type":"summary","summary":"ci smoke fixture","leafUuid":"00000000-0000-0000-0000-000000000001"}
{"type":"user","uuid":"u1","timestamp":"2026-05-15T10:00:00Z","sessionId":"00000000-0000-0000-0000-000000000001","cwd":"/tmp/fixture","message":{"role":"user","content":"hello"}}
{"type":"assistant","uuid":"a1","parentUuid":"u1","timestamp":"2026-05-15T10:00:01Z","sessionId":"00000000-0000-0000-0000-000000000001","message":{"id":"msg_1","role":"assistant","model":"claude-opus-4-7","content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":5,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}
JSONL

# Kill any leftover session from a previous run on the same runner.
tmux kill-session -t ccsight_ci 2>/dev/null || true

HOME="$ISOLATED_HOME" tmux new-session -d -s ccsight_ci -x 140 -y 45 \
    "$(pwd)/target/release/ccsight"

# Generous wait: cold ccsight has to parse the JSONL, build the tantivy
# index, and run the first poll cycle. CI runners are slower than local.
sleep 10

capture_file="$(mktemp)"
tmux capture-pane -t ccsight_ci -p >"$capture_file"
tmux kill-session -t ccsight_ci 2>/dev/null || true

echo "── captured frame ──────────────────────────────────────"
cat "$capture_file"
echo "────────────────────────────────────────────────────────"

# Assertions: banner shows, at least one tab indicator is visible, and no
# panic-shaped output bled through (panics typically show "thread ...
# panicked at" if the panic hook unwinds before the TUI redraws).
fail=0
for needle in "C C S I G H T" "Dashboard" "Live" "Daily" "Insights"; do
    if ! grep -q "$needle" "$capture_file"; then
        echo "FAIL: expected '$needle' in capture" >&2
        fail=1
    fi
done
if grep -q "panicked at" "$capture_file"; then
    echo "FAIL: panic detected in capture" >&2
    fail=1
fi

rm -f "$capture_file"
if [ "$fail" -ne 0 ]; then
    exit 1
fi
echo "smoke-ci: OK"
