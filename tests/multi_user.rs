//! End-to-end multi-clone tests. Spins up a bare "origin" repo and two
//! working clones; exercises the user-visible commands across them via the
//! compiled `tsk` binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
        .args([
            "clone",
            "-q",
            origin.to_str().unwrap(),
            dest.to_str().unwrap(),
        ])
        .status()
        .expect("git clone");
    git(dest, &["config", "user.name", name]);
    git(dest, &["config", "user.email", email]);
    // Configure tsk refspecs so plain `git push`/`git fetch` carries them too.
    tsk_ok(dest, &["git-setup"]);
}

fn init_repo_with_commit(dir: &Path) {
    Command::new("git")
        .args(["init", "-q", "-b", "main", "--object-format=sha1"])
        .current_dir(dir)
        .status()
        .expect("git init");
    git(dir, &["config", "user.name", "Test"]);
    git(dir, &["config", "user.email", "t@e"]);
    std::fs::write(dir.join("README"), b"hi").unwrap();
    git(dir, &["add", "README"]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

fn setup_two_clones() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let origin = dir.path().join("origin.git");
    Command::new("git")
        .args([
            "init",
            "-q",
            "--bare",
            "--object-format=sha1",
            origin.to_str().unwrap(),
        ])
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
fn cli_works_inside_linked_git_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main");
    let linked = dir.path().join("linked");
    std::fs::create_dir(&main).unwrap();
    init_repo_with_commit(&main);

    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ],
    );

    let git_file = std::fs::read_to_string(linked.join(".git")).unwrap();
    assert!(
        git_file.starts_with("gitdir: "),
        "linked worktree should use a .git pointer file, got {git_file:?}"
    );

    tsk_ok(&linked, &["push", "worktree task"]);
    let listed = tsk_ok(&linked, &["list"]);
    assert!(
        listed.contains("worktree task"),
        "task should be visible when tsk runs in a linked worktree: {listed:?}"
    );

    let gitdir = Path::new(git_file.trim().strip_prefix("gitdir: ").unwrap());
    let gitdir = if gitdir.is_absolute() {
        gitdir.to_path_buf()
    } else {
        linked.join(gitdir)
    };
    assert!(
        gitdir.join("tsk/namespace").exists(),
        "clone-local tsk state should live under the linked worktree gitdir"
    );
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
fn open_lists_open_tasks_with_queue_membership() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());

    tsk_ok(dir.path(), &["push", "queued open"]);
    tsk_ok(dir.path(), &["queue", "create", "review"]);
    tsk_ok(dir.path(), &["push", "unqueued open"]);
    tsk_ok(dir.path(), &["assign", "review", "-T", "tsk-2", "-R", ""]);
    tsk_ok(dir.path(), &["push", "closed task"]);
    tsk_ok(dir.path(), &["drop", "-T", "tsk-3"]);

    let open = tsk_ok(dir.path(), &["open"]);
    assert!(
        open.contains("tsk-1\ttsk\tqueued open"),
        "queued open task should report queue membership: {open:?}"
    );
    assert!(
        open.contains("tsk-2\tnone\tunqueued open"),
        "open task outside queue indexes should report none: {open:?}"
    );
    assert!(
        !open.contains("closed task"),
        "done tasks should be excluded from tsk open: {open:?}"
    );
}

