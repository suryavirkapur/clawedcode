use anyhow::Result;
use clawedcode_core::content::ContentBlock;
use clawedcode_core::interactive::{ApprovalRequest, TuiContext, TuiEvent, TuiHandler};
use clawedcode_core::session::{Message, Role};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    DefaultTerminal,
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use std::io::{self, stdout};

pub fn run_with_context(mut ctx: TuiContext) -> Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
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

    fn rebuild_from_session(&mut self, session: &clawedcode_core::session::Session) {
        self.transcript_lines.clear();
        for msg in &session.messages {
            append_message_to_transcript(&mut self.transcript_lines, msg);
        }
    }

    fn visible_lines(&self) -> Vec<String> {
        let mut lines = self.transcript_lines.clone();
        lines.extend(self.overlay_lines.iter().cloned());
        lines
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
            "[approval] Tool '{}' requires approval. Input: {}",
            request.tool_name,
            serde_json::to_string(&request.input).unwrap_or_default()
        ));
        self.state = AppState::AwaitingApproval;
        false
    }
}

fn run_loop(mut terminal: DefaultTerminal, ctx: &mut TuiContext) -> Result<()> {
    let mut handler = ReplHandler::new();
    handler.rebuild_from_session(ctx.session());

    let mut input_buffer = String::new();
    let mut cursor_pos: usize = 0;
    let mut scroll_offset: usize = 0;
    let mut awaiting_approval: Option<ApprovalRequest> = None;
    let mut last_area = Rect::default();

    loop {
        terminal.draw(|frame| {
            let area = frame.area();
            last_area = area;
            let chunks = Layout::vertical([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(3),
            ])
            .split(area);

            let header_text = format!(
                " ClawedCode REPL | Session: {} | q=quit, Enter=submit ",
                ctx.session().id
            );
            let header = Paragraph::new(header_text)
                .block(Block::default().borders(Borders::ALL))
                .style(Style::default().fg(Color::Cyan));

            let transcript_text = handler.visible_lines().join("\n");
            let transcript = Paragraph::new(transcript_text)
                .block(Block::default().title("Transcript").borders(Borders::ALL))
                .wrap(Wrap { trim: false })
                .scroll((scroll_offset as u16, 0));

            let prompt_label = if awaiting_approval.is_some() {
                "Approve tool? (y/n): "
            } else {
                "Enter prompt: "
            };

            let input = Paragraph::new(Line::from(vec![
                Span::styled(prompt_label, Style::default().fg(Color::Yellow)),
                Span::raw(input_buffer.clone()),
            ]))
            .block(Block::default().title("Input").borders(Borders::ALL));

            frame.render_widget(header, chunks[0]);
            frame.render_widget(transcript, chunks[1]);
            frame.render_widget(input, chunks[2]);

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
                            .border_style(Style::default().fg(Color::Yellow)),
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
            Constraint::Length(3),
        ])
        .split(last_area);

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                if awaiting_approval.is_some() {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            let req = awaiting_approval.take().unwrap();
                            handler.push_overlay(format!("[approval] Approved '{}'", req.tool_name));
                            execute_tool_with_approval(ctx, &mut handler, &req, true);
                            handler.rebuild_from_session(ctx.session());
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') => {
                            let req = awaiting_approval.take().unwrap();
                            handler.push_overlay(format!("[approval] Denied '{}'", req.tool_name));
                            let result_block = ContentBlock::tool_error(
                                &req.tool_use_id,
                                format!("Tool '{}' denied by user", req.tool_name),
                            );
                            ctx.session_mut()
                                .push_blocks(Role::Tool, vec![result_block]);
                            handler.rebuild_from_session(ctx.session());
                        }
                        _ => {}
                    }
                    scroll_offset = handler
                        .visible_lines()
                        .len()
                        .saturating_sub(chunks[1].height.saturating_sub(2) as usize);
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
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
                                handler.rebuild_from_session(ctx.session());
                            } else {
                                ctx.submit_interactive(&prompt, &mut handler);
                                handler.rebuild_from_session(ctx.session());
                            }

                            if let Some(req) = check_for_pending_approval(ctx, &handler) {
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
                    KeyCode::Up => {}
                    KeyCode::Down => {}
                    KeyCode::PageUp => {}
                    KeyCode::PageDown => {}
                    KeyCode::Home => {
                        cursor_pos = 0;
                    }
                    KeyCode::End => {
                        cursor_pos = input_buffer.len();
                    }
                    KeyCode::Char(c) => {
                        input_buffer.insert(cursor_pos, c);
                        cursor_pos += 1;
                    }
                    _ => {}
                }

                scroll_offset = handler
                    .visible_lines()
                    .len()
                    .saturating_sub(chunks[1].height.saturating_sub(2) as usize);
            }
        }
    }

    Ok(())
}

fn append_message_to_transcript(lines: &mut Vec<String>, msg: &Message) {
    let role_prefix = match msg.role {
        Role::System => "[system]",
        Role::User => "[user]",
        Role::Assistant => "[assistant]",
        Role::Tool => "[tool]",
    };

    for block in &msg.content_blocks {
        match block {
            ContentBlock::Text { text } => {
                lines.push(format!("{role_prefix} {text}"));
            }
            ContentBlock::Thinking { thinking } => {
                lines.push(format!("[thinking] {thinking}"));
            }
            ContentBlock::ToolUse {
                id, name, input, ..
            } => {
                lines.push(format!(
                    "[tool_use] {name} (id={id}) {}",
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
            }
        }
    }
}

fn check_for_pending_approval(ctx: &TuiContext, _handler: &ReplHandler) -> Option<ApprovalRequest> {
    let session = ctx.session();
    if let Some(last_msg) = session.messages.last() {
        if last_msg.role == Role::Assistant {
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
    if !prompt.starts_with('/') {
        return Ok(false);
    }

    let command = prompt.split_whitespace().next().unwrap_or(prompt);
    match command {
        "/help" => {
            handler.push_overlay("[system] Built-in commands:");
            handler.push_overlay("[system] /help   Show available REPL commands");
            handler.push_overlay("[system] /update Update clawedcode using npm or cargo, depending on how it was installed");
            handler.push_overlay("[system] /clear  Clear local transcript overlays");
        }
        "/clear" => {
            handler.overlay_lines.clear();
        }
        "/update" => match clawedcode_core::update::run_self_update() {
            Ok(outcome) => {
                handler.push_overlay(format!(
                    "[system] Updated clawedcode via {:?} using `{}`",
                    outcome.method, outcome.command
                ));
            }
            Err(err) => {
                handler.push_overlay(format!("[system] Update failed: {err}"));
            }
        },
        other => {
            handler.push_overlay(format!(
                "[system] Unknown command `{other}`. Use `/help`."
            ));
        }
    }

    let _ = ctx.save_session();
    Ok(true)
}

fn is_write_like(tool_name: &str) -> bool {
    matches!(tool_name, "shell" | "apply_patch")
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
