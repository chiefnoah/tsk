# Architecture

`tsk` is a task tracker that stores everything in git refs under
`refs/tsk/*` of an enclosing repository. There are no working-tree
files — pushing the host repo's refs carries the task state along
with it. This document is a tour of the moving pieces; the user-
facing crib sheet lives in [AGENTS.md](AGENTS.md).

## On-disk layout

Everything lives inside the host repo's `.git` directory. Two
classes:

- **Shared, pushed**: `refs/tsk/*` — the actual task data. Visible
  to every clone via standard `git push` / `git fetch` once the
  refspec is set up.
- **Per-clone, not pushed**: `<git-dir>/tsk/{namespace,queue}` —
  two tiny text files holding the active selectors. Each clone
  picks its own.

```
<git-dir>/
├── refs/
│   └── tsk/
│       ├── tasks/<stable-id>          # one ref per task; commit chain = task history
│       ├── namespaces/<name>          # tree: { next, ids/<human-id> → <stable-id> }
│       ├── queues/<name>              # tree: { index, can-pull, inbox/<key> → <stable-id> }
│       └── properties/<key>           # tree: { <stable-id> → blob of values }
└── tsk/
    ├── namespace                      # active namespace (defaults to "tsk")
    ├── queue                          # active queue     (defaults to "tsk")
    └── remote                         # default git remote for sync (defaults to "origin")
```

Every category except `tasks` is a tree; per-task histories are
their own commit chain so each task's edit log stands on its own.

## Identifiers

Two layers of id, related but distinct:

- **Stable id** = SHA-1 of the task's initial `content` blob. Same
  body → same id, on every clone, forever. Used as the ref name
  (`refs/tsk/tasks/<stable-id>`) and as the cross-namespace handle.
- **Human id** = `tsk-<N>`. A small integer minted per-namespace by
  the namespace's `next` counter. Always namespace-scoped: `tsk-5`
  in namespace `alpha` is unrelated to `tsk-5` in namespace `tsk`.

A namespace is essentially `human → stable` plus the `next` counter.

```
namespace tsk         namespace claude
┌─ tsk-1 → 8f77…  ┐   ┌─ tsk-1 → 5c43…  ┐
├─ tsk-2 → 2a09…  │   ├─ tsk-2 → 8f77…  │   ← same stable, different humans
├─ tsk-3 → 0a2d…  │   └────────────────┘
└─ next: 4        ┘
```

## Module map

```mermaid
graph TD
    bin[bin/tsk + bin/git-tsk] --> lib[lib.rs<br/>CLI dispatch]
    lib --> workspace
    lib --> task[task.rs<br/>rich-text parser]
    lib --> fzf[fzf.rs<br/>picker wrapper]
    lib --> errors

    workspace --> object[object.rs<br/>task tree<br/>+ shared signature()]
    workspace --> namespace[namespace.rs<br/>human ↔ stable]
    workspace --> queue[queue.rs<br/>index + inbox]
    workspace --> properties[properties.rs<br/>per-key indices]
    workspace --> patch[patch.rs<br/>mbox export/import]
    workspace --> merge[merge.rs<br/>3-way reconcile]

    object --> propvalue[propvalue.rs<br/>length-prefix codec]
    properties --> propvalue
    merge --> namespace
    merge --> queue
    merge --> object
    patch --> object
```

`object.rs` is the choke point for git plumbing — every other
storage module reuses its `signature()` helper so all writes carry
the same author/committer.

## Task lifecycle

```
            tsk push                tsk drop                tsk reopen
no refs ──► open ─────────────────► done ─────────────────► open
              │                      │                       │
              │ tsk push <same body> │                       │
              └──────────────────────┘ (reopen-on-duplicate)
```

`status` is an auto-managed property: `open` on creation, flipped to
`done` on `tsk drop`. The task ref's commit chain is **never**
deleted — `done` is just a property and the binding stays addressable
(`tsk show -T tsk-N` keeps working). `tsk reopen` flips status back
and re-pushes onto the active queue.

Pushing a body whose stable id already exists short-circuits to a
reopen instead of clobbering — see `Workspace::new_task`.

## Operation flow: `tsk push "..."`

```
new_task(title, body)
        │
        ▼  ① compute stable id (= SHA of content blob)
        ▼  ② object::create   → refs/tsk/tasks/<sha>
        ▼  ③ properties::reindex_task → refs/tsk/properties/status
        ▼  ④ namespace::assign_id    → refs/tsk/namespaces/<active>
push_task(task)
        ▼  ⑤ queue::push_top         → refs/tsk/queues/<active>
```

