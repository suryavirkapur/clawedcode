use anyhow::Result;
use clawedcode_api::ApiEvent;
use clawedcode_core::background_task::{TaskStatus as ShellTaskStatus, list_background_tasks};
use clawedcode_core::compat::SkillDescriptor;
use clawedcode_core::content::ContentBlock;
use clawedcode_core::interactive::{ApprovalRequest, TuiContext, TuiEvent, TuiHandler};
use clawedcode_core::onboarding::{
    increment_project_onboarding_seen_count, maybe_mark_project_onboarding_complete,
    onboarding_steps, should_show_project_onboarding,
};
use clawedcode_core::session::{
    Message, Role, Session, SessionMode, TRANSPORT_SESSION_ID_MARKER_PREFIX,
    transport_session_id_marker,
};
use clawedcode_core::subagent::{
    SubAgentTaskState, SubAgentTaskStatus, list_subagent_tasks_for_parent,
};
use clawedcode_core::tool_input::decode_tool_input;
use clawedcode_core::update::InstallMethod;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
    },
};
use ratatui::{
    DefaultTerminal,
    prelude::*,
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};
use std::{
    cmp::Reverse,
    env, fs,
    io::{self, stdout},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const ACCENT: Color = Color::Rgb(215, 119, 87);
const MUTED: Color = Color::Rgb(150, 150, 150);
const DIM: Color = Color::Rgb(153, 153, 153);
const SUBTLE: Color = Color::Rgb(80, 80, 80);
const ERR_FG: Color = Color::Rgb(255, 107, 128);
const WARN_FG: Color = Color::Rgb(255, 193, 7);
const OK_FG: Color = Color::Rgb(78, 186, 101);
const PERMISSION_FG: Color = Color::Rgb(177, 185, 249);
const AUTO_ACCEPT_FG: Color = Color::Rgb(175, 135, 255);
const PLAN_MODE_FG: Color = Color::Rgb(72, 150, 140);
const PROMPT_BORDER: Color = Color::Rgb(136, 136, 136);
const USER_MSG_BG: Color = Color::Rgb(55, 55, 55);
const DASHBOARD_HEIGHT: u16 = 16;
const BULLET: &str = "⏺";
const HOOK_PREFIX: &str = "  ⎿  ";
const HOOK_CONTINUATION: &str = "     ";
const POINTER: &str = "❯";
const MAX_VISIBLE_QUEUED_PROMPTS: usize = 3;
const SPINNER_FRAME_MS: u128 = 120;
const SPINNER_VERB_MS: u128 = 2_500;

const SPINNER_FRAMES: &[char] = &['·', '✢', '✳', '✶', '✻', '✽', '✻', '✶', '✳', '✢'];
const SPINNER_VERBS: &[&str] = &[
    "Accomplishing",
    "Actioning",
    "Actualizing",
    "Architecting",
    "Baking",
    "Beaming",
    "Beboppin'",
    "Befuddling",
    "Billowing",
    "Blanching",
    "Bloviating",
    "Boogieing",
    "Boondoggling",
    "Booping",
    "Bootstrapping",
    "Brewing",
    "Bunning",
    "Burrowing",
    "Calculating",
    "Canoodling",
    "Caramelizing",
    "Cascading",
    "Catapulting",
    "Cerebrating",
    "Channeling",
    "Channelling",
    "Choreographing",
    "Churning",
    "Clauding",
    "Coalescing",
    "Cogitating",
    "Combobulating",
    "Composing",
    "Computing",
    "Concocting",
    "Considering",
    "Contemplating",
    "Cooking",
    "Crafting",
    "Creating",
    "Crunching",
    "Crystallizing",
    "Cultivating",
    "Deciphering",
    "Deliberating",
    "Determining",
    "Dilly-dallying",
    "Discombobulating",
    "Doing",
    "Doodling",
    "Drizzling",
    "Ebbing",
    "Effecting",
    "Elucidating",
    "Embellishing",
    "Enchanting",
    "Envisioning",
    "Evaporating",
    "Fermenting",
    "Fiddle-faddling",
    "Finagling",
    "Flambéing",
    "Flibbertigibbeting",
    "Flowing",
    "Flummoxing",
    "Fluttering",
    "Forging",
    "Forming",
    "Frolicking",
    "Frosting",
    "Gallivanting",
    "Galloping",
    "Garnishing",
    "Generating",
    "Gesticulating",
    "Germinating",
    "Gitifying",
    "Grooving",
    "Gusting",
    "Harmonizing",
    "Hashing",
    "Hatching",
    "Herding",
    "Honking",
    "Hullaballooing",
    "Hyperspacing",
    "Ideating",
    "Imagining",
    "Improvising",
    "Incubating",
    "Inferring",
    "Infusing",
    "Ionizing",
    "Jitterbugging",
    "Julienning",
    "Kneading",
    "Leavening",
    "Levitating",
    "Lollygagging",
    "Manifesting",
    "Marinating",
    "Meandering",
    "Metamorphosing",
    "Misting",
    "Moonwalking",
    "Moseying",
    "Mulling",
    "Mustering",
    "Musing",
    "Nebulizing",
    "Nesting",
    "Newspapering",
    "Noodling",
    "Nucleating",
    "Orbiting",
    "Orchestrating",
    "Osmosing",
    "Perambulating",
    "Percolating",
    "Perusing",
    "Philosophising",
    "Photosynthesizing",
    "Pollinating",
    "Pondering",
    "Pontificating",
    "Pouncing",
    "Precipitating",
    "Prestidigitating",
    "Processing",
    "Proofing",
    "Propagating",
    "Puttering",
    "Puzzling",
    "Quantumizing",
    "Razzle-dazzling",
    "Razzmatazzing",
    "Recombobulating",
    "Reticulating",
    "Roosting",
    "Ruminating",
    "Sautéing",
    "Scampering",
    "Schlepping",
    "Scurrying",
    "Seasoning",
    "Shenaniganing",
    "Shimmying",
    "Simmering",
    "Skedaddling",
    "Sketching",
    "Slithering",
    "Smooshing",
    "Sock-hopping",
    "Spelunking",
    "Spinning",
    "Sprouting",
    "Stewing",
    "Sublimating",
    "Swirling",
    "Swooping",
    "Symbioting",
    "Synthesizing",
    "Tempering",
    "Thinking",
    "Thoughting",
    "Thundering",
    "Tidying",
    "Tinkering",
    "Tomfooling",
    "Topologizing",
    "Transfiguring",
    "Transmuting",
    "Twisting",
    "Unfurling",
    "Unraveling",
    "Unzipping",
    "Upserting",
    "Vibing",
    "Waltzing",
    "Whirring",
    "Whisking",
    "Wibbling",
    "Wringing",
    "Zesting",
    "Zigzagging",
    "Zooming",
];

const CLAUDE_LOGO: &[&str] = &[" ▐▛███▜▌ ", "▝▜█████▛▘", "  ▘▘ ▝▝  "];

const BUILTIN_COMMANDS: &[&str] = &[
    "/help",
    "/clear",
    "/update",
    "/task",
    "/tasks",
    "/sessions",
    "/fork",
    "/tools",
];

fn is_reserved_command(name: &str) -> bool {
    BUILTIN_COMMANDS.contains(&name)
}

fn is_valid_command_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('/') else {
        return false;
    };
    if rest.is_empty() {
        return false;
    }
    rest.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn update_is_supported(install_method: InstallMethod) -> bool {
    matches!(install_method, InstallMethod::Cargo | InstallMethod::Npm)
}

fn command_is_visible(entry: &CommandEntry, install_method: InstallMethod) -> bool {
    match entry.name.as_str() {
        "/update" => update_is_supported(install_method),
        _ => true,
    }
}

fn skill_is_registerable(skill: &SkillDescriptor) -> bool {
    if !skill.body.trim().is_empty()
        && is_valid_command_name(&skill.slash_command)
        && !is_reserved_command(&skill.slash_command)
    {
        return true;
    }
    false
}

pub fn run_with_context(mut ctx: TuiContext) -> Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, SetTitle("ClawedCode"))?;
    let terminal = ratatui::init();
    let result = run_loop(terminal, &mut ctx);
    restore_terminal()?;
    let _ = ctx.save_session();
    result
}

#[derive(Debug, Clone)]
enum AppState {
    Idle,
    AwaitingApproval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandSource {
    BuiltIn,
    Skill,
}

#[derive(Debug, Clone)]
struct CommandEntry {
    name: String,
    description: String,
    source: CommandSource,
}

#[derive(Debug, Clone)]
struct SavedSessionSummary {
    id: String,
    cwd: PathBuf,
    updated_at: i64,
    mode: SessionMode,
    preview: String,
    is_current: bool,
}

#[derive(Debug, Clone)]
enum LiveToolEventState {
    Use {
        name: String,
        input: serde_json::Value,
    },
    PendingApproval,
    Approved,
    Denied,
    Result {
        content: String,
        is_error: bool,
    },
}

#[derive(Debug, Clone)]
struct LiveToolEntry {
    tool_use_id: String,
    events: Vec<LiveToolEventState>,
}

#[derive(Debug, Clone)]
struct InfoPanel {
    title: String,
    status: Option<String>,
    lines: Vec<String>,
}

#[derive(Debug, Clone)]
enum RenderRow {
    User(String),
    Assistant(String),
    Thinking(String),
    ToolUse {
        name: String,
        input: String,
    },
    ToolStatus(String),
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    SubagentSummary {
        child_session_id: String,
        summary: String,
    },
    Info(String),
    Spacer,
}

fn info_panel_hint_line() -> &'static str {
    "Esc/q/? close · ↑/↓ or j/k scroll"
}

struct ReplHandler {
    transcript_rows: Vec<RenderRow>,
    overlay_rows: Vec<RenderRow>,
    live_user_prompt: Option<String>,
    live_assistant_text: String,
    live_thinking: String,
    live_tools: Vec<LiveToolEntry>,
    info_panel: Option<InfoPanel>,
    show_thinking: bool,
    state: AppState,
    expanded_tool_results: std::collections::HashSet<String>,
}

impl ReplHandler {
    fn new(show_thinking: bool) -> Self {
        Self {
            transcript_rows: Vec::new(),
            overlay_rows: Vec::new(),
            live_user_prompt: None,
            live_assistant_text: String::new(),
            live_thinking: String::new(),
            live_tools: Vec::new(),
            info_panel: None,
            show_thinking,
            state: AppState::Idle,
            expanded_tool_results: std::collections::HashSet::new(),
        }
    }

    fn rebuild_from_session(&mut self, session: &Session, show_thinking: bool) {
        self.transcript_rows.clear();
        let mut turn = TranscriptTurn::default();
        for msg in &session.messages {
            if msg.role == Role::User && !turn.rows.is_empty() {
                flush_turn_rows(&mut self.transcript_rows, &mut turn);
            }
            append_message_to_turn(&mut turn, msg, show_thinking);
        }
        flush_turn_rows(&mut self.transcript_rows, &mut turn);
    }

    fn visible_lines(&self) -> Vec<String> {
        self.visible_rows()
            .into_iter()
            .map(|row| row_text(&row))
            .collect()
    }

    fn visible_rows(&self) -> Vec<RenderRow> {
        let mut rows = self.transcript_rows.clone();
        rows.extend(self.live_rows());
        rows.extend(self.overlay_rows.iter().cloned());
        rows
    }

    fn has_content(&self) -> bool {
        !self.transcript_rows.is_empty()
            || !self.overlay_rows.is_empty()
            || self.live_user_prompt.is_some()
            || !self.live_assistant_text.is_empty()
            || !self.live_thinking.is_empty()
            || !self.live_tools.is_empty()
    }

    fn push_overlay(&mut self, line: impl Into<String>) {
        self.overlay_rows.push(RenderRow::Info(line.into()));
    }

    fn show_panel(&mut self, title: impl Into<String>, lines: Vec<String>) {
        self.info_panel = Some(InfoPanel {
            title: title.into(),
            status: None,
            lines,
        });
    }

    fn clear_panel(&mut self) {
        self.info_panel = None;
    }

    fn begin_live_turn(&mut self, prompt: impl Into<String>) {
        self.clear_live_turn();
        self.live_user_prompt = Some(prompt.into());
    }

    fn clear_live_turn(&mut self) {
        self.live_user_prompt = None;
        self.live_assistant_text.clear();
        self.live_thinking.clear();
        self.live_tools.clear();
    }

    fn live_rows(&self) -> Vec<RenderRow> {
        let mut rows = Vec::new();
        let turn_is_open = self.live_user_prompt.is_some()
            || !self.live_assistant_text.trim().is_empty()
            || !self.live_tools.is_empty();

        if let Some(prompt) = &self.live_user_prompt {
            rows.push(RenderRow::User(prompt.clone()));
            rows.push(RenderRow::Spacer);
        }

        if self.show_thinking {
            let thinking = self.live_thinking.trim();
            if !thinking.is_empty() {
                rows.push(RenderRow::Thinking(thinking.to_string()));
            }
        }

        if turn_is_open {
            let assistant = self.live_assistant_text.trim();
            if !assistant.is_empty() {
                rows.push(RenderRow::Assistant(assistant.to_string()));
            }
        }

        for entry in &self.live_tools {
            rows.extend(render_live_tool_entry(entry));
        }

        while matches!(rows.last(), Some(RenderRow::Spacer)) {
            rows.pop();
        }

        rows
    }

    fn record_live_tool_use(
        &mut self,
        tool_use_id: impl Into<String>,
        name: impl Into<String>,
        input: serde_json::Value,
    ) {
        let tool_use_id = tool_use_id.into();
        let entry = self.live_tool_entry_mut(&tool_use_id);
        entry.events.push(LiveToolEventState::Use {
            name: name.into(),
            input,
        });
    }

    fn record_live_tool_pending_approval(&mut self, tool_use_id: &str) {
        self.live_tool_entry_mut(tool_use_id)
            .events
            .push(LiveToolEventState::PendingApproval);
    }

    fn record_live_tool_approval_decision(&mut self, tool_use_id: &str, approved: bool) {
        self.live_tool_entry_mut(tool_use_id)
            .events
            .push(if approved {
                LiveToolEventState::Approved
            } else {
                LiveToolEventState::Denied
            });
    }

    fn record_live_tool_result(&mut self, tool_use_id: &str, content: String, is_error: bool) {
        self.live_tool_entry_mut(tool_use_id)
            .events
            .push(LiveToolEventState::Result { content, is_error });
    }

    fn live_tool_entry_mut(&mut self, tool_use_id: &str) -> &mut LiveToolEntry {
        if let Some(index) = self
            .live_tools
            .iter()
            .position(|entry| entry.tool_use_id == tool_use_id)
        {
            return &mut self.live_tools[index];
        }

        self.live_tools.push(LiveToolEntry {
            tool_use_id: tool_use_id.to_string(),
            events: Vec::new(),
        });
        self.live_tools
            .last_mut()
            .expect("just pushed live tool entry")
    }
}

fn render_live_tool_entry(entry: &LiveToolEntry) -> Vec<RenderRow> {
    let mut rows = Vec::new();

    for event in &entry.events {
        match event {
            LiveToolEventState::Use { name, input } => {
                rows.push(RenderRow::ToolUse {
                    name: name.clone(),
                    input: serde_json::to_string(input).unwrap_or_default(),
                });
            }
            LiveToolEventState::PendingApproval => {
                rows.push(RenderRow::ToolStatus("Awaiting approval…".to_string()));
            }
            LiveToolEventState::Approved => {
                rows.push(RenderRow::ToolStatus("Approved".to_string()));
            }
            LiveToolEventState::Denied => {
                rows.push(RenderRow::ToolStatus("Denied by user".to_string()));
            }
            LiveToolEventState::Result { content, is_error } => {
                rows.push(RenderRow::ToolResult {
                    tool_use_id: entry.tool_use_id.clone(),
                    content: content.clone(),
                    is_error: *is_error,
                });
            }
        }
    }

    rows
}

impl TuiHandler for ReplHandler {
    fn on_event(&mut self, event: &TuiEvent) {
        match event {
            TuiEvent::ThinkingDelta { text } => {
                self.live_thinking.push_str(text);
            }
            TuiEvent::MessageDelta { text } => {
                self.live_assistant_text.push_str(text);
            }
            TuiEvent::ToolUse { id, name, input } => {
                self.record_live_tool_use(id.clone(), name.clone(), input.clone());
            }
            TuiEvent::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                self.record_live_tool_result(tool_use_id, content.clone(), *is_error);
            }
            TuiEvent::AssistantDone | TuiEvent::TurnComplete => {
                self.state = AppState::Idle;
            }
        }
    }

    fn request_approval(&mut self, request: &ApprovalRequest) -> bool {
        self.push_overlay(format!(
            "[info] Tool '{}' requires approval. Input: {}",
            request.tool_name,
            serde_json::to_string(&request.input).unwrap_or_default()
        ));
        self.state = AppState::AwaitingApproval;
        false
    }
}

enum TurnWorkerEvent {
    Ui(TuiEvent),
    ApprovalRequested(ApprovalRequest),
    Finished(Session),
    Failed(String),
}

struct ActiveTurn {
    events: mpsc::Receiver<TurnWorkerEvent>,
    approvals: mpsc::Sender<bool>,
    _join: thread::JoinHandle<()>,
}