#[test]
fn auto_sync_paths_skip_cleanly_without_git_remote() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());

    tsk_ok(dir.path(), &["queue", "create", "review"]);
    tsk_ok(dir.path(), &["push", "first assigned"]);

    let (code, assign_out, assign_err) = tsk(dir.path(), &["assign", "review"]);
    assert_eq!(code, 0, "assign should succeed: {assign_err}");
    assert!(
        assign_out.contains("Assigned to review"),
        "assign should report success: {assign_out}"
    );
    assert!(
        assign_err.is_empty(),
        "assign should not print git remote errors: {assign_err}"
    );

    tsk_ok(dir.path(), &["queue", "switch", "review"]);
    let (code, inbox_out, inbox_err) = tsk(dir.path(), &["inbox"]);
    assert_eq!(code, 0, "inbox should succeed: {inbox_err}");
    assert!(
        inbox_out.contains("first assigned"),
        "inbox should list local assignment: {inbox_out}"
    );
    assert!(
        inbox_err.is_empty(),
        "inbox should not print git remote errors: {inbox_err}"
    );

    let (code, accept_out, accept_err) = tsk(dir.path(), &["accept"]);
    assert_eq!(code, 0, "accept should succeed: {accept_err}");
    assert!(
        accept_out.contains("Accepted as"),
        "accept should report success: {accept_out}"
    );
    assert!(
        accept_err.is_empty(),
        "accept should not print git remote errors: {accept_err}"
    );

    tsk_ok(dir.path(), &["queue", "switch", "tsk"]);
    tsk_ok(dir.path(), &["push", "second assigned"]);
    tsk_ok(dir.path(), &["assign", "review"]);
    tsk_ok(dir.path(), &["queue", "switch", "review"]);
    let (code, reject_out, reject_err) = tsk(dir.path(), &["reject"]);
    assert_eq!(code, 0, "reject should succeed: {reject_err}");
    assert!(
        reject_out.contains("Rejected"),
        "reject should report success: {reject_out}"
    );
    assert!(
        reject_err.is_empty(),
        "reject should not print git remote errors: {reject_err}"
    );

    let (code, _stdout, stderr) = tsk(dir.path(), &["git-push"]);
    assert_ne!(code, 0, "git-push should still require an explicit remote");
    assert!(
        stderr.contains("no git remote configured"),
        "git-push should explain the missing remote: {stderr}"
    );
    assert!(
        !stderr.contains("fatal:"),
        "git-push should not fall through to git's fatal remote error: {stderr}"
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
    assert!(
        assign_out.contains("Assigned to review"),
        "got {assign_out}"
    );
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
fn queue_delete_removes_local_ref_and_active_selector() {
    let (_dir, alice, _bob) = setup_two_clones();

    tsk_ok(&alice, &["queue", "create", "review"]);
    tsk_ok(&alice, &["queue", "switch", "review"]);
    tsk_ok(&alice, &["queue", "delete", "review"]);

    let listed = tsk_ok(&alice, &["queue", "list"]);
    assert!(
        !listed.lines().any(|line| line == "review"),
        "deleted queue must be absent from queue list: {listed}"
    );
    let current = tsk_ok(&alice, &["queue", "current"]);
    assert_eq!(current.trim(), "tsk");
}

#[test]
fn queue_delete_can_be_pushed_and_pulled() {
    let (dir, alice, bob) = setup_two_clones();
    let origin = dir.path().join("origin.git");

    tsk_ok(&alice, &["queue", "create", "review"]);
    tsk_ok(&alice, &["git-push"]);
    tsk_ok(&bob, &["git-pull"]);
    let listed = tsk_ok(&bob, &["queue", "list"]);
    assert!(
        listed.lines().any(|line| line == "review"),
        "bob should see review queue before delete: {listed}"
    );

    tsk_ok(&alice, &["queue", "delete", "review", "-R", "origin"]);
    let origin_refs = git(&origin, &["for-each-ref", "refs/tsk/queues"]);
    assert!(
        !origin_refs.contains("refs/tsk/queues/review"),
        "origin queue ref should be deleted: {origin_refs}"
    );

    tsk_ok(&bob, &["queue", "switch", "review"]);
    let pull_out = tsk_ok(&bob, &["git-pull"]);
    assert!(
        pull_out.contains("deleted queue review"),
        "pull should report remote queue deletion: {pull_out}"
    );
    let listed = tsk_ok(&bob, &["queue", "list"]);
    assert!(
        !listed.lines().any(|line| line == "review"),
        "remote-deleted queue must be absent after pull: {listed}"
    );
    let current = tsk_ok(&bob, &["queue", "current"]);
    assert_eq!(current.trim(), "tsk");
}

#[test]
fn queue_remote_delete_wins_over_unpushed_local_queue_edits() {
    let (_dir, alice, bob) = setup_two_clones();

    tsk_ok(&alice, &["queue", "create", "review"]);
    tsk_ok(&alice, &["git-push"]);
    tsk_ok(&bob, &["git-pull"]);

    tsk_ok(&bob, &["queue", "switch", "review"]);
    tsk_ok(&bob, &["push", "bob local review work"]);
    let listed = tsk_ok(&bob, &["list"]);
    assert!(
        listed.contains("bob local review work"),
        "bob's local queue edit should exist before remote deletion: {listed}"
    );

    tsk_ok(&alice, &["queue", "delete", "review", "-R", "origin"]);
    let pull_out = tsk_ok(&bob, &["git-pull"]);
    assert!(
        pull_out.contains("deleted queue review"),
        "pull should report deletion even when bob edited the queue locally: {pull_out}"
    );

    let listed = tsk_ok(&bob, &["queue", "list"]);
    assert!(
        !listed.lines().any(|line| line == "review"),
        "remote-deleted queue must be absent after pull: {listed}"
    );
    let current = tsk_ok(&bob, &["queue", "current"]);
    assert_eq!(
        current.trim(),
        "tsk",
        "active selector should fall back after active queue is deleted"
    );
    let active_list = tsk_ok(&bob, &["list"]);
    assert!(
        !active_list.contains("bob local review work"),
        "work from the deleted queue must not leak into the fallback queue: {active_list}"
    );
}

#[test]
fn queue_pull_prunes_deleted_queue_shadow_ref() {
    let (_dir, alice, bob) = setup_two_clones();

    tsk_ok(&alice, &["queue", "create", "review"]);
    tsk_ok(&alice, &["git-push"]);
    tsk_ok(&bob, &["git-pull"]);

    let shadow_before = git(
        &bob,
        &["for-each-ref", "refs/tsk-fetched/origin/queues/review"],
    );
    assert!(
        shadow_before.contains("refs/tsk-fetched/origin/queues/review"),
        "bob should have a fetched shadow before deletion: {shadow_before}"
    );

    tsk_ok(&alice, &["queue", "delete", "review", "-R", "origin"]);
    let pull_out = tsk_ok(&bob, &["git-pull"]);
    assert!(
        pull_out.contains("deleted queue review"),
        "first pull should reconcile the remote deletion: {pull_out}"
    );

    let shadow_after = git(
        &bob,
        &["for-each-ref", "refs/tsk-fetched/origin/queues/review"],
    );
    assert!(
        shadow_after.trim().is_empty(),
        "fetch --prune should remove the stale queue shadow: {shadow_after}"
    );

    let second_pull = tsk_ok(&bob, &["git-pull"]);
    assert!(
        !second_pull.contains("deleted queue review"),
        "deleted queue must not be reported again after its shadow is pruned: {second_pull}"
    );
    let listed = tsk_ok(&bob, &["queue", "list"]);
    assert!(
        !listed.lines().any(|line| line == "review"),
        "deleted queue must stay absent after repeated pulls: {listed}"
    );
}

#[test]
fn queue_delete_refuses_default_queue_locally_and_remotely() {
    let (dir, alice, bob) = setup_two_clones();
    let origin = dir.path().join("origin.git");

    tsk_ok(&alice, &["push", "default queue task"]);
    tsk_ok(&alice, &["git-push"]);
    let origin_refs = git(&origin, &["for-each-ref", "refs/tsk/queues/tsk"]);
    assert!(
        origin_refs.contains("refs/tsk/queues/tsk"),
        "origin should have the default queue before delete is attempted: {origin_refs}"
    );

    let (code, stdout, stderr) = tsk(&alice, &["queue", "delete", "tsk", "-R", "origin"]);
    assert_ne!(
        code, 0,
        "deleting the default queue should fail: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stderr.contains("Refusing to delete default queue 'tsk'"),
        "failure should explain that the default queue is protected: {stderr}"
    );

    let local_queues = tsk_ok(&alice, &["queue", "list"]);
    assert!(
        local_queues.lines().any(|line| line == "tsk"),
        "default queue should remain locally after failed delete: {local_queues}"
    );
    let origin_refs = git(&origin, &["for-each-ref", "refs/tsk/queues/tsk"]);
    assert!(
        origin_refs.contains("refs/tsk/queues/tsk"),
        "default queue should remain on origin after failed remote delete: {origin_refs}"
    );

    tsk_ok(&bob, &["git-pull"]);
    let listed = tsk_ok(&bob, &["list"]);
    assert!(
        listed.contains("default queue task"),
        "other clones should still receive the default queue: {listed}"
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
fn namespace_renumber_rewrites_reference_properties() {
    let (_dir, alice, bob) = setup_two_clones();

    tsk_ok(&alice, &["push", "alice task"]);
    tsk_ok(&alice, &["git-push"]);

    tsk_ok(&bob, &["push", "bob target"]);
    tsk_ok(&bob, &["push", "bob source\n\nlinks to [[tsk-1]]"]);
    let (code, _, stderr) = tsk(&bob, &["git-push"]);
    assert_ne!(code, 0, "bob's push should require a pull; stderr={stderr}");

    tsk_ok(&bob, &["git-pull"]);

    let source = tsk_ok(&bob, &["show", "-T", "tsk-2", "-x"]);
    assert!(
        source.contains("links to tsk-3"),
        "source body should be rewritten to the renumbered target: {source}"
    );
    assert!(
        source.contains("references: \"[[tsk-3]]\""),
        "source references property should be rewritten: {source}"
    );
    let target = tsk_ok(&bob, &["show", "-T", "tsk-3", "-x"]);
    assert!(
        target.contains("referenced-by: \"[[tsk-2]]\""),
        "target backlink should still point at the source: {target}"
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

    // `prop list` lists values set on the task.
    let list = tsk_ok(&alice, &["prop", "list", "-T", "tsk-1"]);
    assert!(
        list.lines().any(|line| line == "priority\thigh"),
        "got {list}"
    );
    assert!(list.lines().any(|line| line == "tag\talpha"), "got {list}");
    assert!(list.lines().any(|line| line == "tag\tbeta"), "got {list}");
    assert!(
        !list.lines().any(|line| line == "priority" || line == "tag"),
        "prop list should print values, not key-only rows: {list}"
    );
    assert_eq!(
        tsk_ok(&alice, &["prop", "get", "-T", "tsk-1", "priority"]),
        "high\n"
    );
    assert_eq!(
        tsk_ok(&alice, &["prop", "get", "-T", "tsk-1", "tag"]),
        "alpha\nbeta\n"
    );
    let (code, stdout, stderr) = tsk(&alice, &["prop", "get", "-T", "tsk-1", "missing"]);
    assert_ne!(code, 0, "missing property should fail");
    assert!(
        stdout.is_empty(),
        "missing property should not print: {stdout}"
    );
    assert!(
        stderr.contains("has no property 'missing'"),
        "missing property error should name the key: {stderr}"
    );

    let attrs = tsk_ok(&alice, &["show", "-T", "tsk-1", "-x"]);
    assert!(attrs.contains("priority: \"high\""), "got {attrs}");
    assert!(attrs.contains("tag: \"alpha\""), "got {attrs}");
    assert!(attrs.contains("tag: \"beta\""), "got {attrs}");

    // `prop keys` is task-scoped.
    let keys = tsk_ok(&alice, &["prop", "keys", "-T", "tsk-1"]);
    assert!(keys.lines().any(|line| line == "priority"), "got {keys}");
    assert!(keys.lines().any(|line| line == "tag"), "got {keys}");
    assert!(
        !keys.contains('\t') && !keys.contains("high") && !keys.contains("alpha"),
        "prop keys should list keys only: {keys}"
    );
    let keys = tsk_ok(&alice, &["prop", "keys", "-T", "tsk-2"]);
    assert!(keys.lines().any(|line| line == "priority"), "got {keys}");
    assert!(
        !keys.lines().any(|line| line == "tag"),
        "prop keys should not include keys from other tasks: {keys}"
    );

    // Namespace props lists unique keys for tasks bound in that namespace.
    let props = tsk_ok(&alice, &["namespace", "props"]);
    assert!(props.lines().any(|line| line == "priority"), "got {props}");
    assert!(props.lines().any(|line| line == "tag"), "got {props}");
    assert_eq!(
        props.lines().filter(|line| *line == "priority").count(),
        1,
        "namespace props should deduplicate keys: {props}"
    );
    assert!(
        !props.contains("high") && !props.contains("low"),
        "namespace props should list keys only: {props}"
    );

    tsk_ok(&alice, &["namespace", "switch", "alpha"]);
    tsk_ok(&alice, &["push", "alpha task"]);
    tsk_ok(&alice, &["prop", "add", "-T", "tsk-1", "owner", "alice"]);
    let alpha_props = tsk_ok(&alice, &["namespace", "props"]);
    assert!(
        alpha_props.lines().any(|line| line == "owner"),
        "active namespace props should include alpha key: {alpha_props}"
    );
    assert!(
        !alpha_props.lines().any(|line| line == "priority"),
        "active namespace props should exclude default namespace keys: {alpha_props}"
    );
    let default_props = tsk_ok(&alice, &["namespace", "props", "tsk"]);
    assert!(
        default_props.lines().any(|line| line == "priority"),
        "explicit namespace props should include default namespace key: {default_props}"
    );
    assert!(
        !default_props.lines().any(|line| line == "owner"),
        "explicit namespace props should exclude alpha key: {default_props}"
    );
    tsk_ok(&alice, &["namespace", "switch", "tsk"]);

    // Find tasks with priority=high.
    let found = tsk_ok(&alice, &["prop", "find", "priority", "high"]);
    assert!(found.contains("tsk-1"), "got {found}");
    assert!(!found.contains("tsk-2"), "got {found}");

    // Unsetting one value on multi-value property.
    tsk_ok(&alice, &["prop", "unset", "-T", "tsk-1", "tag", "alpha"]);
    let attrs = tsk_ok(&alice, &["show", "-T", "tsk-1", "-x"]);
    assert!(
        !attrs.contains("tag: \"alpha\""),
        "alpha should be gone: {attrs}"
    );
    assert!(attrs.contains("tag: \"beta\""), "beta survives: {attrs}");

    // Replace whole property.
    tsk_ok(
        &alice,
        &["prop", "set", "-T", "tsk-1", "priority", "medium"],
    );
    let attrs = tsk_ok(&alice, &["show", "-T", "tsk-1", "-x"]);
    assert!(attrs.contains("priority: \"medium\""), "got {attrs}");
    assert!(!attrs.contains("priority: \"high\""), "got {attrs}");
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
fn tabular_commands_can_print_headers() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());
    tsk_ok(dir.path(), &["push", "first\nbody line one\nbody line two"]);
    tsk_ok(
        dir.path(),
        &["prop", "add", "-T", "tsk-1", "owner", "alice"],
    );

    let plain_list = tsk_ok(dir.path(), &["list"]);
    assert!(
        plain_list.starts_with("tsk-1\tfirst"),
        "default list output should not grow a header: {plain_list}"
    );
    let list = tsk_ok(dir.path(), &["--headers", "list"]);
    assert!(
        list.starts_with("id\ttitle\ntsk-1\tfirst"),
        "list should include a header row: {list}"
    );
    let ids = tsk_ok(dir.path(), &["--headers", "list", "-i"]);
    assert_eq!(ids, "id\ntsk-1\n");

    let open = tsk_ok(dir.path(), &["--headers", "open"]);
    assert!(
        open.starts_with("id\tqueues\ttitle\ntsk-1\ttsk\tfirst"),
        "open should include column headers: {open}"
    );
    let prop_list = tsk_ok(dir.path(), &["--headers", "prop", "list", "-T", "tsk-1"]);
    assert!(
        prop_list.starts_with("key\tvalue\n"),
        "prop list should include column headers: {prop_list}"
    );
    let prop_find = tsk_ok(dir.path(), &["--headers", "prop", "find", "owner", "alice"]);
    assert!(
        prop_find.starts_with("id\ttitle\ntsk-1\tfirst"),
        "prop find should include column headers: {prop_find}"
    );
    let namespace_list = tsk_ok(dir.path(), &["--headers", "namespace", "list"]);
    assert!(
        namespace_list.starts_with("namespace\ntsk"),
        "namespace list should include a header row: {namespace_list}"
    );
    let namespace_tasks = tsk_ok(dir.path(), &["--headers", "namespace", "tasks"]);
    assert!(
        namespace_tasks.starts_with("id\ttitle\ntsk-1\tfirst"),
        "namespace tasks should include column headers: {namespace_tasks}"
    );
    let namespace_tasks_body = tsk_ok(dir.path(), &["--headers", "namespace", "tasks", "--body"]);
    assert_eq!(
        namespace_tasks_body,
        "tsk-1 first\n\n    body line one\n    body line two\n"
    );
    let namespace_props = tsk_ok(dir.path(), &["--headers", "namespace", "props"]);
    assert!(
        namespace_props.starts_with("key\nowner"),
        "namespace props should include a header row: {namespace_props}"
    );
    let queue_list = tsk_ok(dir.path(), &["--headers", "queue", "list"]);
    assert!(
        queue_list.starts_with("queue\ntsk"),
        "queue list should include a header row: {queue_list}"
    );
}

#[test]
fn namespace_tasks_body_reads_the_requested_namespace() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());

    tsk_ok(dir.path(), &["namespace", "switch", "alpha"]);
    tsk_ok(dir.path(), &["push", "alpha task\nalpha body"]);
    tsk_ok(dir.path(), &["namespace", "switch", "tsk"]);
    tsk_ok(dir.path(), &["push", "tsk task\ntsk body"]);

    let listed = tsk_ok(dir.path(), &["namespace", "tasks", "--body", "alpha"]);
    assert_eq!(listed, "tsk-1 alpha task\n\n    alpha body\n");
}

#[test]
fn namespace_current_and_list_can_print_ref_tips() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());
    tsk_ok(dir.path(), &["push", "first"]);

    let expected = git(dir.path(), &["rev-parse", "refs/tsk/namespaces/tsk"])
        .trim()
        .to_string();
    let current = tsk_ok(dir.path(), &["namespace", "current", "--head"]);
    assert_eq!(current.trim(), expected);

    let list = tsk_ok(dir.path(), &["namespace", "list", "--head"]);
    assert!(
        list.lines().any(|line| line == format!("tsk\t{expected}")),
        "namespace list --head should include namespace tip: {list}"
    );
}

