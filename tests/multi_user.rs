//! End-to-end multi-clone tests. Spins up a bare "origin" repo and two
//! working clones; exercises the user-visible commands across them via the
//! compiled `tsk` binary.

use std::path::{Path, PathBuf};
use std::process::Command;

fn tsk_bin() -> PathBuf {
    // Cargo sets CARGO_BIN_EXE_<name> for each [[bin]] when running tests.
    PathBuf::from(env!("CARGO_BIN_EXE_tsk"))
}

fn run(cmd: &mut Command) -> (i32, String, String) {
    let out = cmd.output().expect("spawn failed");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git spawn failed");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn tsk(dir: &Path, args: &[&str]) -> (i32, String, String) {
    run(Command::new(tsk_bin()).current_dir(dir).args(args))
}

fn tsk_ok(dir: &Path, args: &[&str]) -> String {
    let (code, stdout, stderr) = tsk(dir, args);
    assert_eq!(
        code, 0,
        "tsk {args:?} failed in {dir:?}: stdout={stdout} stderr={stderr}"
    );
    stdout
}

fn make_clone(origin: &Path, dest: &Path, name: &str, email: &str) {
    let _ = Command::new("git")
        .args(["clone", "-q", origin.to_str().unwrap(), dest.to_str().unwrap()])
        .status()
        .expect("git clone");
    git(dest, &["config", "user.name", name]);
    git(dest, &["config", "user.email", email]);
    // Configure tsk refspecs so plain `git push`/`git fetch` carries them too.
    tsk_ok(dest, &["git-setup"]);
}

fn setup_two_clones() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let origin = dir.path().join("origin.git");
    Command::new("git")
        .args(["init", "-q", "--bare", origin.to_str().unwrap()])
        .status()
        .unwrap();
    let alice = dir.path().join("alice");
    let bob = dir.path().join("bob");
    make_clone(&origin, &alice, "Alice", "a@x");
    make_clone(&origin, &bob, "Bob", "b@x");
    // Each clone needs at least one commit on the default branch before
    // tsk can push refs (origin must accept ref updates).
    std::fs::write(alice.join("README"), b"hi").unwrap();
    git(&alice, &["add", "README"]);
    git(&alice, &["commit", "-q", "-m", "init"]);
    git(&alice, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
    git(&bob, &["pull", "-q", "origin", "main"]);
    (dir, alice, bob)
}

#[test]
fn share_and_pull_between_clones() {
    let (_dir, alice, bob) = setup_two_clones();

    // Alice creates a task and pushes.
    tsk_ok(&alice, &["push", "Alice's task"]);
    tsk_ok(&alice, &["git-push"]);

    // Bob pulls and sees nothing in his stack (different namespace mapping
    // exists, but the queue index is shared).
    tsk_ok(&bob, &["git-pull"]);
    let listed = tsk_ok(&bob, &["list"]);
    // Bob hasn't bound a human id in his namespace, but he's on the same
    // default namespace `tsk`, and he pulled Alice's namespace state too —
    // so the task IS visible.
    assert!(
        listed.contains("Alice's task"),
        "shared default namespace + queue: bob should see alice's task. got: {listed:?}"
    );
}

#[test]
fn assign_to_other_queue_visible_after_push_pull() {
    let (_dir, alice, bob) = setup_two_clones();

    // Bob creates a "review" queue ahead of time.
    tsk_ok(&bob, &["queue", "create", "review"]);
    tsk_ok(&bob, &["git-push"]);

    // Alice pulls the queue, creates a task, assigns to review.
    tsk_ok(&alice, &["git-pull"]);
    tsk_ok(&alice, &["push", "needs review"]);
    let assign_out = tsk_ok(&alice, &["assign", "review", "-R", ""]);
    assert!(assign_out.contains("Assigned to review"), "got {assign_out}");
    tsk_ok(&alice, &["git-push"]);

    // Bob switches to review, pulls, sees inbox.
    tsk_ok(&bob, &["queue", "switch", "review"]);
    tsk_ok(&bob, &["git-pull"]);
    let inbox = tsk_ok(&bob, &["inbox", "-R", ""]);
    assert!(
        inbox.contains("needs review"),
        "bob should see assigned task in inbox: {inbox}"
    );
}

#[test]
fn concurrent_pushes_dont_clobber() {
    let (_dir, alice, bob) = setup_two_clones();

    // Both push a task concurrently before either has pulled.
    tsk_ok(&alice, &["push", "alice work"]);
    tsk_ok(&bob, &["push", "bob work"]);

    // Alice pushes first.
    tsk_ok(&alice, &["git-push"]);
    // Bob's push will be rejected by git's non-fast-forward protection on
    // the namespace ref (since alice already updated it). Verify it errors
    // and didn't silently overwrite.
    let (code, _, stderr) = tsk(&bob, &["git-push"]);
    assert_ne!(
        code, 0,
        "bob's push should fail (non-fast-forward); stderr={stderr}"
    );

    // After bob pulls, his local refs are overwritten with alice's state
    // (v1 has no merge driver for refs/tsk/queues/* — that's tracked for
    // a follow-up). The safety property we DO have is that the failed
    // push above didn't silently win.
    let (_, _, _) = tsk(&bob, &["git-pull"]);
    let listed = tsk_ok(&bob, &["list"]);
    assert!(
        listed.contains("alice work"),
        "after force-pull bob inherits alice's queue state: {listed}"
    );
}

#[test]
fn share_into_namespace_round_trip() {
    let (_dir, alice, _bob) = setup_two_clones();

    let push_out = tsk_ok(&alice, &["push", "to share"]);
    drop(push_out);
    tsk_ok(&alice, &["share", "alpha", "-r", "0"]);

    // Switch to alpha; the task should be visible there.
    tsk_ok(&alice, &["namespace", "switch", "alpha"]);
    // The shared task isn't on alpha's queue (queues are per-queue, not
    // per-namespace), but `tsk show -T tsk-1` should resolve the binding.
    let (code, stdout, stderr) = tsk(&alice, &["show", "-T", "tsk-1"]);
    assert_eq!(code, 0, "show should succeed: stderr={stderr}");
    assert!(stdout.contains("to share"), "got {stdout}");
}