#[derive(Debug, Clone, Default)]
struct TranscriptViewport {
    scroll_offset: usize,
    pinned_to_bottom: bool,
    unseen_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplScreen {
    Main,
    Transcript,
}

#[derive(Debug, Clone, Default)]
struct TranscriptModeState {
    frozen_rows: Vec<RenderRow>,
    viewport: TranscriptViewport,
    dump_mode: bool,
    export_status: Option<String>,
    search_open: bool,
    search_input: String,
    search_query: String,
    search_matches: Vec<usize>,
    search_index: usize,
}

fn run_loop(mut terminal: DefaultTerminal, ctx: &mut TuiContext) -> Result<()> {
    let mut handler = ReplHandler::new(ctx.show_thinking);
    handler.rebuild_from_session(ctx.session(), ctx.show_thinking);
    maybe_mark_project_onboarding_complete(&ctx.session().cwd);

    let mut input_buffer = String::new();
    let mut cursor_pos: usize = 0;
    let mut screen = ReplScreen::Main;
    let mut viewport = TranscriptViewport {
        pinned_to_bottom: true,
        ..TranscriptViewport::default()
    };
    let mut transcript_mode = TranscriptModeState::default();
    let mut queued_prompts: Vec<String> = Vec::new();
    let mut awaiting_approval: Option<ApprovalRequest> = None;
    let mut approval_selection: usize = 0;
    let mut active_turn: Option<ActiveTurn> = None;
    let mut info_panel_scroll: usize = 0;
    let mut last_area = Rect::default();
    let mut last_body_height: usize = 0;
    let mut onboarding_seen_recorded = false;
    let mut last_visible_line_count = handler.visible_lines().len();
    let mut turn_started_at: Option<std::time::Instant> = None;
    let mut palette_index: usize = 0;
    let mut file_index: Option<Vec<String>> = None;
    let mut palette_touched = false;
    let mut history: Vec<String> = Vec::new();
    let mut history_index: Option<usize> = None;
    let mut history_stash = String::new();
    let mut history_search: Option<(String, Option<usize>)> = None;
    let mut history_search_stash = String::new();
    let mut last_esc: Option<std::time::Instant> = None;
    let mut last_ctrl_c: Option<std::time::Instant> = None;
    let mut exit_hint: Option<std::time::Instant> = None;
    let mut last_approval_id: Option<String> = None;
    let always_allowed: Arc<Mutex<std::collections::HashSet<String>>> =
        Arc::new(Mutex::new(std::collections::HashSet::new()));

    if let Some(prompt) = ctx.take_startup_prompt() {
        handler.begin_live_turn(prompt.clone());
        turn_started_at = Some(std::time::Instant::now());
        active_turn = Some(spawn_turn_worker(ctx, prompt, None, always_allowed.clone()));
    }

    loop {
        integrate_completed_subagents(ctx, &mut handler);
        drain_turn_events(ctx, &mut handler, &mut active_turn, &mut awaiting_approval);
        maybe_submit_queued_prompt(
            ctx,
            &mut handler,
            &mut active_turn,
            &mut queued_prompts,
            &always_allowed,
        )?;

        // Track spinner timing and reset the approval cursor on each new request.
        match active_turn.is_some() {
            true if turn_started_at.is_none() => {
                turn_started_at = Some(std::time::Instant::now());
            }
            false => turn_started_at = None,
            _ => {}
        }
        let pending_approval_id = awaiting_approval
            .as_ref()
            .map(|request| request.tool_use_id.clone());
        if pending_approval_id != last_approval_id {
            last_approval_id = pending_approval_id;
            approval_selection = 0;
        }

        if ctx.session().messages.is_empty() && !onboarding_seen_recorded {
            if should_show_project_onboarding(&ctx.session().cwd) {
                increment_project_onboarding_seen_count(&ctx.session().cwd);
            }
            onboarding_seen_recorded = true;
        }

        if input_buffer.trim_start().starts_with('/') {
            let _ = ctx.refresh_compatibility_if_stale(Duration::from_millis(500));
        }

        let visible_line_count = handler.visible_lines().len();
        if visible_line_count > last_visible_line_count && !viewport.pinned_to_bottom {
            viewport.unseen_count += visible_line_count - last_visible_line_count;
        }
        last_visible_line_count = visible_line_count;

        // During reverse history search, the input box shows the current
        // match (or the query) instead of the real buffer.
        let display_input = if let Some((query, match_index)) = &history_search {
            match match_index {
                Some(index) => history.get(*index).cloned().unwrap_or_default(),
                None => query.clone(),
            }
        } else {
            input_buffer.clone()
        };
        let display_cursor = if history_search.is_some() {
            display_input.len()
        } else {
            cursor_pos
        };

        terminal.draw(|frame| {
            let area = frame.area();
            last_area = area;

            if screen == ReplScreen::Transcript {
                render_transcript_screen(frame, area, &transcript_mode);
                return;
            }

            let command_entries = filtered_command_entries(ctx, &input_buffer);
            let file_entries = if command_entries.is_empty() {
                file_palette_query(&input_buffer, cursor_pos)
                    .map(|(_, query)| {
                        filtered_file_entries(
                            file_index.get_or_insert_with(|| build_file_index(&ctx.session().cwd)),
                            &query,
                        )
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let palette_height =
                command_palette_height(&command_entries).max(file_palette_height(&file_entries));
            let approval_height =
                approval_selector_height(awaiting_approval.as_ref(), approval_selection);
            let spinner_height = if active_turn.is_some() && awaiting_approval.is_none() {
                1
            } else {
                0
            };
            let queued_height = if queued_prompts.is_empty() {
                0
            } else {
                (queued_prompts.len().min(MAX_VISIBLE_QUEUED_PROMPTS) + 1) as u16
            };
            let input_height = input_box_height(&display_input, area.width);

            let chunks = Layout::vertical([
                Constraint::Min(1),
                Constraint::Length(palette_height),
                Constraint::Length(approval_height),
                Constraint::Length(queued_height),
                Constraint::Length(spinner_height),
                Constraint::Length(input_height),
                Constraint::Length(1),
            ])
            .split(area);

            if viewport.pinned_to_bottom {
                viewport.scroll_offset = max_transcript_scroll(&handler, chunks[0].height as usize);
                viewport.unseen_count = 0;
            }
            last_body_height = chunks[0].height as usize;

            render_body(
                frame,
                chunks[0],
                ctx,
                &handler,
                &input_buffer,
                &[],
                viewport.scroll_offset,
                viewport.unseen_count,
            );

            if !command_entries.is_empty() {
                render_command_palette(
                    frame,
                    chunks[1],
                    &input_buffer,
                    &command_entries,
                    palette_index,
                );
            } else if !file_entries.is_empty() {
                render_file_palette(frame, chunks[1], &file_entries, palette_index);
            }

            if approval_height > 0 {
                render_approval_selector(
                    frame,
                    chunks[2],
                    awaiting_approval.as_ref().unwrap(),
                    approval_selection,
                    ctx,
                );
            }

            if queued_height > 0 {
                frame.render_widget(
                    Paragraph::new(queued_prompt_preview_lines(
                        &queued_prompts,
                        chunks[3].width as usize,
                    )),
                    chunks[3],
                );
            }

            if spinner_height > 0 {
                frame.render_widget(Paragraph::new(spinner_line(turn_started_at)), chunks[4]);
            }

            let cursor = render_input_box(
                frame,
                chunks[5],
                &display_input,
                display_cursor,
                active_turn.is_some(),
            );
            frame.set_cursor_position(cursor);

            let exit_hint_active = exit_hint
                .map(|t| t.elapsed().as_millis() < 1500)
                .unwrap_or(false);
            render_hint_line(
                frame,
                chunks[6],
                ctx,
                awaiting_approval.is_some(),
                history_search.as_ref().map(|(query, _)| query.as_str()),
                exit_hint_active,
            );

            if let Some(panel) = &handler.info_panel {
                let modal_area = centered_rect(72, 72, area);
                let mut panel_lines = Vec::new();
                if let Some(status) = &panel.status {
                    panel_lines.push(status.clone());
                    panel_lines.push(String::new());
                }
                panel_lines.extend(panel.lines.clone());
                panel_lines.push(String::new());
                panel_lines.push(info_panel_hint_line().to_string());
                let modal = Paragraph::new(panel_lines.join("\n"))
                    .block(
                        Block::default()
                            .title(format!(" {} ", panel.title.as_str()))
                            .borders(Borders::ALL)
                            .border_type(BorderType::Rounded)
                            .border_style(Style::default().fg(MUTED)),
                    )
                    .wrap(Wrap { trim: false })
                    .scroll((info_panel_scroll as u16, 0));
                frame.render_widget(Clear, modal_area);
                frame.render_widget(modal, modal_area);
            }
        })?;

        let transcript_height = last_body_height;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                if screen == ReplScreen::Transcript {
                    let transcript_height = last_area
                        .height
                        .saturating_sub(if transcript_mode.dump_mode { 0 } else { 2 })
                        as usize;
                    let max_scroll = max_scroll_for_lines(
                        transcript_mode.frozen_rows.len(),
                        transcript_height.max(1),
                    );

                    if transcript_mode.search_open {
                        match key.code {
                            KeyCode::Esc => {
                                transcript_mode.search_open = false;
                                transcript_mode.search_input = transcript_mode.search_query.clone();
                                continue;
                            }
                            KeyCode::Enter => {
                                transcript_mode.search_open = false;
                                let query = transcript_mode.search_input.clone();
                                update_transcript_search(&mut transcript_mode, query);
                                continue;
                            }
                            KeyCode::Backspace => {
                                transcript_mode.search_input.pop();
                                let query = transcript_mode.search_input.clone();
                                update_transcript_search(&mut transcript_mode, query);
                                continue;
                            }
                            KeyCode::Char(c) => {
                                transcript_mode.search_input.push(c);
                                let query = transcript_mode.search_input.clone();
                                update_transcript_search(&mut transcript_mode, query);
                                continue;
                            }
                            _ => {}
                        }
                    }

                    match key.code {
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break;
                        }
                        KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            screen = ReplScreen::Main;
                            transcript_mode = TranscriptModeState::default();
                            continue;
                        }
                        KeyCode::Char('q') | KeyCode::Esc => {
                            screen = ReplScreen::Main;
                            transcript_mode = TranscriptModeState::default();
                            continue;
                        }
                        KeyCode::Char('/') => {
                            transcript_mode.search_open = true;
                            transcript_mode.search_input = transcript_mode.search_query.clone();
                            continue;
                        }
                        KeyCode::Char('n') if !transcript_mode.search_matches.is_empty() => {
                            navigate_transcript_search(&mut transcript_mode, true);
                            continue;
                        }
                        KeyCode::Char('N') if !transcript_mode.search_matches.is_empty() => {
                            navigate_transcript_search(&mut transcript_mode, false);
                            continue;
                        }
                        KeyCode::Char('v') => {
                            transcript_mode.export_status =
                                Some(export_transcript_lines(&transcript_mode.frozen_rows)?);
                            continue;
                        }
                        KeyCode::Char('[') => {
                            transcript_mode.dump_mode = true;
                            continue;
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            transcript_mode.viewport.scroll_offset =
                                transcript_mode.viewport.scroll_offset.saturating_sub(1);
                            continue;
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            transcript_mode.viewport.scroll_offset = transcript_mode
                                .viewport
                                .scroll_offset
                                .saturating_add(1)
                                .min(max_scroll);
                            continue;
                        }
                        KeyCode::PageUp => {
                            let step = transcript_height.saturating_sub(2).max(1);
                            transcript_mode.viewport.scroll_offset =
                                transcript_mode.viewport.scroll_offset.saturating_sub(step);
                            continue;
                        }
                        KeyCode::PageDown => {
                            let step = transcript_height.saturating_sub(2).max(1);
                            transcript_mode.viewport.scroll_offset = transcript_mode
                                .viewport
                                .scroll_offset
                                .saturating_add(step)
                                .min(max_scroll);
                            continue;
                        }
                        KeyCode::Home => {
                            transcript_mode.viewport.scroll_offset = 0;
                            continue;
                        }
                        KeyCode::End => {
                            transcript_mode.viewport.scroll_offset = max_scroll;
                            continue;
                        }
                        _ => {}
                    }

                    continue;
                }

                if awaiting_approval.is_some() {
                    const APPROVAL_OPTIONS: usize = 3;
                    let choose = |choice: usize,
                                  awaiting_approval: &mut Option<ApprovalRequest>,
                                  handler: &mut ReplHandler,
                                  active_turn: &Option<ActiveTurn>,
                                  always_allowed: &Arc<
                        Mutex<std::collections::HashSet<String>>,
                    >| {
                        if let Some(req) = awaiting_approval.take() {
                            let approved = choice != 2;
                            if choice == 1 {
                                if let Ok(mut set) = always_allowed.lock() {
                                    set.insert(req.tool_name.clone());
                                }
                            }
                            handler.record_live_tool_approval_decision(&req.tool_use_id, approved);
                            if let Some(turn) = active_turn {
                                let _ = turn.approvals.send(approved);
                            }
                        }
                    };

                    match key.code {
                        KeyCode::Up | KeyCode::Char('k') => {
                            approval_selection = approval_selection.saturating_sub(1);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            approval_selection = (approval_selection + 1).min(APPROVAL_OPTIONS - 1);
                        }
                        KeyCode::Char('1') => choose(
                            0,
                            &mut awaiting_approval,
                            &mut handler,
                            &active_turn,
                            &always_allowed,
                        ),
                        KeyCode::Char('2') => choose(
                            1,
                            &mut awaiting_approval,
                            &mut handler,
                            &active_turn,
                            &always_allowed,
                        ),
                        KeyCode::Char('3') | KeyCode::Char('n') | KeyCode::Char('N') => choose(
                            2,
                            &mut awaiting_approval,
                            &mut handler,
                            &active_turn,
                            &always_allowed,
                        ),
                        KeyCode::Char('y') | KeyCode::Char('Y') => choose(
                            0,
                            &mut awaiting_approval,
                            &mut handler,
                            &active_turn,
                            &always_allowed,
                        ),
                        KeyCode::Enter => choose(
                            approval_selection,
                            &mut awaiting_approval,
                            &mut handler,
                            &active_turn,
                            &always_allowed,
                        ),
                        KeyCode::Esc => choose(
                            2,
                            &mut awaiting_approval,
                            &mut handler,
                            &active_turn,
                            &always_allowed,
                        ),
                        _ => {}
                    }
                    viewport.pinned_to_bottom = true;
                    viewport.unseen_count = 0;
                    continue;
                }

                if let Some((query, match_index)) = history_search.clone() {
                    let find = |query: &str, before: usize| -> Option<usize> {
                        (0..before.min(history.len()))
                            .rev()
                            .find(|&index| history[index].contains(query))
                    };
                    match key.code {
                        KeyCode::Esc => {
                            input_buffer = history_search_stash.clone();
                            cursor_pos = input_buffer.len();
                            history_search = None;
                        }
                        KeyCode::Enter => {
                            if let Some(index) = match_index {
                                input_buffer = history[index].clone();
                                cursor_pos = input_buffer.len();
                            }
                            history_search = None;
                        }
                        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            let before = match_index.unwrap_or(history.len());
                            let index = find(&query, before);
                            history_search = Some((query, index));
                        }
                        KeyCode::Backspace => {
                            let mut query = query;
                            query.pop();
                            let index = find(&query, history.len());
                            history_search = Some((query, index));
                        }
                        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                            let mut query = query;
                            query.push(ch);
                            let index = find(&query, history.len());
                            history_search = Some((query, index));
                        }
                        _ => {
                            history_search = None;
                        }
                    }
                    continue;
                }

                if handler.info_panel.is_some() {
                    let max_scroll = handler
                        .info_panel
                        .as_ref()
                        .map(|panel| panel.lines.len().saturating_sub(1))
                        .unwrap_or(0);
                    match key.code {
                        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('?') => {
                            handler.clear_panel();
                            info_panel_scroll = 0;
                            continue;
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            info_panel_scroll = info_panel_scroll.saturating_sub(1);
                            continue;
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            info_panel_scroll = info_panel_scroll.saturating_add(1).min(max_scroll);
                            continue;
                        }
                        KeyCode::PageUp => {
                            info_panel_scroll = info_panel_scroll.saturating_sub(8);
                            continue;
                        }
                        KeyCode::PageDown => {
                            info_panel_scroll = info_panel_scroll.saturating_add(8).min(max_scroll);
                            continue;
                        }
                        _ => {}
                    }
                }

                let command_palette_open = !filtered_command_entries(ctx, &input_buffer).is_empty();
                let file_token = file_palette_query(&input_buffer, cursor_pos);
                let file_entries_now = if command_palette_open {
                    Vec::new()
                } else {
                    file_token
                        .as_ref()
                        .map(|(_, query)| {
                            filtered_file_entries(
                                file_index
                                    .get_or_insert_with(|| build_file_index(&ctx.session().cwd)),
                                query,
                            )
                        })
                        .unwrap_or_default()
                };
                let palette_open = command_palette_open || !file_entries_now.is_empty();

                match key.code {
                    KeyCode::Char('o')
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && handler.has_content() =>
                    {
                        screen = ReplScreen::Transcript;
                        transcript_mode = enter_transcript_mode(&handler, transcript_height);
                        transcript_mode.viewport.scroll_offset = viewport.scroll_offset;
                        transcript_mode.viewport.pinned_to_bottom = false;
                    }
                    KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if !history.is_empty() {
                            history_search_stash = input_buffer.clone();
                            history_search = Some((String::new(), None));
                            history_index = None;
                        }
                    }
                    KeyCode::BackTab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        ctx.cycle_permission_mode();
                        handler.push_overlay(format!(
                            "[info] {} (shift+tab to cycle)",
                            permission_mode_label(ctx.permission_mode())
                        ));
                        handler.rebuild_from_session(ctx.session(), ctx.show_thinking);
                    }
                    KeyCode::Up if palette_open => {
                        palette_touched = true;
                        palette_index = palette_index.saturating_sub(1);
                    }
                    KeyCode::Down if palette_open => {
                        palette_touched = true;
                        let len = if !file_entries_now.is_empty() {
                            file_entries_now.len()
                        } else {
                            filtered_command_entries(ctx, &input_buffer).len().min(8)
                        };
                        palette_index = (palette_index + 1).min(len.saturating_sub(1));
                    }
                    KeyCode::Tab if palette_open => {
                        if !file_entries_now.is_empty() {
                            if let (Some((at, _)), Some(path)) = (
                                file_token,
                                file_entries_now.get(palette_index.min(file_entries_now.len() - 1)),
                            ) {
                                input_buffer.replace_range(
                                    at..cursor_pos,
                                    &format!("@{} ", path.trim_end_matches('/')),
                                );
                                cursor_pos = at + path.trim_end_matches('/').len() + 2;
                            }
                        } else {
                            let entries = filtered_command_entries(ctx, &input_buffer);
                            if let Some(entry) = entries.get(palette_index.min(entries.len() - 1)) {
                                input_buffer = format!("{} ", entry.name);
                                cursor_pos = input_buffer.len();
                            }
                        }
                        palette_index = 0;
                        palette_touched = false;
                    }
                    KeyCode::Up if history_index.is_some() || !history.is_empty() => {
                        if history_index.is_none() {
                            history_stash = input_buffer.clone();
                            history_index = Some(history.len() - 1);
                        } else if let Some(index) = history_index {
                            if index > 0 {
                                history_index = Some(index - 1);
                            }
                        }
                        if let Some(index) = history_index {
                            if let Some(entry) = history.get(index) {
                                input_buffer = entry.clone();
                                cursor_pos = input_buffer.len();
                            }
                        }
                    }
                    KeyCode::Down if history_index.is_some() => {
                        let index = history_index.unwrap();
                        if index + 1 < history.len() {
                            history_index = Some(index + 1);
                            input_buffer = history[index + 1].clone();
                        } else {
                            history_index = None;
                            input_buffer = history_stash.clone();
                        }
                        cursor_pos = input_buffer.len();
                    }
                    KeyCode::PageUp if handler.has_content() => {
                        let step = transcript_height.saturating_sub(2).max(1);
                        viewport.scroll_offset = viewport.scroll_offset.saturating_sub(step);
                        viewport.pinned_to_bottom = false;
                    }
                    KeyCode::PageDown if handler.has_content() => {
                        let max_scroll = max_transcript_scroll(&handler, transcript_height);
                        let step = transcript_height.saturating_sub(2).max(1);
                        viewport.scroll_offset =
                            viewport.scroll_offset.saturating_add(step).min(max_scroll);
                        viewport.pinned_to_bottom = viewport.scroll_offset >= max_scroll;
                        if viewport.pinned_to_bottom {
                            viewport.unseen_count = 0;
                        }
                    }
                    KeyCode::Esc => {
                        if active_turn.is_some() {
                            interrupt_active_turn(
                                ctx,
                                &mut handler,
                                &mut active_turn,
                                &mut awaiting_approval,
                                &mut queued_prompts,
                            );
                            turn_started_at = None;
                        } else if !input_buffer.is_empty() {
                            // Claude Code clears the input on a double-tap Esc.
                            let now = std::time::Instant::now();
                            if last_esc
                                .map(|t| now.duration_since(t).as_millis() < 1000)
                                .unwrap_or(false)
                            {
                                input_buffer.clear();
                                cursor_pos = 0;
                                history_index = None;
                                last_esc = None;
                            } else {
                                last_esc = Some(now);
                            }
                        }
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        let now = std::time::Instant::now();
                        if last_ctrl_c
                            .map(|t| now.duration_since(t).as_millis() < 1500)
                            .unwrap_or(false)
                        {
                            break;
                        }
                        last_ctrl_c = Some(now);
                        exit_hint = Some(now);
                    }
                    KeyCode::Char('d')
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && input_buffer.is_empty() =>
                    {
                        break;
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        input_buffer.drain(..cursor_pos);
                        cursor_pos = 0;
                    }
                    KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        let before = input_buffer[..cursor_pos].trim_end();
                        let new_len = before
                            .rfind(char::is_whitespace)
                            .map(|i| i + 1)
                            .unwrap_or(0);
                        input_buffer.drain(new_len..cursor_pos);
                        cursor_pos = new_len;
                    }
                    KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        cursor_pos = input_buffer
                            .find('\n')
                            .filter(|line_start| *line_start >= cursor_pos)
                            .map(|line_start| {
                                input_buffer[..line_start]
                                    .rfind('\n')
                                    .map(|prev| prev + 1)
                                    .unwrap_or(0)
                            })
                            .unwrap_or(0);
                    }
                    KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        cursor_pos = input_buffer[cursor_pos..]
                            .find('\n')
                            .map(|offset| cursor_pos + offset)
                            .unwrap_or(input_buffer.len());
                    }
                    KeyCode::Char('?') if input_buffer.is_empty() => {
                        let panel = help_panel(ctx);
                        handler.show_panel(panel.title, panel.lines);
                        info_panel_scroll = 0;
                    }
                    KeyCode::Enter => {
                        let ends_with_backslash =
                            input_buffer.trim_end_matches('\n').ends_with('\\');
                        if key.modifiers.contains(KeyModifiers::ALT) || ends_with_backslash {
                            if ends_with_backslash {
                                let trim_at = input_buffer
                                    .rfind('\\')
                                    .expect("checked ends_with_backslash");
                                input_buffer.remove(trim_at);
                                if cursor_pos > trim_at {
                                    cursor_pos = trim_at;
                                }
                            }
                            input_buffer.insert(cursor_pos, '\n');
                            cursor_pos += 1;
                        } else if !file_entries_now.is_empty() && palette_touched {
                            // Enter accepts the focused file completion.
                            if let (Some((at, _)), Some(path)) = (
                                file_token,
                                file_entries_now.get(palette_index.min(file_entries_now.len() - 1)),
                            ) {
                                input_buffer.replace_range(
                                    at..cursor_pos,
                                    &format!("@{} ", path.trim_end_matches('/')),
                                );
                                cursor_pos = at + path.trim_end_matches('/').len() + 2;
                            }
                            palette_index = 0;
                            palette_touched = false;
                        } else if palette_open && palette_touched {
                            let entries = filtered_command_entries(ctx, &input_buffer);
                            if let Some(entry) =
                                entries.get(palette_index.min(entries.len().saturating_sub(1)))
                            {
                                input_buffer = entry.name.clone();
                                cursor_pos = input_buffer.len();
                            }
                            let prompt = input_buffer.trim().to_string();
                            if !prompt.is_empty() {
                                input_buffer.clear();
                                cursor_pos = 0;
                                if handle_slash_command(
                                    ctx,
                                    &mut handler,
                                    &prompt,
                                    &mut active_turn,
                                    &always_allowed,
                                )? {
                                    handler.rebuild_from_session(ctx.session(), ctx.show_thinking);
                                }
                            }
                        } else if !input_buffer.trim().is_empty() {
                            let prompt = input_buffer.trim().to_string();
                            input_buffer.clear();
                            cursor_pos = 0;
                            palette_index = 0;
                            palette_touched = false;
                            history_index = None;

                            if history.last().map(|h| h.as_str()) != Some(prompt.as_str()) {
                                history.push(prompt.clone());
                            }

                            if active_turn.is_some() {
                                queued_prompts.push(prompt);
                            } else if handle_slash_command(
                                ctx,
                                &mut handler,
                                &prompt,
                                &mut active_turn,
                                &always_allowed,
                            )? {
                                handler.rebuild_from_session(ctx.session(), ctx.show_thinking);
                            } else if let Some(command) = prompt.strip_prefix('!') {
                                let command = command.trim().to_string();
                                if !command.is_empty() {
                                    handler.begin_live_turn(prompt.clone());
                                    turn_started_at = Some(std::time::Instant::now());
                                    active_turn =
                                        Some(spawn_bash_worker(ctx.cloned_session(), command));
                                }
                            } else {
                                handler.begin_live_turn(prompt.clone());
                                turn_started_at = Some(std::time::Instant::now());
                                active_turn = Some(spawn_turn_worker(
                                    ctx,
                                    prompt.clone(),
                                    expand_at_mentions(&prompt, &ctx.session().cwd),
                                    always_allowed.clone(),
                                ));
                            }
                        }
                    }
                    KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        input_buffer.insert(cursor_pos, '\n');
                        cursor_pos += 1;
                    }
                    KeyCode::Backspace => {
                        if cursor_pos > 0 {
                            input_buffer.remove(cursor_pos - 1);
                            cursor_pos -= 1;
                        }
                    }
                    KeyCode::Delete => {
                        if cursor_pos < input_buffer.len() {
                            input_buffer.remove(cursor_pos);
                        }
                    }
                    KeyCode::Left => {
                        if cursor_pos > 0 {
                            cursor_pos = input_buffer[..cursor_pos]
                                .char_indices()
                                .last()
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                        }
                    }
                    KeyCode::Right => {
                        if cursor_pos < input_buffer.len() {
                            cursor_pos += input_buffer[cursor_pos..]
                                .chars()
                                .next()
                                .map(|c| c.len_utf8())
                                .unwrap_or(0);
                        }
                    }
                    KeyCode::Home => {
                        if input_buffer.is_empty() && handler.has_content() {
                            viewport.scroll_offset = 0;
                            viewport.pinned_to_bottom = false;
                        } else {
                            cursor_pos = 0;
                        }
                    }
                    KeyCode::End => {
                        if input_buffer.is_empty() && handler.has_content() {
                            viewport.scroll_offset =
                                max_transcript_scroll(&handler, transcript_height);
                            viewport.pinned_to_bottom = true;
                            viewport.unseen_count = 0;
                        } else {
                            cursor_pos = input_buffer.len();
                        }
                    }
                    KeyCode::Char(c) => {
                        input_buffer.insert(cursor_pos, c);
                        cursor_pos += c.len_utf8();
                        history_index = None;
                    }
                    _ => {}
                }

                let len = if input_buffer.trim_start().starts_with('/') {
                    filtered_command_entries(ctx, &input_buffer).len().min(8)
                } else {
                    file_entries_now.len()
                };
                if len == 0 {
                    palette_index = 0;
                    palette_touched = false;
                } else if palette_index >= len {
                    palette_index = len - 1;
                }
            }
        }
    }

    Ok(())
}

