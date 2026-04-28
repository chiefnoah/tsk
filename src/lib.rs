pub mod errors;
mod fzf;
mod namespace;
mod merge;
mod object;
mod patch;
mod propvalue;
mod properties;
mod queue;
mod task;
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
use workspace::{Id, TaskIdentifier, Workspace};

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
    /// Flip a `done` task back to `open` and push it onto the active queue.
    Reopen {
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
    GitPush {
        remote: Option<String>,
    },
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
}

#[derive(Subcommand)]
enum LogTarget {
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
    /// List every task bound in a namespace (defaults to active),
    /// regardless of which queue (if any) it's on. One row per id.
    Tasks { name: Option<String> },
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
    /// Switch active queue. With no name, fzf-picks from existing queues
    /// (plus a `<new>` sentinel for creating one on the fly).
    Switch { name: Option<String> },
}

#[derive(Subcommand)]
enum RemoteAction {
    /// Print the active default remote (the one used when no `-R` is given).
    Default,
    /// Persist the active default remote for this clone. Must already be
    /// a git remote — use `git remote add ...` (and `tsk git-setup -r
    /// <name>` to configure refspecs) first.
    SetDefault { name: String },
}

#[derive(Args)]
#[group(required = true, multiple = false)]
struct Title {
    #[arg(short, value_name = "TITLE")]
    title: Option<String>,
    #[arg(value_name = "TITLE")]
    title_simple: Option<Vec<String>>,
}

#[derive(Args, Default)]
#[group(required = false, multiple = false)]
struct TaskId {
    #[arg(short = 't', value_name = "ID")]
    id: Option<u32>,
    #[arg(short = 'T', value_name = "TSK-ID", value_parser = parse_id)]
    tsk_id: Option<Id>,
    #[arg(short = 'r', value_name = "RELATIVE")]
    relative_id: Option<u32>,
}

impl TaskId {
    /// True when the user passed none of `-t`, `-T`, or `-r`. Commands
    /// that fall back to a fuzzy finder use this to decide whether to
    /// prompt; commands that prefer "top of stack" silently treat this
    /// as `Relative(0)` via the `From` impl.
    fn is_empty(&self) -> bool {
        self.id.is_none() && self.tsk_id.is_none() && self.relative_id.is_none()
    }

