use crate::errors::{self, Result};
use crate::parse_id;
use crate::workspace::{self, Id, InboxItem, LogCommit, TaskIdentifier, Workspace};
use crate::{LogTarget, NamespaceAction, PropAction, QueueAction, RemoteAction, TaskId, Title};
use crate::{fzf, merge, queue, task};
use edit::edit as open_editor;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio, exit};

pub(crate) fn clean(dir: PathBuf) -> Result<()> {
    let report = Workspace::from_path(dir)?.clean()?;
    println!(
        "clean: repaired {} task(s), pruned {} queue entries, {} empty queues, \
         {} orphan property entries, {} ghost namespace bindings",
        report.tasks_repaired,
        report.queue_entries_pruned,
        report.queues_pruned,
        report.property_orphans_pruned,
        report.ghost_bindings_pruned
    );
    Ok(())
}

pub(crate) fn git_setup(dir: PathBuf, remote: Option<String>) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let r = remote.unwrap_or(ws.default_remote()?);
    ws.configure_git_remote_refspecs(&r)
}

pub(crate) fn git_push(dir: PathBuf, remote: Option<String>) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let r = remote.unwrap_or(ws.default_remote()?);
    ws.git_push(&r)
}

pub(crate) fn git_pull(dir: PathBuf, remote: Option<String>, rebase: bool) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let r = remote.unwrap_or(ws.default_remote()?);
    let strategy = if rebase {
        merge::Strategy::Rebase
    } else {
        merge::Strategy::Merge
    };
    let outcome = ws.git_pull_with_strategy(&r, strategy)?;
    for rec in &outcome.tasks {
        if !matches!(rec.kind, merge::ReconKind::Unchanged) {
            let short = &rec.stable.0[..12.min(rec.stable.0.len())];
            println!("{:?} {short}", rec.kind);
        }
    }
    for nr in &outcome.namespaces {
        for (old, new) in &nr.renumbers {
            println!(
                "{}-{} \u{2192} {}-{} (conflict with {r})",
                nr.namespace, old, nr.namespace, new
            );
        }
    }
    for qr in &outcome.queues {
        match qr.kind {
            merge::QueueReconKind::Merged => println!("merged queue {}", qr.name),
            merge::QueueReconKind::Deleted => println!("deleted queue {}", qr.name),
        }
    }
    Ok(())
}

fn effective_remote(ws: &Workspace, supplied: Option<String>) -> Result<Option<String>> {
    match supplied {
        Some(s) if s.is_empty() => Ok(None),
        Some(s) => Ok(Some(s)),
        None => Ok(Some(ws.default_remote()?)),
    }
}

/// Scoped push (best-effort, silent on `-R ""`).
fn auto_push_refs(ws: &Workspace, remote: Option<String>, refs: Vec<String>) -> Result<()> {
    if let Some(r) = effective_remote(ws, remote)? {
        let _ = ws.git_push_refs(&r, &refs);
    }
    Ok(())
}

fn print_two_col_row(left: impl std::fmt::Display, right: impl std::fmt::Display) {
    println!("{left}\t{right}");
}

fn print_header(cols: &[&str]) {
    println!("{}", cols.join("\t"));
}

fn render_two_col_row(left: impl std::fmt::Display, right: impl std::fmt::Display) -> String {
    format!("{left}\t{right}")
}

fn print_key_value_row(key: impl std::fmt::Display, value: impl std::fmt::Display) {
    println!("{key}\t{value}");
}

fn print_inbox_row(item: &InboxItem) {
    println!("{}\tfrom {}\t{}", item.key, item.source_queue, item.title);
}

fn print_log_row(commit: &LogCommit) {
    // git-log --oneline-style: short oid, summary, then author + date below.
    let short = &commit.oid[..commit.oid.len().min(8)];
    println!("{short} {}", commit.summary);
    println!("    {} ({})", commit.author, format_unix(commit.timestamp));
}

