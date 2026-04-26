//! High-level workspace API. Orchestrates [`object`], [`namespace`], and
//! [`queue`] to back the CLI commands.
//!
//! On disk the workspace is just a `.tsk/` marker directory inside a git
//! repository. `.tsk/namespace` and `.tsk/queue` select the user's active
//! namespace and queue (defaults: `tsk` / `tsk`).

#![allow(dead_code)]

use crate::errors::{Error, Result};
use crate::object::{self, StableId, Task as TaskObj};
use crate::{namespace, properties, queue};
use git2::Repository;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::path::PathBuf;
use std::str::FromStr;

const NAMESPACE_FILE: &str = "namespace";
const QUEUE_FILE: &str = "queue";
/// Auto-managed property holding the task's lifecycle state. Set to
/// `STATUS_OPEN` on creation and flipped to `STATUS_DONE` by [`Workspace::drop`].
pub const STATUS_KEY: &str = "status";
pub const STATUS_OPEN: &str = "open";
pub const STATUS_DONE: &str = "done";
/// User-local state lives under `<git-dir>/<STATE_DIR>/` so it isn't tracked
/// by the enclosing repo (the `.git/` directory is by definition not in the
/// working tree). Each clone gets its own active namespace + queue.
const STATE_DIR: &str = "tsk";

/// A human-readable task identifier (`tsk-N`). Always namespace-scoped: the
/// integer N has no meaning without the namespace it was minted in.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
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
    fn from(v: u32) -> Self {
        Id(v)
    }
}

pub enum TaskIdentifier {
    Id(Id),
    /// Index into the active queue's stack (0 = top).
    Relative(u32),
}

impl From<Id> for TaskIdentifier {
    fn from(v: Id) -> Self {
        TaskIdentifier::Id(v)
    }
}

/// One row of a queue listing.
pub struct StackEntry {
    pub id: Id,
    pub stable: StableId,
    pub title: String,
}

/// User-facing task: human id (in active namespace) + content + properties.
/// Each property holds zero or more text values.
pub struct Task {
    pub id: Id,
    pub stable: StableId,
    pub title: String,
    pub body: String,
    pub attributes: BTreeMap<String, Vec<String>>,
}

impl Display for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n\n{}", self.title, self.body)
    }
}

/// One pending inbox item in the active queue.
pub struct InboxItem {
    pub key: String,
    pub source_queue: String,
    pub stable: StableId,
    pub title: String,
}

pub struct Workspace {
    /// The user-local state directory: `<git-dir>/tsk/`. Holds the
    /// `namespace` and `queue` selectors — both per-clone, not pushed.
    pub path: PathBuf,
    /// The enclosing git repo's `.git` directory.
    pub git_dir: PathBuf,
}

impl Workspace {
    /// Bootstrap user-local state in `<git-dir>/tsk/`. Idempotent: existing
    /// state files are left alone so re-init doesn't reset the active
    /// namespace/queue. Errors if `path` isn't inside a git repository.
    pub fn init(path: PathBuf) -> Result<()> {
        let git_dir = find_git_dir(&path)
            .ok_or_else(|| Error::Parse("tsk requires an enclosing git repository".into()))?;
        let state_dir = git_dir.join(STATE_DIR);
        std::fs::create_dir_all(&state_dir)?;
        // Lift any pre-existing selectors out of a legacy `.tsk/` directory
        // *before* writing defaults, so the migrated values win.
        if let Some(workdir) = git_dir.parent() {
            let legacy = workdir.join(".tsk");
            if legacy.is_dir() {
                for name in [NAMESPACE_FILE, QUEUE_FILE] {
                    let src = legacy.join(name);
                    let dst = state_dir.join(name);
                    if src.exists() && !dst.exists() {
                        let _ = std::fs::copy(&src, &dst);
                    }
                }
                let _ = std::fs::remove_dir_all(&legacy);
            }
        }
        let ns = state_dir.join(NAMESPACE_FILE);
        if !ns.exists() {
            std::fs::write(&ns, namespace::DEFAULT_NS.as_bytes())?;
        }
        let q = state_dir.join(QUEUE_FILE);
        if !q.exists() {
            std::fs::write(&q, queue::DEFAULT_QUEUE.as_bytes())?;
        }
        Ok(())
    }

