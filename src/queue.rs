//! A queue: an ordered list of tasks plus an inbox of pending assignments.
//!
//! Stored as a commit chain at `refs/tsk/queues/<name>`. Tree layout:
//!   index           → blob: ordered stable ids (one per line, top-of-stack first)
//!   can-pull        → blob: "true" or "false" (defaults: true for `tsk`, false otherwise)
//!   inbox/<src>-<n> → blob: stable id of a task assigned by queue <src>
//!
//! Queues are pushed/shared (refspec `refs/tsk/*`). The active queue per-user
//! is selected by `<git-dir>/tsk/queue`.

use crate::errors::{Error, Result};
use crate::object::{self, StableId};
use git2::{Oid, Repository};
use std::collections::BTreeMap;

pub const QUEUE_REF_PREFIX: &str = "refs/tsk/queues/";
pub const DEFAULT_QUEUE: &str = "tsk";
const INDEX_FILE: &str = "index";
const CAN_PULL_FILE: &str = "can-pull";
const INBOX_DIR: &str = "inbox";

pub fn refname(name: &str) -> String {
    format!("{QUEUE_REF_PREFIX}{name}")
}

pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::Parse("Queue name cannot be empty".into()));
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return Err(Error::Parse(format!(
            "Queue '{name}' must contain only alphanumerics, '-', or '_'"
        )));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Queue {
    /// top-of-stack first
    pub index: Vec<StableId>,
    pub can_pull: bool,
    /// inbox key → stable id of the task being offered
    pub inbox: BTreeMap<String, StableId>,
}

impl Queue {
    pub fn new(name: &str) -> Self {
        Self {
            index: Vec::new(),
            can_pull: name == DEFAULT_QUEUE,
            inbox: BTreeMap::new(),
        }
    }
}

pub fn read(repo: &Repository, name: &str) -> Result<Queue> {
    let Ok(r) = repo.find_reference(&refname(name)) else {
        return Ok(Queue::new(name));
    };
    let Some(target) = r.target() else {
        return Ok(Queue::new(name));
    };
    let tree = repo.find_commit(target)?.tree()?;
    let mut q = Queue::new(name);
    if let Some(e) = tree.get_name(INDEX_FILE) {
        let blob = e.to_object(repo)?.peel_to_blob()?;
        q.index = String::from_utf8_lossy(blob.content())
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| StableId(l.trim().to_string()))
            .collect();
    }
    if let Some(e) = tree.get_name(CAN_PULL_FILE) {
        let blob = e.to_object(repo)?.peel_to_blob()?;
        q.can_pull = String::from_utf8_lossy(blob.content()).trim() == "true";
    }
    if let Some(e) = tree.get_name(INBOX_DIR) {
        let inbox_tree = e.to_object(repo)?.peel_to_tree()?;
        for ie in inbox_tree.iter() {
            let Some(name) = ie.name() else { continue };
            let blob = ie.to_object(repo)?.peel_to_blob()?;
            let stable = String::from_utf8_lossy(blob.content()).trim().to_string();
            if !stable.is_empty() {
                q.inbox
                    .insert(name.to_string(), StableId(stable));
            }
        }
    }
    Ok(q)
}

fn build_tree(repo: &Repository, q: &Queue) -> Result<Oid> {
    let mut tb = repo.treebuilder(None)?;
    let index_text: String = q
        .index
        .iter()
        .map(|s| format!("{}\n", s.0))
        .collect();
    let index_oid = repo.blob(index_text.as_bytes())?;
    tb.insert(INDEX_FILE, index_oid, 0o100644)?;
    let cp = if q.can_pull { "true\n" } else { "false\n" };
    let cp_oid = repo.blob(cp.as_bytes())?;
    tb.insert(CAN_PULL_FILE, cp_oid, 0o100644)?;
    if !q.inbox.is_empty() {
        let mut ib = repo.treebuilder(None)?;
        for (k, v) in &q.inbox {
            let oid = repo.blob(v.0.as_bytes())?;
            ib.insert(k.as_str(), oid, 0o100644)?;
        }
        let ib_oid = ib.write()?;
        tb.insert(INBOX_DIR, ib_oid, 0o040000)?;
    }
    Ok(tb.write()?)
}

pub fn write(repo: &Repository, name: &str, q: &Queue, message: &str) -> Result<()> {
    validate_name(name)?;
    let tree_oid = build_tree(repo, q)?;
    let parent = repo
        .find_reference(&refname(name))
        .ok()
        .and_then(|r| r.target())
        .and_then(|o| repo.find_commit(o).ok());
    if let Some(p) = &parent
        && p.tree_id() == tree_oid
    {
        return Ok(());
    }
    let sig = object::signature(repo);
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    let commit = repo.commit(
        None,
        &sig,
        &sig,
        message,
        &repo.find_tree(tree_oid)?,
        &parents,
    )?;
    repo.reference(&refname(name), commit, true, message)?;
    Ok(())
}