fn read_title_and_body(
    edit: bool,
    body: Option<String>,
    title_arg: Title,
) -> Result<(String, String)> {
    let mut title = if let Some(t) = title_arg.title {
        t
    } else if let Some(ts) = title_arg.title_simple {
        ts.join(" ")
    } else {
        String::new()
    };
    let mut body = if body.is_none() {
        if let Some((first, rest)) = title.split_once('\n') {
            let extracted = rest.to_string();
            title = first.to_string();
            extracted
        } else {
            String::new()
        }
    } else {
        title = title.replace(['\n', '\r'], " ");
        body.unwrap_or_default()
    };
    if body == "-" {
        body.clear();
        io::stdin().read_to_string(&mut body)?;
    }
    if edit {
        let new_content = open_editor(format!("{title}\n\n{body}"))?;
        if let Some((t, b)) = new_content.split_once('\n') {
            title = t.to_string();
            body = b.trim_start_matches('\n').to_string();
        }
    }
    title = title.replace(['\n', '\r'], " ");
    Ok((title, body))
}

pub(crate) fn command_push(
    dir: PathBuf,
    edit: bool,
    body: Option<String>,
    title: Title,
    on_top: bool,
) -> Result<()> {
    let (title, body) = read_title_and_body(edit, body, title)?;
    let ws = Workspace::from_path(dir)?;
    let task = ws.new_task(title, body)?;
    if on_top {
        ws.push_task(task)
    } else {
        ws.append_task(task)
    }
}

pub(crate) fn command_list(
    dir: PathBuf,
    all: bool,
    count: usize,
    ids_only: bool,
    headers: bool,
) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let stack = ws.read_stack()?;
    if stack.is_empty() {
        println!("*No tasks*");
        return Ok(());
    }
    if headers {
        if ids_only {
            print_header(&["id"]);
        } else {
            print_header(&["id", "title"]);
        }
    }
    for (i, entry) in stack.iter().enumerate() {
        if !all && i >= count {
            break;
        }
        if ids_only {
            println!("{}", entry.id);
        } else {
            print_two_col_row(entry.id, &entry.title);
        }
    }
    Ok(())
}

pub(crate) fn command_open(dir: PathBuf, headers: bool) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let tasks = ws.open_tasks()?;
    if tasks.is_empty() {
        println!("*No open tasks*");
        return Ok(());
    }
    if headers {
        print_header(&["id", "queues", "title"]);
    }
    for task in tasks {
        let queues = if task.queues.is_empty() {
            "none".to_string()
        } else {
            task.queues.join(",")
        };
        println!("{}\t{}\t{}", task.id, queues, task.title);
    }
    Ok(())
}

pub(crate) fn command_find(dir: PathBuf, multi: bool, all: bool, body: bool) -> Result<()> {
    let ws = Workspace::from_path(dir.clone())?;
    let entries = if all {
        ws.list_namespace_tasks(&ws.namespace()?)?
    } else {
        ws.read_stack()?
    };
    if entries.is_empty() {
        return Err(errors::Error::NoTasks);
    }

    for id in select_task_ids(&dir, &ws, entries, body, multi)? {
        println!("{id}");
    }
    Ok(())
}

fn task_search_lines(
    ws: &Workspace,
    entries: impl IntoIterator<Item = workspace::StackEntry>,
    body: bool,
) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for entry in entries {
        let mut line = render_two_col_row(entry.id, single_line(&entry.title));
        if body {
            let task = ws.task(TaskIdentifier::Id(entry.id))?;
            line.push('\t');
            line.push_str(&single_line(&task.body));
        }
        lines.push(line);
    }
    Ok(lines)
}

fn task_search_args(dir: &std::path::Path, body: bool, multi: bool) -> Result<Vec<OsString>> {
    let preview = format!(
        "CLICOLOR_FORCE=1 {} -C {} show -x -T {{1}}",
        shell_quote(&std::env::current_exe()?.to_string_lossy()),
        shell_quote(&dir.to_string_lossy()),
    );
    let mut args: Vec<OsString> = vec![
        "--ansi".into(),
        "--delimiter".into(),
        "\t".into(),
        "--with-nth".into(),
        if body { "1,2,3" } else { "1,2" }.into(),
        "--nth".into(),
        if body { "1,2,3" } else { "1,2" }.into(),
        "--preview".into(),
        preview.into(),
        "--preview-window".into(),
        "up:60%:wrap".into(),
        "--prompt".into(),
        "task> ".into(),
    ];
    if multi {
        args.push("--multi".into());
    }
    Ok(args)
}

