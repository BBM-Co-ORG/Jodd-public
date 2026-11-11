#!/usr/bin/env bash
# Compare headless invocation variants on one prompt. Run from an
# AUTHENTICATED session — an unauthenticated claude dies before the API and
# every number below collapses to startup cost alone.
set -uo pipefail
PROMPT=${1:-"Reply with the single word: ok"}
WORK=$(mktemp -d); trap 'rm -rf "$WORK"' EXIT; cd "$WORK" || exit 1

run() {
  local label="$1"; shift
  local start end out
  start=$(python3 -c 'import time;print(time.time())')
  out=$(printf '%s' "$PROMPT" | "$@" 2>&1 | tail -1)
  end=$(python3 -c 'import time;print(time.time())')
  printf '%-42s wall=%6.2fs api_ms=%s in=%s out=%s\n' "$label" \
    "$(python3 -c "print($end-$start)")" \
    "$(printf '%s' "$out" | python3 -c 'import sys,json;d=json.load(sys.stdin);print(d.get("duration_api_ms","?"))' 2>/dev/null || echo '?')" \
    "$(printf '%s' "$out" | python3 -c 'import sys,json;d=json.load(sys.stdin);print(d.get("usage",{}).get("input_tokens","?"))' 2>/dev/null || echo '?')" \
    "$(printf '%s' "$out" | python3 -c 'import sys,json;d=json.load(sys.stdin);print(d.get("usage",{}).get("output_tokens","?"))' 2>/dev/null || echo '?')"
}

run "claude: as shipped before this plan" claude -p --output-format json
run "claude: isolated (this plan)" claude -p --output-format json \
    --strict-mcp-config --setting-sources '' --tools ''

# Control: proves the harness reports a real difference rather than noise.
run "claude: isolated, repeat" claude -p --output-format json \
    --strict-mcp-config --setting-sources '' --tools ''
