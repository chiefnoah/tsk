#![allow(dead_code)]
//! High-level workspace API. The workspace owns a [`Store`](crate::backend::Store)
//! and exposes typed task / stack / remote operations on top of it.

use crate::backend::{self, Loc, Store};
use crate::errors::{Error, Result};
use crate::stack::{StackItem, TaskStack};
use crate::task::parse as parse_task;
use crate::{fzf, util};
use std::collections::{BTreeMap, HashSet};
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

enum PullAction {
    /// Local already matches remote — nothing to do.
    Skip,
    /// Take the remote OID verbatim (local missing or unchanged since last sync).
    Take,
    /// Both sides moved; the ref is union-mergeable, do a 3-way merge.
    Merge,
    /// Both sides moved and the ref is not auto-mergeable.
    Conflict,
}

fn is_mergeable_key(rel: &str) -> bool {
    rel.starts_with("log/") || rel.ends_with("/log") || rel == "index" || rel.ends_with("/index")
}

fn resolve_pull(
    local: Option<git2::Oid>,
    old_remote: Option<git2::Oid>,
    new_remote: git2::Oid,
    rel: &str,
) -> PullAction {
    match local {
        None => PullAction::Take,
        Some(l) if l == new_remote => PullAction::Skip,
        Some(l) => match old_remote {
            // Local hasn't moved since last sync; remote did → take remote.
            Some(o) if o == l => PullAction::Take,
            // Remote hasn't moved since last sync; local did → keep local.
            Some(o) if o == new_remote => PullAction::Skip,
            // Either no shared base, or both moved.
            _ => {
                if is_mergeable_key(rel) {
                    PullAction::Merge
                } else {
                    PullAction::Conflict
                }
            }
        },
    }
}