fn select_task_ids(
    dir: &std::path::Path,
    ws: &Workspace,
    entries: Vec<workspace::StackEntry>,
    body: bool,
    multi: bool,
) -> Result<Vec<Id>> {
    let lines = task_search_lines(ws, entries, body)?;
    let args = task_search_args(dir, body, multi)?;

    fzf::select_raw(lines, args)?
        .into_iter()
        .filter_map(|selected| {
            selected
                .split('\t')
                .next()
                .filter(|id| !id.is_empty())
                .map(parse_id)
        })
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| errors::Error::Parse(err.to_string()))
}

fn single_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn command_reopen(
    dir: PathBuf,
    task_id: TaskId,
    body: bool,
    no_queue: bool,
) -> Result<()> {
    let ws = Workspace::from_path(dir.clone())?;
    let identifier = if task_id.is_empty() {
        let entries = ws.closed_tasks()?;
        if entries.is_empty() {
            return Err(errors::Error::NoTasks);
        }
        let picked = select_task_ids(&dir, &ws, entries, body, false)?
            .into_iter()
            .next()
            .ok_or_else(|| errors::Error::Parse("No task selected".into()))?;
        TaskIdentifier::Id(picked)
    } else {
        task_id.into()
    };
    let id = ws.reopen(identifier, !no_queue)?;
    println!("Reopened {id}");
    Ok(())
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub(crate) fn command_show(
    dir: PathBuf,
    task_id: TaskId,
    show_attrs: bool,
    stable_id: bool,
    latest_commit: bool,
    raw: bool,
) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let task = ws.task(task_id.into())?;
    if stable_id {
        println!("{}", task.stable);
        return Ok(());
    }
    if latest_commit {
        let commits = ws.log_ref(&task.stable.refname())?;
        let commit = commits
            .first()
            .ok_or_else(|| errors::Error::Parse(format!("task {} has no commits", task.stable)))?;
        println!("{}", commit.oid);
        return Ok(());
    }
    if show_attrs && !task.attributes.is_empty() {
        println!("---");
        for (k, vs) in &task.attributes {
            for v in vs {
                println!("{k}: \"{v}\"");
            }
        }
        println!("---");
    }
    let plain = task.to_string();
    match (raw, task::parse(&plain)) {
        (false, Some(parsed)) => {
            print!("{}", parsed.content);
            // Footnote section: resolve each [[...]] link against the active
            // namespace (or just echo for foreign / external links).
            if !parsed.links.is_empty() {
                println!();
                for (i, link) in parsed.links.iter().enumerate() {
                    println!("\n{} {}", task::super_num(i + 1), render_link(&ws, link));
                }
            }
        }
        _ => print!("{plain}"),
    }
    println!();
    Ok(())
}

fn render_link(ws: &Workspace, link: &task::ParsedLink) -> String {
    use task::ParsedLink::*;
    match link {
        Internal(id) => match ws.task((*id).into()) {
            Ok(t) => format!("{id}: {}", t.title),
            Err(_) => match ws.namespace() {
                Ok(ns) => format!("{id}: <not bound in '{ns}'>"),
                Err(e) => format!("{id}: <invalid namespace: {e}>"),
            },
        },
        Namespaced { namespace, id } => format!("{namespace}/{id}"),
        Foreign { prefix, id } => format!("{prefix}-{id} (foreign)"),
        External(url) => url.to_string(),
    }
}

fn render_follow_link(link: &task::ParsedLink) -> String {
    use task::ParsedLink::*;
    match link {
        Internal(id) => format!("[[{id}]]"),
        Namespaced { namespace, id } => format!("[[{namespace}/{id}]]"),
        Foreign { prefix, id } => format!("[[{prefix}-{id}]]"),
        External(url) => url.to_string(),
    }
}

fn taskid_from_id(id: Id) -> TaskId {
    TaskId {
        id: None,
        tsk_id: Some(id),
        relative_id: None,
    }
}

