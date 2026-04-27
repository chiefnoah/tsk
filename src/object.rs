//! A task: a tree of `{content, <prop>...}` blobs with its own commit history.
//!
//! Stable id = SHA-1 hex of the initial `content` blob. Refs:
//! `refs/tsk/tasks/<stable-id>` → latest commit on that task.
//!
//! Tree layout for a task at any commit:
//!   content     → blob: full task body, first line is the title
//!   <prop-key>  → blob: property value (one file per property)
//!
//! Older trees may also contain a `title` blob (a cache of content's first
//! line). It's ignored on read and silently dropped on the next write.

use crate::errors::Result;
use crate::propvalue;
use git2::{Oid, Repository, Signature};
use std::collections::BTreeMap;
use std::fmt::Display;

pub const TASK_REF_PREFIX: &str = "refs/tsk/tasks/";
pub const CONTENT_FILE: &str = "content";
pub const TITLE_FILE: &str = "title";

/// Stable identifier for a task: hex SHA-1 of its initial content blob.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd)]
pub struct StableId(pub String);

impl StableId {
    pub fn refname(&self) -> String {
        format!("{TASK_REF_PREFIX}{}", self.0)
    }
    #[allow(dead_code)] // used by upcoming display layer
    pub fn short(&self) -> &str {
        &self.0[..12.min(self.0.len())]
    }
}

impl Display for StableId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Task {
    pub content: String,
    /// Each property is zero or more text values (one per line in storage).
    pub properties: BTreeMap<String, Vec<String>>,
}

impl Task {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            properties: BTreeMap::new(),
        }
    }

    pub fn title(&self) -> &str {
        self.content.lines().next().unwrap_or("")
    }

    pub fn body(&self) -> &str {
        match self.content.split_once('\n') {
            Some((_, rest)) => rest.trim_start_matches('\n'),
            None => "",
        }
    }
}

/// Local user's git signature, with a `tsk@local` fallback. Shared by
/// every writer so commits all carry the same author/committer.
pub(crate) fn signature(repo: &Repository) -> Signature<'static> {
    repo.signature()
        .map(|s| s.to_owned())
        .unwrap_or_else(|_| Signature::now("tsk", "tsk@local").unwrap())
}

fn build_tree(
    repo: &Repository,
    content_oid: Oid,
    properties: &BTreeMap<String, Vec<String>>,
) -> Result<Oid> {
    let mut tb = repo.treebuilder(None)?;
    tb.insert(CONTENT_FILE, content_oid, 0o100644)?;
    for (k, values) in properties {
        if k == CONTENT_FILE || k == TITLE_FILE {
            continue;
        }
        let body = propvalue::encode(values);
        let oid = repo.blob(&body)?;
        tb.insert(k.as_str(), oid, 0o100644)?;
    }
    Ok(tb.write()?)
}

/// Create a brand-new task. Returns its freshly-minted stable id.
pub fn create(repo: &Repository, task: &Task, message: &str) -> Result<StableId> {
    let content_oid = repo.blob(task.content.as_bytes())?;
    let stable = StableId(content_oid.to_string());
    let tree_oid = build_tree(repo, content_oid, &task.properties)?;
    let sig = signature(repo);
    let commit = repo.commit(
        None,
        &sig,
        &sig,
        message,
        &repo.find_tree(tree_oid)?,
        &[],
    )?;
    repo.reference(&stable.refname(), commit, true, message)?;
    Ok(stable)
}

/// Append a new commit to a task's history. Returns `true` if a commit
/// was actually written; `false` when the resulting tree matches the
/// parent's (idempotent no-op).
pub fn update(repo: &Repository, id: &StableId, task: &Task, message: &str) -> Result<bool> {
    let content_oid = repo.blob(task.content.as_bytes())?;
    let tree_oid = build_tree(repo, content_oid, &task.properties)?;
    let parent = repo
        .find_reference(&id.refname())
        .ok()
        .and_then(|r| r.target())
        .and_then(|o| repo.find_commit(o).ok());
    if let Some(p) = &parent
        && p.tree_id() == tree_oid
    {
        return Ok(false);
    }
    let sig = signature(repo);
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    let commit = repo.commit(
        None,
        &sig,
        &sig,
        message,
        &repo.find_tree(tree_oid)?,
        &parents,
    )?;
    repo.reference(&id.refname(), commit, true, message)?;
    Ok(true)
}

