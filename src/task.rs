#![allow(dead_code)]

use std::str::FromStr;
use url::Url;

use crate::workspace::Id;
use colored::Colorize;

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
enum ParserState {
    // Started by ` =`, terminated by `=
    Highlight(usize),
    // Started by ` [`, terminated by `](`
    Linktext(usize),
    // Started by `](`, terminated by `) `, must immedately follow a Linktext
    Link(usize),
    RawLink(usize),
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

    // TODO: implement these.
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

#[derive(Debug, Eq, PartialEq, Clone)]
pub(crate) enum ParsedLink {
    Internal(Id),
    External(Url),
}

pub(crate) struct ParsedTask {
    pub(crate) content: String,
    pub(crate) links: Vec<ParsedLink>,
}

pub(crate) fn parse(s: &str) -> Option<ParsedTask> {
    let mut state: Vec<ParserState> = Vec::new();
    let mut out = String::with_capacity(s.len());
    let mut stream = s.char_indices().peekable();
    let mut links = Vec::new();
    let mut last = '\0';
    use ParserState::*;
    loop {
        let state_last = state.last().cloned();
        match stream.next() {
            // there will always be an op code in the stack
            Some((_, c)) => {
                out.push(c);
                let end = out.len() - 1;
                match (last, c, state_last) {
                    ('[', '[', _) => {
                        state.push(InternalLink(end));
                    }
                    (']', ']', Some(InternalLink(il))) => {
                        state.pop();
                        let contents = out.get(il + 1..out.len() - 2)?;
                        if let Ok(id) = Id::from_str(&contents) {
                            let linktext = format!(
                                "{}{}",
                                contents.purple(),
                                super_num(links.len() + 1).purple()
                            );
                            out.replace_range(il - 1..out.len(), &linktext);
                            links.push(ParsedLink::Internal(id));
                        } else {
                            panic!("Internal link is not a valid id: {contents}");
                        }
                    }
                    (' ' | '\r' | '\n', '[', _) => {
                        state.push(Linktext(end));
                    }
                    (']', '(', Some(Linktext(_))) => {
                        state.push(Link(end));
                    }
                    (')', ' ' | '\n' | '\r' | '.' | '!' | '?', Some(Link(_))) => {
                        let linkpos = if let Link(lp) = state.pop().unwrap() {
                            lp
                        } else {
                            // remove the linktext state, it is always present.
                            state.pop();
                            continue;
                        };
                        let linktextpos = if let Linktext(lt) = state.pop().unwrap() {
                            lt
                        } else {
                            continue;
                        };
                        let linktext = format!(
                            "{}{}",
                            out.get(linktextpos + 1..linkpos - 1)?.blue(),
                            super_num(links.len() + 1).purple()
                        );
                        let link = out.get(linkpos + 1..end - 1)?;
                        if let Ok(url) = Url::parse(link) {
                            links.push(ParsedLink::External(url));
                            out.replace_range(linktextpos..end, &linktext);
                        }
                    }
                    ('>', ' ' | '\n' | '\r' | '.' | '!' | '?', Some(RawLink(hl))) => {
                        state.pop();
                        let link = out.get(hl + 1..out.len() - 2)?;
                        if let Ok(url) = Url::parse(link) {
                            let linktext =
                                format!("{}{}", link.blue(), super_num(links.len() + 1).purple());
                            links.push(ParsedLink::External(url));
                            out.replace_range(hl..end, &linktext);
                        }
                    }
                    (' ' | '\r' | '\n', '<', _) => {
                        state.push(RawLink(end));
                    }
                    ('=', ' ' | '\n' | '\r' | '.' | '!' | '?', Some(Highlight(hl))) => {
                        state.pop();
                        out.replace_range(
                            hl..end,
                            &out.get(hl + 1..out.len() - 2)?.reversed().to_string(),
                        );
                    }
                    (' ' | '\r' | '\n', '=', _) => {
                        state.push(Highlight(end));
                    }
                    (' ' | '\r' | '\n', '*', _) => {
                        state.push(Italics(end));
                    }
                    ('*', ' ' | '\n' | '\r' | '.' | '!' | '?', Some(Italics(il))) => {
                        state.pop();
                        out.replace_range(
                            il..end,
                            &out.get(il + 1..out.len() - 2)?.italic().to_string(),
                        );
                    }
                    (' ' | '\r' | '\n', '!', _) => {
                        state.push(Bold(end));
                    }
                    ('!', ' ' | '\n' | '\r' | '.' | '!' | '?', Some(Bold(il))) => {
                        state.pop();
                        out.replace_range(il..end, &out.get(il + 1..end - 1)?.bold().to_string());
                    }
                    (' ' | '\r' | '\n', '_', _) => {
                        state.push(Underline(end));
                    }
                    ('_', ' ' | '\n' | '\r' | '.' | '!' | '?', Some(Underline(il))) => {
                        state.pop();
                        out.replace_range(
                            il..end,
                            &out.get(il + 1..end - 1)?.underline().to_string(),
                        );
                    }
                    (' ' | '\r' | '\n', '~', _) => {
                        state.push(Strikethrough(end));
                    }
                    ('~', ' ' | '\n' | '\r' | '.' | '!' | '?', Some(Strikethrough(il))) => {
                        state.pop();
                        out.replace_range(
                            il..end,
                            &out.get(il + 1..end - 1)?.strikethrough().to_string(),
                        );
                    }
                    ('`', ' ' | '\n' | '\r' | '.' | '!' | '?', Some(InlineBlock(hl))) => {
                        state.pop();
                        out.replace_range(
                            hl..end,
                            &out.get(hl + 1..out.len() - 1)?.green().to_string(),
                        );
                    }
                    (' ' | '\r' | '\n', '`', _) => {
                        state.push(InlineBlock(end));
                    }
                    _ => (),
                }
                if c == '\n' || c == '\r' {
                    state.clear();
                }
                last = c;
            }
            None => break,
        }
    }
    Some(ParsedTask {
        content: out,
        links,
    })
}

/// Converts a unsigned integer into a superscripted string
fn super_num(num: usize) -> String {
    let num_str = num.to_string();
    let mut out = String::with_capacity(num_str.len());
    for char in num_str.chars() {
        out.push(match char {
            '0' => '⁰',
            '1' => '¹',
            '2' => '²',
            '3' => '³',
            '4' => '⁴',
            '5' => '⁵',
            '6' => '⁶',
            '7' => '⁷',
            '8' => '⁸',
            '9' => '⁹',
            _ => unreachable!(),
        });
    }
    out
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
        let input = "hello =world\n";
        let output = parse(input).expect("parse to work");
        assert_eq!(input, output.content);
    }

