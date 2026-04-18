#![allow(dead_code)]
use nix::fcntl::{Flock, FlockArg};
use xattr::FileExt;

use crate::attrs::Attrs;
use crate::errors::{Error, Result};
use crate::stack::{StackItem, TaskStack};
use crate::task::parse as parse_task;
use crate::{fzf, util};
use std::collections::{vec_deque, BTreeMap, HashSet};
use std::ffi::OsString;
use std::fmt::Display;
use std::fs::{remove_file, File};
use std::io::{BufRead as _, BufReader, Read, Seek, SeekFrom};
use std::ops::Deref;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::{fs::OpenOptions, io::Write};

const INDEXFILE: &str = "index";
const TITLECACHEFILE: &str = "cache";
const XATTRPREFIX: &str = "user.tsk.";
const BACKREFXATTR: &str = "user.tsk.references";
/// A unique identifier for a task. When referenced in text, it is prefixed with `tsk-`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Id(pub u32);

impl FromStr for Id {
    type Err = Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let upper = s.to_uppercase();
        let s = upper
            .trim()
            .strip_prefix("TSK-")
            .ok_or(Self::Err::Parse(format!("expected tsk- prefix. Got {s}")))?;
        Ok(Self(s.parse()?))
    }
}

impl Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tsk-{}", self.0)
    }
}

impl From<u32> for Id {
    fn from(value: u32) -> Self {
        Id(value)
    }
}

impl Id {
    /// Returns the filename for a task with this id.
    pub fn filename(&self) -> String {
        format!("tsk-{}.tsk", self.0)
    }
}

pub enum TaskIdentifier {
    Id(Id),
    Relative(u32),
    Find { exclude_body: bool, archived: bool },
}

impl From<Id> for TaskIdentifier {
    fn from(value: Id) -> Self {
        TaskIdentifier::Id(value)
    }
}

pub struct Workspace {
    /// The path to the workspace root, excluding the .tsk directory. This should *contain* the
    /// .tsk directory.
    path: PathBuf,
}

impl Workspace {
    pub fn init(path: PathBuf) -> Result<()> {
        // TODO: detect if in a git repo and add .tsk/ to `.git/info/exclude`
        let tsk_dir = path.join(".tsk");
        if tsk_dir.exists() {
            return Err(Error::AlreadyInitialized);
        }
        std::fs::create_dir(&tsk_dir)?;
        // Create the tasks directory
        std::fs::create_dir(tsk_dir.join("tasks"))?;
        // Create the archive directory
        std::fs::create_dir(tsk_dir.join("archive"))?;
        let mut next = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(tsk_dir.join("next"))?;
        // initialize the next file with ID 1
        next.write_all(b"1\n")?;
        Ok(())
    }

    pub fn from_path(path: PathBuf) -> Result<Self> {
        let tsk_dir = util::find_parent_with_dir(path, ".tsk")?.ok_or(Error::Uninitialized)?;
        Ok(Self { path: tsk_dir })
    }

    fn resolve(&self, identifier: TaskIdentifier) -> Result<Id> {
        match identifier {
            TaskIdentifier::Id(id) => Ok(id),
            TaskIdentifier::Relative(r) => {
                let stack = self.read_stack()?;
                let stack_item = stack.get(r as usize).ok_or(Error::NoTasks)?;
                Ok(stack_item.id)
            }
            TaskIdentifier::Find {
                exclude_body,
                archived,
            } => self
                .search(None, !exclude_body, archived)?
                .ok_or(Error::NotSelected),
        }
    }

    /// Increments the `next` counter and returns the previous value.
    pub fn next_id(&self) -> Result<Id> {
        let mut file = util::flopen(self.path.join("next"), FlockArg::LockExclusive)?;
        let mut buf = String::new();
        file.read_to_string(&mut buf)?;
        let id = buf.trim().parse::<u32>()?;
        // reset the files contents
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        // store the *next* if
        file.write_all(format!("{}\n", id + 1).as_bytes())?;
        Ok(Id(id))
    }

