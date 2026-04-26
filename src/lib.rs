pub mod errors;
mod fzf;
mod namespace;
mod object;
mod properties;
mod queue;
mod task;
mod util;
mod workspace;

use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use edit::edit as open_editor;
use errors::Result;
use std::env::current_dir;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::exit;
use std::str::FromStr as _;
use workspace::{Id, Task, TaskIdentifier, Workspace};

fn default_dir() -> Result<PathBuf> {
    Ok(current_dir()?)
}

fn parse_id(s: &str) -> std::result::Result<Id, &'static str> {
    Id::from_str(s).map_err(|_| "Unable to parse tsk- ID")
}

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Override the tsk root directory.
    #[arg(short = 'C', env = "TSK_ROOT", value_name = "DIR")]
    dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a `.tsk/` marker in the current git repo. (Auto-created on first use.)
    Init,
    /// Create a new task and push it onto the active queue.
    Push {
        #[arg(short = 'e', default_value_t = false)]
        edit: bool,
        #[arg(short = 'b')]
        body: Option<String>,
        #[command(flatten)]
        title: Title,
    },
    /// Create a new task and append it to the bottom of the active queue.
    Append {
        #[arg(short = 'e', default_value_t = false)]
        edit: bool,
        #[arg(short = 'b')]
        body: Option<String>,
        #[command(flatten)]
        title: Title,
    },
    /// Print the active queue's stack (top-of-stack first).
    List {
        #[arg(short = 'a', default_value_t = false)]
        all: bool,
        #[arg(short = 'c', default_value_t = 10)]
        count: usize,
        #[arg(short = 'q', default_value_t = false)]
        ids_only: bool,
    },
    /// Show a task by id.
    Show {
        #[arg(short = 'x', default_value_t = false)]
        show_attrs: bool,
        #[command(flatten)]
        task_id: TaskId,
    },
    /// Open `$EDITOR` to modify a task.
    Edit {
        #[command(flatten)]
        task_id: TaskId,
    },
    /// Drop a task (remove from queue + unbind human id, history retained).
    Drop {
        #[command(flatten)]
        task_id: TaskId,
    },
    /// Swap the top two tasks.
    Swap,
    /// Rotate top 3: third → top.
    Rot,
    /// Reverse-rotate top 3: top → third.
    Tor,
    /// Move a task to the top of the stack.
    Prioritize {
        #[command(flatten)]
        task_id: TaskId,
    },
    /// Move a task to the bottom of the stack.
    Deprioritize {
        #[command(flatten)]
        task_id: TaskId,
    },
    /// Drop index entries whose stable ids no longer resolve.
    Clean,
    /// Print refspec/setup hints for `git push`/`git fetch` to include `refs/tsk/*`.
    GitSetup {
        /// Configure push/fetch refspecs on the named remote (default: origin).
        #[arg(short = 'r')]
        remote: Option<String>,
    },
    /// Push tsk refs to a git remote (default: origin).
    GitPush {
        remote: Option<String>,
    },
    /// Fetch tsk refs from a git remote (default: origin).
    GitPull {
        remote: Option<String>,
    },
    /// Share a task into another namespace (binds same stable id under that namespace's next human id).
    Share {
        target: String,
        #[command(flatten)]
        task_id: TaskId,
    },
    /// Move a task from the active queue's index into another queue's inbox.
    Assign {
        target: String,
        #[command(flatten)]
        task_id: TaskId,
        /// Auto-push refs to this remote after assigning. Empty string skips. Default: origin.
        #[arg(short = 'R')]
        remote: Option<String>,
    },
    /// Pull a task from another queue's index (only allowed if its can-pull is true).
    Pull {
        source: String,
        #[command(flatten)]
        task_id: TaskId,
    },
    /// List inbox items pending in the active queue.
    Inbox {
        /// Auto-pull from this remote first. Empty string skips. Default: origin.
        #[arg(short = 'R')]
        remote: Option<String>,
    },
    /// Accept an inbox item by key (no key = first item).
    Accept { key: Option<String> },
    /// Reject an inbox item by key (no key = first item).
    Reject {
        key: Option<String>,
        /// Auto-push refs to this remote after rejecting. Empty string skips. Default: origin.
        #[arg(short = 'R')]
        remote: Option<String>,
    },
    /// Get/set/find tasks by property. Properties are zero-or-more text values
    /// stored as files in the task's tree object; each value is one line.
    Prop {
        #[command(subcommand)]
        action: PropAction,
    },
    /// Manage namespaces.
    Namespace {
        #[command(subcommand)]
        action: NamespaceAction,
    },
    /// Manage queues.
    Queue {
        #[command(subcommand)]
        action: QueueAction,
    },
    /// Switch active namespace (shorthand). With no name, fzf-picks from
    /// existing namespaces (plus a `<new>` sentinel for creating one on
    /// the fly).
    Switch { name: Option<String> },
    /// Generate shell completion.
    Completion {
        #[arg(short = 's')]
        shell: Shell,
    },
}

