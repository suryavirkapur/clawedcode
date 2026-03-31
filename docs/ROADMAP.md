# Reimplementation Roadmap

This roadmap is for the Rust-native rewrite of `claude-code-src`, not for the small bootstrap we started with.

It is aligned with:

- the current Rust workspace layout
- the upstream architecture described in [SPEC.md](/home/sk/Projekts/clawedcode/docs/SPEC.md)
- the actual weight of the upstream codebase: bootstrap, REPL shell, provider client, MCP, permissions, and agent orchestration

## Current State

Already done:

- [x] Rust workspace split into `cli`, `core`, `api`, `mcp`, `tools`, `tui`
- [x] Basic CLI entrypoint and subcommand structure
- [x] Staged bootstrap flow (mode resolution + config/compat/session init)
- [x] Basic config loading
- [x] Compatibility discovery for settings, skills, and MCP config
- [x] Session persistence skeleton
- [x] Built-in prompt registry
- [x] Provider boundary in `clawedcode-api` with incremental streaming (`Provider` trait)
- [x] Streaming runtime plumbing (CLI streams deltas; TUI consumes event stream)
- [x] Permission engine scaffold (default/accept-edits/plan/bypass)
- [x] Built-in tool execution (shell/read_file/apply_patch) with tool_result persistence
- [x] Stateful REPL shell (basic TUI)
- [x] Source-backed implementation spec

Not done:

- [ ] MCP non-stdio transports
- [x] Session compaction / context trimming
- [ ] sub-agents
- [ ] remote/direct-connect/ssh modes

## Phase 1: Bootstrap And Modes

**Goal**: Replace the current simple `parse -> run` flow with the staged startup model the upstream app actually uses.

### Deliverables

- [ ] Early bootstrap pipeline before full command handling
- [ ] Interactive vs headless mode detection
- [ ] Entrypoint classification
- [ ] Early settings-affecting flag handling
- [ ] Migration/init hooks scaffold
- [ ] Clean execution mode dispatch

### Exit Criteria

```bash
cargo run -- config
cargo run -- compat
cargo run -- run --prompt "test" --json
```

All of those should go through the same staged bootstrap path, with mode detection separated from the command body.

## Phase 2: Session And Message Model

**Goal**: Promote the current placeholder session into the real runtime state model.

### Deliverables

- [ ] Structured message/content block model
- [ ] Session transcript persistence format
- [ ] Tool-use and tool-result message representation
- [ ] Thinking/event stream representation
- [ ] File history and content replacement model
- [ ] Resume-safe session identifiers and metadata

### Exit Criteria

```bash
cargo run -- run --prompt "hello"
```

Produces a persisted session transcript with enough structure to support:

- resume
- streaming
- tool execution
- transcript rendering

## Phase 3: Provider Client And Streaming Runtime

**Goal**: Make `clawedcode-api` the real model-facing subsystem instead of a mock response generator.

### Deliverables

- [x] Provider trait with streaming and non-streaming paths
- [x] Initial request shaping (system prompt + latest user message + tools metadata)
- [x] Full session-history shaping (multi-turn transcript)
- [x] Streaming event model
- [x] Retry and timeout envelope (scaffold)
- [x] Usage accounting scaffold
- [x] Model/provider abstraction (env-selected provider)

### Exit Criteria

```bash
cargo run -- run --prompt "explain ownership"
```

Should stream incremental deltas through the runtime (mock provider is acceptable for offline tests).

Optional real-provider smoke test (not used in CI):

```bash
export CLAWEDCODE_PROVIDER=anthropic
export ANTHROPIC_API_KEY=...
export CLAWEDCODE_ANTHROPIC_ENDPOINT=https://api.anthropic.com/v1/messages
cargo run -- run --prompt "hello"
```

## Phase 4: Tool Registry, Permissions, And Execution

**Goal**: Move from tool metadata to real tool behavior.

### Deliverables

- [x] Tool trait and execution context
- [x] Read/write/shell risk classification
- [x] Permission mode model: default, accept-edits, plan, bypass
- [x] Approval decision engine (headless: y/N prompt or `--yes`)
- [x] Built-in file and shell tool implementations
- [x] Tool result insertion back into the conversation loop (tool-call loop)

### Exit Criteria

```bash
cargo run -- run --prompt "read Cargo.toml and summarize it"
cargo run -- run --prompt "edit config to change the default theme"
```

The first should execute without unsafe prompts. The second should flow through the approval engine.

Headless auto-approval:

```bash
cargo run -- run --prompt "edit config to change the default theme" --yes
```

## Phase 5: Stateful REPL

