#![allow(dead_code)]
//! High-level workspace API. The workspace owns a [`Store`](crate::backend::Store)
//! and exposes typed task / stack / remote operations on top of it.

use crate::attrs::Attrs;
use crate::backend::{self, Loc, Store};
use crate::errors::{Error, Result};
use crate::stack::{StackItem, TaskStack};
use crate::task::parse as parse_task;
use crate::{fzf, util};
use std::collections::{BTreeMap, HashSet, vec_deque};
use std::fmt::Display;
use std::path::PathBuf;
use std::str::FromStr;

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Remote {
    pub prefix: String,
    pub path: PathBuf,
}

impl Display for Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\t{}", self.prefix, self.path.display())
    }
}

pub struct Workspace {
    /// The path to the .tsk marker directory.
    pub path: PathBuf,
    store: Box<dyn Store>,
}

impl Workspace {
    pub fn init(path: PathBuf) -> Result<()> {
        let tsk_dir = path.join(".tsk");
        if tsk_dir.exists() {
            return Err(Error::AlreadyInitialized);
        }
        std::fs::create_dir(&tsk_dir)?;
        // If we're in a git repo, mark this workspace as git-backed and use refs
        // for storage. Otherwise fall back to the file backend (tasks live under
        // .tsk/).
        if let Some(git_dir) = backend::detect_git_dir(&path) {
            std::fs::write(
                tsk_dir.join(backend::GIT_BACKED_MARKER),
                git_dir.to_string_lossy().as_bytes(),
            )?;
            // GitStore is fully ref-based — no on-disk task data.
        } else {
            // Pre-create directory tree for the file backend.
            std::fs::create_dir(tsk_dir.join("tasks"))?;
            std::fs::create_dir(tsk_dir.join("archive"))?;
            std::fs::write(tsk_dir.join("next"), b"1\n")?;
        }
        Ok(())
    }

    pub fn from_path(path: PathBuf) -> Result<Self> {
        let tsk_dir = util::find_parent_with_dir(path, ".tsk")?.ok_or(Error::Uninitialized)?;
        let store = backend::store_for(&tsk_dir)?;
        Ok(Self {
            path: tsk_dir,
            store,
        })
    }

    pub fn store(&self) -> &dyn Store {
        self.store.as_ref()
    }

