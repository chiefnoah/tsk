mod commands;
mod config;
mod db;
mod error;
mod types;
mod views;
use crate::error::Result;
use crate::views::home::{render_home, AppState};
use crate::{config::Config, db::Db};
//use chrono::{DateTime, Utc};
use crossterm::{
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use log::debug;
use ratatui::prelude::{CrosstermBackend, Terminal};
use std::io::stdout;

fn main() -> Result<()> {
    env_logger::init();
    debug!("Initializing db...");
    let mut db = Db::new()?;
    let config = Config::default();
    debug!("Initialized db.");
    stdout().execute(EnterAlternateScreen)?;
    enable_raw_mode()?;

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    terminal.draw(|frame| {
        let mut next = AppState::Home;
        loop {
            match next {
                AppState::Home => {
                    next = match render_home(frame, &mut db, &config) {
                        Ok(n) => n,
                        Err(_) => return,
                    }
                }
                AppState::Details => todo!(),
                AppState::Query => todo!(),
                AppState::Exit => return,
            }
        }
    })?;

    stdout().execute(LeaveAlternateScreen)?;
    disable_raw_mode()?;

    Ok(())
}
