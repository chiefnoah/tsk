mod backend;
mod errors;
mod fzf;
mod stack;
mod task;
mod util;
mod workspace;
use clap_complete::{Shell, generate};
use errors::Result;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::exit;
use std::str::FromStr as _;
use std::{env::current_dir, fs::OpenOptions, io::Read};
use task::ParsedLink;
use workspace::{Id, Task, TaskIdentifier, Workspace};

//use smol;
//use iocraft::prelude::*;
use clap::{Args, CommandFactory, Parser, Subcommand};
use edit::edit as open_editor;

fn default_dir() -> Result<PathBuf> {
    Ok(current_dir()?)
}

fn parse_id(s: &str) -> std::result::Result<Id, &'static str> {
    Id::from_str(s).map_err(|_| "Unable to parse tsk- ID")
}

#[derive(Parser)]
// TODO: add long_about
#[command(version, about)]
struct Cli {
    /// Override the tsk root directory.
    #[arg(short = 'C', env = "TSK_ROOT", value_name = "DIR")]
    dir: Option<PathBuf>,
    // TODO: other global options
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initializes a .tsk workspace in the current effective directory, which defaults to PWD.
    Init,
    /// Creates a new task, automatically assigning it a unique identifider and persisting
    Push {
        /// Whether to open $EDITOR to edit the content of the task. The first line if the
        /// resulting file will be the task's title. The body follows the title after two newlines,
        /// similr to the format of a commit message.
        #[arg(short = 'e', default_value_t = false)]
        edit: bool,

        /// The body of the task. It may be specified as either a string using quotes or the
        /// special character '-' to read from stdin.
        #[arg(short = 'b')]
        body: Option<String>,

        /// The title of the task as a raw string. It mus be proceeded by two dashes (--).
        #[command(flatten)]
        title: Title,
    },
    /// Creates a new task just like `push`, but instead of putting it at the top of the stack, it
    /// puts it at the bottom
    Append {
        /// Whether to open $EDITOR to edit the content of the task. The first line if the
        /// resulting file will be the task's title. The body follows the title after two newlines,
        /// similr to the format of a commit message.
        #[arg(short = 'e', default_value_t = false)]
        edit: bool,

        /// The body of the task. It may be specified as either a string using quotes or the
        /// special character '-' to read from stdin.
        #[arg(short = 'b')]
        body: Option<String>,

        /// The title of the task as a raw string. It mus be proceeded by two dashes (--).
        #[command(flatten)]
        title: Title,
    },
    /// Print the task stack. This will include just TSK-IDs and the title.
    List {
        /// Whether to list all tasks in the task stack. If specified, -c / count is ignored.
        #[arg(short = 'a', default_value_t = false)]
        all: bool,
        #[arg(short = 'c', default_value_t = 10)]
        count: usize,
        /// Only print task IDs, one per line.
        #[arg(short = 'q', default_value_t = false)]
        ids_only: bool,
    },

    /// Swaps the top two tasks on the stack. If there are less than 2 tasks on the stack, there is
    /// no effect.
    Swap,

    /// Open up an editor to modify the task with the given ID.
    Edit {
        #[command(flatten)]
        task_id: TaskId,
    },

    /// Generates completion for a given shell.
    Completion {
        #[arg(short = 's')]
        shell: Shell,
    },

    /// Use fuzzy finding with `fzf` to search for a task
    Find {
        #[command(flatten)]
        args: FindArgs,
        /// Whether to print the a shortened tsk ID (just the integer portion). Defaults to *false*
        #[arg(short = 'f', default_value_t = false)]
        short_id: bool,
    },

    /// Prints the contents of a task, parsing the body as rich text and formatting it using ANSI
    /// escape sequences.
    Show {
        /// Shows raw file attributes for the file
        #[arg(short = 'x', default_value_t = false)]
        show_attrs: bool,

        #[arg(short = 'R', default_value_t = false)]
        raw: bool,
        /// The [TSK-]ID of the task to display
        #[command(flatten)]
        task_id: TaskId,
    },

