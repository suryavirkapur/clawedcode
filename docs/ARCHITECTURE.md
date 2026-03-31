# Architecture

## Design Principles

1. **Fast Startup** - Minimize dependency trees in hot path, lazy load modules
2. **Layer Separation** - Orchestration, UI, tool dispatch, and persistence as distinct layers
3. **Data-Driven Behavior** - Prompts and policies as versioned data, not scattered control flow
4. **Runtime Agnostic** - Headless and interactive execution share the same core

---

## Layer Overview

```
┌─────────────────────────────────────────────────────────────┐
│                      CLI Bootstrap                          │
│  Parse args → Resolve config → Init logger → Dispatch      │
├─────────────────────────────────────────────────────────────┤
│                      Runtime Core                            │
│  Session state, message flow, tool orchestration, budgets   │
├─────────────────────────────────────────────────────────────┤
│                     Prompt Registry                          │
│  Versioned system prompts loaded as data                    │
├─────────────────────────────────────────────────────────────┤
│                     Tool Execution                           │
│  Schema validation, permission checks, dispatch, streaming  │
├─────────────────────────────────────────────────────────────┤
│                      Persistence                             │
│  JSON transcripts, TOML config, file history                 │
├─────────────────────────────────────────────────────────────┤
│                      Terminal UI                             │
│  Flexbox layout, event handling, reactive rendering          │
└─────────────────────────────────────────────────────────────┘
```

---

## 1. CLI Bootstrap

Entry point responsibilities:

### 1.1 Argument Parsing

```
Subcommands:
├── tui          Interactive terminal interface (default)
├── run          Single query execution with --prompt
├── config       Print resolved configuration as JSON
└── compat       Print discovered assets (skills, MCP, settings)

Global Options:
├── --config <path>     Override config file location
├── --data-dir <path>   Override data directory
└── --cwd <path>        Working directory
```

### 1.2 Configuration Resolution

```
Config Priority (highest to lowest):
1. CLI --config argument
2. Project .claude/settings.local.json
3. Project .claude/settings.json
4. User ~/.claude/settings.json
5. Compiled defaults
```

### 1.3 Logger Initialization

```
Log Levels (configurable via env):
├── TRACE   Internal state transitions
├── DEBUG   Tool execution details
├── INFO    Session events
├── WARN    Recoverable errors
└── ERROR   Failures

Output: Structured logs with timestamps, targets, compact format
```

---

## 2. Runtime Core

Central orchestration layer. Ownsn ode flow, tool invocation, and context management.

### 2.1 Session Management

```
Session Lifecycle:
├── new(cwd)         Generate UUID, init message list
├── push(role, msg)  Append with timestamp
├── compact()        Summarize old messages, preserve context
└── save(dir)        Persist to JSON
```

### 2.2 Query Loop

```
QueryFlow:
    1. Receive user input
    2. Push to session
    3. Build request:
       ├── System prompt from registry
       ├── Message history (maybe compacted)
       ├── Available tools
       └── Token budget
    4. Stream API response:
       ├── Parse content blocks
       ├── Handle tool_use
       └── Handle thinking
    5. If tool_use blocks:
       ├── Parse input
       ├── Check permissions
       ├── Execute tool→ ToolResult
       ├── Push result to session
       └── Goto 3 (continue loop)
    6. Return final response
```

### 2.3 Permission System

```
Permission Flow:
┌──────────────────────────────────────────────────────────┐
│                    check_permissions()                    │
├──────────────────────────────────────────────────────────┤
│                                                          │
│  1. Rules Evaluation                                     │
│     ├── Check explicit deny rules                        │
│     └── Check explicit allow rules                       │
│                                                          │
│  2. Mode Behavior                                        │
│     ├── default: Ask every time                         │
│     ├── acceptEdits: Auto-approve file edits            │
│     ├── bypassPermissions: Allow all                    │
│     ├── plan: Read-only mode                             │
│     └── auto: ML classifier decides                      │
│                                                          │
│  3. If 'ask' behavior:                                   │
│     └── Show permission dialog → Get user decision       │
│                                                          │
│  4. Return: allow | deny | ask| passthrough              │
└──────────────────────────────────────────────────────────┘
```

---

## 3. Prompt Registry

System prompts as versioned, loadable data.

### 3.1 Directory Structure

