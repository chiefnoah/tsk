---
name: tsk
description: Use tsk, the task tracker maintained alongside this repository, to inspect task queues, manage local task records, create agent follow-ups, audit task history, and sync refs/tsk task state. Use when Codex is working in this repo and needs to choose queued work, record TODOs, mark tasks done, or coordinate task state with users or other agents.
---

# tsk Skill for Codex

Use `tsk` as the source of truth for local task work in this repository. Start by
looking at the active queue, keep user-visible task state accurate, and prefer
`tsk` records over ad hoc TODO comments.

## Basic workflow

```sh
tsk list
tsk show -T tsk-12
tsk push -- "Fix parser panic
Reproduce with an empty input file, then add a regression test."
tsk push -e -- "Draft task"      # open $EDITOR before saving
tsk append -- "Follow up: document parser edge cases"
tsk append -e -- "Follow up"     # open $EDITOR before saving
tsk drop -T tsk-12
tsk abandon -T tsk-12
```

- `tsk list` prints the active queue top first.
- `tsk show -T tsk-N` renders a task; add `-x` to include properties as YAML
  front matter, or `-R` for raw task text.
- `tsk push -- "title"` creates a task at the top of the active queue.
- `tsk append -- "title"` creates a task at the bottom of the active queue.
- Add `-e` to `push` or `append` to open `$EDITOR` before saving the new task.
- `tsk drop -x -T tsk-N` records the current git commit in `closed-on`, marks
  work done, and removes it from the active queue. Use plain `drop` only when
  there is no implementing commit to record.
- `tsk abandon -T tsk-N` removes a task from the active queue without changing
  its status.

## Creating and editing tasks

Use a single argument for a title, or put a body after the first newline:

```sh
tsk push -- "Investigate flaky sync test
Seen in CI after git-pull merge reconciliation. Capture the failing command and
decide whether the merge driver or test fixture owns the fix."
```

Read the body from stdin when another command already produced the detail:

```sh
printf 'Failure details...\n' | tsk push -- "Record failing CI output" -b -
```

Edit an existing task interactively with `$EDITOR`, or replace only the body
non-interactively with `-b`. Use `-b -` to read the replacement body from stdin:

```sh
tsk edit -T tsk-12
tsk edit -T tsk-12 -b "New body text"
printf 'New body from a script\n' | tsk edit -T tsk-12 -b -
```

Reopen completed work when it becomes active again:

```sh
tsk reopen -T tsk-12
tsk reopen --no-queue -T tsk-12
```

## Queue order

Move important work up and park lower-priority work at the bottom:

```sh
tsk prioritize -T tsk-9
tsk deprioritize -T tsk-15
tsk swap
tsk rot
tsk tor
```

- `prioritize` moves a task to the top of the active queue.
- `deprioritize` moves a task to the bottom.
- `swap`, `rot`, and `tor` are quick stack operations for the top tasks.

## History and audit

Every task, namespace, and queue is backed by git history:

```sh
tsk log task -T tsk-9
tsk log namespace
tsk log namespace claude
tsk log queue
tsk log queue review
```

Use logs to understand who changed task text, id bindings, or queue order before
you overwrite or reorganize work.

## Sharing, assigning, and inboxes

Share a task into another namespace when another user or agent needs a local id
for the same underlying task:

```sh
tsk share claude -T tsk-9
```

Assign a task from the active queue to another queue's inbox:

```sh
tsk assign review -T tsk-9
tsk assign review -T tsk-9 -R ""
```

The `-R ""` form skips the default auto-push. Without it, assign tries to push
the relevant `refs/tsk/*` refs to the default remote.

Work an inbox from the receiving queue:

```sh
tsk queue switch review
tsk inbox
tsk accept
tsk accept review-1
tsk reject review-2
tsk inbox -R ""
tsk accept review-1 -R ""
tsk reject review-2 -R ""
```

- `tsk inbox` lists pending items for the active queue and auto-pulls first by
  default.
- `tsk accept [key]` moves an inbox item onto the active queue.
- `tsk reject [key]` returns an inbox item to its source queue's inbox.
- Use `-R ""` on inbox, accept, or reject to skip the default remote action.

## Namespaces

Namespaces control human-readable task ids such as `tsk-12`. The same stable task
can be bound into multiple namespaces with different local ids.

```sh
tsk namespace current
tsk namespace list
tsk namespace switch claude
tsk switch tsk
tsk namespace tasks
tsk namespace tasks claude
```

- `namespace current` prints the active namespace.
- `namespace list` lists known namespaces.
- `namespace switch <name>` changes the active namespace; `tsk switch <name>` is
  shorthand.
- `namespace tasks [name]` lists every task bound in a namespace, independent of
  queue membership.

Use a separate agent namespace such as `claude` when you need agent-owned ids or
follow-up bookkeeping that should not disturb the user's namespace.

## Queues

Queues are ordered work stacks. Task lifecycle comes from the protected
`status` property, not queue membership. Keep the user's queue focused; put
agent-created follow-ups in an agent queue when they are not part of the current
request.

```sh
tsk queue current
tsk queue list
tsk queue switch claude
tsk --queue review list
```

- `queue current` prints the active queue.
- `queue list` lists known queues.
- `queue switch <name>` changes the active queue.
- `--queue <name>` overrides the active queue for one invocation.

Example agent follow-up:

```sh
tsk queue switch claude
tsk push -- "Add regression test for imported task conflict
The current change fixed the bug, but there is no test covering an imported task
whose namespace id collides after git-pull."
tsk queue switch tsk
```

## Remote sync

Use `tsk` sync commands for `refs/tsk/*`; plain `git fetch` can overwrite task
refs without running tsk's reconciliation logic.

```sh
tsk remote default
tsk remote set-default origin
tsk git-setup -r origin
tsk git-pull
tsk git-pull --rebase
tsk git-push
```

- `git-setup` configures refspecs for sharing `refs/tsk/*`.
- `git-pull` fetches and reconciles divergent task histories.
- `git-push` pushes task refs to the default or supplied remote.

## Agent rules of thumb

- Run `tsk list` before choosing work unless the user gave an explicit task.
- Use `./target/release/tsk` after rebuilding this repo, because an installed
  `tsk` may lag behind local changes.
- Commit completed code changes before dropping the task. Include the
  human-readable task id at the bottom of the commit message body, then drop the
  task with `./target/release/tsk drop -x -T tsk-N` so `closed-on` records the
  implementing commit. Drop without `-x` only when the user explicitly confirms
  or there is no code commit.
- Treat properties and queue changes as user-visible state.
- Prefer `tsk push` in an agent queue for follow-up work instead of adding
  unrelated `TODO` comments in code.