fn render_body(
    frame: &mut Frame,
    area: Rect,
    ctx: &TuiContext,
    handler: &ReplHandler,
    input_buffer: &str,
    command_entries: &[CommandEntry],
    scroll_offset: usize,
    unseen_count: usize,
) {
    let _ = command_entries;
    if handler.has_content() {
        render_conversation(frame, area, handler, scroll_offset, unseen_count);
    } else {
        render_dashboard(frame, area, ctx, input_buffer, command_entries);
    }
}

fn render_conversation(
    frame: &mut Frame,
    area: Rect,
    handler: &ReplHandler,
    scroll_offset: usize,
    unseen_count: usize,
) {
    let has_unseen = unseen_count > 0;
    let divider_height = if has_unseen { 1 } else { 0 };

    let chunks =
        Layout::vertical([Constraint::Min(1), Constraint::Length(divider_height)]).split(area);

    let transcript = Paragraph::new(render_rows(
        &handler.visible_rows(),
        None,
        &[],
        &handler.expanded_tool_results,
        chunks[0].width as usize,
    ))
    .wrap(Wrap { trim: false })
    .scroll((scroll_offset as u16, 0));
    frame.render_widget(transcript, chunks[0]);

    if has_unseen {
        let divider = format!(
            "─── {} new message{} ───",
            unseen_count,
            if unseen_count == 1 { "" } else { "s" }
        );
        let remaining_width = chunks[1]
            .width
            .saturating_sub(divider.chars().count() as u16);
        let left = remaining_width / 2;
        let right = remaining_width.saturating_sub(left);
        let line = format!(
            "{} {} {}",
            "─".repeat(left as usize),
            divider,
            "─".repeat(right as usize)
        );
        frame.render_widget(
            Paragraph::new(line).style(Style::default().fg(MUTED)),
            chunks[1],
        );
    }
}

fn render_dashboard(
    frame: &mut Frame,
    area: Rect,
    ctx: &TuiContext,
    input_buffer: &str,
    command_entries: &[CommandEntry],
) {
    let chunks = if command_entries.is_empty() {
        Layout::vertical([Constraint::Length(DASHBOARD_HEIGHT), Constraint::Min(0)]).split(area)
    } else {
        Layout::vertical([
            Constraint::Length(DASHBOARD_HEIGHT),
            Constraint::Length(command_palette_height(command_entries)),
            Constraint::Min(0),
        ])
        .split(area)
    };

    frame.render_widget(
        Paragraph::new(welcome_lines(ctx)).wrap(Wrap { trim: false }),
        chunks[0],
    );

    let _ = (input_buffer, command_entries);
}

/// Claude Code-style welcome block: logo on the left, product metadata
/// on the right, then tips/onboarding and recent activity.
fn welcome_lines(ctx: &TuiContext) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let logo_width = CLAUDE_LOGO
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);

    let meta = [
        format!("ClawedCode v{}", env!("CARGO_PKG_VERSION")),
        format!("{} · {}", ctx.model_name(), provider_label()),
        display_path(&ctx.session().cwd),
    ];

    for (index, logo) in CLAUDE_LOGO.iter().enumerate() {
        let padding = " ".repeat(logo_width - logo.chars().count() + 3);
        let meta_text = meta.get(index).cloned().unwrap_or_default();
        let meta_style = if index == 0 {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(MUTED)
        };
        lines.push(Line::from(vec![
            Span::styled((*logo).to_string(), Style::default().fg(ACCENT)),
            Span::raw(padding),
            Span::styled(meta_text, meta_style),
        ]));
    }

    lines.push(Line::from(""));
    lines.extend(welcome_right_lines(ctx));
    lines
}

fn render_transcript_screen(frame: &mut Frame, area: Rect, state: &TranscriptModeState) {
    let transcript = Paragraph::new(transcript_render_lines(state, area.width as usize))
        .wrap(Wrap { trim: false })
        .scroll((state.viewport.scroll_offset as u16, 0));

    if state.dump_mode {
        frame.render_widget(transcript, area);
        return;
    }

    let chunks = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);

    let transcript = transcript.block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(MUTED)),
    );
    frame.render_widget(transcript, chunks[0]);

    render_transcript_footer(frame, chunks[1], state);

    let bottom_line = if state.search_open {
        format!("Search: {}", state.search_input)
    } else {
        transcript_footer_hint_line(state)
    };
    frame.render_widget(
        Paragraph::new(bottom_line).style(Style::default().fg(MUTED)),
        chunks[2],
    );
}

fn render_command_palette(
    frame: &mut Frame,
    area: Rect,
    input_buffer: &str,
    entries: &[CommandEntry],
    selected: usize,
) {
    let _ = input_buffer;
    let mut lines = vec![Line::from("")];

    lines.extend(entries.iter().take(8).enumerate().map(|(index, entry)| {
        let is_selected = index == selected;
        let (marker, name_style) = if is_selected {
            (
                "❯ ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )
        } else {
            ("  ", Style::default().add_modifier(Modifier::BOLD))
        };
        let (badge, badge_color) = command_source_badge(entry.source);
        Line::from(vec![
            Span::styled(marker, name_style),
            Span::styled(format!("{:<14}", entry.name), name_style),
            Span::styled(entry.description.clone(), Style::default().fg(DIM)),
            Span::styled(format!("  {badge}"), Style::default().fg(badge_color)),
        ])
    }));

    frame.render_widget(Paragraph::new(lines), area);
}

fn command_palette_height(entries: &[CommandEntry]) -> u16 {
    if entries.is_empty() {
        0
    } else {
        entries.len().min(8) as u16 + 1
    }
}

/// Height of the inline approval selector (0 when not awaiting approval).
fn approval_selector_height(request: Option<&ApprovalRequest>, _selected: usize) -> u16 {
    if request.is_none() {
        return 0;
    }
    // border + title + blank + input + blank + question + 3 options + blank + footer
    10
}

/// Permission dialog: a top-only rounded border in `permission` blue, the
/// tool title, the rendered input, "Do you want to proceed?", then the
/// numbered options with a `❯` focus marker — matches PermissionDialog +
/// PermissionPrompt from the reference implementation.
fn render_approval_selector(
    frame: &mut Frame,
    area: Rect,
    request: &ApprovalRequest,
    selected: usize,
    ctx: &TuiContext,
) {
    let display_name = tool_display_name(&request.tool_name);
    let title = if request.tool_name == "shell" {
        "Bash command".to_string()
    } else {
        "Tool use".to_string()
    };
    let input_json = serde_json::to_string(&request.input).unwrap_or_default();
    let arg = tool_display_arg(&request.tool_name, &input_json);
    let cwd = display_path(&ctx.session().cwd);
    let subject = if request.tool_name == "shell" {
        request
            .input
            .get("command")
            .and_then(|v| v.as_str())
            .and_then(|cmd| cmd.split_whitespace().next())
            .map(|word| format!("`{word}`"))
            .unwrap_or_else(|| display_name.to_string())
    } else {
        display_name.to_string()
    };

    let options = [
        "Yes".to_string(),
        format!("Yes, and don't ask again for {subject} commands in {cwd}"),
        "No, and tell Claude what to do differently (esc)".to_string(),
    ];

    // Top border line: ╭────╮ in the `permission` color.
    let border_width = area.width.saturating_sub(2) as usize;
    frame.render_widget(
        Paragraph::new(format!("╭{}╮", "─".repeat(border_width)))
            .style(Style::default().fg(PERMISSION_FG)),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let mut lines = vec![
        Line::from(vec![
            Span::raw(" "),
            Span::styled(
                title,
                Style::default()
                    .fg(PERMISSION_FG)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            format!(" {arg}"),
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(Span::styled(
            " Do you want to proceed?".to_string(),
            Style::default().fg(DIM),
        )),
    ];

    for (index, option) in options.iter().enumerate() {
        let is_selected = index == selected;
        let marker = if is_selected { "❯ " } else { "  " };
        let style = if is_selected {
            Style::default().fg(PERMISSION_FG)
        } else {
            Style::default().fg(DIM)
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {marker}"), style),
            Span::styled(format!("{}. {}", index + 1, option), style),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " Esc to cancel · Tab to amend".to_string(),
        Style::default().fg(DIM),
    )));

    frame.render_widget(
        Paragraph::new(lines),
        Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1),
        ),
    );
}

/// Animated "working" line above the input box: `✻ Verb… (esc to interrupt · Ns)`.
fn spinner_line(turn_started_at: Option<std::time::Instant>) -> Line<'static> {
    let elapsed = turn_started_at
        .map(|start| start.elapsed().as_millis())
        .unwrap_or(0);
    let frame_index = (elapsed / SPINNER_FRAME_MS) as usize % SPINNER_FRAMES.len();
    let verb_index = (elapsed / SPINNER_VERB_MS) as usize % SPINNER_VERBS.len();
    let seconds = elapsed / 1000;

    Line::from(vec![
        Span::styled(
            format!("{} ", SPINNER_FRAMES[frame_index]),
            Style::default().fg(ACCENT),
        ),
        Span::styled(
            format!("{}… ", SPINNER_VERBS[verb_index]),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("(esc to interrupt · {}s)", seconds),
            Style::default().fg(DIM),
        ),
    ])
}

/// Rows needed for the bordered input box (input lines + 2 border rows).
fn input_box_height(input: &str, width: u16) -> u16 {
    let inner = input_inner_width(width);
    (wrap_input(input, inner).len().min(8) as u16) + 2
}

fn input_inner_width(width: u16) -> usize {
    // "❯ " prefix (2 display columns) — no side borders on the input area
    (width as usize).saturating_sub(2).max(10)
}

/// Wrap `> {input}` into display lines fitting `width` characters.
fn wrap_input(input: &str, width: usize) -> Vec<String> {
    let text = format!("> {input}");
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut col = 0usize;

    for ch in text.chars() {
        if ch == '\n' {
            lines.push(std::mem::take(&mut current));
            col = 0;
            continue;
        }
        if col >= width {
            lines.push(std::mem::take(&mut current));
            col = 0;
        }
        current.push(ch);
        col += 1;
    }
    lines.push(current);
    lines
}

/// Draw the Claude Code input area: top+bottom rounded borders only
/// (╭─╮ / ╰─╯), a colored `❯` pointer prompt, and the editing caret.
fn render_input_box(
    frame: &mut Frame,
    area: Rect,
    input: &str,
    cursor_pos: usize,
    loading: bool,
) -> ratatui::layout::Position {
    if area.height == 0 {
        return ratatui::layout::Position::new(area.x, area.y);
    }

    let border_style = Style::default().fg(PROMPT_BORDER);
    let border_width = area.width.saturating_sub(2) as usize;
    let top = format!("╭{}╮", "─".repeat(border_width));
    let bottom = format!("╰{}╯", "─".repeat(border_width));

    frame.render_widget(
        Paragraph::new(top).style(border_style),
        Rect::new(area.x, area.y, area.width, 1),
    );
    frame.render_widget(
        Paragraph::new(bottom).style(border_style),
        Rect::new(area.x, area.y + area.height - 1, area.width, 1),
    );

    let inner = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(2),
    );
    let wrap_width = input_inner_width(area.width);
    let all_lines = wrap_input(input, wrap_width);
    let visible_count = all_lines.len().min(8);
    // Bash mode (`!cmd`) swaps the pointer glyph and tints it, like Claude Code.
    let bash_mode = input.starts_with('!');
    let pointer = if bash_mode { "!" } else { POINTER };
    let prompt_style = if loading {
        Style::default()
            .fg(if bash_mode { PERMISSION_FG } else { ACCENT })
            .add_modifier(Modifier::DIM)
    } else {
        Style::default().fg(if bash_mode { PERMISSION_FG } else { ACCENT })
    };

    for (index, line) in all_lines.iter().take(8).enumerate() {
        let y = inner.y + index as u16;
        if index == 0 {
            // First display line carries the "❯ " (or "!" in bash mode) pointer.
            let raw = line.strip_prefix("> ").unwrap_or(line);
            let body = if bash_mode {
                raw.strip_prefix('!').unwrap_or(raw)
            } else {
                raw
            };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(format!("{pointer} "), prompt_style),
                    Span::raw(body.to_string()),
                ])),
                Rect::new(inner.x, y, inner.width, 1),
            );
        } else {
            frame.render_widget(
                Paragraph::new(Line::from(Span::raw(format!("  {line}")))),
                Rect::new(inner.x, y, inner.width, 1),
            );
        }
    }

    // Locate the caret in wrapped coordinates: prefix "❯ " + input[..cursor].
    // In bash mode the leading `!` is shown as the pointer, not in the body.
    let prefix_chars = 2usize;
    let cursor_chars = prefix_chars
        + input[..cursor_pos]
            .chars()
            .count()
            .saturating_sub(if bash_mode { 1 } else { 0 });
    let text = if bash_mode {
        format!("> {}", input.strip_prefix('!').unwrap_or(input))
    } else {
        format!("> {input}")
    };
    let mut col = 0usize;
    let mut row = 0usize;
    for (index, ch) in text.chars().enumerate() {
        if index == cursor_chars {
            break;
        }
        if ch == '\n' {
            row += 1;
            col = 0;
        } else if col + 1 >= wrap_width {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    let row = row.min(7);
    let _ = visible_count;
    ratatui::layout::Position::new(
        inner.x + col.min(wrap_width.saturating_sub(1)) as u16,
        inner.y + row as u16,
    )
}

/// Bottom hint line: approval hints, history-search label, permission-mode
/// banner, or `? for shortcuts`.
fn render_hint_line(
    frame: &mut Frame,
    area: Rect,
    ctx: &TuiContext,
    awaiting: bool,
    history_search: Option<&str>,
    exit_hint: bool,
) {
    let mode = ctx.permission_mode();
    let mode_text = permission_mode_label(mode);

    let left = if awaiting {
        "↑/↓ select · enter confirm · esc deny".to_string()
    } else if exit_hint {
        "Press Ctrl+C again to exit".to_string()
    } else if let Some(query) = history_search {
        format!("(reverse-i-search)`{query}`:")
    } else if mode != clawedcode_core::permissions::PermissionMode::Default {
        format!("{mode_text} · ? for shortcuts")
    } else {
        "? for shortcuts".to_string()
    };

    let left_style = if history_search.is_some() || exit_hint {
        Style::default().fg(PERMISSION_FG)
    } else if mode != clawedcode_core::permissions::PermissionMode::Default {
        Style::default().fg(permission_mode_color(mode))
    } else {
        Style::default().fg(SUBTLE)
    };

    let right = format!(
        "{} · {}",
        ctx.model_name(),
        display_path(&ctx.session().cwd)
    );
    let right_width = right
        .chars()
        .count()
        .min(area.width.saturating_sub(2) as usize) as u16;

    let chunks =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(right_width)]).split(area);
    frame.render_widget(
        Paragraph::new(format!("  {left}")).style(left_style),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(right)
            .style(Style::default().fg(SUBTLE))
            .alignment(Alignment::Right),
        chunks[1],
    );
}

fn permission_mode_label(mode: clawedcode_core::permissions::PermissionMode) -> &'static str {
    match mode {
        clawedcode_core::permissions::PermissionMode::Default => "default",
        clawedcode_core::permissions::PermissionMode::AcceptEdits => {
            "⏵⏵ accept edits on (shift+tab to cycle)"
        }
        clawedcode_core::permissions::PermissionMode::Plan => "⏸ plan mode on (shift+tab to cycle)",
        clawedcode_core::permissions::PermissionMode::Bypass => {
            "⏵⏵ bypass permissions on (shift+tab to cycle)"
        }
    }
}

/// Mode colors from the reference dark theme: autoAccept violet, planMode
/// sage, bypass red.
fn permission_mode_color(mode: clawedcode_core::permissions::PermissionMode) -> Color {
    match mode {
        clawedcode_core::permissions::PermissionMode::Default => Color::White,
        clawedcode_core::permissions::PermissionMode::AcceptEdits => AUTO_ACCEPT_FG,
        clawedcode_core::permissions::PermissionMode::Plan => PLAN_MODE_FG,
        clawedcode_core::permissions::PermissionMode::Bypass => ERR_FG,
    }
}

/// Esc during a running turn: keep the user's prompt + an interrupted marker
/// in the session and drop the worker's event channel.
fn interrupt_active_turn(
    ctx: &mut TuiContext,
    handler: &mut ReplHandler,
    active_turn: &mut Option<ActiveTurn>,
    awaiting_approval: &mut Option<ApprovalRequest>,
    queued_prompts: &mut Vec<String>,
) {
    *awaiting_approval = None;
    queued_prompts.clear();

    if let Some(prompt) = handler.live_user_prompt.take() {
        ctx.session_mut().push(Role::User, prompt);
        let mut blocks = Vec::new();
        let partial = handler.live_assistant_text.trim();
        if !partial.is_empty() {
            blocks.push(ContentBlock::text(partial));
        }
        blocks.push(ContentBlock::text("[Interrupted by user]"));
        ctx.session_mut().push_blocks(Role::Assistant, blocks);
    }

    handler.clear_live_turn();
    *active_turn = None;
    handler.push_overlay("[warn] Interrupted by user");
    handler.rebuild_from_session(ctx.session(), ctx.show_thinking);
}