    pub fn from_path(path: PathBuf) -> Result<Self> {
        let git_dir = find_git_dir(&path).ok_or(Error::Uninitialized)?;
        let state_dir = git_dir.join(STATE_DIR);
        if !state_dir.exists() {
            // Auto-bootstrap so `git tsk <anything>` works without an
            // explicit init step.
            Self::init(path)?;
        }
        Ok(Self {
            path: state_dir,
            git_dir,
        })
    }

    fn repo(&self) -> Result<Repository> {
        Ok(Repository::open(&self.git_dir)?)
    }

    pub fn namespace(&self) -> String {
        std::fs::read_to_string(self.path.join(NAMESPACE_FILE))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| namespace::DEFAULT_NS.to_string())
    }

    pub fn queue(&self) -> String {
        std::fs::read_to_string(self.path.join(QUEUE_FILE))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| queue::DEFAULT_QUEUE.to_string())
    }

    pub fn switch_namespace(&self, name: &str) -> Result<()> {
        namespace::validate_name(name)?;
        std::fs::write(self.path.join(NAMESPACE_FILE), name.as_bytes())?;
        Ok(())
    }

    pub fn switch_queue(&self, name: &str) -> Result<()> {
        queue::validate_name(name)?;
        std::fs::write(self.path.join(QUEUE_FILE), name.as_bytes())?;
        Ok(())
    }

    pub fn list_namespaces(&self) -> Result<Vec<String>> {
        namespace::list_names(&self.repo()?)
    }

    pub fn list_queues(&self) -> Result<Vec<String>> {
        queue::list_names(&self.repo()?)
    }

    pub fn create_queue(&self, name: &str, can_pull: Option<bool>) -> Result<()> {
        queue::validate_name(name)?;
        let repo = self.repo()?;
        let mut q = queue::read(&repo, name)?;
        if let Some(cp) = can_pull {
            q.can_pull = cp;
        }
        queue::write(&repo, name, &q, "create queue")
    }

    fn resolve(&self, identifier: TaskIdentifier) -> Result<(Id, StableId)> {
        match identifier {
            TaskIdentifier::Id(id) => {
                let stable = namespace::lookup(&self.repo()?, &self.namespace(), id.0)?
                    .ok_or_else(|| Error::Parse(format!("Task {id} not found in namespace")))?;
                Ok((id, stable))
            }
            TaskIdentifier::Relative(r) => {
                let stack = self.read_stack()?;
                let entry = stack.into_iter().nth(r as usize).ok_or(Error::NoTasks)?;
                Ok((entry.id, entry.stable))
            }
        }
    }

    pub fn new_task(&self, title: String, body: String) -> Result<Task> {
        let repo = self.repo()?;
        let content = if body.is_empty() {
            title.trim().to_string()
        } else {
            format!("{}\n\n{}", title.trim(), body.trim())
        };
        let mut task_obj = TaskObj::new(content);
        task_obj
            .properties
            .insert(STATUS_KEY.into(), vec![STATUS_OPEN.into()]);
        let stable = object::create(&repo, &task_obj, "create")?;
        properties::reindex_task(&repo, &stable, &task_obj.properties)?;
        let human = namespace::assign_id(&repo, &self.namespace(), stable.clone(), "assign-id")?;
        Ok(Task {
            id: Id(human),
            stable,
            title: task_obj.title().to_string(),
            body: task_obj.body().to_string(),
            attributes: task_obj.properties,
        })
    }

    pub fn task(&self, identifier: TaskIdentifier) -> Result<Task> {
        let (id, stable) = self.resolve(identifier)?;
        let repo = self.repo()?;
        let task_obj = object::read(&repo, &stable)?
            .ok_or_else(|| Error::Parse(format!("Task {id} content missing")))?;
        Ok(Task {
            id,
            stable,
            title: task_obj.title().to_string(),
            body: task_obj.body().to_string(),
            attributes: task_obj.properties,
        })
    }

    pub fn save_task(&self, task: &Task) -> Result<()> {
        let repo = self.repo()?;
        let content = if task.body.is_empty() {
            task.title.trim().to_string()
        } else {
            format!("{}\n\n{}", task.title.trim(), task.body.trim())
        };
        let task_obj = TaskObj {
            content,
            properties: task.attributes.clone(),
        };
        object::update(&repo, &task.stable, &task_obj, "edit")?;
        properties::reindex_task(&repo, &task.stable, &task.attributes)?;
        Ok(())
    }

    /// Append a value to a property on a task. If the value is already
    /// present, this is a no-op. Persists both the task tree and the index.
    pub fn add_property_value(
        &self,
        identifier: TaskIdentifier,
        key: &str,
        value: &str,
    ) -> Result<()> {
        let mut task = self.task(identifier)?;
        let entry = task.attributes.entry(key.to_string()).or_default();
        if !entry.iter().any(|v| v == value) {
            entry.push(value.to_string());
        }
        self.save_task(&task)
    }

    /// Replace the entire value list for a property.
    pub fn set_property(
        &self,
        identifier: TaskIdentifier,
        key: &str,
        values: Vec<String>,
    ) -> Result<()> {
        let mut task = self.task(identifier)?;
        if values.is_empty() {
            task.attributes.remove(key);
        } else {
            task.attributes.insert(key.to_string(), values);
        }
        self.save_task(&task)
    }

    /// Remove a single value from a property, or the whole property if
    /// `value` is None.
    pub fn unset_property(
        &self,
        identifier: TaskIdentifier,
        key: &str,
        value: Option<&str>,
    ) -> Result<()> {
        let mut task = self.task(identifier)?;
        match value {
            None => {
                task.attributes.remove(key);
            }
            Some(v) => {
                if let Some(entry) = task.attributes.get_mut(key) {
                    entry.retain(|x| x != v);
                    if entry.is_empty() {
                        task.attributes.remove(key);
                    }
                }
            }
        }
        self.save_task(&task)
    }

    pub fn property_keys(&self) -> Result<Vec<String>> {
        properties::list_keys(&self.repo()?)
    }

    pub fn property_values(&self, key: &str) -> Result<Vec<String>> {
        properties::values_for(&self.repo()?, key)
    }

    /// Find tasks (by human id, scoped to active namespace) that have
    /// `key` set; if `value` is supplied, restricts to entries containing
    /// that value.
    pub fn find_by_property(
        &self,
        key: &str,
        value: Option<&str>,
    ) -> Result<Vec<(Id, StableId, String)>> {
        let repo = self.repo()?;
        let stables = properties::find(&repo, key, value)?;
        let ns = namespace::read(&repo, &self.namespace())?;
        let mut by_stable: BTreeMap<&StableId, u32> = BTreeMap::new();
        for (h, s) in &ns.mapping {
            by_stable.insert(s, *h);
        }
        let mut out = Vec::new();
        for stable in stables {
            // Only return tasks visible in the active namespace.
            let Some(&human) = by_stable.get(&stable) else {
                continue;
            };
            let title = object::read(&repo, &stable)?
                .map(|t| t.title().to_string())
                .unwrap_or_default();
            out.push((Id(human), stable, title));
        }
        Ok(out)
    }

    pub fn push_task(&self, task: Task) -> Result<()> {
        queue::push_top(&self.repo()?, &self.queue(), task.stable, "push")
    }

    pub fn append_task(&self, task: Task) -> Result<()> {
        queue::push_bottom(&self.repo()?, &self.queue(), task.stable, "append")
    }

    pub fn read_stack(&self) -> Result<Vec<StackEntry>> {
        let repo = self.repo()?;
        let q = queue::read(&repo, &self.queue())?;
        let ns_name = self.namespace();
        let ns = namespace::read(&repo, &ns_name)?;
        let mut by_stable: BTreeMap<&StableId, u32> = BTreeMap::new();
        for (h, s) in &ns.mapping {
            by_stable.insert(s, *h);
        }
        let mut out = Vec::with_capacity(q.index.len());
        for stable in q.index {
            // Skip tasks not visible in the active namespace (different ns owns them).
            let Some(&human) = by_stable.get(&stable) else {
                continue;
            };
            let title = object::read(&repo, &stable)?
                .map(|t| t.title().to_string())
                .unwrap_or_default();
            out.push(StackEntry {
                id: Id(human),
                stable,
                title,
            });
        }
        Ok(out)
    }

    /// Set `status=open` on every task in the active namespace that has no
    /// status yet. Skips tasks already marked done. Returns the number of
    /// tasks updated. One-shot migration for tasks created before
    /// auto-status existed.
    pub fn backfill_status(&self) -> Result<usize> {
        let repo = self.repo()?;
        let ns = namespace::read(&repo, &self.namespace())?;
        let mut updated = 0usize;
        for (human, _stable) in ns.mapping.iter() {
            let mut task = self.task(TaskIdentifier::Id(Id(*human)))?;
            if task.attributes.contains_key(STATUS_KEY) {
                continue;
            }
            task.attributes
                .insert(STATUS_KEY.into(), vec![STATUS_OPEN.into()]);
            self.save_task(&task)?;
            updated += 1;
        }
        Ok(updated)
    }

    /// Drop a task from the active queue and mark it `status=done`. The
    /// namespace binding is kept so the task remains addressable by its
    /// human id (and discoverable via `tsk prop find status done`); the
    /// task object's commit history is preserved either way.
    pub fn drop(&self, identifier: TaskIdentifier) -> Result<Option<Id>> {
        let (id, stable) = self.resolve(identifier)?;
        let repo = self.repo()?;
        queue::remove(&repo, &self.queue(), &stable, "drop")?;
        // Flip status=done in the task's tree + index.
        let mut task = self.task(TaskIdentifier::Id(id))?;
        task.attributes
            .insert(STATUS_KEY.into(), vec![STATUS_DONE.into()]);
        self.save_task(&task)?;
        Ok(Some(id))
    }

    fn mutate_index<F: FnOnce(&mut Vec<StableId>)>(&self, f: F, msg: &str) -> Result<()> {
        let repo = self.repo()?;
        let mut q = queue::read(&repo, &self.queue())?;
        f(&mut q.index);
        queue::write(&repo, &self.queue(), &q, msg)
    }

    pub fn swap_top(&self) -> Result<()> {
        self.mutate_index(
            |idx| {
                if idx.len() >= 2 {
                    idx.swap(0, 1);
                }
            },
            "swap",
        )
    }

    fn rotate_top3(&self, third_to_top: bool) -> Result<()> {
        self.mutate_index(
            |idx| {
                if idx.len() >= 3 {
                    if third_to_top {
                        let c = idx.remove(2);
                        idx.insert(0, c);
                    } else {
                        let a = idx.remove(0);
                        idx.insert(2, a);
                    }
                }
            },
            "rotate",
        )
    }

    pub fn rot(&self) -> Result<()> {
        self.rotate_top3(true)
    }

    pub fn tor(&self) -> Result<()> {
        self.rotate_top3(false)
    }

    fn move_in_index(&self, identifier: TaskIdentifier, to_front: bool) -> Result<()> {
        let (_, stable) = self.resolve(identifier)?;
        self.mutate_index(
            |idx| {
                idx.retain(|s| s != &stable);
                if to_front {
                    idx.insert(0, stable);
                } else {
                    idx.push(stable);
                }
            },
            if to_front { "prioritize" } else { "deprioritize" },
        )
    }

    pub fn prioritize(&self, identifier: TaskIdentifier) -> Result<()> {
        self.move_in_index(identifier, true)
    }

    pub fn deprioritize(&self, identifier: TaskIdentifier) -> Result<()> {
        self.move_in_index(identifier, false)
    }

    /// Drop entries from the active queue's index whose stable ids no longer
    /// resolve to a task object.
    pub fn clean(&self) -> Result<()> {
        let repo = self.repo()?;
        let mut q = queue::read(&repo, &self.queue())?;
        let before = q.index.len();
        q.index.retain(|s| {
            repo.find_reference(&s.refname())
                .ok()
                .and_then(|r| r.target())
                .is_some()
        });
        if q.index.len() != before {
            queue::write(&repo, &self.queue(), &q, "clean")?;
        }
        Ok(())
    }

    /// Share a task into another namespace by binding the same stable id to
    /// the next human id in `target_ns`.
    pub fn share(&self, identifier: TaskIdentifier, target_ns: &str) -> Result<u32> {
        let cur = self.namespace();
        if target_ns == cur {
            return Err(Error::Parse(
                "Refusing to share a task into its own namespace".into(),
            ));
        }
        namespace::validate_name(target_ns)?;
        let (_, stable) = self.resolve(identifier)?;
        let repo = self.repo()?;
        namespace::assign_id(&repo, target_ns, stable, "share")
    }

    /// Move a task from the active queue's index into `target_queue`'s inbox.
    pub fn assign_to_queue(
        &self,
        identifier: TaskIdentifier,
        target_queue: &str,
    ) -> Result<String> {
        let cur = self.queue();
        if target_queue == cur {
            return Err(Error::Parse(
                "Refusing to assign a task to its own queue".into(),
            ));
        }
        queue::validate_name(target_queue)?;
        let (id, stable) = self.resolve(identifier)?;
        let repo = self.repo()?;
        let key = queue::inbox_key(&cur, id.0);
        queue::add_to_inbox(&repo, target_queue, key.clone(), stable.clone(), "assign")?;
        queue::remove(&repo, &cur, &stable, "assigned-out")?;
        Ok(key)
    }

    pub fn list_inbox(&self) -> Result<Vec<InboxItem>> {
        let repo = self.repo()?;
        let q = queue::read(&repo, &self.queue())?;
        let mut out = Vec::with_capacity(q.inbox.len());
        for (key, stable) in q.inbox {
            let source_queue = key
                .rsplit_once('-')
                .map(|(s, _)| s.to_string())
                .unwrap_or_else(|| key.clone());
            let title = object::read(&repo, &stable)?
                .map(|t| t.title().to_string())
                .unwrap_or_default();
            out.push(InboxItem {
                key,
                source_queue,
                stable,
                title,
            });
        }
        Ok(out)
    }

    /// Accept an inbox item: bind to a human id in the active namespace
    /// (if not already), and push onto the top of the active queue.
    pub fn accept_inbox(&self, key: &str) -> Result<Id> {
        let repo = self.repo()?;
        let stable = queue::take_from_inbox(&repo, &self.queue(), key, "accept")?
            .ok_or_else(|| Error::Parse(format!("Inbox item '{key}' not found")))?;
        let ns_name = self.namespace();
        let human = match namespace::human_for(&repo, &ns_name, &stable)? {
            Some(h) => h,
            None => namespace::assign_id(&repo, &ns_name, stable.clone(), "accept-bind")?,
        };
        queue::push_top(&repo, &self.queue(), stable, "accept-push")?;
        Ok(Id(human))
    }

    pub fn reject_inbox(&self, key: &str) -> Result<()> {
        let repo = self.repo()?;
        queue::take_from_inbox(&repo, &self.queue(), key, "reject")?
            .ok_or_else(|| Error::Parse(format!("Inbox item '{key}' not found")))?;
        Ok(())
    }

    /// Pull a task from a foreign queue's index into the active queue's
    /// index. Only allowed if the source queue's `can_pull` is true.
    pub fn pull_from_queue(&self, source_queue: &str, identifier: TaskIdentifier) -> Result<Id> {
        let cur = self.queue();
        if source_queue == cur {
            return Err(Error::Parse("Source queue equals active queue".into()));
        }
        let repo = self.repo()?;
        let src = queue::read(&repo, source_queue)?;
        if !src.can_pull {
            return Err(Error::Parse(format!(
                "Queue '{source_queue}' has can-pull=false; refusing"
            )));
        }
        let (_, stable) = self.resolve(identifier)?;
        if !src.index.iter().any(|s| s == &stable) {
            return Err(Error::Parse(format!(
                "Task not present in queue '{source_queue}'"
            )));
        }
        queue::remove(&repo, source_queue, &stable, "pulled-out")?;
        queue::push_top(&repo, &cur, stable.clone(), "pull")?;
        let ns_name = self.namespace();
        let human = match namespace::human_for(&repo, &ns_name, &stable)? {
            Some(h) => h,
            None => namespace::assign_id(&repo, &ns_name, stable, "pull-bind")?,
        };
        Ok(Id(human))
    }

    pub fn configure_git_remote_refspecs(&self, remote: &str) -> Result<()> {
        for (key, value) in [
            (format!("remote.{remote}.push"), "refs/tsk/*:refs/tsk/*"),
            (format!("remote.{remote}.fetch"), "+refs/tsk/*:refs/tsk/*"),
        ] {
            let cmd = std::process::Command::new("git")
                .arg("--git-dir")
                .arg(&self.git_dir)
                .args(["config", "--get-all", &key])
                .output()?;
            if String::from_utf8_lossy(&cmd.stdout)
                .lines()
                .any(|l| l.trim() == value)
            {
                continue;
            }
            let s = std::process::Command::new("git")
                .arg("--git-dir")
                .arg(&self.git_dir)
                .args(["config", "--add", &key, value])
                .status()?;
            if !s.success() {
                return Err(Error::Parse("git config failed".into()));
            }
        }
        Ok(())
    }

    pub fn git_push(&self, remote: &str) -> Result<()> {
        let s = std::process::Command::new("git")
            .arg("--git-dir")
            .arg(&self.git_dir)
            .args(["push", remote, "refs/tsk/*:refs/tsk/*"])
            .status()?;
        if !s.success() {
            return Err(Error::Parse("git push failed".into()));
        }
        Ok(())
    }

    pub fn git_pull(&self, remote: &str) -> Result<()> {
        let s = std::process::Command::new("git")
            .arg("--git-dir")
            .arg(&self.git_dir)
            .args(["fetch", "--prune", remote, "+refs/tsk/*:refs/tsk/*"])
            .status()?;
        if !s.success() {
            return Err(Error::Parse("git fetch failed".into()));
        }
        Ok(())
    }
}