pub(crate) fn command_follow(
    dir: PathBuf,
    task_id: TaskId,
    link_index: Option<usize>,
    select: bool,
    edit: bool,
) -> Result<()> {
    let ws = Workspace::from_path(dir.clone())?;
    let task = ws.task(task_id.into())?;
    let Some(parsed_task) = task::parse(&task.to_string()) else {
        eprintln!("Unable to parse any links from body.");
        exit(1);
    };
    if parsed_task.links.is_empty() {
        eprintln!("No links found in {}.", task.id);
        return Ok(());
    }

    let idx = match (link_index, select) {
        (Some(n), _) => n,
        (None, true) => {
            let lines: Vec<String> = parsed_task
                .links
                .iter()
                .enumerate()
                .map(|(i, link)| render_two_col_row(i + 1, render_follow_link(link)))
                .collect();
            let selected = fzf::select_raw(
                lines,
                [
                    "--delimiter",
                    "\t",
                    "--with-nth",
                    "1,2",
                    "--nth",
                    "1,2",
                    "--prompt",
                    "link> ",
                ],
            )?;
            selected
                .first()
                .and_then(|line| line.split('\t').next())
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or_else(|| {
                    eprintln!("No link selected.");
                    exit(1);
                })
        }
        (None, false) => {
            for (i, link) in parsed_task.links.iter().enumerate() {
                print_two_col_row(i + 1, render_follow_link(link));
            }
            return Ok(());
        }
    };

    if idx == 0 || idx > parsed_task.links.len() {
        eprintln!("Link index out of bounds.");
        exit(1);
    }
    match &parsed_task.links[idx - 1] {
        task::ParsedLink::External(url) => open_detached(url.as_str()),
        task::ParsedLink::Internal(id) => {
            let task_id = taskid_from_id(*id);
            if edit {
                command_edit(dir, task_id, None)
            } else {
                command_show(dir, task_id, false, false, false, false)
            }
        }
        task::ParsedLink::Namespaced { namespace, id } => {
            if edit {
                eprintln!("Editing namespaced links is not supported.");
                exit(1);
            }
            let task = Workspace::from_path(dir)?.task_in_namespace(namespace, *id)?;
            let plain = task.to_string();
            match task::parse(&plain) {
                Some(parsed) => print!("{}", parsed.content),
                None => print!("{plain}"),
            }
            println!();
            Ok(())
        }
        task::ParsedLink::Foreign { prefix, id } => Err(errors::Error::Parse(format!(
            "foreign link resolution is not supported in this storage backend: {prefix}-{id}"
        ))),
    }
}

fn open_detached(target: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = Command::new("open");
        c.arg(target);
        c
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", "", target]);
        c
    };
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let mut command = {
        let mut c = Command::new("xdg-open");
        c.arg(target);
        c
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

pub(crate) fn command_edit(dir: PathBuf, task_id: TaskId, body: Option<String>) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let mut task = ws.task(task_id.into())?;
    if let Some(mut body) = body {
        if body == "-" {
            body.clear();
            io::stdin().read_to_string(&mut body)?;
        }
        task.body = body;
        save_edited_task(&ws, &task)?;
        return Ok(());
    }

    let new_content = open_editor(format!("{}\n\n{}", task.title.trim(), task.body.trim()))?;
    if let Some((title, body)) = new_content.split_once('\n') {
        task.title = title.replace(['\n', '\r'], " ");
        task.body = body.trim_start_matches('\n').to_string();
        save_edited_task(&ws, &task)?;
    }
    Ok(())
}

fn save_edited_task(ws: &Workspace, task: &workspace::Task) -> Result<()> {
    let outcome = ws.save_task_with_outcome(task)?;
    for dependency in outcome.created_dependencies {
        eprintln!(
            "Created blocking task {}\t{}",
            dependency.task_ref, dependency.title
        );
    }
    Ok(())
}

pub(crate) fn command_drop(dir: PathBuf, task_id: TaskId, closed_on_commit: bool) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let dropped = if closed_on_commit {
        let closed_on = ws.head_commit()?;
        ws.drop_with_closed_on(task_id.into(), Some(closed_on))?
    } else {
        ws.drop(task_id.into())?
    };
    if let Some(id) = dropped {
        println!("Dropped {id}");
        Ok(())
    } else {
        eprintln!("No task to drop.");
        exit(1);
    }
}

pub(crate) fn command_abandon(dir: PathBuf, task_id: TaskId) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let id = ws.abandon(task_id.into())?;
    println!("Abandoned {id}");
    Ok(())
}

pub(crate) fn command_share(dir: PathBuf, target: String, task_id: TaskId) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let h = ws.share(task_id.into(), &target)?;
    println!("Shared as {target}/tsk-{h}");
    Ok(())
}

