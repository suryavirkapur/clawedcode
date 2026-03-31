# ClawedCode

ClawedCode is an educational Rust-native reimplementation of a coding agent shell, focused on startup speed, terminal UX, and compatibility research.

## Current status

This repository now has a clean Rust baseline with:

- a real CLI entrypoint
- compatibility discovery for existing config, skills, and MCP files
- persisted config and session storage
- a built-in system prompt registry
- a starter runtime loop
- a Ratatui shell for the interactive path

## Commands

```bash
cargo run -- config
cargo run -- compat
cargo run -- run --prompt "inspect this workspace"
cargo run -- tui
```

## Layout

- `src/app.rs`: top-level application flow
- `src/cli.rs`: command-line parsing
- `src/config.rs`: config loading and defaults
- `src/prompt.rs`: built-in prompt packs
- `src/runtime.rs`: session lifecycle and request submission
- `src/session.rs`: transcript persistence
- `src/tool.rs`: tool registry and metadata
- `src/tui.rs`: interactive terminal surface
- `docs/`: architecture and implementation notes

## Direction

The implementation is intentionally small in the first pass. The next layer is the execution engine: model adapters, approval handling, transcript streaming, tool dispatch, and task orchestration.