fn render_footer_bar(frame: &mut Frame, area: Rect, left: &str, right: Option<&str>) {
    if let Some(right) = right.filter(|value| !value.is_empty()) {
        let right_width = right
            .chars()
            .count()
            .min(area.width.saturating_sub(1) as usize) as u16;
        let chunks =
            Layout::horizontal([Constraint::Min(1), Constraint::Length(right_width)]).split(area);
        frame.render_widget(
            Paragraph::new(left.to_string()).style(Style::default().fg(MUTED)),
            chunks[0],
        );
        frame.render_widget(
            Paragraph::new(right.to_string())
                .style(Style::default().fg(MUTED))
                .alignment(Alignment::Right),
            chunks[1],
        );
    } else {
        frame.render_widget(
            Paragraph::new(left.to_string()).style(Style::default().fg(MUTED)),
            area,
        );
    }
}

fn render_transcript_footer(frame: &mut Frame, area: Rect, state: &TranscriptModeState) {
    let (left, right, _) = transcript_footer_parts(state);
    render_footer_bar(frame, area, &left, right.as_deref());
}

fn append_panel_section(lines: &mut Vec<String>, title: &str, body: Vec<String>) {
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.push(format!("{title}:"));
    for line in body {
        lines.push(format!("  - {line}"));
    }
}

fn transcript_footer_parts(state: &TranscriptModeState) -> (String, Option<String>, String) {
    let status_right = if let Some(status) = &state.export_status {
        Some(status.clone())
    } else {
        transcript_search_badge(state).map(|(current, total)| format!("{current}/{total}"))
    };

    let status_left = "Showing detailed transcript · Ctrl+O to return".to_string();
    let hint = if state.dump_mode {
        "Dump mode · ↑/↓/PgUp/PgDn/Home/End scroll · q exits".to_string()
    } else if state.search_open {
        "Search transcript · Enter commits · Esc cancels".to_string()
    } else if state.search_matches.is_empty() {
        "/ search · v export · [ dump · ↑/↓/PgUp/PgDn/Home/End scroll".to_string()
    } else {
        "/ search · n/N navigate · v export · [ dump · ↑/↓/PgUp/PgDn/Home/End scroll".to_string()
    };

    (status_left, status_right, hint)
}

#[cfg_attr(not(test), allow(dead_code))]
fn transcript_footer_status_line(state: &TranscriptModeState) -> String {
    let (left, right, _) = transcript_footer_parts(state);
    match right {
        Some(right) => format!("{left} · {right}"),
        None => left,
    }
}

fn transcript_footer_hint_line(state: &TranscriptModeState) -> String {
    let (_, _, hint) = transcript_footer_parts(state);
    hint
}

fn transcript_search_badge(state: &TranscriptModeState) -> Option<(usize, usize)> {
    if state.search_query.is_empty() || state.search_matches.is_empty() {
        None
    } else {
        Some((state.search_index + 1, state.search_matches.len()))
    }
}

fn transcript_search_current(state: &TranscriptModeState) -> Option<usize> {
    state.search_matches.get(state.search_index).copied()
}

fn row_text(row: &RenderRow) -> String {
    match row {
        RenderRow::User(text) => format!("> {text}"),
        RenderRow::Assistant(text) => format!("{BULLET} {text}"),
        RenderRow::Thinking(text) => format!("{BULLET} {text}"),
        RenderRow::ToolUse { name, input } => {
            let arg = tool_display_arg(name, input);
            format!("{BULLET} {}({arg})", tool_display_name(name))
        }
        RenderRow::ToolStatus(text) => format!("{HOOK_PREFIX}{text}"),
        RenderRow::ToolResult {
            tool_use_id: _,
            content,
            is_error: _,
        } => format!("{HOOK_PREFIX}{}", content.lines().next().unwrap_or("")),
        RenderRow::SubagentSummary {
            child_session_id,
            summary,
        } => format!("{BULLET} Task({child_session_id})\n{HOOK_PREFIX}{summary}"),
        RenderRow::Info(text) => text.clone(),
        RenderRow::Spacer => String::new(),
    }
}

fn render_rows(
    rows: &[RenderRow],
    selected_index: Option<usize>,
    search_matches: &[usize],
    expanded_tools: &std::collections::HashSet<String>,
    width: usize,
) -> Vec<Line<'static>> {
    rows.iter()
        .enumerate()
        .flat_map(|(index, row)| {
            let is_selected = selected_index == Some(index);
            let is_match = search_matches.contains(&index);

            let mut lines = match row {
                RenderRow::Spacer => vec![Line::from(String::new())],
                RenderRow::User(text) => render_user_message(text, width),
                RenderRow::Assistant(text) => {
                    let mut result = Vec::new();
                    if text.is_empty() {
                        result.push(Line::from(Span::styled(
                            BULLET,
                            Style::default().add_modifier(Modifier::BOLD),
                        )));
                    } else {
                        let parsed = crate::markdown::parse_markdown(text);
                        for (index, line) in parsed.into_iter().enumerate() {
                            let mut spans = Vec::with_capacity(line.spans.len() + 1);
                            if index == 0 {
                                spans.push(Span::styled(
                                    format!("{BULLET} "),
                                    Style::default().add_modifier(Modifier::BOLD),
                                ));
                            }
                            spans.extend(line.spans);
                            result.push(Line::from(spans));
                        }
                    }
                    result
                }
                RenderRow::Thinking(text) => {
                    let mut result = Vec::new();
                    for (index, line) in text.lines().enumerate() {
                        let prefix = if index == 0 {
                            format!("{BULLET} ")
                        } else {
                            "  ".to_string()
                        };
                        result.push(Line::from(vec![
                            Span::styled(prefix, Style::default().fg(DIM)),
                            Span::styled(
                                line.to_string(),
                                Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
                            ),
                        ]));
                    }
                    result
                }
                RenderRow::ToolUse { name, input } => {
                    let arg = tool_display_arg(name, input);
                    vec![Line::from(vec![
                        Span::styled(
                            format!("{BULLET} "),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            tool_display_name(name).to_string(),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(format!("({arg})"), Style::default().fg(DIM)),
                    ])]
                }
                RenderRow::ToolStatus(text) => vec![Line::from(Span::styled(
                    format!("{HOOK_PREFIX}{text}"),
                    Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
                ))],
                RenderRow::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => render_tool_result(tool_use_id, content, *is_error, expanded_tools),
                RenderRow::SubagentSummary {
                    child_session_id,
                    summary,
                } => {
                    let mut result = vec![Line::from(vec![
                        Span::styled(
                            format!("{BULLET} "),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            "Task".to_string(),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(format!("({child_session_id})"), Style::default().fg(DIM)),
                    ])];
                    for line in summary.lines() {
                        result.push(Line::from(vec![
                            Span::styled(HOOK_PREFIX, Style::default().fg(DIM)),
                            Span::styled(line.to_string(), Style::default().fg(DIM)),
                        ]));
                    }
                    result
                }
                RenderRow::Info(text) => vec![info_line(text)],
            };

            // Apply selection/match highlighting
            if is_selected || is_match {
                let fg = if is_selected { ACCENT } else { Color::Yellow };
                for line in &mut lines {
                    for span in &mut line.spans {
                        span.style = span.style.fg(fg);
                    }
                }
            }

            lines
        })
        .collect()
}

/// User messages render as a full-width band on `userMessageBackground`
/// (rgb(55,55,55) in the dark theme), text only — no `>` prefix.
fn render_user_message(text: &str, width: usize) -> Vec<Line<'static>> {
    let style = Style::default().bg(USER_MSG_BG);
    let text_style = Style::default().fg(Color::White).bg(USER_MSG_BG);
    let mut result = Vec::new();
    let push_band = |result: &mut Vec<Line<'static>>, raw: &str| {
        let text_len = raw.chars().count() + 1; // 1-char right padding
        let pad = width.saturating_sub(text_len);
        result.push(Line::from(vec![
            Span::styled(raw.to_string(), text_style),
            Span::styled(" ".repeat(pad + 1), style),
        ]));
    };

    if text.is_empty() {
        push_band(&mut result, " ");
        return result;
    }

    for raw in text.lines() {
        push_band(&mut result, raw);
    }
    result
}

/// Render a tool result as Claude Code does: collapsed results show the
/// first line plus an expand hint, expanded results list every line under
/// the `⎿` hook. Diff-looking lines get +/- coloring.
fn render_tool_result(
    tool_use_id: &str,
    content: &str,
    is_error: bool,
    expanded_tools: &std::collections::HashSet<String>,
) -> Vec<Line<'static>> {
    let is_expanded = expanded_tools.contains(tool_use_id);
    let body_color = if is_error { ERR_FG } else { DIM };
    let hook_color = if is_error { ERR_FG } else { MUTED };

    if content.trim().is_empty() {
        return vec![Line::from(vec![
            Span::styled(HOOK_PREFIX, Style::default().fg(hook_color)),
            Span::styled(
                if is_error {
                    "Error: (no content)"
                } else {
                    "(No content)"
                },
                Style::default()
                    .fg(body_color)
                    .add_modifier(Modifier::ITALIC),
            ),
        ])];
    }

    let content_lines: Vec<&str> = content.lines().collect();

    if !is_expanded {
        let mut result = Vec::new();
        let first = content_lines[0];
        result.push(Line::from(vec![
            Span::styled(HOOK_PREFIX, Style::default().fg(hook_color)),
            Span::styled(
                truncate_for_display(first.trim(), 100),
                Style::default().fg(body_color),
            ),
        ]));
        if content_lines.len() > 1 {
            result.push(Line::from(vec![
                Span::styled(HOOK_CONTINUATION, Style::default().fg(hook_color)),
                Span::styled(
                    format!("… +{} lines (ctrl+o to expand)", content_lines.len() - 1),
                    Style::default().fg(SUBTLE).add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
        return result;
    }

    content_lines
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            let trimmed = raw.trim_start();
            let line_style = if is_error {
                Style::default().fg(ERR_FG)
            } else if trimmed.starts_with('+') {
                Style::default().fg(OK_FG)
            } else if trimmed.starts_with('-') {
                Style::default().fg(ERR_FG)
            } else {
                Style::default().fg(DIM)
            };
            let prefix = if index == 0 {
                HOOK_PREFIX
            } else {
                HOOK_CONTINUATION
            };
            Line::from(vec![
                Span::styled(prefix, Style::default().fg(hook_color)),
                Span::styled(raw.to_string(), line_style),
            ])
        })
        .collect()
}

/// Style overlay/status rows by tag: [error]/[warn] get their colors,
/// everything else renders dim.
fn info_line(text: &str) -> Line<'static> {
    let (color, rest) = if let Some(rest) = text.strip_prefix("[error] ") {
        (ERR_FG, rest)
    } else if let Some(rest) = text.strip_prefix("[warn] ") {
        (WARN_FG, rest)
    } else if let Some(rest) = text.strip_prefix("[info] ") {
        (DIM, rest)
    } else {
        (DIM, text)
    };
    Line::from(Span::styled(rest.to_string(), Style::default().fg(color)))
}

/// Map internal tool names to Claude Code display names.
fn tool_display_name(name: &str) -> &str {
    match name {
        "shell" => "Bash",
        "read_file" => "Read",
        "apply_patch" => "Edit",
        other => other,
    }
}

/// Produce the short argument shown inside `ToolName(...)` parens.
fn tool_display_arg(name: &str, raw_input: &str) -> String {
    let input: serde_json::Value =
        serde_json::from_str(raw_input).unwrap_or(serde_json::Value::Null);
    let get_str = |keys: &[&str]| -> Option<String> {
        for key in keys {
            if let Some(value) = input.get(*key).and_then(|v| v.as_str()) {
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
        None
    };
    let first_line = |s: String| s.lines().next().unwrap_or("").to_string();

    let arg = match name {
        "shell" => get_str(&["command"]).map(first_line),
        "read_file" => get_str(&["path", "file_path"]),
        "apply_patch" => patch_display_target(raw_input),
        "Agent" | "Task" => get_str(&["description", "prompt"]).map(first_line),
        "TaskCreate" => get_str(&["subject"]).map(first_line),
        "TaskUpdate" => get_str(&["taskId", "task_id"]).map(|id| {
            let subject = get_str(&["subject"]);
            match subject {
                Some(subject) => format!("{id}: {}", first_line(subject)),
                None => id,
            }
        }),
        "TaskGet" | "TaskOutput" | "TaskStop" => get_str(&["task_id", "taskId", "shell_id"]),
        "ListMcpResourcesTool" | "TaskList" => None,
        "ReadMcpResourceTool" => get_str(&["uri"]),
        _ => None,
    };

    if let Some(arg) = arg {
        return truncate_for_display(&arg, 64);
    }

    // Fallback: compact JSON of the first string/number field.
    if let Some(map) = input.as_object() {
        if map.is_empty() {
            return String::new();
        }
        if let Some((key, value)) = map.iter().next() {
            let text = match value {
                serde_json::Value::String(s) => first_line(s.clone()),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            return truncate_for_display(&format!("{key}: {text}"), 64);
        }
    }

    truncate_for_display(raw_input, 64)
}

/// Extract the file path an apply_patch input touches for display.
fn patch_display_target(raw_input: &str) -> Option<String> {
    let input: serde_json::Value = serde_json::from_str(raw_input).ok()?;
    let patch = input.get("patch")?.as_str()?;
    for line in patch.lines() {
        if let Some(rest) = line
            .strip_prefix("*** Update File: ")
            .or_else(|| line.strip_prefix("*** Add File: "))
            .or_else(|| line.strip_prefix("*** Delete File: "))
        {
            return Some(rest.trim().to_string());
        }
    }
    None
}

fn max_transcript_scroll(handler: &ReplHandler, viewport_height: usize) -> usize {
    handler
        .visible_lines()
        .len()
        .saturating_sub(viewport_height)
}

fn max_scroll_for_lines(line_count: usize, viewport_height: usize) -> usize {
    line_count.saturating_sub(viewport_height)
}

fn transcript_mode_refresh_search(state: &mut TranscriptModeState) {
    state.search_matches = transcript_search_matches(&state.frozen_rows, &state.search_query);
    if state.search_matches.is_empty() {
        state.search_index = 0;
        return;
    }
    if state.search_index >= state.search_matches.len() {
        state.search_index = 0;
    }
    state.viewport.scroll_offset = state.search_matches[state.search_index];
}

fn transcript_search_matches(rows: &[RenderRow], query: &str) -> Vec<usize> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let needle = trimmed.to_ascii_lowercase();
    rows.iter()
        .enumerate()
        .filter_map(|(idx, row)| {
            row_text(row)
                .to_ascii_lowercase()
                .contains(&needle)
                .then_some(idx)
        })
        .collect()
}

fn export_transcript_lines(rows: &[RenderRow]) -> Result<String> {
    let path = env::temp_dir().join(format!(
        "clawedcode-transcript-{}.txt",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    ));
    fs::write(
        &path,
        rows.iter().map(row_text).collect::<Vec<_>>().join("\n"),
    )?;

    if let Some(editor) = env::var("VISUAL").ok().or_else(|| env::var("EDITOR").ok()) {
        let command = format!(r#"{editor} "{}""#, path.display());
        Command::new("sh").arg("-lc").arg(command).spawn()?;
        Ok(format!("opening {}", path.display()))
    } else {
        Ok(format!("wrote {}", path.display()))
    }
}

fn enter_transcript_mode(handler: &ReplHandler, viewport_height: usize) -> TranscriptModeState {
    let frozen_rows = handler.visible_rows();
    let max_scroll = frozen_rows.len().saturating_sub(viewport_height);
    TranscriptModeState {
        frozen_rows,
        viewport: TranscriptViewport {
            scroll_offset: max_scroll,
            pinned_to_bottom: true,
            unseen_count: 0,
        },
        ..TranscriptModeState::default()
    }
}

fn update_transcript_search(state: &mut TranscriptModeState, query: String) {
    state.search_query = query;
    state.search_index = 0;
    transcript_mode_refresh_search(state);
}

fn navigate_transcript_search(state: &mut TranscriptModeState, forward: bool) {
    if state.search_matches.is_empty() {
        return;
    }

    let len = state.search_matches.len();
    state.search_index = if forward {
        (state.search_index + 1) % len
    } else if state.search_index == 0 {
        len - 1
    } else {
        state.search_index - 1
    };

    state.viewport.scroll_offset = state.search_matches[state.search_index];
    state.viewport.pinned_to_bottom = false;
}

fn transcript_render_lines(state: &TranscriptModeState, width: usize) -> Vec<Line<'static>> {
    // Transcript mode is the "expanded" view — every tool result renders in
    // full (this is what "ctrl+o to expand" means in the reference).
    let expanded: std::collections::HashSet<String> = state
        .frozen_rows
        .iter()
        .filter_map(|row| match row {
            RenderRow::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
            _ => None,
        })
        .collect();
    render_rows(
        &state.frozen_rows,
        transcript_search_current(state),
        &state.search_matches,
        &expanded,
        width,
    )
}

/// Queued prompts preview above the input area — dimmed echoes of what will
/// run next, matching the queued-commands strip in the reference.
fn queued_prompt_preview_lines(queued_prompts: &[String], width: usize) -> Vec<Line<'static>> {
    let visible_count = queued_prompts.len().min(MAX_VISIBLE_QUEUED_PROMPTS);
    let mut lines = Vec::new();
    for prompt in queued_prompts.iter().take(visible_count) {
        lines.extend(render_user_message(
            &truncate_for_display(prompt, 96),
            width,
        ));
    }

    if queued_prompts.len() > visible_count {
        lines.push(Line::from(Span::styled(
            format!("  … +{} more", queued_prompts.len() - visible_count),
            Style::default().fg(SUBTLE),
        )));
    }

    lines
}

fn maybe_submit_queued_prompt(
    ctx: &mut TuiContext,
    handler: &mut ReplHandler,
    active_turn: &mut Option<ActiveTurn>,
    queued_prompts: &mut Vec<String>,
    always_allowed: &Arc<Mutex<std::collections::HashSet<String>>>,
) -> Result<()> {
    if active_turn.is_some() || queued_prompts.is_empty() {
        return Ok(());
    }

    let prompt = queued_prompts.remove(0);
    if handle_slash_command(ctx, handler, &prompt, active_turn, always_allowed)? {
        handler.rebuild_from_session(ctx.session(), ctx.show_thinking);
    } else if let Some(command) = prompt.strip_prefix('!').map(str::trim)
        && !command.is_empty()
    {
        handler.begin_live_turn(prompt.clone());
        *active_turn = Some(spawn_bash_worker(ctx.cloned_session(), command.to_string()));
    } else {
        handler.begin_live_turn(prompt.clone());
        *active_turn = Some(spawn_turn_worker(
            ctx,
            prompt.clone(),
            expand_at_mentions(&prompt, &ctx.session().cwd),
            always_allowed.clone(),
        ));
    }

    Ok(())
}

fn welcome_right_lines(ctx: &TuiContext) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    if let Some(label) = ctx.transport_label() {
        lines.push(section_title("Connection"));
        lines.push(Line::from(Span::styled(
            format!("Interactive transport: {label}"),
            Style::default().fg(DIM),
        )));
        lines.push(Line::from(""));
    }

    if should_show_project_onboarding(&ctx.session().cwd) {
        lines.push(section_title("Get started"));
        for step in onboarding_steps(&ctx.session().cwd)
            .into_iter()
            .filter(|step| step.is_enabled)
        {
            let (mark, style) = if step.is_complete {
                ("☑", Style::default().fg(DIM))
            } else {
                ("☐", Style::default())
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{mark} "), style),
                Span::styled(step.text.to_string(), style),
            ]));
        }
    } else {
        lines.push(section_title("Tips for getting started"));
        let tips = [
            "Run /init to create a CLAUDE.md file with instructions for ClawedCode",
            "Ask ClawedCode to make changes, fix bugs, or add features",
            "Use /help to see all available commands",
        ];
        for (index, tip) in tips.iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(format!("{}. ", index + 1), Style::default().fg(DIM)),
                Span::styled((*tip).to_string(), Style::default().fg(MUTED)),
            ]));
        }
    }

    if launched_in_home(&ctx.session().cwd) {
        lines.push(Line::from(Span::styled(
            "⚠ You launched clawedcode in your home directory — run it inside a project directory instead.",
            Style::default().fg(WARN_FG),
        )));
    }

    lines.push(Line::from(""));
    lines.push(section_title("Recent activity"));

    let activity = recent_activity(ctx, 3);
    if activity.is_empty() {
        lines.push(Line::from(Span::styled(
            "No recent activity",
            Style::default().fg(MUTED),
        )));
    } else {
        for item in activity {
            lines.push(Line::from(item));
        }
    }

    lines
}

