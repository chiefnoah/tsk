#![allow(dead_code)]
use crate::types::TaskId;

use combine::error::ParseError;
use combine::parser::char::{alpha_num, char, digit, spaces, string};
use combine::parser::repeat::repeat_until;
use combine::{any, eof, many, many1, satisfy};
use combine::{
    attempt, between, parser::choice::choice, stream::position, EasyParser, Parser, Stream,
};

pub(crate) trait Command {
    type Arg;
    fn args(&self) -> Option<&Self::Arg>;
    fn valid<F>(&self, check: Option<F>) -> bool
    where
        F: FnMut(&Self::Arg) -> bool;
    fn wait() -> bool {
        true
    }

    fn parse_argument<Input>(&self) -> impl Parser<Input, Output = Self::Arg>
    where
        Input: Stream<Token = char>,
        Input::Error: ParseError<Input::Token, Input::Range, Input::Position>;
}

macro_rules! simple_command {
    {$name:ident, $arg:ident -> $argty:ty, $parser:expr} => {
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

            fn parse_argument<Input>(&self) -> impl Parser<Input, Output = $argty>
                where
                    Input: Stream<Token = char>,
                    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
            {
                spaces().with($parser)
            }
        }
    };
    ($name:ident) => {
        simple_command!($name, none -> (), eof());
    };
}

simple_command! {
    Push,
    title -> String,
    repeat_until(any(), eof()).map(|c| c)
}
simple_command! {
    Edit,
    task_id -> TaskIdentifier,
    task_identifier()
}
simple_command! {
    Drop,
    task_id -> TaskIdentifier,
    task_identifier()
}

simple_command!(Complete);
simple_command!(Start);

simple_command!(Quit);
simple_command!(Swap);
simple_command!(Todo);
simple_command!(Rot);
simple_command!(NRot);
simple_command! {
    Reprioritize,
    task_id -> TaskIdentifier,
    task_identifier()
}
simple_command! {
    Make,
    name -> String,
    repeat_until(any(), eof()).map(|c| c)
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

fn make<Input>() -> impl Parser<Input, Output = Make>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    attempt(string("make"))
        .or(char('m').map(|_| "make"))
        .skip(spaces())
        .with(many1(alpha_num()))
        .map(|s: String| Make { name: Some(s) })
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
        .map(|s| Reprioritize {
            task_id: Some(TaskIdentifier::Task(s)),
        })
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
    Make(Make),
    /*
    New(New),
    Backlog(Backlog),
    Connect(Option<(TaskId, Tag, TaskId)>),
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

#[derive(Debug, Clone)]
pub(crate) enum TaskIdentifier {
    Task(TaskId),
    Stack(u8),
}

fn task_identifier<Input>() -> impl Parser<Input, Output = TaskIdentifier>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    // unwrap is safe in parse becuase we are guaranteed to only have digits
    choice((
        attempt(many1(digit()).map(|c: String| TaskIdentifier::Stack(c.parse::<u8>().unwrap()))),
        tsk().map(TaskIdentifier::Task),
    ))
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
        make().map(HomeCommand::Make),
        // r
        rot().map(HomeCommand::Rot),
        nrot().map(HomeCommand::NRot),
        reprioritize().map(HomeCommand::Reprioritize),
        // quit
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