#[derive(Subcommand)]
enum PropAction {
    /// List all values for every property on a task.
    List {
        #[command(flatten)]
        task_id: TaskId,
    },
    /// Append a value to a property on a task. Creates the property if absent.
    Add {
        #[command(flatten)]
        task_id: TaskId,
        key: String,
        value: String,
    },
    /// Replace the entire value list for a property. With no values, removes the property.
    Set {
        #[command(flatten)]
        task_id: TaskId,
        key: String,
        values: Vec<String>,
    },
    /// Remove a single value (or, with no value, the entire property).
    Unset {
        #[command(flatten)]
        task_id: TaskId,
        key: String,
        value: Option<String>,
    },
    /// List every property key currently in use across the workspace.
    Keys,
    /// List distinct values seen for a property key.
    Values { key: String },
    /// Find every task in the active namespace whose `key` is set (and equals
    /// `value`, if supplied). With both omitted, fzf-picks the key, then value.
    Find {
        key: Option<String>,
        value: Option<String>,
    },
}

#[derive(Subcommand)]
enum NamespaceAction {
    List,
    Current,
    /// Switch active namespace. With no name, fzf-picks from existing
    /// namespaces (plus a `<new>` sentinel for creating one on the fly).
    Switch { name: Option<String> },
}

#[derive(Subcommand)]
enum QueueAction {
    List,
    Current,
    /// Create a new queue. By default `can-pull=false`; use `-p` to make it true.
    Create {
        name: String,
        #[arg(short = 'p', default_value_t = false)]
        can_pull: bool,
    },
    Switch { name: String },
}

#[derive(Args)]
#[group(required = true, multiple = false)]
struct Title {
    #[arg(short, value_name = "TITLE")]
    title: Option<String>,
    #[arg(value_name = "TITLE")]
    title_simple: Option<Vec<String>>,
}

#[derive(Args)]
#[group(required = false, multiple = false)]
struct TaskId {
    #[arg(short = 't', value_name = "ID")]
    id: Option<u32>,
    #[arg(short = 'T', value_name = "TSK-ID", value_parser = parse_id)]
    tsk_id: Option<Id>,
    #[arg(short = 'r', value_name = "RELATIVE", default_value_t = 0)]
    relative_id: u32,
}

impl From<TaskId> for TaskIdentifier {
    fn from(v: TaskId) -> Self {
        if let Some(id) = v.id.map(Id::from).or(v.tsk_id) {
            TaskIdentifier::Id(id)
        } else {
            TaskIdentifier::Relative(v.relative_id)
        }
    }
}

fn effective_remote(supplied: Option<String>) -> Option<String> {
    supplied
        .map(|s| if s.is_empty() { None } else { Some(s) })
        .unwrap_or_else(|| Some("origin".to_string()))
}

