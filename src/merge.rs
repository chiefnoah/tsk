//! Reconcile divergent task histories on `tsk git-pull`.
//!
//! `git_pull` fetches the remote's `refs/tsk/*` into a non-clobbering shadow
//! namespace at `refs/tsk-fetched/<remote>/*` so both the local and remote
//! tip of every task ref are available in the same repo. We then walk every
//! task that exists in either, and for each:
//!
//! - one side missing → take the side that has it
//! - one side strictly ancestor of the other → fast-forward (or no-op)
//! - both diverged → reconcile per [`Strategy`]
//!
//! `Strategy::Merge` (default) creates a merge commit using `git2`'s
//! 3-way `merge_trees` against the common ancestor; clean merges land as a
//! single commit with two parents and the local user as both author and
//! committer. `Strategy::Rebase` replays each local-only commit on top of
//! the remote tip, preserving each commit's original author and updating
//! the committer to the local user — same shape as `git rebase`.
//!
//! True content conflicts (both sides edited the same blob in incompatible
//! ways) abort that one task's reconciliation and leave the local ref
//! untouched. The conflict surfaces in the pull summary so the user can
//! re-run with the other strategy or hand-resolve.

use crate::errors::Result;
use crate::object::{StableId, TASK_REF_PREFIX};
use git2::{Commit, Oid, Repository, Signature};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Strategy {
    #[default]
    Merge,
    Rebase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconKind {
    /// No work needed (either side strict ancestor or refs identical).
    Unchanged,
    /// Local was strict ancestor of remote; ref now points at remote tip.
    FastForward,
    /// Remote ref existed without a local counterpart; copied verbatim.
    NewRemote,
    /// Wrote a merge commit with two parents.
    Merged,
    /// Replayed local-only commits onto the remote tip.
    Rebased,
    /// Reconciliation aborted due to overlapping edits; local ref unchanged.
    Conflict,
}

#[derive(Debug)]
pub struct Reconciliation {
    pub stable: StableId,
    pub kind: ReconKind,
}

pub const FETCH_PREFIX: &str = "refs/tsk-fetched/";

pub fn fetched_prefix(remote: &str) -> String {
    format!("{FETCH_PREFIX}{remote}/")
}

/// Reconcile every `refs/tsk/tasks/*` against its fetched counterpart at
/// `refs/tsk-fetched/<remote>/tasks/*`. Returns one entry per task that
/// existed in either side.
pub fn reconcile_task_refs(
    repo: &Repository,
    remote: &str,
    strategy: Strategy,
) -> Result<Vec<Reconciliation>> {
    let fetched_tasks = format!("{}tasks/", fetched_prefix(remote));
    let mut stables: BTreeSet<String> = BTreeSet::new();
    for r in repo.references_glob(&format!("{TASK_REF_PREFIX}*"))? {
        let r = r?;
        if let Some(name) = r.name().and_then(|n| n.strip_prefix(TASK_REF_PREFIX)) {
            stables.insert(name.to_string());
        }
    }
    for r in repo.references_glob(&format!("{fetched_tasks}*"))? {
        let r = r?;
        if let Some(name) = r.name().and_then(|n| n.strip_prefix(fetched_tasks.as_str())) {
            stables.insert(name.to_string());
        }
    }
    let mut out = Vec::new();
    for s in stables {
        let stable = StableId(s.clone());
        let local = repo
            .find_reference(&stable.refname())
            .ok()
            .and_then(|r| r.target());
        let remote_ref = format!("{fetched_tasks}{s}");
        let remote_tip = repo
            .find_reference(&remote_ref)
            .ok()
            .and_then(|r| r.target());
        let kind = reconcile_one(repo, &stable, local, remote_tip, strategy)?;
        out.push(Reconciliation { stable, kind });
    }
    Ok(out)
}

fn reconcile_one(
    repo: &Repository,
    stable: &StableId,
    local: Option<Oid>,
    remote: Option<Oid>,
    strategy: Strategy,
) -> Result<ReconKind> {
    match (local, remote) {
        (None, None) | (Some(_), None) => Ok(ReconKind::Unchanged),
        (None, Some(r)) => {
            repo.reference(&stable.refname(), r, true, "pull-import")?;
            Ok(ReconKind::NewRemote)
        }
        (Some(l), Some(r)) if l == r => Ok(ReconKind::Unchanged),
        (Some(l), Some(r)) => {
            // graph_descendant_of(a, b) is true iff a descends from b.
            if repo.graph_descendant_of(l, r).unwrap_or(false) {
                Ok(ReconKind::Unchanged)
            } else if repo.graph_descendant_of(r, l).unwrap_or(false) {
                repo.reference(&stable.refname(), r, true, "fast-forward")?;
                Ok(ReconKind::FastForward)
            } else {
                match strategy {
                    Strategy::Merge => merge_strategy(repo, stable, l, r),
                    Strategy::Rebase => rebase_strategy(repo, stable, l, r),
                }
            }
        }
    }
}

fn merge_strategy(
    repo: &Repository,
    stable: &StableId,
    local: Oid,
    remote: Oid,
) -> Result<ReconKind> {
    let base_oid = repo.merge_base(local, remote)?;
    let base_tree = repo.find_commit(base_oid)?.tree()?;
    let our_tree = repo.find_commit(local)?.tree()?;
    let their_tree = repo.find_commit(remote)?.tree()?;
    let mut idx = repo.merge_trees(&base_tree, &our_tree, &their_tree, None)?;
    if idx.has_conflicts() {
        return Ok(ReconKind::Conflict);
    }
    let tree_oid = idx.write_tree_to(repo)?;
    let sig = signature(repo);
    let local_commit = repo.find_commit(local)?;
    let remote_commit = repo.find_commit(remote)?;
    let parents: Vec<&Commit> = vec![&local_commit, &remote_commit];
    let short = &stable.0[..12.min(stable.0.len())];
    let merge_oid = repo.commit(
        None,
        &sig,
        &sig,
        &format!("merge tsk-{short}"),
        &repo.find_tree(tree_oid)?,
        &parents,
    )?;
    repo.reference(&stable.refname(), merge_oid, true, "merge")?;
    Ok(ReconKind::Merged)
}

fn rebase_strategy(
    repo: &Repository,
    stable: &StableId,
    local: Oid,
    remote: Oid,
) -> Result<ReconKind> {
    let base_oid = repo.merge_base(local, remote)?;
    // Walk local from tip back to (but not including) base, then reverse so
    // we replay oldest-first.
    let mut to_replay: Vec<Oid> = Vec::new();
    let mut cur = repo.find_commit(local)?;
    while cur.id() != base_oid {
        to_replay.push(cur.id());
        let Ok(parent) = cur.parent(0) else { break };
        cur = parent;
    }
    to_replay.reverse();
    let committer = signature(repo);
    let mut current = remote;
    for c_oid in to_replay {
        let c = repo.find_commit(c_oid)?;
        let parent_tree = c.parent(0)?.tree()?;
        let c_tree = c.tree()?;
        let cur_commit = repo.find_commit(current)?;
        let cur_tree = cur_commit.tree()?;
        let mut idx = repo.merge_trees(&parent_tree, &cur_tree, &c_tree, None)?;
        if idx.has_conflicts() {
            return Ok(ReconKind::Conflict);
        }
        let tree_oid = idx.write_tree_to(repo)?;
        let new_oid = repo.commit(
            None,
            &c.author(),
            &committer,
            c.message().unwrap_or(""),
            &repo.find_tree(tree_oid)?,
            &[&cur_commit],
        )?;
        current = new_oid;
    }
    repo.reference(&stable.refname(), current, true, "rebase")?;
    Ok(ReconKind::Rebased)
}

fn signature(repo: &Repository) -> Signature<'static> {
    repo.signature()
        .map(|s| s.to_owned())
        .unwrap_or_else(|_| Signature::now("tsk", "tsk@local").unwrap())
}

