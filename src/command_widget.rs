use crate::commands::HomeCommand;
use ratatui::{
    prelude::{Buffer, Rect},
    widgets::StatefulWidget,
};
use tui_textarea::TextArea;

const BUFFER_SIZE: usize = 180;

pub enum CommandMode {
    None,
    Home(HomeCommand),
}

pub struct CommandWidget {
    mode: CommandMode,
    text: String,
}

impl StatefulWidget for CommandWidget {
    type State = CommandMode;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        todo!()
    }
}