    pub fn new_task(&self, title: String, body: String) -> Result<Task> {
        // WARN: we could improperly increment the id if the task is not written to disk/errors.
        // But who cares
        let id = self.next_id()?;
        let task_name = format!("tsk-{}.tsk", id.0);
        // the task goes in the archive first
        let task_path = self.path.join("archive").join(&task_name);
        let mut file = util::flopen(task_path.clone(), FlockArg::LockExclusive)?;
        file.write_all(format!("{title}\n\n{body}").as_bytes())?;
        // create a hardlink to the task dir to mark it as "open"
        symlink(
            PathBuf::from("../archive").join(&task_name),
            self.path.join("tasks").join(task_name),
        )?;
        Ok(Task {
            id,
            title,
            body,
            file,
            attributes: Default::default(),
        })
    }

    pub fn task(&self, identifier: TaskIdentifier) -> Result<Task> {
        let id = self.resolve(identifier)?;

        let file = util::flopen(
            self.path.join("tasks").join(format!("tsk-{}.tsk", id.0)),
            FlockArg::LockExclusive,
        )?;
        let mut title = String::new();
        let mut body = String::new();
        let mut reader = BufReader::new(&*file);
        reader.read_line(&mut title)?;
        reader.read_to_string(&mut body)?;
        drop(reader);
        let mut read_attributes = BTreeMap::new();
        if let Ok(attrs) = file.list_xattr() {
            for attr in attrs {
                if let Some((key, value)) = Self::read_xattr(&file, attr) {
                    read_attributes.insert(key, value);
                }
            }
        }
        Ok(Task {
            id,
            file,
            title: title.trim().to_string(),
            body: body.trim().to_string(),
            attributes: Attrs::from_written(read_attributes),
        })
    }

    pub fn handle_metadata(&self, tsk: &Task, pre_links: Option<HashSet<Id>>) -> Result<()> {
        // Parse the task and update any backlinks
        if let Some(parsed_task) = parse_task(&tsk.to_string()) {
            let internal_links = parsed_task.intenal_links();
            for link in &internal_links {
                self.add_backlink(*link, tsk.id)?;
            }
            if let Some(pre_links) = pre_links {
                let removed_links = pre_links.difference(&internal_links);
                for link in removed_links {
                    self.remove_backlink(*link, tsk.id)?;
                }
            }
        }
        Ok(())
    }

    fn add_backlink(&self, to: Id, from: Id) -> Result<()> {
        let to_task = self.task(TaskIdentifier::Id(to))?;
        let (_, current_backlinks_text) =
            Self::read_xattr(&to_task.file, BACKREFXATTR.into()).unwrap_or_default();
        let mut backlinks: HashSet<Id> = current_backlinks_text
            .split(',')
            .filter_map(|s| Id::from_str(s).ok())
            .collect();
        backlinks.insert(from);
        Self::set_xattr(
            &to_task.file,
            BACKREFXATTR,
            &itertools::join(backlinks, ","),
        )
    }

    fn remove_backlink(&self, to: Id, from: Id) -> Result<()> {
        let to_task = self.task(TaskIdentifier::Id(to))?;
        let (_, current_backlinks_text) =
            Self::read_xattr(&to_task.file, BACKREFXATTR.into()).unwrap_or_default();
        let mut backlinks: HashSet<Id> = current_backlinks_text
            .split(',')
            .filter_map(|s| Id::from_str(s).ok())
            .collect();
        backlinks.remove(&from);
        Self::set_xattr(
            &to_task.file,
            BACKREFXATTR,
            &itertools::join(backlinks, ","),
        )
    }

    /// Reads an xattr from a file, stripping the prefix for
    fn read_xattr<D: Deref<Target = File>>(file: &D, key: OsString) -> Option<(String, String)> {
        // this *shouldn't* allocate, but it does O(n) scan the str for UTF-8 correctness
        let parsedkey = key.as_os_str().to_str()?.strip_prefix(XATTRPREFIX)?;
        let valuebytes = file.get_xattr(&key).ok().flatten()?;
        Some((parsedkey.to_string(), String::from_utf8(valuebytes).ok()?))
    }

