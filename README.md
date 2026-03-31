# ClawedCode

ClawedCode is an educational Rust-native reimplementation of a coding agent shell, focused on startup speed, terminal UX, and compatibility research.

## Install

### Cargo (Rust)

From crates.io:

```bash
cargo install clawedcode
```

From source (this repo):

```bash
cargo install --path crates/clawedcode-cli
```

### npm (prebuilt binaries)

Global install:

```bash
npm install -g clawedcode
```

This downloads a platform-specific `clawedcode-bin` from GitHub Releases on install (and also lazily on first run if needed).

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

- `crates/`: Rust workspace crates (`clawedcode` CLI + core/libs)
- `docs/`: architecture and implementation notes

## Direction

The implementation is intentionally small in the first pass. The next layer is the execution engine: model adapters, approval handling, transcript streaming, tool dispatch, and task orchestration.
