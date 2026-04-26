//! Storage backends for tsk workspaces.
//!
//! A [`Store`] is a logical key/value blob store. Two impls are provided:
//!
//! - [`FileStore`] keeps blobs as files under `.tsk/`. Used when `tsk init` runs
//!   outside a git repository.
//! - [`GitStore`] stores each blob as a git blob, addressed by a ref under
//!   `refs/tsk/`. Used when `tsk init` runs inside a git repository — the git
//!   refs are the only durable storage; nothing is cached on disk.
//!
//! Higher-level operations (tasks, attrs, backlinks, index, remotes) are
//! implemented as free functions over `dyn Store` so both backends share a
//! single implementation.

use crate::errors::{Error, Result};
use crate::workspace::{Id, Remote};
use git2::{ObjectType, Reference, Repository};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub const GIT_BACKED_MARKER: &str = "git-backed";
pub const NAMESPACE_FILE: &str = "namespace";
pub const DEFAULT_NAMESPACE: &str = "default";
const REF_ROOT: &str = "refs/tsk";

/// A logical blob store. Keys are forward-slash separated strings.
pub trait Store: Send + Sync {
    fn read(&self, key: &str) -> Result<Option<Vec<u8>>>;
    fn write(&self, key: &str, data: &[u8]) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
    fn exists(&self, key: &str) -> Result<bool>;
    /// List all keys with the given prefix (no trailing slash). Returns full keys.
    fn list(&self, prefix: &str) -> Result<Vec<String>>;
}

// ─── FileStore ──────────────────────────────────────────────────────────────

pub struct FileStore {
    pub root: PathBuf,
}

impl FileStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn path(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }
}

