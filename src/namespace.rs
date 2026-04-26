//! A namespace: a tree mapping human-readable ids → task stable ids.
//!
//! Stored as a commit chain at `refs/tsk/namespaces/<name>`. Tree layout:
//!   next            → blob: next human id to allocate, e.g. "5\n"
//!   ids/<human-id>  → blob: 40-hex stable id
//!
//! The default namespace is `tsk`. Namespaces are self-contained (no
//! cross-namespace references at this layer).

use crate::errors::{Error, Result};
use crate::object::StableId;
use git2::{Oid, Repository, Signature};
use std::collections::BTreeMap;

pub const NS_REF_PREFIX: &str = "refs/tsk/namespaces/";
pub const DEFAULT_NS: &str = "tsk";
const NEXT_FILE: &str = "next";
const IDS_DIR: &str = "ids";

pub fn refname(name: &str) -> String {
    format!("{NS_REF_PREFIX}{name}")
}

pub fn validate_name(name: &str) -> Result<()> {
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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Namespace {
    pub next: u32,
    /// human id → stable id
    pub mapping: BTreeMap<u32, StableId>,
}

fn signature(repo: &Repository) -> Signature<'static> {
    repo.signature()
        .map(|s| s.to_owned())
        .unwrap_or_else(|_| Signature::now("tsk", "tsk@local").unwrap())
}

pub fn read(repo: &Repository, name: &str) -> Result<Namespace> {
    let Ok(r) = repo.find_reference(&refname(name)) else {
        return Ok(Namespace {
            next: 1,
            mapping: BTreeMap::new(),
        });
    };
    let Some(target) = r.target() else {
        return Ok(Namespace {
            next: 1,
            mapping: BTreeMap::new(),
        });
    };
    let tree = repo.find_commit(target)?.tree()?;
    let mut ns = Namespace {
        next: 1,
        mapping: BTreeMap::new(),
    };
    if let Some(entry) = tree.get_name(NEXT_FILE) {
        let blob = entry.to_object(repo)?.peel_to_blob()?;
        ns.next = String::from_utf8_lossy(blob.content())
            .trim()
            .parse()
            .unwrap_or(1);
    }
    if let Some(ids_entry) = tree.get_name(IDS_DIR) {
        let ids_tree = ids_entry.to_object(repo)?.peel_to_tree()?;
        for e in ids_tree.iter() {
            let Some(name) = e.name() else { continue };
            let Ok(human) = name.parse::<u32>() else {
                continue;
            };
            let blob = e.to_object(repo)?.peel_to_blob()?;
            let stable = String::from_utf8_lossy(blob.content()).trim().to_string();
            if !stable.is_empty() {
                ns.mapping.insert(human, StableId(stable));
            }
        }
    }
    Ok(ns)
}

fn build_tree(repo: &Repository, ns: &Namespace) -> Result<Oid> {
    let mut ids_tb = repo.treebuilder(None)?;
    for (human, stable) in &ns.mapping {
        let oid = repo.blob(stable.0.as_bytes())?;
        ids_tb.insert(human.to_string().as_str(), oid, 0o100644)?;
    }
    let ids_oid = ids_tb.write()?;

    let mut tb = repo.treebuilder(None)?;
    let next_oid = repo.blob(format!("{}\n", ns.next).as_bytes())?;
    tb.insert(NEXT_FILE, next_oid, 0o100644)?;
    tb.insert(IDS_DIR, ids_oid, 0o040000)?;
    Ok(tb.write()?)
}

pub fn write(repo: &Repository, name: &str, ns: &Namespace, message: &str) -> Result<()> {
    validate_name(name)?;
    let tree_oid = build_tree(repo, ns)?;
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
    repo.reference(&refname(name), commit, true, message)?;
    Ok(())
}

/// Allocate the next human id, insert the binding, and persist. Returns the
/// human id assigned.
pub fn assign_id(
    repo: &Repository,
    name: &str,
    stable: StableId,
    message: &str,
) -> Result<u32> {
    let mut ns = read(repo, name)?;
    let human = ns.next;
    ns.next += 1;
    ns.mapping.insert(human, stable);
    write(repo, name, &ns, message)?;
    Ok(human)
}

pub fn unassign_id(repo: &Repository, name: &str, human: u32, message: &str) -> Result<()> {
    let mut ns = read(repo, name)?;
    if ns.mapping.remove(&human).is_some() {
        write(repo, name, &ns, message)?;
    }
    Ok(())
}

pub fn list_names(repo: &Repository) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for r in repo.references_glob(&format!("{NS_REF_PREFIX}*"))? {
        let r = r?;
        if let Some(name) = r.name()
            && let Some(rest) = name.strip_prefix(NS_REF_PREFIX)
        {
            out.push(rest.to_string());
        }
    }
    out.sort();
    Ok(out)
}

pub fn lookup(repo: &Repository, name: &str, human: u32) -> Result<Option<StableId>> {
    Ok(read(repo, name)?.mapping.get(&human).cloned())
}

/// Reverse lookup: stable → human in the given namespace, if present.
pub fn human_for(repo: &Repository, name: &str, stable: &StableId) -> Result<Option<u32>> {
    Ok(read(repo, name)?
        .mapping
        .iter()
        .find(|(_, s)| *s == stable)
        .map(|(h, _)| *h))
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
    fn empty_namespace_reads_as_default() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let ns = read(&repo, "tsk").unwrap();
        assert_eq!(ns.next, 1);
        assert!(ns.mapping.is_empty());
    }

    #[test]
    fn assign_then_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let s1 = object::create(&repo, &object::Task::new("a"), "c").unwrap();
        let s2 = object::create(&repo, &object::Task::new("b"), "c").unwrap();
        let h1 = assign_id(&repo, "tsk", s1.clone(), "assign").unwrap();
        let h2 = assign_id(&repo, "tsk", s2.clone(), "assign").unwrap();
        assert_eq!(h1, 1);
        assert_eq!(h2, 2);
        let ns = read(&repo, "tsk").unwrap();
        assert_eq!(ns.next, 3);
        assert_eq!(ns.mapping.get(&1), Some(&s1));
        assert_eq!(ns.mapping.get(&2), Some(&s2));
        assert_eq!(human_for(&repo, "tsk", &s1).unwrap(), Some(1));
    }

    #[test]
    fn unassign_removes_only_mapping_keeps_next() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let s = object::create(&repo, &object::Task::new("a"), "c").unwrap();
        let _ = assign_id(&repo, "tsk", s, "assign").unwrap();
        unassign_id(&repo, "tsk", 1, "drop").unwrap();
        let ns = read(&repo, "tsk").unwrap();
        assert!(ns.mapping.is_empty());
        assert_eq!(ns.next, 2, "next must monotonically grow");
    }

    #[test]
    fn list_names_returns_known_namespaces() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let s = object::create(&repo, &object::Task::new("a"), "c").unwrap();
        let _ = assign_id(&repo, "tsk", s.clone(), "assign").unwrap();
        let _ = assign_id(&repo, "alpha", s, "assign").unwrap();
        let mut names = list_names(&repo).unwrap();
        names.sort();
        assert_eq!(names, vec!["alpha".to_string(), "tsk".to_string()]);
    }

    #[test]
    fn validate_name_rejects_bad_input() {
        assert!(validate_name("").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("ok-name_1").is_ok());
    }
}