    fn set_xattr<D: Deref<Target = File>>(file: &D, key: &str, value: &str) -> Result<()> {
        let key = if !key.starts_with(XATTRPREFIX) {
            format!("{XATTRPREFIX}.{key}")
        } else {
            key.to_string()
        };
        Ok(file.set_xattr(key, value.as_bytes())?)
    }

    pub fn read_stack(&self) -> Result<TaskStack> {
        TaskStack::from_tskdir(&self.path)
    }

    pub fn push_task(&self, task: Task) -> Result<()> {
        let mut stack = TaskStack::from_tskdir(&self.path)?;
        stack.push(task.try_into()?);
        stack.save()?;
        Ok(())
    }

    pub fn append_task(&self, task: Task) -> Result<()> {
        let mut stack = TaskStack::from_tskdir(&self.path)?;
        stack.push_back(task.try_into()?);
        stack.save()?;
        Ok(())
    }

    pub fn swap_top(&self) -> Result<()> {
        let mut stack = TaskStack::from_tskdir(&self.path)?;
        stack.swap();
        stack.save()?;
        Ok(())
    }

    pub fn rot(&self) -> Result<()> {
        let mut stack = TaskStack::from_tskdir(&self.path)?;
        let top = stack.pop();
        let second = stack.pop();
        let third = stack.pop();

        if top.is_none() || second.is_none() || third.is_none() {
            return Ok(());
        }

        // unwrap is ok here because we checked above
        stack.push(second.unwrap());
        stack.push(top.unwrap());
        stack.push(third.unwrap());
        stack.save()?;
        Ok(())
    }

    /// The inverse of tor. Pushes the top item behind the second item, shifting #2 and #3 to #1
    /// and #2 respectively.
    pub fn tor(&self) -> Result<()> {
        let mut stack = TaskStack::from_tskdir(&self.path)?;
        let top = stack.pop();
        let second = stack.pop();
        let third = stack.pop();

        if top.is_none() || second.is_none() || third.is_none() {
            return Ok(());
        }

        stack.push(top.unwrap());
        stack.push(third.unwrap());
        stack.push(second.unwrap());
        stack.save()?;
        Ok(())
    }

    pub fn drop(&self, identifier: TaskIdentifier) -> Result<Option<Id>> {
        let id = self.resolve(identifier)?;
        let mut stack = self.read_stack()?;
        let index = &stack.iter().map(|i| i.id).position(|i| i == id);
        // TODO: remove the softlink in .tsk/tasks
        let task = if let Some(index) = index {
            let prioritized_task = stack.remove(*index);
            stack.save()?;
            prioritized_task.map(|t| t.id)
        } else {
            None
        };
        remove_file(self.path.join("tasks").join(format!("{id}.tsk")))?;
        Ok(task)
    }

    pub fn search(
        &self,
        stack: Option<TaskStack>,
        search_body: bool,
        _include_archived: bool,
    ) -> Result<Option<Id>> {
        let stack = if let Some(stack) = stack {
            stack
        } else {
            self.read_stack()?
        };
        if search_body {
            let loader = LazyTaskLoader {
                files: stack.into_iter(),
                workspace: self,
            };
            // search the entirety of a task
            Ok(fzf::select::<_, Id, _>(
                loader,
                [
                    "--no-multi-line",
                    "--accept-nth=1",
                    "--delimiter=\t",
                    "--preview=tsk show -T {1}",
                    "--preview-window=top",
                    "--ansi",
                    "--info-command=tsk show -T {1} | head -n1",
                    "--info=inline-right",
                ],
            )?)
        } else {
            // just search the stack
            Ok(fzf::select::<_, Id, _>(
                stack,
                ["--delimiter=\t", "--accept-nth=1"],
            )?)
        }
    }

    pub fn prioritize(&self, identifier: TaskIdentifier) -> Result<()> {
        let id = self.resolve(identifier)?;
        let mut stack = self.read_stack()?;
        let index = &stack.iter().map(|i| i.id).position(|i| i == id);
        if let Some(index) = index {
            let prioritized_task = stack.remove(*index);
            // unwrap here is safe because we just searched for the index and know it exists
            stack.push(prioritized_task.unwrap());
            stack.save()?;
        }
        Ok(())
    }

