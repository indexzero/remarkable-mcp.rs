# remarkable-core

The library half of [`remarkable-mcp`](https://crates.io/crates/remarkable-mcp):
a reMarkable cloud client and document model, with no MCP/transport concerns.

- `CloudClient` — the reMarkable sync v3 metadata protocol (device/user token auth,
  root-hash change detection, bounded parallel fetch).
- `Library` — reconstructs hierarchy (paths, trees, folders, search) from the flat
  cloud listing.
- `TokenStore` — device/user token storage with atomic, `0600` persistence.

This crate is primarily consumed by the `remarkable-mcp` binary. See
<https://github.com/indexzero/remarkable-mcp.rs> for the full project.

## License

MIT