#[test]
fn drop_can_record_current_head_commit() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());
    let head = git(dir.path(), &["rev-parse", "HEAD"]).trim().to_string();

    tsk_ok(dir.path(), &["push", "without closed-on"]);
    tsk_ok(dir.path(), &["drop", "-T", "tsk-1"]);
    let attrs = tsk_ok(dir.path(), &["show", "-T", "tsk-1", "-x"]);
    assert!(attrs.contains("status: \"done\""), "got {attrs}");
    assert!(
        !attrs.contains("closed-on:"),
        "drop should not record HEAD unless requested: {attrs}"
    );

    tsk_ok(dir.path(), &["push", "with closed-on"]);
    tsk_ok(dir.path(), &["drop", "--closed-on-commit", "-T", "tsk-2"]);
    let attrs = tsk_ok(dir.path(), &["show", "-T", "tsk-2", "-x"]);
    assert!(attrs.contains("status: \"done\""), "got {attrs}");
    assert!(
        attrs.contains(&format!("closed-on: \"{head}\"")),
        "drop --closed-on-commit should record current HEAD {head}: {attrs}"
    );

    tsk_ok(dir.path(), &["push", "with closed-on shortcut"]);
    tsk_ok(dir.path(), &["drop", "-x", "-T", "tsk-3"]);
    let attrs = tsk_ok(dir.path(), &["show", "-T", "tsk-3", "-x"]);
    assert!(
        attrs.contains(&format!("closed-on: \"{head}\"")),
        "drop -x should record current HEAD {head}: {attrs}"
    );

    tsk_ok(dir.path(), &["push", "with legacy closed-on flag"]);
    tsk_ok(dir.path(), &["drop", "--closed-on-head", "-T", "tsk-4"]);
    let attrs = tsk_ok(dir.path(), &["show", "-T", "tsk-4", "-x"]);
    assert!(
        attrs.contains(&format!("closed-on: \"{head}\"")),
        "drop --closed-on-head should remain supported: {attrs}"
    );
}