/// After task refs are reconciled, copy every other fetched ref
/// (`refs/tsk-fetched/<remote>/{namespaces,queues,properties}/*`) onto its
/// `refs/tsk/*` counterpart with force-update. Better merging for these is
/// tracked separately (queue merge, namespace renumber, etc.).
pub fn fast_forward_non_task_refs(repo: &Repository, remote: &str) -> Result<()> {
    let prefix = fetched_prefix(remote);
    let names: Vec<String> = repo
        .references_glob(&format!("{prefix}*"))?
        .filter_map(|r| r.ok().and_then(|r| r.name().map(String::from)))
        .collect();
    for name in names {
        let Some(rest) = name.strip_prefix(prefix.as_str()) else {
            continue;
        };
        if rest.starts_with("tasks/") {
            continue;
        }
        let Some(target) = repo.find_reference(&name).ok().and_then(|r| r.target()) else {
            continue;
        };
        let local_name = format!("refs/tsk/{rest}");
        repo.reference(&local_name, target, true, "pull")?;
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::object::{self, Task};
    use std::path::Path;

    fn init_repo(p: &Path) -> Repository {
        let r = Repository::init(p).unwrap();
        let mut cfg = r.config().unwrap();
        cfg.set_str("user.name", "Tester").unwrap();
        cfg.set_str("user.email", "t@e").unwrap();
        r
    }

    /// Set up a divergent pair of refs in one repo: local at refs/tsk/tasks/<s>
    /// and a "fetched-from-origin" tip at refs/tsk-fetched/origin/tasks/<s>.
    /// `local_props` and `remote_props` get applied to the same root content.
    fn make_diverged(
        repo: &Repository,
        content: &str,
        local_props: &[(&str, &str)],
        remote_props: &[(&str, &str)],
    ) -> StableId {
        let stable = object::create(repo, &Task::new(content), "create").unwrap();
        let root_oid = repo
            .find_reference(&stable.refname())
            .unwrap()
            .target()
            .unwrap();
        // Local edit.
        let mut t_local = Task::new(content);
        for (k, v) in local_props {
            t_local
                .properties
                .insert((*k).to_string(), vec![(*v).to_string()]);
        }
        object::update(repo, &stable, &t_local, "edit-local").unwrap();
        // Build remote commit branching off the root.
        let mut t_remote = Task::new(content);
        for (k, v) in remote_props {
            t_remote
                .properties
                .insert((*k).to_string(), vec![(*v).to_string()]);
        }
        let content_oid = repo.blob(t_remote.content.as_bytes()).unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("content", content_oid, 0o100644).unwrap();
        let title_oid = repo.blob(t_remote.title().as_bytes()).unwrap();
        tb.insert("title", title_oid, 0o100644).unwrap();
        for (k, vs) in &t_remote.properties {
            let body: String = vs.iter().map(|v| format!("{v}\n")).collect();
            let oid = repo.blob(body.as_bytes()).unwrap();
            tb.insert(k.as_str(), oid, 0o100644).unwrap();
        }
        let tree_oid = tb.write().unwrap();
        let sig = Signature::now("Remote", "r@x").unwrap();
        let parent = repo.find_commit(root_oid).unwrap();
        let remote_oid = repo
            .commit(
                None,
                &sig,
                &sig,
                "edit-remote",
                &repo.find_tree(tree_oid).unwrap(),
                &[&parent],
            )
            .unwrap();
        repo.reference(
            &format!("refs/tsk-fetched/origin/tasks/{}", stable.0),
            remote_oid,
            true,
            "test-setup",
        )
        .unwrap();
        stable
    }

    #[test]
    fn merge_clean_when_edits_dont_overlap() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let stable = make_diverged(
            &repo,
            "shared",
            &[("priority", "high")],
            &[("status", "urgent")],
        );
        let recs = reconcile_task_refs(&repo, "origin", Strategy::Merge).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].kind, ReconKind::Merged);
        // Merge commit has two parents.
        let head = repo
            .find_reference(&stable.refname())
            .unwrap()
            .target()
            .unwrap();
        let merge = repo.find_commit(head).unwrap();
        assert_eq!(merge.parent_count(), 2);
        // Both property changes survived.
        let task = object::read(&repo, &stable).unwrap().unwrap();
        assert_eq!(task.properties.get("priority").unwrap(), &vec!["high"]);
        assert_eq!(task.properties.get("status").unwrap(), &vec!["urgent"]);
    }

    #[test]
    fn rebase_replays_local_on_remote_preserving_authors() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let stable = make_diverged(
            &repo,
            "shared",
            &[("priority", "high")],
            &[("status", "urgent")],
        );
        let recs = reconcile_task_refs(&repo, "origin", Strategy::Rebase).unwrap();
        assert_eq!(recs[0].kind, ReconKind::Rebased);
        // Rebased tip should be a single-parent commit whose parent chain
        // traces back through the remote's edit.
        let head = repo
            .find_reference(&stable.refname())
            .unwrap()
            .target()
            .unwrap();
        let tip = repo.find_commit(head).unwrap();
        assert_eq!(tip.parent_count(), 1);
        // Author of the rebased tip preserved (Tester from the local edit).
        assert_eq!(tip.author().name().unwrap(), "Tester");
        // Parent is the remote commit, authored by "Remote".
        let parent = tip.parent(0).unwrap();
        assert_eq!(parent.author().name().unwrap(), "Remote");
        let task = object::read(&repo, &stable).unwrap().unwrap();
        assert_eq!(task.properties.get("priority").unwrap(), &vec!["high"]);
        assert_eq!(task.properties.get("status").unwrap(), &vec!["urgent"]);
    }

    #[test]
    fn conflict_leaves_local_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let stable = object::create(&repo, &Task::new("v0"), "create").unwrap();
        let root_oid = repo
            .find_reference(&stable.refname())
            .unwrap()
            .target()
            .unwrap();
        // Local: change content to "v-local".
        let mut t_local = Task::new("v-local");
        object::update(&repo, &stable, &t_local, "edit-local").unwrap();
        let local_tip = repo
            .find_reference(&stable.refname())
            .unwrap()
            .target()
            .unwrap();
        // Remote: branch off root with "v-remote".
        t_local.content = "v-remote".into();
        let content_oid = repo.blob(t_local.content.as_bytes()).unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("content", content_oid, 0o100644).unwrap();
        let title_oid = repo.blob(t_local.title().as_bytes()).unwrap();
        tb.insert("title", title_oid, 0o100644).unwrap();
        let tree_oid = tb.write().unwrap();
        let sig = Signature::now("Remote", "r@x").unwrap();
        let parent = repo.find_commit(root_oid).unwrap();
        let remote_oid = repo
            .commit(
                None,
                &sig,
                &sig,
                "edit-remote",
                &repo.find_tree(tree_oid).unwrap(),
                &[&parent],
            )
            .unwrap();
        repo.reference(
            &format!("refs/tsk-fetched/origin/tasks/{}", stable.0),
            remote_oid,
            true,
            "test",
        )
        .unwrap();
        let recs = reconcile_task_refs(&repo, "origin", Strategy::Merge).unwrap();
        assert_eq!(recs[0].kind, ReconKind::Conflict);
        // Local ref unchanged.
        let head = repo
            .find_reference(&stable.refname())
            .unwrap()
            .target()
            .unwrap();
        assert_eq!(head, local_tip);
    }

    #[test]
    fn fast_forward_when_local_is_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        let stable = object::create(&repo, &Task::new("v0"), "create").unwrap();
        let root_oid = repo
            .find_reference(&stable.refname())
            .unwrap()
            .target()
            .unwrap();
        // Build a remote with one extra commit on top of the root.
        let content_oid = repo.blob(b"v0").unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("content", content_oid, 0o100644).unwrap();
        tb.insert("title", repo.blob(b"v0").unwrap(), 0o100644).unwrap();
        tb.insert("status", repo.blob(b"open\n").unwrap(), 0o100644)
            .unwrap();
        let tree_oid = tb.write().unwrap();
        let sig = Signature::now("Remote", "r@x").unwrap();
        let parent = repo.find_commit(root_oid).unwrap();
        let remote_oid = repo
            .commit(
                None,
                &sig,
                &sig,
                "edit-remote",
                &repo.find_tree(tree_oid).unwrap(),
                &[&parent],
            )
            .unwrap();
        repo.reference(
            &format!("refs/tsk-fetched/origin/tasks/{}", stable.0),
            remote_oid,
            true,
            "test",
        )
        .unwrap();
        let recs = reconcile_task_refs(&repo, "origin", Strategy::Merge).unwrap();
        assert_eq!(recs[0].kind, ReconKind::FastForward);
        let head = repo
            .find_reference(&stable.refname())
            .unwrap()
            .target()
            .unwrap();
        assert_eq!(head, remote_oid);
    }

    #[test]
    fn new_remote_task_is_imported() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo(dir.path());
        // No local task; stash one only at the fetched ref.
        let stable = object::create(&repo, &Task::new("foreign"), "create").unwrap();
        let oid = repo
            .find_reference(&stable.refname())
            .unwrap()
            .target()
            .unwrap();
        // Move the local ref away so only fetched exists.
        repo.find_reference(&stable.refname())
            .unwrap()
            .delete()
            .unwrap();
        repo.reference(
            &format!("refs/tsk-fetched/origin/tasks/{}", stable.0),
            oid,
            true,
            "test",
        )
        .unwrap();
        let recs = reconcile_task_refs(&repo, "origin", Strategy::Merge).unwrap();
        assert_eq!(recs[0].kind, ReconKind::NewRemote);
        assert!(repo.find_reference(&stable.refname()).is_ok());
    }
}
