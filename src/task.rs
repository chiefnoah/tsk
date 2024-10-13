#![allow(dead_code)]
use url::Url;

use crate::workspace::Id;

/// An AST node parsed from the plain-text representation of
enum TaskASTNode<'n> {
    /// Unmodified text
    Plain(&'n str),
    /// Text that has been highlighted using =test= syntax.
    Highlight(Box<TaskASTNode<'n>>),
    /// A standard Markdown-style link
    Link {
        text: Box<TaskASTNode<'n>>,
        to: Url,
    },
    /// An internal link to another task using custom [[tsk-id]] syntax
    InternalLink(Id),
    /// Italicized text using Markdown *text* syntax.
    Italics(Box<TaskASTNode<'n>>),
    /// Bolded text using !text! syntax.
    Bold(Box<TaskASTNode<'n>>),
    /// Underlined text using custom _text_ syntax.
    Underline(Box<TaskASTNode<'n>>),
    /// Strikethrough using -text- syntax
    Strikethrough(Box<TaskASTNode<'n>>),
    /// Unordered list using Markdown * list-item syntax
    UnorderedList(Vec<TaskASTNode<'n>>),
    /// Ordered list using Markdown 1. list-item syntax
    OrderedList(Vec<TaskASTNode<'n>>),
    /// Literal block using markdown triple-backtick syntax.
    Block {
        /// An optional syntax specifier. This *may* be used to apply syntax formatting to contents
        /// in the future
        syntax: Option<&'n str>,
        /// The verbatim content of the block
        content: &'n str,
    },
    /// Literal block using markdown single-backtick syntax.
    InlineBlock(&'n str),
    /// Blockquotes using Markdown > quote syntax
    Blockquote(&'n str),
}

impl<'i> TaskASTNode<'i> {
    fn parse(s: &'i String) -> Result<Vec<TaskASTNode<'i>>, String> {
        let i = 0;
        let mut roots: Vec<TaskASTNode<'i>> = Vec::new();

        todo!();
    }
}