#[test]
fn reopen_without_id_uses_fzf_search_with_body_option() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());
    tsk_ok(
        dir.path(),
        &["push", "open queued title\n\nopen body should not show"],
    );
    tsk_ok(
        dir.path(),
        &["push", "searchable title\n\nbody needle for reopen"],
    );
    tsk_ok(dir.path(), &["drop", "-T", "tsk-2"]);
    tsk_ok(dir.path(), &["queue", "create", "review"]);
    tsk_ok(
        dir.path(),
        &["push", "inboxed title\n\ninbox body should not show"],
    );
    tsk_ok(dir.path(), &["assign", "review", "-T", "tsk-3", "-R", ""]);

    let fake_bin = tempfile::tempdir().unwrap();
    let capture = fake_bin.path().join("fzf-input");
    let fzf = fake_bin.path().join("fzf");
    std::fs::write(
        &fzf,
        "#!/bin/sh\ntr '\\000' '\\n' > \"$FZF_CAPTURE\"\ngrep '^tsk-2\t' \"$FZF_CAPTURE\" | head -n 1 | tr '\\n' '\\000'\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&fzf).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&fzf, perms).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(fake_bin.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
    )
    .unwrap();

    let mut cmd = Command::new(tsk_bin());
    cmd.current_dir(dir.path())
        .env("PATH", path)
        .env("FZF_CAPTURE", &capture)
        .env("TSK_TEST_ALLOW_FZF", "1")
        .args(["reopen", "-b"]);
    let (code, stdout, stderr) = run(&mut cmd);
    assert_eq!(
        code, 0,
        "reopen should succeed: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("Reopened tsk-2"),
        "reopen should print reopened id: {stdout}"
    );
    let input = std::fs::read_to_string(capture).unwrap();
    assert!(
        input.contains("body needle for reopen"),
        "reopen -b should include task body in fzf input: {input}"
    );
    assert!(
        input.contains("open body should not show"),
        "reopen picker should include namespace tasks: {input}"
    );
    assert!(
        input.contains("inbox body should not show"),
        "reopen picker should include inboxed namespace tasks: {input}"
    );
    let attrs = tsk_ok(dir.path(), &["show", "-T", "tsk-2", "-x"]);
    assert!(attrs.contains("status: \"open\""), "got {attrs}");
    let list = tsk_ok(dir.path(), &["list"]);
    assert!(
        list.contains("tsk-2"),
        "reopened task should be queued: {list}"
    );
}