    /// Follow a link that is parsed from a task body. It may be an internal or external link (ie.
    /// a url or a wiki-style link using double square brackets). When using the `tsk show`
    /// command, links that are successfully parsed get a numeric superscript that may be used to
    /// address the link. That number should be supplied to the -l/link_index where it will be
    /// subsequently followed opened or shown.
    Follow {
        /// The task whose body will be searched for links.
        #[command(flatten)]
        task_id: TaskId,
        /// The index of the link to open. Must be supplied.
        #[arg(short = 'l', default_value_t = 1)]
        link_index: usize,
        /// When opening an internal link, whether to show or edit the addressed task.
        #[arg(short = 'e', default_value_t = false)]
        edit: bool,
    },

    /// Drops the task on the top of the stack and archives it.
    Drop {
        /// The [TSK-]ID of the task to drop.
        #[command(flatten)]
        task_id: TaskId,
    },

    /// Moves the 3rd item on the stack to the front of the stack, shifting everything else down by
    /// one. If there are less than 3 tasks on the stack, has no effect.
    Rot,
    /// Moves the task on the top of the stack back behind the 2nd element, shifting the next two
    /// task up.
    Tor,

    /// Prioritizes an arbitrary task to the top of the stack.
    Prioritize {
        /// The [TSK-]ID to prioritize. If it exists, it is moved to the top of the stack.
        #[command(flatten)]
        task_id: TaskId,
    },

    /// Deprioritizes a task to the bottom of the stack.
    Deprioritize {
        /// The [TSK-]ID to deprioritize. If it exists, it is moved to the bottom of the stack.
        #[command(flatten)]
        task_id: TaskId,
    },

    /// Cleans up orphaned task files in .tsk/tasks/ that are no longer in the stack index.
    Clean,

    /// Manage remote workspace mappings for cross-workspace task linking.
    Remote {
        #[command(subcommand)]
        action: RemoteAction,
    },

    /// Sets up git integration by adding .tsk/ to .git/info/exclude or .gitignore.
    GitSetup {
        /// Use .gitignore instead of .git/info/exclude.
        #[arg(short = 'g', default_value_t = false)]
        gitignore: bool,
        /// Also configure push/fetch refspecs on the named remote so refs/tsk/*
        /// is included in `git push <remote>` and `git fetch <remote>`.
        #[arg(short = 'r')]
        remote: Option<String>,
    },

    /// Push refs/tsk/* to a git remote so other clones can pull task state.
    GitPush {
        /// Remote name (e.g. origin).
        remote: String,
    },

    /// Fetch refs/tsk/* from a git remote, overwriting local task state.
    GitPull {
        /// Remote name (e.g. origin).
        remote: String,
    },

    /// Send a task to another namespace's inbox. Defaults to the top-of-stack
    /// task; use -T to pick a different one. Sets `assigned=[[<ns>/tsk-N]]`
    /// on the source.
    Export {
        /// Target namespace.
        target: String,
        #[command(flatten)]
        task_id: TaskId,
    },

    /// List tasks pending in the current namespace's inbox.
    Inbox,

    /// Accept a pending inbox item, creating a new local task with copied
    /// content + properties and `source=[[<src-ns>/tsk-N]]` set.
    Accept {
        /// Inbox key (e.g. `alice-3` or `inbox/alice-3`). With no argument,
        /// accepts the first item in the inbox.
        key: Option<String>,
    },

    /// Bundle the entire workspace into a zip archive.
    Bundle {
        /// Output path. Defaults to ./tsk.zip.
        #[arg(short = 'o')]
        output: Option<PathBuf>,
    },

    /// Migrate a file-backed workspace to a git-backed one. The directory must
    /// now be inside a git repository (run `git init` first if needed). All
    /// task data is copied into refs/tsk/* and the on-disk files are removed.
    Migrate,

    /// Print the event log. Without -T, prints every event in the current
    /// namespace, newest first, in git-log style. With -T, scopes to one task.
    Log {
        /// Optionally scope to a single task by tsk-ID.
        #[arg(short = 'T', value_name = "TSK-ID", value_parser = parse_id)]
        tsk_id: Option<Id>,
    },

