//! High-level workspace API. Orchestrates [`object`], [`namespace`], and
//! [`queue`] to back the CLI commands.
//!
//! Per-clone state lives under `<git-dir>/tsk/` (not tracked, not pushed).
//! Two files select the active namespace and queue (defaults: `tsk` / `tsk`):
//! `<git-dir>/tsk/namespace` and `<git-dir>/tsk/queue`.

use crate::errors::{Error, Result};
use crate::object::{self, StableId, Task as TaskObj};
use crate::patch;
use crate::{merge, namespace, properties, queue};
use git2::Repository;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::OnceLock;

/// Process-wide override for the active queue, set once by the CLI's
/// `-q/--queue` flag. When `Some`, `Workspace::queue()` returns this
/// value instead of reading `<git-dir>/tsk/queue`.
static QUEUE_OVERRIDE: OnceLock<Option<String>> = OnceLock::new();

pub fn set_queue_override(q: Option<String>) {
    let _ = QUEUE_OVERRIDE.set(q);
}

#[derive(Debug)]
pub struct ImportOutcome {
    pub stable: StableId,
    pub commits_imported: usize,
    pub bound_human: Option<u32>,
}

const NAMESPACE_FILE: &str = "namespace";
const QUEUE_FILE: &str = "queue";
const REMOTE_FILE: &str = "remote";
pub const DEFAULT_REMOTE: &str = "origin";
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

#[derive(Clone)]
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
#[derive(Debug)]
pub struct Task {
    #[allow(dead_code)] // exposed for callers; constructed by workspace
    pub id: Id,
    #[allow(dead_code)] // exposed for callers; constructed by workspace
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

/// One commit on a tsk ref's history.
pub struct LogCommit {
    pub oid: String,
    pub timestamp: i64,
    pub author: String,
    pub summary: String,
}

/// One pending inbox item in the active queue.
pub struct InboxItem {
    pub key: String,
    pub source_queue: String,
    #[allow(dead_code)] // exposed for callers; constructed by workspace
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

    fn read_selector(&self, file: &str, default: &str) -> String {
        std::fs::read_to_string(self.path.join(file))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default.to_string())
    }

    pub fn namespace(&self) -> String {
        self.read_selector(NAMESPACE_FILE, namespace::DEFAULT_NS)
    }

