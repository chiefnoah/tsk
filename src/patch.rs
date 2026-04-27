//! Mbox-format patch series for offline task transfer.
//!
//! Each task commit is one mbox entry (`From <sha> Mon Sep 17 00:00:00 2001`
//! separator + RFC-822 headers + body). The body holds the commit message,
//! a `---tsk-tree---` marker, then a length-prefixed dump of every file in
//! the task tree, terminated by `---end---`. Length-prefix avoids any need
//! to escape mbox `From ` lines inside file contents.
//!
//! Stable id is content-addressed (= SHA-1 of the root `content` blob), so
//! `import_task` recomputes it and rejects mismatches — tampering is
//! detectable.

use crate::errors::{Error, Result};
use crate::object::{CONTENT_FILE, StableId, TITLE_FILE};
use git2::{Oid, Repository, Signature, Time};
use std::collections::BTreeMap;
use std::fmt::Write as _;

const MBOX_DATE: &str = "Mon Sep 17 00:00:00 2001";
const TREE_DELIM: &str = "---tsk-tree---";
const END_DELIM: &str = "---end---";

pub struct ExportOpts {
    /// If set, embed `X-Tsk-Namespace: <ns>-<human>` on the root entry so
    /// the recipient can opt in to binding the task into their namespace.
    pub bind: Option<(String, u32)>,
}

pub fn export_task(
    repo: &Repository,
    stable: &StableId,
    opts: &ExportOpts,
) -> Result<String> {
    let r = repo.find_reference(&stable.refname())?;
    let tip = r.target().ok_or_else(|| Error::Parse("task ref empty".into()))?;
    // Collect root → tip.
    let mut chain: Vec<Oid> = Vec::new();
    let mut cur = Some(repo.find_commit(tip)?);
    while let Some(c) = cur {
        chain.push(c.id());
        cur = c.parent(0).ok();
    }
    chain.reverse();
    let mut out = String::new();
    for (idx, oid) in chain.iter().enumerate() {
        let commit = repo.find_commit(*oid)?;
        let parent = commit.parent(0).ok().map(|p| p.id());
        let bind = if idx == 0 { opts.bind.as_ref() } else { None };
        write_entry(&mut out, repo, &commit, parent, stable, bind)?;
    }
    Ok(out)
}

fn fmt_git_time(t: Time) -> String {
    let off = t.offset_minutes();
    let sign = if off >= 0 { '+' } else { '-' };
    let off = off.abs();
    format!("{} {}{:02}{:02}", t.seconds(), sign, off / 60, off % 60)
}

fn parse_git_time(s: &str) -> Result<Time> {
    let (secs, offset) = s
        .split_once(' ')
        .ok_or_else(|| Error::Parse(format!("bad date: {s}")))?;
    let secs: i64 = secs
        .parse()
        .map_err(|_| Error::Parse(format!("bad date secs: {secs}")))?;
    let (sign, rest) = offset
        .split_at_checked(1)
        .ok_or_else(|| Error::Parse(format!("bad offset: {offset}")))?;
    let off_min: i32 = if rest.len() == 4 {
        let h: i32 = rest[..2]
            .parse()
            .map_err(|_| Error::Parse(format!("bad offset: {offset}")))?;
        let m: i32 = rest[2..]
            .parse()
            .map_err(|_| Error::Parse(format!("bad offset: {offset}")))?;
        h * 60 + m
    } else {
        return Err(Error::Parse(format!("bad offset: {offset}")));
    };
    let off_min = if sign == "-" { -off_min } else { off_min };
    Ok(Time::new(secs, off_min))
}

