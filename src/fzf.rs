use crate::errors::{Error, Result};
use std::ffi::OsStr;
use std::fmt::Display;
use std::io::{IsTerminal, Write};
use std::process::{Command, Stdio};
use std::str::FromStr;

/// Sends each item as a line to stdin to the `fzf` command and returns the selected item's string
/// representation as output
pub fn select<I, O, S>(
    input: impl IntoIterator<Item = I>,
    extra: impl IntoIterator<Item = S>,
) -> Result<Option<O>>
where
    O: FromStr,
    I: Display,
    Error: From<<O as FromStr>::Err>,
    S: AsRef<OsStr>,
{
    ensure_interactive()?;
    let mut command = Command::new("fzf");
    let mut child = command
        .args(extra)
        .arg("--read0")
        .stderr(Stdio::inherit())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    {
        // unwrap: this can never fail
        let mut child_in = child.stdin.take().unwrap();
        for item in input.into_iter() {
            write!(child_in, "{item}\0")?;
        }
    }
    let output = child.wait_with_output()?;
    if output.stdout.is_empty() {
        Ok(None)
    } else {
        // fzf appends a trailing newline; strip it so the FromStr impls
        // (Id, String, usize, etc.) all work.
        let raw = String::from_utf8(output.stdout)?;
        Ok(Some(raw.trim().parse()?))
    }
}

/// Sends NUL-delimited display lines to fzf and returns every selected raw line.
pub fn select_raw<I, S>(
    input: impl IntoIterator<Item = I>,
    extra: impl IntoIterator<Item = S>,
) -> Result<Vec<String>>
where
    I: Display,
    S: AsRef<OsStr>,
{
    ensure_interactive()?;
    let mut command = Command::new("fzf");
    let mut child = command
        .args(extra)
        .arg("--read0")
        .arg("--print0")
        .stderr(Stdio::inherit())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    {
        let mut child_in = child.stdin.take().unwrap();
        for item in input.into_iter() {
            write!(child_in, "{item}\0")?;
        }
    }
    let output = child.wait_with_output()?;
    if output.stdout.is_empty() {
        return Ok(Vec::new());
    }
    let raw = String::from_utf8(output.stdout)?;
    Ok(raw
        .split('\0')
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect())
}

fn ensure_interactive() -> Result<()> {
    if std::io::stdin().is_terminal() || std::env::var_os("TSK_TEST_ALLOW_FZF").is_some() {
        Ok(())
    } else {
        Err(Error::Parse(
            "refusing to launch fzf without an interactive terminal".into(),
        ))
    }
}
