# SamMorrowDrums — Feature Teardown (TPM lens)

**Upstream:** SamMorrowDrums (third-party; analyzed during the port, not vendored) · **Language:** Python · **MCP SDK:** `mcp` (FastMCP) ≥1.27
**Transport:** stdio · **reMarkable surface:** Cloud **+ SSH + USB-web** (3 transports w/ auto-fallback)
**One-liner:** The maximalist, production-grade reference. Most features, most transports, most tests.

---

## What this project actually is

The "everything" implementation. 13 tools spanning read, render, OCR, write, and an **interactive MCP Apps canvas** (SEP-1865). It abstracts three physical connection methods (cloud, on-device SSH, USB web interface) behind one client interface and **auto-falls back to cloud** when a device transport is configured but unreachable. It is the most ambitious of the three by a wide margin — and the most operationally complex.

## Best features (ranked by port value)

| # | Feature | Why it matters (TPM) | Port? |
|---|---------|----------------------|-------|
| 1 | **Transport abstraction + capability matrix** (cloud/SSH/USB behind one interface; tools disabled per-transport at registration; `remarkable_status` reports a read/render/upload/mkdir/… matrix) | This is the *product insight* of the whole corpus: the same user wants their tablet whether it's plugged in (fast SSH), on USB, or remote (cloud). Exposing **what each transport can/can't do** is honest UX. | ✅ **Yes — north star** (phase via a `Transport` trait; cloud first) |
| 2 | **`_hint` on every response + `did_you_mean` fuzzy matching** | Turns dead-end errors into next-actions. "Document not found" → "did you mean *Q3 Planning*?" is the difference between a model that recovers and one that gives up. | ✅ Yes |
| 3 | **Confirmation-gated destructive writes** (delete elicits confirmation unless `REMARKABLE_SKIP_CONFIRM=1`; `destructiveHint=True` annotation; cloud delete → trash, recoverable) | Irreversible ops behind a confirmation is table stakes for a tool an LLM drives autonomously. Tool annotations let clients render the risk. | ✅ Yes (annotations + read-only mode now; elicitation later) |
| 4 | **Smart `.rm` rendering with PDF fallback** (rmscene v5/v6 auto-detect → SVG → PNG; on render failure, fall back to the tablet's native PDF export) | The killer feature *and* the killer complexity. The fallback chain is what makes it survive firmware drift. | ⏳ Later (pulls in cairo + rm parser; out of v1 Rust scope) |
| 5 | **Pluggable OCR** (MCP **sampling** = client's own LLM, no API key; Google Vision; Tesseract; `auto`) | Sampling-based OCR is genuinely novel: zero-config handwriting recognition using the connected model. No secrets to manage. | ⏳ Later (depends on rendering) |
| 6 | **Async correctness** (every blocking call — SSH, HTTP, render, OCR, zip — pushed to threads via `asyncio.to_thread`) | Without this, concurrent tool calls serialize. In Rust this is *free-er* (tokio + spawn_blocking) but the discipline is the lesson. | ✅ Principle (tokio-native) |
| 7 | **Three-tier test strategy** (unit mocks / live-SSH integration / **deterministic multi-transport smoke harness** that drives the real server over stdio, PASS/FAIL/N-A/SKIP grid, write ops confined to a per-run folder + cleanup) | The smoke grid is the best test artifact in the corpus — it proves *every tool × every transport*, no AI, with cleanup. A model PR-reviewer's dream. | ✅ Adopt the *shape* (stdio smoke test) |
| 8 | **Auto-redirect ergonomics** (`browse("/Doc.pdf")` returns the read result; auto-OCR when a notebook has no typed text) | Removes a round-trip. The tool does the obviously-intended thing. | ✅ Yes (browse→read redirect) |
| 9 | **Root-path scoping** (`REMARKABLE_ROOT_PATH=/Work` scopes all ops, paths shown relative to root) | Lets a user safely point the server at one folder. Good for least-privilege. | ✅ Yes (cheap) |
| 10 | **Interactive canvas** (MCP Apps, negotiated at `initialize`, degrades to PNG) | Forward-looking; great demo. Not core. | ❌ Skip (niche, heavy) |

## Critical assessment (where it falls short)

- **Operational surface is large.** Three transports × write tools × OCR backends × rendering fallbacks = a big test/maintenance matrix. The smoke harness exists *because it has to*. For a v1 Rust port, shipping all of this at once is a quality risk.
- **SSH/USB require device-side setup** (developer mode / USB web interface). Powerful but not universal; cloud is the only zero-setup path.
- **Write-on-by-default** is a defensible but bold stance for an autonomous agent; we will prefer **read-only-by-default with explicit opt-in**.
- **Heavy native deps** (cairo, rmscene, PyMuPDF, tesseract). The render/OCR pipeline is the bulk of the complexity and the least portable to Rust quickly.
- **Delete semantics differ by transport** (cloud=trash, SSH=permanent) — correct, but a footgun that the capability matrix only partly mitigates.

## Verdict for the Borg

**SamMorrowDrums sets the product vision; we adopt its *ideas* before its *volume*.** Steal the transport-capability concept (as a `Transport` trait, cloud-first), `_hint`/`did_you_mean`, confirmation-gated + annotated writes, root scoping, browse→read redirect, and especially the **stdio smoke-test shape**. Defer rendering/OCR/SSH/USB/canvas to clearly-scoped later phases — they are where the value *and* the risk concentrate. The lesson: this project earned its complexity; our v1 should earn it incrementally.