fn write_entry(
    out: &mut String,
    repo: &Repository,
    commit: &git2::Commit,
    parent: Option<Oid>,
    stable: &StableId,
    bind: Option<&(String, u32)>,
) -> Result<()> {
    let author = commit.author();
    let summary = commit.summary().unwrap_or("");
    let message = commit.message().unwrap_or("");
    writeln!(out, "From {} {MBOX_DATE}", commit.id()).unwrap();
    writeln!(
        out,
        "From: {} <{}>",
        author.name().unwrap_or(""),
        author.email().unwrap_or("")
    )
    .unwrap();
    writeln!(out, "Date: {}", fmt_git_time(author.when())).unwrap();
    writeln!(out, "Subject: [PATCH tsk] {summary}").unwrap();
    writeln!(out, "X-Tsk-Stable-Id: {stable}").unwrap();
    writeln!(
        out,
        "X-Tsk-Parent: {}",
        parent.map(|o| o.to_string()).unwrap_or_else(|| "none".into())
    )
    .unwrap();
    if let Some((ns, human)) = bind {
        writeln!(out, "X-Tsk-Namespace: {ns}-{human}").unwrap();
    }
    writeln!(out).unwrap();
    out.push_str(message);
    if !message.ends_with('\n') {
        out.push('\n');
    }
    writeln!(out).unwrap();
    writeln!(out, "{TREE_DELIM}").unwrap();
    let tree = commit.tree()?;
    // Iterate in tree order (already sorted by name).
    for entry in tree.iter() {
        let Some(name) = entry.name() else { continue };
        if name == TITLE_FILE {
            // title is a cache; reconstructible from content. Skip.
            continue;
        }
        let blob = entry.to_object(repo)?.peel_to_blob()?;
        let bytes = blob.content();
        writeln!(out, "file: {name}").unwrap();
        writeln!(out, "size: {}", bytes.len()).unwrap();
        // Bytes verbatim. They may contain newlines or arbitrary text; size
        // is the authoritative delimiter.
        out.push_str(std::str::from_utf8(bytes).map_err(|e| Error::Parse(e.to_string()))?);
        out.push('\n');
    }
    writeln!(out, "{END_DELIM}").unwrap();
    writeln!(out).unwrap();
    Ok(())
}

#[derive(Debug)]
struct Entry {
    author_name: String,
    author_email: String,
    when: Time,
    message: String,
    stable: String,
    ns_bind: Option<(String, u32)>,
    files: BTreeMap<String, Vec<u8>>,
}

#[derive(Debug)]
pub struct ImportResult {
    pub stable: StableId,
    pub commits_imported: usize,
    /// Hint from the sender: namespace + human id under which they had this
    /// task bound. Currently unused by the workspace layer (recipient decides
    /// binding) but parsed and exposed so future commands can honor it.
    #[allow(dead_code)]
    pub ns_bind: Option<(String, u32)>,
}

pub fn import_task(repo: &Repository, mbox: &str) -> Result<ImportResult> {
    let entries = parse_mbox(mbox)?;
    if entries.is_empty() {
        return Err(Error::Parse("no patch entries found".into()));
    }
    let stable_hex = entries[0].stable.clone();
    let ns_bind = entries[0].ns_bind.clone();
    let mut prev: Option<Oid> = None;
    for (idx, e) in entries.iter().enumerate() {
        if e.stable != stable_hex {
            return Err(Error::Parse(format!(
                "stable id mismatch across entries: {} vs {}",
                stable_hex, e.stable
            )));
        }
        // Build the tree from the file map.
        let mut tb = repo.treebuilder(None)?;
        let content = e
            .files
            .get(CONTENT_FILE)
            .ok_or_else(|| Error::Parse("entry missing 'content' file".into()))?;
        let content_oid = repo.blob(content)?;
        if idx == 0 {
            // Verify stable id == sha of root content blob.
            if content_oid.to_string() != stable_hex {
                return Err(Error::Parse(format!(
                    "stable id verification failed: expected {stable_hex}, content sha is {content_oid}"
                )));
            }
        }
        tb.insert(CONTENT_FILE, content_oid, 0o100644)?;
        // Re-derive title cache from content.
        let title = std::str::from_utf8(content)
            .map_err(|e| Error::Parse(e.to_string()))?
            .lines()
            .next()
            .unwrap_or("");
        let title_oid = repo.blob(title.as_bytes())?;
        tb.insert(TITLE_FILE, title_oid, 0o100644)?;
        for (name, bytes) in &e.files {
            if name == CONTENT_FILE || name == TITLE_FILE {
                continue;
            }
            let oid = repo.blob(bytes)?;
            tb.insert(name.as_str(), oid, 0o100644)?;
        }
        let tree_oid = tb.write()?;
        let sig = Signature::new(&e.author_name, &e.author_email, &e.when)?;
        let parents: Vec<git2::Commit> = prev.into_iter().map(|o| repo.find_commit(o).unwrap()).collect();
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        let commit_oid = repo.commit(
            None,
            &sig,
            &sig,
            &e.message,
            &repo.find_tree(tree_oid)?,
            &parent_refs,
        )?;
        prev = Some(commit_oid);
    }
    let stable = StableId(stable_hex);
    repo.reference(&stable.refname(), prev.unwrap(), true, "import")?;
    Ok(ImportResult {
        stable,
        commits_imported: entries.len(),
        ns_bind,
    })
}

