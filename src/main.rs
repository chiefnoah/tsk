mod errors;
mod stack;
mod util;
mod workspace;
use std::path::PathBuf;
use std::{env::current_dir, io::Read};
use clap_complete::Shell;
use workspace::Workspace;

//use smol;
//use iocraft::prelude::*;
use clap::{Args, Parser, Subcommand};
use edit::edit as open_editor;

fn default_dir() -> PathBuf {
    current_dir().unwrap()
}

#[derive(Parser)]
// TODO: add long_about
#[command(version, about)]
struct Cli {
    #[arg(short = 'C', env = "TSK_ROOT", value_name = "DIR")]
    dir: Option<PathBuf>,
    // TODO: other global options
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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
    List {
        /// Whether to list all tasks in the task stack. If specified, -c / count is ignored.
        #[arg(short = 'a', default_value_t = false)]
        all: bool,
        #[arg(short = 'c', default_value_t = 10)]
        count: usize,
    },

    Swap,

    Edit {
        #[arg(short = 't')]
        task_id: u32
    },

    Completion {
        #[arg(short = 's')]
        shell: Shell
    }
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

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init => command_init(cli.dir.unwrap_or(default_dir())),
        Commands::Push { edit, body, title } => {
            command_push(cli.dir.unwrap_or(default_dir()), edit, body, title)
        }
        Commands::List { all, count } => command_list(cli.dir.unwrap_or(default_dir()), all, count),
        Commands::Swap => command_swap(cli.dir.unwrap_or(default_dir()))
    }
}

fn command_init(dir: PathBuf) {
    Workspace::init(dir).expect("Init failed")
}

fn command_push(dir: PathBuf, edit: bool, body: Option<String>, title: Title) {
    let workspace = Workspace::from_path(dir).expect("Unable to find .tsk dir");
    let title = if let Some(title) = title.title {
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
        eprintln!("");
        body.clear();
        std::io::stdin()
            .read_to_string(&mut body)
            .expect("Failed to read stdin");
    }
    if edit {
        body = open_editor(format!("{title}\n\n{body}")).expect("Failed to edit file");
    }
    let task = workspace
        .new_task(title, body)
        .expect("Failed to create task");
    workspace
        .push_task(task)
        .expect("Failed to push task to stack");
}

fn command_list(dir: PathBuf, all: bool, count: usize) {
    let workspace = Workspace::from_path(dir).expect("Unable to find .tsk dir");
    let stack = if all {
        workspace.read_stack(None).expect("Failed to read index")
    } else {
        workspace
            .read_stack(Some(count))
            .expect("Failed to read index")
    };
    if stack.empty() {
        println!("*No tasks*");
    } else {
        println!("{}", stack);
    }
}

fn command_swap(dir: PathBuf) {
    let workspace = Workspace::from_path(dir).expect("Unable to find .tsk dir");
    workspace.swap_top().expect("swap to work");
}

fn command_edit(dir: PathBuf) {
    let workspace = Workspace::from_path(dir).expect("Unable to find .tsk dir");
    let task = workspace.
}