    pub fn is_git_backed(&self) -> bool {
        self.path.join(backend::GIT_BACKED_MARKER).exists()
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

    pub fn next_id(&self) -> Result<Id> {
        backend::next_id(self.store())
    }

    pub fn new_task(&self, title: String, body: String) -> Result<Task> {
        let id = self.next_id()?;
        backend::write_task(self.store(), id, &title, &body, Loc::Active)?;
        Ok(Task {
            id,
            title,
            body,
            attributes: Default::default(),
        })
    }

    pub fn task(&self, identifier: TaskIdentifier) -> Result<Task> {
        let id = self.resolve(identifier)?;
        let (title, body, _loc) = backend::read_task(self.store(), id)?
            .ok_or_else(|| Error::Parse(format!("Task {id} not found")))?;
        let attrs_map = backend::read_attrs(self.store(), id)?;
        Ok(Task {
            id,
            title,
            body,
            attributes: Attrs::from_written(attrs_map),
        })
    }

    pub fn save_task(&self, task: &Task) -> Result<()> {
        let loc = match backend::task_location(self.store(), task.id)? {
            Some(l) => l,
            None => Loc::Active,
        };
        backend::write_task(self.store(), task.id, &task.title, &task.body, loc)?;
        // Persist any modified attrs.
        let mut combined: BTreeMap<String, String> = task.attributes.written.clone();
        for (k, v) in task.attributes.updated.iter() {
            combined.insert(k.clone(), v.clone());
        }
        backend::write_attrs(self.store(), task.id, &combined)?;
        // After editing, refresh stack title for this id.
        self.update_stack_title(task.id, &task.title)?;
        Ok(())
    }

    fn update_stack_title(&self, id: Id, title: &str) -> Result<()> {
        let mut stack = self.read_stack()?;
        let mut changed = false;
        for item in stack.all.iter_mut() {
            if item.id == id {
                item.title = title.replace('\t', " ");
                changed = true;
            }
        }
        if changed {
            stack.save(self.store())?;
        }
        Ok(())
    }

    pub fn handle_metadata(&self, tsk: &Task, pre_links: Option<HashSet<Id>>) -> Result<()> {
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
        let mut links = backend::read_backlinks(self.store(), to)?;
        links.insert(from);
        backend::write_backlinks(self.store(), to, &links)
    }

    fn remove_backlink(&self, to: Id, from: Id) -> Result<()> {
        let mut links = backend::read_backlinks(self.store(), to)?;
        links.remove(&from);
        backend::write_backlinks(self.store(), to, &links)
    }

    pub fn read_stack(&self) -> Result<TaskStack> {
        TaskStack::load(self.store())
    }

    pub fn push_task(&self, task: Task) -> Result<()> {
        let mut stack = self.read_stack()?;
        stack.push((&task).into());
        stack.save(self.store())
    }

    pub fn append_task(&self, task: Task) -> Result<()> {
        let mut stack = self.read_stack()?;
        stack.push_back((&task).into());
        stack.save(self.store())
    }

    pub fn swap_top(&self) -> Result<()> {
        let mut stack = self.read_stack()?;
        stack.swap();
        stack.save(self.store())
    }

    pub fn rot(&self) -> Result<()> {
        let mut stack = self.read_stack()?;
        let (a, b, c) = (stack.pop(), stack.pop(), stack.pop());
        if let (Some(a), Some(b), Some(c)) = (a, b, c) {
            stack.push(b);
            stack.push(a);
            stack.push(c);
            stack.save(self.store())?;
        }
        Ok(())
    }

    pub fn tor(&self) -> Result<()> {
        let mut stack = self.read_stack()?;
        let (a, b, c) = (stack.pop(), stack.pop(), stack.pop());
        if let (Some(a), Some(b), Some(c)) = (a, b, c) {
            stack.push(a);
            stack.push(c);
            stack.push(b);
            stack.save(self.store())?;
        }
        Ok(())
    }

    pub fn drop(&self, identifier: TaskIdentifier) -> Result<Option<Id>> {
        let id = self.resolve(identifier)?;
        let mut stack = self.read_stack()?;
        let removed = if let Some(idx) = stack.position(id) {
            let item = stack.remove(idx);
            stack.save(self.store())?;
            item.map(|t| t.id)
        } else {
            None
        };
        // Move the task content to the archive bucket.
        if backend::task_location(self.store(), id)? == Some(Loc::Active) {
            backend::move_task(self.store(), id, Loc::Archived)?;
        }
        Ok(removed)
    }

    pub fn search(
        &self,
        stack: Option<TaskStack>,
        search_body: bool,
        include_archived: bool,
    ) -> Result<Option<Id>> {
        let stack = if let Some(stack) = stack {
            stack
        } else {
            self.read_stack()?
        };
        if include_archived {
            let mut all_tasks: Vec<SearchTask> = Vec::new();
            let mut seen: HashSet<Id> = HashSet::new();
            for item in stack.iter() {
                if let Ok(t) = self.task(TaskIdentifier::Id(item.id)) {
                    seen.insert(t.id);
                    all_tasks.push(t.bare());
                }
            }
            for id in backend::list_archive(self.store())? {
                if seen.contains(&id) {
                    continue;
                }
                if let Some((title, body, _)) = backend::read_task(self.store(), id)? {
                    all_tasks.push(SearchTask { id, title, body });
                }
            }
            if search_body {
                Ok(fzf::select::<_, Id, _>(
                    all_tasks,
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
                Ok(fzf::select::<_, Id, _>(
                    all_tasks,
                    ["--delimiter=\t", "--accept-nth=1"],
                )?)
            }
        } else if search_body {
            let loader = LazyTaskLoader {
                items: stack.into_iter(),
                workspace: self,
            };
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
            Ok(fzf::select::<_, Id, _>(
                stack,
                ["--delimiter=\t", "--accept-nth=1"],
            )?)
        }
    }

    pub fn prioritize(&self, identifier: TaskIdentifier) -> Result<()> {
        let id = self.resolve(identifier)?;
        let mut stack = self.read_stack()?;
        if let Some(idx) = stack.position(id) {
            let task = stack.remove(idx).unwrap();
            stack.push(task);
            stack.save(self.store())?;
        }
        Ok(())
    }

    pub fn deprioritize(&self, identifier: TaskIdentifier) -> Result<()> {
        let id = self.resolve(identifier)?;
        let mut stack = self.read_stack()?;
        if let Some(idx) = stack.position(id) {
            let task = stack.remove(idx).unwrap();
            stack.push_back(task);
            stack.save(self.store())?;
        }
        Ok(())
    }

    /// Remove "active" task entries that aren't in the index.
    pub fn clean(&self) -> Result<()> {
        let stack = self.read_stack()?;
        let indexed: HashSet<Id> = stack.iter().map(|i| i.id).collect();
        for id in backend::list_active(self.store())? {
            if !indexed.contains(&id) {
                // Move orphan to archive rather than delete, to avoid data loss.
                backend::move_task(self.store(), id, Loc::Archived)?;
                eprintln!("Removed orphaned task: {id}");
            }
        }
        Ok(())
    }

    pub fn read_remotes(&self) -> Result<Vec<Remote>> {
        backend::read_remotes(self.store())
    }

    pub fn add_remote(&self, prefix: &str, path: &str) -> Result<()> {
        let mut remotes = self.read_remotes()?;
        if remotes.iter().any(|r| r.prefix == prefix) {
            return Err(Error::Parse(format!("Remote '{prefix}' already exists")));
        }
        remotes.push(Remote {
            prefix: prefix.to_string(),
            path: PathBuf::from(path),
        });
        backend::write_remotes(self.store(), &remotes)
    }

    pub fn remove_remote(&self, prefix: &str) -> Result<()> {
        let remotes = self.read_remotes()?;
        let len = remotes.len();
        let new_remotes: Vec<Remote> = remotes.into_iter().filter(|r| r.prefix != prefix).collect();
        if new_remotes.len() == len {
            return Err(Error::Parse(format!("Remote '{prefix}' not found")));
        }
        backend::write_remotes(self.store(), &new_remotes)
    }

    pub fn resolve_foreign_link(&self, prefix: &str, id: u32) -> Result<Option<Task>> {
        let remotes = self.read_remotes()?;
        let remote = remotes
            .iter()
            .find(|r| r.prefix == prefix)
            .ok_or_else(|| Error::Parse(format!("Unknown remote prefix: {prefix}")))?;
        let workspace = Workspace::from_path(remote.path.clone())?;
        let task = workspace.task(TaskIdentifier::Id(Id(id)))?;
        Ok(Some(task))
    }

    /// Migrate a file-backed workspace to a git-backed one. Returns Err if the
    /// workspace is already git-backed or if no enclosing git repo is found.
    /// All blobs are copied into refs/tsk/* and the on-disk task data is then
    /// removed, leaving only the `.tsk/git-backed` marker.
    pub fn migrate_to_git(&self) -> Result<PathBuf> {
        if self.is_git_backed() {
            return Err(Error::Parse("Workspace is already git-backed".into()));
        }
        let git_dir = backend::detect_git_dir(&self.path)
            .ok_or_else(|| Error::Parse("No enclosing git repository found".into()))?;
        let dest = backend::GitStore::open(git_dir.clone())?;
        // Copy every logical blob across.
        let prefixes = ["tasks", "archive", "attrs", "backlinks"];
        for prefix in prefixes {
            for key in self.store().list(prefix)? {
                if let Some(data) = self.store().read(&key)? {
                    dest.write(&key, &data)?;
                }
            }
        }
        for top in ["index", "next", "remotes"] {
            if let Some(data) = self.store().read(top)? {
                dest.write(top, &data)?;
            }
        }
        // Drop on-disk file backend state: everything under .tsk/ except the
        // marker we're about to write.
        for entry in std::fs::read_dir(&self.path)? {
            let entry = entry?;
            let p = entry.path();
            if p.is_dir() {
                std::fs::remove_dir_all(&p)?;
            } else {
                std::fs::remove_file(&p)?;
            }
        }
        std::fs::write(
            self.path.join(backend::GIT_BACKED_MARKER),
            git_dir.to_string_lossy().as_bytes(),
        )?;
        Ok(git_dir)
    }

    pub fn reopen(&self, identifier: TaskIdentifier) -> Result<Id> {
        let id = self.resolve(identifier)?;
        match backend::task_location(self.store(), id)? {
            None => return Err(Error::Parse(format!("Task {id} not found in archive"))),
            Some(Loc::Active) => return Err(Error::Parse(format!("Task {id} is already open"))),
            Some(Loc::Archived) => {}
        }
        backend::move_task(self.store(), id, Loc::Active)?;
        let (title, _, _) = backend::read_task(self.store(), id)?
            .ok_or_else(|| Error::Parse(format!("Task {id} content missing after move")))?;
        let mut stack = self.read_stack()?;
        stack.push(StackItem {
            id,
            title: title.replace('\t', " "),
            modify_time: std::time::SystemTime::now(),
        });
        stack.save(self.store())?;
        Ok(id)
    }
}

pub struct Task {
    pub id: Id,
    pub title: String,
    pub body: String,
    pub attributes: Attrs,
}

impl Display for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n\n{}", self.title, &self.body)
    }
}

impl Task {
    fn bare(self) -> SearchTask {
        SearchTask {
            id: self.id,
            title: self.title,
            body: self.body,
        }
    }
}

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
    items: vec_deque::IntoIter<StackItem>,
    workspace: &'a Workspace,
}