#[test]
fn edit_without_id_uses_top_task() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());
    tsk_ok(dir.path(), &["push", "top task\n\nold body"]);
    tsk_ok(dir.path(), &["append", "selected task\n\nold body"]);

    tsk_ok(dir.path(), &["edit", "-b", "new body"]);
    let top = tsk_ok(dir.path(), &["show", "-T", "tsk-1", "-R"]);
    assert!(top.contains("new body"), "top task should be edited: {top}");
    let selected = tsk_ok(dir.path(), &["show", "-T", "tsk-2", "-R"]);
    assert!(
        selected.contains("old body"),
        "non-top task should not be edited: {selected}"
    );
}

#[test]
fn task_id_f_flag_fuzzy_finds_by_title_without_body() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());
    tsk_ok(dir.path(), &["push", "top task\n\nbody needle"]);
    tsk_ok(dir.path(), &["append", "selected task\n\nselected body"]);

    let fake_bin = tempfile::tempdir().unwrap();
    let capture = fake_bin.path().join("fzf-input");
    let fzf = fake_bin.path().join("fzf");
    std::fs::write(
        &fzf,
        "#!/bin/sh\ntr '\\000' '\\n' > \"$FZF_CAPTURE\"\ngrep '^tsk-2\t' \"$FZF_CAPTURE\" | head -n 1 | tr '\\n' '\\000'\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&fzf).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&fzf, perms).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(fake_bin.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
    )
    .unwrap();

    let mut cmd = Command::new(tsk_bin());
    cmd.current_dir(dir.path())
        .env("PATH", path)
        .env("FZF_CAPTURE", &capture)
        .env("TSK_TEST_ALLOW_FZF", "1")
        .args(["show", "-f", "-R"]);
    let (code, stdout, stderr) = run(&mut cmd);
    assert_eq!(
        code, 0,
        "show -f should succeed: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("selected task"),
        "selected task should be shown: {stdout}"
    );

    let input = std::fs::read_to_string(capture).unwrap();
    assert!(
        input.contains("tsk-1\ttop task") && input.contains("tsk-2\tselected task"),
        "fuzzy picker should include task titles: {input}"
    );
    assert!(
        !input.contains("body needle") && !input.contains("selected body"),
        "-f picker input should not include bodies: {input}"
    );
}

#[test]
fn task_id_capital_f_flag_fuzzy_finds_with_body() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());
    tsk_ok(dir.path(), &["push", "top task\n\nold body"]);
    tsk_ok(dir.path(), &["append", "selected task\n\nbody needle"]);

    let fake_bin = tempfile::tempdir().unwrap();
    let capture = fake_bin.path().join("fzf-input");
    let fzf = fake_bin.path().join("fzf");
    std::fs::write(
        &fzf,
        "#!/bin/sh\ntr '\\000' '\\n' > \"$FZF_CAPTURE\"\ngrep 'body needle' \"$FZF_CAPTURE\" | head -n 1 | tr '\\n' '\\000'\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&fzf).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&fzf, perms).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(fake_bin.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
    )
    .unwrap();

    let mut cmd = Command::new(tsk_bin());
    cmd.current_dir(dir.path())
        .env("PATH", path)
        .env("FZF_CAPTURE", &capture)
        .env("TSK_TEST_ALLOW_FZF", "1")
        .args(["edit", "-F", "-b", "new body"]);
    let (code, stdout, stderr) = run(&mut cmd);
    assert_eq!(
        code, 0,
        "edit -F should succeed: stdout={stdout} stderr={stderr}"
    );

    let input = std::fs::read_to_string(capture).unwrap();
    assert!(
        input.contains("body needle"),
        "-F picker input should include bodies: {input}"
    );
    let top = tsk_ok(dir.path(), &["show", "-T", "tsk-1", "-R"]);
    assert!(
        top.contains("old body"),
        "top task should not be edited: {top}"
    );
    let selected = tsk_ok(dir.path(), &["show", "-T", "tsk-2", "-R"]);
    assert!(
        selected.contains("new body"),
        "body-selected task should be edited: {selected}"
    );
}

#[test]
fn abandon_removes_from_queue_without_changing_status() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());

    tsk_ok(dir.path(), &["push", "parked open task"]);
    let out = tsk_ok(dir.path(), &["abandon", "-T", "tsk-1"]);
    assert!(out.contains("Abandoned tsk-1"), "got {out}");

    let attrs = tsk_ok(dir.path(), &["show", "-T", "tsk-1", "-x"]);
    assert!(attrs.contains("status: \"open\""), "got {attrs}");
    let list = tsk_ok(dir.path(), &["list"]);
    assert!(
        !list.contains("tsk-1"),
        "abandoned task should leave the active queue: {list}"
    );
    let open = tsk_ok(dir.path(), &["open"]);
    assert!(
        open.contains("tsk-1\tnone\tparked open task"),
        "abandoned open task should remain open with no queue: {open}"
    );
}

#[test]
fn accept_task_id_assigns_unqueued_task_to_active_queue() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());

    tsk_ok(dir.path(), &["push", "ready later"]);
    tsk_ok(dir.path(), &["abandon", "-T", "tsk-1"]);

    let out = tsk_ok(dir.path(), &["accept", "-T", "tsk-1", "-R", ""]);
    assert!(out.contains("Accepted tsk-1"), "got {out}");
    let list = tsk_ok(dir.path(), &["list"]);
    assert!(
        list.starts_with("tsk-1\tready later"),
        "accepted unqueued task should be on top of active queue: {list}"
    );
    let open = tsk_ok(dir.path(), &["open"]);
    assert!(
        open.contains("tsk-1\ttsk\tready later"),
        "accepted task should report active queue membership: {open}"
    );
}

