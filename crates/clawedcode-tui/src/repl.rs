use anyhow::Result;
use clawedcode_core::compat::SkillDescriptor;
use clawedcode_core::content::ContentBlock;
use clawedcode_core::interactive::{ApprovalRequest, TuiContext, TuiEvent, TuiHandler};
use clawedcode_core::session::{Message, Role, Session};
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
    fs,
    io::{self, stdout},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const ACCENT: Color = Color::Rgb(224, 122, 95);
const MUTED: Color = Color::Rgb(150, 150, 150);
const DASHBOARD_HEIGHT: u16 = 13;

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

struct ReplHandler {
    transcript_lines: Vec<String>,
    overlay_lines: Vec<String>,
    state: AppState,
}

impl ReplHandler {
    fn new() -> Self {
        Self {
            transcript_lines: Vec::new(),
            overlay_lines: Vec::new(),
            state: AppState::Idle,
        }
    }

    fn rebuild_from_session(&mut self, session: &Session, show_thinking: bool) {
        self.transcript_lines.clear();
        for msg in &session.messages {
            append_message_to_transcript(&mut self.transcript_lines, msg, show_thinking);
        }
    }

    fn visible_lines(&self) -> Vec<String> {
        let mut lines = self.transcript_lines.clone();
        lines.extend(self.overlay_lines.iter().cloned());
        lines
    }

    fn has_content(&self) -> bool {
        !self.transcript_lines.is_empty() || !self.overlay_lines.is_empty()
    }

    fn push_overlay(&mut self, line: impl Into<String>) {
        self.overlay_lines.push(line.into());
    }
}

impl TuiHandler for ReplHandler {
    fn on_event(&mut self, event: &TuiEvent) {
        match event {
            TuiEvent::ThinkingDelta { .. }
            | TuiEvent::MessageDelta { .. }
            | TuiEvent::ToolUse { .. }
            | TuiEvent::ToolResult { .. }
            | TuiEvent::AssistantDone
            | TuiEvent::TurnComplete => {
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

fn run_loop(mut terminal: DefaultTerminal, ctx: &mut TuiContext) -> Result<()> {
    let mut handler = ReplHandler::new();
    handler.rebuild_from_session(ctx.session(), ctx.show_thinking);

    let mut input_buffer = String::new();
    let mut cursor_pos: usize = 0;
    let mut scroll_offset: usize = 0;
    let mut awaiting_approval: Option<ApprovalRequest> = None;
    let mut last_area = Rect::default();

    loop {
        if input_buffer.trim_start().starts_with('/') {
            let _ = ctx.refresh_compatibility_if_stale(Duration::from_millis(500));
        }

        terminal.draw(|frame| {
            let area = frame.area();
            last_area = area;

            let chunks = Layout::vertical([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

            let command_entries = filtered_command_entries(ctx, &input_buffer);

            frame.render_widget(
                Paragraph::new(launch_banner(ctx)).style(Style::default().fg(MUTED)),
                chunks[0],
            );

            render_body(
                frame,
                chunks[1],
                ctx,
                &handler,
                &command_entries,
                scroll_offset,
            );

            frame.render_widget(
                Paragraph::new(shortcuts_hint(&input_buffer)).style(Style::default().fg(MUTED)),
                chunks[2],
            );

            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        if awaiting_approval.is_some() {
                            "Approve tool? (y/n): "
                        } else {
                            "> "
                        },
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(input_buffer.clone()),
                ])),
                chunks[3],
            );

            if let Some(req) = &awaiting_approval {
                let modal_text = format!(
                    "Tool '{}' requires approval.\nInput: {}\n\nPress y to approve, n to deny.",
                    req.tool_name,
                    serde_json::to_string_pretty(&req.input).unwrap_or_default()
                );
                let modal_area = centered_rect(60, 40, area);
                let modal = Paragraph::new(modal_text)
                    .block(
                        Block::default()
                            .title("Tool Approval")
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(ACCENT)),
                    )
                    .wrap(Wrap { trim: true })
                    .alignment(Alignment::Center);
                frame.render_widget(Clear, modal_area);
                frame.render_widget(modal, modal_area);
            }
        })?;

        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(last_area);

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                if awaiting_approval.is_some() {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            let req = awaiting_approval.take().expect("approval request");
                            handler.push_overlay(format!("[info] Approved '{}'", req.tool_name));
                            execute_tool_with_approval(ctx, &mut handler, &req, true);
                            handler.rebuild_from_session(ctx.session(), ctx.show_thinking);
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') => {
                            let req = awaiting_approval.take().expect("approval request");
                            handler.push_overlay(format!("[info] Denied '{}'", req.tool_name));
                            let result_block = ContentBlock::tool_error(
                                &req.tool_use_id,
                                format!("Tool '{}' denied by user", req.tool_name),
                            );
                            ctx.session_mut()
                                .push_blocks(Role::Tool, vec![result_block]);
                            handler.rebuild_from_session(ctx.session(), ctx.show_thinking);
                        }
                        _ => {}
                    }
                    scroll_offset = handler
                        .visible_lines()
                        .len()
                        .saturating_sub(chunks[1].height as usize);
                    continue;
                }

                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        break;
                    }
                    KeyCode::Char('q') => {
                        break;
                    }
                    KeyCode::Enter => {
                        if !input_buffer.trim().is_empty() {
                            let prompt = input_buffer.trim().to_string();
                            input_buffer.clear();
                            cursor_pos = 0;

                            if handle_slash_command(ctx, &mut handler, &prompt)? {
                                handler.rebuild_from_session(ctx.session(), ctx.show_thinking);
                            } else {
                                ctx.submit_interactive(&prompt, &mut handler);
                                handler.rebuild_from_session(ctx.session(), ctx.show_thinking);
                            }

                            if let Some(req) = check_for_pending_approval(ctx) {
                                awaiting_approval = Some(req);
                            }
                        }
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
                            cursor_pos -= 1;
                        }
                    }
                    KeyCode::Right => {
                        if cursor_pos < input_buffer.len() {
                            cursor_pos += 1;
                        }
                    }
                    KeyCode::Home => cursor_pos = 0,
                    KeyCode::End => cursor_pos = input_buffer.len(),
                    KeyCode::Char(c) => {
                        input_buffer.insert(cursor_pos, c);
                        cursor_pos += 1;
                    }
                    _ => {}
                }

                scroll_offset = handler
                    .visible_lines()
                    .len()
                    .saturating_sub(chunks[1].height as usize);
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
    command_entries: &[CommandEntry],
    scroll_offset: usize,
) {
    if handler.has_content() {
        render_conversation(frame, area, handler, command_entries, scroll_offset);
    } else {
        render_dashboard(frame, area, ctx, command_entries);
    }
}

