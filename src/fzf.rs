use crate::errors::{Error, Result};
use std::fmt::Display;
use std::io::Write;
use std::process::{Command, Stdio};
use std::str::FromStr;

/// Sends each item as a line to stdin to the `fzf` command and returns the selected item's string
/// representation as output
pub fn select<I>(input: impl IntoIterator<Item = I>) -> Result<Option<I>>
where
    I: Display + FromStr,
    Error: From<<I as FromStr>::Err>,
{
    let mut child = Command::new("fzf")
        .args(["-d", "\t"])
        .stderr(Stdio::inherit())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    // unwrap: this can never fail
    let child_in = child.stdin.as_mut().unwrap();
    for item in input.into_iter() {
        write!(child_in, "{}\n", item.to_string())?;
    }
    let output = child.wait_with_output()?;
    if output.stdout.is_empty() {
        Ok(None)
    } else {
        Ok(Some(String::from_utf8(output.stdout)?.parse()?))
    }
}
