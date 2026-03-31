# Roadmap

## Phase 1

- Establish the Rust crate and repository hygiene.
- Define stable modules for config, prompts, tools, sessions, and UI.
- Keep the interactive shell compiling from day one.

## Phase 2

- Add provider adapters with streamed token output.
- Implement conversation turns against a reusable runtime engine.
- Persist transcripts and session metadata cleanly.

## Phase 3

- Add structured tool dispatch for shell, patching, planning, and background tasks.
- Build approval flows that work both in headless and interactive modes.
- Add rich terminal views for tool activity and task state.

## Phase 4

- Introduce benchmarks for startup latency and steady-state responsiveness.
- Profile allocation hotspots in the runtime and renderer.
- Optimize transcript handling for long sessions.