fn section_title(title: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        format!("{title}:"),
        Style::default().add_modifier(Modifier::BOLD),
    ))
}

#[derive(Default)]
struct TranscriptTurn {
    rows: Vec<RenderRow>,
}

fn flush_turn_rows(rows: &mut Vec<RenderRow>, turn: &mut TranscriptTurn) {
    if turn.rows.is_empty() {
        return;
    }
    if !rows.is_empty() && !matches!(rows.last(), Some(RenderRow::Spacer)) {
        rows.push(RenderRow::Spacer);
    }
    rows.append(&mut turn.rows);
}

fn append_message_to_turn(turn: &mut TranscriptTurn, msg: &Message, show_thinking: bool) {
    match msg.role {
        Role::System => {}
        Role::User => {
            append_message_blocks(turn, "You", &msg.content_blocks, show_thinking, true);
        }
        Role::Assistant => {
            append_message_blocks(
                turn,
                "ClawedCode",
                &msg.content_blocks,
                show_thinking,
                false,
            );
        }
        Role::Tool => {
            append_message_blocks(turn, "Tool", &msg.content_blocks, show_thinking, false);
        }
    }
}

fn append_message_blocks(
    turn: &mut TranscriptTurn,
    role_prefix: &str,
    blocks: &[ContentBlock],
    show_thinking: bool,
    add_spacing_after: bool,
) {
    let mut wrote_block = false;
    let has_toolish_block = blocks.iter().any(|block| {
        matches!(
            block,
            ContentBlock::ToolUse { .. }
                | ContentBlock::ToolResult { .. }
                | ContentBlock::SubAgentSummary { .. }
        )
    });

    for block in blocks {
        match block {
            ContentBlock::Text { text } => {
                let text = text.trim();
                if role_prefix == "You" && text.starts_with("<bash-input>") {
                    // Bash-mode entries recorded by `!cmd` runs — render like the live turn.
                    let command = text
                        .strip_prefix("<bash-input>")
                        .and_then(|t| t.split("</bash-input>").next())
                        .unwrap_or("")
                        .trim();
                    turn.rows.push(RenderRow::User(format!("! {command}")));
                    turn.rows.push(RenderRow::ToolUse {
                        name: "shell".to_string(),
                        input: format!("{{\"command\":{command:?}}}"),
                    });
                    if let Some(output) = text.split("<bash-output>").nth(1) {
                        let output = output.trim_end_matches("</bash-output>").trim_end();
                        if !output.is_empty() {
                            turn.rows.push(RenderRow::ToolResult {
                                tool_use_id: String::new(),
                                content: output.to_string(),
                                is_error: false,
                            });
                        }
                    }
                    wrote_block = true;
                } else if !text.is_empty() {
                    turn.rows.push(match role_prefix {
                        "You" => RenderRow::User(text.to_string()),
                        "ClawedCode" => RenderRow::Assistant(text.to_string()),
                        _ => RenderRow::Info(format!("{role_prefix}: {text}")),
                    });
                    wrote_block = true;
                }
            }
            ContentBlock::Thinking { thinking } => {
                if show_thinking && !thinking.starts_with(TRANSPORT_SESSION_ID_MARKER_PREFIX) {
                    let thinking = thinking.trim();
                    if !thinking.is_empty() {
                        turn.rows.push(RenderRow::Thinking(thinking.to_string()));
                        wrote_block = true;
                    }
                }
            }
            ContentBlock::ToolUse { name, input, .. } => {
                turn.rows.push(RenderRow::ToolUse {
                    name: name.clone(),
                    input: serde_json::to_string(input).unwrap_or_default(),
                });
                wrote_block = true;
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                turn.rows.push(RenderRow::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    content: content.clone(),
                    is_error: *is_error,
                });
                wrote_block = true;
            }
            ContentBlock::SubAgentSummary {
                child_session_id,
                summary,
            } => {
                turn.rows.push(RenderRow::SubagentSummary {
                    child_session_id: child_session_id[..8.min(child_session_id.len())].to_string(),
                    summary: summary.clone(),
                });
                wrote_block = true;
            }
        }
    }

    if wrote_block && add_spacing_after && !has_toolish_block {
        turn.rows.push(RenderRow::Spacer);
    }
}

/// Runs a `!cmd` bash-mode command directly, bypassing the model, and records
/// `<bash-input>`/`<bash-output>` context in the session like Claude Code does.
fn spawn_bash_worker(session: Session, command: String) -> ActiveTurn {
    let (event_tx, event_rx) = mpsc::channel();
    let (approval_tx, _approval_rx) = mpsc::channel::<bool>();
    let join = thread::spawn(move || {
        let mut session = session;
        let tool_use_id = format!("bash-mode-{}", uuid::Uuid::new_v4());
        let _ = event_tx.send(TurnWorkerEvent::Ui(TuiEvent::ToolUse {
            id: tool_use_id.clone(),
            name: "shell".to_string(),
            input: serde_json::json!({ "command": command }),
        }));
        let (content, is_error) = match std::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .output()
        {
            Ok(output) => {
                let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&output.stderr));
                if text.is_empty() && output.status.success() {
                    text.push_str("(no output)");
                }
                (text, !output.status.success())
            }
            Err(err) => (format!("failed to run command: {err}"), true),
        };
        let _ = event_tx.send(TurnWorkerEvent::Ui(TuiEvent::ToolResult {
            tool_use_id,
            content: content.clone(),
            is_error,
        }));
        let _ = event_tx.send(TurnWorkerEvent::Ui(TuiEvent::TurnComplete));
        session.push(
            Role::User,
            format!("<bash-input>{command}</bash-input>\n<bash-output>{content}</bash-output>"),
        );
        let _ = event_tx.send(TurnWorkerEvent::Finished(session));
    });
    ActiveTurn {
        events: event_rx,
        approvals: approval_tx,
        _join: join,
    }
}

fn spawn_turn_worker(
    ctx: &TuiContext,
    visible_prompt: String,
    execution_prompt: Option<String>,
    always_allowed: Arc<Mutex<std::collections::HashSet<String>>>,
) -> ActiveTurn {
    let runtime = ctx.replacement_runtime();
    let external_turn_executor = ctx.external_turn_executor();
    let permission_mode = ctx.permission_mode();
    let mut session = ctx.cloned_session();
    let (event_tx, event_rx) = mpsc::channel();
    let (approval_tx, approval_rx) = mpsc::channel();
    let approval_rx = Arc::new(Mutex::new(approval_rx));

    let join = thread::spawn(move || {
        let approval_rx = approval_rx.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(executor) = external_turn_executor {
                session.push(Role::User, visible_prompt.clone());
                let tx = event_tx.clone();
                let mut on_event = move |event: TuiEvent| {
                    let _ = tx.send(TurnWorkerEvent::Ui(event));
                };
                let tx = event_tx.clone();
                let approval_rx = approval_rx.clone();
                let always_allowed = always_allowed.clone();
                let permission_engine =
                    clawedcode_core::permissions::PermissionEngine::new(permission_mode);
                let mut request_approval = move |request: ApprovalRequest| {
                    match permission_engine.decide(true, is_write_like(&request.tool_name)) {
                        clawedcode_core::permissions::PermissionDecision::Allow => return true,
                        clawedcode_core::permissions::PermissionDecision::Deny => return false,
                        clawedcode_core::permissions::PermissionDecision::Ask => {}
                    }
                    if always_allowed
                        .lock()
                        .map(|set| set.contains(&request.tool_name))
                        .unwrap_or(false)
                    {
                        return true;
                    }

                    if tx
                        .send(TurnWorkerEvent::ApprovalRequested(request))
                        .is_err()
                    {
                        return false;
                    }

                    approval_rx
                        .lock()
                        .ok()
                        .and_then(|rx| rx.recv().ok())
                        .unwrap_or(false)
                };
                match executor.submit_turn(
                    &mut session,
                    &visible_prompt,
                    execution_prompt.as_deref(),
                    &mut on_event,
                    &mut request_approval,
                ) {
                    Ok(result) => {
                        let mut assistant_blocks = Vec::new();
                        if let Some(transport_session_id) = result.transport_session_id {
                            session.transport_session_id = Some(transport_session_id);
                            assistant_blocks.push(ContentBlock::thinking(
                                transport_session_id_marker(
                                    session.transport_session_id.as_deref().unwrap(),
                                ),
                            ));
                        }
                        if !result.response.is_empty() {
                            assistant_blocks.insert(0, ContentBlock::text(result.response));
                        }
                        if assistant_blocks.is_empty() {
                            assistant_blocks.push(ContentBlock::text(""));
                        }
                        session.push_blocks(Role::Assistant, assistant_blocks);
                    }
                    Err(err) => {
                        let _ = event_tx.send(TurnWorkerEvent::Failed(format!(
                            "interactive transport turn failed: {err}"
                        )));
                        return;
                    }
                }

                let _ = event_tx.send(TurnWorkerEvent::Ui(TuiEvent::TurnComplete));
                let _ = event_tx.send(TurnWorkerEvent::Finished(session));
                return;
            }

            let tx = event_tx.clone();
            let approval_fn =
                move |tool_use_id: &str, tool_name: &str, input: &serde_json::Value| {
                    if !is_write_like(tool_name) {
                        return true;
                    }

                    if always_allowed
                        .lock()
                        .map(|set| set.contains(tool_name))
                        .unwrap_or(false)
                    {
                        return true;
                    }

                    let request = ApprovalRequest {
                        tool_use_id: tool_use_id.to_string(),
                        tool_name: tool_name.to_string(),
                        input: input.clone(),
                    };

                    if tx
                        .send(TurnWorkerEvent::ApprovalRequested(request))
                        .is_err()
                    {
                        return false;
                    }

                    approval_rx
                        .lock()
                        .ok()
                        .and_then(|rx| rx.recv().ok())
                        .unwrap_or(false)
                };

            let tx = event_tx.clone();
            let runtime_result = {
                let future = runtime.submit_stream_with_prompt_override_and_approval(
                    &mut session,
                    &visible_prompt,
                    execution_prompt.as_deref(),
                    |event| {
                        if let Some(tui_event) = to_tui_event(event) {
                            let _ = tx.send(TurnWorkerEvent::Ui(tui_event));
                        }
                    },
                    &approval_fn,
                );

                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to build tokio runtime");
                rt.block_on(future)
            };

            let _ = event_tx.send(TurnWorkerEvent::Ui(TuiEvent::AssistantDone));
            let _ = event_tx.send(TurnWorkerEvent::Ui(TuiEvent::TurnComplete));
            let _ = runtime_result;
            let _ = event_tx.send(TurnWorkerEvent::Finished(session));
        }));

        if result.is_err() {
            let _ = event_tx.send(TurnWorkerEvent::Failed(
                "interactive turn worker panicked".to_string(),
            ));
        }
    });

    ActiveTurn {
        events: event_rx,
        approvals: approval_tx,
        _join: join,
    }
}

fn drain_turn_events(
    ctx: &mut TuiContext,
    handler: &mut ReplHandler,
    active_turn: &mut Option<ActiveTurn>,
    awaiting_approval: &mut Option<ApprovalRequest>,
) {
    let mut finished = false;

    if let Some(turn) = active_turn.as_mut() {
        while let Ok(event) = turn.events.try_recv() {
            match event {
                TurnWorkerEvent::Ui(event) => handler.on_event(&event),
                TurnWorkerEvent::ApprovalRequested(request) => {
                    handler.record_live_tool_pending_approval(&request.tool_use_id);
                    *awaiting_approval = Some(request);
                    handler.state = AppState::AwaitingApproval;
                }
                TurnWorkerEvent::Finished(session) => {
                    ctx.replace_session(session);
                    maybe_mark_project_onboarding_complete(&ctx.session().cwd);
                    handler.clear_live_turn();
                    handler.rebuild_from_session(ctx.session(), ctx.show_thinking);
                    let _ = ctx.save_session();
                    *awaiting_approval = None;
                    finished = true;
                }
                TurnWorkerEvent::Failed(message) => {
                    handler.clear_live_turn();
                    handler.push_overlay(format!("[error] {message}"));
                    *awaiting_approval = None;
                    finished = true;
                }
            }
        }
    }

    if finished {
        *active_turn = None;
    }
}

fn to_tui_event(event: &ApiEvent) -> Option<TuiEvent> {
    match event {
        ApiEvent::ThinkingDelta { text } => Some(TuiEvent::ThinkingDelta { text: text.clone() }),
        ApiEvent::MessageDelta { text } => Some(TuiEvent::MessageDelta { text: text.clone() }),
        ApiEvent::ToolUse { tool_use } => Some(TuiEvent::ToolUse {
            id: tool_use.id.clone(),
            name: tool_use.name.clone(),
            input: decode_tool_input(&tool_use.name, &tool_use.input),
        }),
        ApiEvent::ToolResult { tool_result } => Some(TuiEvent::ToolResult {
            tool_use_id: tool_result.tool_use_id.clone(),
            content: tool_result.content.clone(),
            is_error: tool_result.is_error,
        }),
        ApiEvent::Usage { .. } | ApiEvent::Completed => None,
    }
}

fn handle_slash_command(
    ctx: &mut TuiContext,
    handler: &mut ReplHandler,
    prompt: &str,
    active_turn: &mut Option<ActiveTurn>,
    always_allowed: &Arc<Mutex<std::collections::HashSet<String>>>,
) -> Result<bool> {
    if !prompt.starts_with('/') && prompt.trim() != "?" {
        return Ok(false);
    }

    let command = if prompt.trim() == "?" {
        "/help"
    } else {
        prompt.split_whitespace().next().unwrap_or(prompt)
    };

    if let Err(err) = ctx.refresh_compatibility() {
        handler.push_overlay(format!("[warn] Failed to refresh commands: {err}"));
    }

    match find_visible_command_entry(ctx, command) {
        Some(entry) => match entry.source {
            CommandSource::BuiltIn => match entry.name.as_str() {
                "/help" => {
                    let panel = help_panel(ctx);
                    handler.show_panel(panel.title, panel.lines);
                    let _ = ctx.save_session();
                    return Ok(true);
                }
                "/clear" => {
                    handler.overlay_rows.clear();
                    handler.clear_panel();
                    let _ = ctx.save_session();
                    return Ok(true);
                }
                "/update" => {
                    let install_method = clawedcode_core::update::detect_install_method();
                    if !update_is_supported(install_method) {
                        let msg = match install_method {
                            InstallMethod::LocalBuild => "[info] Self-update is not supported for local builds. Install via cargo or npm to enable updates.".to_string(),
                            InstallMethod::Unknown => "[info] Could not determine install method. Set CLAWEDCODE_INSTALL_METHOD to cargo or npm.".to_string(),
                            _ => "[info] Self-update is not available.".to_string(),
                        };
                        handler.push_overlay(msg);
                    } else {
                        match clawedcode_core::update::run_self_update() {
                            Ok(outcome) => {
                                handler.push_overlay(format!(
                                    "[info] Updated clawedcode via {:?} using `{}`",
                                    outcome.method, outcome.command
                                ));
                            }
                            Err(err) => {
                                handler.push_overlay(format!("[info] Update failed: {err}"));
                            }
                        }
                    }
                }
                "/task" => {
                    let task_prompt = prompt
                        .strip_prefix("/task")
                        .map(str::trim)
                        .unwrap_or_default();
                    if task_prompt.is_empty() {
                        handler.push_overlay(
                            "[info] Usage: /task <prompt>. Provide a prompt for the sub-agent."
                                .to_string(),
                        );
                    } else {
                        handler.push_overlay(format!(
                            "[info] Starting background sub-agent: {}...",
                            truncate_for_display(task_prompt, 50)
                        ));
                        match ctx.spawn_subagent_background(task_prompt) {
                            Ok(result) => {
                                handler.push_overlay(format!(
                                    "[info] Sub-agent queued (session: {}, status: running)",
                                    &result.child_session_id.to_string()[..8],
                                ));
                            }
                            Err(err) => {
                                handler.push_overlay(format!("[error] Sub-agent failed: {err}"));
                            }
                        }
                    }
                    let _ = ctx.save_session();
                    return Ok(true);
                }
                "/tasks" => {
                    let panel = tasks_panel(ctx);
                    handler.show_panel(panel.title, panel.lines);
                    let _ = ctx.save_session();
                    return Ok(true);
                }
                "/sessions" => {
                    let panel = sessions_panel(ctx);
                    handler.show_panel(panel.title, panel.lines);
                    let _ = ctx.save_session();
                    return Ok(true);
                }
                "/tools" => {
                    let panel = tools_panel(ctx);
                    handler.show_panel(panel.title, panel.lines);
                    let _ = ctx.save_session();
                    return Ok(true);
                }
                "/fork" => {
                    let prefix = prompt
                        .strip_prefix("/fork")
                        .map(str::trim)
                        .unwrap_or_default();
                    if prefix.is_empty() {
                        handler
                            .push_overlay("[info] Usage: /fork <session-id-or-prefix>".to_string());
                        let _ = ctx.save_session();
                        return Ok(true);
                    }

                    let _ = ctx.save_session();
                    match resolve_saved_session(ctx, prefix) {
                        Ok(source) => {
                            let forked = Session::fork_from(&source);
                            let source_id = source.id.to_string();
                            let forked_id = forked.id.to_string();
                            let source_short = short_session_id(&source_id).to_string();
                            let forked_short = short_session_id(&forked_id).to_string();
                            ctx.replace_session(forked);
                            let _ = ctx.save_session();
                            handler.overlay_rows.clear();
                            handler.clear_panel();
                            handler.push_overlay(format!(
                                "[info] Forked {source_short} -> {forked_short}"
                            ));
                        }
                        Err(err) => {
                            handler.push_overlay(format!(
                                "[error] Fork failed: {}",
                                compact_fork_error(prefix, &err)
                            ));
                        }
                    }
                    return Ok(true);
                }
                _ => {
                    handler
                        .push_overlay(format!("[info] Unknown command `{command}`. Use `/help`."));
                }
            },
            CommandSource::Skill => {
                if let Some(skill) = find_skill_command(ctx, command) {
                    execute_skill_command(ctx, handler, prompt, skill, active_turn, always_allowed);
                    return Ok(true);
                }
                handler.push_overlay(format!("[info] Unknown command `{command}`. Use `/help`."));
            }
        },
        None => {
            handler.push_overlay(format!("[info] Unknown command `{command}`. Use `/help`."));
        }
    }

    let _ = ctx.save_session();
    Ok(true)
}

fn execute_skill_command(
    ctx: &mut TuiContext,
    handler: &mut ReplHandler,
    visible_prompt: &str,
    skill: SkillDescriptor,
    active_turn: &mut Option<ActiveTurn>,
    always_allowed: &Arc<Mutex<std::collections::HashSet<String>>>,
) {
    let args = visible_prompt
        .strip_prefix(&skill.slash_command)
        .map(str::trim)
        .unwrap_or_default();
    let execution_prompt = build_skill_execution_prompt(&skill, args);
    handler.push_overlay(format!("[info] Executing skill: {}", skill.slash_command));
    handler.begin_live_turn(visible_prompt.to_string());
    *active_turn = Some(spawn_turn_worker(
        ctx,
        visible_prompt.to_string(),
        Some(execution_prompt),
        always_allowed.clone(),
    ));
}

fn all_command_entries(ctx: &TuiContext) -> Vec<CommandEntry> {
    let install_method = clawedcode_core::update::detect_install_method();
    let mut entries = vec![
        CommandEntry {
            name: "/help".to_string(),
            description: "Show built-in and discovered commands".to_string(),
            source: CommandSource::BuiltIn,
        },
        CommandEntry {
            name: "/clear".to_string(),
            description: "Clear local transcript overlays".to_string(),
            source: CommandSource::BuiltIn,
        },
        CommandEntry {
            name: "/update".to_string(),
            description: "Update clawedcode using the detected install method".to_string(),
            source: CommandSource::BuiltIn,
        },
        CommandEntry {
            name: "/task".to_string(),
            description: "Spawn a child agent to handle a sub-task".to_string(),
            source: CommandSource::BuiltIn,
        },
        CommandEntry {
            name: "/tasks".to_string(),
            description: "Show running and completed background tasks".to_string(),
            source: CommandSource::BuiltIn,
        },
        CommandEntry {
            name: "/sessions".to_string(),
            description: "List recent saved sessions".to_string(),
            source: CommandSource::BuiltIn,
        },
        CommandEntry {
            name: "/fork".to_string(),
            description: "Fork a saved session into a new interactive session".to_string(),
            source: CommandSource::BuiltIn,
        },
        CommandEntry {
            name: "/tools".to_string(),
            description: "Show active built-in, skill, and MCP tools".to_string(),
            source: CommandSource::BuiltIn,
        },
    ];

    entries.retain(|entry| command_is_visible(entry, install_method));

    for skill in ctx
        .skills()
        .iter()
        .filter(|skill| skill_is_registerable(skill))
    {
        entries.push(CommandEntry {
            name: skill.slash_command.clone(),
            description: skill
                .description
                .clone()
                .or_else(|| skill.when_to_use.clone())
                .unwrap_or_else(|| "Discovered skill command".to_string()),
            source: CommandSource::Skill,
        });
    }

    entries
}

