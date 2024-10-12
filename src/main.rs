mod attrs;
mod errors;
mod fzf;
mod stack;
mod util;
mod workspace;
use clap_complete::{generate, Shell};
use errors::Result;
use std::io;
use std::path::PathBuf;
use std::process::exit;
use std::{env::current_dir, io::Read};
use workspace::{Id, TaskIdentifier, Workspace};

//use smol;
//use iocraft::prelude::*;
use clap::{value_parser, Args, CommandFactory, Parser, Subcommand};
use edit::edit as open_editor;

fn default_dir() -> PathBuf {
    current_dir().unwrap()
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
    /// Print the task stack. This will include just TSK-IDs and the title.
    List {
        /// Whether to list all tasks in the task stack. If specified, -c / count is ignored.
        #[arg(short = 'a', default_value_t = false)]
        all: bool,
        #[arg(short = 'c', default_value_t = 10)]
        count: usize,
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
        /// Whether to print the full TSK-ID (instead of just an integer)
        #[arg(short = 'F', default_value_t = true)]
        full_id: bool,
    },

    /// Prints the contents of a task.
    Show {
        /// Shows raw file attributes for the file
        #[arg(short = 'x', default_value_t = false)]
        show_attrs: bool,
        /// The [TSK-]ID of the task to display
        #[command(flatten)]
        task_id: TaskId,
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
    #[arg(short = 'T', value_name = "TSK-ID", value_parser = value_parser!(String))]
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
    /// Include the contents of tasks in the search criteria.
    #[arg(short = 'b', default_value_t = false)]
    search_body: bool,
    /* TODO: implement this
    /// Include archived tasks in the search criteria. Combine with `-b` to include archived
    /// bodies in the search criteria.
    #[arg(short = 'a', default_value_t = false)]
    search_archived: bool,
    */
}

impl From<TaskId> for TaskIdentifier {
    fn from(value: TaskId) -> Self {
        if let Some(id) = value.id.map(Id::from).or(value.tsk_id) {
            TaskIdentifier::Id(id)
        } else {
            if value.find.find {
                TaskIdentifier::Find {
                    search_body: value.find.args.search_body,
                    archived: false,
                }
            } else {
                TaskIdentifier::Relative(value.relative_id)
            }
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let dir = cli.dir.unwrap_or(default_dir());
    let var_name = match cli.command {
        Commands::Init => command_init(dir),
        Commands::Push { edit, body, title } => command_push(dir, edit, body, title),
        Commands::List { all, count } => command_list(dir, all, count),
        Commands::Swap => command_swap(dir),
        Commands::Show {
            task_id,
            show_attrs,
        } => command_show(dir, task_id, show_attrs),
        Commands::Edit { task_id } => command_edit(dir, task_id),
        Commands::Completion { shell } => command_completion(shell),
        Commands::Drop { task_id } => command_drop(dir, task_id),
        Commands::Find { args, full_id } => command_find(dir, full_id, args),
        Commands::Rot => Workspace::from_path(dir).unwrap().rot(),
        Commands::Tor => Workspace::from_path(dir).unwrap().tor(),
        Commands::Prioritize { task_id } => command_prioritize(dir, task_id),
        Commands::Deprioritize { task_id } => command_deprioritize(dir, task_id),
    };
    let result = var_name;
    match result {
        Ok(_) => exit(0),
        Err(e) => {
            eprintln!("{e}");
            exit(2);
        }
    }
}

fn command_init(dir: PathBuf) -> Result<()> {
    Workspace::init(dir)
}

fn command_push(dir: PathBuf, edit: bool, body: Option<String>, title: Title) -> Result<()> {
    let workspace = Workspace::from_path(dir)?;
    let mut title = if let Some(title) = title.title {
        title
    } else if let Some(title) = title.title_simple {
        let joined = title.join(" ");
        joined
    } else {
        "".to_string()
    };
    let mut body = body.unwrap_or_default();
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
    let task = workspace.new_task(title, body)?;
    workspace.push_task(task)
}

fn command_list(dir: PathBuf, all: bool, count: usize) -> Result<()> {
    let workspace = Workspace::from_path(dir)?;
    let stack = if all {
        workspace.read_stack()?
    } else {
        workspace.read_stack()?
    };
    if stack.empty() {
        println!("*No tasks*");
        exit(0);
    } else {
        if !all {
            for stack_item in stack.into_iter().take(count) {
                println!("{stack_item}");
            }
        } else {
            for stack_item in stack.into_iter() {
                println!("{stack_item}");
            }
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
    let new_content = open_editor(format!("{}\n\n{}", task.title.trim(), task.body.trim()))?;
    if let Some((title, body)) = new_content.split_once("\n") {
        task.title = title.to_string();
        task.body = body.to_string();
        task.save()?;
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

fn command_find(dir: PathBuf, full_id: bool, find_args: FindArgs) -> Result<()> {
    let id = Workspace::from_path(dir)?.search(None, find_args.search_body, false)?;
    if let Some(id) = id {
        if full_id {
            println!("{id}");
        } else {
            // print as integer
            println!("{}", id.0);
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

fn command_show(dir: PathBuf, task_id: TaskId, show_attrs: bool) -> Result<()> {
    let task = Workspace::from_path(dir)?.task(task_id.into())?;
    // YAML front-matter style. YAML is gross, but it's what everyone uses!
    if show_attrs {
        println!("---");
        for (attr, value) in task.attributes.iter() {
            println!("{attr}: \"{value}\"");
        }
        println!("---");
    }
    println!("{task}");
    Ok(())
}
