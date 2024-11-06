#![allow(dead_code)]
use url::Url;

use crate::{errors::Error, workspace::Id};
use colored::Colorize;

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
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

#[derive(Default)]
struct ParseState {
    highlight: Option<usize>,
    link: Option<usize>,
    internal: Option<usize>,
    italics: Option<usize>,
    bold: Option<usize>,
    underline: Option<usize>,
    strikethrough: Option<usize>,
    block: bool,
    inline: Option<usize>,
    quote: Option<(usize, u8)>,
}

pub(crate) fn parse(s: &str) -> Option<ParsedTask> {
    let mut state = ParseState::default();
    let mut out = s.to_string();
    let mut stream = s.char_indices().peekable();
    let outgoing_internal_links = Vec::new();
    let links = Vec::new();
    let mut last = '\0';
    loop {
        match stream.next() {
            // there will always be an op code in the stack
            Some((pos, c)) => {
                match (last, c, &state) {
                    (
                        ' ',
                        '=',
                        ParseState {
                            highlight: Some(hl),
                            ..
                        },
                    )
                    | (
                        '=',
                        ' ',
                        ParseState {
                            highlight: Some(hl),
                            ..
                        },
                    )
                    | (
                        '=',
                        '\n',
                        ParseState {
                            highlight: Some(hl),
                            ..
                        },
                    ) => {
                        out.replace_range(
                            *hl..pos,
                            &out.get(*hl + 1..pos - 1)?.reversed().to_string(),
                        );
                    }
                    (
                        ' ',
                        '=',
                        ParseState {
                            highlight: None, ..
                        },
                    ) => {
                        state.highlight = Some(pos);
                    }

                    _ => (),
                }
                last = c;
            }
            None => break,
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
        let input = "hello =world=\n";
        let output = parse(input).expect("parse to work");
        assert_eq!("hello \u{1b}[7mworld\u{1b}[0m\n", output.content);
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