fn visible_command_entries(ctx: &TuiContext) -> Vec<CommandEntry> {
    all_command_entries(ctx)
}

fn find_visible_command_entry(ctx: &TuiContext, command: &str) -> Option<CommandEntry> {
    visible_command_entries(ctx)
        .into_iter()
        .find(|entry| entry.name == command)
}

/// Detects an `@token` ending at the cursor: the `@` must be at the start of
/// the buffer or follow whitespace, and the token itself contains no spaces.
/// Returns the byte offset of the `@` and the partial path query.
fn file_palette_query(input: &str, cursor: usize) -> Option<(usize, String)> {
    let before = &input[..cursor.min(input.len())];
    let at = before.rfind('@')?;
    if at > 0 && !before[..at].ends_with(char::is_whitespace) {
        return None;
    }
    let token = &before[at + 1..];
    if token.contains(char::is_whitespace) {
        return None;
    }
    Some((at, token.to_string()))
}

/// Builds the cwd-relative file index once (lazy). Skips hidden dirs and the
/// usual heavy directories, like Claude Code's file palette.
fn build_file_index(cwd: &std::path::Path) -> Vec<String> {
    const SKIP: &[&str] = &[".git", "target", "node_modules", "vendor", "dist", "build"];
    const MAX_ENTRIES: usize = 5000;
    const MAX_DEPTH: usize = 8;

    let mut entries = Vec::new();
    let mut stack = vec![(cwd.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, rel, depth)) = stack.pop() {
        if depth > MAX_DEPTH || entries.len() >= MAX_ENTRIES {
            continue;
        }
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for item in read.flatten() {
            if entries.len() >= MAX_ENTRIES {
                break;
            }
            let name = item.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || SKIP.contains(&name.as_str()) {
                continue;
            }
            let path = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let Ok(meta) = item.metadata() else {
                continue;
            };
            if meta.is_dir() {
                entries.push(format!("{path}/"));
                stack.push((item.path(), path, depth + 1));
            } else {
                entries.push(path);
            }
        }
    }
    entries.sort();
    entries
}

/// Substring-matches the file index against the `@` query (directories and
/// shallower paths rank first), capped for the palette.
fn filtered_file_entries(index: &[String], query: &str) -> Vec<String> {
    let query = query.to_lowercase();
    let mut matches: Vec<&String> = index
        .iter()
        .filter(|path| path.to_lowercase().contains(&query))
        .collect();
    matches.sort_by(|a, b| {
        a.matches('/')
            .count()
            .cmp(&b.matches('/').count())
            .then(a.cmp(b))
    });
    matches.into_iter().take(8).cloned().collect()
}

fn file_palette_height(entries: &[String]) -> u16 {
    if entries.is_empty() {
        0
    } else {
        entries.len() as u16 + 1
    }
}

fn render_file_palette(frame: &mut Frame, area: Rect, entries: &[String], selected: usize) {
    let mut lines = vec![Line::from("")];
    lines.extend(entries.iter().take(8).enumerate().map(|(index, path)| {
        let is_selected = index == selected;
        let (marker, style) = if is_selected {
            (
                "❯ ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )
        } else {
            ("  ", Style::default())
        };
        Line::from(vec![
            Span::styled(marker, style),
            Span::styled(path.clone(), style),
        ])
    }));
    frame.render_widget(Paragraph::new(lines), area);
}

/// Expands `@path` mentions into `<file>` context blocks appended to the
/// execution prompt, like Claude Code's file attachments. Unknown or
/// oversized tokens are left untouched.
fn expand_at_mentions(prompt: &str, cwd: &std::path::Path) -> Option<String> {
    const MAX_FILE_BYTES: u64 = 200 * 1024;
    let mut blocks = Vec::new();
    for token in prompt.split_whitespace() {
        let Some(rel) = token.strip_prefix('@') else {
            continue;
        };
        let rel = rel.trim_end_matches('/');
        if rel.is_empty() {
            continue;
        }
        let path = cwd.join(rel);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        blocks.push(format!("<file path=\"{rel}\">\n{content}\n</file>"));
    }
    if blocks.is_empty() {
        None
    } else {
        Some(format!("{prompt}\n\n{}", blocks.join("\n\n")))
    }
}

fn filtered_command_entries(ctx: &TuiContext, input_buffer: &str) -> Vec<CommandEntry> {
    if !input_buffer.trim_start().starts_with('/') {
        return Vec::new();
    }

    let query = input_buffer.trim();
    let entries = all_command_entries(ctx);
    if query == "/" {
        return curated_command_entries(entries);
    }

    let mut entries: Vec<_> = entries
        .into_iter()
        .filter(|entry| entry.name.starts_with(query))
        .collect();
    entries.sort_by(|a, b| compare_command_entries(query, a, b));
    entries
}

fn curated_command_entries(entries: Vec<CommandEntry>) -> Vec<CommandEntry> {
    let builtin_priority = [
        "/help",
        "/task",
        "/tasks",
        "/sessions",
        "/tools",
        "/fork",
        "/clear",
        "/update",
    ];

    let mut ranked = Vec::new();
    for name in builtin_priority {
        if let Some(index) = entries.iter().position(|entry| entry.name == name) {
            ranked.push(entries[index].clone());
        }
    }

    let mut remaining_skills: Vec<_> = entries
        .into_iter()
        .filter(|entry| matches!(entry.source, CommandSource::Skill))
        .collect();
    remaining_skills.sort_by(|a, b| a.name.cmp(&b.name));
    ranked.extend(remaining_skills);
    ranked
}

fn compare_command_entries(
    query: &str,
    left: &CommandEntry,
    right: &CommandEntry,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let exact_left = left.name == query;
    let exact_right = right.name == query;
    match exact_right.cmp(&exact_left) {
        Ordering::Equal => {}
        other => return other,
    }

    let left_builtin = matches!(left.source, CommandSource::BuiltIn);
    let right_builtin = matches!(right.source, CommandSource::BuiltIn);
    match right_builtin.cmp(&left_builtin) {
        Ordering::Equal => {}
        other => return other,
    }

    match left.name.len().cmp(&right.name.len()) {
        Ordering::Equal => left.name.cmp(&right.name),
        other => other,
    }
}

fn command_source_badge(source: CommandSource) -> (&'static str, Color) {
    match source {
        CommandSource::BuiltIn => ("[built-in]", ACCENT),
        CommandSource::Skill => ("[skill]", Color::Cyan),
    }
}

fn integrate_completed_subagents(ctx: &mut TuiContext, handler: &mut ReplHandler) {
    let completed = ctx.drain_completed_subagent_summaries();
    if completed.is_empty() {
        return;
    }

    for task in completed {
        let status = match task.status {
            SubAgentTaskStatus::Completed => "completed",
            SubAgentTaskStatus::Failed => "failed",
            SubAgentTaskStatus::Running => "running",
        };
        handler.push_overlay(format!(
            "[info] Background sub-agent {} {status} ({} tool calls)",
            &task.child_session_id.to_string()[..8],
            task.tools_executed
        ));
    }

    handler.rebuild_from_session(ctx.session(), ctx.show_thinking);
}

fn background_task_lines(ctx: &TuiContext) -> Vec<String> {
    let session_id = ctx.session().id.to_string();
    let mut lines = Vec::new();

    let mut shell_tasks: Vec<_> = list_background_tasks()
        .into_iter()
        .filter(|task| task.session_id == session_id)
        .collect();
    shell_tasks.sort_by_key(|task| task.created_at);

    for task in shell_tasks {
        lines.push(format!(
            "[shell:{status}] {id} {desc}",
            status = shell_task_status_name(task.status),
            id = &task.id[..8.min(task.id.len())],
            desc = truncate_for_display(&task.description, 48),
        ));
    }

    let subagent_tasks = list_subagent_tasks_for_parent(ctx.session().id);
    for task in subagent_tasks {
        lines.push(format_subagent_task_line(&task));
    }

    if lines.is_empty() {
        vec!["No background tasks".to_string()]
    } else {
        lines
    }
}

fn shell_task_status_name(status: ShellTaskStatus) -> &'static str {
    match status {
        ShellTaskStatus::Pending => "pending",
        ShellTaskStatus::Running => "running",
        ShellTaskStatus::Completed => "completed",
        ShellTaskStatus::Failed => "failed",
        ShellTaskStatus::Killed => "killed",
    }
}

fn format_subagent_task_line(task: &SubAgentTaskState) -> String {
    let status = match task.status {
        SubAgentTaskStatus::Running => "running",
        SubAgentTaskStatus::Completed => "completed",
        SubAgentTaskStatus::Failed => "failed",
    };
    let detail = task
        .summary
        .clone()
        .or_else(|| task.error.clone())
        .unwrap_or_else(|| truncate_for_display(&task.prompt, 48));

    format!(
        "[agent:{status}] {id} {detail}",
        id = &task.child_session_id.to_string()[..8],
        detail = truncate_for_display(&detail, 48),
    )
}

fn is_write_like(tool_name: &str) -> bool {
    matches!(tool_name, "shell" | "apply_patch")
}

fn find_skill_command(ctx: &TuiContext, command: &str) -> Option<SkillDescriptor> {
    ctx.skills()
        .iter()
        .find(|skill| skill_is_registerable(skill) && skill.slash_command == command)
        .cloned()
}

fn build_skill_execution_prompt(skill: &SkillDescriptor, args: &str) -> String {
    let args = args.trim();
    let argument_text = if args.is_empty() {
        "No explicit arguments were provided. Infer the likely task from the skill and ask for clarification only if required.".to_string()
    } else {
        format!("User arguments:\n{args}")
    };

    format!(
        "The user invoked the slash command {command}.\nTreat the following skill as active instructions for this response only.\n\nSkill name: {name}\nDescription: {description}\nWhen to use: {when}\nLegacy command: {legacy}\nPath: {path}\n\nSkill body:\n{body}\n\n{args}",
        command = skill.slash_command,
        name = skill.name,
        description = skill.description.as_deref().unwrap_or(""),
        when = skill.when_to_use.as_deref().unwrap_or(""),
        legacy = if skill.legacy_command { "yes" } else { "no" },
        path = skill.path.display(),
        body = skill.body,
        args = argument_text,
    )
}

fn provider_label() -> &'static str {
    match std::env::var("CLAWEDCODE_PROVIDER")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "anthropic" => "Anthropic-compatible API",
        "mock" => "Mock provider",
        _ => "Local session",
    }
}

fn launched_in_home(path: &Path) -> bool {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home == path)
        .unwrap_or(false)
}

fn display_path(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(home) = home.as_ref() {
        if let Ok(suffix) = path.strip_prefix(home) {
            if suffix.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", suffix.display());
        }
    }
    path.display().to_string()
}

fn recent_activity(ctx: &TuiContext, limit: usize) -> Vec<String> {
    load_saved_sessions(&ctx.sessions_dir)
        .into_iter()
        .filter(|session| session.id != ctx.session().id)
        .take(limit)
        .map(|session| {
            let summary = session
                .last_user_text()
                .map(truncate_summary)
                .unwrap_or_else(|| "No prompt".to_string());
            format!(
                "{summary} ({})",
                relative_time_label(session.updated_at.timestamp())
            )
        })
        .collect()
}

fn load_saved_sessions(sessions_dir: &Path) -> Vec<Session> {
    let Ok(entries) = fs::read_dir(sessions_dir) else {
        return Vec::new();
    };

    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(session) = serde_json::from_str::<Session>(&raw) else {
            continue;
        };
        sessions.push(session);
    }

    sessions.sort_by_key(|session| Reverse(session.updated_at.timestamp()));
    sessions
}

fn list_saved_sessions(ctx: &TuiContext, limit: usize) -> Vec<SavedSessionSummary> {
    let current_id = ctx.session().id;
    let mut sessions = load_saved_sessions(&ctx.sessions_dir);
    if !sessions.iter().any(|session| session.id == current_id) {
        sessions.push(ctx.cloned_session());
        sessions.sort_by_key(|session| Reverse(session.updated_at.timestamp()));
    }

    sessions
        .into_iter()
        .take(limit)
        .map(|session| {
            let preview = session
                .last_user_text()
                .map(truncate_summary)
                .unwrap_or_else(|| "No prompt".to_string());
            SavedSessionSummary {
                id: session.id.to_string(),
                cwd: session.cwd,
                updated_at: session.updated_at.timestamp(),
                mode: session.execution_mode.clone(),
                preview,
                is_current: session.id == current_id,
            }
        })
        .collect()
}

fn resolve_saved_session(ctx: &TuiContext, prefix: &str) -> Result<Session> {
    let prefix = prefix.trim();
    let matches: Vec<Session> = load_saved_sessions(&ctx.sessions_dir)
        .into_iter()
        .filter(|session| session.id.to_string().starts_with(prefix))
        .collect();

    match matches.len() {
        0 => Err(anyhow::anyhow!("No saved session matches `{prefix}`")),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => Err(anyhow::anyhow!(
            "Multiple sessions match `{prefix}`. Use a longer prefix."
        )),
    }
}

fn short_session_id(id: &str) -> &str {
    &id[..8.min(id.len())]
}

fn session_mode_label(mode: SessionMode) -> &'static str {
    match mode {
        SessionMode::Interactive => "interactive",
        SessionMode::Headless => "headless",
        SessionMode::Resume => "resume",
        SessionMode::Continue => "continue",
        SessionMode::DirectConnect => "direct-connect",
        SessionMode::Ssh => "ssh",
        SessionMode::Remote => "remote",
    }
}

fn session_compact_status(ctx: &TuiContext) -> String {
    let mut status = format!(
        "model {} · session {} · mode {}",
        ctx.model_name(),
        short_session_id(&ctx.session().id.to_string()),
        session_mode_label(ctx.session().execution_mode.clone()),
    );
    if let Some(label) = ctx.transport_label() {
        status.push_str(" · ");
        status.push_str(label);
    }
    status
}

fn help_panel(ctx: &TuiContext) -> InfoPanel {
    InfoPanel {
        title: "Shortcuts and Commands".to_string(),
        status: Some(session_compact_status(ctx)),
        lines: help_overlay_lines(ctx),
    }
}

fn sessions_panel(ctx: &TuiContext) -> InfoPanel {
    let sessions = list_saved_sessions(ctx, 8);
    let session_count = sessions.len();
    let mut lines =
        vec!["Use /fork <session-id-or-prefix> to branch from a saved session.".to_string()];
    append_panel_section(
        &mut lines,
        "Recent sessions",
        if sessions.is_empty() {
            vec!["No saved sessions found.".to_string()]
        } else {
            sessions
                .into_iter()
                .map(|session| format_saved_session_line(&session, &ctx.session().id))
                .collect()
        },
    );
    InfoPanel {
        title: "Recent Sessions".to_string(),
        status: Some(format!(
            "{session_count} saved session(s) · current {}",
            short_session_id(&ctx.session().id.to_string())
        )),
        lines,
    }
}

fn tasks_panel(ctx: &TuiContext) -> InfoPanel {
    let shell_count = list_background_tasks()
        .into_iter()
        .filter(|task| task.session_id == ctx.session().id.to_string())
        .count();
    let agent_count = list_subagent_tasks_for_parent(ctx.session().id).len();
    let mut lines = Vec::new();
    append_panel_section(&mut lines, "Background tasks", background_task_lines(ctx));
    InfoPanel {
        title: "Background Tasks".to_string(),
        status: Some(format!("shell {shell_count} · agent {agent_count}")),
        lines,
    }
}

fn tools_panel(ctx: &TuiContext) -> InfoPanel {
    let mut lines = vec![format!(
        "built-in={} · skills={} · mcp={}",
        built_in_tool_count(ctx),
        ctx.skills().len(),
        mcp_tool_count(ctx)
    )];
    append_panel_section(&mut lines, "Built-in tools", built_in_tool_lines(ctx));
    append_panel_section(&mut lines, "Skills", skill_surface_lines(ctx));
    append_panel_section(&mut lines, "MCP tools", mcp_surface_lines(ctx));
    InfoPanel {
        title: "Active Tools".to_string(),
        status: Some(format!(
            "built-in {} · skills {} · mcp {}",
            built_in_tool_count(ctx),
            ctx.skills().len(),
            mcp_tool_count(ctx)
        )),
        lines,
    }
}

fn help_overlay_lines(ctx: &TuiContext) -> Vec<String> {
    let entries = visible_command_entries(ctx);
    let (builtins, skills): (Vec<_>, Vec<_>) = entries
        .into_iter()
        .partition(|entry| matches!(entry.source, CommandSource::BuiltIn));

    let mut lines = vec![
        format!(
            "Session context: {} · cwd {} · commands {} · tools {} · skills {}",
            session_compact_status(ctx),
            display_path(&ctx.session().cwd),
            builtins.len() + skills.len(),
            ctx.tool_specs().len(),
            ctx.skills().len()
        ),
        format!(
            "Transport: {}",
            ctx.transport_label().unwrap_or("local session")
        ),
    ];

    append_panel_section(
        &mut lines,
        "Shortcuts",
        vec![
            "Enter        send prompt".to_string(),
            "Enter busy   queue the next prompt while the assistant works".to_string(),
            "?            show this shortcut overlay".to_string(),
            "/            start slash command input".to_string(),
            "↑ / ↓        navigate options or prompt history".to_string(),
            "1 / 2 / 3    approve, always allow, or deny a tool request".to_string(),
            "Esc          interrupt the turn or dismiss panels".to_string(),
            "Ctrl+R       expand or collapse tool output".to_string(),
            "Shift+Tab    cycle permission mode".to_string(),
            "Ctrl+O       open the scrollable transcript".to_string(),
            "q / Ctrl+C   exit the REPL".to_string(),
        ],
    );
    append_panel_section(
        &mut lines,
        "Prompt features",
        vec![
            "!            bash-style intent prefix".to_string(),
            "@            file/reference prefix".to_string(),
            "&            background-task intent prefix".to_string(),
        ],
    );
    append_panel_section(
        &mut lines,
        "Built-in commands",
        builtins
            .into_iter()
            .map(|entry| format!("{:<12} {}", entry.name, entry.description))
            .collect(),
    );
    append_panel_section(
        &mut lines,
        "Discovered skills",
        if skills.is_empty() {
            vec!["(none)".to_string()]
        } else {
            skills
                .into_iter()
                .map(|entry| format!("{:<12} {}", entry.name, entry.description))
                .collect()
        },
    );

    lines
}

fn compact_fork_error(prefix: &str, err: &anyhow::Error) -> String {
    let message = err.to_string();
    if message.contains("No saved session matches") {
        format!("no saved session matches `{prefix}`")
    } else if message.contains("Multiple sessions match") {
        format!("multiple sessions match `{prefix}`")
    } else {
        truncate_for_display(&message, 96)
    }
}

fn format_saved_session_line(
    session: &SavedSessionSummary,
    _current_session_id: &uuid::Uuid,
) -> String {
    let current_marker = if session.is_current { "*" } else { " " };
    let action = if session.is_current {
        "current"
    } else {
        "forkable"
    };
    format!(
        "{} {} {:<14} {:<8} {:<18} {}",
        current_marker,
        short_session_id(&session.id),
        session_mode_label(session.mode.clone()),
        action,
        relative_time_label(session.updated_at),
        truncate_for_display(
            &format!("{} · {}", display_path(&session.cwd), session.preview),
            72
        ),
    )
}

fn built_in_tool_count(ctx: &TuiContext) -> usize {
    ctx.tool_specs()
        .iter()
        .filter(|tool| !tool.name.starts_with("mcp__"))
        .count()
}

fn mcp_tool_count(ctx: &TuiContext) -> usize {
    ctx.tool_specs()
        .iter()
        .filter(|tool| tool.name.starts_with("mcp__"))
        .count()
}

#[allow(dead_code)]
fn tool_surface_lines(ctx: &TuiContext) -> Vec<String> {
    let mut lines = Vec::new();

    let mut builtins: Vec<_> = ctx
        .tool_specs()
        .iter()
        .filter(|tool| !tool.name.starts_with("mcp__"))
        .map(|tool| tool.name.clone())
        .collect();
    builtins.sort();
    if !builtins.is_empty() {
        lines.push(format!("built-in: {}", builtins.join(", ")));
    }

    let mut skills: Vec<_> = ctx
        .skills()
        .iter()
        .map(|skill| skill.slash_command.clone())
        .collect();
    skills.sort();
    if !skills.is_empty() {
        lines.push(format!("skills: {}", skills.join(", ")));
    }

    let mut mcp_tools: Vec<_> = ctx
        .tool_specs()
        .iter()
        .filter(|tool| tool.name.starts_with("mcp__"))
        .map(|tool| tool.name.clone())
        .collect();
    mcp_tools.sort();
    if !mcp_tools.is_empty() {
        lines.push(format!("mcp: {}", mcp_tools.join(", ")));
    }

    if lines.is_empty() {
        lines.push("No active tools discovered".to_string());
    }

    lines
}

