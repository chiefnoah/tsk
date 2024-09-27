use std::path::PathBuf;
use std::{env::current_dir, io::Read};

//use smol;
//use iocraft::prelude::*;
use clap::{Args, Parser, Subcommand};
use edit::edit as open_editor;

fn default_dir() -> PathBuf {
    current_dir().unwrap()
}

#[derive(Parser)]
// TODO: add long_about
#[command(version, about, long_about = None)]
struct Cli {
    #[arg(short = 'C', env = "TSK_DIR", value_name = "DIR")]
    dir: Option<PathBuf>,
    // TODO: other global options
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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
    if let Commands::Push { edit, body, title } = cli.command {
        let title = if let Some(title) = title.title {
            eprintln!("TITLE: {}", title);
            title
        } else if let Some(title) = title.title_simple {
            let joined = title.join(" ");
            eprintln!("TITLE simple: {}", joined);
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
        eprintln!("BODY: {body}");
    }
}