pub(crate) fn command_assign(
    dir: PathBuf,
    target: Option<String>,
    task_id: TaskId,
    remote: Option<String>,
) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let target = match target {
        Some(t) => t,
        None => pick_assign_target(&ws)?,
    };
    let (key, stable) = ws.assign_to_queue(task_id.into(), &target)?;
    println!("Assigned to {target} as {key}");
    auto_push_refs(&ws, remote, ws.refs_for_assign_out(&target, &stable)?)?;
    Ok(())
}

fn pick_assign_target(ws: &Workspace) -> Result<String> {
    let cur = ws.queue()?;
    let candidates: Vec<String> = ws
        .list_queues()?
        .into_iter()
        .filter(|q| q != &cur)
        .collect();
    if candidates.is_empty() {
        return Err(errors::Error::Parse("No other queues to assign to".into()));
    }
    fzf::select::<_, String, _>(candidates, ["--prompt=assign to> "])?
        .ok_or_else(|| errors::Error::Parse("No queue selected".into()))
}

pub(crate) fn command_pull(dir: PathBuf, source: String, task_id: TaskId) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    // For pull, the task id is interpreted in the source queue's namespace
    // mapping context. Simplification: require the caller to use -T <stable>
    // form via human id in active namespace. For v1 we just resolve in
    // active namespace; sharing first lets the user reference foreign tasks.
    let id = ws.pull_from_queue(&source, task_id.into())?;
    println!("Pulled {id}");
    Ok(())
}

pub(crate) fn command_inbox(dir: PathBuf, remote: Option<String>, headers: bool) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    if let Some(r) = effective_remote(&ws, remote)? {
        let refs = ws.refs_for_inbox_pull()?;
        let _ = ws.git_fetch_refs(&r, &refs);
    }
    let inbox = ws.list_inbox()?;
    if inbox.is_empty() {
        println!("*Empty*");
        return Ok(());
    }
    if headers {
        print_header(&["key", "source", "title"]);
    }
    for item in inbox {
        print_inbox_row(&item);
    }
    Ok(())
}

fn pick_inbox_key(ws: &Workspace, key: Option<String>) -> Result<String> {
    if let Some(k) = key {
        return Ok(k);
    }
    Ok(ws
        .list_inbox()?
        .into_iter()
        .next()
        .ok_or_else(|| errors::Error::Parse("Inbox is empty".into()))?
        .key)
}

pub(crate) fn command_accept(
    dir: PathBuf,
    key: Option<String>,
    task_id: TaskId,
    remote: Option<String>,
) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    if !task_id.is_empty() {
        if key.is_some() {
            return Err(errors::Error::Parse(
                "accept takes either an inbox key or a task id, not both".into(),
            ));
        }
        let (id, stable) = ws.accept_unassigned(task_id.into())?;
        println!("Accepted {id}");
        auto_push_refs(&ws, remote, ws.refs_for_accept_unassigned(&stable)?)?;
        return Ok(());
    }
    let key = pick_inbox_key(&ws, key)?;
    let id = ws.accept_inbox(&key)?;
    println!("Accepted as {id}");
    auto_push_refs(&ws, remote, ws.refs_for_accept_inbox()?)?;
    Ok(())
}

pub(crate) fn command_reject(
    dir: PathBuf,
    key: Option<String>,
    remote: Option<String>,
) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let key = pick_inbox_key(&ws, key)?;
    ws.reject_inbox(&key)?;
    let source = key.rsplit_once('-').map(|(s, _)| s.to_string());
    match &source {
        Some(src) => println!("Rejected {key} (returned to '{src}' inbox)"),
        None => println!("Rejected {key}"),
    }
    if let Some(src) = source {
        auto_push_refs(&ws, remote, ws.refs_for_reject_inbox(&src)?)?;
    }
    Ok(())
}

