#![allow(dead_code)]
use crate::types::{Tag, Task, TaskId, TaskStatus};

use combine::error::{ParseError, StreamError};
use combine::parser::char::letter;
use combine::stream::StreamErrorFor;
use combine::{
    easy, many1,
    parser::choice::{choice, optional},
    EasyParser, Parser, Stream,
};

/// `Query` represents a segment of a query when entering "query mode".
enum Query {
    Tag(bool, Tag),
    Status(bool, TaskStatus),
    Text(String),
}

enum HomeCommand {
    Push(String),
    Edit(TaskId),
    New(String),
    Start,
    Complete,
    //Undo
    Backlog,
    Todo(Option<TaskId>),
    Connect(TaskId, Tag, TaskId),
    Make(Tag),
    Query(Vec<Query>),
    Link(TaskId),
    Drop(TaskId),
    Rot,
    NRot,
    Swap,
    Reprioritize(u8),
    Deprioritize(Option<TaskId>),
    Quit,
}

enum QueryCommand {
    Filter(Vec<Query>),
}

enum DetailCommand {}

enum HomeParseResult {
    Partial,
    Complete,
}

#[derive(Debug)]
pub(crate) enum CommandParseError {
    UnknownCommand(String),
    InvalidArgument(Vec<String>),
    Unknown,
}

parser! {
    fn command[Input]()(Input) -> String
        where [Input: Stream<Token = char>]
    {
        choice((
            char('r').and_then(|r: String|)
        ))
    }
}

fn parse_home_commands(input: &str) -> Result<HomeParseResult, CommandParseError> {
    command()
        .easy_parse(input.to_ascii_lowercase())
        .map_err(|err| Err(CommandParseError::Unknown))
}