fn built_in_tool_lines(ctx: &TuiContext) -> Vec<String> {
    let mut builtins: Vec<_> = ctx
        .tool_specs()
        .iter()
        .filter(|tool| !tool.name.starts_with("mcp__"))
        .map(|tool| tool.name.clone())
        .collect();
    builtins.sort();
    if builtins.is_empty() {
        vec!["No built-in tools discovered".to_string()]
    } else {
        builtins.into_iter().map(|name| format!("{name}")).collect()
    }
}

fn skill_surface_lines(ctx: &TuiContext) -> Vec<String> {
    let mut skills: Vec<_> = ctx
        .skills()
        .iter()
        .map(|skill| skill.slash_command.clone())
        .collect();
    skills.sort();
    if skills.is_empty() {
        vec!["(none)".to_string()]
    } else {
        skills
            .into_iter()
            .map(|slash_command| format!("{slash_command}"))
            .collect()
    }
}

fn mcp_surface_lines(ctx: &TuiContext) -> Vec<String> {
    let mut mcp_tools: Vec<_> = ctx
        .tool_specs()
        .iter()
        .filter(|tool| tool.name.starts_with("mcp__"))
        .map(|tool| tool.name.clone())
        .collect();
    mcp_tools.sort();
    if mcp_tools.is_empty() {
        vec!["(none)".to_string()]
    } else {
        mcp_tools.into_iter().collect()
    }
}

fn truncate_summary(text: &str) -> String {
    const MAX_CHARS: usize = 52;
    let trimmed = text.trim();
    let mut chars = trimmed.chars();
    let summary: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{summary}...")
    } else {
        summary
    }
}

fn truncate_for_display(text: &str, max_len: usize) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= max_len {
        trimmed.to_string()
    } else {
        let mut chars = trimmed.chars();
        let summary: String = chars.by_ref().take(max_len).collect();
        format!("{summary}...")
    }
}

fn relative_time_label(timestamp: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(timestamp);
    let delta = now.saturating_sub(timestamp);

    match delta {
        0..=59 => "just now".to_string(),
        60..=3_599 => format!("{}m ago", delta / 60),
        3_600..=86_399 => format!("{}h ago", delta / 3_600),
        _ => format!("{}d ago", delta / 86_400),
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(r);

    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(popup_layout[1])[1]
}

fn restore_terminal() -> Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    ratatui::restore();
    Ok(())
}

