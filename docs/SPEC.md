# ClawedCode Reimplementation Spec

This document is the implementation target for the Rust-native rewrite. It is based on:

- local source inspection of `claude-code-src/main.tsx`
- local source inspection of `claude-code-src/screens/REPL.tsx`
- local source inspection of `claude-code-src/services/api/claude.ts`
- local source inspection of `claude-code-src/services/mcp/client.ts`
- public Claude Code docs

It is intentionally about the target system, not the current small Rust placeholder.

## Monorepo shape

Use a Rust workspace with these crates:

- `clawedcode-cli`: process bootstrap, CLI flags, mode dispatch
- `clawedcode-core`: session model, config, compatibility discovery, orchestration
- `clawedcode-api`: model/provider abstraction, request shaping, streaming surface
- `clawedcode-mcp`: MCP config model, connection orchestration, tool/resource adaptation
- `clawedcode-tools`: built-in tool registry, schemas, approval metadata
- `clawedcode-tui`: terminal rendering, input, transcript, dialogs

Rules:

- `cli` depends on `core`, `tui`
- `core` depends on `api`, `mcp`, `tools`
- `tui` depends on `core`, `tools`, `mcp` once the real UI is ported
- `api` must not depend on `tui`
- `mcp` must not depend on `tui`
- tool implementations that need shell/fs/network stay outside `tui`

## Upstream architecture map

The upstream codebase is not a simple CLI binary. The large files show these dominant seams:

- `main.tsx`: bootstrap, flags, mode selection, migrations, prefetch, session restore, REPL launch
- `screens/REPL.tsx`: long-lived app shell, transcript state, prompt flow, tool streaming, notifications, plugins, MCP/UI integration
- `services/api/claude.ts`: request construction, prompt caching, streaming/non-streaming execution, thinking/tool-use handling, usage accounting
- `services/mcp/client.ts`: connection management, auth caching, batching, tool/resource/command discovery, tool result transformation

That means the Rust port should treat the app as:

1. a bootstrap pipeline
2. a long-lived evented session runtime
3. a provider client with streaming blocks
4. an MCP integration subsystem
5. a terminal application shell

## Boot flow to reproduce

From `main.tsx`, the startup model is:

1. run very early side effects and prefetches
2. normalize argv before full command parsing
3. detect interactive vs print/sdk mode
4. assign an entrypoint/client type
5. eagerly load settings-affecting flags before full init
6. run one-time init and migrations in a pre-action phase
7. resolve the execution path:
   - fresh interactive session
   - print/headless session
   - resume session
   - remote/direct-connect/ssh session
   - special subcommands
8. construct initial app state
9. launch REPL with commands, tools, MCP clients, messages, and session config

Rust implication:

- `clawedcode-cli` needs a staged bootstrap, not just `Cli::parse() -> run()`
- some flags must affect initialization before the rest of the app is built
- remote/resume/headless must be first-class execution modes, not ad hoc options

## REPL responsibilities

From `REPL.tsx`, the REPL is not just a text box and transcript. It owns or coordinates:

- prompt screen vs transcript screen
- app-state subscriptions
- dynamic tool list composition
- dynamic command list composition
- MCP client merge and status
- plugin startup checks
- skill-triggered command reloads
- IDE integration state
- notifications and callouts
- background task and agent transcript views
- streaming tool-use and thinking state
- remote session overlays
- transcript search/render modes

Rust implication:

- the TUI must be built around a central store plus event reducers
- rendering and orchestration must be separated
- prompt submission must be asynchronous and event-driven
- transcript, notifications, tasks, and permission prompts need dedicated components/models

## Model/API subsystem

From `services/api/claude.ts`, the provider layer does more than send prompts:

- translates internal messages into API wire format
- supports streaming and non-streaming modes
- handles tool-use blocks and tool-result pairing
- supports thinking/effort/task-budget controls
- handles prompt caching and cache-control breakpoints
- computes usage/cost accounting
- manages retries/fallbacks/timeouts
- injects metadata, betas, model-specific headers, and provider-specific params

Rust target interfaces:

```rust
trait ApiClient {
    fn complete_stream(&self, request: CompletionRequest) -> impl Stream<Item = ApiEvent>;
    fn complete_once(&self, request: CompletionRequest) -> Result<CompletionResponse>;
}

enum ApiEvent {
    MessageDelta(String),
    ThinkingDelta(String),
    ToolUse(ToolCall),
    Usage(UsageDelta),
    Completed(CompletionResponse),
    Failed(ApiError),
}
```