**Goal**: Replace the placeholder TUI with a real evented REPL shell.

### Deliverables

- [ ] Central app state store
- [ ] Prompt input and submission flow
- [ ] Transcript rendering
- [ ] Streaming message updates
- [ ] Permission dialogs
- [ ] Status/footer and mode indicators
- [ ] Screen switching for prompt/transcript views

### Exit Criteria

```bash
cargo run
```

Should open an interactive REPL that can:

- accept prompts
- stream responses
- surface tool progress
- ask for permissions
- persist the session

## Phase 6: Session Resume, Memory, And Context Management

**Goal**: Build the persistence and long-context behavior needed for real use.

### Deliverables

- [ ] Session listing and resume
- [ ] Continue/fork session behavior
- [ ] `CLAUDE.md` layered memory loading
- [ ] `.claude/rules/*.md` discovery
- [ ] `@include` expansion and cycle protection
- [x] Context trimming/compaction strategy

### Exit Criteria

```bash
cargo run -- --resume
cargo run -- run --prompt "continue from earlier work"
```

The app should restore useful context instead of only loading raw transcript text.

## Phase 7: MCP Discovery And Connectivity

**Goal**: Make `clawedcode-mcp` a real subsystem, not just config parsing.

### Deliverables

- [x] Merge MCP config from settings and project files
- [ ] Connection state model
- [x] Stdio transport
- [ ] HTTP/SSE transport
- [x] Tool discovery
- [x] Resource discovery
- [x] MCP-backed helper tools for resource listing/reading

### Exit Criteria

```bash
cargo run -- compat
cargo run -- run --prompt "list available MCP tools"
```

The runtime should surface connected MCP tools and resources as part of the session.

Current status:

- stdio MCP servers are discovered from settings and ancestor `.mcp.json` files
- MCP tools are surfaced into the runtime as namespaced tools
- MCP stdio tool execution works end-to-end
- MCP resource discovery and helper tools are wired into the runtime
- non-stdio transports are still pending

## Phase 8: Commands, Skills, And Dynamic Command Surface

**Goal**: Support the command layer that sits between memory/config and the REPL UX.

### Deliverables

- [x] Slash-command registry for built-ins
- [ ] Skill-backed command discovery
- [ ] Dynamic command reload on file changes
- [ ] Command filtering by mode and environment
- [ ] Plugin-safe command registration boundaries

### Exit Criteria

Interactive REPL should expose commands from:

- built-ins
- discovered skills
- MCP servers where appropriate

Current status:

- built-in slash commands are available in the TUI (`/help`, `/clear`, `/update`)
- install-aware self-update is available through `clawedcode update` and `/update`
- skill-backed and dynamic command discovery are still pending

## Phase 9: Sub-Agents And Background Tasks

**Goal**: Recreate the nested-agent execution model that exists upstream.

### Deliverables

- [ ] Background task model
- [ ] Nested session runtime for sub-agents
- [ ] Parent/child transcript linkage
- [ ] Progress aggregation
- [ ] TUI views for task status and retained transcripts

### Exit Criteria

```bash
cargo run -- run --prompt "investigate two modules in parallel"
```

Should be able to spawn bounded child work items and merge their outputs back into the main session.

## Phase 10: Remote And Alternate Execution Modes

**Goal**: Add the non-local session entrypoints that the upstream bootstrap handles explicitly.

### Deliverables

- [ ] Headless/SDK mode parity
- [ ] Direct-connect session model
- [ ] SSH-backed remote session model
- [ ] Remote execution config passing
- [ ] Resume compatibility across execution modes

### Exit Criteria

Remote and headless paths should be separate execution modes with explicit runtime boundaries, not bolted onto the local REPL path.

## Phase 11: Hardening

**Goal**: Make the implementation production-grade.

### Deliverables

- [ ] Snapshot tests for transcripts and config resolution
- [ ] Integration tests for tool approval and execution loops
- [ ] Streaming/provider failure tests
- [ ] Session resume regression tests
- [ ] MCP transport and reconnection tests
- [ ] Performance checks for startup and render loops

### Exit Criteria

- `cargo test` covers the main runtime seams
- startup stays fast
- session resume is stable
- provider/tool/MCP failures are recoverable

## Ordering Rules

Do not change this sequence casually.

The correct dependency order is:

1. bootstrap
2. session model
3. provider streaming
4. tools and permissions
5. REPL
6. memory and resume
7. MCP
8. commands and skills
9. sub-agents
10. remote modes
11. hardening

That order reflects the upstream architecture. Building the TUI first or jumping straight to remote features would create rework.
