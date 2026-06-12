#!/bin/bash
# CI smoke test for ccsight TUI.
#
# Headless CI (no real ~/.claude). Builds a small but non-trivial fixture,
# launches ccsight under tmux, then drives it through every tab and a detail
# popup, asserting expected content appears and nothing panics. Navigation
# uses poll-until-ready (`wait_for`) rather than fixed sleeps so it stays
# robust on slow/variable runners. `scripts/smoke.sh` covers richer scenarios
# against real local data.

set -euo pipefail

SESSION="ccsight_ci"

if ! command -v tmux >/dev/null 2>&1; then
    echo "smoke-ci: tmux not installed — skipping"
    exit 0
fi

cargo build --release --locked

# Isolated HOME so we never touch the contributor's real ~/.claude/.
ISOLATED_HOME="$(mktemp -d)"
cleanup() {
    tmux kill-session -t "$SESSION" 2>/dev/null || true
    rm -rf "$ISOLATED_HOME"
}
trap cleanup EXIT

# Fixture: two projects, two sessions on different days, two models, and a
# handful of tool calls (Read/Edit/Bash → .rs/.ts extensions) so the Dashboard
# panels (Projects / Models / Ecosystem / Languages), Daily, Insights and the
# detail popups all have something to render.
mk_session() {
    local proj="$1" sid="$2" day="$3" model_a="$4" model_b="$5"
    local dir="$ISOLATED_HOME/.claude/projects/$proj"
    mkdir -p "$dir"
    cat >"$dir/$sid.jsonl" <<JSONL
{"type":"summary","summary":"smoke $proj","leafUuid":"$sid"}
{"type":"user","uuid":"${sid}-u1","timestamp":"${day}T10:00:00Z","sessionId":"$sid","cwd":"/tmp/$proj","message":{"role":"user","content":"do the thing"}}
{"type":"assistant","uuid":"${sid}-a1","parentUuid":"${sid}-u1","timestamp":"${day}T10:00:01Z","sessionId":"$sid","requestId":"${sid}-r1","message":{"id":"${sid}-m1","role":"assistant","model":"$model_a","content":[{"type":"text","text":"on it"},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/tmp/$proj/src/main.rs"}},{"type":"tool_use","id":"t2","name":"Edit","input":{"file_path":"/tmp/$proj/src/app.ts"}},{"type":"tool_use","id":"t3","name":"Bash","input":{"command":"cargo test"}}],"usage":{"input_tokens":1200,"output_tokens":300,"cache_creation_input_tokens":100,"cache_read_input_tokens":50,"cache_creation":{"ephemeral_5m_input_tokens":80,"ephemeral_1h_input_tokens":20}}}}
{"type":"assistant","uuid":"${sid}-a2","parentUuid":"${sid}-a1","timestamp":"${day}T10:01:00Z","sessionId":"$sid","requestId":"${sid}-r2","message":{"id":"${sid}-m2","role":"assistant","model":"$model_b","content":[{"type":"text","text":"done"}],"usage":{"input_tokens":80,"output_tokens":40}}}
JSONL
}
mk_session "-tmp-projA" "00000000-0000-0000-0000-0000000000a1" "2026-05-14" "claude-opus-4-8" "claude-sonnet-4-20250514"
mk_session "-tmp-projB" "00000000-0000-0000-0000-0000000000b1" "2026-05-15" "claude-opus-4-7" "claude-opus-4-8"

tmux kill-session -t "$SESSION" 2>/dev/null || true
HOME="$ISOLATED_HOME" tmux new-session -d -s "$SESSION" -x 140 -y 45 \
    "$(pwd)/target/release/ccsight"

frame() { tmux capture-pane -t "$SESSION" -p 2>/dev/null || true; }

# Poll until `needle` appears (or timeout). Far more robust than a fixed sleep
# on CI runners, which vary 3-5x in cold-start + parse + index-build time.
wait_for() {
    local needle="$1" timeout="${2:-40}" i=0
    while [ "$i" -lt "$timeout" ]; do
        if frame | grep -qF "$needle"; then return 0; fi
        sleep 1
        i=$((i + 1))
    done
    echo "FAIL: timed out (${timeout}s) waiting for '$needle'" >&2
    echo "── last frame ──"; frame; echo "────────────────"
    return 1
}

fail=0
# A panic unwinds through the panic hook before the TUI can redraw, so its
# "thread ... panicked at" text bleeds into the captured pane.
assert_no_panic() {
    if frame | grep -q "panicked at"; then
        echo "FAIL: panic detected after step '$1'" >&2
        echo "── frame ──"; frame; echo "───────────"
        fail=1
    fi
}

step() {
    local keys="$1" needle="$2" label="$3"
    tmux send-keys -t "$SESSION" "$keys"
    if ! wait_for "$needle"; then fail=1; return; fi
    assert_no_panic "$label"
}

# 1) Initial load: header switches from the splash to "N sessions" once the
#    aggregator finishes. This is the readiness gate for everything below.
if ! wait_for "sessions"; then
    fail=1
else
    assert_no_panic "initial-load"
    # 2) Walk every tab. Needles must be BODY content unique to the target
    # tab/popup — tab-bar labels ("Daily", "Insights") are visible on every
    # frame, so they'd pass without the key ever being processed.
    step "1" "Overview" "dashboard-tab"
    step "2" "Active now" "live-tab"
    step "3" "Breakdown (" "daily-tab"
    step "4" "Today vs Average" "insights-tab"
    # 3) Open the Insights detail popup (single-key) and close it.
    step "i" "Usage by category" "insights-detail-open"
    tmux send-keys -t "$SESSION" Escape
    assert_no_panic "insights-detail-close"
    # 4) Back to Dashboard, open a panel detail popup via Enter, then close.
    step "1" "Overview" "dashboard-return"
    tmux send-keys -t "$SESSION" Enter
    sleep 1
    assert_no_panic "dashboard-detail-open"
    tmux send-keys -t "$SESSION" Escape
    assert_no_panic "dashboard-detail-close"
fi

echo "── final frame ─────────────────────────────────────────"
frame
echo "────────────────────────────────────────────────────────"

if [ "$fail" -ne 0 ]; then
    exit 1
fi
echo "smoke-ci: OK"