fn render_conversation(
    frame: &mut Frame,
    area: Rect,
    handler: &ReplHandler,
    command_entries: &[CommandEntry],
    scroll_offset: usize,
) {
    if command_entries.is_empty() {
        let transcript = Paragraph::new(handler.visible_lines().join("\n"))
            .wrap(Wrap { trim: false })
            .scroll((scroll_offset as u16, 0));
        frame.render_widget(transcript, area);
        return;
    }

    let chunks = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(command_palette_height(command_entries)),
    ])
    .split(area);

    let transcript = Paragraph::new(handler.visible_lines().join("\n"))
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset as u16, 0));

    frame.render_widget(transcript, chunks[0]);
    render_command_palette(frame, chunks[1], command_entries);
}

fn render_dashboard(
    frame: &mut Frame,
    area: Rect,
    ctx: &TuiContext,
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

    let panel = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(Line::from(vec![
            Span::styled(
                " ClawedCode ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("v{} ", env!("CARGO_PKG_VERSION")),
                Style::default().fg(MUTED),
            ),
        ]));
    let inner = panel.inner(chunks[0]);
    frame.render_widget(panel, chunks[0]);

    let body_chunks =
        Layout::horizontal([Constraint::Length(32), Constraint::Min(24)]).split(inner);

    let left = Paragraph::new(welcome_left_lines(ctx))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::RIGHT)
                .border_style(Style::default().fg(ACCENT)),
        );
    let right = Paragraph::new(welcome_right_lines(ctx)).wrap(Wrap { trim: false });

    frame.render_widget(left, body_chunks[0]);
    frame.render_widget(right, body_chunks[1]);

    if !command_entries.is_empty() && chunks.len() > 1 {
        render_command_palette(frame, chunks[1], command_entries);
    }
}

