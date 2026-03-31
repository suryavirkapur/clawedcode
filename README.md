# ClawedCode

ClawedCode is a fast Rust-native coding agent shell, aiming to be a 1:1 replacement for Claude Code.

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

This downloads a platform-specific `clawedcode-bin` from GitHub Releases during install (and also lazily on first run if needed).

## Usage

Start the interactive REPL:

```bash
clawedcode
```

Run a single prompt non-interactively:

```bash
clawedcode run --prompt "inspect this workspace"
```

## Direction

The focus is: terminal UX, compatibility research, and a clean Rust implementation of boot flow, provider streaming, tools/permissions, MCP, and agent orchestration.