The order matters: a crash between any two steps leaves recoverable
state because the most-derived ref (queue) is written last.
`tsk fix-up` (see `Workspace::gc_refs`) is the idempotent cleanup
pass that drops orphan property entries, ghost namespace bindings,
empty queues, and dangling queue index entries.

## Sync flow: `tsk git-pull`

```mermaid
sequenceDiagram
    participant Local
    participant Remote

    Local->>Remote: git fetch --refmap= +refs/tsk/*:refs/tsk-fetched/<remote>/*
    Note over Local: local refs/tsk/* untouched

    loop tasks
        Local->>Local: reconcile_task_refs<br/>(merge or rebase strategy)
    end
    loop namespaces
        Local->>Local: reconcile_namespace_refs<br/>(remote-wins; auto-renumber on collision)
    end
    loop queues
        Local->>Local: reconcile_queue_refs<br/>(3-way set/map merge)
    end
    Local->>Local: fast_forward_non_task_refs<br/>(properties, etc.)
```

Why the shadow `refs/tsk-fetched/<remote>/*` namespace? So the local
`refs/tsk/*` aren't clobbered before the reconcile pass gets a
chance to run. `--refmap=` disables the configured fetch refspec so
git doesn't sneakily perform the default mapping behind our back.

Reconciliation per ref-class:

| Ref class      | Strategy                                                         |
| -------------- | ---------------------------------------------------------------- |
| `tasks/*`      | git2 `merge_trees` 3-way; clean → merge commit, conflict → abort that task. `--rebase` replays local commits onto remote tip preserving authorship. |
| `namespaces/*` | data-aware: remote wins on `human → stable` collision, local binding renumbered to a fresh id past `max(local.next, remote.next)`. |
| `queues/*`     | per-stable 3-way set merge for `index`, per-key 3-way map merge for `inbox`, 3-way bool for `can_pull`. Empty base when no common ancestor. |
| everything else (`properties/*`) | force-update from fetched (concurrent edits on the same key/task are rare and `tsk fix-up` reindexes anyway). |

## Wire formats

Two ways to ship state between repos:

- **Standard git transport** (`tsk git-push` / `tsk git-pull`,
  configured via `tsk git-setup`). Carries every `refs/tsk/*`
  alongside ordinary branches.
- **mbox patch series** (`tsk export` / `tsk import`). Each task
  commit becomes one mbox entry: standard `From <sha>` separator,
  RFC-822 headers including `X-Tsk-Stable-Id`, `X-Tsk-Parent`,
  and an optional `X-Tsk-Namespace` binding hint, then a
  size-prefixed dump of every file in the task tree between
  `---tsk-tree---` and `---end---`. Survives strict mbox parsers
  via standard mboxrd `From `-mangling. Stable id is verified on
  import — tampered patches are rejected.

## Per-clone overrides

The active selectors live in `<git-dir>/tsk/{namespace,queue}`. Two
ways to override per-invocation:

- `tsk -q <queue> <command>` — global flag. A `OnceLock` set in
  `dispatch` is consulted by `Workspace::queue()` before falling
  back to the file.
- `tsk switch <ns>` / `tsk queue switch <q>` — persists by
  rewriting the file.

## Property values

Each property is a list of bytes-strings. Storage is length-prefixed
blocks (`size: N\n<N bytes>\n`) inside the per-key blob, mirroring
the patch wire format. This means values can contain anything,
including newlines. The reader still tolerates the legacy
line-split format and `tsk fix-up` migrates blobs on next save.

## Reconciliation matrix

| Drift class                                            | Detected by         | Fixed by                    |
| ------------------------------------------------------ | ------------------- | --------------------------- |
| Empty non-default queue                                | `gc_refs`           | delete the ref              |
| Property index entry → missing task ref                | `gc_refs`           | `properties::set(empty)`    |
| Namespace binding → missing task ref ("ghost")         | `gc_refs`           | rewrite namespace tree      |
| Queue index entry → missing task ref ("orphan")        | `gc_refs`           | rewrite queue tree          |
| Two clones diverged on the same task ref               | `git-pull` (tasks)  | merge or rebase             |
| Two clones allocated the same human id                 | `git-pull` (ns)     | auto-renumber local         |
| Two clones edited the same queue concurrently          | `git-pull` (queues) | 3-way set/map merge         |
| Property blob in legacy line-split format              | `tsk fix-up`        | re-save with new codec      |

`tsk fix-up` is the umbrella command that runs every one-shot
migration plus `gc_refs`; safe to run repeatedly.
