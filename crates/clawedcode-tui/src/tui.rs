use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    DefaultTerminal,
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
};
use std::io::{self, stdout};

pub fn run() -> Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let terminal = ratatui::init();
    let result = run_loop(terminal);
    restore_terminal()?;
    result
}

fn run_loop(mut terminal: DefaultTerminal) -> Result<()> {
    let mut should_quit = false;

    while !should_quit {
        terminal.draw(|frame| {
            let area = frame.area();
            let chunks = Layout::vertical([Constraint::Length(5), Constraint::Min(0)]).split(area);

            let header = Paragraph::new(
                "ClawedCode\nRust-native coding shell\nPress q to exit.",
            )
            .block(Block::default().title("Status").borders(Borders::ALL))
            .wrap(Wrap { trim: true });

            let body = Paragraph::new(
                "This terminal surface is intentionally small for the first pass.\nThe next iteration should bind runtime events, prompt editing, approval flows, and transcript rendering here.",
            )
            .block(Block::default().title("Workbench").borders(Borders::ALL))
            .wrap(Wrap { trim: true });

            frame.render_widget(header, chunks[0]);
            frame.render_widget(body, chunks[1]);
        })?;

        if event::poll(std::time::Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && matches!(key.code, KeyCode::Char('q')) {
                    should_quit = true;
                }
            }
        }
    }

    Ok(())
}

fn restore_terminal() -> Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    ratatui::restore();
    Ok(())
}