pub(crate) fn command_export(
    dir: PathBuf,
    ids: Vec<Id>,
    where_: Option<String>,
    all: bool,
    bind: bool,
) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let mut identifiers: Vec<TaskIdentifier> = ids.into_iter().map(Into::into).collect();
    if all {
        for entry in ws.list_namespace_tasks(&ws.namespace()?)? {
            identifiers.push(TaskIdentifier::Id(entry.id));
        }
    }
    if let Some(spec) = where_ {
        let (key, value) = spec
            .split_once('=')
            .ok_or_else(|| errors::Error::Parse("expected --where KEY=VALUE".into()))?;
        for (id, _stable, _title) in ws.find_by_property(key, Some(value))? {
            identifiers.push(TaskIdentifier::Id(id));
        }
    }
    if identifiers.is_empty() {
        // Interactive fallback: fzf single-pick.
        identifiers.push(TaskId::default().resolve_or_pick(&ws)?);
    }
    // Dedupe while preserving order.
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    identifiers.retain(|i| !matches!(i, TaskIdentifier::Id(id) if !seen.insert(id.0)));
    let mbox = ws.export_tasks(&identifiers, bind)?;
    print!("{mbox}");
    Ok(())
}

pub(crate) fn command_import(dir: PathBuf, bind: bool) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
    let outcomes = ws.import_task(&buf, bind)?;
    for res in &outcomes {
        let bound = if let Some(id) = res.bound_human {
            format!(" bound as {}-{}", ws.namespace()?, id)
        } else {
            String::new()
        };
        println!(
            "Imported {} commit(s) for task {}{bound}",
            res.commits_imported, res.stable
        );
    }
    Ok(())
}

pub(crate) fn command_log(dir: PathBuf, target: LogTarget) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let commits = match target {
        LogTarget::Task { task_id } => ws.log_task(task_id.into())?,
        LogTarget::Namespace { name } => {
            let target = match name {
                Some(name) => name,
                None => ws.namespace()?,
            };
            ws.log_namespace(&target)?
        }
        LogTarget::Queue { name } => {
            let target = match name {
                Some(name) => name,
                None => ws.queue()?,
            };
            ws.log_queue(&target)?
        }
    };
    for commit in commits {
        print_log_row(&commit);
    }
    Ok(())
}

fn format_unix(ts: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let delta = now - ts;
    if delta < 0 {
        return "in the future".to_string();
    }
    relative_time(delta as u64)
}

fn relative_time(secs: u64) -> String {
    const M: u64 = 60;
    const H: u64 = 60 * M;
    const D: u64 = 24 * H;
    if secs < M {
        format!("{secs}s ago")
    } else if secs < H {
        format!("{}m ago", secs / M)
    } else if secs < D {
        format!("{}h ago", secs / H)
    } else if secs < 30 * D {
        format!("{}d ago", secs / D)
    } else if secs < 365 * D {
        format!("{}mo ago", secs / (30 * D))
    } else {
        format!("{}y ago", secs / (365 * D))
    }
}

pub(crate) fn command_prop(dir: PathBuf, action: PropAction, headers: bool) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    match action {
        PropAction::List { task_id } => {
            let task = ws.task(task_id.into())?;
            if headers && !task.attributes.is_empty() {
                print_header(&["key", "value"]);
            }
            for (key, values) in &task.attributes {
                if values.is_empty() {
                    println!("{key}");
                } else {
                    for value in values {
                        print_key_value_row(key, value);
                    }
                }
            }
        }
        PropAction::Get { task_id, key } => {
            let task = ws.task(task_id.into())?;
            let values = task.attributes.get(&key).ok_or_else(|| {
                errors::Error::Parse(format!("Task {} has no property '{key}'", task.id))
            })?;
            print_lines(values);
        }
        PropAction::Add {
            task_id,
            key,
            value,
        } => ws.add_property_value(task_id.into(), &key, &value)?,
        PropAction::Set {
            task_id,
            key,
            values,
        } => ws.set_property(task_id.into(), &key, values)?,
        PropAction::Unset {
            task_id,
            key,
            value,
        } => ws.unset_property(task_id.into(), &key, value.as_deref())?,
        PropAction::Keys { task_id } => {
            let task = ws.task(task_id.into())?;
            if headers && !task.attributes.is_empty() {
                print_header(&["key"]);
            }
            print_lines(task.attributes.keys());
        }
        PropAction::Values { key } => {
            let values = ws.property_values(&key)?;
            if headers && !values.is_empty() {
                print_header(&["value"]);
            }
            print_lines(values);
        }
        PropAction::Find { key, value } => {
            let key = match key {
                Some(k) => k,
                None => fzf::select::<_, String, _>(ws.property_keys()?, ["--prompt=key> "])?
                    .ok_or_else(|| errors::Error::Parse("No key selected".into()))?,
            };
            let value = match value {
                Some(v) if v == "<any>" => None,
                Some(v) => Some(v),
                None => {
                    let mut choices = ws.property_values(&key)?;
                    choices.insert(0, "<any>".to_string());
                    let picked = fzf::select::<_, String, _>(choices, ["--prompt=value> "])?
                        .ok_or_else(|| errors::Error::Parse("No value selected".into()))?;
                    if picked == "<any>" {
                        None
                    } else {
                        Some(picked)
                    }
                }
            };
            let matches = ws.find_by_property(&key, value.as_deref())?;
            if headers && !matches.is_empty() {
                print_header(&["id", "title"]);
            }
            for (id, _stable, title) in matches {
                print_two_col_row(id, title);
            }
        }
    }
    Ok(())
}

