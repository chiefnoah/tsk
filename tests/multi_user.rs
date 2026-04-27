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

    // After bob pulls, the queue merge driver merges both sides so neither
    // task is lost. Namespace conflict (both allocated tsk-1) auto-renumbers
    // the local binding (tsk-12 + tsk-34).
    tsk_ok(&bob, &["git-pull"]);
    let listed = tsk_ok(&bob, &["list"]);
    assert!(
        listed.contains("alice work"),
        "alice's task must survive the merge: {listed}"
    );
    assert!(
        listed.contains("bob work"),
        "bob's task must survive the merge: {listed}"
    );
}

#[test]
fn property_set_find_round_trip_via_binary() {
    let (_dir, alice, _bob) = setup_two_clones();

    tsk_ok(&alice, &["push", "first"]);
    tsk_ok(&alice, &["push", "second"]);
    // Set priority on tsk-1 (the bottom of stack — first pushed).
    tsk_ok(&alice, &["prop", "add", "-T", "tsk-1", "priority", "high"]);
    tsk_ok(&alice, &["prop", "add", "-T", "tsk-1", "tag", "alpha"]);
    tsk_ok(&alice, &["prop", "add", "-T", "tsk-1", "tag", "beta"]);
    tsk_ok(&alice, &["prop", "add", "-T", "tsk-2", "priority", "low"]);

    // List on tsk-1: priority=high, tag=alpha, tag=beta.
    let list = tsk_ok(&alice, &["prop", "list", "-T", "tsk-1"]);
    assert!(list.contains("priority\thigh"), "got {list}");
    assert!(list.contains("tag\talpha"), "got {list}");
    assert!(list.contains("tag\tbeta"), "got {list}");

    // Keys index has both `priority` and `tag`.
    let keys = tsk_ok(&alice, &["prop", "keys"]);
    assert!(keys.contains("priority"), "got {keys}");
    assert!(keys.contains("tag"), "got {keys}");

    // Find tasks with priority=high.
    let found = tsk_ok(&alice, &["prop", "find", "priority", "high"]);
    assert!(found.contains("tsk-1"), "got {found}");
    assert!(!found.contains("tsk-2"), "got {found}");

    // Unsetting one value on multi-value property.
    tsk_ok(&alice, &["prop", "unset", "-T", "tsk-1", "tag", "alpha"]);
    let list = tsk_ok(&alice, &["prop", "list", "-T", "tsk-1"]);
    assert!(!list.contains("tag\talpha"), "alpha should be gone: {list}");
    assert!(list.contains("tag\tbeta"), "beta survives: {list}");

    // Replace whole property.
    tsk_ok(&alice, &["prop", "set", "-T", "tsk-1", "priority", "medium"]);
    let list = tsk_ok(&alice, &["prop", "list", "-T", "tsk-1"]);
    assert!(list.contains("priority\tmedium"), "got {list}");
    assert!(!list.contains("priority\thigh"), "got {list}");
}

#[test]
fn property_index_pushed_and_visible_to_other_clone() {
    let (_dir, alice, bob) = setup_two_clones();
    tsk_ok(&alice, &["push", "shared task"]);
    tsk_ok(&alice, &["prop", "add", "-T", "tsk-1", "owner", "alice"]);
    tsk_ok(&alice, &["git-push"]);

    tsk_ok(&bob, &["git-pull"]);
    let found = tsk_ok(&bob, &["prop", "find", "owner", "alice"]);
    assert!(found.contains("tsk-1"), "bob sees alice's index: {found}");
}

