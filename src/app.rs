#![allow(dead_code)]
use chrono::{DateTime, Utc};
use sqlite::Connection;

struct Task {
    created: DateTime<Utc>,
    resolved: Option<DateTime<Utc>>,
    title: String,
    body: Option<String>,
}

enum View {
    TaskList(Vec<Task>),
    TaskDetails(Task),
    History, // TODO...
}

struct App {
    view: View,
    task_manager: TaskManager,
}

struct TaskManager {
    connection: Connection,
}
