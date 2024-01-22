#![allow(dead_code)]
use crate::commands::HomeCommand;
use ratatui::{
    prelude::{Buffer, Rect},
    widgets::StatefulWidget,
};

const BUFFER_SIZE: usize = 180;

pub enum ParsingMode<T> {
    None,
    Parsing(T),
}

impl<T> Default for ParsingMode<T> {
    fn default() -> Self {
        ParsingMode::None
    }
}

pub enum CommandMode {
    Home(ParsingMode<HomeCommand>),
}

pub struct CommandWidget;

struct CommandState {
    text: String,
    command_mode: CommandMode,
}

impl Default for CommandState {
    fn default() -> Self {
        Self {
            text: String::with_capacity(BUFFER_SIZE),
            command_mode: CommandMode::Home(ParsingMode::default()),
        }
    }
}

impl StatefulWidget for CommandWidget {
    type State = CommandMode;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        todo!()
    }
}