    /// Get/set/find tasks by property. Properties are arbitrary key/value
    /// pairs stored alongside a task; some are synthetic (state, has-links,
    /// references, referenced-by) and computed on read.
    Prop {
        #[command(subcommand)]
        action: PropAction,
    },

    /// Manage namespaces within a git-backed workspace. Namespaces let multiple
    /// people share the same git repo without sharing tasks; refs live under
    /// refs/tsk/<namespace>/.
    Namespace {
        #[command(subcommand)]
        action: NamespaceAction,
    },

    /// Switch to a different namespace. Shorthand for `tsk namespace switch`.
    Switch { name: String },

    /// List the hyperlinks parsed from a task's body. With -s, pipe the list
    /// through fzf and open the selected link via the existing follow path:
    /// URLs go to the system handler, [[tsk-N]] internal links are shown,
    /// foreign refs resolve through the configured remote.
    Links {
        #[command(flatten)]
        task_id: TaskId,
        /// Use fzf to select a link, then open it.
        #[arg(short = 's', default_value_t = false)]
        select: bool,
    },

    /// Reopens an archived task, recreating the symlink and adding it back to the stack.
    Reopen {
        #[command(flatten)]
        task_id: TaskId,
    },
}

#[derive(Subcommand)]
enum PropAction {
    /// List all properties on a task (stored + synthetic).
    List {
        #[command(flatten)]
        task_id: TaskId,
    },
    /// Set a property. Value may be omitted for unary properties.
    Set {
        #[command(flatten)]
        task_id: TaskId,
        key: String,
        value: Option<String>,
    },
    /// Remove a property from a task. No-op if not set.
    Unset {
        #[command(flatten)]
        task_id: TaskId,
        key: String,
    },
    /// Find every task whose property KEY equals VALUE (or that has KEY set
    /// at all when VALUE is omitted).
    Find { key: String, value: Option<String> },
}

#[derive(Subcommand)]
enum NamespaceAction {
    /// List all namespaces with refs in this repo.
    List,
    /// Print the current namespace name.
    Current,
    /// Switch to (create on first push of) the given namespace.
    Switch { name: String },
    /// Create an empty namespace and switch to it.
    Create { name: String },
    /// Delete every ref under the given namespace. Refuses if the namespace is
    /// the active one. Prompts for confirmation when it has tasks unless -y.
    Delete {
        name: String,
        /// Skip the confirmation prompt.
        #[arg(short = 'y', default_value_t = false)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum RemoteAction {
    /// List configured remote workspaces.
    List,
    /// Add a remote workspace mapping.
    Add {
        /// The prefix to use for this remote (e.g. "jira", "gl").
        prefix: String,
        /// The path to the remote workspace.
        path: String,
    },
    /// Remove a remote workspace mapping.
    Remove {
        /// The prefix of the remote to remove.
        prefix: String,
    },
}

#[derive(Args)]
#[group(required = true, multiple = false)]
struct Title {
    /// The title of the task. This is useful for when you also wish to specify the body of the
    /// task as an argument (ie. with -b).
    #[arg(short, value_name = "TITLE")]
    title: Option<String>,

    #[arg(value_name = "TITLE")]
    title_simple: Option<Vec<String>>,
}

#[derive(Args)]
#[group(required = false, multiple = false)]
struct TaskId {
    /// The ID of the task to select as a plain integer.
    #[arg(short = 't', value_name = "ID")]
    id: Option<u32>,

    /// The ID of the task to select with the 'tsk-' prefix.
    #[arg(short = 'T', value_name = "TSK-ID", value_parser = parse_id)]
    tsk_id: Option<Id>,

    /// Selects a task relative to the top of the stack.
    /// If no option is specified, the task selected will be the top of the stack.
    #[arg(short = 'r', value_name = "RELATIVE", default_value_t = 0)]
    relative_id: u32,