Required first-pass features:

- request shaping from session history
- streaming text deltas
- structured tool-use events
- timeout and retry envelope
- model/provider abstraction

Defer until later:

- prompt caching
- advanced beta headers
- quota/telemetry/cost analytics
- fallback models

## MCP subsystem

From `services/mcp/client.ts`, MCP is a primary extension surface, not an afterthought. The upstream client:

- supports multiple transports: stdio, SSE, HTTP, WebSocket, SDK/control
- batches connections with different concurrency for local vs remote servers
- caches auth failures and session expiry
- lists tools, commands, resources, and skills
- exposes resource helper tools (`list`, `read`) when resources are available
- transforms large/binary result payloads before handing them to the model
- reconnects and retries on session expiry

Rust target interfaces:

```rust
struct McpRegistry {
    servers: BTreeMap<String, McpServerConfig>,
}

struct ConnectedServer {
    name: String,
    state: McpConnectionState,
    tools: Vec<ToolSpec>,
    commands: Vec<SlashCommand>,
    resources: Vec<ResourceRef>,
}
```

First-pass scope:

- config parsing and merge
- connection state model
- tool/command/resource discovery
- stable namespaced tool identity

Later scope:

- OAuth/auth retry
- binary/blob persistence helpers
- MCP skill extraction
- IDE-specific MCP affordances

## Memory, settings, and context rules

Important external behavior from the docs:

- `CLAUDE.md` is layered memory, loaded from system, user, project, and local scopes
- files closer to the current working directory have higher effective priority
- `.claude/rules/*.md` are modular memory files
- `@include` supports relative, home, and absolute paths, with recursion limits and circular protection
- some memory files can be path-targeted via frontmatter globs
- settings and environment variables both shape startup behavior
- `CLAUDE_CONFIG_DIR` relocates the whole config/state root
- bare mode disables a large amount of background/discovery work

Rust target:

- keep compatibility discovery in `core`
- move MCP config types into `mcp`
- later add a dedicated memory loader module with:
  - layered discovery
  - include expansion
  - path filtering
  - exclusion rules

## Permission model to preserve

Important external behavior from the docs:

- read-only operations auto-approve in the default mode
- shell, file writes, and networked/MCP side effects require stronger checks
- `acceptEdits` auto-approves edits but not arbitrary shell
- `plan` is explicitly read-only and blocks mutating operations
- there is an escape path from planning into execution
- there is a full bypass mode

Rust target types:

```rust
enum PermissionMode {
    Default,
    AcceptEdits,
    Plan,
    Bypass,
}

enum PermissionDecision {
    Allow,
    Deny { reason: String },
    Ask { prompt: String },
}
```

The approval engine belongs in `core`, while each tool contributes risk metadata from `tools`.

## Tool system to preserve

Important external behavior from the docs:

- core tool classes include file reads/edits, shell execution, web access, search, and task/sub-agent work
- MCP tools are surfaced alongside built-ins
- tool availability changes with mode, settings, permissions, and connected servers

Rust target:

- `clawedcode-tools` owns:
  - built-in tool metadata
  - input/output schemas
  - approval classification
  - execution adapters
- `core` owns:
  - tool selection for a turn
  - permission evaluation
  - tool result insertion into the session

## Multi-agent target

Important external behavior from the docs and source:

- sub-agents are a first-class workflow primitive
- they run as bounded work items with isolated context
- the main agent remains the orchestrator
- agent sessions can retain and later reload transcripts

Rust target:

- represent sub-agents as nested session runtimes, not just async tasks
- keep transcript persistence independent from the TUI
- define an agent protocol now, even if the first implementation is local-only

## Concrete port order

Do not port the biggest upstream files line-for-line.

Vertical slices:

1. bootstrap + headless mode
2. session store + resume
3. streaming provider client
4. tool execution and permissions
5. REPL event loop
6. MCP discovery and tool surfacing
7. sub-agent orchestration
8. remote/ssh/direct-connect modes

## What changed in the workspace now

The workspace now already reflects the target seams:

- `clawedcode-tools` owns built-in tool metadata
- `clawedcode-mcp` owns MCP server config types and settings-level discovery
- `clawedcode-api` owns the provider boundary, with a mock client for now
- `clawedcode-core` orchestrates sessions and delegates to those crates

That split is intentional: more source can land without collapsing back into a single crate.
