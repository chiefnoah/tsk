#![allow(dead_code)]
use crate::types::{Tag, TaskId, TaskStatus};

use combine::error::ParseError;
use combine::parser::char::{char, spaces, string};
use combine::{
    parser::choice::{choice, optional},
    stream::{position, ResetStream},
    EasyParser, Parser, Stream,
};

pub(crate) trait Command {
    type Arg;
    fn suggestion(&self) -> Option<&'static str> {
        None
    }
    fn arg(&self) -> Option<&Self::Arg>;
    fn valid<F>(&self, check: Option<F>) -> bool
    where
        F: FnMut(&Self::Arg) -> bool;
    fn wait() -> bool {
        true
    }
}

macro_rules! simple_command {
    ($name:ident, $arg:ident, $argty:ty) => {
        struct $name {
            $arg: Option<$argty>,
        }

        impl Default for $name {
            fn default() -> Self {
                Self {
                    $arg: Default::default(),
                }
            }
        }

        impl Command for $name {
            type Arg = $argty;

            fn arg(&self) -> Option<&Self::Arg> {
                self.$arg.as_ref()
            }

            fn valid<F>(&self, check: Option<F>) -> bool
            where
                F: FnMut(&Self::Arg) -> bool,
            {
                if check.is_some() && self.$arg.is_some() {
                    check.unwrap()(&self.$arg.as_ref().unwrap())
                } else {
                    self.$arg.is_some()
                }
            }
        }
    };
    ($name:ident) => {
        simple_command!($name, none, ());
    };
}

simple_command!(Push, title, String);
simple_command!(Edit, task_id, TaskId);

/// `Query` represents a segment of a query when entering "query mode".
enum Query {
    Tag(bool, Tag),
    Status(bool, TaskStatus),
    Text(String),
}

enum HomeCommand {
    Push(Push),
    Edit(Edit),
    New(Option<String>),
    Start,
    Complete,
    //Undo
    Backlog,
    Todo(Option<TaskId>),
    Connect(Option<(TaskId, Tag, TaskId)>),
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
    Partial(HomeCommand),
    Complete(HomeCommand, Vec<String>),
}

#[derive(Debug)]
pub(crate) enum CommandParseError {
    UnknownCommand(String),
    InvalidArgument(Vec<String>),
    Unknown,
}

fn command<Input>() -> impl Parser<Input, Output = HomeCommand>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    let push = || char('p').and(optional(string("ush")).silent());
    let edit = || char('e').and(optional(string("edit")).silent());

    choice((
        push().map(|_| HomeCommand::Push(Push::default())),
        edit().map(|_| HomeCommand::Edit(Edit::default())),
    ))
    .skip(spaces().silent())
}

fn parse_home_commands(input: &str) -> HomeCommand {
    let lower = input.to_ascii_uppercase();
    let (out, _) = command()
        .easy_parse(position::Stream::new(lower.as_str()))
        .unwrap();
    out
}