fn render_command_palette(frame: &mut Frame, area: Rect, entries: &[CommandEntry]) {
    let lines: Vec<Line> = entries
        .iter()
        .take(8)
        .map(|entry| {
            let name_color = match entry.source {
                CommandSource::BuiltIn => Color::White,
                CommandSource::Skill => Color::Cyan,
            };
            Line::from(vec![
                Span::styled(
                    format!("{:<18}", entry.name),
                    Style::default().fg(name_color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(entry.description.clone(), Style::default().fg(MUTED)),
            ])
        })
        .collect();

    let palette = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(MUTED)),
    );
    frame.render_widget(palette, area);
}

fn command_palette_height(entries: &[CommandEntry]) -> u16 {
    entries.len().min(8) as u16 + 2
}

fn launch_banner(ctx: &TuiContext) -> String {
    if ctx.session().messages.is_empty() {
        format!("Launching ClawedCode with {}...", ctx.model_name())
    } else {
        format!(
            "ClawedCode session {} in {}",
            &ctx.session().id.to_string()[..8],
            display_path(&ctx.session().cwd)
        )
    }
}

fn shortcuts_hint(input_buffer: &str) -> &'static str {
    if input_buffer.trim_start().starts_with('/') {
        "Enter to run a slash command. Ctrl+C or q exits."
    } else {
        "? for shortcuts"
    }
}

fn welcome_left_lines(ctx: &TuiContext) -> Vec<Line<'static>> {
    vec![
        Line::from(""),
        Line::from(Span::styled(
            "Welcome back!",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("       .-.-.       "),
        Line::from("      ( o o )      "),
        Line::from("     /|  V  |\\     "),
        Line::from("      | === |      "),
        Line::from("       -----       "),
        Line::from(""),
        Line::from(Span::styled(
            ctx.model_name().to_string(),
            Style::default().fg(MUTED),
        )),
        Line::from(Span::styled(
            provider_label().to_string(),
            Style::default().fg(MUTED),
        )),
        Line::from(Span::styled(
            display_path(&ctx.session().cwd),
            Style::default().fg(MUTED),
        )),
    ]
}

fn welcome_right_lines(ctx: &TuiContext) -> Vec<Line<'static>> {
    let mut lines = vec![
        section_title("Tips for getting started"),
        Line::from("Run /init to create a CLAUDE.md file with instructions for ClawedCode."),
    ];

    if launched_in_home(&ctx.session().cwd) {
        lines.push(Line::from(
            "Note: You have launched clawedcode in your home directory. For the best experience, launch it in a project directory instead.",
        ));
    } else {
        lines.push(Line::from(
            "Type /help to inspect built-in commands and discovered skills.",
        ));
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
        title,
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ))
}

fn append_message_to_transcript(lines: &mut Vec<String>, msg: &Message, show_thinking: bool) {
    let role_prefix = match msg.role {
        Role::System => return,
        Role::User => "You",
        Role::Assistant => "ClawedCode",
        Role::Tool => "Tool",
    };

    for block in &msg.content_blocks {
        match block {
            ContentBlock::Text { text } => {
                let text = text.trim();
                if !text.is_empty() {
                    lines.push(format!("{role_prefix}: {text}"));
                    lines.push(String::new());
                }
            }
            ContentBlock::Thinking { thinking } => {
                if show_thinking {
                    let thinking = thinking.trim();
                    if !thinking.is_empty() {
                        lines.push(format!("[thinking] {thinking}"));
                        lines.push(String::new());
                    }
                }
            }
            ContentBlock::ToolUse {
                id, name, input, ..
            } => {
                lines.push(format!(
                    "[tool] {name} (id={id}) {}",
                    serde_json::to_string(input).unwrap_or_default()
                ));
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let prefix = if *is_error {
                    "[tool_error]"
                } else {
                    "[tool_result]"
                };
                lines.push(format!("{prefix} {tool_use_id}: {content}"));
                lines.push(String::new());
            }
        }
    }

    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
}

fn check_for_pending_approval(ctx: &TuiContext) -> Option<ApprovalRequest> {
    let session = ctx.session();
    let last_msg = session.messages.last()?;
    if last_msg.role != Role::Assistant {
        return None;
    }

    for block in &last_msg.content_blocks {
        if let ContentBlock::ToolUse {
            id, name, input, ..
        } = block
        {
            if is_write_like(name) {
                return Some(ApprovalRequest {
                    tool_use_id: id.clone(),
                    tool_name: name.clone(),
                    input: input.clone(),
                });
            }
        }
    }

    None
}