#[test]
fn show_renders_styled_body() {
    let (_dir, alice, _bob) = setup_two_clones();
    // Push a body that exercises every inline style.
    tsk_ok(
        &alice,
        &[
            "push",
            "rendered\n\nthis is !bold!, *italic*, _under_, ~struck~, =hi=, `code`.",
        ],
    );

    // Force colored output even in the test harness's pipe.
    let mut cmd = Command::new(tsk_bin());
    cmd.current_dir(&alice)
        .env("CLICOLOR_FORCE", "1")
        .args(["show", "-r", "0"]);
    let (code, stdout, stderr) = run(&mut cmd);
    assert_eq!(code, 0, "stderr={stderr}");

    // Markup characters must be stripped.
    assert!(!stdout.contains("!bold!"), "got {stdout:?}");
    assert!(!stdout.contains("*italic*"), "got {stdout:?}");
    assert!(!stdout.contains("=hi="), "got {stdout:?}");
    // ANSI bold escape must be present somewhere.
    assert!(
        stdout.contains("\x1b["),
        "expected ANSI escapes in styled output: {stdout:?}"
    );

    // -R bypasses the parser and prints the raw bytes.
    let raw = tsk_ok(&alice, &["show", "-r", "0", "-R"]);
    assert!(raw.contains("!bold!"), "raw must keep markup: {raw:?}");
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

#[test]
fn divergent_task_edits_merge_on_pull() {
    let (_dir, alice, bob) = setup_two_clones();

    // Alice creates a task, pushes; Bob pulls so they share the same
    // root commit on the task ref.
    tsk_ok(&alice, &["push", "shared task"]);
    tsk_ok(&alice, &["git-push"]);
    tsk_ok(&bob, &["git-pull"]);

    // Both edit the same task, touching different properties (no overlap).
    tsk_ok(&alice, &["prop", "add", "-T", "tsk-1", "priority", "high"]);
    tsk_ok(&bob, &["prop", "add", "-T", "tsk-1", "owner", "bob"]);

    // Alice pushes first; Bob's push would be non-fast-forward, so he pulls.
    tsk_ok(&alice, &["git-push"]);
    tsk_ok(&bob, &["git-pull"]);

    // After the merge pull, Bob should see both his and Alice's edits on
    // the task.
    let listing = tsk_ok(&bob, &["prop", "list", "-T", "tsk-1"]);
    eprintln!("LISTING: {listing}");
    assert!(listing.contains("priority\thigh"), "alice's edit lost: {listing}");
    assert!(listing.contains("owner\tbob"), "bob's edit lost: {listing}");
}

#[test]
fn divergent_task_edits_rebase_on_pull() {
    let (_dir, alice, bob) = setup_two_clones();

    tsk_ok(&alice, &["push", "shared task"]);
    tsk_ok(&alice, &["git-push"]);
    tsk_ok(&bob, &["git-pull"]);

    tsk_ok(&alice, &["prop", "add", "-T", "tsk-1", "priority", "high"]);
    tsk_ok(&bob, &["prop", "add", "-T", "tsk-1", "owner", "bob"]);

    tsk_ok(&alice, &["git-push"]);
    let pull_out = tsk_ok(&bob, &["git-pull", "--rebase"]);
    assert!(
        pull_out.contains("Rebased") || pull_out.is_empty(),
        "expected rebase summary, got {pull_out}"
    );

    // Both edits survived.
    let listing = tsk_ok(&bob, &["prop", "list", "-T", "tsk-1"]);
    assert!(listing.contains("priority\thigh"), "alice's edit lost: {listing}");
    assert!(listing.contains("owner\tbob"), "bob's edit lost: {listing}");
}

#[test]
fn assign_auto_push_only_targets_relevant_refs() {
    let (dir, alice, _bob) = setup_two_clones();
    let origin = dir.path().join("origin.git");

    // Alice creates a review queue and a task, neither pushed yet.
    tsk_ok(&alice, &["queue", "create", "review"]);
    tsk_ok(&alice, &["push", "task-to-assign"]);

    // Capture origin's refs before the auto-push.
    let before = git(&origin, &["for-each-ref", "refs/tsk/"]);
    assert!(
        !before.contains("refs/tsk/"),
        "origin should be empty: {before}"
    );

    // Assign auto-pushes to origin (the default).
    tsk_ok(&alice, &["assign", "review", "-r", "0"]);

    // Origin should have *only* the target queue, the task ref, and any
    // property indices referencing that task. The active queue (tsk) and
    // namespace must NOT have been pushed.
    let after = git(&origin, &["for-each-ref", "refs/tsk/"]);
    assert!(
        after.contains("refs/tsk/queues/review"),
        "review queue must be pushed: {after}"
    );
    let task_ref_present = after.lines().any(|l| l.contains("refs/tsk/tasks/"));
    assert!(task_ref_present, "task ref must be pushed: {after}");
    assert!(
        !after.contains("refs/tsk/queues/tsk"),
        "active queue must NOT be pushed: {after}"
    );
    assert!(
        !after.contains("refs/tsk/namespaces/"),
        "namespace must NOT be pushed: {after}"
    );
}

#[test]
fn namespace_collision_renumbers_local_on_pull() {
    let (_dir, alice, bob) = setup_two_clones();

    // Both clones independently allocate tsk-1 to different stable ids.
    tsk_ok(&alice, &["push", "alice's task"]);
    tsk_ok(&bob, &["push", "bob's task"]);

    // Alice pushes first: origin's namespace now binds tsk-1 → alice-stable.
    tsk_ok(&alice, &["git-push"]);

    // Bob pulls — his local namespace had tsk-1 → bob-stable, conflict with
    // origin's tsk-1 → alice-stable. Auto-renumber should move bob's binding
    // to a fresh id (tsk-2) and let alice's win tsk-1.
    let pull_out = tsk_ok(&bob, &["git-pull"]);
    assert!(
        pull_out.contains("tsk-1 → tsk-2") || pull_out.contains("tsk-1 \u{2192} tsk-2"),
        "expected renumber message in pull output, got: {pull_out}"
    );

    // tsk-1 on bob's side now resolves to alice's task.
    let show1 = tsk_ok(&bob, &["show", "-T", "tsk-1"]);
    assert!(
        show1.contains("alice's task"),
        "tsk-1 must point at alice's task after pull: {show1}"
    );
    // bob's original task is bound at tsk-2.
    let show2 = tsk_ok(&bob, &["show", "-T", "tsk-2"]);
    assert!(
        show2.contains("bob's task"),
        "tsk-2 must point at bob's task after renumber: {show2}"
    );
}