```
prompts/
├── core.md          Default coding assistant
├── planning.md      Explicit execution plans
├── review.md        Focus on defects and tests
└── *.md             Custom user prompts
```

### 3.2 Loading

```
resolve_prompt(name?):
    1. If name specified, lookup by name
    2. Else use config default
    3. Fallback to first builtin
    4. Cache result
```

### 3.3 Format

```markdown
# System Prompt Content

Instructions for the AI model...
```

---

## 4. Tool Execution

Central dispatch with schema validation, permission checks, and progress streaming.

### 4.1 Tool Definition

```
Tool:
├── name: string
├── aliases: string[]
├── inputSchema: ZodSchema
├── description(input, opts) → string
├── prompt(opts) → string
├── isConcurrencySafe(input) → bool
├── isReadOnly(input) → bool
├── isDestructive(input?) → bool
├── checkPermissions(input, ctx) → PermissionResult
├── call(input, ctx, ...) → Promise<ToolResult>
├── renderToolUseMessage(input, opts) → RenderOutput
└── renderToolResultMessage(output, progress) → RenderOutput
```

### 4.2 Execution Pipeline

```
execute_tool(toolName, input, context):
    1.lookup_tool(toolName)
    2. validate_input(schema, input)
    3. run_pre_tool_use_hooks(toolName, input)
    4. result = check_permissions(input, context)
    5. switch result.behavior:
       ├── 'allow': → execute()
       ├── 'deny':  → return PermissionError
       ├── 'ask':   → show_dialog() → user decision
       └── 'passthrough': → forward to parent context
    6. execute_with_budget_tracking():
       ├── spawn task on thread pool
       ├── stream progress updates
       └── handle cancellation
    7. run_post_tool_use_hooks(toolName, input, result)
    8. return result
```

### 4.3 Concurrency

```
Concurrency Rules:
├── Non-concurrent tools: Execute serially
│   ├── shell (subprocesses may conflict)
│   ├── file_write (file locks)
│   └── file_edit (file locks)
│
└── Concurrent-safe tools: Execute in parallel
    ├── file_read (read-only)
    ├── glob (read-only)
    ├── grep (read-only)
    └── web_fetch (independent network)
```

---

## 5. Persistence

### 5.1 Session Storage

```
sessions/
├── <uuid>.json      Sessiontranscript
└── <uuid>.json

Format:
{
    "id": "uuid",
    "cwd": "/path",
    "createdAt": "ISO8601",
    "updatedAt": "ISO8601",
    "messages": [...]
}
```

### 5.2 Configuration

```
XDG Paths:
├── config: ~/.config/<app>/config.toml
├── data: ~/.local/share/<app>/
│   ├── sessions/
│   └── file_history/
└── state: ~/.local/state/<app>/

Config Layers (merge in order):
1. User global   ~/.claude/settings.json
2. Project       <project>/.claude/settings.json
3. Local         <project>/.claude/settings.local.json
```

### 5.3 File History

```
file_history/
└── <path_hash>/
    └── snapshots/
        ├── 001.json
        ├── 002.json
        └── ...

Snapshot:
{
    "content": "file contents",
    "timestamp": "ISO8601",
    "sourceTool": "file_edit"
}    
```

---

## 6. Terminal UI

Declarative UI with flexbox layout and reactive state.

### 6.1 Widget Hierarchy

```
App
├── Header (status bar)
│   ├── ModelIndicator
│   ├── SessionInfo
│   └── ModeBadge
│
├── Transcript (scrollable)
│   ├── UserMessage
│   ├── AssistantMessage
│   │├── TextBlock
│   │   ├── ToolUseBlock
│   │   └── ThinkingBlock
│   ├── ToolResultMessage
│   ├── ProgressMessage
│   └── SystemMessage
│
├── InputArea
│   ├── PromptEditor
│   ├── AttachmentIndicator
│   └── VimModeIndicator
│
└── StatusBar
    ├── TokenCount
    ├── CostEstimate
    └── ModeDisplay
```

### 6.2 State Shape

```
struct UIState:
    transcript: TranscriptEntry[]
    inputBuffer: string
    cursorPosition: number
    mode: UIMode           // normal | insert | visual | command
    focus: FocusTarget     // input | transcript | dialog
    scrollOffset: number
    vimState: VimState?
    attachments: Attachment[]
    dialog: DialogState?
```