    /// Resolve to a `TaskIdentifier`, dropping into an fzf picker when no
    /// flag was supplied. Use when interactive selection is the desired
    /// fallback (e.g. `tsk export`); otherwise prefer `Into`, which
    /// silently picks the top of the stack.
    fn resolve_or_pick(self, ws: &Workspace) -> Result<TaskIdentifier> {
        if !self.is_empty() {
            return Ok(self.into());
        }
        let entries = ws.list_namespace_tasks(&ws.namespace())?;
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
        let id: Id =
            parse_id(id_str).map_err(|e| errors::Error::Parse(e.to_string()))?;
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

fn effective_remote(ws: &Workspace, supplied: Option<String>) -> Option<String> {
    supplied
        .map(|s| if s.is_empty() { None } else { Some(s) })
        .unwrap_or_else(|| Some(ws.default_remote()))
}

/// Scoped push (best-effort, silent on `-R ""`).
fn auto_push_refs(ws: &Workspace, remote: Option<String>, refs: Vec<String>) {
    if let Some(r) = effective_remote(ws, remote) {
        let _ = ws.git_push_refs(&r, &refs);
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
            raw,
        } => command_show(dir, task_id, show_attrs, raw),
        Commands::Edit { task_id } => command_edit(dir, task_id),
        Commands::Drop { task_id } => command_drop(dir, task_id),
        Commands::Reopen { task_id } => {
            let id = Workspace::from_path(dir)?.reopen(task_id.into())?;
            println!("Reopened {id}");
            Ok(())
        }
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
        Commands::Export {
            ids,
            r#where,
            all,
            bind,
        } => command_export(dir, ids, r#where, all, bind),
        Commands::Import { bind } => command_import(dir, bind),
        Commands::Log { target } => command_log(dir, target),
        Commands::FixUp => {
            let ws = Workspace::from_path(dir)?;
            let n = ws.backfill_status()?;
            println!("backfill-status: set status=open on {n} task(s)");
            let m = ws.migrate_property_encoding()?;
            println!("migrate-property-encoding: rewrote {m} task(s)");
            let (q, p, b, qe) = ws.gc_refs()?;
            println!(
                "gc-refs: pruned {q} empty queue(s), {p} orphan property entries, \
                 {b} ghost namespace binding(s), {qe} orphan queue index entries"
            );
            Ok(())
        }
        Commands::GitSetup { remote } => {
            let ws = Workspace::from_path(dir)?;
            let r = remote.unwrap_or_else(|| ws.default_remote());
            ws.configure_git_remote_refspecs(&r)
        }
        Commands::GitPush { remote } => {
            let ws = Workspace::from_path(dir)?;
            let r = remote.unwrap_or_else(|| ws.default_remote());
            ws.git_push(&r)
        }
        Commands::GitPull { remote, rebase } => {
            let ws = Workspace::from_path(dir)?;
            let r = remote.unwrap_or_else(|| ws.default_remote());
            let strategy = if rebase {
                merge::Strategy::Rebase
            } else {
                merge::Strategy::Merge
            };
            let outcome = ws.git_pull_with_strategy(&r, strategy)?;
            for rec in &outcome.tasks {
                if !matches!(rec.kind, merge::ReconKind::Unchanged) {
                    let short = &rec.stable.0[..12.min(rec.stable.0.len())];
                    println!("{:?} {short}", rec.kind);
                }
            }
            for nr in &outcome.namespaces {
                for (old, new) in &nr.renumbers {
                    println!(
                        "{}-{} → {}-{} (conflict with {r})",
                        nr.namespace, old, nr.namespace, new
                    );
                }
            }
            for qr in &outcome.queues {
                println!("merged queue {}", qr.name);
            }
            Ok(())
        }
        Commands::Share { target, task_id } => command_share(dir, target, task_id),
        Commands::Assign {
            target,
            task_id,
            remote,
        } => command_assign(dir, target, task_id, remote),
        Commands::Pull { source, task_id } => command_pull(dir, source, task_id),
        Commands::Inbox { remote } => command_inbox(dir, remote),
        Commands::Accept { key, remote } => command_accept(dir, key, remote),
        Commands::Reject { key, remote } => command_reject(dir, key, remote),
        Commands::Prop { action } => command_prop(dir, action),
        Commands::Namespace { action } => command_namespace(dir, action),
        Commands::Queue { action } => command_queue(dir, action),
        Commands::Remote { action } => command_remote(dir, action),
        Commands::Switch { name } => {
            resolve_and_switch_namespace(&Workspace::from_path(dir)?, name)
        }
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

fn command_show(dir: PathBuf, task_id: TaskId, show_attrs: bool, raw: bool) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let task = ws.task(task_id.into())?;
    if show_attrs && !task.attributes.is_empty() {
        println!("---");
        for (k, vs) in &task.attributes {
            for v in vs {
                println!("{k}: \"{v}\"");
            }
        }
        println!("---");
    }
    let plain = task.to_string();
    match (raw, task::parse(&plain)) {
        (false, Some(parsed)) => {
            print!("{}", parsed.content);
            // Footnote section: resolve each [[...]] link against the active
            // namespace (or just echo for foreign / external links).
            if !parsed.links.is_empty() {
                println!();
                for (i, link) in parsed.links.iter().enumerate() {
                    println!("\n{} {}", task::super_num(i + 1), render_link(&ws, link));
                }
            }
        }
        _ => print!("{plain}"),
    }
    println!();
    Ok(())
}

fn render_link(ws: &Workspace, link: &task::ParsedLink) -> String {
    use task::ParsedLink::*;
    match link {
        Internal(id) => match ws.task((*id).into()) {
            Ok(t) => format!("{id}: {}", t.title),
            Err(_) => format!("{id}: <not bound in '{}'>", ws.namespace()),
        },
        Namespaced { namespace, id } => format!("{namespace}/{id}"),
        Foreign { prefix, id } => format!("{prefix}-{id} (foreign)"),
        External(url) => url.to_string(),
    }
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
    let (key, stable) = ws.assign_to_queue(task_id.into(), &target)?;
    println!("Assigned to {target} as {key}");
    auto_push_refs(&ws, remote, ws.refs_for_assign_out(&target, &stable)?);
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
    if let Some(r) = effective_remote(&ws, remote) {
        let refs = ws.refs_for_inbox_pull();
        let _ = ws.git_fetch_refs(&r, &refs);
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

fn pick_inbox_key(ws: &Workspace, key: Option<String>) -> Result<String> {
    if let Some(k) = key {
        return Ok(k);
    }
    Ok(ws
        .list_inbox()?
        .into_iter()
        .next()
        .ok_or_else(|| errors::Error::Parse("Inbox is empty".into()))?
        .key)
}

fn command_accept(dir: PathBuf, key: Option<String>, remote: Option<String>) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let key = pick_inbox_key(&ws, key)?;
    let id = ws.accept_inbox(&key)?;
    println!("Accepted as {id}");
    auto_push_refs(&ws, remote, ws.refs_for_accept_inbox());
    Ok(())
}

fn command_reject(dir: PathBuf, key: Option<String>, remote: Option<String>) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let key = pick_inbox_key(&ws, key)?;
    ws.reject_inbox(&key)?;
    let source = key.rsplit_once('-').map(|(s, _)| s.to_string());
    match &source {
        Some(src) => println!("Rejected {key} (returned to '{src}' inbox)"),
        None => println!("Rejected {key}"),
    }
    if let Some(src) = source {
        auto_push_refs(&ws, remote, ws.refs_for_reject_inbox(&src));
    }
    Ok(())
}

fn command_export(
    dir: PathBuf,
    ids: Vec<Id>,
    where_: Option<String>,
    all: bool,
    bind: bool,
) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let mut identifiers: Vec<TaskIdentifier> = ids.into_iter().map(Into::into).collect();
    if all {
        for entry in ws.list_namespace_tasks(&ws.namespace())? {
            identifiers.push(TaskIdentifier::Id(entry.id));
        }
    }
    if let Some(spec) = where_ {
        let (key, value) = spec
            .split_once('=')
            .ok_or_else(|| errors::Error::Parse("expected --where KEY=VALUE".into()))?;
        for (id, _stable, _title) in ws.find_by_property(key, Some(value))? {
            identifiers.push(TaskIdentifier::Id(id));
        }
    }
    if identifiers.is_empty() {
        // Interactive fallback: fzf single-pick.
        identifiers.push(TaskId::default().resolve_or_pick(&ws)?);
    }
    // Dedupe while preserving order.
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    identifiers.retain(|i| !matches!(i, TaskIdentifier::Id(id) if !seen.insert(id.0)));
    let mbox = ws.export_tasks(&identifiers, bind)?;
    print!("{mbox}");
    Ok(())
}

fn command_import(dir: PathBuf, bind: bool) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
    let outcomes = ws.import_task(&buf, bind)?;
    for res in &outcomes {
        let bound = if let Some(id) = res.bound_human {
            format!(" bound as {}-{}", ws.namespace(), id)
        } else {
            String::new()
        };
        println!(
            "Imported {} commit(s) for task {}{bound}",
            res.commits_imported, res.stable
        );
    }
    Ok(())
}

fn command_log(dir: PathBuf, target: LogTarget) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let commits = match target {
        LogTarget::Task { task_id } => ws.log_task(task_id.into())?,
        LogTarget::Namespace { name } => {
            ws.log_namespace(&name.unwrap_or_else(|| ws.namespace()))?
        }
        LogTarget::Queue { name } => ws.log_queue(&name.unwrap_or_else(|| ws.queue()))?,
    };
    for c in commits {
        // git-log --oneline-style: short oid, summary, then author + date below.
        let short = &c.oid[..c.oid.len().min(8)];
        println!("{short} {}", c.summary);
        println!("    {} ({})", c.author, format_unix(c.timestamp));
    }
    Ok(())
}

