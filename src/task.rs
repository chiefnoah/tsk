#![allow(dead_code)]
use url::Url;

use crate::{errors::Error, workspace::Id};
use colored::Colorize;

#[derive(Debug, Eq, PartialEq)]
enum ParserOpcode {
    // Started by ` =`, terminated by `=
    Highlight(usize),
    // Started by ` [`, terminated by `](`
    Linktext(usize),
    // Started by `](`, terminated by `) `, must immedately follow a Linktext
    Link(usize),
    // Used to signal to the parser that the Linktext parsed properly and we should parse the
    // subsequent ( character as a
    LinkJoin,
    // Started by ` [[`, terminated by `]] `
    InternalLink(usize),
    // Started by ` *`, terminated by `* `
    Italics(usize),
    // Started by ` !`, termianted by `!`
    Bold(usize),
    // Started by ` _`, terminated by `_ `
    Underline(usize),
    // Started by ` -`, terminated by `- `
    Strikethrough(usize),
    // Started by `_ `, terminated by `_`
    UnorderedList(usize, u8),
    // Started by `^\w+1.`, terminated by `\n`
    OrderedList(usize, u8),
    // Started by `^`````, terminated by a [`ParserOpcode::BlockEnd`]
    BlockStart(usize),
    // Started by `$````, is terminal itself. It must appear on its own line and be preceeded by a
    // `\n` and followed by a `\n`
    BlockEnd(usize),
    // Started by ` ``, terminated by `` ` or `\n`
    InlineBlock(usize),
    // Started by `^\w+>`, terminated by `\n`
    Blockquote(usize),
}

pub(crate) struct ParsedTask {
    content: String,
    outgoing_internal_links: Vec<Id>,
    links: Vec<Url>,
}

pub(crate) fn parse(s: &str) -> Option<ParsedTask> {
    let mut out = String::with_capacity(s.len());
    let mut ops: Vec<ParserOpcode> = Vec::new();
    let mut stream = s.char_indices().peekable();
    let outgoing_internal_links = Vec::new();
    let mut links = Vec::new();
    loop {
        use ParserOpcode::*;
        match stream.next() {
            // there will always be an op code in the stack
            Some((pos, c)) => match dbg!((ops.last(), c)) {
                // Highlight terminal
                (Some(Highlight(start)), '=') => {
                    out.push_str(&s[start + 1..=pos - 1].reversed().to_string());
                    // reduce
                    ops.pop();
                }
                // Highlight start
                (op, '=') => {
                    ops.push(Highlight(pos));
                }
                (Some(Linktext(start)), ']') => match stream.peek() {
                    Some((_, '(')) => {
                        out.push_str(&s[start + 1..=pos - 1].bright_blue().underline().to_string());
                        ops.pop();
                        ops.push(LinkJoin)
                    }
                    // Terminal for internal link
                    Some((_, ']')) => {
                        out.push_str(&s[start + 1..=pos - 1].green().bold().to_string());
                        ops.pop();
                    }
                    _ => (),
                },
                (Some(Link(start)), ')') => {
                    if let Ok(uri) = Url::parse(&s[start + 1..=pos - 1]) {
                        links.push(uri);
                    }
                }
                (op, '[') => {
                    if let Some(op) = op {
                        ops.push(op);
                    }
                    ops.push(Linktext(pos));
                }
                (Some(LinkJoin), '(') => {
                    ops.push(Link(pos));
                }
                (None | Some(_), c) => out.push(c),
            },
            None => match ops.pop() {
                Some(
                    Plain(start) | Highlight(start) | Linktext(start) | Link(start)
                    | InternalLink(start) | Italics(start) | Bold(start) | Underline(start)
                    | Strikethrough(start),
                ) => {
                    // We have an
                    return None;
                }
                None => {
                    break;
                }
                Some(LinkJoin) => unreachable!(),
                Some(UnorderedList(_, _)) => todo!(),
                Some(OrderedList(_, _)) => todo!(),
                Some(BlockStart(_)) => todo!(),
                Some(BlockEnd(_)) => todo!(),
                Some(InlineBlock(_)) => todo!(),
                Some(Blockquote(_)) => todo!(),
            },
        }
    }
    Some(ParsedTask {
        content: out,
        outgoing_internal_links,
        links,
    })
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn test_highlight() {
        let input = "hello =world=";
        let output = parse(input).expect("parse to work");
        assert_eq!("hello \u{1b}[7mworld\u{1b}[0m", output.content);
    }

    #[test]
    fn test_highlight_bad() {
        let input = "hello =world";
        let output = parse(input).expect("parse to work");
        assert_eq!("hello =world", output.content);
    }

    #[test]
    fn test_link() {
        let input = "hello [world](https://ngp.computer)";
        let output = parse(input).expect("parse to work");
        assert_eq!(
            &[Url::parse("https://ngp.computer").unwrap()],
            output.links.as_slice()
        );
        assert_eq!("hello \u{1b}[4;94mworld\u{1b}[0m", output.content);
    }

    #[ignore = "Known styling bug"]
    #[test]
    fn test_link_no_terminal_link() {
        let input = "hello [world](https://ngp.computer";
        let output = parse(input).expect("parse to work");
        assert!(output.links.len() == 0);
        assert_eq!(input, output.content);
    }
    #[test]
    fn test_link_bad_no_start_link() {
        let input = "hello [world]https://ngp.computer)";
        let output = parse(input).expect("parse to work");
        assert!(output.links.len() == 0);
        assert_eq!(input, output.content);
    }
    #[test]
    fn test_link_bad_no_link() {
        let input = "hello [world]";
        let output = parse(input).expect("parse to work");
        assert!(output.links.len() == 0);
        assert_eq!(input, output.content);
    }
}
