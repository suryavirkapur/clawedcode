# Architecture

## Goals

- Minimize startup cost by avoiding large dependency trees in the hot path.
- Keep orchestration, UI, tool dispatch, and persistence as separate layers.
- Treat prompts and policies as data, not scattered control flow.
- Make headless and interactive execution share the same runtime core.

## Proposed layers

### 1. CLI and process bootstrap

The binary should do three things quickly:

- resolve configuration
- set up logging and data directories
- choose between headless and interactive execution

This keeps startup predictable and leaves expensive work to later stages.

### 2. Runtime core

The runtime owns:

- session state
- message flow
- prompt selection
- tool availability
- turn limits and budgets

The key rule is that the runtime should not depend on terminal rendering. The TUI consumes runtime events rather than owning business logic.

### 3. Prompt registry

System behavior belongs in versioned prompt packs with explicit names and intended use. This makes it easier to audit behavior changes and swap operating modes without entangling UI code.

### 4. Tool execution

Tool definitions should describe:

- schema
- safety requirements
- side effects
- streaming capabilities

Execution should be routed through a central dispatcher so approval, logging, retries, and telemetry can stay consistent.

### 5. Persistence

Config and session data should be plain files with stable formats. JSON for transcripts and TOML for operator configuration is sufficient for the first iterations.

### 6. Terminal UI

The terminal path should use Ratatui for rendering and Crossterm for input. The UI should remain a thin shell around:

- transcript panes
- prompt editor
- approval dialogs
- task status
- tool activity

## Near-term build order

1. Replace the placeholder runtime response with provider adapters and streamed events.
2. Introduce a typed tool protocol and approval engine.
3. Expand the TUI into a full transcript and prompt workflow.
4. Add background task orchestration and structured logs.