    pub fn deprioritize(&self, identifier: TaskIdentifier) -> Result<()> {
        let id = self.resolve(identifier)?;
        let mut stack = self.read_stack()?;
        let index = &stack.iter().map(|i| i.id).position(|i| i == id);
        if let Some(index) = index {
            let deprioritized_task = stack.remove(*index);
            // unwrap here is safe because we just searched for the index and know it exists
            stack.push_back(deprioritized_task.unwrap());
            stack.save()?;
        }
        Ok(())
    }

    pub fn clean(&self) -> Result<()> {
        let stack = self.read_stack()?;
        let indexed_ids: HashSet<Id> = stack.iter().map(|item| item.id).collect();

        let tasks_dir = self.path.join("tasks");
        if !tasks_dir.exists() {
            return Ok(());
        }

        for entry in std::fs::read_dir(&tasks_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let filename = entry.file_name();
            let filename_str = filename.to_string_lossy();
            if let Some(id_str) = filename_str
                .strip_prefix("tsk-")
                .and_then(|s| s.strip_suffix(".tsk"))
            {
                if let Ok(id_num) = id_str.parse::<u32>() {
                    let id = Id(id_num);
                    if !indexed_ids.contains(&id) {
                        remove_file(&path)?;
                        eprintln!("Removed orphaned task: {id}");
                    }
                }
            }
        }
        Ok(())
    }
}

pub struct Task {
    pub id: Id,
    pub title: String,
    pub body: String,
    pub file: Flock<File>,
    pub attributes: Attrs,
}

impl Display for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n\n{}", self.title, &self.body)
    }
}

impl Task {
    /// Consumes a task and saves it to disk.
    pub fn save(mut self) -> Result<()> {
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(self.title.trim().as_bytes())?;
        self.file.write_all(b"\n\n")?;
        self.file.write_all(self.body.trim().as_bytes())?;
        Ok(())
    }

    /// Returns a [`SearchTas`] which is plain task data with no file or attrs
    fn bare(self) -> SearchTask {
        SearchTask {
            id: self.id,
            title: self.title,
            body: self.body,
        }
    }
}

/// A task container without a file handle
pub struct SearchTask {
    pub id: Id,
    pub title: String,
    pub body: String,
}

impl Display for SearchTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\t{}", self.id, self.title.trim())?;
        if !self.body.is_empty() {
            write!(f, "\n\n{}", self.body)?;
        }
        Ok(())
    }
}

struct LazyTaskLoader<'a> {
    files: vec_deque::IntoIter<StackItem>,
    workspace: &'a Workspace,
}

impl Iterator for LazyTaskLoader<'_> {
    type Item = SearchTask;

    fn next(&mut self) -> Option<Self::Item> {
        let stack_item = self.files.next()?;
        let task = self
            .workspace
            .task(TaskIdentifier::Id(stack_item.id))
            .ok()?;
        Some(task.bare())
    }
}

fn select_task(input: impl IntoIterator<Item = SearchTask>) -> Result<Option<Id>> {
    let mut child = Command::new("cat")
        .stderr(Stdio::inherit())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let child_in = child.stdin.as_mut().unwrap();
    for item in input.into_iter() {
        writeln!(child_in, "{item}\0")?;
    }
    let output = child.wait_with_output()?;
    if output.stdout.is_empty() {
        Ok(None)
    } else {
        Ok(Some(String::from_utf8(output.stdout)?.parse()?))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_bare_task_display() {
        let task = SearchTask {
            id: Id(123),
            title: "Hello, world".to_string(),
            body: "The body of the task.\nAnother line is here.".to_string(),
        };
        assert_eq!(
            "tsk-123\tHello, world\n\nThe body of the task.\nAnother line is here.",
            task.to_string()
        );
    }

    #[test]
    fn test_task_display() {
        let task = Task {
            id: Id(123),
            title: "Hello, world".to_string(),
            body: "The body of the task.".to_string(),
            file: util::flopen("/dev/null".into(), FlockArg::LockShared).unwrap(),
            attributes: Default::default(),
        };
        assert_eq!("Hello, world\n\nThe body of the task.", task.to_string());
    }
}