#[test]
fn bare_accept_takes_top_inbox_item() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());

    tsk_ok(dir.path(), &["queue", "create", "review"]);
    tsk_ok(dir.path(), &["push", "first assigned"]);
    tsk_ok(dir.path(), &["assign", "review", "-T", "tsk-1", "-R", ""]);
    tsk_ok(dir.path(), &["push", "second assigned"]);
    tsk_ok(dir.path(), &["assign", "review", "-T", "tsk-2", "-R", ""]);
    tsk_ok(dir.path(), &["queue", "switch", "review"]);

    let inbox = tsk_ok(dir.path(), &["inbox", "-R", ""]);
    assert!(
        inbox.starts_with("tsk-2\tfrom tsk\tsecond assigned"),
        "newest inbox item should be top: {inbox}"
    );
    let out = tsk_ok(dir.path(), &["accept", "-R", ""]);
    assert!(out.contains("Accepted as tsk-2"), "got {out}");
    let list = tsk_ok(dir.path(), &["list"]);
    assert!(
        list.starts_with("tsk-2\tsecond assigned"),
        "bare accept should take the top inbox item: {list}"
    );
}

#[test]
fn reopen_can_skip_or_assign_active_queue() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());

    tsk_ok(dir.path(), &["push", "reopen target"]);
    tsk_ok(dir.path(), &["drop", "-T", "tsk-1"]);

    tsk_ok(dir.path(), &["reopen", "--no-queue", "-T", "tsk-1"]);
    let attrs = tsk_ok(dir.path(), &["show", "-T", "tsk-1", "-x"]);
    assert!(attrs.contains("status: \"open\""), "got {attrs}");
    let list = tsk_ok(dir.path(), &["list"]);
    assert!(
        !list.contains("tsk-1"),
        "reopen --no-queue should leave the task out of the queue: {list}"
    );

    tsk_ok(dir.path(), &["reopen", "-T", "tsk-1"]);
    let list = tsk_ok(dir.path(), &["list"]);
    assert!(
        list.starts_with("tsk-1\treopen target"),
        "default reopen should queue the task at the top: {list}"
    );
}

#[test]
fn queue_mutations_require_active_queue_membership() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());

    tsk_ok(dir.path(), &["queue", "create", "review"]);
    tsk_ok(dir.path(), &["queue", "create", "later"]);
    tsk_ok(dir.path(), &["push", "assigned away"]);
    tsk_ok(dir.path(), &["assign", "review", "-T", "tsk-1", "-R", ""]);

    for args in [
        &["prioritize", "-T", "tsk-1"][..],
        &["deprioritize", "-T", "tsk-1"][..],
        &["assign", "later", "-T", "tsk-1", "-R", ""][..],
        &["drop", "-T", "tsk-1"][..],
    ] {
        let (code, _stdout, stderr) = tsk(dir.path(), args);
        assert_ne!(code, 0, "tsk {args:?} should fail for nonqueued task");
        assert!(
            stderr.contains("not on active queue"),
            "failure should mention active queue membership: {stderr}"
        );
    }

    tsk_ok(dir.path(), &["push", "already dropped"]);
    tsk_ok(dir.path(), &["drop", "-T", "tsk-2"]);
    let (code, _stdout, stderr) = tsk(dir.path(), &["prioritize", "-T", "tsk-2"]);
    assert_ne!(code, 0, "prioritize should reject a dropped task");
    assert!(
        stderr.contains("not on active queue"),
        "failure should mention active queue membership: {stderr}"
    );
}

#[test]
fn prioritize_without_id_fuzzy_finds_active_queue() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());
    tsk_ok(dir.path(), &["push", "top task"]);
    tsk_ok(dir.path(), &["append", "selected task"]);

    let fake_bin = tempfile::tempdir().unwrap();
    let capture = fake_bin.path().join("fzf-input");
    let fzf = fake_bin.path().join("fzf");
    std::fs::write(
        &fzf,
        "#!/bin/sh\ntr '\\000' '\\n' > \"$FZF_CAPTURE\"\ngrep '^tsk-2\t' \"$FZF_CAPTURE\" | head -n 1 | tr '\\n' '\\000'\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&fzf).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&fzf, perms).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(fake_bin.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
    )
    .unwrap();

    let mut cmd = Command::new(tsk_bin());
    cmd.current_dir(dir.path())
        .env("PATH", path)
        .env("FZF_CAPTURE", &capture)
        .env("TSK_TEST_ALLOW_FZF", "1")
        .args(["prioritize"]);
    let (code, stdout, stderr) = run(&mut cmd);
    assert_eq!(
        code, 0,
        "prioritize should succeed: stdout={stdout} stderr={stderr}"
    );

    let input = std::fs::read_to_string(capture).unwrap();
    assert!(
        input.contains("tsk-1\ttop task") && input.contains("tsk-2\tselected task"),
        "prioritize picker should include active queue tasks: {input}"
    );
    let list = tsk_ok(dir.path(), &["list"]);
    assert!(
        list.starts_with("tsk-2\tselected task"),
        "selected task should be prioritized: {list}"
    );
}

#[test]
fn queue_set_can_pull_changes_pull_permission() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());

    tsk_ok(dir.path(), &["queue", "create", "private"]);
    tsk_ok(dir.path(), &["queue", "switch", "private"]);
    tsk_ok(dir.path(), &["push", "private task"]);
    tsk_ok(dir.path(), &["queue", "switch", "tsk"]);

    let (code, _stdout, stderr) = tsk(dir.path(), &["pull", "private", "-T", "tsk-1"]);
    assert_ne!(code, 0, "pull from can-pull=false queue should fail");
    assert!(
        stderr.contains("can-pull=false"),
        "failure should mention can-pull=false: {stderr}"
    );

    let output = tsk_ok(dir.path(), &["queue", "can-pull", "private", "true"]);
    assert!(
        output.contains("Set queue 'private' can-pull=true"),
        "can-pull should report updated value: {output}"
    );
    let pulled = tsk_ok(dir.path(), &["pull", "private", "-T", "tsk-1"]);
    assert!(
        pulled.contains("Pulled tsk-1"),
        "pull should succeed after enabling can-pull: {pulled}"
    );

    tsk_ok(dir.path(), &["queue", "can-pull", "private", "false"]);
    tsk_ok(dir.path(), &["queue", "switch", "private"]);
    tsk_ok(dir.path(), &["push", "second private task"]);
    tsk_ok(dir.path(), &["queue", "switch", "tsk"]);
    let (code, _stdout, stderr) = tsk(dir.path(), &["pull", "private", "-T", "tsk-2"]);
    assert_ne!(code, 0, "pull should fail again after disabling can-pull");
    assert!(
        stderr.contains("can-pull=false"),
        "failure should mention can-pull=false: {stderr}"
    );
}