    #[command(flatten)]
    find: Find,
}

/// Use fuzzy finding to search for and select a task.
/// Does not support searching task bodies or archived tasks.
#[derive(Args)]
#[group(required = false, multiple = true)]
struct Find {
    /// Use fuzzy finding to select a task.
    #[arg(short = 'f', value_name = "FIND", default_value_t = false)]
    find: bool,
    #[command(flatten)]
    args: FindArgs,
}

#[derive(Args)]
#[group(required = false, multiple = false)]
struct FindArgs {
    /// Exclude the contents of tasks in the search criteria.
    #[arg(short = 'b', default_value_t = false)]
    exclude_body: bool,
    /// Include archived tasks in the search criteria. Combine with `-b` to include archived
    /// bodies in the search criteria.
    #[arg(short = 'a', default_value_t = false)]
    search_archived: bool,
}

impl From<TaskId> for TaskIdentifier {
    fn from(value: TaskId) -> Self {
        if let Some(id) = value.id.map(Id::from).or(value.tsk_id) {
            TaskIdentifier::Id(id)
        } else if value.find.find {
            TaskIdentifier::Find {
                exclude_body: value.find.args.exclude_body,
                archived: value.find.args.search_archived,
            }
        } else {
            TaskIdentifier::Relative(value.relative_id)
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let dir = match cli.dir {
        Some(d) => d,
        None => default_dir()?,
    };
    match cli.command {
        Commands::Init => command_init(dir),
        Commands::Push { edit, body, title } => command_push(dir, edit, body, title),
        Commands::Append { edit, body, title } => command_append(dir, edit, body, title),
        Commands::List {
            all,
            count,
            ids_only,
        } => command_list(dir, all, count, ids_only),
        Commands::Swap => command_swap(dir),
        Commands::Show {
            task_id,
            raw,
            show_attrs,
        } => command_show(dir, task_id, show_attrs, raw),
        Commands::Follow {
            task_id,
            link_index,
            edit,
        } => command_follow(dir, task_id, link_index, edit),
        Commands::Edit { task_id } => command_edit(dir, task_id),
        Commands::Completion { shell } => command_completion(shell),
        Commands::Drop { task_id } => command_drop(dir, task_id),
        Commands::Find { args, short_id } => command_find(dir, short_id, args),
        Commands::Rot => Workspace::from_path(dir)?.rot(),
        Commands::Tor => Workspace::from_path(dir)?.tor(),
        Commands::Prioritize { task_id } => command_prioritize(dir, task_id),
        Commands::Deprioritize { task_id } => command_deprioritize(dir, task_id),
        Commands::Clean => command_clean(dir),
        Commands::Remote { action } => command_remote(dir, action),
        Commands::GitSetup { gitignore, remote } => command_git_setup(dir, gitignore, remote),
        Commands::GitPush { remote } => command_git_push(dir, remote),
        Commands::GitPull { remote } => command_git_pull(dir, remote),
        Commands::Export { target, task_id } => command_export_to_ns(dir, target, task_id),
        Commands::Inbox => command_inbox(dir),
        Commands::Accept { key } => command_accept(dir, key),
        Commands::Bundle { output } => command_bundle(dir, output),
        Commands::Migrate => command_migrate(dir),
        Commands::Links { task_id, select } => command_links(dir, task_id, select),
        Commands::Reopen { task_id } => command_reopen(dir, task_id),
        Commands::Log { tsk_id } => command_log(dir, tsk_id),
        Commands::Prop { action } => command_prop(dir, action),
        Commands::Namespace { action } => command_namespace(dir, action),
        Commands::Switch { name } => command_namespace_switch(dir, &name),
    }
}

fn main() {
    match run(Cli::parse()) {
        Ok(()) => exit(0),
        Err(e) => {
            eprintln!("{e}");
            exit(2);
        }
    }
}

fn taskid_from_tsk_id(tsk_id: Id) -> TaskId {
    TaskId {
        tsk_id: Some(tsk_id),
        id: None,
        relative_id: 0,
        find: Find {
            find: false,
            args: FindArgs {
                exclude_body: true,
                search_archived: false,
            },
        },
    }
}

fn command_init(dir: PathBuf) -> Result<()> {
    Workspace::init(dir)
}

fn create_task(
    workspace: &mut Workspace,
    edit: bool,
    body: Option<String>,
    title: Title,
) -> Result<Task> {
    let mut title = if let Some(title) = title.title {
        title
    } else if let Some(title) = title.title_simple {
        title.join(" ")
    } else {
        "".to_string()
    };
    // If no body was explicitly provided and the title contains newlines,
    // treat the first line as the title and the rest as the body (like git commit -m)
    let mut body = if body.is_none() {
        if let Some((first_line, rest)) = title.split_once('\n') {
            let extracted_body = rest.to_string();
            title = first_line.to_string();
            extracted_body
        } else {
            String::new()
        }
    } else {
        // Body was explicitly provided, so strip any newlines from title
        title = title.replace(['\n', '\r'], " ");
        body.unwrap_or_default()
    };
    if body == "-" {
        // add newline so you can type directly in the shell
        //eprintln!("");
        body.clear();
        std::io::stdin().read_to_string(&mut body)?;
    }
    if edit {
        let new_content = open_editor(format!("{title}\n\n{body}"))?;
        if let Some(content) = new_content.split_once("\n") {
            title = content.0.to_string();
            body = content.1.to_string();
        }
    }
    // Ensure title never contains newlines (invariant for index file format)
    title = title.replace(['\n', '\r'], " ");
    let task = workspace.new_task(title, body)?;
    workspace.handle_metadata(&task, None)?;
    Ok(task)
}

fn command_push(dir: PathBuf, edit: bool, body: Option<String>, title: Title) -> Result<()> {
    let mut workspace = Workspace::from_path(dir)?;
    let task = create_task(&mut workspace, edit, body, title)?;
    workspace.push_task(task)
}

fn command_append(dir: PathBuf, edit: bool, body: Option<String>, title: Title) -> Result<()> {
    let mut workspace = Workspace::from_path(dir)?;
    let task = create_task(&mut workspace, edit, body, title)?;
    workspace.append_task(task)
}

fn command_list(dir: PathBuf, all: bool, count: usize, ids_only: bool) -> Result<()> {
    let workspace = Workspace::from_path(dir)?;
    let stack = workspace.read_stack()?;

    if stack.empty() {
        println!("*No tasks*");
        exit(0);
    }

    for (_, stack_item) in stack
        .into_iter()
        .enumerate()
        .take_while(|(idx, _)| all || idx < &count)
    {
        if ids_only {
            println!("{}", stack_item.id);
        } else if let Some(parsed) = task::parse(&stack_item.title) {
            println!("{}\t{}", stack_item.id, parsed.content.trim());
        } else {
            println!("{stack_item}");
        }
    }
    Ok(())
}

fn command_swap(dir: PathBuf) -> Result<()> {
    let workspace = Workspace::from_path(dir)?;
    workspace.swap_top()?;
    Ok(())
}

fn command_edit(dir: PathBuf, id: TaskId) -> Result<()> {
    let workspace = Workspace::from_path(dir)?;
    let id: TaskIdentifier = id.into();
    let mut task = workspace.task(id)?;
    let pre_links = task::parse(&task.to_string()).map(|pt| pt.intenal_links());
    let new_content = open_editor(format!("{}\n\n{}", task.title.trim(), task.body.trim()))?;
    if let Some((title, body)) = new_content.split_once("\n") {
        // Ensure title never contains newlines (invariant for index file format)
        task.title = title.replace(['\n', '\r'], " ");
        task.body = body.to_string();
        workspace.handle_metadata(&task, pre_links)?;
        workspace.save_task(&task)?;
    }
    Ok(())
}

fn command_completion(shell: Shell) -> Result<()> {
    generate(shell, &mut Cli::command(), "tsk", &mut io::stdout());
    Ok(())
}

fn command_drop(dir: PathBuf, task_id: TaskId) -> Result<()> {
    if let Some(id) = Workspace::from_path(dir)?.drop(task_id.into())? {
        eprint!("Dropped ");
        println!("{id}");
    } else {
        eprintln!("No task to drop.");
        exit(1);
    }
    Ok(())
}

fn command_find(dir: PathBuf, short_id: bool, find_args: FindArgs) -> Result<()> {
    let id = Workspace::from_path(dir)?.search(None, !find_args.exclude_body, false)?;
    if let Some(id) = id {
        if short_id {
            // print as integer
            println!("{}", id.0);
        } else {
            println!("{id}");
        }
    } else {
        eprintln!("No task selected.");
        exit(1);
    }
    Ok(())
}

fn command_prioritize(dir: PathBuf, task_id: TaskId) -> Result<()> {
    Workspace::from_path(dir)?.prioritize(task_id.into())
}

fn command_deprioritize(dir: PathBuf, task_id: TaskId) -> Result<()> {
    Workspace::from_path(dir)?.deprioritize(task_id.into())
}

fn command_show(dir: PathBuf, task_id: TaskId, show_attrs: bool, raw: bool) -> Result<()> {
    let task = Workspace::from_path(dir)?.task(task_id.into())?;
    // YAML front-matter style. YAML is gross, but it's what everyone uses!
    if show_attrs && !task.attributes.is_empty() {
        println!("---");
        for (attr, value) in task.attributes.iter() {
            println!("{attr}: \"{value}\"");
        }
        println!("---");
    }
    match task::parse(&task.to_string()) {
        Some(styled_task) if !raw => {
            writeln!(io::stdout(), "{}", styled_task.content)?;
        }
        _ => {
            println!("{task}");
        }
    }
    Ok(())
}

fn command_follow(dir: PathBuf, task_id: TaskId, link_index: usize, edit: bool) -> Result<()> {
    let task = Workspace::from_path(dir.clone())?.task(task_id.into())?;
    if let Some(parsed_task) = task::parse(&task.to_string()) {
        if link_index == 0 || link_index > parsed_task.links.len() {
            eprintln!("Link index out of bounds.");
            exit(1);
        }
        let link = &parsed_task.links[link_index - 1];
        match link {
            ParsedLink::External(url) => {
                open::that_detached(url.as_str())?;
                Ok(())
            }
            ParsedLink::Internal(id) => {
                let taskid = taskid_from_tsk_id(*id);
                if edit {
                    command_edit(dir, taskid)
                } else {
                    command_show(dir, taskid, false, false)
                }
            }
            ParsedLink::Foreign { prefix, id } => {
                let workspace = Workspace::from_path(dir.clone())?;
                if let Some(task) = workspace.resolve_foreign_link(prefix, *id)? {
                    if edit {
                        eprintln!("Editing foreign tasks is not supported.");
                        exit(1);
                    } else {
                        println!("{task}");
                    }
                } else {
                    eprintln!("Task {prefix}-{id} not found in remote workspace.");
                    exit(1);
                }
                Ok(())
            }
        }
    } else {
        eprintln!("Unable to parse any links from body.");
        exit(1);
    }
}

fn command_clean(dir: PathBuf) -> Result<()> {
    Workspace::from_path(dir)?.clean()?;
    Ok(())
}

fn command_remote(dir: PathBuf, action: RemoteAction) -> Result<()> {
    let workspace = Workspace::from_path(dir)?;
    match action {
        RemoteAction::List => {
            let remotes = workspace.read_remotes()?;
            if remotes.is_empty() {
                println!("No remotes configured.");
            } else {
                for remote in remotes {
                    println!("{remote}");
                }
            }
        }
        RemoteAction::Add { prefix, path } => {
            workspace.add_remote(&prefix, &path)?;
            eprintln!("Added remote '{prefix}' -> {path}");
        }
        RemoteAction::Remove { prefix } => {
            workspace.remove_remote(&prefix)?;
            eprintln!("Removed remote '{prefix}'");
        }
    }
    Ok(())
}

fn command_git_push(dir: PathBuf, remote: String) -> Result<()> {
    let workspace = Workspace::from_path(dir)?;
    workspace.git_push_refs(&remote)
}

fn command_git_pull(dir: PathBuf, remote: String) -> Result<()> {
    let workspace = Workspace::from_path(dir)?;
    workspace.git_pull_refs(&remote)
}

fn command_git_setup(dir: PathBuf, use_gitignore: bool, remote: Option<String>) -> Result<()> {
    let workspace = Workspace::from_path(dir)?;
    let git_dir = workspace.path.join(".git");
    if !git_dir.exists() {
        eprintln!("No .git directory found at workspace root.");
        exit(1);
    }
    let (ignore_file, label) = if use_gitignore {
        (workspace.path.join(".gitignore"), ".gitignore")
    } else {
        let info_dir = git_dir.join("info");
        std::fs::create_dir_all(&info_dir)?;
        (info_dir.join("exclude"), ".git/info/exclude")
    };
    let content = if ignore_file.exists() {
        std::fs::read_to_string(&ignore_file)?
    } else {
        String::new()
    };
    if content.lines().any(|line| line.trim() == ".tsk/") {
        eprintln!(".tsk/ is already in {label}.");
        return Ok(());
    }
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&ignore_file)?;
    writeln!(file, ".tsk/")?;
    eprintln!("Added .tsk/ to {label}.");
    if let Some(remote) = remote {
        workspace.configure_git_remote_refspecs(&remote)?;
        eprintln!("Configured push/fetch refspecs on remote '{remote}' for refs/tsk/*");
    }
    Ok(())
}

fn command_bundle(dir: PathBuf, output: Option<PathBuf>) -> Result<()> {
    let workspace = Workspace::from_path(dir)?;
    let dest = output.unwrap_or_else(|| PathBuf::from("tsk.zip"));
    workspace.export_zip(&dest)?;
    eprintln!("Wrote {}", dest.display());
    Ok(())
}

fn command_export_to_ns(dir: PathBuf, target: String, task_id: TaskId) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let id = ws.task(task_id.into())?.id;
    let key = ws.export_to_namespace(&target, id)?;
    eprintln!("Sent {id} to namespace '{target}' (inbox key: {key})");
    Ok(())
}

fn command_inbox(dir: PathBuf) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let items = ws.list_inbox()?;
    if items.is_empty() {
        println!("Inbox is empty.");
        return Ok(());
    }
    for item in items {
        println!(
            "{}\t{}/tsk-{}\t{}",
            item.inbox_key.trim_start_matches("inbox/"),
            item.source_namespace,
            item.source_id,
            item.title
        );
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
                .inbox_key
        }
    };
    let id = ws.accept_inbox(&key)?;
    eprintln!("Accepted as {id}");
    Ok(())
}

