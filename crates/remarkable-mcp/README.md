# remarkable-mcp

An [MCP](https://modelcontextprotocol.io) server for the **reMarkable** tablet
cloud. Browse, search, and inspect your tablet's library from Claude, VS Code, or
any MCP client.

## Install

```bash
cargo install remarkable-mcp
# or, prebuilt (no compile):
cargo binstall remarkable-mcp
```

## Use

```bash
# 1. Pair (one-time code from https://my.remarkable.com/device/desktop/connect):
remarkable-mcp auth <one-time-code>
remarkable-mcp status

# 2. Run as an MCP server over stdio (no arguments):
remarkable-mcp
```

Wire it into an MCP client:

```json
{ "mcpServers": { "remarkable": { "command": "remarkable-mcp" } } }
```

Tools: `remarkable_status`, `remarkable_list`, `remarkable_tree`,
`remarkable_search`, `remarkable_recent`, `remarkable_get`.

Full documentation, configuration, and design notes:
<https://github.com/indexzero/remarkable-mcp.rs>

## License

MIT
