#![allow(dead_code)]
use crate::commands::HomeCommand;
use ratatui::{
    prelude::{Buffer, Rect},
    style::Style,
    widgets::{Block, StatefulWidget},
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

pub struct CommandWidget<'a> {
    block: Option<Block<'a>>,
    style: Style,
    highlight_style: Style,
}

impl<'a> Default for CommandWidget<'a> {
    fn default() -> Self {
        Self {
            block: Default::default(),
            style: Default::default(),
            highlight_style: Default::default(),
        }
    }
}

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

impl<'a> StatefulWidget for CommandWidget<'a> {
    type State = CommandMode;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        todo!()
    }
}
