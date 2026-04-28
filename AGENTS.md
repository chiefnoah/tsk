# tsk for agents

`tsk` is the task tracker maintained alongside this codebase. Use it to
discover what to work on, record TODOs you spawn, and mark work done.

## Quick reference

```
tsk list                 # active queue, top first
tsk show -T tsk-N        # render one task (rich text)
tsk show -T tsk-N -x     # also dump properties as YAML front-matter
tsk push -- "title"      # new task at top of active queue, status=open
tsk push -- "title
body"                    # multi-line title gets split at first newline
tsk push -- "title" -b - # read body from stdin
tsk drop -T tsk-N        # remove from queue, flip status=done
tsk edit -T tsk-N        # open $EDITOR on the task body
```

## Properties

Every task has zero or more text values per property. Status is auto-managed
(`open` on push, `done` on drop). Anything else is set by hand:

```
tsk prop add  -T tsk-N <key> <value>     # append a value
tsk prop set  -T tsk-N <key> <values...> # replace the whole list
tsk prop unset -T tsk-N <key> [<value>]  # drop one value, or the whole key
tsk prop list -T tsk-N                   # all (key, value) lines on a task
tsk prop find <key> [<value>]            # tasks with that key (= value)
tsk prop find status open                # all in-progress tasks
```

## Namespaces and queues

The repo has two of each by convention:

| Kind        | `tsk`              | `claude`                   |
|-------------|--------------------|----------------------------|
| Namespace   | user-owned task ids| agent-owned task ids       |
| Queue       | user's backlog     | agent's backlog (TODOs)    |

Both are pushed/shared at `refs/tsk/{namespaces,queues,tasks,properties}/*`.
The active selection is per-clone (lives in `<git-dir>/tsk/`):

```
tsk namespace current     # what you're reading/writing as
tsk namespace tasks       # every (id, title) bound here, ignoring queue
tsk queue current         # what your push/list/drop affect
tsk switch <name>         # switch namespace (no arg = fzf-pick)
tsk queue switch <name>
```

## When you spawn a TODO

If you find scope-creep or follow-up work, push it to the `claude` queue
rather than expanding the current task or dropping a `// TODO:` in code:

```
tsk queue switch claude
tsk push -- "<short title>
<body explaining why and what to do>"
tsk queue switch tsk      # switch back when done
```

This keeps the user's `tsk` queue free of agent-internal bookkeeping while
still surfacing the work for later.

## History

Every task and namespace is a commit chain in git, so edits are
auditable:

```
tsk log task -T tsk-N            # all commits on a task's tree
tsk log namespace [<name>]       # id assignments / drops in a namespace
```

## Migrations

Storage and conventions evolve. A single command runs every known one-shot
fix-up against the active workspace; safe to re-run (each migration is
idempotent):

```
tsk fix-up
```

## Sync with the user

```
tsk git-setup [-r <remote>]   # one-time per clone: configure refspecs so plain
                              # git push/fetch will carry refs/tsk/* (default remote
                              # is "origin"). Mutates .git/config; opt-in.
tsk remote set-default <name> # use this git remote for git-push/pull and the auto-push paths
tsk git-push              # push refs/tsk/* to default remote (also runs after assign/reject)
tsk git-pull              # fetch + reconcile divergent task histories (with merge driver)
tsk git-pull --rebase     # replay local commits on the remote tip instead of merging
tsk inbox                 # auto-pulls then lists pending inbox items
```

Use `tsk git-pull` (not plain `git fetch`) when local task / queue /
namespace refs may have diverged from the remote — only the tsk command
runs the 3-way merge driver. Plain `git fetch` force-overwrites them.

Note: `tsk git-setup` adds `remote.<name>.push = refs/tsk/*:refs/tsk/*`
which OVERRIDES `push.default`. After running it, `git push` with no
args pushes the configured refspecs only (i.e. tsk refs) — branches
need an explicit `git push <remote> <branch>`. If that's not what you
want, just keep using `tsk git-push` and skip the setup.

## Offline transfer (email, etc.)

Export a task as an mbox-format patch series and pipe it anywhere:

```
tsk export -T tsk-N [--bind] > task.mbox    # one mbox entry per commit
tsk import [--bind] < task.mbox             # rebuild + verify on receiver
```

`--bind` on export embeds your namespace+human-id; `--bind` on import binds
the task into the receiver's active namespace. Stable ids are content-
addressed, so the receiver verifies the SHA on import — tampered patches
are rejected.

## Conventions for agents working this repo

- Always `tsk list` first. Don't invent work; pick the top task or ask.
- Run from `./target/release/tsk` after any rebuild (the user's installed
  `tsk` may lag behind your changes).
- Drop a task only when the work is committed (or the user has confirmed).
- Properties you set are pushed; treat them as user-visible state, not
  scratch space.
- Markup in task bodies: `!bold!`, `*italic*`, `_under_`, `~strike~`,
  `=highlight=`, `` `code` ``, `[text](url)`, `[[tsk-N]]`. Stripped on
  `tsk show -R`.
