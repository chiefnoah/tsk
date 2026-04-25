//! Mirror tsk workspace state into git refs under `refs/tsk/`.
//!
//! When `tsk init` is run inside a git repository, a `.tsk/git-backed` marker is
//! written containing the absolute path to the `.git` directory. After every
//! mutating command, [`sync`] walks the workspace and writes each task / index
//! file as a git blob, updating refs to point at them. The on-disk files remain
//! the source of truth; git refs are an additive durable mirror.

use crate::errors::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const MARKER: &str = "git-backed";
const REF_PREFIX: &str = "refs/tsk";

pub fn detect_git_dir(start: &Path) -> Option<PathBuf> {
    crate::util::find_parent_with_dir(start.to_path_buf(), ".git").ok().flatten()
}

pub fn write_marker(tsk_dir: &Path, git_dir: &Path) -> Result<()> {
    std::fs::write(tsk_dir.join(MARKER), git_dir.to_string_lossy().as_bytes())?;
    Ok(())
}

pub fn read_marker(tsk_dir: &Path) -> Option<PathBuf> {
    let s = std::fs::read_to_string(tsk_dir.join(MARKER)).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

fn git(git_dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.env("GIT_DIR", git_dir);
    c
}

fn hash_object(git_dir: &Path, path: &Path) -> Result<String> {
    let out = git(git_dir)
        .args(["hash-object", "-w", "--"])
        .arg(path)
        .stderr(Stdio::piped())
        .output()?;
    if !out.status.success() {
        return Err(Error::Parse(format!(
            "git hash-object failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn update_ref(git_dir: &Path, refname: &str, hash: &str) -> Result<()> {
    let status = git(git_dir)
        .args(["update-ref", refname, hash])
        .stderr(Stdio::piped())
        .status()?;
    if !status.success() {
        return Err(Error::Parse(format!("git update-ref {refname} failed")));
    }
    Ok(())
}

fn delete_ref(git_dir: &Path, refname: &str) -> Result<()> {
    let _ = git(git_dir)
        .args(["update-ref", "-d", refname])
        .stderr(Stdio::null())
        .status()?;
    Ok(())
}

fn list_refs(git_dir: &Path, prefix: &str) -> Result<Vec<String>> {
    let out = git(git_dir)
        .args(["for-each-ref", "--format=%(refname)", prefix])
        .output()?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| s.to_string())
        .collect())
}

/// Walk the workspace and mirror its contents to git refs. No-op if no marker.
pub fn sync(tsk_dir: &Path) -> Result<()> {
    let Some(git_dir) = read_marker(tsk_dir) else {
        return Ok(());
    };
    if !git_dir.exists() {
        return Ok(());
    }

    let mut wanted: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Mirror archive task contents.
    let archive_dir = tsk_dir.join("archive");
    if archive_dir.exists() {
        for entry in std::fs::read_dir(&archive_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.starts_with("tsk-") || !name.ends_with(".tsk") {
                continue;
            }
            let hash = hash_object(&git_dir, &path)?;
            // Determine whether the task is currently active (has a symlink in tasks/).
            let active = tsk_dir.join("tasks").join(name).exists();
            let bucket = if active { "tasks" } else { "archive" };
            let refname = format!("{REF_PREFIX}/{bucket}/{}", name.trim_end_matches(".tsk"));
            update_ref(&git_dir, &refname, &hash)?;
            wanted.insert(refname);
        }
    }

    // Mirror top-level metadata files.
    for meta in ["index", "next", "cache", "remotes"] {
        let path = tsk_dir.join(meta);
        if path.is_file() {
            let hash = hash_object(&git_dir, &path)?;
            let refname = format!("{REF_PREFIX}/meta/{meta}");
            update_ref(&git_dir, &refname, &hash)?;
            wanted.insert(refname);
        }
    }

    // Prune stale refs.
    for refname in list_refs(&git_dir, REF_PREFIX)? {
        if !wanted.contains(&refname) {
            delete_ref(&git_dir, &refname)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    fn run(cmd: &mut Command) {
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{:?}", out);
    }

    #[test]
    fn test_detect_and_sync() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Initialize a real git repo.
        run(Command::new("git").args(["init", "-q"]).current_dir(root));

        // Create a .tsk workspace inside it.
        let tsk_dir = root.join(".tsk");
        std::fs::create_dir(&tsk_dir).unwrap();
        std::fs::create_dir(tsk_dir.join("tasks")).unwrap();
        std::fs::create_dir(tsk_dir.join("archive")).unwrap();

        let git_dir = detect_git_dir(&tsk_dir).expect("git dir found");
        write_marker(&tsk_dir, &git_dir).unwrap();
        assert_eq!(read_marker(&tsk_dir), Some(git_dir.clone()));

        // Create one active task and one archived task.
        std::fs::write(tsk_dir.join("archive/tsk-1.tsk"), "active title\n\nbody").unwrap();
        std::os::unix::fs::symlink(
            PathBuf::from("../archive/tsk-1.tsk"),
            tsk_dir.join("tasks/tsk-1.tsk"),
        )
        .unwrap();
        std::fs::write(tsk_dir.join("archive/tsk-2.tsk"), "archived title\n\n").unwrap();
        std::fs::write(tsk_dir.join("index"), "tsk-1\tactive title\t0\n").unwrap();
        std::fs::write(tsk_dir.join("next"), "3\n").unwrap();

        sync(&tsk_dir).unwrap();

        let refs = list_refs(&git_dir, REF_PREFIX).unwrap();
        assert!(refs.contains(&"refs/tsk/tasks/tsk-1".to_string()));
        assert!(refs.contains(&"refs/tsk/archive/tsk-2".to_string()));
        assert!(refs.contains(&"refs/tsk/meta/index".to_string()));
        assert!(refs.contains(&"refs/tsk/meta/next".to_string()));

        // Drop tsk-1 (remove symlink) and re-sync; ref should move to archive.
        std::fs::remove_file(tsk_dir.join("tasks/tsk-1.tsk")).unwrap();
        sync(&tsk_dir).unwrap();
        let refs = list_refs(&git_dir, REF_PREFIX).unwrap();
        assert!(!refs.contains(&"refs/tsk/tasks/tsk-1".to_string()));
        assert!(refs.contains(&"refs/tsk/archive/tsk-1".to_string()));
    }

    #[test]
    fn test_sync_noop_without_marker() {
        let dir = tempfile::tempdir().unwrap();
        let tsk_dir = dir.path().join(".tsk");
        std::fs::create_dir(&tsk_dir).unwrap();
        // No marker, no git dir — should not error.
        sync(&tsk_dir).unwrap();
    }
}