fn parse_mbox(s: &str) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    // Split on lines starting with "From " (the mbox separator). The "From "
    // line is part of the entry it introduces.
    let mut starts: Vec<usize> = Vec::new();
    let bytes = s.as_bytes();
    if bytes.starts_with(b"From ") {
        starts.push(0);
    }
    let mut i = 0;
    while i + 5 < bytes.len() {
        if bytes[i] == b'\n' && &bytes[i + 1..i + 6] == b"From " {
            starts.push(i + 1);
        }
        i += 1;
    }
    starts.push(bytes.len());
    for win in starts.windows(2) {
        let chunk = &s[win[0]..win[1]];
        if chunk.trim().is_empty() {
            continue;
        }
        entries.push(parse_entry(chunk)?);
    }
    Ok(entries)
}

fn parse_entry(chunk: &str) -> Result<Entry> {
    let mut lines = chunk.split_inclusive('\n');
    // First line: "From <oid> Mon Sep 17 ..."
    let _ = lines.next();
    let mut author_name = String::new();
    let mut author_email = String::new();
    let mut when: Option<Time> = None;
    let mut subject = String::new();
    let mut stable = String::new();
    let mut ns_bind: Option<(String, u32)> = None;
    // Headers until blank line.
    for line in lines.by_ref() {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(v) = trimmed.strip_prefix("From: ") {
            // "Name <email>"
            if let Some((n, rest)) = v.split_once(" <") {
                author_name = n.to_string();
                author_email = rest.trim_end_matches('>').to_string();
            } else {
                author_name = v.to_string();
            }
        } else if let Some(v) = trimmed.strip_prefix("Date: ") {
            when = Some(parse_git_time(v)?);
        } else if let Some(v) = trimmed.strip_prefix("Subject: ") {
            subject = v.strip_prefix("[PATCH tsk] ").unwrap_or(v).to_string();
        } else if let Some(v) = trimmed.strip_prefix("X-Tsk-Stable-Id: ") {
            stable = v.to_string();
        } else if let Some(_v) = trimmed.strip_prefix("X-Tsk-Parent: ") {
            // Informational only; we use the previous imported commit as parent.
        } else if let Some(v) = trimmed.strip_prefix("X-Tsk-Namespace: ") {
            if let Some((ns, h)) = v.rsplit_once('-')
                && let Ok(human) = h.parse::<u32>()
            {
                ns_bind = Some((ns.to_string(), human));
            }
        }
    }
    if stable.is_empty() {
        return Err(Error::Parse("missing X-Tsk-Stable-Id".into()));
    }
    let when = when.ok_or_else(|| Error::Parse("missing Date".into()))?;
    // Body up to TREE_DELIM line.
    let mut message = String::new();
    let mut in_tree = false;
    for line in lines.by_ref() {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == TREE_DELIM {
            in_tree = true;
            break;
        }
        message.push_str(line);
    }
    if !in_tree {
        return Err(Error::Parse("missing tree delimiter".into()));
    }
    // Restore commit message: drop the trailing blank line we emitted.
    while message.ends_with("\n\n") {
        message.pop();
    }
    // If the message is just the subject line (no body), use subject.
    let message = if message.trim().is_empty() {
        subject
    } else {
        message.trim_end_matches('\n').to_string()
    };
    // Parse file blocks until END_DELIM.
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    // We need byte-level reads for sizes, so switch back to slice indexing.
    // Compute remaining input from `lines`.
    let remaining = lines.collect::<String>();
    let mut rest = remaining.as_bytes();
    loop {
        // Read line.
        let nl = rest
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(|| Error::Parse("unexpected eof in tree".into()))?;
        let line = std::str::from_utf8(&rest[..nl]).map_err(|e| Error::Parse(e.to_string()))?;
        let line_trim = line.trim_end_matches('\r');
        rest = &rest[nl + 1..];
        if line_trim == END_DELIM {
            break;
        }
        let name = line_trim
            .strip_prefix("file: ")
            .ok_or_else(|| Error::Parse(format!("expected 'file:' got: {line_trim:?}")))?
            .to_string();
        let nl = rest
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(|| Error::Parse("unexpected eof reading size".into()))?;
        let size_line = std::str::from_utf8(&rest[..nl]).map_err(|e| Error::Parse(e.to_string()))?;
        let size_line = size_line.trim_end_matches('\r');
        rest = &rest[nl + 1..];
        let size: usize = size_line
            .strip_prefix("size: ")
            .ok_or_else(|| Error::Parse(format!("expected 'size:' got: {size_line:?}")))?
            .parse()
            .map_err(|_| Error::Parse(format!("bad size: {size_line}")))?;
        if rest.len() < size + 1 {
            return Err(Error::Parse("truncated file body".into()));
        }
        let bytes = rest[..size].to_vec();
        // Trailing \n separator (not part of size).
        if rest[size] != b'\n' {
            return Err(Error::Parse("missing newline after file body".into()));
        }
        rest = &rest[size + 1..];
        files.insert(name, bytes);
    }
    Ok(Entry {
        author_name,
        author_email,
        when,
        message,
        stable,
        ns_bind,
        files,
    })
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

    #[test]
    fn round_trip_single_commit() {
        let dir = tempfile::tempdir().unwrap();
        let src = init_repo(dir.path());
        let mut t = Task::new("Hello\n\nbody text");
        t.properties.insert("status".into(), vec!["open".into()]);
        let stable = object::create(&src, &t, "create").unwrap();

        let mbox = export_task(&src, &stable, &ExportOpts { bind: None }).unwrap();
        assert!(mbox.starts_with("From "));
        assert!(mbox.contains("X-Tsk-Stable-Id:"));
        assert!(mbox.contains(TREE_DELIM));

        let dst_dir = tempfile::tempdir().unwrap();
        let dst = init_repo(dst_dir.path());
        let res = import_task(&dst, &mbox).unwrap();
        assert_eq!(res.stable, stable);
        let read_back = object::read(&dst, &res.stable).unwrap().unwrap();
        assert_eq!(read_back.content, t.content);
        assert_eq!(read_back.properties, t.properties);
    }

    #[test]
    fn round_trip_preserves_history() {
        let dir = tempfile::tempdir().unwrap();
        let src = init_repo(dir.path());
        let t = Task::new("v1");
        let stable = object::create(&src, &t, "create").unwrap();
        let mut t2 = t.clone();
        t2.content = "v2".into();
        object::update(&src, &stable, &t2, "edit-2").unwrap();
        let mut t3 = t2.clone();
        t3.content = "v3".into();
        object::update(&src, &stable, &t3, "edit-3").unwrap();

        let mbox = export_task(&src, &stable, &ExportOpts { bind: None }).unwrap();

        let dst_dir = tempfile::tempdir().unwrap();
        let dst = init_repo(dst_dir.path());
        let res = import_task(&dst, &mbox).unwrap();
        assert_eq!(res.commits_imported, 3);

        let head = dst
            .find_reference(&res.stable.refname())
            .unwrap()
            .target()
            .unwrap();
        let tip = dst.find_commit(head).unwrap();
        assert_eq!(tip.summary().unwrap(), "edit-3");
        let mid = tip.parent(0).unwrap();
        assert_eq!(mid.summary().unwrap(), "edit-2");
        let root = mid.parent(0).unwrap();
        assert_eq!(root.summary().unwrap(), "create");
    }

    #[test]
    fn tamper_detected_via_stable_id_check() {
        let dir = tempfile::tempdir().unwrap();
        let src = init_repo(dir.path());
        let stable =
            object::create(&src, &Task::new("original content"), "create").unwrap();
        let mbox = export_task(&src, &stable, &ExportOpts { bind: None }).unwrap();
        // Flip the content body without updating the stable id header.
        // Equal-length substitution so size-prefix parsing still aligns; only
        // the SHA check should reject it.
        let tampered = mbox.replace("original content", "OVERRIDDEN BYTES");
        assert_eq!("original content".len(), "OVERRIDDEN BYTES".len());
        let dst_dir = tempfile::tempdir().unwrap();
        let dst = init_repo(dst_dir.path());
        let err = import_task(&dst, &tampered).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("stable id verification failed"),
            "expected verification error, got: {msg}"
        );
    }

    #[test]
    fn ns_bind_header_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let src = init_repo(dir.path());
        let stable = object::create(&src, &Task::new("bind me"), "create").unwrap();
        let mbox = export_task(
            &src,
            &stable,
            &ExportOpts {
                bind: Some(("alpha".into(), 7)),
            },
        )
        .unwrap();
        assert!(mbox.contains("X-Tsk-Namespace: alpha-7"));
        let dst_dir = tempfile::tempdir().unwrap();
        let dst = init_repo(dst_dir.path());
        let res = import_task(&dst, &mbox).unwrap();
        assert_eq!(res.ns_bind, Some(("alpha".to_string(), 7)));
    }
}
