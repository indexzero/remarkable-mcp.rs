# wavyrai — Feature Teardown (TPM lens)

**Upstream:** wavyrai (third-party; analyzed during the port, not vendored) · **Language:** Python · **MCP SDK:** `mcp` (FastMCP) ≥1.0
**Transport:** stdio · **reMarkable surface:** Cloud only (sync v4 root / v3 files)
**One-liner:** The search-and-retrieval specialist. Cloud-only, read-only, but the smartest about caching and findability.

---

## What this project actually is

A **read-only**, cloud-only MCP server (6 tools) whose distinguishing investment is **information retrieval**: a persistent **SQLite FTS5 full-text index** of document content, a disciplined **3-layer cache** (in-memory → SQLite → cloud), and **root-hash change detection** so it doesn't re-traverse the library on every call. Where lanej is "manage the library" and SamMorrow is "do everything," wavyrai is "**find the right note fast, then read it well**."

## Best features (ranked by port value)

| # | Feature | Why it matters (TPM) | Port? |
|---|---------|----------------------|-------|
| 1 | **Root-hash change detection** (cheap `GET /sync/v4/root` returns a hash; skip full collection re-fetch if unchanged) | This is the caching primitive lanej *lacks*. One tiny request tells you whether anything changed. It makes a long-lived server cheap and snappy (388 docs: ~4s cold, ~0.5s warm). | ✅ **Yes — core caching** |
| 2 | **3-layer cache** (L1 in-mem extraction/OCR/file-type/render caches → L2 SQLite FTS5 → L3 cloud) | Survives process restarts, serves search without re-download. The layering is textbook. | ✅ L1 now; L2 FTS later |
| 3 | **SQLite FTS5 content index** (`documents` + `pages` + `pages_fts`, porter/unicode61, snippet extraction, per-page OCR cache w/ backend tracking) | Full-text search across a handwritten+typed library is a genuine superpower. Reports **index coverage** ("8/50 indexed") so the user knows what's searchable. | ⏳ Later (needs content extraction first) |
| 4 | **Structured errors** (`make_error` → `error_type`, `message`, `suggestion`, `did_you_mean`) + **grep auto-redirect** (no match on page N → search all pages, jump to first hit) | Same recover-don't-fail philosophy as SamMorrow, cleanly factored. Grep auto-redirect is a lovely touch — the tool finds the page for you. | ✅ Yes (errors now; grep later) |
| 5 | **`compact_output` / `REMARKABLE_COMPACT`** (omit `_hint`s to cut tokens) + **`REMARKABLE_MAX_OUTPUT_CHARS`** (50k cap) | Another LLM-native token-economy control, complementary to lanej's budgeting. Let the caller choose verbosity. | ✅ Yes |
| 6 | **Thread-safe token renewal** (401 → lock → device→user exchange → retry; `HTTPAdapter` retry w/ backoff, `status_forcelist=[500,502,503,504]`, pooled connections) | Correct concurrency around the single shared mutable thing (the user token). The retry/backoff + pooling is production hygiene. | ✅ Yes (tokio-native: `RwLock`/`OnceCell` + retry) |
| 7 | **Parallel metadata fetch** (`ThreadPoolExecutor`, `REMARKABLE_PARALLEL_WORKERS=5`, clamped) | Same unlock as the others; bounded concurrency is the right default. | ✅ Yes (`buffer_unordered`) |
| 8 | **Multi-page specs** (`pages="all" | "1-3" | "2,4,5"`) + **char-based pagination** (~8000 chars/page) | Thoughtful read ergonomics for long PDFs. | ⏳ Later (with content) |
| 9 | **Zip-slip protection** (`_safe_extractall` validates extraction paths) | Security hygiene most reference impls skip. Noted for when we add blob download. | ✅ Principle |
| 10 | **Sampling-OCR with explicit model preference** (`intelligencePriority=1.0, speedPriority=0.2, costPriority=0.0`; opus→sonnet→gemini→gpt) | Tuning sampling hints for *accuracy over cost* is the right call for OCR. | ⏳ Later |

## Critical assessment (where it falls short)

- **Read-only, cloud-only.** No writes, no SSH/USB. Narrower than SamMorrow by design. For "organize my library," wavyrai can't help.
- **FTS index value is gated on content extraction**, which still needs rmscene/rmc + cairo (same native-dep tax as SamMorrow). The index is only as good as what it can extract; empty notebooks need OCR to be findable.
- **`REMARKABLE_OCR_BACKEND=sampling` only** (no Vision/Tesseract fallback like SamMorrow) — simpler, but no offline path.
- **Cache invalidation is TTL + root-hash**, not event-driven — a 60s window where a change made elsewhere isn't seen. Acceptable, worth documenting.
- **Index coverage can be low** on first runs (lazy/background indexing), so early searches under-return. The coverage report mitigates but doesn't fix the cold-start gap.

## Verdict for the Borg

**wavyrai is the brain for findability and cheap freshness.** Even before we have content extraction, take its **root-hash change detection** and **bounded parallel fetch** and **structured errors** and **compact/cap token controls** — these are pure wins for a cloud metadata server *today*. Stage the **SQLite FTS5 index**, **grep auto-redirect**, **multi-page reads**, and **sampling OCR** for the content phase, where they become the differentiator. wavyrai proves that "read-only but *fast and findable*" is a complete, shippable product on its own.

---

## Cross-source synthesis (what the Borg takes from all three)

| Capability | Best source | Take now / later |
|---|---|---|
| Cloud sync-v3 metadata traversal | lanej | **Now** |
| Flat→tree path map, ASCII tree | lanej | **Now** |
| Token-aware response budgeting | lanej | **Now** |
| MCP prompts (workflows) | lanej | **Now** |
| Root-hash change detection + bounded parallel fetch | wavyrai | **Now** |
| Structured errors + `did_you_mean` + `_hint` | SamMorrow + wavyrai | **Now** |
| `compact_output` / output caps | wavyrai | **Now** |
| Confirmation-gated + annotated writes, read-only default | SamMorrow | **Now** (annotations + gating) |
| Root-path scoping, browse→read redirect | SamMorrow | **Now** |
| stdio smoke-test grid | SamMorrow | **Now** (shape) |
| Transport trait (cloud → SSH → USB) | SamMorrow | **Later** |
| `.rm` render + PDF fallback | SamMorrow | **Later** |
| SQLite FTS5 + grep auto-redirect + multi-page | wavyrai | **Later** |
| Pluggable / sampling OCR | wavyrai + SamMorrow | **Later** |
| Interactive canvas (MCP Apps) | SamMorrow | **Maybe** |