fn execute_tool_with_approval(
    ctx: &mut TuiContext,
    _handler: &mut ReplHandler,
    req: &ApprovalRequest,
    approved: bool,
) {
    if approved {
        let result = ctx.runtime.execute_tool(
            &req.tool_use_id,
            &req.tool_name,
            req.input.clone(),
            &ctx.session().cwd,
        );
        if let ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } = result
        {
            ctx.session_mut().push_blocks(
                Role::Tool,
                vec![ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                }],
            );
        }
    } else {
        ctx.session_mut().push_blocks(
            Role::Tool,
            vec![ContentBlock::tool_error(
                &req.tool_use_id,
                format!("Tool '{}' denied by user", req.tool_name),
            )],
        );
    }
}

fn handle_slash_command(
    ctx: &mut TuiContext,
    handler: &mut ReplHandler,
    prompt: &str,
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

    match command {
        "/help" => {
            handler.push_overlay("[info] Commands:");
            for entry in all_command_entries(ctx) {
                handler.push_overlay(format!("[info] {:<18} {}", entry.name, entry.description));
            }
            let _ = ctx.save_session();
            return Ok(true);
        }
        "/clear" => {
            handler.overlay_lines.clear();
            let _ = ctx.save_session();
            return Ok(true);
        }
        "/update" => match clawedcode_core::update::run_self_update() {
            Ok(outcome) => {
                handler.push_overlay(format!(
                    "[info] Updated clawedcode via {:?} using `{}`",
                    outcome.method, outcome.command
                ));
            }
            Err(err) => {
                handler.push_overlay(format!("[info] Update failed: {err}"));
            }
        },
        other => {
            if let Some(skill) = find_skill_command(ctx, other) {
                execute_skill_command(ctx, handler, prompt, skill);
                return Ok(true);
            } else {
                handler.push_overlay(format!("[info] Unknown command `{other}`. Use `/help`."));
            }
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
) {
    if !skill_is_executable(&skill) {
        handler.push_overlay(format!(
            "[info] Skill '{}' has no content",
            skill.slash_command
        ));
        return;
    }

    let args = visible_prompt
        .strip_prefix(&skill.slash_command)
        .map(str::trim)
        .unwrap_or_default();
    let execution_prompt = build_skill_execution_prompt(&skill, args);
    handler.push_overlay(format!("[info] Executing skill: {}", skill.slash_command));

    ctx.submit_interactive_with_prompt_override(visible_prompt, Some(&execution_prompt), handler);

    if let Some(req) = check_for_pending_approval(ctx) {
        handler.push_overlay(format!("[warn] Tool '{}' requires approval", req.tool_name));
    }
}

fn all_command_entries(ctx: &TuiContext) -> Vec<CommandEntry> {
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
    ];

    for skill in ctx
        .skills()
        .iter()
        .filter(|skill| skill_is_executable(skill))
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

    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

fn filtered_command_entries(ctx: &TuiContext, input_buffer: &str) -> Vec<CommandEntry> {
    if !input_buffer.trim_start().starts_with('/') {
        return Vec::new();
    }

    let query = input_buffer.trim();
    all_command_entries(ctx)
        .into_iter()
        .filter(|entry| entry.name.starts_with(query))
        .collect()
}

fn is_write_like(tool_name: &str) -> bool {
    matches!(tool_name, "shell" | "apply_patch")
}

fn find_skill_command(ctx: &TuiContext, command: &str) -> Option<SkillDescriptor> {
    ctx.skills()
        .iter()
        .find(|skill| skill_is_executable(skill) && skill.slash_command == command)
        .cloned()
}

fn skill_is_executable(skill: &SkillDescriptor) -> bool {
    skill.slash_command.starts_with('/') && !skill.body.trim().is_empty()
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
    let Ok(entries) = fs::read_dir(&ctx.sessions_dir) else {
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
        if session.id == ctx.session().id {
            continue;
        }
        sessions.push(session);
    }

    sessions.sort_by_key(|session| Reverse(session.updated_at.timestamp()));
    sessions
        .into_iter()
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