fn print_lines<I: std::fmt::Display>(items: impl IntoIterator<Item = I>) {
    for i in items {
        println!("{i}");
    }
}

pub(crate) fn command_namespace(
    dir: PathBuf,
    action: NamespaceAction,
    headers: bool,
) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    match action {
        NamespaceAction::List { head } => {
            let namespaces = ws.list_namespaces()?;
            if headers && !namespaces.is_empty() {
                if head {
                    print_header(&["namespace", "head"]);
                } else {
                    print_header(&["namespace"]);
                }
            }
            if head {
                for name in namespaces {
                    print_key_value_row(&name, ws.namespace_head_commit(&name)?);
                }
            } else {
                print_lines(namespaces);
            }
        }
        NamespaceAction::Current { head } => {
            let name = ws.namespace()?;
            if head {
                println!("{}", ws.namespace_head_commit(&name)?);
            } else {
                println!("{name}");
            }
        }
        NamespaceAction::Switch { name } => return resolve_and_switch_namespace(&ws, name),
        NamespaceAction::Tasks { name } => {
            let target = match name {
                Some(name) => name,
                None => ws.namespace()?,
            };
            let entries = ws.list_namespace_tasks(&target)?;
            if headers && !entries.is_empty() {
                print_header(&["id", "title"]);
            }
            for entry in entries {
                print_two_col_row(entry.id, entry.title);
            }
        }
        NamespaceAction::Props { name } => {
            let target = match name {
                Some(name) => name,
                None => ws.namespace()?,
            };
            let keys = ws.namespace_property_keys(&target)?;
            if headers && !keys.is_empty() {
                print_header(&["key"]);
            }
            print_lines(keys);
        }
    }
    Ok(())
}

pub(crate) fn command_remote(dir: PathBuf, action: RemoteAction) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    match action {
        RemoteAction::Default => println!("{}", ws.default_remote()?),
        RemoteAction::SetDefault { name } => {
            ws.set_default_remote(&name)?;
            println!("Default remote set to '{name}'");
        }
    }
    Ok(())
}

pub(crate) fn command_queue(dir: PathBuf, action: QueueAction, headers: bool) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    match action {
        QueueAction::List => {
            let queues = ws.list_queues()?;
            if headers && !queues.is_empty() {
                print_header(&["queue"]);
            }
            print_lines(queues);
        }
        QueueAction::Current => println!("{}", ws.queue()?),
        QueueAction::Create { name, can_pull } => {
            ws.create_queue(&name, Some(can_pull))?;
            println!("Created queue '{name}' (can-pull={can_pull})");
        }
        QueueAction::CanPull { name, can_pull } => {
            let can_pull = parse_bool_arg(&can_pull, "can-pull")?;
            ws.set_queue_can_pull(&name, can_pull)?;
            println!("Set queue '{name}' can-pull={can_pull}");
        }
        QueueAction::Delete { name, remote } => {
            let deleted = ws.delete_queue(&name)?;
            if deleted {
                println!("Deleted queue '{name}'");
            } else {
                println!("Queue '{name}' did not exist");
            }
            if deleted
                && let Some(remote_arg) = remote
                && let Some(r) = effective_remote(&ws, Some(remote_arg))?
            {
                ws.git_delete_ref(&r, &queue::refname(&name))?;
            }
        }
        QueueAction::Switch { name } => return resolve_and_switch_queue(&ws, name),
    }
    Ok(())
}