#[test]
fn pull_without_id_fuzzy_finds_source_queue() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());
    tsk_ok(dir.path(), &["queue", "create", "private", "-p"]);
    tsk_ok(dir.path(), &["queue", "switch", "private"]);
    tsk_ok(dir.path(), &["push", "private top"]);
    tsk_ok(dir.path(), &["append", "private selected"]);
    tsk_ok(dir.path(), &["queue", "switch", "tsk"]);

    let fake_bin = tempfile::tempdir().unwrap();
    let capture = fake_bin.path().join("fzf-input");
    let fzf = fake_bin.path().join("fzf");
    std::fs::write(
        &fzf,
        "#!/bin/sh\ntr '\\000' '\\n' > \"$FZF_CAPTURE\"\ngrep '^tsk-2\t' \"$FZF_CAPTURE\" | head -n 1 | tr '\\n' '\\000'\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&fzf).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&fzf, perms).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(fake_bin.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
    )
    .unwrap();

    let mut cmd = Command::new(tsk_bin());
    cmd.current_dir(dir.path())
        .env("PATH", path)
        .env("FZF_CAPTURE", &capture)
        .env("TSK_TEST_ALLOW_FZF", "1")
        .args(["pull", "private"]);
    let (code, stdout, stderr) = run(&mut cmd);
    assert_eq!(
        code, 0,
        "pull should succeed: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("Pulled tsk-2"),
        "pull should report selected task: {stdout}"
    );

    let input = std::fs::read_to_string(capture).unwrap();
    assert!(
        input.contains("tsk-1\tprivate top") && input.contains("tsk-2\tprivate selected"),
        "pull picker should include source queue tasks: {input}"
    );
    let list = tsk_ok(dir.path(), &["list"]);
    assert!(
        list.starts_with("tsk-2\tprivate selected"),
        "pulled task should be queued: {list}"
    );
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
fn show_stable_id_prints_stable_id_only() {
    let (_dir, alice, _bob) = setup_two_clones();
    tsk_ok(&alice, &["push", "stable title\n\nstable body"]);

    let stable = tsk_ok(&alice, &["show", "-T", "tsk-1", "--stable-id"]);
    let stable = stable.trim();
    assert_eq!(stable.len(), 40, "stable id should be full hex: {stable}");
    assert!(
        stable.chars().all(|c| c.is_ascii_hexdigit()),
        "stable id should be hex: {stable}"
    );
    assert!(
        !stable.contains("stable title") && !stable.contains("stable body"),
        "show --stable-id should not render task content: {stable}"
    );

    git(
        &alice,
        &["show-ref", "--verify", &format!("refs/tsk/tasks/{stable}")],
    );

    let with_attrs = tsk_ok(&alice, &["show", "-T", "tsk-1", "-x", "--stable-id"]);
    assert_eq!(
        with_attrs.trim(),
        stable,
        "show --stable-id should print only the stable id even when -x is passed"
    );
}

#[test]
fn show_latest_commit_prints_task_ref_tip_only() {
    let (_dir, alice, _bob) = setup_two_clones();
    tsk_ok(&alice, &["push", "commit title\n\ncommit body"]);
    tsk_ok(&alice, &["prop", "add", "-T", "tsk-1", "priority", "high"]);

    let stable = tsk_ok(&alice, &["show", "-T", "tsk-1", "--stable-id"]);
    let expected = git(
        &alice,
        &["rev-parse", &format!("refs/tsk/tasks/{}", stable.trim())],
    );

    let commit = tsk_ok(&alice, &["show", "-T", "tsk-1", "--latest-commit"]);
    assert_eq!(
        commit.trim(),
        expected.trim(),
        "show --latest-commit should print the task ref tip"
    );
    assert_eq!(
        commit.trim().len(),
        40,
        "commit should be full hex: {commit}"
    );
    assert!(
        commit.trim().chars().all(|c| c.is_ascii_hexdigit()),
        "commit should be hex: {commit}"
    );
    assert!(
        !commit.contains("commit title") && !commit.contains("commit body"),
        "show --latest-commit should not render task content: {commit}"
    );

    let with_attrs = tsk_ok(&alice, &["show", "-T", "tsk-1", "-x", "-c"]);
    assert_eq!(
        with_attrs.trim(),
        expected.trim(),
        "show -c should print only the commit even when -x is passed"
    );
}

#[test]
fn edit_reports_new_blocking_tasks_on_stderr() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());
    tsk_ok(dir.path(), &["push", "parent"]);

    let editor_dir = tempfile::tempdir().unwrap();
    let editor = editor_dir.path().join("editor");
    std::fs::write(
        &editor,
        "#!/bin/sh\ncat > \"$1\" <<'EOF'\nparent\n\nneeds [> prereq from editor <]\nEOF\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&editor).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&editor, perms).unwrap();

    let mut cmd = Command::new(tsk_bin());
    cmd.current_dir(dir.path())
        .env("EDITOR", &editor)
        .env_remove("VISUAL")
        .args(["edit", "-T", "tsk-1"]);
    let (code, stdout, stderr) = run(&mut cmd);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(
        stderr.contains("Created blocking task [[tsk-2]]\tprereq from editor"),
        "edit should report the newly created dependency on stderr: {stderr}"
    );

    let parent = tsk_ok(dir.path(), &["show", "-T", "tsk-1", "-R"]);
    assert!(
        parent.contains("needs [[tsk-2]]"),
        "placeholder should be replaced in edited task: {parent}"
    );
    let child = tsk_ok(dir.path(), &["show", "-T", "tsk-2", "-R"]);
    assert!(
        child.contains("prereq from editor"),
        "created dependency should be addressable: {child}"
    );
}

#[test]
fn assign_without_target_uses_fzf_queue_picker() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());
    tsk_ok(dir.path(), &["queue", "create", "review"]);
    tsk_ok(dir.path(), &["push", "assign me"]);

    let fake_bin = tempfile::tempdir().unwrap();
    let capture = fake_bin.path().join("fzf-input");
    let fzf = fake_bin.path().join("fzf");
    std::fs::write(
        &fzf,
        "#!/bin/sh\ntr '\\000' '\\n' > \"$FZF_CAPTURE\"\nprintf 'review\\n'\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&fzf).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&fzf, perms).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(fake_bin.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
    )
    .unwrap();

    let mut cmd = Command::new(tsk_bin());
    cmd.current_dir(dir.path())
        .env("PATH", path)
        .env("FZF_CAPTURE", &capture)
        .env("TSK_TEST_ALLOW_FZF", "1")
        .args(["assign", "-T", "tsk-1", "-R", ""]);
    let (code, stdout, stderr) = run(&mut cmd);
    assert_eq!(
        code, 0,
        "assign should succeed: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("Assigned to review"),
        "assign should print picked queue: {stdout}"
    );

    let input = std::fs::read_to_string(capture).unwrap();
    assert!(
        input.lines().any(|line| line == "review"),
        "assign picker should include review queue: {input}"
    );
    assert!(
        !input.lines().any(|line| line == "tsk"),
        "assign picker should exclude current queue: {input}"
    );
    let review = tsk_ok(dir.path(), &["--queue", "review", "inbox", "-R", ""]);
    assert!(
        review.contains("tsk-1\tfrom tsk\tassign me"),
        "review inbox should receive assigned task: {review}"
    );
}