fn command_migrate(dir: PathBuf) -> Result<()> {
    let workspace = Workspace::from_path(dir)?;
    let git_dir = workspace.migrate_to_git()?;
    eprintln!(
        "Migrated workspace to git refs (git dir: {})",
        git_dir.display()
    );
    Ok(())
}

fn command_log(dir: PathBuf, tsk_id: Option<Id>) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let mut entries = match tsk_id {
        Some(id) => ws.read_log(id)?,
        None => ws.read_namespace_log()?,
    };
    if entries.is_empty() {
        eprintln!("No log entries.");
        return Ok(());
    }
    // Newest first, git-log style.
    entries.reverse();
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let header = if tsk_id.is_some() {
            format!("event {}", e.event)
        } else {
            format!("event {} {}", e.id, e.event)
        };
        println!("{header}");
        if !e.author.is_empty() {
            println!("Author: {}", e.author);
        }
        let ts = std::time::UNIX_EPOCH + std::time::Duration::from_secs(e.timestamp);
        println!("Date:   {}", format_systemtime(ts));
        if !e.detail.is_empty() {
            println!();
            println!("    {}", e.detail);
        }
    }
    Ok(())
}

fn format_systemtime(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Lightweight RFC3339-ish formatter: split into Y-m-d H:M:S UTC. Avoids
    // pulling in chrono just for this.
    let (y, mo, d, h, mi, s) = ymd_hms_utc(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn ymd_hms_utc(secs: u64) -> (u64, u32, u32, u32, u32, u32) {
    let day = secs / 86_400;
    let rem = secs % 86_400;
    let h = (rem / 3600) as u32;
    let mi = ((rem % 3600) / 60) as u32;
    let s = (rem % 60) as u32;
    // Civil-from-days (Howard Hinnant). Stable for all valid u64 epoch days.
    let z = day as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let mo = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if mo <= 2 { y + 1 } else { y };
    (y as u64, mo, d, h, mi, s)
}

fn command_prop(dir: PathBuf, action: PropAction) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    match action {
        PropAction::List { task_id } => {
            let id = ws.task(task_id.into())?.id;
            for (k, v) in ws.properties(id)? {
                if v.is_empty() {
                    println!("{k}");
                } else {
                    println!("{k}\t{v}");
                }
            }
        }
        PropAction::Set {
            task_id,
            key,
            value,
        } => {
            let id = ws.task(task_id.into())?.id;
            ws.set_property(id, &key, value.as_deref().unwrap_or(""))?;
        }
        PropAction::Unset { task_id, key } => {
            let id = ws.task(task_id.into())?.id;
            ws.unset_property(id, &key)?;
        }
        PropAction::Find { key, value } => {
            for id in ws.find_by_property(&key, value.as_deref())? {
                println!("{id}");
            }
        }
    }
    Ok(())
}

fn command_namespace_switch(dir: PathBuf, name: &str) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    ws.switch_namespace(name)?;
    eprintln!("Switched to namespace '{name}'");
    Ok(())
}

