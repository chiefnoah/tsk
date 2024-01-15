use crate::types::QueryArgs;
use ratatui::{backend::Backend, Terminal};

use crate::{
    config::Config,
    db::Db,
    error::{Error, Result},
};

use super::home::AppState;

pub(crate) fn render_query<B: Backend>(
    term: &mut Terminal<B>,
    db: &mut Db,
    config: &Config,
    query: Vec<QueryArgs>,
) -> Result<AppState> {
    todo!();
}
