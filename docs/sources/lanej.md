# lanej — Feature Teardown (TPM lens)

**Upstream:** lanej (third-party; analyzed during the port, not vendored) · **Language:** Go · **MCP SDK:** `github.com/modelcontextprotocol/go-sdk` v0.2.0
**Transport:** stdio · **reMarkable surface:** Cloud only (sync v3 / sync15)
**One-liner:** The clean, disciplined, metadata-only cloud client. Narrow scope, high polish.

---

## What this project actually is

A focused MCP server over the reMarkable **cloud sync v3** protocol that does **document management** (browse, search, organize) and nothing else — no content extraction, no rendering, no OCR. It treats the library as a tree of metadata and is engineered to be *correct and economical* rather than *broad*.

The standout engineering decision: it generates a type-safe API client from a **reverse-engineered OpenAPI spec** (`api/openapi.yaml`) via `ogen`. That is a maturity signal most MCP servers lack.

## Best features (ranked by port value)

| # | Feature | Why it matters (TPM) | Port? |
|---|---------|----------------------|-------|
| 1 | **Token-aware response budgeting** (`REMARKABLE_MAX_RESPONSE_TOKENS`, default 4000; `len/4` token estimate) | An LLM tool that floods the context window is *worse than useless*. lanej detects oversized responses and returns a **structured pagination hint** ("use limit=15, offset by 15") instead of dumping. This is the single most LLM-native idea in any of the three. | ✅ **Yes — flagship** |
| 2 | **Depth-limited ASCII tree** (`remarkable_tree`, `📁/📄`, `depth=1` collapses children to "… (N items)") | Humans and models both reason better over a shaped tree than a flat list. The depth collapse keeps it bounded. | ✅ Yes |
| 3 | **Flat→hierarchy path resolution** (`buildPathMap`, recursive parent-walk with memoization) | The cloud API returns a *flat* list with parent UUIDs. Turning that into `/Work/Notes` paths is the core data-model primitive every tool needs. lanej's memoized implementation is the reference. | ✅ Yes — core |
| 4 | **Metadata-only sync traversal** (fetch blob index → grab only the `.metadata` file by hash, never the full blob) | Listing a 400-doc library without downloading 400 zip archives. This is *the* performance unlock for cloud mode. | ✅ Yes — core |
| 5 | **Dual ID/path addressing** (`GetItemByPath` falls back from UUID parse to path lookup) | Tools accept either `/Work/Notes` or a raw UUID. Low cost, high ergonomics. | ✅ Yes |
| 6 | **Token lifecycle done right** (permanent device token + 23h user-token cache + `EnsureUserToken` auto-refresh; XDG `~/.local/state`, 0600) | Auth that silently refreshes is invisible auth. XDG + 0600 is the correct storage discipline. | ✅ Yes |
| 7 | **MCP Prompts** (`organize_library`, `backup_documents`, `import_documents`) | Prompts are an underused MCP primitive — they ship *workflows*, not just tools. Cheap differentiation. | ✅ Yes |
| 8 | **Acceptance tests gated by `//go:build acceptance` + auth skip** | Honest test split: unit tests always run; live tests skip cleanly when unauthenticated. | ✅ Pattern |

## Critical assessment (where it falls short)

- **Single nil-keyed in-memory cache, invalidated wholesale.** No root-hash change detection (wavyrai's L2 is smarter). Fine for a session, weak for a long-lived server.
- **No content at all.** Can't read a PDF, can't see a notebook page. For "summarize my meeting notes," lanej is a dead end. This is the scope ceiling.
- **Mutations rely on the *legacy* document-storage API** (`RequestUpload` + `UpdateMetadata` PUT), discovered via a separate service host — *not* the sync v3 write path. This is a coherence risk: reads use sync15, writes use the deprecated API. **Unverifiable without a live device** and a likely source of subtle bugs (version conflicts, generation races).
- **`upload`/`download` registered in spec but not implemented.** Aspirational surface.
- **Hardcoded `deviceDesc = "desktop-linux"`**, no host overrides — fine, but inflexible.

## Verdict for the Borg

**lanej is the architectural spine for cloud metadata.** Take its sync-v3 traversal, path-map, tree, dual addressing, token lifecycle, response budgeting, and prompts almost verbatim. **Leave** its legacy-API mutation path as untrusted (re-derive or defer with eyes open) and **leave** the ogen/OpenAPI codegen (overkill for a hand-written Rust client). It is the "do less, but correctly" reference.