fn dispatch(cli: Cli) -> Result<()> {
    let dir = match cli.dir {
        Some(d) => d,
        None => default_dir()?,
    };
    match cli.command {
        Commands::Init => Workspace::init(dir),
        Commands::Push { edit, body, title } => command_push(dir, edit, body, title, true),
        Commands::Append { edit, body, title } => command_push(dir, edit, body, title, false),
        Commands::List {
            all,
            count,
            ids_only,
        } => command_list(dir, all, count, ids_only),
        Commands::Show {
            task_id,
            show_attrs,
        } => command_show(dir, task_id, show_attrs),
        Commands::Edit { task_id } => command_edit(dir, task_id),
        Commands::Drop { task_id } => command_drop(dir, task_id),
        Commands::Swap => Workspace::from_path(dir)?.swap_top(),
        Commands::Rot => Workspace::from_path(dir)?.rot(),
        Commands::Tor => Workspace::from_path(dir)?.tor(),
        Commands::Prioritize { task_id } => {
            Workspace::from_path(dir)?.prioritize(task_id.into())
        }
        Commands::Deprioritize { task_id } => {
            Workspace::from_path(dir)?.deprioritize(task_id.into())
        }
        Commands::Clean => Workspace::from_path(dir)?.clean(),
        Commands::GitSetup { remote } => {
            let r = remote.unwrap_or_else(|| "origin".to_string());
            Workspace::from_path(dir)?.configure_git_remote_refspecs(&r)
        }
        Commands::GitPush { remote } => {
            let r = remote.unwrap_or_else(|| "origin".to_string());
            Workspace::from_path(dir)?.git_push(&r)
        }
        Commands::GitPull { remote } => {
            let r = remote.unwrap_or_else(|| "origin".to_string());
            Workspace::from_path(dir)?.git_pull(&r)
        }
        Commands::Share { target, task_id } => command_share(dir, target, task_id),
        Commands::Assign {
            target,
            task_id,
            remote,
        } => command_assign(dir, target, task_id, remote),
        Commands::Pull { source, task_id } => command_pull(dir, source, task_id),
        Commands::Inbox { remote } => command_inbox(dir, remote),
        Commands::Accept { key } => command_accept(dir, key),
        Commands::Reject { key, remote } => command_reject(dir, key, remote),
        Commands::Prop { action } => command_prop(dir, action),
        Commands::Namespace { action } => command_namespace(dir, action),
        Commands::Queue { action } => command_queue(dir, action),
        Commands::Switch { name } => command_namespace_switch(dir, name),
        Commands::Completion { shell } => {
            generate(shell, &mut Cli::command(), "tsk", &mut io::stdout());
            Ok(())
        }
    }
}

/// Parse the CLI from `std::env::args()` and execute. Returns the process
/// exit code so callers (the `tsk` and `git-tsk` bins) can hand it to
/// `std::process::exit`.
pub fn run() -> i32 {
    match dispatch(Cli::parse()) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            2
        }
    }
}

fn read_title_and_body(
    edit: bool,
    body: Option<String>,
    title_arg: Title,
) -> Result<(String, String)> {
    let mut title = if let Some(t) = title_arg.title {
        t
    } else if let Some(ts) = title_arg.title_simple {
        ts.join(" ")
    } else {
        String::new()
    };
    let mut body = if body.is_none() {
        if let Some((first, rest)) = title.split_once('\n') {
            let extracted = rest.to_string();
            title = first.to_string();
            extracted
        } else {
            String::new()
        }
    } else {
        title = title.replace(['\n', '\r'], " ");
        body.unwrap_or_default()
    };
    if body == "-" {
        body.clear();
        io::stdin().read_to_string(&mut body)?;
    }
    if edit {
        let new_content = open_editor(format!("{title}\n\n{body}"))?;
        if let Some((t, b)) = new_content.split_once('\n') {
            title = t.to_string();
            body = b.trim_start_matches('\n').to_string();
        }
    }
    title = title.replace(['\n', '\r'], " ");
    Ok((title, body))
}

fn command_push(
    dir: PathBuf,
    edit: bool,
    body: Option<String>,
    title: Title,
    on_top: bool,
) -> Result<()> {
    let (title, body) = read_title_and_body(edit, body, title)?;
    let ws = Workspace::from_path(dir)?;
    let task = ws.new_task(title, body)?;
    if on_top {
        ws.push_task(task)
    } else {
        ws.append_task(task)
    }
}

fn command_list(dir: PathBuf, all: bool, count: usize, ids_only: bool) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let stack = ws.read_stack()?;
    if stack.is_empty() {
        println!("*No tasks*");
        return Ok(());
    }
    for (i, entry) in stack.iter().enumerate() {
        if !all && i >= count {
            break;
        }
        if ids_only {
            println!("{}", entry.id);
        } else {
            println!("{}\t{}", entry.id, entry.title);
        }
    }
    Ok(())
}

fn command_show(dir: PathBuf, task_id: TaskId, show_attrs: bool) -> Result<()> {
    let task = Workspace::from_path(dir)?.task(task_id.into())?;
    if show_attrs && !task.attributes.is_empty() {
        println!("---");
        for (k, vs) in &task.attributes {
            for v in vs {
                println!("{k}: \"{v}\"");
            }
        }
        println!("---");
    }
    println!("{task}");
    Ok(())
}

