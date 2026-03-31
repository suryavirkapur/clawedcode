# Compatibility

## Current compatibility goals

The Rust implementation should honor the existing filesystem conventions operators already rely on.

This first pass includes discovery for:

- `~/.claude/settings.json`
- project `.claude/settings.json`
- project `.claude/settings.local.json`
- `~/.claude/skills/<name>/SKILL.md`
- project `.claude/skills/<name>/SKILL.md`
- legacy `.claude/commands` markdown entries
- project `.mcp.json`
- `mcpServers` declared in settings files

## Current status

The repository now resolves and reports those assets through `cargo run -- compat`.

That gives us a stable starting point for:

- loading the same skill inventory
- reusing the same MCP declarations
- preserving operator configuration during the rewrite

## Next work

1. Map more settings fields into typed Rust config structures.
2. Add runtime loading for discovered skills and MCP servers.
3. Implement the execution semantics and approval behavior around those assets.
