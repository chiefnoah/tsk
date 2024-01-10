#![allow(dead_code, unused_imports)]
use crate::{
    config::Config,
    db::Db,
    error::{Error, Result},
};
use crossterm::{
    event::{self, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use log::{error, debug};
use ratatui::{
    prelude::*,
    style::Style,
    widgets::{Block, Borders, List, ListDirection},
    Frame,
};
use tui_textarea::{Input, Key, TextArea};

pub(crate) enum AppState {
    Home,
    Details,
    Query,
    Exit,
}

pub(crate) fn render_home<B>(
    term: &mut Terminal<B>,
    db: &mut Db,
    config: &Config,
) -> Result<AppState>
where
    B: Backend,
{
    let tasks = db.get_top_n_tasks(config.num_top_tasks)?;
    let layout = Layout::default()
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .direction(Direction::Vertical);
    let mut command_editor = TextArea::default();
    command_editor.set_cursor_line_style(Style::default());
    command_editor.set_placeholder_text("Enter a command...");
    command_editor.set_style(Style::default().fg(Color::White));
    let mut count = 0;
    loop {
        let list = List::new(tasks.iter().map(|t| t.title.as_str()))
            .block(Block::default().title("tasks").borders(Borders::ALL))
            .style(Style::default().fg(Color::White))
            .highlight_style(Style::default().add_modifier(Modifier::ITALIC))
            .direction(ListDirection::BottomToTop);
        term.draw(|frame| {
            let chunks = layout.split(frame.size());
            frame.render_widget(list, chunks[0]);
            frame.render_widget(command_editor.widget(), chunks[1]);
        })?;

        match crossterm::event::read()?.into() {
            Input { key: Key::Esc, .. }
            | Input {
                key: Key::Char('q'),
                ctrl: true,
                ..
            } => break,
            Input {
                key: Key::Char('m'),
                ctrl: true,
                ..
            }
            | Input {
                key: Key::Enter, ..
            } => {}
            input => {
                if command_editor.input(input) {
                    command_editor.set_placeholder_text(format!("Count: {count}"));
                    count += 1;
                }
            }
        }
    }
    Ok(AppState::Exit)
}
