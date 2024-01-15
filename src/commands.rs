#![allow(dead_code)]
use crate::types::{Tag, TaskId, TaskStatus};

use combine::error::{ParseError, UnexpectedParse};
use combine::parser::char::{alpha_num, char, digit, spaces, string};
use combine::parser::combinator::recognize;
use combine::parser::repeat::repeat_until;
use combine::stream::Range;
use combine::{any, eof, many, many1, satisfy, StreamOnce, skip_many1, RangeStream};
use combine::{
    attempt, between, parser::choice::choice, stream::position, EasyParser, Parser, Stream,
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
simple_command!(Start);

simple_command!(Quit);
simple_command!(Swap);
simple_command!(Todo);
simple_command!(Rot);
simple_command!(NRot);
simple_command! {
    Reprioritize,
    task_id -> TaskId
}

macro_rules! simple_parser(
    ($name:ident, $c:literal, $full:literal, $type:ty) => {
        fn $name<Input>() -> impl Parser<Input, Output = $type>
        where
            Input: Stream<Token = char>,
            Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
        {
            attempt(string($full))
                .or(char($c).map(|_| $full))
                .skip(spaces())
                .and(eof())
                .map(|_| <$type>::default())
        }
    };
    ($name:ident, $command:literal, $type:ty) => {
        fn $name<Input>() -> impl Parser<Input, Output = $type>
        where
            Input: Stream<Token = char>,
            Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
        {
            attempt(string($command))
                .skip(spaces())
                .and(eof())
                .map(|_| <$type>::default())
        }
    };
);

fn push<Input>() -> impl Parser<Input, Output = Push>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    attempt(string("push"))
        .or(char('p').map(|_| "push"))
        .skip(spaces())
        .with(alpha_num().and(repeat_until(any(), eof())))
        .map(|(f, rest): (char, String)| Push {
            title: Some(format!("{f}{rest}")),
        })
}

fn tsk<Input>() -> impl Parser<Input, Output = TaskId>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    attempt(string("tsk-"))
        .with(many1(digit()))
        .map(|s: String| s.parse::<TaskId>().unwrap())
}

simple_parser!(edit, 'e', "edit", Edit);
simple_parser!(drop, 'd', "drop", Drop);
simple_parser!(complete, 'c', "complete", Complete);
simple_parser!(quit, "quit", Quit);
simple_parser!(swap, "swap", Swap);
simple_parser!(start, 's', "start", Start);
simple_parser!(todo, 't', "todo", Todo);
simple_parser!(rot, "rot", Rot);
simple_parser!(nrot, '-', "-rot", NRot);

fn reprioritize<Input>() -> impl Parser<Input, Output = Reprioritize>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    attempt(string("rep"))
        .or(char('p').map(|_| "rep"))
        .skip(spaces())
        .with(tsk())
        .map(|s| Reprioritize { task_id: Some(s) })
}

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
    Start(Start),
    Todo(Todo),
    Reprioritize(Reprioritize),
    Rot(Rot),
    NRot(NRot),
    /*
    New(New),
    Start(Start),
    //Undo
    Backlog(Backlog),
    Connect(Option<(TaskId, Tag, TaskId)>),
    Make(Make),
    Query(Query),
    Link(Link),
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

enum TaskOrRelative {
    Task(TaskId),
    Relative(u8),
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
        start().map(HomeCommand::Start),
        todo().map(HomeCommand::Todo),
        rot().map(HomeCommand::Rot),
        nrot().map(HomeCommand::NRot),
        reprioritize().map(HomeCommand::Reprioritize),
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