fn command_edit(dir: PathBuf, task_id: TaskId) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let mut task = ws.task(task_id.into())?;
    let new_content = open_editor(format!("{}\n\n{}", task.title.trim(), task.body.trim()))?;
    if let Some((t, b)) = new_content.split_once('\n') {
        task.title = t.replace(['\n', '\r'], " ");
        task.body = b.trim_start_matches('\n').to_string();
        ws.save_task(&task)?;
    }
    Ok(())
}

fn command_drop(dir: PathBuf, task_id: TaskId) -> Result<()> {
    if let Some(id) = Workspace::from_path(dir)?.drop(task_id.into())? {
        println!("Dropped {id}");
        Ok(())
    } else {
        eprintln!("No task to drop.");
        exit(1);
    }
}

fn command_share(dir: PathBuf, target: String, task_id: TaskId) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let h = ws.share(task_id.into(), &target)?;
    println!("Shared as {target}/tsk-{h}");
    Ok(())
}

fn command_assign(
    dir: PathBuf,
    target: String,
    task_id: TaskId,
    remote: Option<String>,
) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let key = ws.assign_to_queue(task_id.into(), &target)?;
    println!("Assigned to {target} as {key}");
    if let Some(r) = effective_remote(remote) {
        let _ = ws.git_push(&r);
    }
    Ok(())
}

fn command_pull(dir: PathBuf, source: String, task_id: TaskId) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    // For pull, the task id is interpreted in the source queue's namespace
    // mapping context. Simplification: require the caller to use -T <stable>
    // form via human id in active namespace. For v1 we just resolve in
    // active namespace; sharing first lets the user reference foreign tasks.
    let id = ws.pull_from_queue(&source, task_id.into())?;
    println!("Pulled {id}");
    Ok(())
}

fn command_inbox(dir: PathBuf, remote: Option<String>) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    if let Some(r) = effective_remote(remote) {
        let _ = ws.git_pull(&r);
    }
    let inbox = ws.list_inbox()?;
    if inbox.is_empty() {
        println!("*Empty*");
        return Ok(());
    }
    for item in inbox {
        println!("{}\tfrom {}\t{}", item.key, item.source_queue, item.title);
    }
    Ok(())
}

fn command_accept(dir: PathBuf, key: Option<String>) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let key = match key {
        Some(k) => k,
        None => {
            ws.list_inbox()?
                .into_iter()
                .next()
                .ok_or_else(|| errors::Error::Parse("Inbox is empty".into()))?
                .key
        }
    };
    let id = ws.accept_inbox(&key)?;
    println!("Accepted as {id}");
    Ok(())
}

fn command_reject(dir: PathBuf, key: Option<String>, remote: Option<String>) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let key = match key {
        Some(k) => k,
        None => {
            ws.list_inbox()?
                .into_iter()
                .next()
                .ok_or_else(|| errors::Error::Parse("Inbox is empty".into()))?
                .key
        }
    };
    ws.reject_inbox(&key)?;
    println!("Rejected {key}");
    if let Some(r) = effective_remote(remote) {
        let _ = ws.git_push(&r);
    }
    Ok(())
}

fn command_prop(dir: PathBuf, action: PropAction) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    match action {
        PropAction::List { task_id } => {
            let task = ws.task(task_id.into())?;
            for (k, vs) in &task.attributes {
                for v in vs {
                    println!("{k}\t{v}");
                }
            }
        }
        PropAction::Add {
            task_id,
            key,
            value,
        } => ws.add_property_value(task_id.into(), &key, &value)?,
        PropAction::Set {
            task_id,
            key,
            values,
        } => ws.set_property(task_id.into(), &key, values)?,
        PropAction::Unset {
            task_id,
            key,
            value,
        } => ws.unset_property(task_id.into(), &key, value.as_deref())?,
        PropAction::Keys => {
            for k in ws.property_keys()? {
                println!("{k}");
            }
        }
        PropAction::Values { key } => {
            for v in ws.property_values(&key)? {
                println!("{v}");
            }
        }
        PropAction::Find { key, value } => {
            let key = match key {
                Some(k) => k,
                None => fzf::select::<_, String, _>(
                    ws.property_keys()?,
                    ["--prompt=key> "],
                )?
                .ok_or_else(|| errors::Error::Parse("No key selected".into()))?,
            };
            let value = match value {
                Some(v) if v == "<any>" => None,
                Some(v) => Some(v),
                None => {
                    let mut choices = ws.property_values(&key)?;
                    choices.insert(0, "<any>".to_string());
                    let picked = fzf::select::<_, String, _>(
                        choices,
                        ["--prompt=value> "],
                    )?
                    .ok_or_else(|| errors::Error::Parse("No value selected".into()))?;
                    if picked == "<any>" {
                        None
                    } else {
                        Some(picked)
                    }
                }
            };
            for (id, _stable, title) in ws.find_by_property(&key, value.as_deref())? {
                println!("{id}\t{title}");
            }
        }
    }
    Ok(())
}