impl Store for FileStore {
    fn read(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let p = self.path(key);
        match fs::read(&p) {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn write(&self, key: &str, data: &[u8]) -> Result<()> {
        let p = self.path(key);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = p.with_extension("tmp");
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        fs::rename(&tmp, &p)?;
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<()> {
        match fs::remove_file(self.path(key)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn exists(&self, key: &str) -> Result<bool> {
        Ok(self.path(key).exists())
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let dir = self.path(prefix);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && let Some(name) = entry.file_name().to_str()
            {
                out.push(format!("{prefix}/{name}"));
            }
        }
        Ok(out)
    }
}

// ─── GitStore ───────────────────────────────────────────────────────────────

pub struct GitStore {
    git_dir: PathBuf,
    namespace: String,
}

impl GitStore {
    pub fn open(git_dir: PathBuf) -> Result<Self> {
        Self::open_namespace(git_dir, DEFAULT_NAMESPACE.to_string())
    }

    pub fn open_namespace(git_dir: PathBuf, namespace: String) -> Result<Self> {
        Repository::open(&git_dir)?;
        Ok(Self { git_dir, namespace })
    }

    fn repo(&self) -> Result<Repository> {
        Ok(Repository::open(&self.git_dir)?)
    }

    /// Prefix every namespace's refs share, e.g. `refs/tsk/<ns>`.
    fn ns_prefix(&self) -> String {
        format!("{REF_ROOT}/{}", self.namespace)
    }

    fn refname(&self, key: &str) -> String {
        format!("{}/{}", self.ns_prefix(), key)
    }

    /// Names of every ref starting with the given prefix.
    fn refs_starting_with(&self, prefix: &str) -> Result<Vec<String>> {
        Ok(self
            .repo()?
            .references()?
            .filter_map(|r| r.ok().and_then(|r| r.name().map(str::to_string)))
            .filter(|n| n.starts_with(prefix))
            .collect())
    }

    /// Number of refs currently under this store's namespace.
    pub fn namespace_ref_count(&self) -> Result<usize> {
        Ok(self
            .refs_starting_with(&format!("{}/", self.ns_prefix()))?
            .len())
    }

    /// Delete every ref under this store's namespace. Returns the count.
    pub fn delete_namespace_refs(&self) -> Result<usize> {
        let repo = self.repo()?;
        let names = self.refs_starting_with(&format!("{}/", self.ns_prefix()))?;
        let count = names.len();
        for n in names {
            if let Some(mut r) = try_ref(&repo, &n)? {
                r.delete()?;
            }
        }
        Ok(count)
    }

    /// List the namespaces present in this repo (any directory under refs/tsk/
    /// containing at least one ref).
    pub fn list_namespaces(&self) -> Result<Vec<String>> {
        let strip = format!("{REF_ROOT}/");
        let mut out: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for name in self.refs_starting_with(&strip)? {
            if let Some(rest) = name.strip_prefix(&strip)
                && let Some((ns, _)) = rest.split_once('/')
            {
                out.insert(ns.to_string());
            }
        }
        Ok(out.into_iter().collect())
    }
}

/// `find_reference` translating NotFound to None.
fn try_ref<'r>(repo: &'r Repository, name: &str) -> Result<Option<Reference<'r>>> {
    match repo.find_reference(name) {
        Ok(r) => Ok(Some(r)),
        Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

impl Store for GitStore {
    fn read(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let repo = self.repo()?;
        let Some(r) = try_ref(&repo, &self.refname(key))? else {
            return Ok(None);
        };
        let blob = r.peel(ObjectType::Blob)?;
        Ok(Some(
            blob.as_blob()
                .ok_or_else(|| Error::Parse("not a blob".into()))?
                .content()
                .to_vec(),
        ))
    }

    fn write(&self, key: &str, data: &[u8]) -> Result<()> {
        let repo = self.repo()?;
        let oid = repo.blob(data)?;
        repo.reference(&self.refname(key), oid, true, "tsk write")?;
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<()> {
        let repo = self.repo()?;
        if let Some(mut r) = try_ref(&repo, &self.refname(key))? {
            r.delete()?;
        }
        Ok(())
    }

    fn exists(&self, key: &str) -> Result<bool> {
        Ok(try_ref(&self.repo()?, &self.refname(key))?.is_some())
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let repo = self.repo()?;
        let strip = format!("{}/", self.ns_prefix());
        repo.references_glob(&format!("{}/{}/*", self.ns_prefix(), prefix))?
            .filter_map(|r| {
                r.ok()
                    .and_then(|r| {
                        r.name()
                            .and_then(|n| n.strip_prefix(&strip))
                            .map(str::to_string)
                    })
                    .map(Ok)
            })
            .collect()
    }
}

// ─── High-level operations over any Store ───────────────────────────────────

pub fn next_id(store: &dyn Store) -> Result<Id> {
    let cur = store
        .read("next")?
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .unwrap_or_else(|| "1".to_string());
    let id: u32 = cur.parse().unwrap_or(1);
    store.write("next", format!("{}\n", id + 1).as_bytes())?;
    Ok(Id(id))
}

fn task_key(id: Id, archived: bool) -> String {
    let bucket = if archived { "archive" } else { "tasks" };
    format!("{bucket}/{}", id.0)
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Loc {
    Active,
    Archived,
}

pub fn task_location(store: &dyn Store, id: Id) -> Result<Option<Loc>> {
    if store.exists(&task_key(id, false))? {
        Ok(Some(Loc::Active))
    } else if store.exists(&task_key(id, true))? {
        Ok(Some(Loc::Archived))
    } else {
        Ok(None)
    }
}

pub fn read_task(store: &dyn Store, id: Id) -> Result<Option<(String, String, Loc)>> {
    for (loc, archived) in [(Loc::Active, false), (Loc::Archived, true)] {
        if let Some(data) = store.read(&task_key(id, archived))? {
            let text = String::from_utf8_lossy(&data);
            let mut parts = text.splitn(2, '\n');
            let title = parts.next().unwrap_or("").trim().to_string();
            let body = parts.next().unwrap_or("").trim().to_string();
            return Ok(Some((title, body, loc)));
        }
    }
    Ok(None)
}

pub fn write_task(store: &dyn Store, id: Id, title: &str, body: &str, loc: Loc) -> Result<()> {
    let payload = format!("{}\n\n{}", title.trim(), body.trim());
    store.write(&task_key(id, loc == Loc::Archived), payload.as_bytes())?;
    Ok(())
}

pub fn move_task(store: &dyn Store, id: Id, to: Loc) -> Result<()> {
    let from_archived = to == Loc::Active;
    let from_key = task_key(id, from_archived);
    let to_key = task_key(id, to == Loc::Archived);
    if from_key == to_key {
        return Ok(());
    }
    let data = store
        .read(&from_key)?
        .ok_or_else(|| Error::Parse(format!("task {id} not present at {from_key}")))?;
    store.write(&to_key, &data)?;
    store.delete(&from_key)?;
    Ok(())
}

pub fn list_active(store: &dyn Store) -> Result<Vec<Id>> {
    list_bucket(store, "tasks")
}

pub fn list_archive(store: &dyn Store) -> Result<Vec<Id>> {
    list_bucket(store, "archive")
}

fn list_bucket(store: &dyn Store, bucket: &str) -> Result<Vec<Id>> {
    let prefix = format!("{bucket}/");
    let mut ids: Vec<Id> = store
        .list(bucket)?
        .iter()
        .filter_map(|k| {
            k.strip_prefix(&prefix)?
                .trim_end_matches(".tsk")
                .parse()
                .ok()
                .map(Id)
        })
        .collect();
    ids.sort_by_key(|i| i.0);
    Ok(ids)
}

/// Read+lossy-decode a blob, returning empty string when absent.
fn read_text(store: &dyn Store, key: &str) -> Result<String> {
    Ok(store
        .read(key)?
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default())
}

/// Write `body` to `key`, or delete the blob if `body` is empty.
fn write_or_delete(store: &dyn Store, key: &str, body: &str) -> Result<()> {
    if body.is_empty() {
        store.delete(key)
    } else {
        store.write(key, body.as_bytes())
    }
}

pub fn read_attrs(store: &dyn Store, id: Id) -> Result<BTreeMap<String, String>> {
    Ok(read_text(store, &format!("attrs/{}", id.0))?
        .lines()
        .filter_map(|l| {
            l.split_once('\t')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect())
}

pub fn write_attrs(store: &dyn Store, id: Id, attrs: &BTreeMap<String, String>) -> Result<()> {
    let body = attrs
        .iter()
        .map(|(k, v)| format!("{k}\t{v}\n"))
        .collect::<String>();
    write_or_delete(store, &format!("attrs/{}", id.0), &body)
}

pub fn read_backlinks(store: &dyn Store, id: Id) -> Result<HashSet<Id>> {
    Ok(read_text(store, &format!("backlinks/{}", id.0))?
        .split(',')
        .filter_map(|t| Id::from_str(t.trim()).ok())
        .collect())
}

pub fn write_backlinks(store: &dyn Store, id: Id, links: &HashSet<Id>) -> Result<()> {
    write_or_delete(
        store,
        &format!("backlinks/{}", id.0),
        &itertools::join(links, ","),
    )
}

pub fn read_remotes(store: &dyn Store) -> Result<Vec<Remote>> {
    Ok(read_text(store, "remotes")?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('\t'))
        .map(|(p, path)| Remote {
            prefix: p.trim().into(),
            path: PathBuf::from(path.trim()),
        })
        .collect())
}

pub fn write_remotes(store: &dyn Store, remotes: &[Remote]) -> Result<()> {
    let body: String = remotes
        .iter()
        .map(|r| format!("{}\t{}\n", r.prefix, r.path.display()))
        .collect();
    write_or_delete(store, "remotes", &body)
}

// ─── Detection / construction ──────────────────────────────────────────────

pub fn detect_git_dir(start: &Path) -> Option<PathBuf> {
    crate::util::find_parent_with_dir(start.to_path_buf(), ".git")
        .ok()
        .flatten()
}

pub fn read_namespace(tsk_dir: &Path) -> String {
    fs::read_to_string(tsk_dir.join(NAMESPACE_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_NAMESPACE.to_string())
}

pub fn write_namespace(tsk_dir: &Path, namespace: &str) -> Result<()> {
    fs::write(tsk_dir.join(NAMESPACE_FILE), namespace.as_bytes())?;
    Ok(())
}

pub fn store_for(tsk_dir: &Path) -> Result<Box<dyn Store>> {
    let marker = tsk_dir.join(GIT_BACKED_MARKER);
    if marker.exists() {
        let git_dir = PathBuf::from(fs::read_to_string(&marker)?.trim());
        // First: rename any non-namespaced refs into the default namespace, so
        // workspaces created before namespacing keep working seamlessly.
        let probe = GitStore::open(git_dir.clone())?;
        upgrade_to_namespaced(&probe)?;
        let ns = read_namespace(tsk_dir);
        let store = GitStore::open_namespace(git_dir, ns)?;
        upgrade_legacy_keys(&store)?;
        Ok(Box::new(store))
    } else {
        Ok(Box::new(FileStore::new(tsk_dir.to_path_buf())))
    }
}

/// Move any non-namespaced refs (`refs/tsk/<key>`, `refs/tsk/<bucket>/<id>`)
/// into the `default` namespace (`refs/tsk/default/...`). Idempotent.
fn upgrade_to_namespaced(probe: &GitStore) -> Result<()> {
    let repo = probe.repo()?;
    let strip = format!("{REF_ROOT}/");
    let mut moves: Vec<(String, String)> = Vec::new();
    for r in repo.references_glob(&format!("{REF_ROOT}/*"))? {
        let r = r?;
        let Some(name) = r.name() else { continue };
        let Some(rest) = name.strip_prefix(&strip) else {
            continue;
        };
        // Skip already-namespaced refs: first segment is a known top-level key,
        // any other first segment is treated as a namespace.
        let first = rest.split('/').next().unwrap_or("");
        let is_legacy = matches!(
            first,
            "tasks" | "archive" | "attrs" | "backlinks" | "index" | "next" | "remotes"
        );
        if is_legacy {
            moves.push((
                name.to_string(),
                format!("{REF_ROOT}/{DEFAULT_NAMESPACE}/{rest}"),
            ));
        }
    }
    for (old, new) in moves {
        if let Some(r) = try_ref(&repo, &old)?
            && let Some(oid) = r.target()
        {
            repo.reference(&new, oid, true, "tsk namespace upgrade")?;
            if let Some(mut r) = try_ref(&repo, &old)? {
                r.delete()?;
            }
        }
    }
    Ok(())
}

/// Rename legacy-scheme refs (`tasks/tsk-N.tsk`) to the current scheme
/// (`tasks/N`). Older versions of the git backend named blobs after the file
/// path used by the file backend; the current scheme uses just the integer id.
/// Runs on every open so stale workspaces self-heal on first use.
fn upgrade_legacy_keys(store: &dyn Store) -> Result<()> {
    for bucket in ["tasks", "archive"] {
        for key in store.list(bucket)? {
            // key looks like "tasks/<name>" — strip prefix to get the leaf.
            let leaf = key.split('/').next_back().unwrap_or("");
            // Legacy names look like "tsk-N.tsk". New names are just "N".
            if let Some(num) = leaf
                .strip_prefix("tsk-")
                .and_then(|s| s.strip_suffix(".tsk"))
                && num.parse::<u32>().is_ok()
            {
                let new_key = format!("{bucket}/{num}");
                if store.exists(&new_key)? {
                    // New-scheme blob already present; just drop the legacy one.
                    store.delete(&key)?;
                    continue;
                }
                if let Some(data) = store.read(&key)? {
                    store.write(&new_key, &data)?;
                    store.delete(&key)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    fn run_git_init(dir: &Path) {
        let s = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(s.success());
    }

    fn store_pair() -> (tempfile::TempDir, Box<dyn Store>, Box<dyn Store>) {
        let dir = tempfile::tempdir().unwrap();
        let file_root = dir.path().join("file");
        let git_root = dir.path().join("git");
        fs::create_dir_all(&file_root).unwrap();
        fs::create_dir_all(&git_root).unwrap();
        run_git_init(&git_root);
        let f: Box<dyn Store> = Box::new(FileStore::new(file_root));
        let g: Box<dyn Store> = Box::new(GitStore::open(git_root.join(".git")).unwrap());
        (dir, f, g)
    }

    #[test]
    fn test_basic_blob_ops_both_backends() {
        let (_d, file, git) = store_pair();
        for s in [file, git] {
            assert_eq!(s.read("missing").unwrap(), None);
            assert!(!s.exists("k").unwrap());
            s.write("k", b"hello").unwrap();
            assert!(s.exists("k").unwrap());
            assert_eq!(s.read("k").unwrap().as_deref(), Some(&b"hello"[..]));
            s.write("k", b"world").unwrap();
            assert_eq!(s.read("k").unwrap().as_deref(), Some(&b"world"[..]));
            s.delete("k").unwrap();
            assert!(!s.exists("k").unwrap());
            // delete nonexistent is fine
            s.delete("k").unwrap();
        }
    }

    #[test]
    fn test_list_both_backends() {
        let (_d, file, git) = store_pair();
        for s in [file, git] {
            s.write("tasks/1", b"a").unwrap();
            s.write("tasks/2", b"b").unwrap();
            s.write("archive/3", b"c").unwrap();
            let mut tasks = s.list("tasks").unwrap();
            tasks.sort();
            assert_eq!(tasks, vec!["tasks/1", "tasks/2"]);
            let arch = s.list("archive").unwrap();
            assert_eq!(arch, vec!["archive/3"]);
            assert!(s.list("nothing").unwrap().is_empty());
        }
    }

    #[test]
    fn test_high_level_task_ops_both_backends() {
        let (_d, file, git) = store_pair();
        for s in [file.as_ref(), git.as_ref()] {
            let id = next_id(s).unwrap();
            assert_eq!(id, Id(1));
            let id2 = next_id(s).unwrap();
            assert_eq!(id2, Id(2));

            write_task(s, id, "title", "body", Loc::Active).unwrap();
            let (t, b, loc) = read_task(s, id).unwrap().unwrap();
            assert_eq!(t, "title");
            assert_eq!(b, "body");
            assert_eq!(loc, Loc::Active);

            move_task(s, id, Loc::Archived).unwrap();
            assert_eq!(task_location(s, id).unwrap(), Some(Loc::Archived));
            move_task(s, id, Loc::Active).unwrap();
            assert_eq!(task_location(s, id).unwrap(), Some(Loc::Active));

            let mut attrs = BTreeMap::new();
            attrs.insert("foo".to_string(), "bar".to_string());
            write_attrs(s, id, &attrs).unwrap();
            assert_eq!(read_attrs(s, id).unwrap(), attrs);

            let mut bl = HashSet::new();
            bl.insert(Id(7));
            bl.insert(Id(9));
            write_backlinks(s, id, &bl).unwrap();
            assert_eq!(read_backlinks(s, id).unwrap(), bl);

            // Empty attrs/backlinks delete the blob.
            write_attrs(s, id, &BTreeMap::new()).unwrap();
            assert!(read_attrs(s, id).unwrap().is_empty());
            write_backlinks(s, id, &HashSet::new()).unwrap();
            assert!(read_backlinks(s, id).unwrap().is_empty());
        }
    }

    #[test]
    fn test_remotes_round_trip_both_backends() {
        let (_d, file, git) = store_pair();
        for s in [file.as_ref(), git.as_ref()] {
            assert!(read_remotes(s).unwrap().is_empty());
            let remotes = vec![
                Remote {
                    prefix: "a".into(),
                    path: PathBuf::from("/x"),
                },
                Remote {
                    prefix: "b".into(),
                    path: PathBuf::from("/y"),
                },
            ];
            write_remotes(s, &remotes).unwrap();
            assert_eq!(read_remotes(s).unwrap(), remotes);
            write_remotes(s, &[]).unwrap();
            assert!(read_remotes(s).unwrap().is_empty());
        }
    }

    #[test]
    fn test_upgrade_legacy_keys_renames_old_scheme() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        run_git_init(&root);
        let store = GitStore::open(root.join(".git")).unwrap();

        // Seed legacy-scheme refs (what older versions of tsk wrote).
        store.write("tasks/tsk-1.tsk", b"old\n\nbody").unwrap();
        store.write("archive/tsk-2.tsk", b"old2\n\nbody2").unwrap();
        // And one already-correct new-scheme ref alongside.
        store.write("tasks/3", b"new\n\nbody3").unwrap();

        upgrade_legacy_keys(&store).unwrap();

        // Legacy keys should be gone, new-scheme keys present.
        assert!(!store.exists("tasks/tsk-1.tsk").unwrap());
        assert!(!store.exists("archive/tsk-2.tsk").unwrap());
        assert_eq!(
            store.read("tasks/1").unwrap().as_deref(),
            Some(&b"old\n\nbody"[..])
        );
        assert_eq!(
            store.read("archive/2").unwrap().as_deref(),
            Some(&b"old2\n\nbody2"[..])
        );
        assert_eq!(
            store.read("tasks/3").unwrap().as_deref(),
            Some(&b"new\n\nbody3"[..])
        );
    }

    #[test]
    fn test_upgrade_legacy_keys_keeps_new_when_both_present() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        run_git_init(&root);
        let store = GitStore::open(root.join(".git")).unwrap();

        store.write("tasks/tsk-1.tsk", b"legacy").unwrap();
        store.write("tasks/1", b"current").unwrap();
        upgrade_legacy_keys(&store).unwrap();

        assert!(!store.exists("tasks/tsk-1.tsk").unwrap());
        assert_eq!(
            store.read("tasks/1").unwrap().as_deref(),
            Some(&b"current"[..])
        );
    }

    #[test]
    fn test_list_active_archive_helpers() {
        let (_d, file, git) = store_pair();
        for s in [file.as_ref(), git.as_ref()] {
            write_task(s, Id(1), "t1", "", Loc::Active).unwrap();
            write_task(s, Id(2), "t2", "", Loc::Archived).unwrap();
            write_task(s, Id(3), "t3", "", Loc::Active).unwrap();
            assert_eq!(list_active(s).unwrap(), vec![Id(1), Id(3)]);
            assert_eq!(list_archive(s).unwrap(), vec![Id(2)]);
        }
    }
}