fn command_namespace(dir: PathBuf, action: NamespaceAction) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    match action {
        NamespaceAction::Current => {
            println!("{}", ws.namespace());
        }
        NamespaceAction::List => {
            let cur = ws.namespace();
            for ns in ws.list_namespaces()? {
                let marker = if ns == cur { "* " } else { "  " };
                println!("{marker}{ns}");
            }
        }
        NamespaceAction::Switch { name } | NamespaceAction::Create { name } => {
            ws.switch_namespace(&name)?;
            eprintln!("Switched to namespace '{name}'");
        }
        NamespaceAction::Delete { name, yes } => {
            let count = ws.namespace_ref_count(&name)?;
            if count == 0 {
                eprintln!("Namespace '{name}' has no refs.");
                return Ok(());
            }
            if !yes {
                eprint!("Namespace '{name}' has {count} refs. Delete? [y/N] ");
                use std::io::Write as _;
                io::stderr().flush()?;
                let mut answer = String::new();
                io::stdin().read_line(&mut answer)?;
                if !matches!(answer.trim(), "y" | "Y" | "yes") {
                    eprintln!("Aborted.");
                    return Ok(());
                }
            }
            let n = ws.delete_namespace(&name)?;
            eprintln!("Deleted {n} refs from namespace '{name}'");
        }
    }
    Ok(())
}