fn format_unix(ts: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let delta = now - ts;
    if delta < 0 {
        return "in the future".to_string();
    }
    relative_time(delta as u64)
}

fn relative_time(secs: u64) -> String {
    const M: u64 = 60;
    const H: u64 = 60 * M;
    const D: u64 = 24 * H;
    if secs < M {
        format!("{secs}s ago")
    } else if secs < H {
        format!("{}m ago", secs / M)
    } else if secs < D {
        format!("{}h ago", secs / H)
    } else if secs < 30 * D {
        format!("{}d ago", secs / D)
    } else if secs < 365 * D {
        format!("{}mo ago", secs / (30 * D))
    } else {
        format!("{}y ago", secs / (365 * D))
    }
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
        PropAction::Keys => print_lines(ws.property_keys()?),
        PropAction::Values { key } => print_lines(ws.property_values(&key)?),
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

fn print_lines<I: std::fmt::Display>(items: impl IntoIterator<Item = I>) {
    for i in items {
        println!("{i}");
    }
}

fn command_namespace(dir: PathBuf, action: NamespaceAction) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    match action {
        NamespaceAction::List => print_lines(ws.list_namespaces()?),
        NamespaceAction::Current => println!("{}", ws.namespace()),
        NamespaceAction::Switch { name } => return resolve_and_switch_namespace(&ws, name),
        NamespaceAction::Tasks { name } => {
            let target = name.unwrap_or_else(|| ws.namespace());
            for entry in ws.list_namespace_tasks(&target)? {
                println!("{}\t{}", entry.id, entry.title);
            }
        }
    }
    Ok(())
}