pub fn read(repo: &Repository, id: &StableId) -> Result<Option<Task>> {
    let Ok(r) = repo.find_reference(&id.refname()) else {
        return Ok(None);
    };
    let Some(target) = r.target() else {
        return Ok(None);
    };
    let commit = repo.find_commit(target)?;
    let tree = commit.tree()?;
    let mut task = Task::default();
    for entry in tree.iter() {
        let name = entry.name().unwrap_or("").to_string();
        let blob = entry.to_object(repo)?.peel_to_blob()?;
        let val = String::from_utf8_lossy(blob.content()).into_owned();
        match name.as_str() {
            CONTENT_FILE => task.content = val,
            TITLE_FILE => {} // cache only; canonical title is content's first line
            _ => {
                let values = propvalue::decode(blob.content());
                task.properties.insert(name, values);
            }
        }
    }
    Ok(Some(task))
}

#[allow(dead_code)] // exposed for cleanup tooling / future commands
pub fn delete(repo: &Repository, id: &StableId) -> Result<()> {
    if let Ok(mut r) = repo.find_reference(&id.refname()) {
        r.delete()?;
    }
    Ok(())
}

#[allow(dead_code)] // exposed for cleanup tooling / future commands
pub fn list_all(repo: &Repository) -> Result<Vec<StableId>> {
    let mut out = Vec::new();
    for r in repo.references_glob(&format!("{TASK_REF_PREFIX}*"))? {
        let r = r?;
        if let Some(name) = r.name()
            && let Some(rest) = name.strip_prefix(TASK_REF_PREFIX)
        {
            out.push(StableId(rest.to_string()));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod test {
    use super::*;
    use std::path::Path;

    fn init_repo(p: &Path) -> Repository {
        let r = Repository::init(p).unwrap();
        let mut cfg = r.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "t@e").unwrap();
        r
    }

    #[test]
    fn create_read_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let mut t = Task::new("Hello\n\nbody text");
        t.properties
            .insert("priority".into(), vec!["high".into()]);
        t.properties
            .insert("tag".into(), vec!["alpha".into(), "beta".into()]);
        let id = create(&repo, &t, "create").unwrap();
        let read_back = read(&repo, &id).unwrap().unwrap();
        assert_eq!(read_back.content, t.content);
        assert_eq!(read_back.properties, t.properties);
        assert_eq!(read_back.title(), "Hello");
        assert_eq!(read_back.body(), "body text");
    }

    #[test]
    fn update_appends_commit() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let t = Task::new("v1");
        let id = create(&repo, &t, "create").unwrap();
        let mut t2 = t.clone();
        t2.content = "v2".into();
        update(&repo, &id, &t2, "edit").unwrap();
        // Two commits in the chain.
        let head = repo.find_reference(&id.refname()).unwrap().target().unwrap();
        let head_commit = repo.find_commit(head).unwrap();
        assert_eq!(head_commit.parent_count(), 1);
        let read_back = read(&repo, &id).unwrap().unwrap();
        assert_eq!(read_back.content, "v2");
    }

    #[test]
    fn update_idempotent_when_tree_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let t = Task::new("same");
        let id = create(&repo, &t, "create").unwrap();
        let head1 = repo.find_reference(&id.refname()).unwrap().target().unwrap();
        update(&repo, &id, &t, "noop").unwrap();
        let head2 = repo.find_reference(&id.refname()).unwrap().target().unwrap();
        assert_eq!(head1, head2);
    }

    #[test]
    fn list_all_sees_every_task() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let a = create(&repo, &Task::new("a"), "c").unwrap();
        let b = create(&repo, &Task::new("b"), "c").unwrap();
        let mut got = list_all(&repo).unwrap();
        got.sort();
        let mut want = vec![a, b];
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn stable_id_equals_blob_oid() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let id = create(&repo, &Task::new("xyz"), "c").unwrap();
        let direct = repo.blob(b"xyz").unwrap();
        assert_eq!(id.0, direct.to_string());
    }
}
