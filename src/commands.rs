#![allow(dead_code)]
use crate::types::{Tag, TaskId, TaskStatus};

use combine::error::ParseError;
use combine::parser::char::{alpha_num, char, spaces, string};
use combine::parser::repeat::repeat_until;
use combine::{any, eof, many, many1, parser, satisfy};
use combine::{
    between,
    parser::choice::{choice, optional},
    stream::position,
    EasyParser, Parser, StdParseResult, Stream,
};

pub(crate) trait Command: Sized {
    type Arg;
    fn args(&self) -> Option<&Self::Arg>;
    fn valid<F>(&self, check: Option<F>) -> bool
    where
        F: FnMut(&Self::Arg) -> bool;
    fn wait() -> bool {
        true
    }
}

macro_rules! simple_command {
    {$name:ident, $arg:ident -> $argty:ty} => {
        #[derive(Debug)]
        pub(crate) struct $name {
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

            fn args(&self) -> Option<&Self::Arg> {
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
        simple_command!($name, none -> ());
    };
}

simple_command! {
    Push,
    title -> String
}
simple_command! {
    Edit,
    task_id -> TaskId
}
simple_command! {
    Drop,
    task_id -> TaskId
}

simple_command! {
    Complete,
    task_id -> TaskId
}

simple_command!(Quit);
simple_command!(Swap);

macro_rules! simple_parser(
    ($name:ident, $c:literal, $rest:literal, $type:ty) => {
        fn $name<Input>() -> impl Parser<Input, Output = $type>
        where
            Input: Stream<Token = char>,
            Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
        {
            char($c)
                .and(optional(string($rest)).silent())
                .map(|_| <$type>::default())
        }
    };
);

fn push<Input>() -> impl Parser<Input, Output = Push>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    char('p')
        .and(optional(string("ush")))
        .skip(spaces())
        .with(alpha_num().and(repeat_until(any(), eof())))
        .map(|(f, rest): (char, String)| Push {
            title: Some(format!("{f}{rest}")),
        })
}

simple_parser!(edit, 'e', "dit", Edit);
simple_parser!(drop, 'd', "rop", Drop);
simple_parser!(complete, 'c', "omplete", Complete);
simple_parser!(quit, 'q', "uit", Quit);
simple_parser!(swap, 's', "wap", Swap);
/*
simple_command!(New, title, String);
simple_command!(Start);
simple_command!(Complete);
simple_command!(Backlog);
simple_command!(Todo, task_id, TaskId);
simple_command!(Make, tag, String);
simple_command!(Query, query, Vec<QueryArgs>);
simple_command!(Link, task_id, TaskId);
simple_command!(Drop, task_id, TaskId);
simple_command!(Rot);
simple_command!(NRot);
simple_command!(Swap);
simple_command!(Reprioritize, relative_order, u8);
simple_command!(Deprioritize);
simple_command!(Quit);
*/

/// `Query` represents a segment of a query when entering "query mode".
enum QueryArgs {
    Tag(bool, Tag),
    Status(bool, TaskStatus),
    Text(String),
}

#[derive(Debug)]
pub(crate) enum HomeCommand {
    Push(Push),
    Edit(Edit),
    Drop(Drop),
    Complete(Complete),
    Quit(Quit),
    Swap(Swap),
    /*
    New(New),
    Start(Start),
    //Undo
    Backlog(Backlog),
    Todo(Todo),
    Connect(Option<(TaskId, Tag, TaskId)>),
    Make(Make),
    Query(Query),
    Link(Link),
    Rot(Rot),
    NRot(NRot),
    Reprioritize(Reprioritize),
    Deprioritize(Deprioritize),
    Quit(Quit),
    */
}

enum DetailCommand {}

#[derive(Debug)]
pub(crate) enum CommandParseError {
    UnknownCommand(String),
    InvalidArgument(Vec<String>),
    Unknown,
}

fn str<Input>() -> impl Parser<Input, Output = String>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    between(char('"'), char('"'), many(satisfy(|c| c != '"')))
}

fn command<Input>() -> impl Parser<Input, Output = HomeCommand>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    choice((
        push().map(HomeCommand::Push),
        edit().map(HomeCommand::Edit),
        drop().map(HomeCommand::Drop),
        complete().map(HomeCommand::Complete),
        swap().map(HomeCommand::Swap),
        quit().map(HomeCommand::Quit),
    ))
}

pub(crate) fn parse_home_command(input: &str) -> Option<HomeCommand> {
    let lower = input.to_ascii_lowercase();
    let out = command()
        .easy_parse(position::Stream::new(lower.as_str()))
        .map(|c| c.0);
    out.ok()
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_parse_push() {
        let input = "push this is a test";
        let command = parse_home_command(input);
        assert!(command.is_some());
    }
}