fn command_remote(dir: PathBuf, action: RemoteAction) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    match action {
        RemoteAction::Default => println!("{}", ws.default_remote()),
        RemoteAction::SetDefault { name } => {
            ws.set_default_remote(&name)?;
            println!("Default remote set to '{name}'");
        }
    }
    Ok(())
}

fn command_queue(dir: PathBuf, action: QueueAction) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    match action {
        QueueAction::List => print_lines(ws.list_queues()?),
        QueueAction::Current => println!("{}", ws.queue()),
        QueueAction::Create { name, can_pull } => {
            ws.create_queue(&name, Some(can_pull))?;
            println!("Created queue '{name}' (can-pull={can_pull})");
        }
        QueueAction::Switch { name } => return resolve_and_switch_queue(&ws, name),
    }
    Ok(())
}

const NEW_NS_SENTINEL: &str = "<new>";

fn resolve_and_switch_namespace(ws: &Workspace, name: Option<String>) -> Result<()> {
    let target = match name {
        Some(n) => n,
        None => pick_with_new(&ws.list_namespaces()?, &ws.namespace(), "namespace")?,
    };
    ws.switch_namespace(&target)?;
    println!("Switched to namespace '{target}'");
    Ok(())
}

fn resolve_and_switch_queue(ws: &Workspace, name: Option<String>) -> Result<()> {
    let target = match name {
        Some(n) => n,
        None => {
            let picked = pick_with_new(&ws.list_queues()?, &ws.queue(), "queue")?;
            if !ws.list_queues()?.iter().any(|q| q == &picked) {
                ws.create_queue(&picked, None)?;
            }
            picked
        }
    };
    ws.switch_queue(&target)?;
    println!("Switched to queue '{target}'");
    Ok(())
}

fn pick_with_new(existing: &[String], current: &str, label: &str) -> Result<String> {
    let entries = picker_entries(existing, current);
    let picked = fzf::select::<_, String, _>(entries, [format!("--prompt={label}> ")])?
        .ok_or_else(|| errors::Error::Parse(format!("No {label} selected")))?;
    let picked = strip_picker_marker(&picked);
    if picked == NEW_NS_SENTINEL {
        let name = prompt_line(&format!("New {label} name: "))?;
        if name.is_empty() {
            return Err(errors::Error::Parse(format!("Empty {label} name")));
        }
        Ok(name)
    } else {
        Ok(picked.to_string())
    }
}

/// Build the fzf input lines: every existing entry (active marked with
/// `* `, others with `  `) plus a trailing `<new>` sentinel for creating
/// one on the fly. The active entry is always present even when no refs
/// have been written yet.
fn picker_entries(existing: &[String], current: &str) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_time_breakpoints() {
        assert_eq!(relative_time(0), "0s ago");
        assert_eq!(relative_time(59), "59s ago");
        assert_eq!(relative_time(60), "1m ago");
        assert_eq!(relative_time(3599), "59m ago");
        assert_eq!(relative_time(3600), "1h ago");
        assert_eq!(relative_time(86_399), "23h ago");
        assert_eq!(relative_time(86_400), "1d ago");
        assert_eq!(relative_time(30 * 86_400), "1mo ago");
        assert_eq!(relative_time(365 * 86_400), "1y ago");
    }

    #[test]
    fn picker_marks_current_and_appends_sentinel() {
        let entries = picker_entries(
            &["alpha".to_string(), "tsk".to_string()],
            "tsk",
        );
        assert_eq!(entries, vec!["  alpha", "* tsk", "<new>"]);
    }

    #[test]
    fn picker_includes_current_when_missing_from_list() {
        let entries = picker_entries(&[], "tsk");
        assert_eq!(entries, vec!["* tsk", "<new>"]);
    }

    #[test]
    fn strip_marker_handles_all_prefixes() {
        assert_eq!(strip_picker_marker("* tsk"), "tsk");
        assert_eq!(strip_picker_marker("  alpha"), "alpha");
        assert_eq!(strip_picker_marker("<new>"), "<new>");
    }
}
