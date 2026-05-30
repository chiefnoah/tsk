mod commands;
pub mod errors;
mod fzf;
mod merge;
mod namespace;
mod object;
mod patch;
mod properties;
mod propvalue;
mod queue;
mod references;
mod serv;
mod task;
mod workspace;

use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use errors::Result;
use std::env::current_dir;
use std::io::{self, Write};
use std::path::PathBuf;
use std::str::FromStr as _;
use workspace::{Id, TaskIdentifier, Workspace};

fn default_dir() -> Result<PathBuf> {
    Ok(current_dir()?)
}

pub(crate) fn parse_id(s: &str) -> std::result::Result<Id, &'static str> {
    Id::from_str(s).map_err(|_| "Unable to parse tsk- ID")
}

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Override the tsk root directory.
    #[arg(short = 'C', env = "TSK_ROOT", value_name = "DIR")]
    dir: Option<PathBuf>,
    /// Override the active queue for this invocation only. Affects every
    /// command that reads/writes the active queue (push, drop, swap,
    /// rot/tor, prioritize/deprioritize, list, inbox, assign, accept,
    /// reject, export, ...).
    #[arg(short = 'q', long = "queue", value_name = "QUEUE", global = true)]
    queue: Option<String>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Bootstrap user-local state in `<git-dir>/tsk/`. (Auto-created on first use.)
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
        #[arg(short = 'i', default_value_t = false)]
        ids_only: bool,
    },
    /// Fuzzy-find tasks and print the selected task id(s).
    Find {
        /// Allow selecting multiple tasks.
        #[arg(short, long, default_value_t = false)]
        multi: bool,
        /// Search every task in the active namespace instead of only the active queue.
        #[arg(short, long, default_value_t = false)]
        all: bool,
        /// Include task bodies in the search text.
        #[arg(short, long, default_value_t = false)]
        body: bool,
    },
    /// Show a task by id.
    Show {
        /// Print xattr-style YAML front-matter for the task's properties.
        #[arg(short = 'x', default_value_t = false)]
        show_attrs: bool,
        /// Skip the rich-text parser and print the raw bytes verbatim.
        #[arg(short = 'R', default_value_t = false)]
        raw: bool,
        #[command(flatten)]
        task_id: TaskId,
    },
    /// List or follow a link parsed from a task's body.
    Follow {
        /// The task whose body will be searched for links.
        #[command(flatten)]
        task_id: TaskId,
        /// The index of the link to open. Omit with no -s to just list links.
        #[arg(short = 'l')]
        link_index: Option<usize>,
        /// fzf-pick a link to open instead of supplying -l.
        #[arg(short = 's', default_value_t = false)]
        select: bool,
        /// When opening an internal link, edit the addressed task instead of showing.
        #[arg(short = 'e', default_value_t = false)]
        edit: bool,
    },
    /// Open `$EDITOR` to modify a task.
    Edit {
        #[command(flatten)]
        task_id: TaskId,
    },
    /// Drop a task (remove from queue + mark done, history retained).
    Drop {
        /// Record the enclosing git repo's current HEAD commit in `closed-on`.
        #[arg(
            short = 'x',
            long = "closed-on-commit",
            alias = "closed-on-head",
            default_value_t = false
        )]
        closed_on_commit: bool,
        #[command(flatten)]
        task_id: TaskId,
    },
    /// Flip a `done` task back to `open` and push it onto the active queue.
    Reopen {
        /// Include task bodies in the interactive search text.
        #[arg(short, long, default_value_t = false)]
        body: bool,
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
    /// Run every known one-shot migration against the active workspace.
    /// Currently: backfill `status=open` on tasks without a status property.
    /// New migrations land here as they're added.
    FixUp,
    /// Export one or more tasks as a concatenated mbox-format patch series.
    /// With no -T / --where / --all, drops into fzf for a single-task pick.
    /// Pipe to a file for offline transfer; recipient runs `tsk import`.
    Export {
        /// Specific task by tsk-id. Repeatable for multi-task export.
        #[arg(short = 'T', value_parser = parse_id)]
        ids: Vec<Id>,
        /// Property filter: `--where status=open`. Combines with -T flags.
        #[arg(long, value_name = "KEY=VALUE")]
        r#where: Option<String>,
        /// Export every task bound in the active namespace.
        #[arg(long)]
        all: bool,
        /// Embed each task's namespace+human-id in its root entry so the
        /// recipient can opt in to mirroring the bindings on import.
        #[arg(long)]
        bind: bool,
    },
    /// Import a task from an mbox-format patch series (read from stdin).
    /// Verifies stable id; rejects tampered patches.
    Import {
        /// Bind the imported task into the active namespace, allocating a
        /// fresh human id (or reusing an existing binding to the same stable id).
        #[arg(long)]
        bind: bool,
    },
    /// Print the commit history of a tsk ref. Newest commit first.
    Log {
        #[command(subcommand)]
        target: LogTarget,
    },
    /// Print refspec/setup hints for `git push`/`git fetch` to include `refs/tsk/*`.
    GitSetup {
        /// Configure push/fetch refspecs on the named remote (default: origin).
        #[arg(short = 'r')]
        remote: Option<String>,
    },
    /// Push tsk refs to a git remote (default: origin).
    GitPush { remote: Option<String> },
    /// Fetch tsk refs from a git remote (default: origin) and reconcile
    /// divergent task histories. Default strategy is merge; pass --rebase
    /// to replay local-only commits onto the remote tip instead.
    GitPull {
        remote: Option<String>,
        #[arg(long)]
        rebase: bool,
    },
    /// Share a task into another namespace (binds same stable id under that namespace's next human id).
    Share {
        target: String,
        #[command(flatten)]
        task_id: TaskId,
    },
    /// Move a task from the active queue's index into another queue's inbox.
    Assign {
        /// Target queue. Omit to fzf-pick from existing queues.
        target: Option<String>,
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
    Accept {
        key: Option<String>,
        /// Auto-push refs to this remote after accepting. Empty string skips. Default: origin.
        #[arg(short = 'R')]
        remote: Option<String>,
    },
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
    /// Manage git remotes that carry tsk refs.
    Remote {
        #[command(subcommand)]
        action: RemoteAction,
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
    /// Print coding-agent usage guidance for tsk.
    Skill,
    /// Host a read-only HTTP browser for queues, namespaces, and tasks.
    Serv {
        #[command(flatten)]
        args: serv::ServeArgs,
    },
}

#[derive(Subcommand)]
pub(crate) enum LogTarget {
    /// Edit history of a single task.
    Task {
        #[command(flatten)]
        task_id: TaskId,
    },
    /// Edit history of a namespace tree (id assignments, drops, shares).
    /// Defaults to the active namespace.
    Namespace { name: Option<String> },
    /// Edit history of a queue tree (pushes, drops, inbox moves).
    /// Defaults to the active queue.
    Queue { name: Option<String> },
}

#[derive(Subcommand)]
pub(crate) enum PropAction {
    /// List properties set on a task.
    List {
        #[command(flatten)]
        task_id: TaskId,
    },
    /// Print newline-delimited values for one property on a task.
    Get {
        #[command(flatten)]
        task_id: TaskId,
        key: String,
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
    /// List property keys set on a task.
    Keys {
        #[command(flatten)]
        task_id: TaskId,
    },
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
pub(crate) enum NamespaceAction {
    List,
    Current,
    /// Switch active namespace. With no name, fzf-picks from existing
    /// namespaces (plus a `<new>` sentinel for creating one on the fly).
    Switch {
        name: Option<String>,
    },
    /// List every task bound in a namespace (defaults to active),
    /// regardless of which queue (if any) it's on. One row per id.
    Tasks {
        name: Option<String>,
    },
    /// List unique property keys on tasks in a namespace (defaults to active).
    Props {
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum QueueAction {
    List,
    Current,
    /// Create a new queue. By default `can-pull=false`; use `-p` to make it true.
    Create {
        name: String,
        #[arg(short = 'p', default_value_t = false)]
        can_pull: bool,
    },
    /// Delete a queue. Refuses the default queue. Use -R to also delete it remotely.
    Delete {
        name: String,
        /// Also push the delete to this remote. Empty string skips.
        #[arg(short = 'R')]
        remote: Option<String>,
    },
    /// Switch active queue. With no name, fzf-picks from existing queues
    /// (plus a `<new>` sentinel for creating one on the fly).
    Switch {
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum RemoteAction {
    /// Print the active default remote (the one used when no `-R` is given).
    Default,
    /// Persist the active default remote for this clone. Must already be
    /// a git remote — use `git remote add ...` (and `tsk git-setup -r
    /// <name>` to configure refspecs) first.
    SetDefault { name: String },
}

#[derive(Args)]
#[group(required = true, multiple = false)]
pub(crate) struct Title {
    #[arg(short, value_name = "TITLE")]
    pub(crate) title: Option<String>,
    #[arg(value_name = "TITLE")]
    pub(crate) title_simple: Option<Vec<String>>,
}

#[derive(Args, Default)]
#[group(required = false, multiple = false)]
pub(crate) struct TaskId {
    #[arg(short = 't', value_name = "ID")]
    pub(crate) id: Option<u32>,
    #[arg(short = 'T', value_name = "TSK-ID", value_parser = parse_id)]
    pub(crate) tsk_id: Option<Id>,
    #[arg(short = 'r', value_name = "RELATIVE")]
    pub(crate) relative_id: Option<u32>,
}

impl TaskId {
    /// True when the user passed none of `-t`, `-T`, or `-r`. Commands
    /// that fall back to a fuzzy finder use this to decide whether to
    /// prompt; commands that prefer "top of stack" silently treat this
    /// as `Relative(0)` via the `From` impl.
    pub(crate) fn is_empty(&self) -> bool {
        self.id.is_none() && self.tsk_id.is_none() && self.relative_id.is_none()
    }

    /// Resolve to a `TaskIdentifier`, dropping into an fzf picker when no
    /// flag was supplied. Use when interactive selection is the desired
    /// fallback (e.g. `tsk export`); otherwise prefer `Into`, which
    /// silently picks the top of the stack.
    pub(crate) fn resolve_or_pick(self, ws: &Workspace) -> Result<TaskIdentifier> {
        if !self.is_empty() {
            return Ok(self.into());
        }
        let entries = ws.list_namespace_tasks(&ws.namespace()?)?;
        if entries.is_empty() {
            return Err(errors::Error::NoTasks);
        }
        let lines: Vec<String> = entries
            .iter()
            .map(|e| format!("{}\t{}", e.id, e.title))
            .collect();
        let picked: Option<String> = fzf::select(lines, ["--prompt=task> "])?;
        let picked = picked.ok_or(errors::Error::NoTasks)?;
        let id_str = picked.split('\t').next().unwrap_or("");
        let id: Id = parse_id(id_str).map_err(|e| errors::Error::Parse(e.to_string()))?;
        Ok(TaskIdentifier::Id(id))
    }
}

impl From<TaskId> for TaskIdentifier {
    fn from(v: TaskId) -> Self {
        if let Some(id) = v.id.map(Id::from).or(v.tsk_id) {
            TaskIdentifier::Id(id)
        } else {
            TaskIdentifier::Relative(v.relative_id.unwrap_or(0))
        }
    }
}

fn dispatch(cli: Cli) -> Result<()> {
    workspace::set_queue_override(cli.queue);
    let dir = match cli.dir {
        Some(d) => d,
        None => default_dir()?,
    };
    match cli.command {
        Commands::Init => Workspace::init(dir),
        Commands::Push { edit, body, title } => {
            commands::command_push(dir, edit, body, title, true)
        }
        Commands::Append { edit, body, title } => {
            commands::command_push(dir, edit, body, title, false)
        }
        Commands::List {
            all,
            count,
            ids_only,
        } => commands::command_list(dir, all, count, ids_only),
        Commands::Find { multi, all, body } => commands::command_find(dir, multi, all, body),
        Commands::Show {
            task_id,
            show_attrs,
            raw,
        } => commands::command_show(dir, task_id, show_attrs, raw),
        Commands::Follow {
            task_id,
            link_index,
            select,
            edit,
        } => commands::command_follow(dir, task_id, link_index, select, edit),
        Commands::Edit { task_id } => commands::command_edit(dir, task_id),
        Commands::Drop {
            closed_on_commit,
            task_id,
        } => commands::command_drop(dir, task_id, closed_on_commit),
        Commands::Reopen { body, task_id } => commands::command_reopen(dir, task_id, body),
        Commands::Swap => Workspace::from_path(dir)?.swap_top(),
        Commands::Rot => Workspace::from_path(dir)?.rot(),
        Commands::Tor => Workspace::from_path(dir)?.tor(),
        Commands::Prioritize { task_id } => Workspace::from_path(dir)?.prioritize(task_id.into()),
        Commands::Deprioritize { task_id } => {
            Workspace::from_path(dir)?.deprioritize(task_id.into())
        }
        Commands::Clean => commands::clean(dir),
        Commands::Export {
            ids,
            r#where,
            all,
            bind,
        } => commands::command_export(dir, ids, r#where, all, bind),
        Commands::Import { bind } => commands::command_import(dir, bind),
        Commands::Log { target } => commands::command_log(dir, target),
        Commands::FixUp => commands::fix_up(dir),
        Commands::GitSetup { remote } => commands::git_setup(dir, remote),
        Commands::GitPush { remote } => commands::git_push(dir, remote),
        Commands::GitPull { remote, rebase } => commands::git_pull(dir, remote, rebase),
        Commands::Share { target, task_id } => commands::command_share(dir, target, task_id),
        Commands::Assign {
            target,
            task_id,
            remote,
        } => commands::command_assign(dir, target, task_id, remote),
        Commands::Pull { source, task_id } => commands::command_pull(dir, source, task_id),
        Commands::Inbox { remote } => commands::command_inbox(dir, remote),
        Commands::Accept { key, remote } => commands::command_accept(dir, key, remote),
        Commands::Reject { key, remote } => commands::command_reject(dir, key, remote),
        Commands::Prop { action } => commands::command_prop(dir, action),
        Commands::Namespace { action } => commands::command_namespace(dir, action),
        Commands::Queue { action } => commands::command_queue(dir, action),
        Commands::Remote { action } => commands::command_remote(dir, action),
        Commands::Switch { name } => {
            commands::resolve_and_switch_namespace(&Workspace::from_path(dir)?, name)
        }
        Commands::Completion { shell } => {
            generate(shell, &mut Cli::command(), "tsk", &mut io::stdout());
            Ok(())
        }
        Commands::Skill => {
            io::stdout().write_all(include_bytes!("../SKILL.md"))?;
            Ok(())
        }
        Commands::Serv { args } => serv::serve(dir, args),
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

/// Dedicated entry point for the `tsk-serv` binary.
pub fn run_serv() -> i32 {
    match serv::run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            2
        }
    }
}