impl Iterator for LazyTaskLoader<'_> {
    type Item = SearchTask;
    fn next(&mut self) -> Option<Self::Item> {
        let item = self.items.next()?;
        let task = self.workspace.task(TaskIdentifier::Id(item.id)).ok()?;
        Some(task.bare())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn run_git_init(dir: &std::path::Path) {
        let s = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(s.success());
    }

    /// Create both a file-backed and a git-backed workspace for the same test.
    fn setup_dual() -> (tempfile::TempDir, Workspace, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let file_root = dir.path().join("file");
        let git_root = dir.path().join("git");
        std::fs::create_dir_all(&file_root).unwrap();
        std::fs::create_dir_all(&git_root).unwrap();
        run_git_init(&git_root);
        Workspace::init(file_root.clone()).unwrap();
        Workspace::init(git_root.clone()).unwrap();
        let f = Workspace::from_path(file_root).unwrap();
        let g = Workspace::from_path(git_root).unwrap();
        assert!(
            !f.is_git_backed(),
            "file workspace should not be git-backed"
        );
        assert!(g.is_git_backed(), "git workspace should be git-backed");
        (dir, f, g)
    }

    fn run_full_lifecycle(ws: &Workspace) {
        // Push two tasks, drop one, verify state.
        let t1 = ws
            .new_task("First".to_string(), "body one".to_string())
            .unwrap();
        let id1 = t1.id;
        ws.push_task(t1).unwrap();
        let t2 = ws
            .new_task("Second".to_string(), "body two".to_string())
            .unwrap();
        let id2 = t2.id;
        ws.push_task(t2).unwrap();

        let stack = ws.read_stack().unwrap();
        assert_eq!(stack.iter().count(), 2);
        assert_eq!(stack.iter().next().unwrap().id, id2, "newest on top");

        // Read back the task content.
        let read = ws.task(TaskIdentifier::Id(id1)).unwrap();
        assert_eq!(read.title, "First");
        assert_eq!(read.body, "body one");

        // Drop top.
        ws.drop(TaskIdentifier::Id(id2)).unwrap();
        let stack = ws.read_stack().unwrap();
        assert_eq!(stack.iter().count(), 1);
        assert_eq!(stack.iter().next().unwrap().id, id1);

        // Reopen.
        ws.reopen(TaskIdentifier::Id(id2)).unwrap();
        let stack = ws.read_stack().unwrap();
        assert_eq!(stack.iter().count(), 2);

        // Reopen non-archived fails.
        assert!(ws.reopen(TaskIdentifier::Id(id1)).is_err());

        // Edit and save.
        let mut t = ws.task(TaskIdentifier::Id(id1)).unwrap();
        t.title = "First (edited)".into();
        t.body = "new body".into();
        ws.save_task(&t).unwrap();
        let read = ws.task(TaskIdentifier::Id(id1)).unwrap();
        assert_eq!(read.title, "First (edited)");
        let stack = ws.read_stack().unwrap();
        let item = stack.iter().find(|i| i.id == id1).unwrap();
        assert_eq!(
            item.title, "First (edited)",
            "stack title should refresh on save"
        );

        // Remotes.
        ws.add_remote("up", "/path").unwrap();
        let remotes = ws.read_remotes().unwrap();
        assert_eq!(remotes.len(), 1);
        ws.remove_remote("up").unwrap();
        assert!(ws.read_remotes().unwrap().is_empty());

        // Backlinks.
        ws.handle_metadata(
            &Task {
                id: id1,
                title: "x".into(),
                body: format!("see [[{id2}]]"),
                attributes: Default::default(),
            },
            None,
        )
        .unwrap();
        let bl = backend::read_backlinks(ws.store(), id2).unwrap();
        assert!(bl.contains(&id1));
    }

    #[test]
    fn test_full_lifecycle_file_backend() {
        let dir = tempfile::tempdir().unwrap();
        Workspace::init(dir.path().to_path_buf()).unwrap();
        let ws = Workspace::from_path(dir.path().to_path_buf()).unwrap();
        assert!(!ws.is_git_backed());
        run_full_lifecycle(&ws);
    }

    #[test]
    fn test_full_lifecycle_git_backend() {
        let dir = tempfile::tempdir().unwrap();
        run_git_init(dir.path());
        Workspace::init(dir.path().to_path_buf()).unwrap();
        let ws = Workspace::from_path(dir.path().to_path_buf()).unwrap();
        assert!(ws.is_git_backed());
        run_full_lifecycle(&ws);
    }

    #[test]
    fn test_init_picks_backend_correctly() {
        let (_d, f, g) = setup_dual();
        assert!(!f.is_git_backed());
        assert!(g.is_git_backed());
    }

    #[test]
    fn test_clean_archives_orphaned_tasks() {
        let (_d, file, git) = setup_dual();
        for ws in [&file, &git] {
            // Push a task, then directly orphan it in the store.
            let t = ws.new_task("Indexed".into(), "ok".into()).unwrap();
            ws.push_task(t).unwrap();
            // Write an unindexed task directly to the store.
            backend::write_task(ws.store(), Id(999), "orphan", "", Loc::Active).unwrap();

            let active_before = backend::list_active(ws.store()).unwrap();
            assert!(active_before.contains(&Id(999)));
            ws.clean().unwrap();
            let active_after = backend::list_active(ws.store()).unwrap();
            assert!(!active_after.contains(&Id(999)));
            let archived = backend::list_archive(ws.store()).unwrap();
            assert!(archived.contains(&Id(999)));
        }
    }

    #[test]
    fn test_remote_persistence() {
        let (_d, file, git) = setup_dual();
        for ws in [&file, &git] {
            ws.add_remote("a", "/x").unwrap();
            ws.add_remote("b", "/y").unwrap();
            let ws2 = Workspace::from_path(ws.path.clone()).unwrap();
            assert_eq!(ws2.read_remotes().unwrap().len(), 2);
            assert!(ws.add_remote("a", "/z").is_err());
            assert!(ws.remove_remote("nope").is_err());
            ws.remove_remote("a").unwrap();
            assert_eq!(ws.read_remotes().unwrap().len(), 1);
        }
    }

    #[test]
    fn test_search_archived_round_trip() {
        let (_d, file, git) = setup_dual();
        for ws in [&file, &git] {
            let t = ws.new_task("Archived".into(), "a".into()).unwrap();
            let id = t.id;
            ws.push_task(t).unwrap();
            ws.drop(TaskIdentifier::Id(id)).unwrap();
            assert_eq!(
                backend::task_location(ws.store(), id).unwrap(),
                Some(Loc::Archived)
            );
        }
    }

    #[test]
    fn test_rot_tor_swap() {
        let (_d, file, git) = setup_dual();
        for ws in [&file, &git] {
            let mut ids = Vec::new();
            for n in 0..3 {
                let t = ws.new_task(format!("t{n}"), "".into()).unwrap();
                ids.push(t.id);
                ws.push_task(t).unwrap();
            }
            // Stack now: [ids[2], ids[1], ids[0]]
            ws.swap_top().unwrap();
            let s = ws.read_stack().unwrap();
            let order: Vec<_> = s.iter().map(|i| i.id).collect();
            assert_eq!(order, vec![ids[1], ids[2], ids[0]]);
            ws.swap_top().unwrap(); // back
            ws.rot().unwrap();
            ws.tor().unwrap();
            let s = ws.read_stack().unwrap();
            let order: Vec<_> = s.iter().map(|i| i.id).collect();
            assert_eq!(
                order,
                vec![ids[2], ids[1], ids[0]],
                "rot then tor is identity"
            );
        }
    }

    #[test]
    fn test_remote_display() {
        let r = Remote {
            prefix: "jira".into(),
            path: PathBuf::from("/p"),
        };
        assert_eq!(r.to_string(), "jira\t/p");
    }

    #[test]
    fn test_bare_task_display() {
        let t = SearchTask {
            id: Id(1),
            title: "x".into(),
            body: "y".into(),
        };
        assert_eq!(t.to_string(), "tsk-1\tx\n\ny");
    }

    #[test]
    fn test_task_display() {
        let t = Task {
            id: Id(1),
            title: "x".into(),
            body: "y".into(),
            attributes: Default::default(),
        };
        assert_eq!(t.to_string(), "x\n\ny");
    }

    #[test]
    fn test_migrate_file_to_git() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        // Init as file-backed (no git yet).
        Workspace::init(root.clone()).unwrap();
        let ws = Workspace::from_path(root.clone()).unwrap();
        assert!(!ws.is_git_backed());

        // Populate some state.
        let t1 = ws.new_task("Active".into(), "body1".into()).unwrap();
        let id1 = t1.id;
        ws.push_task(t1).unwrap();
        let t2 = ws.new_task("Will archive".into(), "body2".into()).unwrap();
        let id2 = t2.id;
        ws.push_task(t2).unwrap();
        ws.drop(TaskIdentifier::Id(id2)).unwrap();
        ws.add_remote("up", "/path").unwrap();
        let mut t = ws.task(TaskIdentifier::Id(id1)).unwrap();
        t.attributes.insert("k".into(), "v".into());
        ws.save_task(&t).unwrap();
        ws.handle_metadata(
            &Task {
                id: id1,
                title: "x".into(),
                body: format!("see [[{id2}]]"),
                attributes: Default::default(),
            },
            None,
        )
        .unwrap();

        // Migration before git init must fail.
        assert!(ws.migrate_to_git().is_err());

        // Now turn the directory into a git repo and migrate.
        run_git_init(&root);
        ws.migrate_to_git().unwrap();

        // Re-open the workspace (picks up the new marker → GitStore).
        let ws2 = Workspace::from_path(root.clone()).unwrap();
        assert!(ws2.is_git_backed());

        // All on-disk task data should be gone except the marker.
        let entries: Vec<_> = std::fs::read_dir(ws2.path.clone())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
            .collect();
        assert_eq!(entries, vec!["git-backed".to_string()]);

        // State preserved.
        let stack = ws2.read_stack().unwrap();
        let ids: Vec<_> = stack.iter().map(|i| i.id).collect();
        assert_eq!(ids, vec![id1]);
        let read = ws2.task(TaskIdentifier::Id(id1)).unwrap();
        assert_eq!(read.title, "Active");
        assert_eq!(read.attributes.get("k"), Some(&"v".to_string()));
        assert_eq!(backend::task_location(ws2.store(), id2).unwrap(), Some(Loc::Archived));
        let bl = backend::read_backlinks(ws2.store(), id2).unwrap();
        assert!(bl.contains(&id1));
        assert_eq!(ws2.read_remotes().unwrap().len(), 1);

        // Migrating an already-git-backed workspace fails.
        assert!(ws2.migrate_to_git().is_err());
    }

    #[test]
    fn test_attrs_round_trip() {
        let (_d, file, git) = setup_dual();
        for ws in [&file, &git] {
            let mut t = ws.new_task("t".into(), "b".into()).unwrap();
            t.attributes.insert("k1".into(), "v1".into());
            t.attributes.insert("k2".into(), "v2".into());
            ws.save_task(&t).unwrap();
            let reread = ws.task(TaskIdentifier::Id(t.id)).unwrap();
            assert_eq!(reread.attributes.get("k1"), Some(&"v1".to_string()));
            assert_eq!(reread.attributes.get("k2"), Some(&"v2".to_string()));
        }
    }
}
