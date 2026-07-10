#!/usr/bin/env python3
"""rmc — drive the remarkable-mcp stdio server from the command line.

Performs a full MCP handshake (initialize + notifications/initialized) then a
single tools/call or tools/list, and prints the tool's text result.

Usage:
  ./rmc.py                                  # list tools
  ./rmc.py list                             # same
  ./rmc.py remarkable_status
  ./rmc.py remarkable_tree depth=2
  ./rmc.py remarkable_tree path=/Work depth=3
  ./rmc.py remarkable_search query=notes limit=5
  ./rmc.py remarkable_get target=/Work/Q3
  ./rmc.py remarkable_tree '{"depth":2,"path":"/Work"}'   # raw JSON also accepted

Arguments are `key=value` pairs (values are JSON-coerced: 2 -> int, true -> bool,
everything else -> string), or a single JSON object.

Environment:
  REMARKABLE_MCP_BIN     explicit path to the binary
  REMARKABLE_TOKEN_PATH  point at a specific token file
  (and every other REMARKABLE_* var the server reads)

Binary resolution: $REMARKABLE_MCP_BIN, then ./target/release, ./target/debug, PATH.
"""
import json
import os
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
PROTOCOL_VERSION = "2025-11-25"
TOOLS = [
    "remarkable_status",
    "remarkable_list",
    "remarkable_tree",
    "remarkable_search",
    "remarkable_recent",
    "remarkable_get",
]


def find_bin():
    env = os.environ.get("REMARKABLE_MCP_BIN")
    if env:
        return env
    for rel in ("target/release/remarkable-mcp", "target/debug/remarkable-mcp"):
        cand = os.path.join(HERE, rel)
        if os.path.isfile(cand) and os.access(cand, os.X_OK):
            return cand
    found = shutil.which("remarkable-mcp")
    if found:
        return found
    sys.exit("error: remarkable-mcp binary not found; run `cargo build` or set REMARKABLE_MCP_BIN")


def coerce(value):
    """JSON-coerce a bare CLI value: 2 -> int, true -> bool, else the raw string."""
    try:
        return json.loads(value)
    except (json.JSONDecodeError, ValueError):
        return value


def parse_args(items):
    if not items:
        return {}
    if len(items) == 1 and items[0].lstrip().startswith("{"):
        return json.loads(items[0])
    out = {}
    for item in items:
        if "=" not in item:
            sys.exit(f"error: expected key=value or a JSON object, got: {item!r}")
        key, val = item.split("=", 1)
        out[key] = coerce(val)
    return out


def rpc(requests):
    """Feed a batch of JSON-RPC messages to the server; return parsed responses."""
    payload = "".join(json.dumps(r) + "\n" for r in requests)
    proc = subprocess.run(
        [find_bin()], input=payload, capture_output=True, text=True
    )
    messages = []
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            messages.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    return messages


def handshake():
    return [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "rmc", "version": "0"},
            },
        },
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
    ]


def call_tool(name, args):
    req = {
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": name, "arguments": args},
    }
    for msg in rpc(handshake() + [req]):
        if msg.get("id") != 2:
            continue
        if "error" in msg:
            print(json.dumps(msg["error"], indent=2))
            return 1
        content = msg.get("result", {}).get("content") or []
        if content and content[0].get("type") == "text":
            print(content[0]["text"])
        else:
            print(json.dumps(msg.get("result", {}), indent=2))
        return 0
    print("error: no response from server", file=sys.stderr)
    return 1


def list_tools():
    req = {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}
    for msg in rpc(handshake() + [req]):
        if msg.get("id") != 2:
            continue
        for tool in msg["result"]["tools"]:
            required = ", ".join(tool.get("inputSchema", {}).get("required", [])) or "(none)"
            print(f"{tool['name']:<20} required: {required}")
            desc = (tool.get("description") or "").strip().split("\n")
            if desc and desc[0]:
                print(f"{'':<20}   {desc[0].strip()}")
        return 0
    print("error: no response from server", file=sys.stderr)
    return 1


def main(argv):
    if not argv or argv[0] in ("list", "--list", "-l", "tools"):
        return list_tools()
    if argv[0] in ("-h", "--help"):
        print(__doc__)
        return 0
    tool = argv[0]
    if tool not in TOOLS:
        print(f"warning: unknown tool {tool!r} (known: {', '.join(TOOLS)})", file=sys.stderr)
    try:
        args = parse_args(argv[1:])
    except json.JSONDecodeError as exc:
        sys.exit(f"error: invalid JSON arguments: {exc}")
    return call_tool(tool, args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
