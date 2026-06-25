# ADR 0001 — A cloud-only, read-first Rust port

- **Status:** Accepted
- **Date:** 2026-06-25
- **Deciders:** initial port (no live reMarkable hardware available)

## Context

We are porting three reMarkable MCP servers — lanej (Go), SamMorrowDrums (Python),
and wavyrai (Python) — into one Rust implementation, taking the best parts of each
("be the Borg"). The three differ sharply in scope:

- lanej: cloud-only, metadata-only, disciplined, well-budgeted.
- SamMorrowDrums: three transports (cloud/SSH/USB), writes, rendering, OCR, an
  interactive canvas — the largest surface and the largest operational risk.
- wavyrai: cloud-only, read-only, but the strongest caching and findability.

Two hard constraints shaped the decision:

1. **No reMarkable account/token was available**, so the live network path cannot
   be exercised. The cloud protocol is reverse-engineered.
2. The sync15 **write** path is content-addressed and dangerous; lanej even
   implements writes against a *different, deprecated* API than its reads. An
   untested write path could corrupt a user's library.

## Decision

Ship v1 as **cloud-only, read-only, metadata-only**, built on a strict
`remarkable-core` (lib) / `remarkable-mcp` (bin) split:

- Port lanej's sync-v3 metadata traversal, path/tree logic, dual ID/path
  addressing, and token-aware response budgeting.
- Add wavyrai's root-hash change detection, bounded parallel fetch, and structured
  errors.
- Add SamMorrowDrums' `did_you_mean`/`_hint` ergonomics, root-path scoping, and the
  stdio smoke-test shape.
- Defer writes, content/rendering, OCR, FTS indexing, and SSH/USB transports to
  later, clearly-scoped phases.

Parsing, path resolution, tree rendering, search, scoping, and the token store are
**unit-tested with fixtures**; the HTTP round-trips are not (they can't be without
a device). The sync root defaults to v3 (the shape with exact reference code) and
is overridable to v4.

## Consequences

- **Positive:** a small, correct, fully-tested, mergeable v1 that does real work
  (browse/search/organize-by-reading) with no chance of damaging user data. The
  lib/bin split makes the deferred transports and writes additive, not rewrites.
- **Negative:** no content reading or writing yet; the live cloud path is unproven
  until a real token is used (first real use is the integration test).
- **Mitigations:** the network shapes carry three independent sources' agreement;
  responses are parsed defensively; every assumption is documented in the README
  and here.

## Revisit when

A reMarkable token is available for live verification, or a user needs write /
content features — at which point writes should land behind confirmation gating
and annotations (per SamMorrowDrums) and only after live read verification.