fn parse_bool_arg(value: &str, label: &str) -> Result<bool> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(errors::Error::Parse(format!(
            "{label} must be 'true' or 'false'"
        ))),
    }
}

const NEW_NS_SENTINEL: &str = "<new>";

pub(crate) fn resolve_and_switch_namespace(ws: &Workspace, name: Option<String>) -> Result<()> {
    let target = match name {
        Some(n) => n,
        None => pick_with_new(&ws.list_namespaces()?, &ws.namespace()?, "namespace")?,
    };
    ws.switch_namespace(&target)?;
    println!("Switched to namespace '{target}'");
    Ok(())
}

fn resolve_and_switch_queue(ws: &Workspace, name: Option<String>) -> Result<()> {
    let target = match name {
        Some(n) => n,
        None => {
            let picked = pick_with_new(&ws.list_queues()?, &ws.queue()?, "queue")?;
            if !ws.list_queues()?.iter().any(|q| q == &picked) {
                ws.create_queue(&picked, None)?;
            }
            picked
        }
    };
    ws.switch_queue(&target)?;
    println!("Switched to queue '{target}'");
    Ok(())
}

fn pick_with_new(existing: &[String], current: &str, label: &str) -> Result<String> {
    let entries = picker_entries(existing, current);
    let picked = fzf::select::<_, String, _>(entries, [format!("--prompt={label}> ")])?
        .ok_or_else(|| errors::Error::Parse(format!("No {label} selected")))?;
    let picked = strip_picker_marker(&picked);
    if picked == NEW_NS_SENTINEL {
        let name = prompt_line(&format!("New {label} name: "))?;
        if name.is_empty() {
            return Err(errors::Error::Parse(format!("Empty {label} name")));
        }
        Ok(name)
    } else {
        Ok(picked.to_string())
    }
}

/// Build the fzf input lines: every existing entry (active marked with
/// `* `, others with `  `) plus a trailing `<new>` sentinel for creating
/// one on the fly. The active entry is always present even when no refs
/// have been written yet.
fn picker_entries(existing: &[String], current: &str) -> Vec<String> {
    let mut entries: Vec<String> = existing
        .iter()
        .map(|n| {
            if n == current {
                format!("* {n}")
            } else {
                format!("  {n}")
            }
        })
        .collect();
    if !existing.iter().any(|n| n == current) {
        entries.insert(0, format!("* {current}"));
    }
    entries.push(NEW_NS_SENTINEL.to_string());
    entries
}

fn strip_picker_marker(s: &str) -> &str {
    s.strip_prefix("* ")
        .or_else(|| s.strip_prefix("  "))
        .unwrap_or(s)
}

fn prompt_line(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    io::stderr().flush()?;
    let mut s = String::new();
    io::stdin().read_line(&mut s)?;
    Ok(s.trim_end_matches(['\n', '\r']).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_time_breakpoints() {
        assert_eq!(relative_time(0), "0s ago");
        assert_eq!(relative_time(59), "59s ago");
        assert_eq!(relative_time(60), "1m ago");
        assert_eq!(relative_time(3599), "59m ago");
        assert_eq!(relative_time(3600), "1h ago");
        assert_eq!(relative_time(86_399), "23h ago");
        assert_eq!(relative_time(86_400), "1d ago");
        assert_eq!(relative_time(30 * 86_400), "1mo ago");
        assert_eq!(relative_time(365 * 86_400), "1y ago");
    }

    #[test]
    fn picker_marks_current_and_appends_sentinel() {
        let entries = picker_entries(&["alpha".to_string(), "tsk".to_string()], "tsk");
        assert_eq!(entries, vec!["  alpha", "* tsk", "<new>"]);
    }

    #[test]
    fn picker_includes_current_when_missing_from_list() {
        let entries = picker_entries(&[], "tsk");
        assert_eq!(entries, vec!["* tsk", "<new>"]);
    }

    #[test]
    fn strip_marker_handles_all_prefixes() {
        assert_eq!(strip_picker_marker("* tsk"), "tsk");
        assert_eq!(strip_picker_marker("  alpha"), "alpha");
        assert_eq!(strip_picker_marker("<new>"), "<new>");
    }
}
