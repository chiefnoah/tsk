use crate::errors::{Error, Result};
use std::ffi::OsStr;
use std::fmt::Display;
use std::io::Write;
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
    let mut command = Command::new("fzf");
    let mut child = command
        .args(extra)
        .arg("--read0")
        .stderr(Stdio::inherit())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    // unwrap: this can never fail
    let child_in = child.stdin.as_mut().unwrap();
    for item in input.into_iter() {
        write!(child_in, "{item}\0")?;
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
