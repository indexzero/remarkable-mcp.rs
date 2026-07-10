#!/usr/bin/env bash
# Deterministic, no-AI, no-network smoke test for the MCP server.
#
# Inspired by SamMorrowDrums' smoke harness: it drives the real binary over stdio,
# performs the MCP handshake, lists tools, and exercises the unauthenticated tool
# paths (which return structured errors, not crashes). Exits non-zero on any
# deviation. Network/auth are NOT required — tool *calls* without a token return
# the documented `not_authenticated` envelope.
set -euo pipefail

cd "$(dirname "$0")/.."
cargo build --quiet

BIN=target/debug/remarkable-mcp
# Point at a guaranteed-absent token file so the run is hermetic.
export REMARKABLE_TOKEN_PATH="$(mktemp -u)/tokens.json"

REQUESTS=$(cat <<'JSON'
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"remarkable_status","arguments":{}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"remarkable_list","arguments":{"path":"/"}}}
JSON
)

OUT_FILE=$(mktemp)
trap 'rm -f "$OUT_FILE"' EXIT
printf '%s\n' "$REQUESTS" | timeout 15 "$BIN" 2>/dev/null > "$OUT_FILE" || true

# Pass the captured transcript as argv[1]; the heredoc is python's program (stdin),
# so the two cannot share the same stdin.
python3 - "$OUT_FILE" <<'PY'
import json, sys

with open(sys.argv[1]) as f:
    rows = [json.loads(l) for l in f if l.strip()]
by_id = {m.get("id"): m for m in rows if "id" in m}

def fail(msg):
    print(f"SMOKE FAIL: {msg}")
    sys.exit(1)

# initialize
init = by_id.get(1, {}).get("result")
if not init or init.get("serverInfo", {}).get("name") != "remarkable-mcp":
    fail("initialize did not return serverInfo.name=remarkable-mcp")

# tools/list
tools = by_id.get(2, {}).get("result", {}).get("tools")
names = sorted(t["name"] for t in (tools or []))
expected = sorted([
    "remarkable_status", "remarkable_list", "remarkable_tree",
    "remarkable_search", "remarkable_recent", "remarkable_get",
])
if names != expected:
    fail(f"tools/list mismatch: {names} != {expected}")

# status tool: unauthenticated, well-formed JSON
status = json.loads(by_id[3]["result"]["content"][0]["text"])
if status.get("authenticated") is not False:
    fail("status should report authenticated=false with no token")

# list tool: structured not_authenticated error, not a crash
listing = json.loads(by_id[4]["result"]["content"][0]["text"])
if listing.get("_error", {}).get("type") != "not_authenticated":
    fail("list without a token should return a not_authenticated error envelope")

grid = "\n".join(f"  ✅ {n}" for n in expected)
print("SMOKE PASS")
print(f"  handshake: {init['serverInfo']['name']} {init['serverInfo']['version']} "
      f"(protocol {init['protocolVersion']})")
print(f"  {len(expected)} tools registered:")
print(grid)
PY