pub fn list_names(repo: &Repository) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for r in repo.references_glob(&format!("{QUEUE_REF_PREFIX}*"))? {
        let r = r?;
        if let Some(name) = r.name()
            && let Some(rest) = name.strip_prefix(QUEUE_REF_PREFIX)
        {
            out.push(rest.to_string());
        }
    }
    out.sort();
    Ok(out)
}

/// Push `stable` onto the top of `name`'s index. Idempotent: if already
/// present, moves it to the top.
pub fn push_top(repo: &Repository, name: &str, stable: StableId, message: &str) -> Result<()> {
    let mut q = read(repo, name)?;
    q.index.retain(|s| s != &stable);
    q.index.insert(0, stable);
    write(repo, name, &q, message)
}

pub fn push_bottom(
    repo: &Repository,
    name: &str,
    stable: StableId,
    message: &str,
) -> Result<()> {
    let mut q = read(repo, name)?;
    q.index.retain(|s| s != &stable);
    q.index.push(stable);
    write(repo, name, &q, message)
}

pub fn remove(repo: &Repository, name: &str, stable: &StableId, message: &str) -> Result<bool> {
    let mut q = read(repo, name)?;
    let len = q.index.len();
    q.index.retain(|s| s != stable);
    if q.index.len() == len {
        return Ok(false);
    }
    write(repo, name, &q, message)?;
    Ok(true)
}

/// Stable inbox key for a `(source-queue, sequence)` pair so re-assigns
/// overwrite the same slot rather than piling up. Sequence is the per-source
/// counter the caller maintains; we don't try to compute it here.
pub fn inbox_key(src_queue: &str, src_seq: u32) -> String {
    format!("{src_queue}-{src_seq}")
}

pub fn add_to_inbox(
    repo: &Repository,
    name: &str,
    key: String,
    stable: StableId,
    message: &str,
) -> Result<()> {
    let mut q = read(repo, name)?;
    q.inbox.insert(key, stable);
    write(repo, name, &q, message)
}

pub fn take_from_inbox(
    repo: &Repository,
    name: &str,
    key: &str,
    message: &str,
) -> Result<Option<StableId>> {
    let mut q = read(repo, name)?;
    let Some(stable) = q.inbox.remove(key) else {
        return Ok(None);
    };
    write(repo, name, &q, message)?;
    Ok(Some(stable))
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::object;

    fn init_repo(p: &std::path::Path) -> Repository {
        let r = Repository::init(p).unwrap();
        let mut cfg = r.config().unwrap();
        cfg.set_str("user.name", "T").unwrap();
        cfg.set_str("user.email", "t@e").unwrap();
        r
    }

    #[test]
    fn empty_default_queue_is_can_pull() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let q = read(&repo, "tsk").unwrap();
        assert!(q.can_pull);
        assert!(q.index.is_empty());
    }

    #[test]
    fn empty_named_queue_defaults_no_pull() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let q = read(&repo, "private").unwrap();
        assert!(!q.can_pull);
    }

    #[test]
    fn push_pop_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let s1 = object::create(&repo, &object::Task::new("a"), "c").unwrap();
        let s2 = object::create(&repo, &object::Task::new("b"), "c").unwrap();
        push_top(&repo, "tsk", s1.clone(), "push").unwrap();
        push_top(&repo, "tsk", s2.clone(), "push").unwrap();
        let q = read(&repo, "tsk").unwrap();
        assert_eq!(q.index, vec![s2.clone(), s1.clone()]);
        assert!(remove(&repo, "tsk", &s2, "drop").unwrap());
        let q = read(&repo, "tsk").unwrap();
        assert_eq!(q.index, vec![s1]);
    }

    #[test]
    fn push_top_dedupes_existing() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let s = object::create(&repo, &object::Task::new("a"), "c").unwrap();
        push_top(&repo, "tsk", s.clone(), "push").unwrap();
        push_bottom(&repo, "tsk", s.clone(), "push").unwrap(); // same id, moved to bottom
        let q = read(&repo, "tsk").unwrap();
        assert_eq!(q.index.len(), 1);
        assert_eq!(q.index[0], s);
    }

    #[test]
    fn inbox_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let s = object::create(&repo, &object::Task::new("a"), "c").unwrap();
        let key = inbox_key("alice", 5);
        add_to_inbox(&repo, "bob", key.clone(), s.clone(), "assign").unwrap();
        let q = read(&repo, "bob").unwrap();
        assert_eq!(q.inbox.get(&key), Some(&s));
        let taken = take_from_inbox(&repo, "bob", &key, "accept").unwrap();
        assert_eq!(taken, Some(s));
        let q = read(&repo, "bob").unwrap();
        assert!(q.inbox.is_empty());
    }
}