fn render_link(link: &ParsedLink) -> String {
    match link {
        ParsedLink::External(url) => url.to_string(),
        ParsedLink::Internal(id) => format!("[[{id}]]"),
        ParsedLink::Foreign { prefix, id } => format!("[[{prefix}-{id}]]"),
    }
}

fn command_links(dir: PathBuf, task_id: TaskId, select: bool) -> Result<()> {
    let workspace = Workspace::from_path(dir.clone())?;
    let task = workspace.task(task_id.into())?;
    let parsed = task::parse(&task.to_string());
    let links: Vec<ParsedLink> = parsed.map(|p| p.links).unwrap_or_default();
    if links.is_empty() {
        eprintln!("No links found in {}.", task.id);
        return Ok(());
    }

    if !select {
        for (i, link) in links.iter().enumerate() {
            println!("{}\t{}", i + 1, render_link(link));
        }
        return Ok(());
    }

    // -s: pipe through fzf and open the picked link via command_follow.
    let lines: Vec<String> = links
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{}\t{}", i + 1, render_link(l)))
        .collect();
    let chosen: Option<usize> =
        fzf::select::<_, usize, _>(lines, ["--delimiter=\t", "--accept-nth=1"])?;
    let Some(idx) = chosen else {
        eprintln!("No link selected.");
        exit(1);
    };
    command_follow(dir, taskid_from_tsk_id(task.id), idx, false)
}

fn command_reopen(dir: PathBuf, task_id: TaskId) -> Result<()> {
    let workspace = Workspace::from_path(dir)?;
    let id: TaskIdentifier = task_id.into();
    let reopened_id = workspace.reopen(id)?;
    eprintln!("Reopened ");
    println!("{reopened_id}");
    Ok(())
}