pub fn find_git_dir(start: &std::path::Path) -> Option<PathBuf> {
    let mut cur = Some(start.to_path_buf());
    while let Some(p) = cur {
        let candidate = p.join(".git");
        if candidate.exists() {
            return Some(candidate);
        }
        cur = p.parent().map(|q| q.to_path_buf());
    }
    None
}

#[cfg(test)]
mod test {
    use super::*;

    fn run_git_init(p: &std::path::Path) {
        let s = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(p)
            .status()
            .unwrap();
        assert!(s.success());
        let _ = std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(p)
            .status();
        let _ = std::process::Command::new("git")
            .args(["config", "user.email", "t@e"])
            .current_dir(p)
            .status();
    }

    fn fresh_workspace() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        run_git_init(dir.path());
        Workspace::init(dir.path().to_path_buf()).unwrap();
        let ws = Workspace::from_path(dir.path().to_path_buf()).unwrap();
        (dir, ws)
    }

    #[test]
    fn push_list_drop_round_trip() {
        let (_d, ws) = fresh_workspace();
        let t1 = ws.new_task("first".into(), "body 1".into()).unwrap();
        let id1 = t1.id;
        ws.push_task(t1).unwrap();
        let t2 = ws.new_task("second".into(), "".into()).unwrap();
        let id2 = t2.id;
        ws.push_task(t2).unwrap();
        let stack = ws.read_stack().unwrap();
        assert_eq!(
            stack.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![id2, id1]
        );
        let read = ws.task(TaskIdentifier::Id(id1)).unwrap();
        assert_eq!(read.title, "first");
        assert_eq!(read.body, "body 1");
        let dropped = ws.drop(TaskIdentifier::Id(id1)).unwrap();
        assert_eq!(dropped, Some(id1));
        let stack = ws.read_stack().unwrap();
        assert_eq!(stack.iter().map(|e| e.id).collect::<Vec<_>>(), vec![id2]);
    }

    #[test]
    fn id_allocation_monotonic_across_drops() {
        let (_d, ws) = fresh_workspace();
        let t1 = ws.new_task("a".into(), "".into()).unwrap();
        let id1 = t1.id;
        ws.push_task(t1).unwrap();
        ws.drop(TaskIdentifier::Id(id1)).unwrap();
        let t2 = ws.new_task("b".into(), "".into()).unwrap();
        assert_eq!(t2.id.0, id1.0 + 1, "ids must not be reused after drop");
    }

    #[test]
    fn edit_appends_history() {
        let (_d, ws) = fresh_workspace();
        let t = ws.new_task("v1".into(), "body".into()).unwrap();
        let id = t.id;
        let stable = t.stable.clone();
        ws.push_task(t).unwrap();
        let mut t = ws.task(TaskIdentifier::Id(id)).unwrap();
        t.title = "v2".into();
        ws.save_task(&t).unwrap();
        let read = ws.task(TaskIdentifier::Id(id)).unwrap();
        assert_eq!(read.title, "v2");
        assert_eq!(read.stable, stable, "stable id must not change on edit");
        let repo = ws.repo().unwrap();
        let head = repo
            .find_reference(&stable.refname())
            .unwrap()
            .target()
            .unwrap();
        let commit = repo.find_commit(head).unwrap();
        assert_eq!(commit.parent_count(), 1);
    }

    #[test]
    fn share_to_other_namespace() {
        let (_d, ws) = fresh_workspace();
        let t = ws.new_task("shared".into(), "".into()).unwrap();
        let id_in_tsk = t.id;
        let stable = t.stable.clone();
        ws.push_task(t).unwrap();
        let h = ws.share(TaskIdentifier::Id(id_in_tsk), "alpha").unwrap();
        ws.switch_namespace("alpha").unwrap();
        let task_in_alpha = ws.task(TaskIdentifier::Id(Id(h))).unwrap();
        assert_eq!(task_in_alpha.stable, stable);
        assert_eq!(task_in_alpha.title, "shared");
    }

    #[test]
    fn assign_moves_to_target_inbox() {
        let (_d, ws) = fresh_workspace();
        ws.create_queue("review", None).unwrap();
        let t = ws.new_task("for review".into(), "".into()).unwrap();
        let id = t.id;
        ws.push_task(t).unwrap();
        let key = ws
            .assign_to_queue(TaskIdentifier::Id(id), "review")
            .unwrap();
        let stack = ws.read_stack().unwrap();
        assert!(stack.is_empty());
        ws.switch_queue("review").unwrap();
        let inbox = ws.list_inbox().unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].key, key);
        let accepted = ws.accept_inbox(&key).unwrap();
        assert_eq!(accepted.0, id.0);
        let stack = ws.read_stack().unwrap();
        assert_eq!(stack.len(), 1);
    }

    #[test]
    fn pull_only_when_can_pull() {
        let (_d, ws) = fresh_workspace();
        ws.create_queue("private", Some(false)).unwrap();
        ws.switch_queue("private").unwrap();
        let t = ws.new_task("private task".into(), "".into()).unwrap();
        let id = t.id;
        ws.push_task(t).unwrap();
        ws.switch_queue("tsk").unwrap();
        let r = ws.pull_from_queue("private", TaskIdentifier::Id(id));
        assert!(r.is_err(), "pull from can-pull=false queue must fail");
        ws.create_queue("private", Some(true)).unwrap();
        let pulled = ws.pull_from_queue("private", TaskIdentifier::Id(id)).unwrap();
        assert_eq!(pulled.0, id.0);
        let stack = ws.read_stack().unwrap();
        assert_eq!(stack.len(), 1);
    }

    #[test]
    fn backfill_status_marks_legacy_tasks_open_and_skips_done() {
        let (_d, ws) = fresh_workspace();

        // Simulate a legacy task: bind a stable id with no status property.
        let repo = ws.repo().unwrap();
        let raw = object::Task::new("legacy task");
        let stable = object::create(&repo, &raw, "create").unwrap();
        let h_legacy =
            namespace::assign_id(&repo, &ws.namespace(), stable.clone(), "assign").unwrap();
        queue::push_top(&repo, &ws.queue(), stable, "push").unwrap();

        // Plus a fresh task that already has status=open and a dropped one.
        let t_open = ws.new_task("fresh open".into(), "".into()).unwrap();
        let id_open = t_open.id;
        ws.push_task(t_open).unwrap();
        let t_done = ws.new_task("will drop".into(), "".into()).unwrap();
        let id_done = t_done.id;
        ws.push_task(t_done).unwrap();
        ws.drop(TaskIdentifier::Id(id_done)).unwrap();

        // Pre-condition: the legacy task has no status.
        let read = ws.task(TaskIdentifier::Id(Id(h_legacy))).unwrap();
        assert!(!read.attributes.contains_key(STATUS_KEY));

        let n = ws.backfill_status().unwrap();
        assert_eq!(n, 1, "only the legacy task gets backfilled");

        // Legacy is now open, fresh-open stays open, dropped stays done.
        let read = ws.task(TaskIdentifier::Id(Id(h_legacy))).unwrap();
        assert_eq!(
            read.attributes.get(STATUS_KEY),
            Some(&vec![STATUS_OPEN.to_string()])
        );
        let read = ws.task(TaskIdentifier::Id(id_open)).unwrap();
        assert_eq!(
            read.attributes.get(STATUS_KEY),
            Some(&vec![STATUS_OPEN.to_string()])
        );
        let read = ws.task(TaskIdentifier::Id(id_done)).unwrap();
        assert_eq!(
            read.attributes.get(STATUS_KEY),
            Some(&vec![STATUS_DONE.to_string()])
        );

        // Re-running is a no-op.
        let n = ws.backfill_status().unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn new_task_starts_open_drop_marks_done() {
        let (_d, ws) = fresh_workspace();
        let t = ws.new_task("a".into(), "".into()).unwrap();
        assert_eq!(
            t.attributes.get(STATUS_KEY),
            Some(&vec![STATUS_OPEN.to_string()])
        );
        let id = t.id;
        ws.push_task(t).unwrap();

        // Index reflects the new open task.
        let opens = ws
            .find_by_property(STATUS_KEY, Some(STATUS_OPEN))
            .unwrap();
        assert_eq!(opens.len(), 1);
        assert_eq!(opens[0].0, id);

        ws.drop(TaskIdentifier::Id(id)).unwrap();

        // Status flipped, queue empty, namespace binding kept.
        let read = ws.task(TaskIdentifier::Id(id)).unwrap();
        assert_eq!(
            read.attributes.get(STATUS_KEY),
            Some(&vec![STATUS_DONE.to_string()])
        );
        assert!(ws.read_stack().unwrap().is_empty());
        let dones = ws
            .find_by_property(STATUS_KEY, Some(STATUS_DONE))
            .unwrap();
        assert_eq!(dones.len(), 1);
        assert_eq!(dones[0].0, id);
    }

    #[test]
    fn init_does_not_create_files_in_working_tree() {
        let dir = tempfile::tempdir().unwrap();
        run_git_init(dir.path());
        Workspace::init(dir.path().to_path_buf()).unwrap();
        // The only directory entries in the working tree should be `.git`
        // (from `git init`) — no `.tsk` and nothing else.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect();
        assert_eq!(
            entries,
            vec![".git".to_string()],
            "no user-local state should land in the working tree"
        );
        // The state files should live under <git-dir>/tsk/.
        assert!(dir.path().join(".git/tsk/namespace").exists());
        assert!(dir.path().join(".git/tsk/queue").exists());
    }

    #[test]
    fn legacy_dot_tsk_directory_is_migrated_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        run_git_init(dir.path());
        // Simulate an old workspace with a tracked `.tsk/namespace` already.
        let legacy = dir.path().join(".tsk");
        std::fs::create_dir(&legacy).unwrap();
        std::fs::write(legacy.join("namespace"), b"alpha").unwrap();
        std::fs::write(legacy.join("queue"), b"review").unwrap();

        Workspace::init(dir.path().to_path_buf()).unwrap();
        let ws = Workspace::from_path(dir.path().to_path_buf()).unwrap();
        assert_eq!(ws.namespace(), "alpha", "legacy namespace must migrate");
        assert_eq!(ws.queue(), "review", "legacy queue must migrate");
        assert!(!legacy.exists(), "legacy .tsk/ must be removed");
    }

    #[test]
    fn rot_tor_swap_round_trip() {
        let (_d, ws) = fresh_workspace();
        let mut ids = Vec::new();
        for n in 0..3 {
            let t = ws.new_task(format!("t{n}"), "".into()).unwrap();
            ids.push(t.id);
            ws.push_task(t).unwrap();
        }
        ws.swap_top().unwrap();
        let s = ws.read_stack().unwrap();
        assert_eq!(
            s.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![ids[1], ids[2], ids[0]]
        );
        ws.swap_top().unwrap();
        ws.rot().unwrap();
        ws.tor().unwrap();
        let s = ws.read_stack().unwrap();
        assert_eq!(
            s.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![ids[2], ids[1], ids[0]]
        );
    }
}