fn command_namespace(dir: PathBuf, action: NamespaceAction) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    match action {
        NamespaceAction::List => {
            for n in ws.list_namespaces()? {
                println!("{n}");
            }
        }
        NamespaceAction::Current => println!("{}", ws.namespace()),
        NamespaceAction::Switch { name } => return resolve_and_switch_namespace(&ws, name),
    }
    Ok(())
}

fn command_queue(dir: PathBuf, action: QueueAction) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    match action {
        QueueAction::List => {
            for n in ws.list_queues()? {
                println!("{n}");
            }
        }
        QueueAction::Current => println!("{}", ws.queue()),
        QueueAction::Create { name, can_pull } => {
            ws.create_queue(&name, Some(can_pull))?;
            println!("Created queue '{name}' (can-pull={can_pull})");
        }
        QueueAction::Switch { name } => ws.switch_queue(&name)?,
    }
    Ok(())
}

const NEW_NS_SENTINEL: &str = "<new>";

fn command_namespace_switch(dir: PathBuf, name: Option<String>) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    resolve_and_switch_namespace(&ws, name)
}

fn resolve_and_switch_namespace(ws: &Workspace, name: Option<String>) -> Result<()> {
    let target = match name {
        Some(n) => n,
        None => pick_namespace(ws)?,
    };
    ws.switch_namespace(&target)?;
    println!("Switched to namespace '{target}'");
    Ok(())
}

fn pick_namespace(ws: &Workspace) -> Result<String> {
    let cur = ws.namespace();
    let existing = ws.list_namespaces()?;
    let entries = namespace_picker_entries(&existing, &cur);
    let picked = fzf::select::<_, String, _>(entries, ["--prompt=namespace> "])?
        .ok_or_else(|| errors::Error::Parse("No namespace selected".into()))?;
    let picked = strip_picker_marker(&picked);
    if picked == NEW_NS_SENTINEL {
        let name = prompt_line("New namespace name: ")?;
        if name.is_empty() {
            return Err(errors::Error::Parse("Empty namespace name".into()));
        }
        Ok(name)
    } else {
        Ok(picked.to_string())
    }
}

/// Build the fzf input lines for namespace selection: every existing
/// namespace (active marked with `* `, others with `  `) plus a trailing
/// `<new>` sentinel for creating one on the fly. The active namespace is
/// always present even when no refs have been written yet.
fn namespace_picker_entries(existing: &[String], current: &str) -> Vec<String> {
    let mut entries: Vec<String> = existing
        .iter()
        .map(|n| {
            if n == current {
                format!("* {n}")
            } else {
                format!("  {n}")
            }
        })
        .collect();
    if !existing.iter().any(|n| n == current) {
        entries.insert(0, format!("* {current}"));
    }
    entries.push(NEW_NS_SENTINEL.to_string());
    entries
}

fn strip_picker_marker(s: &str) -> &str {
    s.strip_prefix("* ").or_else(|| s.strip_prefix("  ")).unwrap_or(s)
}

fn prompt_line(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    io::stderr().flush()?;
    let mut s = String::new();
    io::stdin().read_line(&mut s)?;
    Ok(s.trim_end_matches(['\n', '\r']).to_string())
}

#[allow(dead_code)]
fn _silence_unused(_w: &dyn Write, _t: Task) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_marks_current_and_appends_sentinel() {
        let entries = namespace_picker_entries(
            &["alpha".to_string(), "tsk".to_string()],
            "tsk",
        );
        assert_eq!(entries, vec!["  alpha", "* tsk", "<new>"]);
    }

    #[test]
    fn picker_includes_current_when_missing_from_list() {
        let entries = namespace_picker_entries(&[], "tsk");
        assert_eq!(entries, vec!["* tsk", "<new>"]);
    }

    #[test]
    fn strip_marker_handles_all_prefixes() {
        assert_eq!(strip_picker_marker("* tsk"), "tsk");
        assert_eq!(strip_picker_marker("  alpha"), "alpha");
        assert_eq!(strip_picker_marker("<new>"), "<new>");
    }
}