    #[test]
    fn test_link() {
        let input = "hello [world](https://ngp.computer)\n";
        let output = parse(input).expect("parse to work");
        assert_eq!(
            &[ParsedLink::External(
                Url::parse("https://ngp.computer").unwrap()
            )],
            output.links.as_slice()
        );
        assert_eq!(
            "hello \u{1b}[34mworld\u{1b}[0m\u{1b}[35m¹\u{1b}[0m\n",
            output.content
        );
    }

    #[test]
    fn test_link_no_terminal_link() {
        let input = "hello [world](https://ngp.computer\n";
        let output = parse(input).expect("parse to work");
        assert!(output.links.len() == 0);
        assert_eq!(input, output.content);
    }
    #[test]
    fn test_link_bad_no_start_link() {
        let input = "hello [world]https://ngp.computer)\n";
        let output = parse(input).expect("parse to work");
        assert!(output.links.len() == 0);
        assert_eq!(input, output.content);
    }
    #[test]
    fn test_link_bad_no_link() {
        let input = "hello [world]\n";
        let output = parse(input).expect("parse to work");
        assert!(output.links.len() == 0);
        assert_eq!(input, output.content);
    }

    #[test]
    fn test_internal_link_good() {
        let input = "hello [[tsk-123]]\n";
        let output = parse(input).expect("parse to work");
        assert_eq!(&[ParsedLink::Internal(Id(123))], output.links.as_slice());
        assert_eq!(
            "hello \u{1b}[35mtsk-123\u{1b}[0m\u{1b}[35m¹\u{1b}[0m\n",
            output.content
        );
    }

    #[test]
    fn test_internal_link_bad() {
        let input = "hello [[tsk-123";
        let output = parse(input).expect("parse to work");
        assert!(output.links.len() == 0);
        assert_eq!(input, output.content);
    }

    #[test]
    fn test_italics() {
        let input = "hello *world*\n";
        let output = parse(input).expect("parse to work");
        assert_eq!("hello \u{1b}[3mworld\u{1b}[0m\n", output.content);
    }

    #[test]
    fn test_italics_bad() {
        let input = "hello *world";
        let output = parse(input).expect("parse to work");
        assert_eq!(input, output.content);
    }

    #[test]
    fn test_bold() {
        let input = "hello !world!\n";
        let output = parse(input).expect("parse to work");
        assert_eq!("hello \u{1b}[1mworld\u{1b}[0m\n", output.content);
    }

    #[test]
    fn test_bold_bad() {
        let input = "hello !world\n";
        let output = parse(input).expect("parse to work");
        assert_eq!(input, output.content);
    }

    #[test]
    fn test_underline() {
        let input = "hello _world_\n";
        let output = parse(input).expect("parse to work");
        assert_eq!("hello \u{1b}[4mworld\u{1b}[0m\n", output.content);
    }

    #[test]
    fn test_underline_bad() {
        let input = "hello _world\n";
        let output = parse(input).expect("parse to work");
        assert_eq!(input, output.content);
    }

    #[test]
    fn test_strikethrough() {
        let input = "hello ~world~\n";
        let output = parse(input).expect("parse to work");
        assert_eq!("hello \u{1b}[9mworld\u{1b}[0m\n", output.content);
    }

    #[test]
    fn test_strikethrough_bad() {
        let input = "hello ~world\n";
        let output = parse(input).expect("parse to work");
        assert_eq!(input, output.content);
    }

    #[test]
    fn test_inlineblock() {
        let input = "hello `world`\n";
        let output = parse(input).expect("parse to work");
        assert_eq!("hello \u{1b}[32mworld\u{1b}[0m\n", output.content);
    }

    #[test]
    fn test_inlineblock_bad() {
        let input = "hello `world\n";
        let output = parse(input).expect("parse to work");
        assert_eq!(input, output.content);
    }

    #[test]
    fn test_multiple_styles() {
        let input = "hello *italic* ~strikethrough~ !bold!\n";
        let output = parse(input).expect("parse to work");
        assert_eq!(
            "hello \u{1b}[3mitalic\u{1b}[0m \u{1b}[9mstrikethrough\u{1b}[0m \u{1b}[1mbold\u{1b}[0m\n",
            output.content
        );
    }
}
