#![allow(dead_code)]

use std::{collections::HashSet, str::FromStr};
use url::Url;

use crate::workspace::Id;
use colored::Colorize;

/// Returns true if the character is a word boundary (whitespace or punctuation)
fn is_boundary(c: char) -> bool {
    c.is_whitespace() || c.is_ascii_punctuation()
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
enum ParserState {
    // Started by ` =`, terminated by `=
    Highlight(usize, usize),
    // Started by ` [`, terminated by `](`
    Linktext(usize, usize),
    // Started by `](`, terminated by `) `, must immedately follow a Linktext
    Link(usize, usize),
    RawLink(usize, usize),
    // Started by ` [[`, terminated by `]] `
    InternalLink(usize, usize),
    // Started by ` *`, terminated by `* `
    Italics(usize, usize),
    // Started by ` !`, termianted by `!`
    Bold(usize, usize),
    // Started by ` _`, terminated by `_ `
    Underline(usize, usize),
    // Started by ` -`, terminated by `- `
    Strikethrough(usize, usize),

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
    InlineBlock(usize, usize),
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

impl ParsedTask {
    pub(crate) fn intenal_links(&self) -> HashSet<Id> {
        let mut out = HashSet::with_capacity(self.links.len());
        for link in &self.links {
            if let ParsedLink::Internal(id) = link {
                out.insert(*id);
            }
        }
        out
    }
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
            Some((char_pos, c)) => {
                out.push(c);
                let end = out.len() - 1;
                match (last, c, state_last) {
                    ('[', '[', _) => {
                        state.push(InternalLink(end, char_pos));
                    }
                    (']', ']', Some(InternalLink(il, s_pos))) => {
                        state.pop();
                        let contents = s.get(s_pos + 1..char_pos - 1)?;
                        if let Ok(id) = Id::from_str(contents) {
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
                    (last, '[', _) if is_boundary(last) => {
                        state.push(Linktext(end, char_pos));
                    }
                    (']', '(', Some(Linktext(_, _))) => {
                        state.push(Link(end, char_pos));
                    }
                    (')', c, Some(Link(_, _))) if is_boundary(c) => {
                        // TODO: this needs to be updated to use `s` instead of `out` for position
                        // parsing
                        let linkpos = if let Link(lp, _) = state.pop().unwrap() {
                            lp
                        } else {
                            // remove the linktext state, it is always present.
                            state.pop();
                            continue;
                        };
                        let linktextpos = if let Linktext(lt, _) = state.pop().unwrap() {
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
                    ('>', c, Some(RawLink(hl, s_pos)))
                        if is_boundary(c) && s_pos != char_pos - 1 =>
                    {
                        state.pop();
                        let link = s.get(s_pos + 1..char_pos - 1)?;
                        if let Ok(url) = Url::parse(link) {
                            let linktext =
                                format!("{}{}", link.blue(), super_num(links.len() + 1).purple());
                            links.push(ParsedLink::External(url));
                            out.replace_range(hl..end, &linktext);
                        }
                    }
                    (last, '<', _) if is_boundary(last) => {
                        state.push(RawLink(end, char_pos));
                    }
                    ('=', c, Some(Highlight(hl, s_pos)))
                        if is_boundary(c) && s_pos != char_pos - 1 =>
                    {
                        state.pop();
                        out.replace_range(
                            hl..end,
                            &s.get(s_pos + 1..char_pos - 1)?.reversed().to_string(),
                        );
                    }
                    (last, '=', _) if is_boundary(last) => {
                        state.push(Highlight(end, char_pos));
                    }
                    (last, '*', _) if is_boundary(last) => {
                        state.push(Italics(end, char_pos));
                    }
                    ('*', c, Some(Italics(il, s_pos)))
                        if is_boundary(c) && s_pos != char_pos - 1 =>
                    {
                        state.pop();
                        out.replace_range(
                            il..end,
                            &s.get(s_pos + 1..char_pos - 1)?.italic().to_string(),
                        );
                    }
                    (last, '!', _) if is_boundary(last) => {
                        state.push(Bold(end, char_pos));
                    }
                    ('!', c, Some(Bold(il, s_pos))) if is_boundary(c) && s_pos != char_pos - 1 => {
                        state.pop();
                        out.replace_range(
                            il..end,
                            &s.get(s_pos + 1..char_pos - 1)?.bold().to_string(),
                        );
                    }
                    (last, '_', _) if is_boundary(last) => {
                        state.push(Underline(end, char_pos));
                    }
                    ('_', c, Some(Underline(il, s_pos)))
                        if is_boundary(c) && s_pos != char_pos - 1 =>
                    {
                        state.pop();
                        out.replace_range(
                            il..end,
                            &s.get(s_pos + 1..char_pos - 1)?.underline().to_string(),
                        );
                    }
                    (last, '~', _) if is_boundary(last) => {
                        state.push(Strikethrough(end, char_pos));
                    }
                    ('~', c, Some(Strikethrough(il, s_pos)))
                        if is_boundary(c) && s_pos != char_pos - 1 =>
                    {
                        state.pop();
                        out.replace_range(
                            il..end,
                            &s.get(s_pos + 1..char_pos - 1)?.strikethrough().to_string(),
                        );
                    }
                    ('`', c, Some(InlineBlock(hl, s_pos)))
                        if is_boundary(c) && s_pos != char_pos - 1 =>
                    {
                        out.replace_range(
                            hl..end,
                            &s.get(s_pos + 1..char_pos - 1)?.green().to_string(),
                        );
                    }
                    (last, '`', _) if is_boundary(last) => {
                        state.push(InlineBlock(end, char_pos));
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

    fn setup() {
        colored::control::set_override(true);
    }

    #[test]
    fn test_highlight() {
        setup();
        let input = "hello =world=\n";
        let output = parse(input).expect("parse to work");
        assert_eq!("hello \u{1b}[7mworld\u{1b}[0m\n", output.content);
    }

    #[test]
    fn test_highlight_bad() {
        setup();
        let input = "hello =world\n";
        let output = parse(input).expect("parse to work");
        assert_eq!(input, output.content);
    }

    #[test]
    fn test_link() {
        setup();
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
        setup();
        let input = "hello [world](https://ngp.computer\n";
        let output = parse(input).expect("parse to work");
        assert!(output.links.is_empty());
        assert_eq!(input, output.content);
    }
    #[test]
    fn test_link_bad_no_start_link() {
        setup();
        let input = "hello [world]https://ngp.computer)\n";
        let output = parse(input).expect("parse to work");
        assert!(output.links.is_empty());
        assert_eq!(input, output.content);
    }
    #[test]
    fn test_link_bad_no_link() {
        setup();
        let input = "hello [world]\n";
        let output = parse(input).expect("parse to work");
        assert!(output.links.is_empty());
        assert_eq!(input, output.content);
    }

    #[test]
    fn test_internal_link_good() {
        setup();
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
        setup();
        let input = "hello [[tsk-123";
        let output = parse(input).expect("parse to work");
        assert!(output.links.is_empty());
        assert_eq!(input, output.content);
    }

    #[test]
    fn test_italics() {
        setup();
        let input = "hello *world*\n";
        let output = parse(input).expect("parse to work");
        assert_eq!("hello \u{1b}[3mworld\u{1b}[0m\n", output.content);
    }

    #[test]
    fn test_italics_bad() {
        setup();
        let input = "hello *world";
        let output = parse(input).expect("parse to work");
        assert_eq!(input, output.content);
    }

    #[test]
    fn test_bold() {
        setup();
        let input = "hello !world!\n";
        let output = parse(input).expect("parse to work");
        assert_eq!("hello \u{1b}[1mworld\u{1b}[0m\n", output.content);
    }

    #[test]
    fn test_bold_bad() {
        setup();
        let input = "hello !world\n";
        let output = parse(input).expect("parse to work");
        assert_eq!(input, output.content);
    }

    #[test]
    fn test_underline() {
        setup();
        let input = "hello _world_\n";
        let output = parse(input).expect("parse to work");
        assert_eq!("hello \u{1b}[4mworld\u{1b}[0m\n", output.content);
    }

    #[test]
    fn test_underline_bad() {
        setup();
        let input = "hello _world\n";
        let output = parse(input).expect("parse to work");
        assert_eq!(input, output.content);
    }

    #[test]
    fn test_strikethrough() {
        setup();
        let input = "hello ~world~\n";
        let output = parse(input).expect("parse to work");
        assert_eq!("hello \u{1b}[9mworld\u{1b}[0m\n", output.content);
    }

    #[test]
    fn test_strikethrough_bad() {
        setup();
        let input = "hello ~world\n";
        let output = parse(input).expect("parse to work");
        assert_eq!(input, output.content);
    }

    #[test]
    fn test_inlineblock() {
        setup();
        let input = "hello `world`\n";
        let output = parse(input).expect("parse to work");
        assert_eq!("hello \u{1b}[32mworld\u{1b}[0m\n", output.content);
    }

    #[test]
    fn test_inlineblock_bad() {
        setup();
        let input = "hello `world\n";
        let output = parse(input).expect("parse to work");
        assert_eq!(input, output.content);
    }

    #[test]
    fn test_multiple_styles() {
        setup();
        let input = "hello *italic* ~strikethrough~ !bold!\n";
        let output = parse(input).expect("parse to work");
        assert_eq!(
            "hello \u{1b}[3mitalic\u{1b}[0m \u{1b}[9mstrikethrough\u{1b}[0m \u{1b}[1mbold\u{1b}[0m\n",
            output.content
        );
    }
}
