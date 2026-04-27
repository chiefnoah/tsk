//! Per-property indices: `refs/tsk/properties/<key>` → commit chain whose
//! tree contains one blob per task that has the key set. Each blob's lines
//! are that task's values for the key.
//!
//! Each key has its own commit history so concurrent edits to *different*
//! keys can never conflict. Concurrent edits to the same key on different
//! tasks land in different tree entries and can be merged by git's default
//! tree merge; only same-key/same-task races require manual resolution.

use crate::errors::Result;
use crate::object::StableId;
use crate::propvalue;
use git2::{Oid, Repository, Signature};
use std::collections::BTreeMap;

pub const PROP_REF_PREFIX: &str = "refs/tsk/properties/";

pub fn refname(key: &str) -> String {
    format!("{PROP_REF_PREFIX}{key}")
}

fn signature(repo: &Repository) -> Signature<'static> {
    repo.signature()
        .map(|s| s.to_owned())
        .unwrap_or_else(|_| Signature::now("tsk", "tsk@local").unwrap())
}

/// Read every (stable_id, values) entry currently indexed under `key`.
pub fn read(repo: &Repository, key: &str) -> Result<BTreeMap<StableId, Vec<String>>> {
    let mut out = BTreeMap::new();
    let Ok(r) = repo.find_reference(&refname(key)) else {
        return Ok(out);
    };
    let Some(target) = r.target() else {
        return Ok(out);
    };
    let tree = repo.find_commit(target)?.tree()?;
    for entry in tree.iter() {
        let Some(name) = entry.name() else { continue };
        let blob = entry.to_object(repo)?.peel_to_blob()?;
        let values = propvalue::decode(blob.content());
        out.insert(StableId(name.to_string()), values);
    }
    Ok(out)
}

fn write_index(
    repo: &Repository,
    key: &str,
    entries: &BTreeMap<StableId, Vec<String>>,
    message: &str,
) -> Result<()> {
    if entries.is_empty() {
        // Drop the ref entirely — empty indexes are noise.
        if let Ok(mut r) = repo.find_reference(&refname(key)) {
            r.delete()?;
        }
        return Ok(());
    }
    let mut tb = repo.treebuilder(None)?;
    for (stable, values) in entries {
        let body = propvalue::encode(values);
        let oid = repo.blob(&body)?;
        tb.insert(stable.0.as_str(), oid, 0o100644)?;
    }
    let tree_oid: Oid = tb.write()?;
    let parent = repo
        .find_reference(&refname(key))
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
    repo.reference(&refname(key), commit, true, message)?;
    Ok(())
}

/// Set values for `(key, stable)` in the index; empty `values` removes the
/// task from this key's index.
pub fn set(
    repo: &Repository,
    key: &str,
    stable: &StableId,
    values: &[String],
    message: &str,
) -> Result<()> {
    let mut entries = read(repo, key)?;
    if values.is_empty() {
        entries.remove(stable);
    } else {
        entries.insert(stable.clone(), values.to_vec());
    }
    write_index(repo, key, &entries, message)
}

/// Every property key currently indexed in this repo.
pub fn list_keys(repo: &Repository) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for r in repo.references_glob(&format!("{PROP_REF_PREFIX}*"))? {
        let r = r?;
        if let Some(name) = r.name()
            && let Some(rest) = name.strip_prefix(PROP_REF_PREFIX)
        {
            out.push(rest.to_string());
        }
    }
    out.sort();
    Ok(out)
}

/// Stable ids of every task that has `key` set; if `value` is supplied,
/// restricts to tasks where `key` contains that value.
pub fn find(repo: &Repository, key: &str, value: Option<&str>) -> Result<Vec<StableId>> {
    let entries = read(repo, key)?;
    Ok(entries
        .into_iter()
        .filter(|(_, vs)| value.map_or(true, |target| vs.iter().any(|v| v == target)))
        .map(|(s, _)| s)
        .collect())
}

/// Distinct values seen for `key`, sorted alphabetically.
pub fn values_for(repo: &Repository, key: &str) -> Result<Vec<String>> {
    let entries = read(repo, key)?;
    let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for vs in entries.values() {
        for v in vs {
            set.insert(v.clone());
        }
    }
    Ok(set.into_iter().collect())
}

/// Re-index a task across all currently-indexed keys + its own properties.
/// Removes the task from indices it no longer belongs to. Call this after
/// any task tree write that may have added/removed properties.
pub fn reindex_task(
    repo: &Repository,
    stable: &StableId,
    properties: &BTreeMap<String, Vec<String>>,
) -> Result<()> {
    use std::collections::BTreeSet;
    let known: BTreeSet<String> = list_keys(repo)?.into_iter().collect();
    let current: BTreeSet<String> = properties.keys().cloned().collect();
    // Update keys the task currently has.
    for key in &current {
        let values = properties.get(key).cloned().unwrap_or_default();
        set(repo, key, stable, &values, "reindex")?;
    }
    // Drop the task from indices it no longer participates in.
    for key in known.difference(&current) {
        set(repo, key, stable, &[], "reindex-remove")?;
    }
    Ok(())
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
    fn set_find_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let s1 = object::create(&repo, &object::Task::new("a"), "c").unwrap();
        let s2 = object::create(&repo, &object::Task::new("b"), "c").unwrap();
        set(&repo, "priority", &s1, &["high".into()], "x").unwrap();
        set(&repo, "priority", &s2, &["low".into(), "medium".into()], "x").unwrap();
        let high = find(&repo, "priority", Some("high")).unwrap();
        assert_eq!(high, vec![s1.clone()]);
        let any_priority = find(&repo, "priority", None).unwrap();
        assert_eq!(any_priority.len(), 2);
        assert_eq!(
            values_for(&repo, "priority").unwrap(),
            vec!["high".to_string(), "low".into(), "medium".into()]
        );
    }

    #[test]
    fn empty_values_removes_from_index() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let s = object::create(&repo, &object::Task::new("a"), "c").unwrap();
        set(&repo, "tag", &s, &["x".into()], "x").unwrap();
        assert_eq!(find(&repo, "tag", None).unwrap(), vec![s.clone()]);
        set(&repo, "tag", &s, &[], "rm").unwrap();
        assert_eq!(find(&repo, "tag", None).unwrap(), Vec::<StableId>::new());
        // Index ref is dropped when the last entry is removed.
        assert!(repo.find_reference(&refname("tag")).is_err());
    }

    #[test]
    fn list_keys_reports_indexed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let s = object::create(&repo, &object::Task::new("a"), "c").unwrap();
        set(&repo, "k1", &s, &["v".into()], "x").unwrap();
        set(&repo, "k2", &s, &["v".into()], "x").unwrap();
        let mut keys = list_keys(&repo).unwrap();
        keys.sort();
        assert_eq!(keys, vec!["k1".to_string(), "k2".into()]);
    }

    #[test]
    fn reindex_drops_removed_keys() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let s = object::create(&repo, &object::Task::new("a"), "c").unwrap();
        set(&repo, "kept", &s, &["v".into()], "x").unwrap();
        set(&repo, "gone", &s, &["v".into()], "x").unwrap();

        let mut props: BTreeMap<String, Vec<String>> = BTreeMap::new();
        props.insert("kept".into(), vec!["v".into()]);
        // "gone" is not in props anymore; reindex should remove the task from it.
        reindex_task(&repo, &s, &props).unwrap();
        assert!(find(&repo, "gone", None).unwrap().is_empty());
        assert_eq!(find(&repo, "kept", None).unwrap(), vec![s]);
    }
}