    pub fn queue(&self) -> String {
        if let Some(Some(q)) = QUEUE_OVERRIDE.get() {
            return q.clone();
        }
        self.read_selector(QUEUE_FILE, queue::DEFAULT_QUEUE)
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

    /// Persist a clone-local default remote so `tsk git-push` /
    /// `tsk git-pull` (and the auto-push paths) target it without an
    /// explicit `<remote>` arg.
    pub fn default_remote(&self) -> String {
        self.read_selector(REMOTE_FILE, DEFAULT_REMOTE)
    }

    /// Persist `name` as the default remote. Errors if `name` isn't a
    /// configured git remote — tsk only ever uses remotes the host repo
    /// already knows about.
    pub fn set_default_remote(&self, name: &str) -> Result<()> {
        let known = self.git_remotes()?;
        if !known.iter().any(|r| r == name) {
            return Err(Error::Parse(format!(
                "no such git remote '{name}'; configured: {}",
                if known.is_empty() {
                    "<none>".into()
                } else {
                    known.join(", ")
                }
            )));
        }
        std::fs::write(self.path.join(REMOTE_FILE), name.as_bytes())?;
        Ok(())
    }

    pub fn git_remotes(&self) -> Result<Vec<String>> {
        let out = self.git().arg("remote").output()?;
        if !out.status.success() {
            return Err(Error::Parse("git remote failed".into()));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
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

    fn make_task(id: Id, stable: StableId, obj: TaskObj) -> Task {
        Task {
            title: obj.title().to_string(),
            body: obj.body().to_string(),
            attributes: obj.properties,
            id,
            stable,
        }
    }

    fn read_task_obj(repo: &Repository, stable: &StableId) -> Result<TaskObj> {
        object::read(repo, stable)?
            .ok_or_else(|| Error::Parse(format!("task {stable} content missing")))
    }

    fn title_for(repo: &Repository, stable: &StableId) -> Result<String> {
        Ok(object::read(repo, stable)?
            .map(|t| t.title().to_string())
            .unwrap_or_default())
    }

    /// Create a task — or, when the content matches an existing task,
    /// reopen / re-bind it instead of clobbering.
    ///
    /// Stable id is content-addressed (SHA-1 of the content blob), so two
    /// `new_task` calls with the same body produce the same stable id and
    /// would collide on the task ref. Branches:
    ///
    /// - ref doesn't exist → fresh create.
    /// - ref exists, bound in active namespace, status=done → reopen
    ///   (flip status back to `open`, return the existing human id).
    /// - ref exists, bound in active namespace, status=open → idempotent;
    ///   return the existing human id without touching the task tree.
    /// - ref exists, bound in another namespace but not the active one →
    ///   error with a hint to use `tsk share` or `tsk reopen -T`.
    /// - ref exists, unbound everywhere → bind it in the active namespace.
    pub fn new_task(&self, title: String, body: String) -> Result<Task> {
        let repo = self.repo()?;
        let content = if body.is_empty() {
            title.trim().to_string()
        } else {
            format!("{}\n\n{}", title.trim(), body.trim())
        };
        // Compute the stable id without writing anything.
        let content_oid = repo.blob(content.as_bytes())?;
        let stable = StableId(content_oid.to_string());
        let active_ns = self.namespace();

        if repo.find_reference(&stable.refname()).is_err() {
            let mut obj = TaskObj::new(content);
            obj.properties
                .insert(STATUS_KEY.into(), vec![STATUS_OPEN.into()]);
            let stable = object::create(&repo, &obj, "create")?;
            properties::reindex_task(&repo, &stable, &obj.properties)?;
            let human = namespace::assign_id(&repo, &active_ns, stable.clone(), "assign-id")?;
            return Ok(Self::make_task(Id(human), stable, obj));
        }

        // Ref already exists. Decide between reopen / idempotent / bind / error.
        if let Some(human) = namespace::human_for(&repo, &active_ns, &stable)? {
            let mut obj = Self::read_task_obj(&repo, &stable)?;
            let is_done = obj
                .properties
                .get(STATUS_KEY)
                .is_some_and(|v| v.iter().any(|s| s == STATUS_DONE));
            if is_done {
                obj.properties
                    .insert(STATUS_KEY.into(), vec![STATUS_OPEN.into()]);
                object::update(&repo, &stable, &obj, "reopen")?;
                properties::reindex_task(&repo, &stable, &obj.properties)?;
            }
            return Ok(Self::make_task(Id(human), stable, obj));
        }

        // Not bound here. Refuse if it lives in another namespace; otherwise bind.
        let mut elsewhere = Vec::new();
        for ns_name in namespace::list_names(&repo)? {
            if ns_name == active_ns {
                continue;
            }
            if let Some(h) = namespace::human_for(&repo, &ns_name, &stable)? {
                elsewhere.push(format!("{ns_name}-{h}"));
            }
        }
        if !elsewhere.is_empty() {
            return Err(Error::Parse(format!(
                "task with this content is already bound at {} — use `tsk share {active_ns} -T <id>` or `tsk reopen -T <id>` to bind it into '{active_ns}'",
                elsewhere.join(", ")
            )));
        }
        let obj = Self::read_task_obj(&repo, &stable)?;
        let human = namespace::assign_id(&repo, &active_ns, stable.clone(), "assign-id")?;
        Ok(Self::make_task(Id(human), stable, obj))
    }

    pub fn task(&self, identifier: TaskIdentifier) -> Result<Task> {
        let (id, stable) = self.resolve(identifier)?;
        let obj = Self::read_task_obj(&self.repo()?, &stable)?;
        Ok(Self::make_task(id, stable, obj))
    }

    pub fn task_in_namespace(&self, name: &str, id: Id) -> Result<Task> {
        namespace::validate_name(name)?;
        let repo = self.repo()?;
        let stable = namespace::lookup(&repo, name, id.0)?
            .ok_or_else(|| Error::Parse(format!("Task {name}/{id} not found in namespace")))?;
        let obj = Self::read_task_obj(&repo, &stable)?;
        Ok(Self::make_task(id, stable, obj))
    }

    /// Persist any in-memory edits to a task. Returns `true` when the
    /// underlying `object::update` actually wrote a new commit (i.e. the
    /// resulting tree differs from the current tip); `false` on a no-op.
    pub fn save_task(&self, task: &Task) -> Result<bool> {
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
        let wrote = object::update(&repo, &task.stable, &task_obj, "edit")?;
        properties::reindex_task(&repo, &task.stable, &task.attributes)?;
        Ok(wrote)
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
        self.save_task(&task)?;
        Ok(())
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
        self.save_task(&task)?;
        Ok(())
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
        self.save_task(&task)?;
        Ok(())
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
        let by_stable = ns_reverse(&namespace::read(&repo, &self.namespace())?);
        let mut out = Vec::new();
        for stable in properties::find(&repo, key, value)? {
            let Some(&human) = by_stable.get(&stable) else {
                continue;
            };
            out.push((Id(human), stable.clone(), Self::title_for(&repo, &stable)?));
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
        let by_stable = ns_reverse(&namespace::read(&repo, &self.namespace())?);
        let mut out = Vec::new();
        for stable in queue::read(&repo, &self.queue())?.index {
            // Skip tasks not visible in the active namespace (different ns owns them).
            let Some(&human) = by_stable.get(&stable) else {
                continue;
            };
            let title = Self::title_for(&repo, &stable)?;
            out.push(StackEntry {
                id: Id(human),
                stable,
                title,
            });
        }
        Ok(out)
    }

    /// Every (human id, stable id, title) bound in the given namespace,
    /// sorted by human id ascending. Independent of any queue.
    pub fn list_namespace_tasks(&self, name: &str) -> Result<Vec<StackEntry>> {
        let repo = self.repo()?;
        let mut out = Vec::new();
        for (human, stable) in namespace::read(&repo, name)?.mapping {
            let title = Self::title_for(&repo, &stable)?;
            out.push(StackEntry {
                id: Id(human),
                stable,
                title,
            });
        }
        Ok(out)
    }

    /// One commit on a tsk ref (task / namespace / queue).
    pub fn log_ref(&self, refname: &str) -> Result<Vec<LogCommit>> {
        let repo = self.repo()?;
        let Ok(r) = repo.find_reference(refname) else {
            return Err(Error::Parse(format!("ref {refname} not found")));
        };
        let Some(target) = r.target() else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut current = repo.find_commit(target).ok();
        while let Some(c) = current {
            out.push(LogCommit {
                oid: c.id().to_string(),
                timestamp: c.time().seconds(),
                author: format!(
                    "{} <{}>",
                    c.author().name().unwrap_or(""),
                    c.author().email().unwrap_or("")
                ),
                summary: c.summary().ok().flatten().unwrap_or("").to_string(),
            });
            current = c.parent(0).ok();
        }
        Ok(out)
    }

    /// Export a task as an mbox-format patch series. With `bind=true`, the
    /// root entry carries the active namespace's human id so the recipient
    /// can opt in to mirroring the binding on import.
    #[allow(dead_code)] // single-task wrapper, kept for callers that don't care about batch
    pub fn export_task(&self, identifier: TaskIdentifier, bind: bool) -> Result<String> {
        self.export_tasks(&[identifier], bind)
    }

    /// Export multiple tasks as a single concatenated mbox stream.
    /// Each task's full commit chain is emitted in order; the importer
    /// groups them back by stable id.
    pub fn export_tasks(&self, identifiers: &[TaskIdentifier], bind: bool) -> Result<String> {
        let repo = self.repo()?;
        let mut out = String::new();
        for ident in identifiers {
            let (id, stable) = self.resolve(ident.clone())?;
            let opts = patch::ExportOpts {
                bind: if bind {
                    Some((self.namespace(), id.0))
                } else {
                    None
                },
            };
            out.push_str(&patch::export_task(&repo, &stable, &opts)?);
        }
        Ok(out)
    }

    /// Import a task from an mbox patch series produced by `export_task`.
    /// On `bind=true`, also bind the imported stable id into the active
    /// namespace (reusing the existing human id if already bound).
    pub fn import_task(&self, mbox: &str, bind: bool) -> Result<Vec<ImportOutcome>> {
        let repo = self.repo()?;
        let results = patch::import_mbox(&repo, mbox)?;
        let mut out = Vec::with_capacity(results.len());
        for res in results {
            let bound_human = if bind {
                Some(namespace::ensure_bound(
                    &repo,
                    &self.namespace(),
                    res.stable.clone(),
                    "import-bind",
                )?)
            } else {
                None
            };
            out.push(ImportOutcome {
                stable: res.stable,
                commits_imported: res.commits_imported,
                bound_human,
            });
        }
        Ok(out)
    }

    /// History of edits to a single task.
    pub fn log_task(&self, identifier: TaskIdentifier) -> Result<Vec<LogCommit>> {
        let (_, stable) = self.resolve(identifier)?;
        self.log_ref(&stable.refname())
    }

    /// History of edits to a namespace's tree (id assignments, drops, shares).
    pub fn log_namespace(&self, name: &str) -> Result<Vec<LogCommit>> {
        self.log_ref(&namespace::refname(name))
    }

    /// History of edits to a queue's tree (pushes, drops, inbox moves).
    pub fn log_queue(&self, name: &str) -> Result<Vec<LogCommit>> {
        self.log_ref(&queue::refname(name))
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

    /// Re-save every task in the active namespace whose property blobs are
    /// in the legacy line-split encoding, rewriting them as size-prefixed.
    /// Returns the number of tasks rewritten. Idempotent — `save_task`
    /// no-ops on already-migrated tasks.
    pub fn migrate_property_encoding(&self) -> Result<usize> {
        let ns = namespace::read(&self.repo()?, &self.namespace())?;
        let mut rewritten = 0;
        for human in ns.mapping.keys() {
            let task = self.task(TaskIdentifier::Id(Id(*human)))?;
            if self.save_task(&task)? {
                rewritten += 1;
            }
        }
        Ok(rewritten)
    }

    /// Prune empty / orphan refs under `refs/tsk/*` and recover from
    /// partial multi-ref writes. Returns
    /// `(queues_dropped, prop_orphans_dropped, ghost_bindings_dropped, orphan_queue_entries_dropped)`.
    ///
    /// Recovers:
    /// - empty queues (no index, no inbox) other than the default `tsk`
    ///   queue, which always exists by convention;
    /// - property index entries pointing at task refs that no longer
    ///   resolve (the index ref itself is auto-deleted by `properties::set`
    ///   when its last entry goes);
    /// - ghost namespace bindings (`human → stable` where stable's task
    ///   ref doesn't resolve) — left behind if a crash hit between
    ///   `object::create` and `namespace::assign_id` and the task object
    ///   was later GC'd, or if a remote namespace ref was force-pushed
    ///   ahead of its task refs;
    /// - queue index entries pointing at missing task refs (same root
    ///   cause); also covered by `tsk clean` for the active queue.
    ///
    /// Task object refs are left alone — task history is preserved
    /// intentionally — and namespace `next` counters are valid even when
    /// no live binding uses the latest id.
    pub fn gc_refs(&self) -> Result<(usize, usize, usize, usize)> {
        let repo = self.repo()?;
        let task_exists = |s: &StableId| repo.find_reference(&s.refname()).is_ok();
        let mut queues_pruned = 0;
        let mut prop_orphans = 0;
        let mut ghost_bindings = 0;
        let mut orphan_queue_entries = 0;

        // Empty queues + orphan queue index entries.
        for name in queue::list_names(&repo)? {
            let mut q = queue::read(&repo, &name)?;
            let before = q.index.len();
            q.index.retain(&task_exists);
            if q.index.len() != before {
                let removed = before - q.index.len();
                orphan_queue_entries += removed;
                queue::write(&repo, &name, &q, "gc-orphan-queue")?;
            }
            if name != queue::DEFAULT_QUEUE && q.index.is_empty() && q.inbox.is_empty() {
                if let Ok(mut r) = repo.find_reference(&queue::refname(&name)) {
                    r.delete()?;
                    queues_pruned += 1;
                }
            }
        }

        // Orphan property index entries.
        for key in properties::list_keys(&repo)? {
            for (stable, _vals) in properties::read(&repo, &key)? {
                if !task_exists(&stable) {
                    properties::set(&repo, &key, &stable, &[], "gc-orphan")?;
                    prop_orphans += 1;
                }
            }
        }

        // Ghost namespace bindings (human → stable with no task ref).
        for ns_name in namespace::list_names(&repo)? {
            let mut ns = namespace::read(&repo, &ns_name)?;
            let before = ns.mapping.len();
            ns.mapping.retain(|_, s| task_exists(s));
            if ns.mapping.len() != before {
                ghost_bindings += before - ns.mapping.len();
                namespace::write(&repo, &ns_name, &ns, "gc-ghost-bindings")?;
            }
        }

        Ok((
            queues_pruned,
            prop_orphans,
            ghost_bindings,
            orphan_queue_entries,
        ))
    }

    /// Flip a task back to `status=open` and push it to the top of the
    /// active queue. Idempotent — already-open tasks are unchanged on
    /// disk; the queue push deduplicates on the existing entry.
    pub fn reopen(&self, identifier: TaskIdentifier) -> Result<Id> {
        let mut task = self.task(identifier)?;
        task.attributes
            .insert(STATUS_KEY.into(), vec![STATUS_OPEN.into()]);
        self.save_task(&task)?;
        queue::push_top(&self.repo()?, &self.queue(), task.stable, "reopen")?;
        Ok(task.id)
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
            if to_front {
                "prioritize"
            } else {
                "deprioritize"
            },
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
    ) -> Result<(String, StableId)> {
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
        Ok((key, stable))
    }

    pub fn list_inbox(&self) -> Result<Vec<InboxItem>> {
        let repo = self.repo()?;
        let mut out = Vec::new();
        for (key, stable) in queue::read(&repo, &self.queue())?.inbox {
            let source_queue = key
                .rsplit_once('-')
                .map(|(s, _)| s.to_string())
                .unwrap_or_else(|| key.clone());
            let title = Self::title_for(&repo, &stable)?;
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
        let human =
            namespace::ensure_bound(&repo, &self.namespace(), stable.clone(), "accept-bind")?;
        queue::push_top(&repo, &self.queue(), stable, "accept-push")?;
        Ok(Id(human))
    }

    /// Reject an inbox item: remove it from the active queue's inbox and
    /// bounce it back to the sender's inbox so they see the return. The
    /// source queue is recovered from the key (`<src>-<seq>`); the return
    /// key is `<active>-<seq>` so each round-trip is uniquely identified.
    pub fn reject_inbox(&self, key: &str) -> Result<()> {
        let repo = self.repo()?;
        let stable = queue::take_from_inbox(&repo, &self.queue(), key, "reject")?
            .ok_or_else(|| Error::Parse(format!("Inbox item '{key}' not found")))?;
        if let Some((src, seq)) = key.rsplit_once('-') {
            let cur = self.queue();
            if src != cur {
                let return_key = format!("{cur}-{seq}");
                queue::add_to_inbox(&repo, src, return_key, stable, "reject-return")?;
            }
        }
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
        let human = namespace::ensure_bound(&repo, &self.namespace(), stable, "pull-bind")?;
        Ok(Id(human))
    }

    fn git(&self) -> std::process::Command {
        let mut c = std::process::Command::new("git");
        c.arg("--git-dir").arg(&self.git_dir);
        c
    }

    pub fn configure_git_remote_refspecs(&self, remote: &str) -> Result<()> {
        for (key, value) in [
            (format!("remote.{remote}.push"), "refs/tsk/*:refs/tsk/*"),
            (format!("remote.{remote}.fetch"), "+refs/tsk/*:refs/tsk/*"),
        ] {
            let existing = self.git().args(["config", "--get-all", &key]).output()?;
            if String::from_utf8_lossy(&existing.stdout)
                .lines()
                .any(|l| l.trim() == value)
            {
                continue;
            }
            if !self
                .git()
                .args(["config", "--add", &key, value])
                .status()?
                .success()
            {
                return Err(Error::Parse("git config failed".into()));
            }
        }
        Ok(())
    }

    pub fn git_push(&self, remote: &str) -> Result<()> {
        if !self
            .git()
            .args(["push", remote, "refs/tsk/*:refs/tsk/*"])
            .status()?
            .success()
        {
            return Err(Error::Parse("git push failed".into()));
        }
        Ok(())
    }

    #[allow(dead_code)] // CLI calls git_pull_with_strategy directly; kept for API symmetry.
    pub fn git_pull(&self, remote: &str) -> Result<()> {
        self.git_pull_with_strategy(remote, merge::Strategy::default())?;
        Ok(())
    }

    /// Push only the named refs to `remote`. Each ref is sent as its own
    /// `<ref>:<ref>` refspec (no force) so the operation refuses
    /// non-fast-forward updates the same way `git_push` does.
    pub fn git_push_refs(&self, remote: &str, refs: &[String]) -> Result<()> {
        if refs.is_empty() {
            return Ok(());
        }
        let mut cmd = self.git();
        cmd.args(["push", remote]);
        for r in refs {
            cmd.arg(format!("{r}:{r}"));
        }
        if !cmd.status()?.success() {
            return Err(Error::Parse("git push failed".into()));
        }
        Ok(())
    }

    /// Fetch only the named refs from `remote`, force-updating each. Used
    /// by paths (e.g. `tsk inbox`) that need a single ref refreshed without
    /// the wire cost of a full `git_pull`.
    pub fn git_fetch_refs(&self, remote: &str, refs: &[String]) -> Result<()> {
        if refs.is_empty() {
            return Ok(());
        }
        let mut cmd = self.git();
        cmd.args(["fetch", "--refmap=", remote]);
        for r in refs {
            cmd.arg(format!("+{r}:{r}"));
        }
        if !cmd.status()?.success() {
            return Err(Error::Parse("git fetch failed".into()));
        }
        Ok(())
    }

    /// Refs to push after `assign_to_queue`: the target queue (gained an
    /// inbox entry), the task ref itself (so the receiver can read the
    /// body), and every property index that already references this task.
    pub fn refs_for_assign_out(
        &self,
        target_queue: &str,
        stable: &StableId,
    ) -> Result<Vec<String>> {
        let repo = self.repo()?;
        let mut refs = vec![queue::refname(target_queue), stable.refname()];
        for key in properties::list_keys(&repo)? {
            let entries = properties::read(&repo, &key)?;
            if entries.contains_key(stable) {
                refs.push(properties::refname(&key));
            }
        }
        Ok(refs)
    }

    /// Refs to push after `accept_inbox`: the active queue (entry moved
    /// from inbox to index) and the active namespace (the receiver may have
    /// allocated a new human id binding the accepted task).
    pub fn refs_for_accept_inbox(&self) -> Vec<String> {
        vec![
            queue::refname(&self.queue()),
            namespace::refname(&self.namespace()),
        ]
    }

    /// Refs to push after `reject_inbox`: the active queue (entry left the
    /// inbox) and the source queue (entry was bounced back into its inbox).
    pub fn refs_for_reject_inbox(&self, source_queue: &str) -> Vec<String> {
        vec![queue::refname(&self.queue()), queue::refname(source_queue)]
    }

    /// Refs to fetch before listing the inbox: just the active queue.
    pub fn refs_for_inbox_pull(&self) -> Vec<String> {
        vec![queue::refname(&self.queue())]
    }

    /// Fetch into a non-clobbering shadow namespace, then reconcile each
    /// task ref under the chosen strategy (default `merge`). Non-task refs
    /// (namespaces/queues/property indices) still force-update from the
    /// remote — better merging for those is tracked separately.
    pub fn git_pull_with_strategy(
        &self,
        remote: &str,
        strategy: merge::Strategy,
    ) -> Result<merge::PullOutcome> {
        // `--refmap=` disables the remote's configured fetch refspec so our
        // explicit refspec is the *only* one applied; otherwise git also
        // performs the configured `+refs/tsk/*:refs/tsk/*` mapping and
        // clobbers local task refs before we get a chance to reconcile.
        let refspec = format!("+refs/tsk/*:{}{remote}/*", merge::FETCH_PREFIX);
        if !self
            .git()
            .args(["fetch", "--prune", "--refmap=", remote])
            .arg(&refspec)
            .status()?
            .success()
        {
            return Err(Error::Parse("git fetch failed".into()));
        }
        let repo = self.repo()?;
        let tasks = merge::reconcile_task_refs(&repo, remote, strategy)?;
        let namespaces = merge::reconcile_namespace_refs(&repo, remote)?;
        let queues = merge::reconcile_queue_refs(&repo, remote)?;
        merge::fast_forward_non_task_refs(&repo, remote)?;
        Ok(merge::PullOutcome {
            tasks,
            namespaces,
            queues,
        })
    }
}

/// `stable → human` reverse of a namespace mapping for O(log n) visibility checks.
fn ns_reverse(ns: &namespace::Namespace) -> BTreeMap<StableId, u32> {
    ns.mapping.iter().map(|(h, s)| (s.clone(), *h)).collect()
}

pub fn find_git_dir(start: &std::path::Path) -> Option<PathBuf> {
    let repo = Repository::discover(start).ok()?;
    let git_dir = repo.path();
    Some(
        git_dir
            .canonicalize()
            .unwrap_or_else(|_| git_dir.to_path_buf()),
    )
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
            .unwrap()
            .0;
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
    fn reject_returns_to_source_inbox() {
        let (_d, ws) = fresh_workspace();
        ws.create_queue("review", None).unwrap();
        let t = ws.new_task("bounce me".into(), "".into()).unwrap();
        let id = t.id;
        let stable = t.stable.clone();
        ws.push_task(t).unwrap();
        let assign_key = ws
            .assign_to_queue(TaskIdentifier::Id(id), "review")
            .unwrap()
            .0;
        ws.switch_queue("review").unwrap();
        ws.reject_inbox(&assign_key).unwrap();
        let inbox_here = ws.list_inbox().unwrap();
        assert!(
            inbox_here.is_empty(),
            "rejected item must leave receiver inbox"
        );
        ws.switch_queue("tsk").unwrap();
        let returned = ws.list_inbox().unwrap();
        assert_eq!(returned.len(), 1, "rejected item must land in sender inbox");
        assert_eq!(returned[0].source_queue, "review");
        assert_eq!(returned[0].stable, stable);
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
        let pulled = ws
            .pull_from_queue("private", TaskIdentifier::Id(id))
            .unwrap();
        assert_eq!(pulled.0, id.0);
        let stack = ws.read_stack().unwrap();
        assert_eq!(stack.len(), 1);
    }

    #[test]
    fn list_namespace_tasks_shows_all_bindings_including_dropped() {
        let (_d, ws) = fresh_workspace();
        let t1 = ws.new_task("alpha".into(), "".into()).unwrap();
        let id1 = t1.id;
        ws.push_task(t1).unwrap();
        let t2 = ws.new_task("beta".into(), "".into()).unwrap();
        let id2 = t2.id;
        ws.push_task(t2).unwrap();
        // Dropped tasks keep their namespace binding (per status property design).
        ws.drop(TaskIdentifier::Id(id1)).unwrap();

        let listed = ws.list_namespace_tasks("tsk").unwrap();
        let ids: Vec<_> = listed.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![id1, id2]);
        let titles: Vec<_> = listed.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, vec!["alpha", "beta"]);
    }

    #[test]
    fn log_task_walks_commit_chain_newest_first() {
        let (_d, ws) = fresh_workspace();
        let t = ws.new_task("v1".into(), "".into()).unwrap();
        let id = t.id;
        ws.push_task(t).unwrap();

        // Two edits (each appends a commit).
        let mut t = ws.task(TaskIdentifier::Id(id)).unwrap();
        t.title = "v2".into();
        ws.save_task(&t).unwrap();
        let mut t = ws.task(TaskIdentifier::Id(id)).unwrap();
        t.title = "v3".into();
        ws.save_task(&t).unwrap();

        let log = ws.log_task(TaskIdentifier::Id(id)).unwrap();
        // create + 2 edits = 3 commits.
        assert_eq!(log.len(), 3);
        // Newest first.
        assert_eq!(log[0].summary, "edit");
        assert_eq!(log[1].summary, "edit");
        assert_eq!(log[2].summary, "create");
    }

    #[test]
    fn log_namespace_walks_id_assignments() {
        let (_d, ws) = fresh_workspace();
        let t1 = ws.new_task("a".into(), "".into()).unwrap();
        ws.push_task(t1).unwrap();
        let t2 = ws.new_task("b".into(), "".into()).unwrap();
        ws.push_task(t2).unwrap();

        let log = ws.log_namespace("tsk").unwrap();
        // Two id-assignments.
        assert!(log.len() >= 2, "got {}", log.len());
        assert_eq!(log[0].summary, "assign-id tsk-2");
        assert_eq!(log[1].summary, "assign-id tsk-1");
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
        let opens = ws.find_by_property(STATUS_KEY, Some(STATUS_OPEN)).unwrap();
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
        let dones = ws.find_by_property(STATUS_KEY, Some(STATUS_DONE)).unwrap();
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
    fn duplicate_content_in_active_ns_done_reopens() {
        let (_d, ws) = fresh_workspace();
        let t = ws.new_task("clean kitchen".into(), "".into()).unwrap();
        let original_id = t.id;
        let stable = t.stable.clone();
        ws.push_task(t).unwrap();
        ws.drop(TaskIdentifier::Id(original_id)).unwrap();
        // Task is now status=done. Re-creating with the same content should
        // reopen rather than mint a new id.
        let again = ws.new_task("clean kitchen".into(), "".into()).unwrap();
        assert_eq!(again.id, original_id, "reopened task keeps its human id");
        assert_eq!(again.stable, stable);
        assert_eq!(
            again.attributes.get(STATUS_KEY).unwrap(),
            &vec![STATUS_OPEN.to_string()],
            "reopen flips status back to open"
        );
    }

    #[test]
    fn duplicate_content_open_in_active_ns_is_idempotent() {
        let (_d, ws) = fresh_workspace();
        let first = ws.new_task("write report".into(), "".into()).unwrap();
        let id = first.id;
        ws.push_task(first).unwrap();
        let second = ws.new_task("write report".into(), "".into()).unwrap();
        assert_eq!(second.id, id, "same content returns the same id");
    }

    #[test]
    fn duplicate_content_bound_only_in_other_ns_errors() {
        let (_d, ws) = fresh_workspace();
        let t = ws.new_task("file taxes".into(), "".into()).unwrap();
        let id = t.id;
        ws.push_task(t).unwrap();
        // Move the binding from the default `tsk` namespace into `alpha`,
        // leaving the active `tsk` namespace without a binding.
        ws.share(TaskIdentifier::Id(id), "alpha").unwrap();
        // Manually unbind from `tsk` (simulating "this content lives only
        // in another namespace").
        let repo = ws.repo().unwrap();
        namespace::unassign_id(&repo, "tsk", id.0, "test-unbind").unwrap();
        // Now creating with the same content should refuse.
        let err = ws
            .new_task("file taxes".into(), "".into())
            .expect_err("must error when content lives only in another ns");
        let msg = format!("{err}");
        assert!(
            msg.contains("alpha-"),
            "error should reference the foreign binding: {msg}"
        );
    }

    #[test]
    fn duplicate_content_unbound_everywhere_binds_in_active_ns() {
        let (_d, ws) = fresh_workspace();
        let t = ws.new_task("legacy task".into(), "".into()).unwrap();
        let id = t.id;
        let stable = t.stable.clone();
        ws.push_task(t).unwrap();
        // Forcibly unbind everywhere to simulate an orphaned task ref.
        let repo = ws.repo().unwrap();
        namespace::unassign_id(&repo, "tsk", id.0, "test-unbind").unwrap();
        // Same content again should re-bind into active ns with a new id.
        let again = ws.new_task("legacy task".into(), "".into()).unwrap();
        assert_eq!(
            again.stable, stable,
            "stable id must match the orphaned ref"
        );
        assert_ne!(
            again.id, id,
            "rebinding allocates a fresh human id from `next`"
        );
    }

    #[test]
    fn gc_refs_prunes_all_drift_classes() {
        let (_d, ws) = fresh_workspace();
        let repo = ws.repo().unwrap();

        // 1. Empty non-default queue → pruned.
        ws.create_queue("empty", None).unwrap();

        // 2. Orphan property index entry pointing at a missing task ref.
        let orphan = StableId("0".repeat(40));
        properties::set(&repo, "ghost", &orphan, &["x".into()], "test").unwrap();

        // 3. Ghost namespace binding: assign_id binds before the task ref
        //    exists (simulating a partial multi-ref write).
        namespace::assign_id(&repo, "tsk", orphan.clone(), "ghost-bind").unwrap();

        // 4. Orphan queue index entry pointing at the same missing stable.
        queue::push_top(&repo, "tsk", orphan.clone(), "orphan-push").unwrap();

        let (queues, props, ghosts, qe) = ws.gc_refs().unwrap();
        assert_eq!(queues, 1, "empty queue pruned");
        assert_eq!(props, 1, "orphan property entry pruned");
        assert_eq!(ghosts, 1, "ghost namespace binding dropped");
        assert_eq!(qe, 1, "orphan queue index entry dropped");
        assert!(repo.find_reference(&queue::refname("empty")).is_err());
        assert!(repo.find_reference(&properties::refname("ghost")).is_err());
        assert!(
            namespace::human_for(&repo, "tsk", &orphan)
                .unwrap()
                .is_none()
        );

        // Idempotent: second pass changes nothing.
        assert_eq!(ws.gc_refs().unwrap(), (0, 0, 0, 0));
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