#[cfg(test)]
mod command_policy_tests {
    use super::*;
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::{Arc, Mutex, MutexGuard, OnceLock},
    };

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        match LOCK.get_or_init(|| Mutex::new(())).lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("clawed_tui_policy_{name}_{unique}"));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn make_context_at(cwd: PathBuf, sessions_dir: PathBuf) -> TuiContext {
        TuiContext::new(
            clawedcode_core::config::AppConfig::default(),
            clawedcode_core::prompt::PromptSpec {
                name: "test",
                summary: "test",
                body: "You are a test assistant.",
            },
            clawedcode_core::compat::CompatibilitySnapshot {
                settings_files: vec![],
                settings: serde_json::Value::Null,
                skills: vec![],
                memory_files: vec![],
                memory: String::new(),
                mcp_servers: std::collections::BTreeMap::new(),
            },
            cwd,
            sessions_dir,
        )
    }

    fn write_skill(root: &Path, file_name: &str, name: &str, body: &str) {
        let skills_dir = root.join(".claude").join("skills");
        fs::create_dir_all(&skills_dir).expect("create skills dir");
        fs::write(
            skills_dir.join(file_name),
            format!("---\nname: {name}\n---\n{body}"),
        )
        .expect("write skill");
    }

    #[test]
    fn test_is_reserved_command() {
        assert!(is_reserved_command("/help"));
        assert!(is_reserved_command("/clear"));
        assert!(is_reserved_command("/update"));
        assert!(is_reserved_command("/task"));
        assert!(is_reserved_command("/tasks"));
        assert!(is_reserved_command("/sessions"));
        assert!(is_reserved_command("/fork"));
        assert!(is_reserved_command("/tools"));
        assert!(!is_reserved_command("/code-review"));
        assert!(!is_reserved_command("/my-skill"));
    }

    #[test]
    fn test_is_valid_command_name_valid() {
        assert!(is_valid_command_name("/help"));
        assert!(is_valid_command_name("/clear"));
        assert!(is_valid_command_name("/task"));
        assert!(is_valid_command_name("/tasks"));
        assert!(is_valid_command_name("/sessions"));
        assert!(is_valid_command_name("/fork"));
        assert!(is_valid_command_name("/tools"));
        assert!(is_valid_command_name("/code-review"));
        assert!(is_valid_command_name("/my-skill-123"));
        assert!(is_valid_command_name("/a"));
        assert!(is_valid_command_name("/abc123"));
    }

    #[test]
    fn test_is_valid_command_name_invalid() {
        assert!(!is_valid_command_name("help"));
        assert!(!is_valid_command_name("/"));
        assert!(!is_valid_command_name("/Bad"));
        assert!(!is_valid_command_name("/bad_name"));
        assert!(!is_valid_command_name("/bad name"));
        assert!(!is_valid_command_name("/bad@name"));
        assert!(!is_valid_command_name("/bad.Name"));
        assert!(!is_valid_command_name("/bad name"));
    }

    #[test]
    fn test_update_is_supported() {
        assert!(update_is_supported(InstallMethod::Cargo));
        assert!(update_is_supported(InstallMethod::Npm));
        assert!(!update_is_supported(InstallMethod::LocalBuild));
        assert!(!update_is_supported(InstallMethod::Unknown));
    }

    #[test]
    fn test_skill_is_registerable_valid() {
        let skill = SkillDescriptor {
            slash_command: "/code-review".to_string(),
            name: "Code Review".to_string(),
            description: Some("Reviews code".to_string()),
            when_to_use: None,
            legacy_command: false,
            body: "Review the code changes".to_string(),
            path: PathBuf::from("/skills/code-review.md"),
        };
        assert!(skill_is_registerable(&skill));
    }

    #[test]
    fn test_skill_is_registerable_collides_with_builtin() {
        let skill = SkillDescriptor {
            slash_command: "/help".to_string(),
            name: "Help Skill".to_string(),
            description: Some("Provides help".to_string()),
            when_to_use: None,
            legacy_command: false,
            body: "Help content".to_string(),
            path: PathBuf::from("/skills/help.md"),
        };
        assert!(!skill_is_registerable(&skill));
    }

    #[test]
    fn test_skill_is_registerable_malformed_name() {
        let skill = SkillDescriptor {
            slash_command: "/Bad Name".to_string(),
            name: "Bad Name Skill".to_string(),
            description: Some("Bad name".to_string()),
            when_to_use: None,
            legacy_command: false,
            body: "Content".to_string(),
            path: PathBuf::from("/skills/bad-name.md"),
        };
        assert!(!skill_is_registerable(&skill));
    }

    #[test]
    fn test_skill_is_registerable_empty_body() {
        let skill = SkillDescriptor {
            slash_command: "/valid".to_string(),
            name: "Valid Skill".to_string(),
            description: Some("Valid".to_string()),
            when_to_use: None,
            legacy_command: false,
            body: "   ".to_string(),
            path: PathBuf::from("/skills/valid.md"),
        };
        assert!(!skill_is_registerable(&skill));
    }

    #[test]
    fn test_command_is_visible_update_cargo() {
        let entry = CommandEntry {
            name: "/update".to_string(),
            description: "Update clawedcode".to_string(),
            source: CommandSource::BuiltIn,
        };
        assert!(command_is_visible(&entry, InstallMethod::Cargo));
    }

    #[test]
    fn test_command_is_visible_update_npm() {
        let entry = CommandEntry {
            name: "/update".to_string(),
            description: "Update clawedcode".to_string(),
            source: CommandSource::BuiltIn,
        };
        assert!(command_is_visible(&entry, InstallMethod::Npm));
    }

    #[test]
    fn test_command_is_visible_update_local_build() {
        let entry = CommandEntry {
            name: "/update".to_string(),
            description: "Update clawedcode".to_string(),
            source: CommandSource::BuiltIn,
        };
        assert!(!command_is_visible(&entry, InstallMethod::LocalBuild));
    }

    #[test]
    fn test_command_is_visible_update_unknown() {
        let entry = CommandEntry {
            name: "/update".to_string(),
            description: "Update clawedcode".to_string(),
            source: CommandSource::BuiltIn,
        };
        assert!(!command_is_visible(&entry, InstallMethod::Unknown));
    }

    #[test]
    fn test_command_is_visible_help_always_visible() {
        let entry = CommandEntry {
            name: "/help".to_string(),
            description: "Show help".to_string(),
            source: CommandSource::BuiltIn,
        };
        assert!(command_is_visible(&entry, InstallMethod::Cargo));
        assert!(command_is_visible(&entry, InstallMethod::LocalBuild));
        assert!(command_is_visible(&entry, InstallMethod::Unknown));
    }

    #[test]
    fn bare_slash_uses_curated_builtin_first_entries() {
        let _guard = env_lock();
        unsafe { std::env::set_var("CLAWEDCODE_INSTALL_METHOD", "cargo") };

        let root = temp_dir("bare_slash_curated");
        let project = root.join("project");
        let sessions_dir = root.join("sessions");
        fs::create_dir_all(&project).expect("create project dir");
        fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        write_skill(&project, "AlphaSkill.md", "Alpha Skill", "Use alpha.");

        let ctx = make_context_at(project, sessions_dir);
        let entries = filtered_command_entries(&ctx, "/");
        let names: Vec<_> = entries
            .iter()
            .take(6)
            .map(|entry| entry.name.as_str())
            .collect();

        assert_eq!(
            names,
            vec!["/help", "/task", "/tasks", "/sessions", "/tools", "/fork"]
        );
    }

    #[test]
    fn slash_matches_rank_exact_then_builtin_then_shorter_names() {
        let entries = vec![
            CommandEntry {
                name: "/help".to_string(),
                description: "Show help".to_string(),
                source: CommandSource::BuiltIn,
            },
            CommandEntry {
                name: "/helper".to_string(),
                description: "Helper".to_string(),
                source: CommandSource::Skill,
            },
            CommandEntry {
                name: "/hello".to_string(),
                description: "Hello".to_string(),
                source: CommandSource::BuiltIn,
            },
            CommandEntry {
                name: "/helm".to_string(),
                description: "Helm".to_string(),
                source: CommandSource::Skill,
            },
        ];

        let mut filtered = entries
            .into_iter()
            .filter(|entry| entry.name.starts_with("/hel"))
            .collect::<Vec<_>>();
        filtered.sort_by(|a, b| compare_command_entries("/help", a, b));

        let names: Vec<_> = filtered.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["/help", "/hello", "/helm", "/helper"]);
    }

    #[test]
    fn manual_update_does_not_execute_when_hidden() {
        let _guard = env_lock();
        unsafe { std::env::set_var("CLAWEDCODE_INSTALL_METHOD", "local") };

        let root = temp_dir("manual_update_hidden");
        let project = root.join("project");
        let sessions_dir = root.join("sessions");
        fs::create_dir_all(&project).expect("create project dir");
        fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        let mut ctx = make_context_at(project, sessions_dir);
        let mut handler = ReplHandler::new(false);
        let mut active_turn = None;

        handle_slash_command(
            &mut ctx,
            &mut handler,
            "/update",
            &mut active_turn,
            &Arc::new(Mutex::new(std::collections::HashSet::new())),
        )
        .unwrap();

        assert!(active_turn.is_none());
        assert!(
            handler
                .overlay_rows
                .iter()
                .any(|line| row_text(line).contains("Unknown command `/update`"))
        );
        assert!(
            handler
                .overlay_rows
                .iter()
                .all(|line| !row_text(line).contains("Updated clawedcode"))
        );

        unsafe { std::env::remove_var("CLAWEDCODE_INSTALL_METHOD") };
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn manual_invalid_skill_does_not_execute() {
        let _guard = env_lock();
        let root = temp_dir("manual_invalid_skill");
        let project = root.join("project");
        let sessions_dir = root.join("sessions");
        fs::create_dir_all(&project).expect("create project dir");
        fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        write_skill(&project, "HiddenSkill.md", "Hidden Skill", "   ");

        let mut ctx = make_context_at(project, sessions_dir);
        let mut handler = ReplHandler::new(false);
        let mut active_turn = None;

        handle_slash_command(
            &mut ctx,
            &mut handler,
            "/hidden-skill",
            &mut active_turn,
            &Arc::new(Mutex::new(std::collections::HashSet::new())),
        )
        .unwrap();

        assert!(active_turn.is_none());
        assert!(
            handler
                .overlay_rows
                .iter()
                .any(|line| row_text(line).contains("Unknown command `/hidden-skill`"))
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn help_overlay_is_sectioned_and_contextual() {
        let _guard = env_lock();
        unsafe { std::env::set_var("CLAWEDCODE_INSTALL_METHOD", "local") };

        let root = temp_dir("help_visible_commands");
        let project = root.join("project");
        let sessions_dir = root.join("sessions");
        fs::create_dir_all(&project).expect("create project dir");
        fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        write_skill(
            &project,
            "VisibleSkill.md",
            "Visible Skill",
            "Use this skill.",
        );
        write_skill(&project, "HiddenSkill.md", "Hidden Skill", "   ");

        let mut ctx = make_context_at(project, sessions_dir);
        let mut handler = ReplHandler::new(false);
        let mut active_turn = None;

        handle_slash_command(
            &mut ctx,
            &mut handler,
            "/help",
            &mut active_turn,
            &Arc::new(Mutex::new(std::collections::HashSet::new())),
        )
        .unwrap();

        let rendered = handler
            .info_panel
            .as_ref()
            .expect("help panel should be visible")
            .lines
            .join("\n");
        assert!(rendered.contains("Session context:"));
        assert!(rendered.contains("Shortcuts:"));
        assert!(rendered.contains("?            show this shortcut overlay"));
        assert!(rendered.contains("Prompt features:"));
        assert!(rendered.contains("Built-in commands:"));
        assert!(rendered.contains("Discovered skills:"));
        assert!(rendered.contains("model "));
        assert!(rendered.contains("cwd"));
        assert!(rendered.contains("/help"));
        assert!(rendered.contains("/clear"));
        assert!(rendered.contains("/task"));
        assert!(rendered.contains("/visible-skill"));
        assert!(!rendered.contains("/update"));
        assert!(!rendered.contains("/hidden-skill"));

        unsafe { std::env::remove_var("CLAWEDCODE_INSTALL_METHOD") };
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn info_panel_hint_mentions_close_and_scroll_controls() {
        let hint = info_panel_hint_line();
        assert!(hint.contains("Esc/q/?"));
        assert!(hint.contains("scroll"));
    }

    #[test]
    fn tool_display_names_match_claude_code() {
        assert_eq!(tool_display_name("shell"), "Bash");
        assert_eq!(tool_display_name("read_file"), "Read");
        assert_eq!(tool_display_name("apply_patch"), "Edit");
        assert_eq!(tool_display_name("TaskCreate"), "TaskCreate");
        assert_eq!(
            tool_display_name("mcp__github__search"),
            "mcp__github__search"
        );
    }

    #[test]
    fn tool_display_arg_summarizes_known_tools() {
        assert_eq!(
            tool_display_arg("shell", r#"{"command":"ls -la"}"#),
            "ls -la"
        );
        assert_eq!(
            tool_display_arg("read_file", r#"{"path":"src/main.rs"}"#),
            "src/main.rs"
        );
        assert_eq!(
            tool_display_arg(
                "apply_patch",
                r#"{"patch":"*** Begin Patch\n*** Update File: src/app.rs\n@@ x\n*** End Patch"}"#
            ),
            "src/app.rs"
        );
        assert_eq!(
            tool_display_arg("TaskCreate", r#"{"subject":"Fix bug"}"#),
            "Fix bug"
        );
    }

    #[test]
    fn permission_mode_label_covers_modes() {
        assert_eq!(
            permission_mode_label(clawedcode_core::permissions::PermissionMode::Default),
            "default"
        );
        assert!(
            permission_mode_label(clawedcode_core::permissions::PermissionMode::AcceptEdits)
                .contains("accept edits")
        );
        assert!(
            permission_mode_label(clawedcode_core::permissions::PermissionMode::Plan)
                .contains("plan mode")
        );
    }

    #[test]
    fn queued_prompts_render_as_dim_prompt_echoes() {
        let queued_lines = queued_prompt_preview_lines(
            &[
                "first queued prompt".to_string(),
                "second queued prompt".to_string(),
            ],
            80,
        );
        let rendered_queue = queued_lines
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered_queue.contains("first queued prompt"));
        assert!(rendered_queue.contains("second queued prompt"));
    }

    #[test]
    fn bash_mode_entries_rebuild_as_bash_rows() {
        let mut session = Session::new(std::path::PathBuf::from("/tmp/t"));
        session.push(
            Role::User,
            "<bash-input>ls -la</bash-input>\n<bash-output>total 42</bash-output>".to_string(),
        );
        let mut handler = ReplHandler::new(false);
        handler.rebuild_from_session(&session, false);
        let lines = handler.visible_lines().join("\n");
        assert!(lines.contains("! ls -la"));
        assert!(lines.contains("Bash(ls -la)"));
        assert!(lines.contains("total 42"));
        assert!(!lines.contains("bash-input"));
    }

    #[test]
    fn wrap_input_prefixes_and_wraps() {
        let lines = wrap_input("hello", 10);
        assert_eq!(lines, vec!["> hello".to_string()]);

        let wrapped = wrap_input("abcdefghijklmnop", 8);
        assert_eq!(wrapped.len(), 3);

        let multiline = wrap_input("a\nb", 10);
        assert_eq!(multiline, vec!["> a".to_string(), "b".to_string()]);
    }

    #[test]
    fn approval_selector_height_only_when_pending() {
        let request = ApprovalRequest {
            tool_use_id: "tool-1".to_string(),
            tool_name: "shell".to_string(),
            input: serde_json::json!({"command": "ls -la"}),
        };
        assert_eq!(approval_selector_height(None, 0), 0);
        assert!(approval_selector_height(Some(&request), 0) >= 5);
    }

    #[test]
    fn info_panels_are_sectioned_and_contextual() {
        let root = temp_dir("info_panels");
        let project = root.join("project");
        let sessions_dir = root.join("sessions");
        fs::create_dir_all(&project).expect("create project dir");
        fs::create_dir_all(&sessions_dir).expect("create sessions dir");

        let ctx = make_context_at(project.clone(), sessions_dir.clone());
        let current = ctx.session().clone();
        current.save(&sessions_dir).expect("save current session");
        let older = Session::with_mode(project, SessionMode::Headless);
        older.save(&sessions_dir).expect("save older session");

        let help = help_panel(&ctx);
        let sessions = sessions_panel(&ctx);
        let tools = tools_panel(&ctx);
        let tasks = tasks_panel(&ctx);

        assert_eq!(help.title, "Shortcuts and Commands");
        assert!(
            help.lines
                .iter()
                .any(|line| line.contains("Built-in commands:"))
        );
        assert_eq!(sessions.title, "Recent Sessions");
        assert!(sessions.lines.iter().any(|line| line.contains("Use /fork")));
        assert_eq!(tools.title, "Active Tools");
        assert!(tools.lines[0].contains("built-in="));
        assert_eq!(tasks.title, "Background Tasks");
        assert!(
            tasks
                .lines
                .iter()
                .any(|line| line.contains("No background tasks"))
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn info_panel_footer_surfaces_close_and_scroll_controls() {
        let root = temp_dir("info_panel_footer");
        let project = root.join("project");
        let sessions_dir = root.join("sessions");
        fs::create_dir_all(&project).expect("create project dir");
        fs::create_dir_all(&sessions_dir).expect("create sessions dir");

        let ctx = make_context_at(project, sessions_dir);
        let panel = help_panel(&ctx);
        let body = panel
            .lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(panel.title, "Shortcuts and Commands");
        assert!(body.contains("Esc"));
        assert!(body.contains("Shift+Tab"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn scrolled_transcript_footer_surfaces_navigation_and_unseen_state() {
        let root = temp_dir("scrolled_transcript_footer");
        let project = root.join("project");
        let sessions_dir = root.join("sessions");
        fs::create_dir_all(&project).expect("create project dir");
        fs::create_dir_all(&sessions_dir).expect("create sessions dir");

        let _ctx = make_context_at(project, sessions_dir);
        let state = TranscriptModeState {
            viewport: TranscriptViewport {
                scroll_offset: 4,
                pinned_to_bottom: false,
                unseen_count: 3,
            },
            ..TranscriptModeState::default()
        };
        let status = transcript_footer_status_line(&state);
        let hint = transcript_footer_hint_line(&state);

        assert!(status.contains("Showing detailed transcript"));
        assert!(hint.contains("/ search"));
        assert!(hint.contains("scroll"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn transcript_search_matches_are_case_insensitive() {
        let rows = vec![
            RenderRow::User("alpha".to_string()),
            RenderRow::Assistant("Beta target".to_string()),
            RenderRow::Info("gamma TARGET".to_string()),
        ];

        assert_eq!(transcript_search_matches(&rows, "target"), vec![1, 2]);
    }

    #[test]
    fn transcript_search_badge_reflects_current_match() {
        let state = TranscriptModeState {
            search_query: "target".to_string(),
            search_matches: vec![4, 8, 12],
            search_index: 1,
            ..TranscriptModeState::default()
        };

        assert_eq!(transcript_search_badge(&state), Some((2, 3)));
    }

    #[test]
    fn render_row_text_formats_rows_consistently() {
        assert_eq!(row_text(&RenderRow::User("hello".into())), "> hello");
        assert_eq!(
            row_text(&RenderRow::ToolResult {
                tool_use_id: "abc".into(),
                content: "ok".into(),
                is_error: false,
            }),
            "  ⎿  ok"
        );
        assert_eq!(row_text(&RenderRow::Spacer), "");
    }

    #[test]
    fn export_transcript_lines_writes_temp_file_without_editor() {
        static ENV_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        let _guard = ENV_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("lock env guard");
        unsafe { std::env::remove_var("VISUAL") };
        unsafe { std::env::remove_var("EDITOR") };

        let status = export_transcript_lines(&[
            RenderRow::User("one".to_string()),
            RenderRow::Assistant("two".to_string()),
        ])
        .expect("export succeeds");
        assert!(status.starts_with("wrote "));

        let path = status.trim_start_matches("wrote ");
        let exported = fs::read_to_string(path).expect("read exported transcript");
        assert_eq!(exported, "> one\n⏺ two");
        fs::remove_file(path).ok();
    }

    #[test]
    fn transport_label_appears_in_banner_help_and_dashboard() {
        let root = temp_dir("transport_label_surfaces");
        let project = root.join("project");
        let sessions_dir = root.join("sessions");
        fs::create_dir_all(&project).expect("create project dir");
        fs::create_dir_all(&sessions_dir).expect("create sessions dir");

        let mut ctx = make_context_at(project, sessions_dir);
        ctx.set_transport_label(Some("ssh devbox".to_string()));

        let banner = welcome_lines(&ctx)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let help = help_overlay_lines(&ctx).join("\n");
        let dashboard = welcome_right_lines(&ctx)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let compact = session_compact_status(&ctx);

        assert!(banner.contains("ssh devbox"));
        assert!(help.contains("Transport: ssh devbox"));
        assert!(dashboard.contains("Connection"));
        assert!(dashboard.contains("Interactive transport: ssh devbox"));
        assert!(compact.contains("ssh devbox"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn transport_session_marker_thinking_is_hidden_even_when_thinking_is_enabled() {
        let mut turn = TranscriptTurn::default();
        let message = Message::from_blocks(
            Role::Assistant,
            vec![
                ContentBlock::text("Remote hello"),
                ContentBlock::thinking(transport_session_id_marker(
                    "11111111-1111-1111-1111-111111111111",
                )),
            ],
        );

        append_message_to_turn(&mut turn, &message, true);
        let rendered = turn
            .rows
            .iter()
            .map(row_text)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("⏺ Remote hello"));
        assert!(!rendered.contains("clawedcode-transport-session-id"));
    }

    #[test]
    fn sessions_overlay_marks_current_session() {
        let root = temp_dir("sessions_overlay");
        let project = root.join("project");
        let sessions_dir = root.join("sessions");
        fs::create_dir_all(&project).expect("create project dir");
        fs::create_dir_all(&sessions_dir).expect("create sessions dir");

        let mut ctx = make_context_at(project.clone(), sessions_dir.clone());
        ctx.session_mut().push(Role::User, "current prompt");
        ctx.save_session().expect("save current session");

        let mut older = Session::new(project);
        older.push(Role::User, "older prompt");
        older.save(&sessions_dir).expect("save older session");

        let mut handler = ReplHandler::new(false);
        let mut active_turn = None;
        handle_slash_command(
            &mut ctx,
            &mut handler,
            "/sessions",
            &mut active_turn,
            &Arc::new(Mutex::new(std::collections::HashSet::new())),
        )
        .unwrap();

        let panel = handler
            .info_panel
            .as_ref()
            .expect("sessions panel should be visible");
        let rendered = panel.lines.join("\n");
        assert_eq!(panel.title, "Recent Sessions");
        assert!(rendered.contains("Use /fork"));
        assert!(rendered.contains("* "));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn dashboard_shows_onboarding_for_empty_workspace() {
        let _guard = env_lock();
        let root = temp_dir("dashboard_onboarding_empty");
        let project = root.join("project");
        let sessions_dir = root.join("sessions");
        let data_dir = root.join("data");
        fs::create_dir_all(&project).expect("create project dir");
        fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        fs::create_dir_all(&data_dir).expect("create data dir");
        unsafe { std::env::set_var("CLAWEDCODE_DATA_DIR", &data_dir) };

        let ctx = make_context_at(project, sessions_dir);
        let rendered = welcome_right_lines(&ctx)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Get started"));
        assert!(rendered.contains("Ask ClawedCode to create a new app or clone a repository"));
        assert!(!rendered.contains("Tips for getting started"));

        unsafe { std::env::remove_var("CLAWEDCODE_DATA_DIR") };
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn dashboard_shows_default_tips_after_claudemd_exists() {
        let _guard = env_lock();
        let root = temp_dir("dashboard_onboarding_complete");
        let project = root.join("project");
        let sessions_dir = root.join("sessions");
        let data_dir = root.join("data");
        fs::create_dir_all(&project).expect("create project dir");
        fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        fs::create_dir_all(&data_dir).expect("create data dir");
        fs::write(project.join("CLAUDE.md"), "rules").expect("write CLAUDE.md");
        unsafe { std::env::set_var("CLAWEDCODE_DATA_DIR", &data_dir) };
        clawedcode_core::onboarding::maybe_mark_project_onboarding_complete(&project);

        let ctx = make_context_at(project, sessions_dir);
        let rendered = welcome_right_lines(&ctx)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Tips for getting started"));
        assert!(!rendered.contains("Getting started"));

        unsafe { std::env::remove_var("CLAWEDCODE_DATA_DIR") };
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn format_subagent_task_line_uses_summary_when_available() {
        let task = SubAgentTaskState {
            child_session_id: uuid::Uuid::new_v4(),
            parent_session_id: uuid::Uuid::new_v4(),
            prompt: "Inspect two modules".to_string(),
            status: SubAgentTaskStatus::Completed,
            summary: Some("Compared the modules and found the shared bug".to_string()),
            tools_executed: 3,
            error: None,
            created_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            surfaced: false,
        };

        let line = format_subagent_task_line(&task);
        assert!(line.starts_with("[agent:completed] "));
        assert!(line.contains("Compared the modules"));
    }

    #[test]
    fn format_subagent_task_line_falls_back_to_prompt() {
        let task = SubAgentTaskState {
            child_session_id: uuid::Uuid::new_v4(),
            parent_session_id: uuid::Uuid::new_v4(),
            prompt: "Inspect two modules in parallel".to_string(),
            status: SubAgentTaskStatus::Running,
            summary: None,
            tools_executed: 0,
            error: None,
            created_at: chrono::Utc::now(),
            ended_at: None,
            surfaced: false,
        };

        let line = format_subagent_task_line(&task);
        assert!(line.starts_with("[agent:running] "));
        assert!(line.contains("Inspect two modules"));
    }

    #[test]
    fn format_saved_session_line_includes_id_mode_and_path() {
        let summary = SavedSessionSummary {
            id: "12345678-1234-1234-1234-123456789abc".to_string(),
            cwd: PathBuf::from("/tmp/project"),
            updated_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
            mode: SessionMode::Resume,
            preview: "Continue the task".to_string(),
            is_current: true,
        };

        let line = format_saved_session_line(&summary, &uuid::Uuid::new_v4());
        assert!(line.starts_with("* 12345678"));
        assert!(line.contains("resume"));
        assert!(line.contains("current"));
        assert!(line.contains("/tmp/project"));
        assert!(line.contains("Continue the task"));
    }

    #[test]
    fn tool_surface_lines_include_default_builtins() {
        let ctx = TuiContext::new(
            clawedcode_core::config::AppConfig::default(),
            clawedcode_core::prompt::PromptSpec {
                name: "test",
                summary: "test",
                body: "You are a test assistant.",
            },
            clawedcode_core::compat::CompatibilitySnapshot {
                settings_files: vec![],
                settings: serde_json::Value::Null,
                skills: vec![],
                memory_files: vec![],
                memory: String::new(),
                mcp_servers: std::collections::BTreeMap::new(),
            },
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp/sessions"),
        );

        let lines = tool_surface_lines(&ctx);
        assert!(built_in_tool_count(&ctx) > 0);
        assert!(lines.iter().any(|line| line.starts_with("built-in: ")));
        assert!(lines.iter().any(|line| line.contains("read_file")));
    }

    mod transcript_snapshot_tests {
        use super::*;
        use clawedcode_core::{
            compat::CompatibilitySnapshot, config::AppConfig, prompt::PromptSpec,
        };
        use std::{
            fs,
            path::PathBuf,
            time::{Duration, Instant, SystemTime, UNIX_EPOCH},
        };

        fn temp_dir(name: &str) -> PathBuf {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("clawed_tui_{name}_{unique}"));
            fs::create_dir_all(&dir).expect("create temp dir");
            dir
        }

        fn render_session(session: &Session, show_thinking: bool) -> String {
            let mut handler = ReplHandler::new(show_thinking);
            handler.rebuild_from_session(session, show_thinking);
            handler.visible_lines().join("\n")
        }

        fn make_context(sessions_dir: PathBuf) -> TuiContext {
            TuiContext::new(
                AppConfig::default(),
                PromptSpec {
                    name: "test",
                    summary: "test",
                    body: "You are a test assistant.",
                },
                CompatibilitySnapshot {
                    settings_files: vec![],
                    settings: serde_json::Value::Null,
                    skills: vec![],
                    memory_files: vec![],
                    memory: String::new(),
                    mcp_servers: std::collections::BTreeMap::new(),
                },
                PathBuf::from("/tmp"),
                sessions_dir,
            )
        }

        #[test]
        fn transcript_snapshot_user_and_assistant_text_only() {
            let mut session = Session::new(PathBuf::from("/tmp/test"));
            session.push(Role::User, "What is 2+2?");
            session.push(Role::Assistant, "The answer is 4.");

            assert_eq!(
                render_session(&session, false),
                "> What is 2+2?\n\n⏺ The answer is 4."
            );
        }

        #[test]
        fn transcript_snapshot_hides_system_and_thinking_by_default() {
            let mut session = Session::new(PathBuf::from("/tmp/test"));
            session.push(Role::System, "You should not see this.");
            session.push(Role::User, "Hello");
            session.push_blocks(
                Role::Assistant,
                vec![
                    ContentBlock::thinking("Let me think about this."),
                    ContentBlock::text("Hi there."),
                ],
            );

            assert_eq!(render_session(&session, false), "> Hello\n\n⏺ Hi there.");
        }

        #[test]
        fn transcript_snapshot_shows_thinking_when_enabled() {
            let mut session = Session::new(PathBuf::from("/tmp/test"));
            session.push(Role::User, "Hello");
            session.push_blocks(
                Role::Assistant,
                vec![
                    ContentBlock::thinking("Let me think about this."),
                    ContentBlock::text("Hi there."),
                ],
            );

            assert_eq!(
                render_session(&session, true),
                "> Hello\n\n⏺ Let me think about this.\n⏺ Hi there."
            );
        }

        #[test]
        fn transcript_snapshot_includes_tool_lines_but_not_system_messages() {
            let mut session = Session::new(PathBuf::from("/tmp/test"));
            session.push(Role::System, "You should not see this.");
            session.push(Role::User, "Read the file.");
            session.push_blocks(
                Role::Assistant,
                vec![ContentBlock::tool_use(
                    "tool-1",
                    "read_file",
                    serde_json::json!({"path": "test.txt"}),
                )],
            );
            session.push_blocks(
                Role::Tool,
                vec![ContentBlock::tool_result("tool-1", "file contents here")],
            );

            assert_eq!(
                render_session(&session, false),
                "> Read the file.\n\n⏺ Read(test.txt)\n  ⎿  file contents here"
            );
        }

        #[test]
        fn transcript_snapshot_renders_subagent_summary() {
            let mut session = Session::new(PathBuf::from("/tmp/test"));
            session.push_blocks(
                Role::Assistant,
                vec![ContentBlock::subagent_summary(
                    "child-session-123".to_string(),
                    "Completed the task successfully.".to_string(),
                )],
            );

            assert_eq!(
                render_session(&session, false),
                "⏺ Task(child-se)\n  ⎿  Completed the task successfully."
            );
        }

        #[test]
        fn transcript_rebuild_stays_fast_for_small_session() {
            let mut session = Session::new(PathBuf::from("/tmp/test"));
            for idx in 0..200 {
                session.push(Role::User, format!("prompt {idx}"));
                session.push(Role::Assistant, format!("reply {idx}"));
            }

            let start = Instant::now();
            let rendered = render_session(&session, false);
            let elapsed = start.elapsed();

            assert!(rendered.contains("prompt 199"));
            assert!(elapsed < Duration::from_secs(2), "elapsed: {elapsed:?}");
        }

        #[test]
        fn live_stream_deltas_render_before_turn_commit() {
            let mut handler = ReplHandler::new(true);
            handler.begin_live_turn("hello");
            handler.on_event(&TuiEvent::ThinkingDelta {
                text: "let me think".to_string(),
            });
            handler.on_event(&TuiEvent::MessageDelta {
                text: "Hi".to_string(),
            });

            assert_eq!(
                handler.visible_lines().join("\n"),
                "> hello\n\n⏺ let me think\n⏺ Hi"
            );
        }

        #[test]
        fn live_tool_pending_approval_appears_in_transcript() {
            let mut handler = ReplHandler::new(false);
            handler.begin_live_turn("please inspect");
            handler.on_event(&TuiEvent::ToolUse {
                id: "tool-1".to_string(),
                name: "shell".to_string(),
                input: serde_json::json!({"command": "ls -la"}),
            });
            handler.record_live_tool_pending_approval("tool-1");

            assert_eq!(
                handler.visible_lines().join("\n"),
                "> please inspect\n\n⏺ Bash(ls -la)\n  ⎿  Awaiting approval…"
            );
        }

        #[test]
        fn live_tool_events_keep_the_assistant_turn_anchored() {
            let mut handler = ReplHandler::new(false);
            handler.begin_live_turn("please inspect");
            handler.on_event(&TuiEvent::ToolUse {
                id: "tool-1".to_string(),
                name: "shell".to_string(),
                input: serde_json::json!({"command": "ls -la"}),
            });

            assert_eq!(
                handler.visible_lines().join("\n"),
                "> please inspect\n\n⏺ Bash(ls -la)"
            );
        }

        #[test]
        fn live_tool_approval_and_result_update_promptly() {
            let mut handler = ReplHandler::new(false);
            handler.begin_live_turn("please inspect");
            handler.on_event(&TuiEvent::ToolUse {
                id: "tool-1".to_string(),
                name: "shell".to_string(),
                input: serde_json::json!({"command": "ls -la"}),
            });
            handler.record_live_tool_pending_approval("tool-1");
            handler.record_live_tool_approval_decision("tool-1", true);
            handler.record_live_tool_result("tool-1", "done".to_string(), false);

            assert_eq!(
                handler.visible_lines().join("\n"),
                "> please inspect\n\n⏺ Bash(ls -la)\n  ⎿  Awaiting approval…\n  ⎿  Approved\n  ⎿  done"
            );

            let mut denied_handler = ReplHandler::new(false);
            denied_handler.begin_live_turn("please inspect");
            denied_handler.on_event(&TuiEvent::ToolUse {
                id: "tool-2".to_string(),
                name: "shell".to_string(),
                input: serde_json::json!({"command": "ls -la"}),
            });
            denied_handler.record_live_tool_pending_approval("tool-2");
            denied_handler.record_live_tool_approval_decision("tool-2", false);

            assert_eq!(
                denied_handler.visible_lines().join("\n"),
                "> please inspect\n\n⏺ Bash(ls -la)\n  ⎿  Awaiting approval…\n  ⎿  Denied by user"
            );
        }

        #[test]
        fn resolve_saved_session_accepts_unique_prefix() {
            let root = temp_dir("resolve_saved_session_unique");
            let sessions_dir = root.join("sessions");
            fs::create_dir_all(&sessions_dir).unwrap();

            let mut first = Session::new(PathBuf::from("/tmp/one"));
            first.push(Role::User, "first");
            first.save(&sessions_dir).unwrap();

            let mut second = Session::new(PathBuf::from("/tmp/two"));
            second.push(Role::User, "second");
            second.save(&sessions_dir).unwrap();

            let ctx = make_context(sessions_dir.clone());
            let prefix = &first.id.to_string()[..8];
            let resolved = resolve_saved_session(&ctx, prefix).unwrap();
            assert_eq!(resolved.id, first.id);

            fs::remove_dir_all(root).ok();
        }

        #[test]
        fn resolve_saved_session_rejects_ambiguous_prefix() {
            let root = temp_dir("resolve_saved_session_ambiguous");
            let sessions_dir = root.join("sessions");
            fs::create_dir_all(&sessions_dir).unwrap();

            let mut first = Session::new(PathBuf::from("/tmp/one"));
            first.id = uuid::Uuid::parse_str("aaaaaaaa-0000-0000-0000-000000000001").unwrap();
            first.push(Role::User, "first");
            first.save(&sessions_dir).unwrap();

            let mut second = Session::new(PathBuf::from("/tmp/two"));
            second.id = uuid::Uuid::parse_str("aaaaaaaa-0000-0000-0000-000000000002").unwrap();
            second.push(Role::User, "second");
            second.save(&sessions_dir).unwrap();

            let ctx = make_context(sessions_dir.clone());
            let error = resolve_saved_session(&ctx, "aaaaaaaa").unwrap_err();
            assert!(error.to_string().contains("Multiple sessions match"));

            fs::remove_dir_all(root).ok();
        }

        #[test]
        fn fork_command_replaces_active_session_with_new_interactive_copy() {
            let root = temp_dir("fork_command");
            let sessions_dir = root.join("sessions");
            fs::create_dir_all(&sessions_dir).unwrap();

            let mut source = Session::with_mode(PathBuf::from("/tmp/source"), SessionMode::Resume);
            source.push(Role::User, "original");
            source.push(Role::Assistant, "reply");
            source.save(&sessions_dir).unwrap();

            let mut ctx = make_context(sessions_dir.clone());
            let source_id = source.id;
            let old_active = ctx.session().id;
            let mut handler = ReplHandler::new(false);
            let mut active_turn = None;

            let command = format!("/fork {}", &source_id.to_string()[..8]);
            let handled = handle_slash_command(
                &mut ctx,
                &mut handler,
                &command,
                &mut active_turn,
                &Arc::new(Mutex::new(std::collections::HashSet::new())),
            )
            .unwrap();

            assert!(handled);
            assert_ne!(ctx.session().id, old_active);
            assert_ne!(ctx.session().id, source_id);
            assert_eq!(ctx.session().execution_mode, SessionMode::Interactive);
            assert_eq!(ctx.session().messages, source.messages);
            assert!(handler.overlay_rows.iter().any(|line| {
                let text = row_text(line);
                text.contains("Forked ") && text.contains(" -> ")
            }));

            fs::remove_dir_all(root).ok();
        }

        #[test]
        fn recent_activity_stays_fast_for_small_session_store() {
            let root = temp_dir("recent_activity");
            let sessions_dir = root.join("sessions");
            fs::create_dir_all(&sessions_dir).unwrap();

            for idx in 0..20 {
                let mut session = Session::new(PathBuf::from(format!("/tmp/project-{idx}")));
                session.push(Role::User, format!("recent prompt {idx}"));
                session.save(&sessions_dir).unwrap();
                std::thread::sleep(Duration::from_millis(2));
            }

            let ctx = make_context(sessions_dir.clone());
            let start = Instant::now();
            let entries = recent_activity(&ctx, 5);
            let elapsed = start.elapsed();

            assert!(!entries.is_empty());
            assert!(elapsed < Duration::from_secs(2), "elapsed: {elapsed:?}");

            fs::remove_dir_all(root).ok();
        }
    }
}
