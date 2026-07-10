# remarkable-mcp

An [MCP](https://modelcontextprotocol.io) server for the **reMarkable** tablet
cloud, written in Rust. Browse, search, and inspect your tablet's library from
Claude, VS Code, or any MCP client.

> **Provenance — "be the Borg."** This is a ground-up Rust port that takes the
> best parts of three existing reMarkable MCP servers and fuses them into one:
> [lanej](https://github.com/lanej) (Go), [SamMorrowDrums](https://github.com/SamMorrowDrums)
> (Python), and [wavyrai](https://github.com/wavyrai) (Python). The per-project
> feature teardowns that drove these decisions are in [`docs/sources/`](docs/sources/).

---

## Status

**v1 is cloud, read-only, and metadata-focused.** It lists, trees, searches, and
inspects your library. Writing (mkdir/move/delete), content extraction, rendering,
OCR, and the SSH/USB transports are deliberately **out of scope for v1** and
tracked on the [roadmap](#roadmap).

The cloud protocol is reverse-engineered, but the read path is **verified
end-to-end against a live reMarkable account** (a 41-document library). CI still
can't exercise the network (it needs a real token), so unit tests cover the
parsing/logic with fixtures. See [Assumptions](#assumptions--caveats).

## What you get

Six tools, synthesized from the three sources:

| Tool | Description | Borrowed from |
|------|-------------|---------------|
| `remarkable_status` | Auth status, configuration, and document count. Call this first. | all |
| `remarkable_list`   | List a folder's contents, with recursion + `limit`/`offset` pagination. | lanej |
| `remarkable_tree`   | ASCII tree (📁/📄) with optional depth collapse and subtree focus. | lanej |
| `remarkable_search` | Case-insensitive name search, kind filter, and `did_you_mean` suggestions. | all |
| `remarkable_recent` | Most-recently-modified documents, newest first. | wavyrai |
| `remarkable_get`    | Metadata for one item, addressed by path **or** UUID. | lanej |

Cross-cutting niceties carried over from the sources:

- **Token-aware response budgeting** (lanej) — oversized listings return a
  `_pagination_needed` hint with a suggested `limit` instead of flooding the
  context window.
- **Root-hash change detection + bounded parallel fetch** (wavyrai) — a 400-doc
  library lists fast and re-lists nearly free when nothing changed.
- **Structured errors with `did_you_mean`** and a `_hint` on every response
  (SamMorrowDrums + wavyrai) so a model can recover instead of giving up.
- **`REMARKABLE_ROOT_PATH` scoping** (SamMorrowDrums), `REMARKABLE_COMPACT` output
  (wavyrai), and a secrets-redacting token store.

## Install & authenticate

```bash
# Build (or `cargo install --path crates/remarkable-mcp`)
cargo build --release

# 1. Get a one-time code from https://my.remarkable.com/device/desktop/connect
# 2. Register this machine (stores a 0600 token file under your XDG state dir):
./target/release/remarkable-mcp auth <one-time-code>

# 3. Confirm:
./target/release/remarkable-mcp status
```

### Wire it into an MCP client

`remarkable-mcp` with no arguments runs the server over stdio. Example Claude
Desktop / Claude Code config:

```json
{
  "mcpServers": {
    "remarkable": {
      "command": "/absolute/path/to/remarkable-mcp"
    }
  }
}
```

## Configuration

All configuration is via environment variables (CLI flag → env → default
precedence, per rust-cli-aspects CONFIG-AND-STATE).

| Variable | Default | Purpose |
|----------|---------|---------|
| `REMARKABLE_TOKEN_PATH` | XDG state dir | Override the token-store path. |
| `REMARKABLE_DEVICE_DESC` | platform (`desktop-macos`/`-windows`/`-linux`) | Device kind sent at registration; sets the reMarkable web "devices" label. Must be a recognized value (`desktop-*`, `mobile-*`, `browser-chrome`, `remarkable`) — reMarkable has no custom-name field. Only applied at `auth` time. |
| `REMARKABLE_SYNC_HOST` | `https://internal.cloud.remarkable.com` | Sync API host. |
| `REMARKABLE_SYNC_ROOT_PATH` | `/sync/v3/root` | Root endpoint (set `/sync/v4/root` to try v4). |
| `REMARKABLE_PARALLEL_WORKERS` | `8` | Concurrent metadata fetches (clamped 1–64). |
| `REMARKABLE_ROOT_PATH` | _(unset)_ | Scope all tools to a subfolder, e.g. `/Work`. |
| `REMARKABLE_COMPACT` | `0` | Omit `_hint` fields to save tokens. |
| `REMARKABLE_MAX_RESPONSE_TOKENS` | `4000` | Soft per-response token budget. |
| `REMARKABLE_MAX_OUTPUT_CHARS` | `50000` | Hard per-response character cap. |
| `RUST_LOG` | `remarkable=info,warn` | Log filter (logs go to **stderr**). |

## Architecture

A workspace with the [lib/bin split](https://github.com/) the rust-cli-aspects
ARCHITECTURE guidance treats as non-negotiable — the library is the product, the
binary is a thin wrapper.

```
crates/
├── remarkable-core/      # the product: cloud client + document model (no MCP)
│   ├── auth.rs           # token store: device/user tokens, atomic 0600 save, secrecy
│   ├── client.rs         # sync v3 protocol, root-hash cache, parallel fetch
│   ├── model.rs          # Item / metadata parsing
│   └── library.rs        # flat→tree: paths, folders, search, tree render, fuzzy
└── remarkable-mcp/       # thin bin: clap CLI + rmcp stdio server
    ├── main.rs           # `auth` / `status` / `mcp` subcommands
    ├── server.rs         # the six #[tool]s
    ├── response.rs       # hints, structured errors, token budgeting
    ├── scope.rs          # REMARKABLE_ROOT_PATH resolution (unit-tested)
    └── config.rs         # env-resolved presentation config
```

Built on [`rmcp`](https://docs.rs/rmcp) 1.8 (the official Rust MCP SDK), `tokio`,
`reqwest` (rustls), and `serde`.

### The reMarkable cloud protocol

The metadata path is identical across all three reference servers (mutual
agreement is the evidence, since it can't be tested here):

1. `POST {auth}/token/json/2/device/new` — pairing code → permanent device token.
2. `POST {auth}/token/json/2/user/new` — device token → ~24h user (bearer) token.
3. `GET {sync}/sync/v3/root` — JSON with the library root `hash`.
4. `GET {sync}/sync/v3/files/{root_hash}` — line-delimited index of all items.
5. Per item: fetch the blob index, locate `{uuid}.metadata`, fetch + parse it.
   Only the small `.metadata` blob is fetched — never page data.

(`auth` = `https://webapp-prod.cloud.remarkable.engineering`,
`sync` = `https://internal.cloud.remarkable.com`.)

## Testing

```bash
cargo test          # 33 unit tests: parsing, paths, trees, search, auth, scope
./scripts/smoke.sh  # deterministic, no-AI, no-network MCP handshake + tool grid
mise run ci         # fmt --check + clippy -D warnings + test
```

The smoke test (inspired by SamMorrowDrums' harness) drives the real binary over
stdio, verifies the handshake and all six tools, and checks that unauthenticated
tool calls return structured error envelopes rather than crashing.

## Assumptions & caveats

These are the decisions a maintainer would otherwise have asked about; they're
recorded here and in [`docs/adr/`](docs/adr/).

1. **The live cloud path is now verified** against a real account (41 documents).
   The original port of lanej's older code failed with HTTP 400 because today's
   API requires an `rm-filename` header on `/sync/v3/files/` GETs and uses a
   single-schema-line index format; both were corrected from SamMorrowDrums' live
   fix. CI still can't exercise the network (no token), so HTTP round-trips remain
   fixture-tested only.
2. **Read-only, cloud-only, metadata-only for v1.** Writes and content/rendering
   are the highest-risk, highest-dependency surfaces (lanej's writes even use a
   *different, deprecated* API than its reads); shipping them untested could
   corrupt a user's library. Deferred deliberately.
3. **Sync root defaults to v3.** lanej's v3 shape is the one with exact reference
   code; wavyrai/SamMorrowDrums use v4. The response is parsed defensively (just
   `hash`) and the path is overridable via `REMARKABLE_SYNC_ROOT_PATH`.
4. **Tokens live in a `0600` file**, following the universal reMarkable
   convention, wrapped so they never appear in logs. OS-keyring storage is a
   tracked enhancement, not a v1 requirement.

## Roadmap

Staged from the [`docs/sources/`](docs/sources/) synthesis (later = higher value *and* higher risk):

- Write tools (`mkdir`/`move`/`rename`/`delete`) with confirmation gating + annotations.
- Content extraction (PDF/EPUB text) and `.rm` rendering with PDF fallback.
- SQLite FTS5 content index + grep auto-redirect + multi-page reads (wavyrai).
- Pluggable / MCP-sampling OCR (wavyrai + SamMorrowDrums).
- SSH and USB-web transports behind a `Transport` trait (SamMorrowDrums).
- MCP prompts (`organize_library`, etc.) (lanej).

## License

MIT.