#[test]
fn edit_body_flag_replaces_body_without_editor() {
    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());
    tsk_ok(dir.path(), &["push", "original title\n\noriginal body"]);

    tsk_ok(
        dir.path(),
        &["edit", "-T", "tsk-1", "-b", "replacement body"],
    );

    let edited = tsk_ok(dir.path(), &["show", "-T", "tsk-1", "-R"]);
    assert!(
        edited.starts_with("original title\n\n"),
        "edit -b should preserve the title: {edited}"
    );
    assert!(
        edited.contains("replacement body"),
        "edit -b should replace the body: {edited}"
    );
    assert!(
        !edited.contains("original body"),
        "edit -b should remove the old body: {edited}"
    );
}

#[test]
fn edit_body_flag_can_read_stdin_and_expand_dependencies() {
    use std::io::Write as _;

    let dir = tempfile::tempdir().unwrap();
    init_repo_with_commit(dir.path());
    tsk_ok(dir.path(), &["push", "parent"]);

    let mut cmd = Command::new(tsk_bin());
    cmd.current_dir(dir.path())
        .args(["edit", "-T", "tsk-1", "-b", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn tsk edit");
    child
        .stdin
        .as_mut()
        .expect("stdin should be piped")
        .write_all(b"needs [> prereq from stdin <]\n")
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait for tsk edit");
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(
        stderr.contains("Created blocking task [[tsk-2]]\tprereq from stdin"),
        "edit -b - should report created dependencies: {stderr}"
    );

    let parent = tsk_ok(dir.path(), &["show", "-T", "tsk-1", "-R"]);
    assert!(
        parent.contains("needs [[tsk-2]]"),
        "placeholder should be replaced in edited body: {parent}"
    );
    let child = tsk_ok(dir.path(), &["show", "-T", "tsk-2", "-R"]);
    assert!(
        child.contains("prereq from stdin"),
        "created dependency should be addressable: {child}"
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
    let listing = tsk_ok(&bob, &["show", "-T", "tsk-1", "-x"]);
    assert!(
        listing.contains("priority: \"high\""),
        "alice's edit lost: {listing}"
    );
    assert!(
        listing.contains("owner: \"bob\""),
        "bob's edit lost: {listing}"
    );
}

#[test]
fn body_conflict_on_pull_opens_editor_and_commits_result() {
    use std::os::unix::fs::PermissionsExt;

    let (_dir, alice, bob) = setup_two_clones();

    tsk_ok(&alice, &["push", "link target"]);
    tsk_ok(&alice, &["push", "shared task\n\ninitial body"]);
    tsk_ok(&alice, &["git-push"]);
    tsk_ok(&bob, &["git-pull"]);

    tsk_ok(&alice, &["edit", "-T", "tsk-2", "-b", "alice body"]);
    tsk_ok(&bob, &["edit", "-T", "tsk-2", "-b", "bob body"]);
    tsk_ok(&alice, &["git-push"]);

    let editor_dir = tempfile::tempdir().unwrap();
    let editor = editor_dir.path().join("resolve-conflict");
    std::fs::write(
        &editor,
        "#!/bin/sh\nfor arg do path=$arg; done\nprintf 'resolved title\\n\\nresolved body links [[tsk-1]]\\n' > \"$path\"\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&editor).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&editor, perms).unwrap();

    let mut cmd = Command::new(tsk_bin());
    cmd.current_dir(&bob)
        .env("EDITOR", "false")
        .env("VISUAL", format!("{} --wait", editor.display()))
        .arg("git-pull");
    let (code, stdout, stderr) = run(&mut cmd);
    assert_eq!(
        code, 0,
        "git-pull should succeed: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("Conflict"),
        "pull summary should report the conflict: {stdout}"
    );

    let raw = tsk_ok(&bob, &["show", "-T", "tsk-2", "-R"]);
    assert!(
        raw.starts_with("resolved title\n\nresolved body links [[tsk-1]]\n"),
        "editor result should be committed: {raw}"
    );
    assert!(
        !raw.contains("<<<<<<<"),
        "conflict markers should not be committed after editor resolution: {raw}"
    );
    let found = tsk_ok(&bob, &["prop", "find", "references", "[[tsk-1]]"]);
    assert!(
        found.contains("tsk-2\tresolved title"),
        "references index should match resolved body links: {found}"
    );
    let target = tsk_ok(&bob, &["show", "-T", "tsk-1", "-x"]);
    assert!(
        target.contains("referenced-by: \"[[tsk-2]]\""),
        "target backlink should match resolved body links: {target}"
    );
    let log = tsk_ok(&bob, &["log", "task", "-T", "tsk-2"]);
    assert!(
        log.contains("merge-conflict"),
        "resolved conflict should be committed as a merge-conflict: {log}"
    );
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
    let listing = tsk_ok(&bob, &["show", "-T", "tsk-1", "-x"]);
    assert!(
        listing.contains("priority: \"high\""),
        "alice's edit lost: {listing}"
    );
    assert!(
        listing.contains("owner: \"bob\""),
        "bob's edit lost: {listing}"
    );
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

#[test]
fn namespace_collision_rewrites_local_internal_links() {
    let (_dir, alice, bob) = setup_two_clones();

    // Bob's second task links to his local tsk-1 before the pull.
    tsk_ok(&bob, &["push", "bob's task"]);
    tsk_ok(&bob, &["push", "blocked by bob\n\nrelated to [[tsk-1]]"]);

    // Alice independently claims tsk-1 and pushes it to origin.
    tsk_ok(&alice, &["push", "alice's task"]);
    tsk_ok(&alice, &["git-push"]);

    // Bob's original tsk-1 is renumbered past his local-only tsk-2, so the
    // link in tsk-2 must follow it to tsk-3.
    let pull_out = tsk_ok(&bob, &["git-pull"]);
    assert!(
        pull_out.contains("tsk-1 → tsk-3") || pull_out.contains("tsk-1 \u{2192} tsk-3"),
        "expected renumber message in pull output, got: {pull_out}"
    );

    let linked = tsk_ok(&bob, &["show", "-T", "tsk-2", "-R"]);
    assert!(
        linked.contains("[[tsk-3]]"),
        "local link must track bob's renumbered task: {linked}"
    );
    assert!(
        !linked.contains("[[tsk-1]]"),
        "local link must not keep pointing at alice's task: {linked}"
    );
}