### 6.3 Rendering Pipeline

```
render_frame():
    1. Calculate layout (flexbox solver)
    2. Diff virtual DOM
    3. Compute minimal ANSI updates
    4. Write to screen buffer
    5. Flush to terminal
```

### 6.4 Event Handling

```
Event Loop:
    loop:
        event = wait_for_event()
        match event:
        ├── KeyEvent(key) → handle_key(key)
        ├── MouseEvent(mouse) → handle_mouse(mouse)
        ├── ResizeEvent(size) → handle_resize(size)
        ├── FocusEvent(focused) → handle_focus(focused)
        └── ToolEvent(progress) → handle_progress(progress)
        
        state = update(state, action)
        render(state)
```

---

## 7. MCP Integration

Model Context Protocol for external tools.

### 7.1 Transport Types

```
McpTransport:
├── Stdio      Subprocess with JSON-RPC on stdin/stdout
├── Http       HTTP POST to URL
├── Sse        Server-Sent Events
├── WebSocket  Full-duplex WebSocket
└── Sdk        In-process (built-in)
```

### 7.2 Lifecycle

```
MCP Server Lifecycle:
    1. discover() → List MCP configs from settings
    2. connect() → Spawn process or open connection
    3. initialize() → Handshake, query capabilities
    4. register() → Add tools to registry
    5. serve() → Handle tool calls
    6. cleanup() → Close connection on shutdown
```

### 7.3 Tool Mapping

```
MCP Tool → Native Tool:
├── name: string (prefixed with mcp_<server>_)
├── inputSchema: JSON Schema → ZodSchema
├── handler: Forward to MCP server
└── output: Parse MCP result
```

---

## 8. Agent / Task System

Background work execution.

### 8.1 Task Types

```
Task:
├── LocalShellTask      Execute shell command in background
├── LocalAgentTask      Spawn subagent with subset of tools
├── RemoteAgentTask     Execute on remote CCR instance
├── InProcessTeammate   Concurrent agent in same process
└── DreamTask           Auto-memory consolidation
```

### 8.2 Task Execution

```
execute_task(task):
    1. Validate parameters
    2. Check preconditions
    3. Spawn executor:
       ├── Shell: subprocess with timeout
       ├── Agent: new session context
       └── Remote: HTTP/WebSocket
    4. Stream progress events
    5. Collect result
    6. Update tracking state
```

---

## 9. Error Handling

### 9.1 Error Categories

```
AgentError:
├── ConfigError         Invalid configuration
├── PermissionError     Tool permission denied
├── ToolError           Tool execution failed
├── ApiError            API request failed
│   ├── RateLimit       Retry with backoff
│   ├── MaxTokens       Compact and retry
│   ├── InvalidRequest  Fix and retry
│   ├── AuthFailed      Re-authenticate
│   └── Timeout         Retry with extension
├── IoError             Filesystem error
└── InternalError        Bug or inconsistency
```

### 9.2 Recovery

```
Error Recovery:
├── RateLimit: Exponential backoff, max 3 retries
├── MaxTokens: Auto-compact, retry once
├── Timeout: Double timeout, retry once
├── ToolError: Report to user, continue session
└── Fatal: Log, cleanup, exit with code
```

---

## 10. Performance Targets

```
Startup:
├── Binary launch: < 50ms
├── Config resolution: < 10ms
├── First render: < 100ms
└── Ready for input: < 200ms

Runtime:
├── Key response: < 16ms (60fps)
├── Tool dispatch: < 5ms
├── State update: < 1ms
└── Render cycle: < 10ms

Memory:
├── Base memory: < 50MB
├── Per-session: < 10MB
└── Large files: Stream, don't buffer
```

---

## Implementation Phases

### Phase 1: Foundation
- CLI argument parsing
- Config discovery and merging
- Session persistence
- Basic prompt registry

### Phase 2: Core Runtime
- Message types and flow
- Query loop
- Tool interface definition
- Permission system

### Phase 3: Essential Tools
- File read/write/edit
- Shell execution
- Glob/grep search
- Ask user

### Phase 4: Terminal UI
- Flexbox layout engine
- Event handling
- Transcript rendering
- Input handling

### Phase 5: Advanced Features
- MCP integration
- Vim mode
- Agent spawning
- Background tasks