/// Union of two append-only logs, sorted by the leading unix timestamp on
/// each line. Duplicate lines collapse.
fn merge_log(local: &str, remote: &str) -> String {
    let mut all: Vec<&str> = local.lines().chain(remote.lines()).collect();
    all.sort_by_key(|l| {
        l.split('\t')
            .next()
            .and_then(|t| t.parse::<u64>().ok())
            .unwrap_or(0)
    });
    all.dedup();
    let mut out = all.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Union of two stack indexes preserving local order; remote-only items get
/// appended in their relative order. Items are identified by their leading
/// `tsk-N` field.
fn merge_index(local: &str, remote: &str) -> String {
    let key = |line: &str| line.split('\t').next().unwrap_or("").to_string();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = String::new();
    for line in local.lines() {
        if line.trim().is_empty() {
            continue;
        }
        seen.insert(key(line));
        out.push_str(line);
        out.push('\n');
    }
    for line in remote.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if seen.insert(key(line)) {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Reject namespace names that contain `/` or other characters problematic in
/// a git ref path.
fn validate_namespace(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::Parse("Namespace name cannot be empty".into()));
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return Err(Error::Parse(format!(
            "Namespace '{name}' must contain only alphanumerics, '-', or '_'"
        )));
    }
    Ok(())
}

/// Summary of one item in a namespace inbox.
pub struct InboxItem {
    pub inbox_key: String,
    pub source_namespace: String,
    pub source_id: u32,
    pub title: String,
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

    /// Name of the namespace this workspace is currently using. Always
    /// `"default"` for file-backed workspaces.
    pub fn namespace(&self) -> String {
        backend::read_namespace(&self.path)
    }

    /// List the namespaces present in the underlying git repo. Errors for
    /// file-backed workspaces.
    pub fn list_namespaces(&self) -> Result<Vec<String>> {
        if !self.is_git_backed() {
            return Err(Error::Parse("Workspace is not git-backed".into()));
        }
        let marker = std::fs::read_to_string(self.path.join(backend::GIT_BACKED_MARKER))?;
        let store = backend::GitStore::open(PathBuf::from(marker.trim()))?;
        store.list_namespaces()
    }

    /// Switch the workspace to a different namespace by writing the namespace
    /// marker file. The namespace need not exist yet — the next mutation
    /// creates refs under it.
    pub fn switch_namespace(&self, name: &str) -> Result<()> {
        if !self.is_git_backed() {
            return Err(Error::Parse("Workspace is not git-backed".into()));
        }
        validate_namespace(name)?;
        backend::write_namespace(&self.path, name)
    }

    /// Delete every ref belonging to the given namespace. Errors if the
    /// namespace is the currently active one. Returns the number of refs
    /// deleted (caller can prompt before invoking if non-zero).
    pub fn delete_namespace(&self, name: &str) -> Result<usize> {
        if !self.is_git_backed() {
            return Err(Error::Parse("Workspace is not git-backed".into()));
        }
        if name == self.namespace() {
            return Err(Error::Parse(
                "Cannot delete the currently active namespace; switch first".into(),
            ));
        }
        let marker = std::fs::read_to_string(self.path.join(backend::GIT_BACKED_MARKER))?;
        let store =
            backend::GitStore::open_namespace(PathBuf::from(marker.trim()), name.to_string())?;
        store.delete_namespace_refs()
    }

    /// Number of refs currently in the given namespace; useful for prompting
    /// before deletion.
    pub fn namespace_ref_count(&self, name: &str) -> Result<usize> {
        if !self.is_git_backed() {
            return Err(Error::Parse("Workspace is not git-backed".into()));
        }
        let marker = std::fs::read_to_string(self.path.join(backend::GIT_BACKED_MARKER))?;
        let store =
            backend::GitStore::open_namespace(PathBuf::from(marker.trim()), name.to_string())?;
        store.namespace_ref_count()
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
        self.log(id, "created", None)?;
        Ok(Task {
            id,
            title,
            body,
            attributes: Default::default(),
        })
    }

    /// Per-task event log, oldest first.
    pub fn read_log(&self, id: Id) -> Result<Vec<backend::LogEntry>> {
        backend::read_log(self.store(), id)
    }

    /// Every event in this namespace, merged and sorted by timestamp ascending.
    pub fn read_namespace_log(&self) -> Result<Vec<backend::LogEntry>> {
        backend::read_all_logs(self.store())
    }

    fn log(&self, id: Id, event: &str, detail: Option<&str>) -> Result<()> {
        let author = self.git_author().unwrap_or_default();
        backend::append_log(self.store(), id, event, detail, &author)
    }

    /// `Name <email>` from the user's git config, if available. Falls back to
    /// just one of the two if only one is set, or `None` otherwise.
    pub fn git_author(&self) -> Option<String> {
        if !self.is_git_backed() {
            return None;
        }
        let marker = std::fs::read_to_string(self.path.join(backend::GIT_BACKED_MARKER)).ok()?;
        let repo = git2::Repository::open(PathBuf::from(marker.trim())).ok()?;
        let cfg = repo.config().ok()?.snapshot().ok()?;
        let name = cfg.get_string("user.name").ok();
        let email = cfg.get_string("user.email").ok();
        match (name, email) {
            (Some(n), Some(e)) => Some(format!("{n} <{e}>")),
            (Some(n), None) => Some(n),
            (None, Some(e)) => Some(e),
            (None, None) => None,
        }
    }

    pub fn task(&self, identifier: TaskIdentifier) -> Result<Task> {
        let id = self.resolve(identifier)?;
        let (title, body, _loc) = backend::read_task(self.store(), id)?
            .ok_or_else(|| Error::Parse(format!("Task {id} not found")))?;
        Ok(Task {
            id,
            title,
            body,
            attributes: backend::read_attrs(self.store(), id)?,
        })
    }

    pub fn save_task(&self, task: &Task) -> Result<()> {
        let loc = match backend::task_location(self.store(), task.id)? {
            Some(l) => l,
            None => Loc::Active,
        };
        backend::write_task(self.store(), task.id, &task.title, &task.body, loc)?;
        backend::write_attrs(self.store(), task.id, &task.attributes)?;
        self.log(task.id, "edited", None)?;
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

    /// Set a single property (a.k.a attribute) on a task. Empty value is
    /// allowed for unary properties.
    pub fn set_property(&self, id: Id, key: &str, value: &str) -> Result<()> {
        let mut attrs = backend::read_attrs(self.store(), id)?;
        attrs.insert(key.to_string(), value.to_string());
        backend::write_attrs(self.store(), id, &attrs)?;
        self.log(id, "prop-set", Some(key))
    }

    /// Remove a property from a task. No-op if not present.
    pub fn unset_property(&self, id: Id, key: &str) -> Result<()> {
        let mut attrs = backend::read_attrs(self.store(), id)?;
        if attrs.remove(key).is_some() {
            backend::write_attrs(self.store(), id, &attrs)?;
            self.log(id, "prop-unset", Some(key))?;
        }
        Ok(())
    }

    /// All properties on a task, both stored and synthetic (state, has-links,
    /// references, referenced-by).
    pub fn properties(&self, id: Id) -> Result<BTreeMap<String, String>> {
        let mut props = backend::read_attrs(self.store(), id)?;
        let synth = self.synthetic_properties(id)?;
        for (k, v) in synth {
            props.entry(k).or_insert(v);
        }
        Ok(props)
    }

    fn synthetic_properties(&self, id: Id) -> Result<BTreeMap<String, String>> {
        let mut out = BTreeMap::new();
        let Some((_, body, loc)) = backend::read_task(self.store(), id)? else {
            return Ok(out);
        };
        out.insert(
            "state".into(),
            match loc {
                Loc::Active => "open".into(),
                Loc::Archived => "archived".into(),
            },
        );
        let parsed = parse_task(&format!("\n\n{body}"));
        let refs: Vec<String> = parsed
            .as_ref()
            .map(|p| {
                p.intenal_links()
                    .iter()
                    .map(|i| format!("[[{i}]]"))
                    .collect()
            })
            .unwrap_or_default();
        out.insert(
            "has-links".into(),
            if refs.is_empty() { "false" } else { "true" }.into(),
        );
        if !refs.is_empty() {
            out.insert("references".into(), refs.join(","));
        }
        let backrefs = backend::read_backlinks(self.store(), id)?;
        if !backrefs.is_empty() {
            let joined: Vec<String> = backrefs.iter().map(|i| format!("[[{i}]]")).collect();
            out.insert("referenced-by".into(), joined.join(","));
        }
        Ok(out)
    }

    /// Find every task whose property `key` is set (and equals `value`, if
    /// provided). Scans both active and archived. Includes synthetic
    /// properties so `state=archived`, `has-links=true`, etc. work.
    pub fn find_by_property(&self, key: &str, value: Option<&str>) -> Result<Vec<Id>> {
        let mut ids: Vec<Id> = backend::list_active(self.store())?;
        ids.extend(backend::list_archive(self.store())?);
        ids.sort_by_key(|i| i.0);
        ids.dedup();
        Ok(ids
            .into_iter()
            .filter_map(|id| {
                let props = self.properties(id).ok()?;
                let v = props.get(key)?;
                if value.is_none_or(|target| v == target) {
                    Some(id)
                } else {
                    None
                }
            })
            .collect())
    }

    pub fn handle_metadata(&self, tsk: &Task, pre_links: Option<HashSet<Id>>) -> Result<()> {
        if let Some(parsed_task) = parse_task(&tsk.to_string()) {
            let internal_links = parsed_task.intenal_links();
            let added: HashSet<Id> = match &pre_links {
                Some(pre) => internal_links.difference(pre).copied().collect(),
                None => internal_links.clone(),
            };
            for link in &added {
                self.add_backlink(*link, tsk.id)?;
            }
            let mut removed_count = 0;
            if let Some(pre_links) = pre_links {
                for link in pre_links.difference(&internal_links) {
                    self.remove_backlink(*link, tsk.id)?;
                    removed_count += 1;
                }
            }
            if !added.is_empty() || removed_count > 0 {
                self.log(tsk.id, "links-changed", None)?;
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

    /// Run `f` on the workspace stack and persist the result.
    fn mutate_stack<F: FnOnce(&mut TaskStack)>(&self, f: F) -> Result<()> {
        let mut stack = self.read_stack()?;
        f(&mut stack);
        stack.save(self.store())
    }

    pub fn push_task(&self, task: Task) -> Result<()> {
        self.mutate_stack(|s| s.push((&task).into()))
    }

    pub fn append_task(&self, task: Task) -> Result<()> {
        self.mutate_stack(|s| s.push_back((&task).into()))
    }

    pub fn swap_top(&self) -> Result<()> {
        self.mutate_stack(|s| s.swap())
    }

    fn rotate_top3(&self, swap_third_with_top: bool) -> Result<()> {
        self.mutate_stack(|stack| {
            if let (Some(a), Some(b), Some(c)) = (stack.pop(), stack.pop(), stack.pop()) {
                if swap_third_with_top {
                    stack.push(b);
                    stack.push(a);
                    stack.push(c);
                } else {
                    stack.push(a);
                    stack.push(c);
                    stack.push(b);
                }
            }
        })
    }

    pub fn rot(&self) -> Result<()> {
        self.rotate_top3(true)
    }
    pub fn tor(&self) -> Result<()> {
        self.rotate_top3(false)
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
        self.log(id, "archived", None)?;
        Ok(removed)
    }

    pub fn search(
        &self,
        stack: Option<TaskStack>,
        search_body: bool,
        include_archived: bool,
    ) -> Result<Option<Id>> {
        const BODY_ARGS: &[&str] = &[
            "--no-multi-line",
            "--accept-nth=1",
            "--delimiter=\t",
            "--preview=tsk show -T {1}",
            "--preview-window=top",
            "--ansi",
            "--info-command=tsk show -T {1} | head -n1",
            "--info=inline-right",
        ];
        const ID_ARGS: &[&str] = &["--delimiter=\t", "--accept-nth=1"];
        let args = if search_body { BODY_ARGS } else { ID_ARGS };
        let stack = stack.map_or_else(|| self.read_stack(), Ok)?;
        if include_archived {
            let mut seen: HashSet<Id> = HashSet::new();
            let mut all: Vec<SearchTask> = stack
                .iter()
                .filter_map(|item| self.task(TaskIdentifier::Id(item.id)).ok().map(Task::bare))
                .inspect(|t| {
                    seen.insert(t.id);
                })
                .collect();
            for id in backend::list_archive(self.store())? {
                if !seen.contains(&id)
                    && let Some((title, body, _)) = backend::read_task(self.store(), id)?
                {
                    all.push(SearchTask { id, title, body });
                }
            }
            fzf::select::<_, Id, _>(all, args)
        } else if search_body {
            fzf::select::<_, Id, _>(
                stack
                    .into_iter()
                    .filter_map(|item| self.task(TaskIdentifier::Id(item.id)).ok().map(Task::bare)),
                args,
            )
        } else {
            fzf::select::<_, Id, _>(stack, args)
        }
    }

    fn move_in_stack(&self, identifier: TaskIdentifier, to_front: bool) -> Result<()> {
        let id = self.resolve(identifier)?;
        self.mutate_stack(|stack| {
            if let Some(idx) = stack.position(id)
                && let Some(item) = stack.remove(idx)
            {
                if to_front {
                    stack.push(item)
                } else {
                    stack.push_back(item)
                }
            }
        })
    }

    pub fn prioritize(&self, identifier: TaskIdentifier) -> Result<()> {
        self.move_in_stack(identifier, true)
    }

    pub fn deprioritize(&self, identifier: TaskIdentifier) -> Result<()> {
        self.move_in_stack(identifier, false)
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

    fn require_git_dir(&self) -> Result<PathBuf> {
        if !self.is_git_backed() {
            return Err(Error::Parse("Workspace is not git-backed".into()));
        }
        let marker = std::fs::read_to_string(self.path.join(backend::GIT_BACKED_MARKER))?;
        Ok(PathBuf::from(marker.trim()))
    }

    fn git_cmd(&self) -> Result<std::process::Command> {
        let mut c = std::process::Command::new("git");
        c.arg("--git-dir").arg(self.require_git_dir()?);
        Ok(c)
    }

    fn run_git(&self, args: &[&str]) -> Result<()> {
        let status = self.git_cmd()?.args(args).status()?;
        if !status.success() {
            return Err(Error::Parse(format!("git {args:?} exited with {status}")));
        }
        Ok(())
    }

    /// Push every refs/tsk/* ref to the given remote, using the per-ref
    /// `--force-with-lease=<ref>:<expected>` so a concurrent push on the
    /// remote causes our push to fail rather than silently overwrite.
    /// The expected OID is taken from the local
    /// `refs/remotes-tsk/<remote>/*` shadow, which is refreshed first.
    /// After a successful push, the shadow is updated to match the new state.
    pub fn git_push_refs(&self, remote: &str) -> Result<()> {
        let _ = self.require_git_dir()?;
        // Refresh the shadow so leases match the remote's current state.
        let _ = self
            .git_cmd()?
            .args([
                "fetch",
                remote,
                &format!("+refs/tsk/*:refs/remotes-tsk/{remote}/*"),
            ])
            .status()?;
        let shadow: BTreeMap<String, git2::Oid> = self.read_shadow(remote)?.into_iter().collect();

        let repo = git2::Repository::open(self.require_git_dir()?)?;
        let mut leases: Vec<String> = Vec::new();
        let mut refspecs: Vec<String> = Vec::new();
        for r in repo.references()? {
            let r = r?;
            let Some(name) = r.name() else { continue };
            let Some(rest) = name.strip_prefix("refs/tsk/") else {
                continue;
            };
            let Some(local_oid) = r.target() else {
                continue;
            };
            if shadow.get(rest) == Some(&local_oid) {
                continue; // up to date
            }
            if let Some(expected) = shadow.get(rest) {
                leases.push(format!("--force-with-lease=refs/tsk/{rest}:{expected}"));
            }
            refspecs.push(format!("refs/tsk/{rest}:refs/tsk/{rest}"));
        }
        if refspecs.is_empty() {
            return Ok(());
        }
        let mut args: Vec<String> = vec!["push".to_string(), remote.to_string()];
        args.extend(leases);
        args.extend(refspecs);
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run_git(&argv)?;
        self.update_remote_shadow(remote)?;
        Ok(())
    }

    /// Reconcile every refs/tsk/* ref with the remote. Fetch lands in
    /// `refs/remotes-tsk/<remote>/*` (force, since it's our private mirror);
    /// then for each ref we look at three OIDs — local, the previous
    /// fetched-from-remote (the merge base), and the new remote — and pick:
    ///
    ///  - local untouched since last sync → take remote
    ///  - remote untouched since last sync → keep local
    ///  - both moved, ref is union-mergeable (`log/*`, `index`) → 3-way merge
    ///  - both moved, ref is not union-mergeable → conflict; abort with a
    ///    list of the offending refs. The fetch shadow is updated either way
    ///    so a re-run after manual resolution sees the right base.
    pub fn git_pull_refs(&self, remote: &str) -> Result<()> {
        let _ = self.require_git_dir()?;
        // Snapshot pre-fetch shadow so we know the previous remote position.
        let pre_fetch: BTreeMap<String, git2::Oid> =
            self.read_shadow(remote)?.into_iter().collect();
        // Fetch (force, into our private shadow only).
        self.run_git(&[
            "fetch",
            remote,
            &format!("+refs/tsk/*:refs/remotes-tsk/{remote}/*"),
        ])?;
        let post_fetch: BTreeMap<String, git2::Oid> =
            self.read_shadow(remote)?.into_iter().collect();

        let repo = git2::Repository::open(self.require_git_dir()?)?;
        let mut conflicts: Vec<String> = Vec::new();
        for (rel, &new_remote) in &post_fetch {
            let local_refname = format!("refs/tsk/{rel}");
            let local_oid = repo
                .find_reference(&local_refname)
                .ok()
                .and_then(|r| r.target());
            let old_remote = pre_fetch.get(rel).copied();
            match resolve_pull(local_oid, old_remote, new_remote, rel) {
                PullAction::Skip => {}
                PullAction::Take => {
                    repo.reference(&local_refname, new_remote, true, "tsk pull")?;
                }
                PullAction::Merge => {
                    let merged = self.merge_blob(&repo, rel, local_oid, new_remote)?;
                    repo.reference(&local_refname, merged, true, "tsk pull merge")?;
                }
                PullAction::Conflict => conflicts.push(rel.clone()),
            }
        }
        if !conflicts.is_empty() {
            return Err(Error::Parse(format!(
                "pull conflicts on: {}\n(local and remote both diverged from the last sync; \
                 these refs aren't auto-mergeable. Resolve manually with `git update-ref` \
                 or by editing the corresponding tsk objects.)",
                conflicts.join(", ")
            )));
        }
        Ok(())
    }

    /// Read every `refs/remotes-tsk/<remote>/*` and return `(rel, oid)` where
    /// `rel` is the path under that prefix (matches the local-side `rel` used
    /// against `refs/tsk/`).
    fn read_shadow(&self, remote: &str) -> Result<Vec<(String, git2::Oid)>> {
        let repo = git2::Repository::open(self.require_git_dir()?)?;
        let prefix = format!("refs/remotes-tsk/{remote}/");
        let mut out = Vec::new();
        for r in repo.references()? {
            let r = r?;
            if let Some(name) = r.name()
                && let Some(rest) = name.strip_prefix(&prefix)
                && let Some(oid) = r.target()
            {
                out.push((rest.to_string(), oid));
            }
        }
        Ok(out)
    }

    /// After a successful push, copy current local `refs/tsk/*` OIDs into
    /// `refs/remotes-tsk/<remote>/*` so the next pull's merge base is correct.
    fn update_remote_shadow(&self, remote: &str) -> Result<()> {
        let repo = git2::Repository::open(self.require_git_dir()?)?;
        let prefix = "refs/tsk/";
        let dest_prefix = format!("refs/remotes-tsk/{remote}/");
        let updates: Vec<(String, git2::Oid)> = repo
            .references()?
            .filter_map(|r| {
                let r = r.ok()?;
                let name = r.name()?.to_string();
                let oid = r.target()?;
                let rest = name.strip_prefix(prefix)?.to_string();
                Some((rest, oid))
            })
            .collect();
        for (rest, oid) in updates {
            repo.reference(
                &format!("{dest_prefix}{rest}"),
                oid,
                true,
                "tsk push shadow",
            )?;
        }
        Ok(())
    }

    /// Read a blob by its OID.
    fn read_oid(&self, repo: &git2::Repository, oid: git2::Oid) -> Result<Vec<u8>> {
        let blob = repo.find_blob(oid)?;
        Ok(blob.content().to_vec())
    }

    /// Three-way merge for union-mergeable refs (`log/*` and `index`).
    fn merge_blob(
        &self,
        repo: &git2::Repository,
        rel: &str,
        local: Option<git2::Oid>,
        remote: git2::Oid,
    ) -> Result<git2::Oid> {
        let local_bytes = match local {
            Some(o) => self.read_oid(repo, o)?,
            None => Vec::new(),
        };
        let remote_bytes = self.read_oid(repo, remote)?;
        let local_text = String::from_utf8_lossy(&local_bytes);
        let remote_text = String::from_utf8_lossy(&remote_bytes);
        let merged = if rel.starts_with("log/") || rel.ends_with("/log") {
            merge_log(&local_text, &remote_text)
        } else {
            // index: union of stack item lines, preserving local order then
            // appending remote-only items in their relative order.
            merge_index(&local_text, &remote_text)
        };
        Ok(repo.blob(merged.as_bytes())?)
    }

    /// Configure git so future `git push <remote>` / `git fetch <remote>`
    /// include the tsk ref namespace. Idempotent.
    pub fn configure_git_remote_refspecs(&self, remote: &str) -> Result<()> {
        for (key, value) in [
            (format!("remote.{remote}.push"), "refs/tsk/*:refs/tsk/*"),
            (format!("remote.{remote}.fetch"), "+refs/tsk/*:refs/tsk/*"),
        ] {
            let existing = self
                .git_cmd()?
                .args(["config", "--get-all", &key])
                .output()?;
            if String::from_utf8_lossy(&existing.stdout)
                .lines()
                .any(|l| l.trim() == value)
            {
                continue;
            }
            self.run_git(&["config", "--add", &key, value])?;
        }
        Ok(())
    }

    /// Every logical blob key that currently exists in the workspace.
    fn all_keys(&self) -> Result<Vec<String>> {
        let mut keys: Vec<String> = Vec::new();
        for prefix in ["tasks", "archive", "attrs", "backlinks", "log", "inbox"] {
            keys.extend(self.store().list(prefix)?);
        }
        for top in ["index", "next", "remotes"] {
            if self.store().exists(top)? {
                keys.push(top.into());
            }
        }
        keys.sort();
        Ok(keys)
    }

    /// Write a zip archive containing every blob in the workspace. Layout in the
    /// zip mirrors the logical key namespace.
    pub fn export_zip(&self, dest: &std::path::Path) -> Result<()> {
        let mut writer = zip::ZipWriter::new(std::fs::File::create(dest)?);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        use std::io::Write as _;
        for key in self.all_keys()? {
            if let Some(data) = self.store().read(&key)? {
                writer
                    .start_file(&key, opts)
                    .map_err(|e| Error::Parse(format!("zip: {e}")))?;
                writer.write_all(&data)?;
            }
        }
        writer
            .finish()
            .map_err(|e| Error::Parse(format!("zip: {e}")))?;
        Ok(())
    }

    /// Migrate a file-backed workspace to a git-backed one. Returns Err if the
    /// workspace is already git-backed or if no enclosing git repo is found.
    /// Send a task to another namespace's inbox in the same git repo. Sets
    /// `assigned=[[<target_ns>/tsk-<id>]]` on the source after a successful
    /// write so it can be tracked. Returns the inbox key used in the target.
    pub fn export_to_namespace(&self, target_ns: &str, src_id: Id) -> Result<String> {
        if !self.is_git_backed() {
            return Err(Error::Parse(
                "Cross-namespace export only works on git-backed workspaces".into(),
            ));
        }
        validate_namespace(target_ns)?;
        let cur = self.namespace();
        if target_ns == cur {
            return Err(Error::Parse(
                "Refusing to export a task to its own namespace".into(),
            ));
        }
        let task = self.task(TaskIdentifier::Id(src_id))?;
        let attrs = backend::read_attrs(self.store(), src_id)?;
        let payload = backend::InboxPayload {
            source_namespace: cur,
            source_id: src_id.0,
            title: task.title.clone(),
            body: task.body.clone(),
            attrs,
        };
        let marker = std::fs::read_to_string(self.path.join(backend::GIT_BACKED_MARKER))?;
        let target =
            backend::GitStore::open_namespace(PathBuf::from(marker.trim()), target_ns.to_string())?;
        let key = backend::inbox_key(&payload.source_namespace, payload.source_id);
        <dyn Store>::write(&target, &key, payload.serialize().as_bytes())?;

        // Mark the source with where it was sent.
        let assigned_link = format!("[[{target_ns}/tsk-{}]]", src_id.0);
        let mut my_attrs = backend::read_attrs(self.store(), src_id)?;
        my_attrs.insert("assigned".into(), assigned_link.clone());
        backend::write_attrs(self.store(), src_id, &my_attrs)?;
        self.log(src_id, "exported", Some(&assigned_link))?;
        Ok(key)
    }

    /// Item pending in the current namespace's inbox.
    pub fn list_inbox(&self) -> Result<Vec<InboxItem>> {
        let mut out = Vec::new();
        for key in self.store().list("inbox")? {
            if let Some(data) = self.store().read(&key)? {
                let payload = backend::InboxPayload::parse(&String::from_utf8_lossy(&data))?;
                out.push(InboxItem {
                    inbox_key: key,
                    source_namespace: payload.source_namespace,
                    source_id: payload.source_id,
                    title: payload.title,
                });
            }
        }
        out.sort_by(|a, b| a.inbox_key.cmp(&b.inbox_key));
        Ok(out)
    }

    /// Accept a pending inbox item: create a new local task with copied
    /// title/body/attrs, set `source=[[<src-ns>/tsk-<src-id>]]`, push it on
    /// the stack, and remove the inbox blob.
    pub fn accept_inbox(&self, inbox_key: &str) -> Result<Id> {
        let key = if inbox_key.starts_with("inbox/") {
            inbox_key.to_string()
        } else {
            format!("inbox/{inbox_key}")
        };
        let data = self
            .store()
            .read(&key)?
            .ok_or_else(|| Error::Parse(format!("Inbox item '{inbox_key}' not found")))?;
        let payload = backend::InboxPayload::parse(&String::from_utf8_lossy(&data))?;

        let task = self.new_task(payload.title.clone(), payload.body.clone())?;
        let new_id = task.id;
        self.push_task(task)?;

        let mut attrs = payload.attrs;
        attrs.insert(
            "source".into(),
            format!("[[{}/tsk-{}]]", payload.source_namespace, payload.source_id),
        );
        // Drop any "assigned" carried over — it was set by the source workspace
        // before export; the new local copy isn't itself assigned anywhere.
        attrs.remove("assigned");
        backend::write_attrs(self.store(), new_id, &attrs)?;
        self.store().delete(&key)?;
        self.log(
            new_id,
            "accepted",
            Some(&format!(
                "[[{}/tsk-{}]]",
                payload.source_namespace, payload.source_id
            )),
        )?;
        Ok(new_id)
    }

    pub fn migrate_to_git(&self) -> Result<PathBuf> {
        if self.is_git_backed() {
            return Err(Error::Parse("Workspace is already git-backed".into()));
        }
        let git_dir = backend::detect_git_dir(&self.path)
            .ok_or_else(|| Error::Parse("No enclosing git repository found".into()))?;
        let dest = backend::GitStore::open(git_dir.clone())?;
        for key in self.all_keys()? {
            if let Some(data) = self.store().read(&key)? {
                dest.write(&key, &data)?;
            }
        }
        for entry in std::fs::read_dir(&self.path)? {
            let p = entry?.path();
            if p.is_dir() {
                std::fs::remove_dir_all(&p)?
            } else {
                std::fs::remove_file(&p)?
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
        self.log(id, "reopened", None)?;
        Ok(id)
    }
}

pub struct Task {
    pub id: Id,
    pub title: String,
    pub body: String,
    pub attributes: BTreeMap<String, String>,
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
    fn test_export_zip_both_backends() {
        let (_d, file, git) = setup_dual();
        for ws in [&file, &git] {
            let t = ws.new_task("t1".into(), "b1".into()).unwrap();
            let id = t.id;
            ws.push_task(t).unwrap();
            ws.add_remote("up", "/p").unwrap();

            let out = ws.path.join("export.zip");
            ws.export_zip(&out).unwrap();
            assert!(out.exists() && std::fs::metadata(&out).unwrap().len() > 0);

            let f = std::fs::File::open(&out).unwrap();
            let mut zip = zip::ZipArchive::new(f).unwrap();
            let names: std::collections::HashSet<String> = (0..zip.len())
                .map(|i| zip.by_index(i).unwrap().name().to_string())
                .collect();
            assert!(names.contains(&format!("tasks/{}", id.0)));
            assert!(names.contains("index"));
            assert!(names.contains("next"));
            assert!(names.contains("remotes"));

            // Round-trip the task content.
            use std::io::Read as _;
            let mut entry = zip.by_name(&format!("tasks/{}", id.0)).unwrap();
            let mut buf = String::new();
            entry.read_to_string(&mut buf).unwrap();
            assert!(buf.starts_with("t1"));
            assert!(buf.contains("b1"));
        }
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
        assert_eq!(
            backend::task_location(ws2.store(), id2).unwrap(),
            Some(Loc::Archived)
        );
        let bl = backend::read_backlinks(ws2.store(), id2).unwrap();
        assert!(bl.contains(&id1));
        assert_eq!(ws2.read_remotes().unwrap().len(), 1);

        // Migrating an already-git-backed workspace fails.
        assert!(ws2.migrate_to_git().is_err());
    }

    /// Runs through every command's workspace-level logic against `ws`. Mirrors
    /// what main.rs's `command_*` functions do (sans interactive bits like fzf
    /// and $EDITOR).
    fn run_every_command(ws: &Workspace) {
        // command_push (twice): create_task → handle_metadata → push_task
        let t1 = ws.new_task("first".into(), "body1".into()).unwrap();
        let id1 = t1.id;
        ws.handle_metadata(&t1, None).unwrap();
        ws.push_task(t1).unwrap();

        let t2 = ws.new_task("second".into(), "body2".into()).unwrap();
        let id2 = t2.id;
        ws.handle_metadata(&t2, None).unwrap();
        ws.push_task(t2).unwrap();

        // command_append: append_task at the bottom
        let t3 = ws.new_task("third".into(), "".into()).unwrap();
        let id3 = t3.id;
        ws.append_task(t3).unwrap();

        // command_list: stack reads in expected order
        let stack = ws.read_stack().unwrap();
        let order: Vec<_> = stack.iter().map(|i| i.id).collect();
        assert_eq!(order, vec![id2, id1, id3], "{order:?}");

        // command_show: read by id
        let shown = ws.task(TaskIdentifier::Id(id1)).unwrap();
        assert_eq!(shown.title, "first");
        assert_eq!(shown.body, "body1");

        // command_show: read by relative position
        let top = ws.task(TaskIdentifier::Relative(0)).unwrap();
        assert_eq!(top.id, id2);

        // command_edit: this is the regression suspected by the user. Mirror the
        // exact code path command_edit uses, sans open_editor.
        {
            let mut task = ws.task(TaskIdentifier::Id(id1)).unwrap();
            let pre_links = parse_task(&task.to_string()).map(|pt| pt.intenal_links());
            let new_content = format!("edited title [[{id3}]]\n\nedited body");
            let (title, body) = new_content.split_once('\n').unwrap();
            task.title = title.replace(['\n', '\r'], " ");
            task.body = body.to_string();
            ws.handle_metadata(&task, pre_links).unwrap();
            ws.save_task(&task).unwrap();

            let reread = ws.task(TaskIdentifier::Id(id1)).unwrap();
            assert!(reread.title.starts_with("edited title"), "{}", reread.title);
            assert_eq!(reread.body.trim(), "edited body");
            // Stack title refreshed.
            let s = ws.read_stack().unwrap();
            let item = s.iter().find(|i| i.id == id1).unwrap();
            assert!(item.title.starts_with("edited title"));
            // Backlink from id1 → id3 should now exist.
            let bl3 = backend::read_backlinks(ws.store(), id3).unwrap();
            assert!(bl3.contains(&id1), "edit should add backlinks: {bl3:?}");
        }

        // Editing an archived task should leave it archived, not resurrect it.
        ws.drop(TaskIdentifier::Id(id3)).unwrap();
        assert_eq!(
            backend::task_location(ws.store(), id3).unwrap(),
            Some(Loc::Archived)
        );
        {
            let mut task = ws.task(TaskIdentifier::Id(id3)).unwrap();
            task.body = "edited while archived".into();
            ws.save_task(&task).unwrap();
            assert_eq!(
                backend::task_location(ws.store(), id3).unwrap(),
                Some(Loc::Archived),
                "save_task must preserve archive location"
            );
            let reread = ws.task(TaskIdentifier::Id(id3)).unwrap();
            assert_eq!(reread.body, "edited while archived");
        }
        // Reopen so subsequent stack ops have it back.
        ws.reopen(TaskIdentifier::Id(id3)).unwrap();

        // command_swap
        let before: Vec<_> = ws.read_stack().unwrap().iter().map(|i| i.id).collect();
        ws.swap_top().unwrap();
        let after: Vec<_> = ws.read_stack().unwrap().iter().map(|i| i.id).collect();
        assert_eq!(after[0], before[1]);
        assert_eq!(after[1], before[0]);
        ws.swap_top().unwrap();

        // command_rot / command_tor are inverses
        let before: Vec<_> = ws.read_stack().unwrap().iter().map(|i| i.id).collect();
        ws.rot().unwrap();
        ws.tor().unwrap();
        let after: Vec<_> = ws.read_stack().unwrap().iter().map(|i| i.id).collect();
        assert_eq!(before, after);

        // command_prioritize
        ws.prioritize(TaskIdentifier::Id(id1)).unwrap();
        assert_eq!(ws.read_stack().unwrap().iter().next().unwrap().id, id1);

        // command_deprioritize
        ws.deprioritize(TaskIdentifier::Id(id1)).unwrap();
        let s = ws.read_stack().unwrap();
        assert_eq!(s.iter().last().unwrap().id, id1);

        // command_drop
        ws.drop(TaskIdentifier::Id(id1)).unwrap();
        assert!(!ws.read_stack().unwrap().iter().any(|i| i.id == id1));
        assert_eq!(
            backend::task_location(ws.store(), id1).unwrap(),
            Some(Loc::Archived)
        );

        // command_reopen
        ws.reopen(TaskIdentifier::Id(id1)).unwrap();
        assert!(ws.read_stack().unwrap().iter().any(|i| i.id == id1));
        assert_eq!(
            backend::task_location(ws.store(), id1).unwrap(),
            Some(Loc::Active)
        );

        // command_clean: orphan a task in active that isn't on the stack
        backend::write_task(ws.store(), Id(99_999), "orphan", "", Loc::Active).unwrap();
        assert!(
            backend::list_active(ws.store())
                .unwrap()
                .contains(&Id(99_999))
        );
        ws.clean().unwrap();
        assert!(
            !backend::list_active(ws.store())
                .unwrap()
                .contains(&Id(99_999))
        );
        assert!(
            backend::list_archive(ws.store())
                .unwrap()
                .contains(&Id(99_999))
        );

        // command_remote (List/Add/Remove)
        assert!(ws.read_remotes().unwrap().is_empty());
        ws.add_remote("up", "/tmp/p").unwrap();
        assert_eq!(ws.read_remotes().unwrap().len(), 1);
        assert!(ws.add_remote("up", "/tmp/q").is_err()); // duplicate
        assert!(ws.remove_remote("nope").is_err()); // nonexistent
        ws.remove_remote("up").unwrap();
        assert!(ws.read_remotes().unwrap().is_empty());

        // command_export: writes a zip with all blobs
        let dest = ws.path.join("exp.zip");
        ws.export_zip(&dest).unwrap();
        assert!(dest.exists());
        let f = std::fs::File::open(&dest).unwrap();
        let zip = zip::ZipArchive::new(f).unwrap();
        assert!(zip.len() >= 2, "export contains at least index + tasks");
        std::fs::remove_file(&dest).unwrap();
    }

    #[test]
    fn test_every_command_file_backend() {
        let dir = tempfile::tempdir().unwrap();
        Workspace::init(dir.path().to_path_buf()).unwrap();
        let ws = Workspace::from_path(dir.path().to_path_buf()).unwrap();
        run_every_command(&ws);
    }

    #[test]
    fn test_every_command_git_backend() {
        let dir = tempfile::tempdir().unwrap();
        run_git_init(dir.path());
        Workspace::init(dir.path().to_path_buf()).unwrap();
        let ws = Workspace::from_path(dir.path().to_path_buf()).unwrap();
        run_every_command(&ws);
    }

    /// Two clones diverge: clone A pushes, clone B edits locally, then B
    /// pulls. Mergeable refs (index, log) auto-merge; a divergent task body
    /// is reported as a conflict.
    #[test]
    fn test_pull_resolves_or_reports_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let remote_dir = dir.path().join("remote.git");
        let a_dir = dir.path().join("a");
        let b_dir = dir.path().join("b");
        std::fs::create_dir_all(&remote_dir).unwrap();
        std::fs::create_dir_all(&a_dir).unwrap();
        std::fs::create_dir_all(&b_dir).unwrap();

        let s = std::process::Command::new("git")
            .args(["init", "--bare", "-q"])
            .current_dir(&remote_dir)
            .status()
            .unwrap();
        assert!(s.success());

        let init_clone = |path: &std::path::Path| {
            run_git_init(path);
            std::process::Command::new("git")
                .args(["remote", "add", "origin"])
                .arg(&remote_dir)
                .current_dir(path)
                .status()
                .unwrap();
            Workspace::init(path.to_path_buf()).unwrap();
            Workspace::from_path(path.to_path_buf()).unwrap()
        };
        let a = init_clone(&a_dir);
        let b = init_clone(&b_dir);

        // A pushes a task that B will start from.
        let t = a.new_task("shared".into(), "v0".into()).unwrap();
        let id = t.id;
        a.push_task(t).unwrap();
        a.git_push_refs("origin").unwrap();
        b.git_pull_refs("origin").unwrap();
        assert_eq!(b.task(TaskIdentifier::Id(id)).unwrap().title, "shared");

        // Both diverge:
        // - A pushes a second task (touches index + new tasks/2 + log/2).
        // - B edits the original task body locally (touches tasks/1 + log/1).
        let t2 = a.new_task("a-only".into(), "v1".into()).unwrap();
        let a2_id = t2.id;
        a.push_task(t2).unwrap();
        a.git_push_refs("origin").unwrap();

        let mut local = b.task(TaskIdentifier::Id(id)).unwrap();
        local.body = "v0-edit".into();
        b.save_task(&local).unwrap();

        // B pulls: tasks/<a2_id> is new → take. index moved both sides → merge.
        // log/<id> moved both sides → merge. tasks/1 moved on B only → keep
        // local. So no conflicts.
        b.git_pull_refs("origin").unwrap();
        // B's edit survived…
        assert_eq!(b.task(TaskIdentifier::Id(id)).unwrap().body, "v0-edit");
        // …and A's new task arrived.
        assert_eq!(b.task(TaskIdentifier::Id(a2_id)).unwrap().title, "a-only");
        // Stack contains both ids.
        let ids: HashSet<Id> = b.read_stack().unwrap().iter().map(|i| i.id).collect();
        assert!(ids.contains(&id));
        assert!(ids.contains(&a2_id));

        // Now both edit the same task body, then B pulls → conflict.
        let mut on_a = a.task(TaskIdentifier::Id(id)).unwrap();
        on_a.body = "a-edit".into();
        a.save_task(&on_a).unwrap();
        a.git_push_refs("origin").unwrap();

        let mut on_b = b.task(TaskIdentifier::Id(id)).unwrap();
        on_b.body = "b-edit".into();
        b.save_task(&on_b).unwrap();

        let err = b.git_pull_refs("origin").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("conflicts on"), "{msg}");
        assert!(
            msg.contains(&format!("default/tasks/{}", id.0)),
            "expected the diverged task ref in error: {msg}"
        );
        // B's local edit is preserved through the failed pull.
        assert_eq!(b.task(TaskIdentifier::Id(id)).unwrap().body, "b-edit");
    }

    #[test]
    fn test_git_push_pull_and_refspec_config() {
        // Set up a bare "remote" and a working repo, both git-backed tsk
        // workspaces. Push from one, pull into another.
        let dir = tempfile::tempdir().unwrap();
        let remote_dir = dir.path().join("remote.git");
        let work_dir = dir.path().join("work");
        let clone_dir = dir.path().join("clone");
        std::fs::create_dir_all(&remote_dir).unwrap();
        std::fs::create_dir_all(&work_dir).unwrap();
        std::fs::create_dir_all(&clone_dir).unwrap();

        // Bare remote.
        let s = std::process::Command::new("git")
            .args(["init", "--bare", "-q"])
            .current_dir(&remote_dir)
            .status()
            .unwrap();
        assert!(s.success());

        // Working repo + tsk init.
        run_git_init(&work_dir);
        Workspace::init(work_dir.clone()).unwrap();
        let ws = Workspace::from_path(work_dir.clone()).unwrap();
        // Add the bare repo as `origin`.
        let s = std::process::Command::new("git")
            .args(["remote", "add", "origin"])
            .arg(&remote_dir)
            .current_dir(&work_dir)
            .status()
            .unwrap();
        assert!(s.success());

        // Pushing without configured refspecs (using the explicit refspec form).
        let t = ws.new_task("task one".into(), "body".into()).unwrap();
        let id = t.id;
        ws.push_task(t).unwrap();
        ws.git_push_refs("origin").unwrap();

        // Confirm refs landed on the remote.
        let out = std::process::Command::new("git")
            .args(["--git-dir"])
            .arg(&remote_dir)
            .args(["for-each-ref", "--format=%(refname)", "refs/tsk/"])
            .output()
            .unwrap();
        let names = String::from_utf8_lossy(&out.stdout);
        assert!(
            names.contains(&format!("refs/tsk/default/tasks/{}", id.0)),
            "{names}"
        );
        assert!(names.contains("refs/tsk/default/index"));

        // Now configure refspecs on the working repo and confirm `git push origin`
        // (with no refspec) sends refs/tsk/*.
        ws.configure_git_remote_refspecs("origin").unwrap();
        let cfg = std::process::Command::new("git")
            .args(["config", "--get-all", "remote.origin.push"])
            .current_dir(&work_dir)
            .output()
            .unwrap();
        let push_cfg = String::from_utf8_lossy(&cfg.stdout);
        assert!(
            push_cfg
                .lines()
                .any(|l| l.trim() == "refs/tsk/*:refs/tsk/*")
        );
        // Idempotent: running again does not duplicate.
        ws.configure_git_remote_refspecs("origin").unwrap();
        let cfg2 = std::process::Command::new("git")
            .args(["config", "--get-all", "remote.origin.push"])
            .current_dir(&work_dir)
            .output()
            .unwrap();
        let push_cfg2 = String::from_utf8_lossy(&cfg2.stdout);
        assert_eq!(
            push_cfg
                .lines()
                .filter(|l| l.trim() == "refs/tsk/*:refs/tsk/*")
                .count(),
            push_cfg2
                .lines()
                .filter(|l| l.trim() == "refs/tsk/*:refs/tsk/*")
                .count()
        );

        // Pull side: a fresh repo set up to fetch from the same remote, then
        // pulling tsk refs in.
        run_git_init(&clone_dir);
        Workspace::init(clone_dir.clone()).unwrap();
        let cws = Workspace::from_path(clone_dir.clone()).unwrap();
        let s = std::process::Command::new("git")
            .args(["remote", "add", "origin"])
            .arg(&remote_dir)
            .current_dir(&clone_dir)
            .status()
            .unwrap();
        assert!(s.success());

        cws.git_pull_refs("origin").unwrap();
        // The pulled-in workspace can read the task.
        let pulled = cws.task(TaskIdentifier::Id(id)).unwrap();
        assert_eq!(pulled.title, "task one");
        let stack = cws.read_stack().unwrap();
        assert!(stack.iter().any(|i| i.id == id));

        // Errors when invoked on a file-backed workspace.
        let file_dir = dir.path().join("file");
        std::fs::create_dir_all(&file_dir).unwrap();
        Workspace::init(file_dir.clone()).unwrap();
        let fws = Workspace::from_path(file_dir).unwrap();
        assert!(fws.git_push_refs("origin").is_err());
        assert!(fws.git_pull_refs("origin").is_err());
        assert!(fws.configure_git_remote_refspecs("origin").is_err());
    }

    #[test]
    fn test_edit_log_records_mutations() {
        let (_d, file, git) = setup_dual();
        for ws in [&file, &git] {
            let t = ws.new_task("first".into(), "body".into()).unwrap();
            let id = t.id;
            ws.push_task(t).unwrap();

            ws.set_property(id, "priority", "high").unwrap();
            ws.unset_property(id, "priority").unwrap();
            // Edit via the same path command_edit uses.
            let mut reread = ws.task(TaskIdentifier::Id(id)).unwrap();
            reread.title = "edited".into();
            ws.save_task(&reread).unwrap();

            // Trigger handle_metadata so links-changed fires.
            let other = ws.new_task("other".into(), "".into()).unwrap();
            let other_id = other.id;
            ws.push_task(other).unwrap();
            let linker = Task {
                id,
                title: "edited".into(),
                body: format!("see [[{other_id}]]"),
                attributes: Default::default(),
            };
            ws.handle_metadata(&linker, Some(HashSet::new())).unwrap();
            // Same links as before → no log entry added.
            let mut same_links = HashSet::new();
            same_links.insert(other_id);
            ws.handle_metadata(&linker, Some(same_links)).unwrap();

            ws.drop(TaskIdentifier::Id(id)).unwrap();
            ws.reopen(TaskIdentifier::Id(id)).unwrap();

            let log = ws.read_log(id).unwrap();
            let events: Vec<&str> = log.iter().map(|e| e.event.as_str()).collect();
            assert_eq!(
                events,
                vec![
                    "created",
                    "prop-set",
                    "prop-unset",
                    "edited",
                    "links-changed",
                    "archived",
                    "reopened",
                ],
                "got: {events:?}"
            );

            // Logs are included in export.
            let dest = ws.path.join("export.zip");
            ws.export_zip(&dest).unwrap();
            let f = std::fs::File::open(&dest).unwrap();
            let zip = zip::ZipArchive::new(f).unwrap();
            let names: std::collections::HashSet<String> =
                zip.file_names().map(|s| s.to_string()).collect();
            assert!(names.contains(&format!("log/{}", id.0)));
            std::fs::remove_file(&dest).unwrap();
        }
    }

    #[test]
    fn test_properties_set_unset_list_find() {
        let (_d, file, git) = setup_dual();
        for ws in [&file, &git] {
            // Push two tasks; mark one with priority=high.
            let t1 = ws.new_task("first".into(), "body".into()).unwrap();
            let id1 = t1.id;
            ws.push_task(t1).unwrap();
            let t2 = ws
                .new_task("second".into(), "see [[tsk-1]]".into())
                .unwrap();
            let id2 = t2.id;
            ws.handle_metadata(&t2, None).unwrap();
            ws.push_task(t2).unwrap();

            ws.set_property(id1, "priority", "high").unwrap();
            ws.set_property(id1, "tag", "").unwrap();

            // Stored properties round-trip.
            let props = ws.properties(id1).unwrap();
            assert_eq!(props.get("priority").map(String::as_str), Some("high"));
            assert_eq!(props.get("tag").map(String::as_str), Some(""));

            // Synthetic properties present.
            assert_eq!(props.get("state").map(String::as_str), Some("open"));
            assert_eq!(props.get("has-links").map(String::as_str), Some("false"));
            // referenced-by on id1 contains id2 (the linker).
            assert!(
                props
                    .get("referenced-by")
                    .unwrap()
                    .contains(&format!("[[{id2}]]"))
            );

            let props2 = ws.properties(id2).unwrap();
            assert_eq!(props2.get("has-links").map(String::as_str), Some("true"));
            assert!(
                props2
                    .get("references")
                    .unwrap()
                    .contains(&format!("[[{id1}]]"))
            );

            // Find by stored property + value.
            let by_priority = ws.find_by_property("priority", Some("high")).unwrap();
            assert_eq!(by_priority, vec![id1]);
            // Find by presence (any value).
            let any_priority = ws.find_by_property("priority", None).unwrap();
            assert_eq!(any_priority, vec![id1]);
            // Find by synthetic property.
            let open = ws.find_by_property("state", Some("open")).unwrap();
            assert!(open.contains(&id1) && open.contains(&id2));
            ws.drop(TaskIdentifier::Id(id2)).unwrap();
            let archived = ws.find_by_property("state", Some("archived")).unwrap();
            assert_eq!(archived, vec![id2]);

            // Unset removes the property.
            ws.unset_property(id1, "priority").unwrap();
            assert!(ws.properties(id1).unwrap().get("priority").is_none());
            // Unset of non-existent is fine.
            ws.unset_property(id1, "nope").unwrap();
        }
    }

    #[test]
    fn test_parsed_links_returns_all_kinds() {
        // Verifies the data the `tsk links` command consumes: internal,
        // foreign, raw URL, and labeled markdown link should all surface.
        let body = "see <https://a.example> and [[tsk-1]] and [b](https://b.example) and [[gh-99]]";
        let parsed = parse_task(&format!("\n\n{body}")).expect("parse");
        let kinds: Vec<&str> = parsed
            .links
            .iter()
            .map(|l| match l {
                crate::task::ParsedLink::External(_) => "ext",
                crate::task::ParsedLink::Internal(_) => "int",
                crate::task::ParsedLink::Foreign { .. } => "for",
            })
            .collect();
        assert_eq!(kinds, vec!["ext", "int", "ext", "for"]);
    }

    #[test]
    fn test_export_and_accept_across_namespaces() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        run_git_init(&root);
        Workspace::init(root.clone()).unwrap();
        let ws = Workspace::from_path(root.clone()).unwrap();

        // Source task in default namespace.
        let t = ws
            .new_task("send me".into(), "see [[tsk-1]]".into())
            .unwrap();
        let src_id = t.id;
        ws.push_task(t).unwrap();
        ws.set_property(src_id, "priority", "high").unwrap();

        // Export to alice.
        let key = ws.export_to_namespace("alice", src_id).unwrap();
        // Source got the assigned property.
        let src_attrs = ws.properties(src_id).unwrap();
        assert_eq!(
            src_attrs.get("assigned").map(String::as_str),
            Some("[[alice/tsk-1]]")
        );

        // Switch to alice and inspect the inbox.
        ws.switch_namespace("alice").unwrap();
        let alice = Workspace::from_path(root.clone()).unwrap();
        let items = alice.list_inbox().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].source_namespace, "default");
        assert_eq!(items[0].source_id, src_id.0);
        assert_eq!(items[0].title, "send me");

        // Accept it.
        let new_id = alice.accept_inbox(&key).unwrap();
        let accepted = alice.task(TaskIdentifier::Id(new_id)).unwrap();
        assert_eq!(accepted.title, "send me");
        let accepted_props = alice.properties(new_id).unwrap();
        assert_eq!(
            accepted_props.get("source").map(String::as_str),
            Some(&format!("[[default/tsk-{}]]", src_id.0)[..])
        );
        // priority property carried over.
        assert_eq!(
            accepted_props.get("priority").map(String::as_str),
            Some("high")
        );
        // 'assigned' should NOT be inherited on the new copy.
        assert!(!accepted_props.contains_key("assigned"));
        // Inbox cleared.
        assert!(alice.list_inbox().unwrap().is_empty());

        // Cannot export to your own namespace.
        let t2 = alice.new_task("local".into(), "".into()).unwrap();
        let local_id = t2.id;
        alice.push_task(t2).unwrap();
        assert!(alice.export_to_namespace("alice", local_id).is_err());

        // Logs include the cross-namespace events.
        let src_log_events: Vec<String> = ws
            .read_log(src_id)
            .unwrap()
            .iter()
            .map(|e| e.event.clone())
            .collect();
        assert!(src_log_events.contains(&"exported".to_string()));
        let dst_log_events: Vec<String> = alice
            .read_log(new_id)
            .unwrap()
            .iter()
            .map(|e| e.event.clone())
            .collect();
        assert!(dst_log_events.contains(&"accepted".to_string()));
    }

    #[test]
    fn test_namespaces_isolate_state() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        run_git_init(&root);
        Workspace::init(root.clone()).unwrap();
        let ws = Workspace::from_path(root.clone()).unwrap();
        assert_eq!(ws.namespace(), "default");

        // Push a task in the default namespace.
        let t = ws.new_task("default-task".into(), "x".into()).unwrap();
        let default_id = t.id;
        ws.push_task(t).unwrap();

        // Switch to a new namespace; stack should appear empty.
        ws.switch_namespace("alice").unwrap();
        let ws2 = Workspace::from_path(root.clone()).unwrap();
        assert_eq!(ws2.namespace(), "alice");
        assert_eq!(ws2.read_stack().unwrap().iter().count(), 0);
        // ID counter resets per-namespace because `next` is namespaced.
        let alice_t = ws2.new_task("alice-task".into(), "y".into()).unwrap();
        assert_eq!(alice_t.id, Id(1));
        ws2.push_task(alice_t).unwrap();

        // Switch back; the original task is still there.
        ws2.switch_namespace("default").unwrap();
        let ws3 = Workspace::from_path(root.clone()).unwrap();
        let stack = ws3.read_stack().unwrap();
        assert_eq!(stack.iter().count(), 1);
        assert_eq!(stack.iter().next().unwrap().id, default_id);

        // Both namespaces appear in the listing.
        let mut nss = ws3.list_namespaces().unwrap();
        nss.sort();
        assert_eq!(nss, vec!["alice".to_string(), "default".to_string()]);

        // Cannot delete the active namespace.
        assert!(ws3.delete_namespace("default").is_err());

        // Deleting alice succeeds and reduces the namespace list.
        let n = ws3.delete_namespace("alice").unwrap();
        assert!(n > 0);
        assert_eq!(ws3.list_namespaces().unwrap(), vec!["default".to_string()]);

        // Invalid namespace names rejected.
        assert!(ws3.switch_namespace("").is_err());
        assert!(ws3.switch_namespace("a/b").is_err());
        assert!(ws3.switch_namespace("a b").is_err());
    }

    #[test]
    fn test_legacy_non_namespaced_refs_upgraded() {
        // A repo whose refs were created before namespacing should get its
        // refs/tsk/<key>/* moved under refs/tsk/default/ on first open.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        run_git_init(&root);
        // Manually init only the tsk marker (skip Workspace::init's namespace
        // logic) so we can plant legacy refs.
        let tsk_dir = root.join(".tsk");
        std::fs::create_dir(&tsk_dir).unwrap();
        std::fs::write(
            tsk_dir.join(backend::GIT_BACKED_MARKER),
            root.join(".git").to_string_lossy().as_bytes(),
        )
        .unwrap();
        // Plant a legacy ref directly via the bare GitStore.
        let bare = backend::GitStore::open(root.join(".git")).unwrap();
        <dyn Store>::write(&bare, "tasks/1", b"legacy\n\nbody").unwrap();

        // Open via Workspace — should auto-migrate.
        let ws = Workspace::from_path(root.clone()).unwrap();
        assert_eq!(ws.namespace(), "default");
        let t = ws.task(TaskIdentifier::Id(Id(1))).unwrap();
        assert_eq!(t.title, "legacy");
        // Confirm at the git ref level: refs/tsk/tasks/1 is gone, the
        // namespaced refs/tsk/default/tasks/1 is present.
        let repo = git2::Repository::open(root.join(".git")).unwrap();
        assert!(repo.find_reference("refs/tsk/tasks/1").is_err());
        assert!(repo.find_reference("refs/tsk/default/tasks/1").is_ok());
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
