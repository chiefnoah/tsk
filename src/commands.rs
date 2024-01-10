#![allow(dead_code)]
use crate::types::{Tag, Task, TaskId, TaskStatus};

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
