use crate::errors::Result;
use crate::object::{self, StableId};
use crate::workspace::{CLOSED_ON_KEY, Id, LogCommit, Workspace};
use crate::{namespace, queue};
use clap::{Args, Parser};
use git2::{Oid, Patch, Repository};
use pulldown_cmark::{CowStr, Event, Options, Parser as MarkdownParser, html};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::str::FromStr as _;
use url::{Url, form_urlencoded};

const PAGE_SIZE: usize = 25;

#[derive(Args, Clone)]
pub struct ServeArgs {
    /// Address to bind.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// Port to bind.
    #[arg(short, long, default_value_t = 7935)]
    port: u16,
}

#[derive(Parser)]
#[command(version, about = "Host a read-only HTTP browser for tsk")]
struct ServeCli {
    /// Override the tsk root directory.
    #[arg(short = 'C', env = "TSK_ROOT", value_name = "DIR")]
    dir: Option<PathBuf>,
    #[command(flatten)]
    args: ServeArgs,
}

pub fn run() -> Result<()> {
    let cli = ServeCli::parse();
    let dir = cli.dir.unwrap_or(std::env::current_dir()?);
    serve(dir, cli.args)
}

pub fn serve(dir: PathBuf, args: ServeArgs) -> Result<()> {
    let ws = Workspace::from_path(dir)?;
    let bind = format!("{}:{}", args.host, args.port);
    let listener = TcpListener::bind(&bind)?;
    eprintln!("Serving tsk read-only browser at http://{bind}/");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(err) = handle_connection(&ws, stream) {
                    eprintln!("request failed: {err}");
                }
            }
            Err(err) => eprintln!("connection failed: {err}"),
        }
    }
    Ok(())
}

fn handle_connection(ws: &Workspace, mut stream: TcpStream) -> Result<()> {
    let mut buffer = [0_u8; 8192];
    let n = stream.read(&mut buffer)?;
    if n == 0 {
        return Ok(());
    }
    let request = String::from_utf8_lossy(&buffer[..n]);
    let Some(line) = request.lines().next() else {
        return Ok(());
    };
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    if method != "GET" && method != "HEAD" {
        return write_response(
            &mut stream,
            405,
            "Method Not Allowed",
            "text/plain; charset=utf-8",
            "read-only server supports GET and HEAD\n",
            method == "HEAD",
        );
    }
    let body = match render_path(ws, target) {
        Ok(Rendered::Html(html)) => {
            return write_response(
                &mut stream,
                200,
                "OK",
                "text/html; charset=utf-8",
                &html,
                method == "HEAD",
            );
        }
        Ok(Rendered::Redirect(location)) => {
            return write_redirect(&mut stream, &location, method == "HEAD");
        }
        Ok(Rendered::NotFound(message)) => message,
        Err(err) => format!("{}\n", err),
    };
    write_response(
        &mut stream,
        404,
        "Not Found",
        "text/plain; charset=utf-8",
        &body,
        method == "HEAD",
    )
}

enum Rendered {
    Html(String),
    Redirect(String),
    NotFound(String),
}

fn render_path(ws: &Workspace, target: &str) -> Result<Rendered> {
    let page = page_from_target(target);
    let path = target.split('?').next().unwrap_or("/");
    let path = path.trim_end_matches('/').trim_start_matches('/');
    if path.is_empty() {
        return Ok(Rendered::Redirect(format!("/queues/{}", ws.queue()?)));
    }
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        ["queues"] => render_queues(ws).map(Rendered::Html),
        ["queues", name, "log"] => render_queue_log(ws, name, page).map(Rendered::Html),
        ["queues", name] => render_queue(ws, name, page).map(Rendered::Html),
        ["namespaces"] => render_namespaces(ws).map(Rendered::Html),
        ["namespaces", name, "log"] => render_namespace_log(ws, name, page).map(Rendered::Html),
        ["namespaces", name] => render_namespace(ws, name, page).map(Rendered::Html),
        ["commits", oid] => render_commit(ws, oid).map(Rendered::Html),
        ["properties"] => render_properties(ws, page).map(Rendered::Html),
        ["properties", key] => render_property(ws, key, page).map(Rendered::Html),
        ["tasks", stable, "log"] => render_task_log(ws, stable, page).map(Rendered::Html),
        ["tasks", stable] => render_task(ws, stable).map(Rendered::Html),
        _ => Ok(Rendered::NotFound("not found\n".to_string())),
    }
}

fn render_queues(ws: &Workspace) -> Result<String> {
    let mut names = ws.list_queues()?;
    let active_queue = ws.queue()?;
    include_current(&mut names, &active_queue);
    let mut items = String::new();
    for name in names {
        let marker = if name == active_queue {
            " <small>active</small>"
        } else {
            ""
        };
        items.push_str(&format!(
            "<li><a href=\"/queues/{0}\">{0}</a>{1}</li>",
            h(&name),
            marker
        ));
    }
    page(ws, "Queues", &format!("<h1>Queues</h1><ul>{items}</ul>"))
}

fn render_queue(ws: &Workspace, name: &str, page_num: usize) -> Result<String> {
    queue::validate_name(name)?;
    let repo = repo(ws)?;
    let q = queue::read(&repo, name)?;
    let bindings = all_bindings(ws, &repo)?;
    let page_slice = paginate(&q.index, page_num);
    let mut rows = String::new();
    for (offset, stable) in page_slice.items.iter().enumerate() {
        let title = title_for(&repo, stable)?;
        rows.push_str(&table_row([
            (page_slice.start + offset + 1).to_string(),
            binding_links(bindings.get(stable)),
            task_anchor(stable, h(&title)),
            render_task_commit(&repo, stable),
        ]));
    }
    if rows.is_empty() {
        rows.push_str(&empty_table_row(4, "No tasks"));
    }
    let inbox = if q.inbox.is_empty() {
        "<p><em>Inbox empty</em></p>".to_string()
    } else {
        let mut items = String::new();
        for key in q.inbox_order {
            let Some(stable) = q.inbox.get(&key) else {
                continue;
            };
            items.push_str(&format!(
                "<li>{}: {}</li>",
                h(&key),
                task_anchor(stable, h(&title_for(&repo, stable)?))
            ));
        }
        format!("<ul>{items}</ul>")
    };
    let pagination = pagination_nav(&format!("/queues/{}", h(name)), &page_slice);
    page(
        ws,
        &format!("Queue {name}"),
        &format!(
            "<h1>Queue {}</h1><p><a href=\"/queues/{}/log\">Log</a></p>\
             <p>can-pull: <code>{}</code></p>\
             {pagination}\
             <table><thead><tr><th>#</th><th>Binding</th><th>Title</th><th>Stable</th></tr></thead><tbody>{rows}</tbody></table>\
             {pagination}\
             <h2>Inbox</h2>{inbox}",
            h(name),
            h(name),
            q.can_pull
        ),
    )
}

fn render_queue_log(ws: &Workspace, name: &str, page: usize) -> Result<String> {
    queue::validate_name(name)?;
    render_log_page(
        ws,
        &format!("Queue {name} Log"),
        &format!("Queue {} Log", h(name)),
        &format!("/queues/{}", h(name)),
        &format!("Queue {}", h(name)),
        &format!("/queues/{}/log", h(name)),
        ws.log_queue(name)?,
        page,
    )
}

fn render_namespaces(ws: &Workspace) -> Result<String> {
    let mut names = ws.list_namespaces()?;
    let active_namespace = ws.namespace()?;
    include_current(&mut names, &active_namespace);
    let mut items = String::new();
    for name in names {
        let marker = if name == active_namespace {
            " <small>active</small>"
        } else {
            ""
        };
        items.push_str(&format!(
            "<li><a href=\"/namespaces/{0}\">{0}</a>{1}</li>",
            h(&name),
            marker
        ));
    }
    page(
        ws,
        "Namespaces",
        &format!("<h1>Namespaces</h1><ul>{items}</ul>"),
    )
}

fn render_namespace(ws: &Workspace, name: &str, page_num: usize) -> Result<String> {
    namespace::validate_name(name)?;
    let repo = repo(ws)?;
    let ns = namespace::read(&repo, name)?;
    let mut mapping: Vec<_> = ns.mapping.into_iter().collect();
    mapping.reverse();
    let page_slice = paginate(&mapping, page_num);
    let mut rows = String::new();
    for (human, stable) in page_slice.items {
        rows.push_str(&table_row([
            format!("{}-{} ", h(name), human),
            task_anchor(&stable, h(&title_for(&repo, &stable)?)),
            render_task_commit(&repo, &stable),
        ]));
    }
    if rows.is_empty() {
        rows.push_str(&empty_table_row(3, "No tasks"));
    }
    let pagination = pagination_nav(&format!("/namespaces/{}", h(name)), &page_slice);
    page(
        ws,
        &format!("Namespace {name}"),
        &format!(
            "<h1>Namespace {}</h1><p><a href=\"/namespaces/{}/log\">Log</a></p>\
             {pagination}\
             <table><thead><tr><th>ID</th><th>Title</th><th>Stable</th></tr></thead><tbody>{rows}</tbody></table>\
             {pagination}",
            h(name),
            h(name)
        ),
    )
}

fn render_namespace_log(ws: &Workspace, name: &str, page: usize) -> Result<String> {
    namespace::validate_name(name)?;
    render_log_page(
        ws,
        &format!("Namespace {name} Log"),
        &format!("Namespace {} Log", h(name)),
        &format!("/namespaces/{}", h(name)),
        &format!("Namespace {}", h(name)),
        &format!("/namespaces/{}/log", h(name)),
        ws.log_namespace(name)?,
        page,
    )
}

struct PropertyRow {
    id: Id,
    stable: StableId,
    title: String,
}

fn render_properties(ws: &Workspace, page_num: usize) -> Result<String> {
    let repo = repo(ws)?;
    let namespace_name = ws.namespace()?;
    let ns = namespace::read(&repo, &namespace_name)?;
    let mut keys = BTreeSet::new();
    for (_human, stable) in ns.mapping {
        let Some(task) = object::read(&repo, &stable)? else {
            continue;
        };
        for key in task.properties.into_keys() {
            keys.insert(key);
        }
    }
    let entries: Vec<_> = keys.into_iter().collect();

    let page_slice = paginate(&entries, page_num);
    let mut items = String::new();
    for key in page_slice.items {
        items.push_str(&format!(
            "<li><a href=\"/properties/{}\"><code>{}</code></a></li>",
            property_path_segment(key),
            h(key)
        ));
    }
    if items.is_empty() {
        items.push_str("<li><em>No properties</em></li>");
    }
    let pagination = pagination_nav("/properties", &page_slice);
    page(
        ws,
        "Properties",
        &format!(
            "<h1>Properties</h1><p class=\"meta\">Namespace <code>{}</code></p>\
             {pagination}\
             <ul>{items}</ul>\
             {pagination}",
            h(&namespace_name)
        ),
    )
}

fn render_property(ws: &Workspace, key: &str, page_num: usize) -> Result<String> {
    let key = decode_path_segment(key);
    let repo = repo(ws)?;
    let namespace_name = ws.namespace()?;
    let ns = namespace::read(&repo, &namespace_name)?;
    let mut entries = Vec::new();
    for (human, stable) in ns.mapping {
        let Some(task) = object::read(&repo, &stable)? else {
            continue;
        };
        if task.properties.contains_key(&key) {
            entries.push(PropertyRow {
                id: Id(human),
                stable: stable.clone(),
                title: task.title().to_string(),
            });
        }
    }
    entries.sort_by(|a, b| a.id.cmp(&b.id));

    let page_slice = paginate(&entries, page_num);
    let mut items = String::new();
    for row in page_slice.items {
        items.push_str(&format!(
            "<li>{} {}</li>",
            task_anchor(&row.stable, row.id.to_string()),
            h(&row.title)
        ));
    }
    if items.is_empty() {
        items.push_str("<li><em>No tasks</em></li>");
    }
    let href = format!("/properties/{}", property_path_segment(&key));
    let pagination = pagination_nav(&href, &page_slice);
    page(
        ws,
        &format!("Property {key}"),
        &format!(
            "<h1>Property <code>{}</code></h1>\
             <p><a href=\"/properties\">Back to Properties</a></p>\
             <p class=\"meta\">Namespace <code>{}</code></p>\
             {pagination}\
             <ul>{items}</ul>\
             {pagination}",
            h(&key),
            h(&namespace_name)
        ),
    )
}

fn render_task(ws: &Workspace, stable: &str) -> Result<String> {
    let stable = StableId(stable.to_string());
    let repo = repo(ws)?;
    let Some(task) = object::read(&repo, &stable)? else {
        return page(
            ws,
            "Task not found",
            &format!(
                "<h1>Task not found</h1><p><code>{}</code></p>",
                h(&stable.0)
            ),
        );
    };
    let bindings = all_bindings(ws, &repo)?;
    let mut props = String::new();
    for (key, values) in &task.properties {
        let rendered_values = values
            .iter()
            .map(|v| render_property_value(ws, &repo, key, v))
            .collect::<Result<Vec<_>>>()?
            .join(", ");
        props.push_str(&table_row([h(key), rendered_values]));
    }
    if props.is_empty() {
        props.push_str(&empty_table_row(2, "No properties"));
    }
    let (content_class, rendered_content) = render_task_content(ws, &repo, &task.content)?;
    let body = format!(
        "<h1>{}</h1>\
         <p class=\"meta\">Stable {}</p>\
         <p><a href=\"/tasks/{}/log\">Log</a></p>\
         <p>Bindings: {}</p>\
         <h2>Content</h2><div class=\"{content_class}\">{rendered_content}</div>\
         <h2>Properties</h2>\
         <table><thead><tr><th>Key</th><th>Values</th></tr></thead><tbody>{props}</tbody></table>",
        h(task.title()),
        render_task_commit(&repo, &stable),
        h(&stable.0),
        binding_links(bindings.get(&stable))
    );
    page(ws, task.title(), &body)
}

fn render_task_log(ws: &Workspace, stable: &str, page: usize) -> Result<String> {
    let stable = StableId(stable.to_string());
    let repo = repo(ws)?;
    let title = title_for(&repo, &stable)?;
    render_log_page(
        ws,
        &format!("{title} Log"),
        &format!("Task Log: {}", h(&title)),
        &format!("/tasks/{}", h(&stable.0)),
        &title,
        &format!("/tasks/{}/log", h(&stable.0)),
        ws.log_ref(&stable.refname())?,
        page,
    )
}

fn render_commit(ws: &Workspace, oid: &str) -> Result<String> {
    let repo = repo(ws)?;
    let Ok(oid) = Oid::from_str(oid) else {
        return page(
            ws,
            "Commit not found",
            &format!("<h1>Commit not found</h1><p><code>{}</code></p>", h(oid)),
        );
    };
    let Ok(commit) = repo.find_commit(oid) else {
        return page(
            ws,
            "Commit not found",
            &format!(
                "<h1>Commit not found</h1><p><code>{}</code></p>",
                h(&oid.to_string())
            ),
        );
    };
    let author = commit.author();
    let author_name = author.name().unwrap_or("unknown");
    let author_email = author.email().unwrap_or("");
    let summary = commit.summary().ok().flatten().unwrap_or("<no summary>");
    let message = commit.message().unwrap_or("");
    let mut parents = String::new();
    for parent in commit.parents() {
        let parent_oid = parent.id().to_string();
        parents.push_str(&format!(
            "<li><a href=\"/commits/{0}\"><code>{1}</code></a></li>",
            h(&parent_oid),
            h(&parent_oid[..parent_oid.len().min(12)])
        ));
    }
    if parents.is_empty() {
        parents.push_str("<li><em>none</em></li>");
    }
    let diff = render_commit_diff(&repo, &commit)?;
    page(
        ws,
        &format!("Commit {}", &oid.to_string()[..12]),
        &format!(
            "<h1>Commit <code>{}</code></h1>\
             <p class=\"meta\">{} &lt;{}&gt; ({})</p>\
             <h2>{}</h2>\
             <pre>{}</pre>\
             <h2>Parents</h2><ul>{parents}</ul>\
             <h2>Changes</h2>{diff}",
            h(&oid.to_string()),
            h(author_name),
            h(author_email),
            h(&format_unix(commit.time().seconds())),
            h(summary),
            h(message)
        ),
    )
}

fn render_commit_diff(repo: &Repository, commit: &git2::Commit<'_>) -> Result<String> {
    let new_tree = commit.tree()?;
    let old_tree = commit
        .parent(0)
        .ok()
        .map(|parent| parent.tree())
        .transpose()?;
    let diff = repo.diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), None)?;
    if diff.deltas().len() == 0 {
        return Ok("<p><em>No file changes.</em></p>".to_string());
    }

    let mut out = String::from("<div class=\"commit-diff\">");
    for index in 0..diff.deltas().len() {
        let delta = diff.get_delta(index).expect("diff delta index");
        let old_path = delta.old_file().path().map(|p| p.to_string_lossy());
        let new_path = delta.new_file().path().map(|p| p.to_string_lossy());
        let display_path = new_path
            .as_deref()
            .or(old_path.as_deref())
            .unwrap_or("unknown");
        let patch = Patch::from_diff(&diff, index)?;
        let stats = patch.as_ref().map(Patch::line_stats).transpose()?;
        let stat = stats
            .map(|(_, additions, deletions)| {
                format!(
                    "<span class=\"diff-stat\"><ins>+{additions}</ins> <del>-{deletions}</del></span>"
                )
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "<section class=\"diff-file\"><header><code>{}</code>{stat}</header>",
            h(display_path)
        ));

        let Some(patch) = patch else {
            out.push_str("<p class=\"diff-binary\"><em>Binary file changed</em></p></section>");
            continue;
        };
        out.push_str("<div class=\"diff-scroll\"><table aria-label=\"File diff\"><tbody>");
        for hunk_index in 0..patch.num_hunks() {
            let (hunk, line_count) = patch.hunk(hunk_index)?;
            out.push_str(&format!(
                "<tr class=\"diff-hunk\"><td colspan=\"3\"><code>{}</code></td></tr>",
                h(&String::from_utf8_lossy(hunk.header()).trim_end())
            ));
            for line_index in 0..line_count {
                let line = patch.line_in_hunk(hunk_index, line_index)?;
                let (class, marker) = match line.origin() {
                    '+' => ("diff-add", "+"),
                    '-' => ("diff-del", "-"),
                    '\\' => ("diff-note", "\\"),
                    _ => ("diff-context", " "),
                };
                let content = String::from_utf8_lossy(line.content());
                let content = content.strip_suffix('\n').unwrap_or(&content);
                let content = content.strip_suffix('\r').unwrap_or(content);
                out.push_str(&format!(
                    "<tr class=\"{class}\"><td class=\"diff-line-no\">{}</td>\
                     <td class=\"diff-line-no\">{}</td><td class=\"diff-code\"><code>{marker}{}</code></td></tr>",
                    line.old_lineno().map(|n| n.to_string()).unwrap_or_default(),
                    line.new_lineno().map(|n| n.to_string()).unwrap_or_default(),
                    h(content)
                ));
            }
        }
        out.push_str("</tbody></table></div></section>");
    }
    out.push_str("</div>");
    Ok(out)
}

fn render_log_page(
    ws: &Workspace,
    title: &str,
    heading: &str,
    back_href: &str,
    back_label: &str,
    page_href: &str,
    commits: Vec<LogCommit>,
    page_num: usize,
) -> Result<String> {
    let page_slice = paginate(&commits, page_num);
    let mut rows = String::new();
    for commit in page_slice.items {
        let short = &commit.oid[..commit.oid.len().min(8)];
        rows.push_str(&table_row([
            commit_anchor(&commit.oid, format!("<code>{}</code>", h(short))),
            h(&commit.summary),
            h(&commit.author),
            h(&format_unix(commit.timestamp)),
        ]));
    }
    if rows.is_empty() {
        rows.push_str(&empty_table_row(4, "No commits"));
    }
    let pagination = pagination_nav(page_href, &page_slice);
    page(
        ws,
        title,
        &format!(
            "<h1>{heading}</h1><p><a href=\"{}\">Back to {}</a></p>\
             {pagination}\
             <table><thead><tr><th>Commit</th><th>Summary</th><th>Author</th><th>When</th></tr></thead><tbody>{rows}</tbody></table>\
             {pagination}",
            h(back_href),
            h(back_label)
        ),
    )
}

fn table_row<const N: usize>(cells: [String; N]) -> String {
    format!(
        "<tr>{}</tr>",
        cells
            .into_iter()
            .map(|cell| format!("<td>{cell}</td>"))
            .collect::<String>()
    )
}

fn empty_table_row(colspan: usize, label: &str) -> String {
    format!(
        "<tr><td colspan=\"{colspan}\"><em>{}</em></td></tr>",
        h(label)
    )
}

fn task_anchor(stable: &StableId, label_html: String) -> String {
    anchor(&format!("/tasks/{}", stable.0), label_html)
}

fn commit_anchor(oid: &str, label_html: String) -> String {
    anchor(&format!("/commits/{oid}"), label_html)
}

fn anchor(href: &str, label_html: String) -> String {
    format!("<a href=\"{}\">{label_html}</a>", h(href))
}

fn render_task_content(
    ws: &Workspace,
    repo: &Repository,
    input: &str,
) -> Result<(&'static str, String)> {
    if looks_like_markdown(input) {
        Ok((
            "task-content task-content-markdown",
            render_markdown(ws, repo, input)?,
        ))
    } else {
        Ok((
            "task-content task-content-plain",
            render_tsk_markup(ws, repo, input)?,
        ))
    }
}

fn looks_like_markdown(input: &str) -> bool {
    input.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("# ")
            || trimmed.starts_with("## ")
            || trimmed.starts_with("### ")
            || trimmed.starts_with("```")
            || trimmed.starts_with("- ")
            || trimmed.starts_with("* ")
            || is_ordered_list_item(trimmed)
    }) || input.contains("**")
}

fn render_markdown(ws: &Workspace, repo: &Repository, input: &str) -> Result<String> {
    let options = Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES;
    let parser = MarkdownParser::new_ext(input, options);
    let mut events = Vec::new();
    for event in parser {
        match event {
            Event::Text(text) => push_markdown_text_events(ws, repo, &text, &mut events)?,
            Event::Html(raw) | Event::InlineHtml(raw) => {
                events.push(Event::Text(CowStr::Boxed(raw.to_string().into_boxed_str())));
            }
            other => events.push(other),
        }
    }
    let mut out = String::new();
    html::push_html(&mut out, events.into_iter());
    Ok(out)
}

fn push_markdown_text_events<'a>(
    ws: &Workspace,
    repo: &Repository,
    text: &str,
    out: &mut Vec<Event<'a>>,
) -> Result<()> {
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        if let Some((html, consumed)) = render_internal_link(ws, repo, rest)? {
            out.push(Event::Html(CowStr::Boxed(html.into_boxed_str())));
            i += consumed;
            continue;
        }

        let next_link = rest.find("[[").unwrap_or(rest.len());
        if next_link > 0 {
            out.push(Event::Text(CowStr::Boxed(
                rest[..next_link].to_string().into_boxed_str(),
            )));
            i += next_link;
        } else {
            let Some(ch) = rest.chars().next() else {
                break;
            };
            out.push(Event::Text(CowStr::Boxed(ch.to_string().into_boxed_str())));
            i += ch.len_utf8();
        }
    }
    Ok(())
}

fn is_ordered_list_item(input: &str) -> bool {
    let Some(dot) = input.find(". ") else {
        return false;
    };
    dot > 0 && input[..dot].chars().all(|c| c.is_ascii_digit())
}

fn render_tsk_markup(ws: &Workspace, repo: &Repository, input: &str) -> Result<String> {
    let mut out = String::new();
    let mut i = 0;
    while i < input.len() {
        let rest = &input[i..];
        if let Some((html, consumed)) = render_internal_link(ws, repo, rest)? {
            out.push_str(&html);
            i += consumed;
            continue;
        }
        if let Some((html, consumed)) = render_markdown_link(rest) {
            out.push_str(&html);
            i += consumed;
            continue;
        }
        if let Some((html, consumed)) = render_raw_link(rest) {
            out.push_str(&html);
            i += consumed;
            continue;
        }
        if let Some((html, consumed)) = render_delimited(rest, '!', "strong") {
            out.push_str(&html);
            i += consumed;
            continue;
        }
        if let Some((html, consumed)) = render_delimited(rest, '*', "em") {
            out.push_str(&html);
            i += consumed;
            continue;
        }
        if let Some((html, consumed)) = render_delimited(rest, '_', "u") {
            out.push_str(&html);
            i += consumed;
            continue;
        }
        if let Some((html, consumed)) = render_delimited(rest, '~', "s") {
            out.push_str(&html);
            i += consumed;
            continue;
        }
        if let Some((html, consumed)) = render_delimited(rest, '=', "mark") {
            out.push_str(&html);
            i += consumed;
            continue;
        }
        if let Some((html, consumed)) = render_delimited(rest, '`', "code") {
            out.push_str(&html);
            i += consumed;
            continue;
        }
        let Some(ch) = rest.chars().next() else {
            break;
        };
        out.push_str(&h(&ch.to_string()));
        i += ch.len_utf8();
    }
    Ok(out)
}

fn render_property_value(
    ws: &Workspace,
    repo: &Repository,
    key: &str,
    value: &str,
) -> Result<String> {
    if key == CLOSED_ON_KEY
        && let Some(html) = render_git_commit(repo, value)
    {
        return Ok(html);
    }
    render_tsk_markup(ws, repo, value)
}

fn render_git_commit(repo: &Repository, value: &str) -> Option<String> {
    let oid = Oid::from_str(value).ok()?;
    repo.find_commit(oid).ok()?;
    Some(commit_link(&oid.to_string(), value))
}

fn render_task_commit(repo: &Repository, stable: &StableId) -> String {
    let stable_html = oid_code(&stable.0);
    let Some(commit_oid) = repo
        .find_reference(&stable.refname())
        .ok()
        .and_then(|reference| reference.target())
    else {
        return stable_html;
    };
    if repo.find_commit(commit_oid).is_err() {
        return stable_html;
    }
    commit_link(&commit_oid.to_string(), &stable.0)
}

fn commit_link(commit_oid: &str, label_oid: &str) -> String {
    format!(
        "<a href=\"/commits/{}\">{}</a>",
        h(commit_oid),
        oid_code(label_oid)
    )
}

fn oid_code(label_oid: &str) -> String {
    let short = &label_oid[..label_oid.len().min(12)];
    format!("<code title=\"{}\">{}</code>", h(label_oid), h(short))
}

fn render_internal_link(
    ws: &Workspace,
    repo: &Repository,
    input: &str,
) -> Result<Option<(String, usize)>> {
    if !input.starts_with("[[") {
        return Ok(None);
    }
    let Some(end) = input[2..].find("]]").map(|idx| idx + 2) else {
        return Ok(None);
    };
    let target = &input[2..end];
    let consumed = end + 2;
    let valid_ident = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    };
    let html = if let Ok(id) = Id::from_str(target) {
        task_id_link(repo, &ws.namespace()?, id, target)?
    } else if let Some((ns, rest)) = target.split_once('/')
        && valid_ident(ns)
        && let Ok(id) = Id::from_str(rest)
    {
        task_id_link(repo, ns, id, target)?
    } else if let Some((prefix, id)) = target.split_once('-')
        && valid_ident(prefix)
        && id.parse::<u32>().is_ok()
    {
        format!("<span class=\"foreign-link\">{}</span>", h(target))
    } else {
        h(&input[..consumed])
    };
    Ok(Some((html, consumed)))
}

fn task_id_link(repo: &Repository, namespace_name: &str, id: Id, label: &str) -> Result<String> {
    Ok(match namespace::lookup(repo, namespace_name, id.0)? {
        Some(stable) => format!(
            "<a href=\"/tasks/{}\" class=\"task-link\">{}</a>",
            h(&stable.0),
            h(label)
        ),
        None => format!("<span class=\"missing-link\">{}</span>", h(label)),
    })
}

fn render_markdown_link(input: &str) -> Option<(String, usize)> {
    if !input.starts_with('[') || input.starts_with("[[") {
        return None;
    }
    let text_end = input.find("](")?;
    let url_start = text_end + 2;
    let url_end = input[url_start..].find(')')? + url_start;
    let text = &input[1..text_end];
    let url = &input[url_start..url_end];
    if Url::parse(url).is_err() {
        return None;
    }
    Some((
        format!(
            "<a href=\"{}\" rel=\"noreferrer\">{}</a>",
            h(url),
            render_inline_plain(text)
        ),
        url_end + 1,
    ))
}

fn render_raw_link(input: &str) -> Option<(String, usize)> {
    if !input.starts_with('<') {
        return None;
    }
    let end = input.find('>')?;
    let url = &input[1..end];
    if Url::parse(url).is_err() {
        return None;
    }
    Some((
        format!("<a href=\"{}\" rel=\"noreferrer\">{}</a>", h(url), h(url)),
        end + 1,
    ))
}

fn render_delimited(input: &str, delimiter: char, tag: &str) -> Option<(String, usize)> {
    if !input.starts_with(delimiter) {
        return None;
    }
    let content_start = delimiter.len_utf8();
    let end = input[content_start..].find(delimiter)? + content_start;
    if end == content_start {
        return None;
    }
    let content = render_inline_plain(&input[content_start..end]);
    Some((
        format!("<{tag}>{content}</{tag}>"),
        end + delimiter.len_utf8(),
    ))
}

fn render_inline_plain(input: &str) -> String {
    h(input)
}

fn all_bindings(
    ws: &Workspace,
    repo: &Repository,
) -> Result<BTreeMap<StableId, Vec<(String, u32)>>> {
    let mut names = ws.list_namespaces()?;
    include_current(&mut names, &ws.namespace()?);
    let mut out: BTreeMap<StableId, Vec<(String, u32)>> = BTreeMap::new();
    for name in names {
        for (human, stable) in namespace::read(repo, &name)?.mapping {
            out.entry(stable).or_default().push((name.clone(), human));
        }
    }
    Ok(out)
}

fn binding_links(bindings: Option<&Vec<(String, u32)>>) -> String {
    let Some(bindings) = bindings else {
        return "<em>unbound</em>".to_string();
    };
    bindings
        .iter()
        .map(|(ns, human)| format!("<a href=\"/namespaces/{0}\">{0}-{1}</a>", h(ns), human))
        .collect::<Vec<_>>()
        .join(", ")
}

fn title_for(repo: &Repository, stable: &StableId) -> Result<String> {
    Ok(object::read(repo, stable)?
        .map(|task| task.title().to_string())
        .unwrap_or_else(|| "<missing>".to_string()))
}

fn repo(ws: &Workspace) -> Result<Repository> {
    Ok(Repository::open(&ws.git_dir)?)
}

fn include_current(names: &mut Vec<String>, current: &str) {
    if !names.iter().any(|name| name == current) {
        names.push(current.to_string());
    }
    let set: BTreeSet<String> = names.drain(..).collect();
    names.extend(set);
}

fn page_from_target(target: &str) -> usize {
    let Some(query) = target.split_once('?').map(|(_, query)| query) else {
        return 1;
    };
    form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == "page")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .filter(|page| *page > 0)
        .unwrap_or(1)
}

struct PageSlice<'a, T> {
    items: &'a [T],
    page: usize,
    total_pages: usize,
    total: usize,
    start: usize,
}

fn paginate<T>(items: &[T], requested_page: usize) -> PageSlice<'_, T> {
    let total = items.len();
    let total_pages = total.div_ceil(PAGE_SIZE).max(1);
    let page = requested_page.clamp(1, total_pages);
    let start = ((page - 1) * PAGE_SIZE).min(total);
    let end = (start + PAGE_SIZE).min(total);
    PageSlice {
        items: &items[start..end],
        page,
        total_pages,
        total,
        start,
    }
}

fn pagination_nav<T>(base_href: &str, page: &PageSlice<'_, T>) -> String {
    if page.total <= PAGE_SIZE {
        return String::new();
    }
    let prev = if page.page > 1 {
        format!(
            "<a href=\"{}?page={}\">Previous</a>",
            h(base_href),
            page.page - 1
        )
    } else {
        "<span>Previous</span>".to_string()
    };
    let next = if page.page < page.total_pages {
        format!(
            "<a href=\"{}?page={}\">Next</a>",
            h(base_href),
            page.page + 1
        )
    } else {
        "<span>Next</span>".to_string()
    };
    format!(
        "<nav class=\"pagination\" aria-label=\"Pagination\"><ul>\
         <li>{prev}</li><li>Page {} of {} ({} entries)</li><li>{next}</li>\
         </ul></nav>",
        page.page, page.total_pages, page.total
    )
}

fn page(ws: &Workspace, title: &str, body: &str) -> Result<String> {
    let active_queue = ws.queue()?;
    let active_namespace = ws.namespace()?;
    Ok(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"color-scheme\" content=\"light dark\">\
         <link rel=\"stylesheet\" href=\"{}\">\
         <title>{}</title>{}</head><body>\
         <nav class=\"container-fluid site-nav\"><ul><li><strong>tsk</strong></li>\
         <li><a href=\"/queues\">Queues</a></li><li><a href=\"/namespaces\">Namespaces</a></li>\
         <li><a href=\"/properties\">Properties</a></li></ul>\
         <ul class=\"active-context\"><li>queue: <a href=\"/queues/{}\">{}</a></li>\
         <li>namespace: <a href=\"/namespaces/{}\">{}</a></li></ul></nav>\
         <main class=\"container\">{}</main></body></html>",
        PICO_CSS_URL,
        h(title),
        STYLE,
        h(&active_queue),
        h(&active_queue),
        h(&active_namespace),
        h(&active_namespace),
        body
    ))
}

const PICO_CSS_URL: &str = "https://cdn.jsdelivr.net/npm/@picocss/pico@2/css/pico.min.css";

const STYLE: &str = "<style>\
*{box-sizing:border-box}\
body{overflow-x:hidden}\
main.container{padding-inline:clamp(.75rem,4vw,1rem)}\
.site-nav{border-bottom:var(--pico-border-width) solid var(--pico-muted-border-color);gap:.5rem;overflow-x:auto}\
.site-nav ul{flex-wrap:wrap;gap:.25rem .75rem;min-width:0}\
.site-nav li{min-width:0}\
.active-context{justify-content:flex-end}\
h1,h2,p,li,td,th{overflow-wrap:anywhere}\
td,th{vertical-align:top}\
table{display:block;max-width:100%;overflow-x:auto;white-space:nowrap}\
.task-content{overflow-wrap:anywhere}\
.task-content-plain{white-space:pre-wrap}\
.task-content-markdown pre{padding:1rem;overflow:auto}\
.task-content-markdown table{white-space:normal}\
.meta{color:var(--pico-muted-color)}\
.commit-diff{display:grid;gap:1rem;margin-bottom:2rem}\
.diff-file{border:var(--pico-border-width) solid var(--pico-muted-border-color);border-radius:var(--pico-border-radius);overflow:hidden}\
.diff-file>header{display:flex;justify-content:space-between;gap:1rem;padding:.65rem .85rem;background:var(--pico-card-sectioning-background-color)}\
.diff-stat{white-space:nowrap}.diff-stat ins{color:#2f9e44}.diff-stat del{color:#e03131}\
.diff-scroll{overflow-x:auto}.diff-scroll table{display:table;width:100%;margin:0;border:0;white-space:pre}\
.diff-scroll td{border:0;padding:0 .55rem;line-height:1.5;font-family:var(--pico-font-family-monospace);font-size:.875rem}\
.diff-line-no{width:1%;min-width:3.5rem;text-align:right;user-select:none;color:var(--pico-muted-color);border-right:1px solid var(--pico-muted-border-color)!important}\
.diff-code{width:100%}.diff-code code,.diff-hunk code{padding:0;background:transparent;color:inherit}\
.diff-add{background:color-mix(in srgb,#2f9e44 18%,transparent);color:color-mix(in srgb,#2f9e44 75%,var(--pico-color))}\
.diff-del{background:color-mix(in srgb,#e03131 18%,transparent);color:color-mix(in srgb,#e03131 75%,var(--pico-color))}\
.diff-note{color:var(--pico-muted-color)}\
.diff-hunk{background:color-mix(in srgb,#228be6 15%,transparent);color:color-mix(in srgb,#228be6 70%,var(--pico-color))}\
.diff-hunk td{padding:.35rem .55rem}.diff-binary{margin:0;padding:.85rem}\
.pagination ul{align-items:center;gap:.5rem;flex-wrap:wrap}\
.pagination li{margin:0}\
@media (max-width:700px){\
.site-nav{display:block;padding-block:.5rem}\
.site-nav ul{justify-content:flex-start;margin:0}\
.site-nav ul+ul{margin-top:.25rem}\
.active-context{font-size:.875rem}\
main.container{padding-block:1rem}\
h1{font-size:1.6rem}\
h2{font-size:1.25rem}\
.pagination ul{justify-content:space-between}\
}\
</style>";

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

fn h(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn property_path_segment(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for byte in key.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn decode_path_segment(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2]))
        {
            out.push(high << 4 | low);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git_init(p: &std::path::Path) {
        let repo = Repository::init(p).expect("git init");
        let mut config = repo.config().expect("git config");
        for (key, value) in [("user.name", "Test"), ("user.email", "t@e")] {
            config.set_str(key, value).expect("set git config");
        }
    }

    fn fresh_workspace() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        run_git_init(dir.path());
        Workspace::init(dir.path().to_path_buf()).unwrap();
        let ws = Workspace::from_path(dir.path().to_path_buf()).unwrap();
        (dir, ws)
    }

    fn create_git_commit(dir: &std::path::Path, message: &str) -> String {
        let repo = Repository::open(dir).unwrap();
        let blob = repo.blob(message.as_bytes()).unwrap();
        let mut builder = repo.treebuilder(None).unwrap();
        builder.insert("file.txt", blob, 0o100644).unwrap();
        let tree_id = builder.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("Test", "t@e").unwrap();
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
        let parents: Vec<_> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .unwrap()
            .to_string()
    }

    #[test]
    fn page_shell_includes_mobile_layout_rules() {
        let (_dir, ws) = fresh_workspace();

        let html = render_queues(&ws).unwrap();

        assert!(
            html.contains(
                "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">"
            ),
            "page should use the device viewport: {html}"
        );
        assert!(
            html.contains("class=\"container-fluid site-nav\""),
            "top nav should expose responsive styling hook: {html}"
        );
        assert!(
            html.contains("class=\"active-context\""),
            "active queue/namespace nav should expose responsive styling hook: {html}"
        );
        assert!(
            html.contains("table{display:block;max-width:100%;overflow-x:auto"),
            "wide tables should scroll within the viewport: {html}"
        );
        assert!(
            html.contains("@media (max-width:700px)"),
            "page shell should include narrow viewport rules: {html}"
        );
    }

    #[test]
    fn task_page_renders_links_in_property_values() {
        let (_dir, ws) = fresh_workspace();
        let task = ws.new_task("linked props".into(), "".into()).unwrap();
        let id = task.id;
        let stable = task.stable.clone();
        ws.push_task(task).unwrap();
        ws.add_property_value(id.into(), "url", "[site](https://example.com/path?q=1&v=2)")
            .unwrap();
        ws.add_property_value(id.into(), "task", "[[tsk-1]]")
            .unwrap();
        ws.add_property_value(id.into(), "raw", "<b>not html</b>")
            .unwrap();

        let html = render_task(&ws, &stable.0).unwrap();

        assert!(
            html.contains(
                "<a href=\"https://example.com/path?q=1&amp;v=2\" rel=\"noreferrer\">site</a>"
            ),
            "external link should render in property value: {html}"
        );
        assert!(
            html.contains(&format!(
                "<a href=\"/tasks/{}\" class=\"task-link\">tsk-1</a>",
                h(&stable.0)
            )),
            "task link should render in property value: {html}"
        );
        assert!(
            html.contains("&lt;b&gt;not html&lt;/b&gt;"),
            "plain html-looking property value should stay escaped: {html}"
        );
    }

    #[test]
    fn closed_on_property_renders_git_commit() {
        let (dir, ws) = fresh_workspace();
        let closed_on = create_git_commit(dir.path(), "implement feature");
        let short = &closed_on[..12];
        let task = ws.new_task("closed task".into(), "".into()).unwrap();
        let id = task.id;
        let stable = task.stable.clone();
        ws.push_task(task).unwrap();
        ws.drop_with_closed_on(id.into(), Some(closed_on.clone()))
            .unwrap();

        let task_html = render_task(&ws, &stable.0).unwrap();
        assert!(
            task_html.contains(&format!(
                "<a href=\"/commits/{closed_on}\"><code title=\"{closed_on}\">{short}</code></a>"
            )),
            "task page should render closed-on as a commit page link: {task_html}"
        );
        assert!(
            !task_html.contains("implement feature") && !task_html.contains("Test ("),
            "task page should not render commit details inline: {task_html}"
        );

        let properties_html = render_properties(&ws, 1).unwrap();
        assert!(
            properties_html.contains(&format!(
                "<a href=\"/properties/{CLOSED_ON_KEY}\"><code>{CLOSED_ON_KEY}</code></a>"
            )),
            "properties page should link closed-on key: {properties_html}"
        );
        assert!(
            !properties_html.contains(&closed_on) && !properties_html.contains("implement feature"),
            "properties page should not render property values or commit details inline: {properties_html}"
        );

        let commit_html = render_commit(&ws, &closed_on).unwrap();
        assert!(
            commit_html.contains("<h2>implement feature</h2>"),
            "commit page should render commit details: {commit_html}"
        );
    }

    #[test]
    fn commit_page_renders_syntax_highlighted_diff() {
        let (dir, ws) = fresh_workspace();
        create_git_commit(dir.path(), "old & line\n");
        let commit = create_git_commit(dir.path(), "new <line>\n");

        let html = render_commit(&ws, &commit).unwrap();

        assert!(
            html.contains("<section class=\"diff-file\">")
                && html.contains("<code>file.txt</code>"),
            "commit page should render a file diff: {html}"
        );
        assert!(
            html.contains("class=\"diff-del\"")
                && html.contains("-old &amp; line")
                && html.contains("class=\"diff-add\"")
                && html.contains("+new &lt;line&gt;"),
            "changed lines should be escaped and syntax highlighted: {html}"
        );
        assert!(
            html.contains("class=\"diff-hunk\"") && html.contains("@@ -1 +1 @@"),
            "diff should include highlighted hunk headers: {html}"
        );
    }

    #[test]
    fn properties_page_lists_active_namespace_property_names() {
        let (_dir, ws) = fresh_workspace();
        let first = ws.new_task("first task".into(), "".into()).unwrap();
        let first_id = first.id;
        ws.push_task(first).unwrap();
        ws.add_property_value(first_id.into(), "priority", "high")
            .unwrap();
        ws.add_property_value(first_id.into(), "link", "[[tsk-1]]")
            .unwrap();

        ws.switch_namespace("alpha").unwrap();
        let alpha = ws.new_task("alpha task".into(), "".into()).unwrap();
        let alpha_id = alpha.id;
        ws.push_task(alpha).unwrap();
        ws.add_property_value(alpha_id.into(), "owner", "alpha")
            .unwrap();
        ws.switch_namespace("tsk").unwrap();

        let html = render_properties(&ws, 1).unwrap();
        assert!(
            html.contains("<h1>Properties</h1>"),
            "properties page should render heading: {html}"
        );
        assert!(
            html.contains("<a href=\"/properties/priority\"><code>priority</code></a>"),
            "properties page should link priority key: {html}"
        );
        assert!(
            html.contains("<a href=\"/properties/link\"><code>link</code></a>"),
            "properties page should link link key: {html}"
        );
        assert!(
            !html.contains("high") && !html.contains("first task"),
            "properties page should not list property values or task titles: {html}"
        );
        assert!(
            !html.contains("owner") && !html.contains("alpha task"),
            "properties page should only show active namespace tasks: {html}"
        );
    }

    #[test]
    fn property_page_lists_active_namespace_tasks() {
        let (_dir, ws) = fresh_workspace();
        let first = ws.new_task("first task".into(), "".into()).unwrap();
        let first_id = first.id;
        let first_stable = first.stable.clone();
        ws.push_task(first).unwrap();
        ws.add_property_value(first_id.into(), "priority", "high")
            .unwrap();
        ws.add_property_value(first_id.into(), "link", "[[tsk-1]]")
            .unwrap();

        ws.switch_namespace("alpha").unwrap();
        let alpha = ws.new_task("alpha task".into(), "".into()).unwrap();
        let alpha_id = alpha.id;
        ws.push_task(alpha).unwrap();
        ws.add_property_value(alpha_id.into(), "priority", "alpha")
            .unwrap();
        ws.switch_namespace("tsk").unwrap();

        let html = render_property(&ws, "priority", 1).unwrap();
        assert!(
            html.contains("<h1>Property <code>priority</code></h1>"),
            "property page should render heading: {html}"
        );
        assert!(
            html.contains(&format!(
                "<li><a href=\"/tasks/{}\">tsk-1</a> first task</li>",
                h(&first_stable.0)
            )),
            "property page should link matching task: {html}"
        );
        assert!(
            !html.contains("high") && !html.contains("alpha task"),
            "property page should only list active namespace tasks, not values: {html}"
        );
    }

    #[test]
    fn properties_route_renders_page() {
        let (_dir, ws) = fresh_workspace();
        let task = ws.new_task("route props".into(), "".into()).unwrap();
        let id = task.id;
        ws.push_task(task).unwrap();
        ws.set_property(id.into(), "status-note", vec!["ready".into()])
            .unwrap();

        let Rendered::Html(html) = render_path(&ws, "/properties").unwrap() else {
            panic!("properties route should render html");
        };
        assert!(
            html.contains("<h1>Properties</h1>"),
            "route should render properties page: {html}"
        );
        assert!(
            html.contains("status-note"),
            "route should include property key: {html}"
        );

        let Rendered::Html(html) = render_path(&ws, "/properties/status-note").unwrap() else {
            panic!("property route should render html");
        };
        assert!(
            html.contains("<h1>Property <code>status-note</code></h1>"),
            "property route should render property page: {html}"
        );
        assert!(
            html.contains("route props"),
            "property route should include matching task: {html}"
        );
    }

    #[test]
    fn task_page_renders_markdown_body_without_tsk_inline_false_positives() {
        let (_dir, ws) = fresh_workspace();
        let task = ws
            .new_task(
                "markdown review".into(),
                r#"## Review

The `tamper_detected_via_stable_id_check` test and `if idx == 0`
should not become tsk underline or highlight markup.

```rust
if idx == 0 {
    if content_oid.to_string() != stable_hex {
        return Err(...);
    }
}
```

- `tsk import` of an mbox you got from someone is its own
  attacker-controlled channel.

1. Decide what stable id means
   across wrapped lines.
"#
                .into(),
            )
            .unwrap();
        let stable = task.stable.clone();
        ws.push_task(task).unwrap();

        let html = render_task(&ws, &stable.0).unwrap();

        assert!(html.contains("<h2>Review</h2>"), "heading rendered: {html}");
        assert!(
            html.contains("<code>tamper_detected_via_stable_id_check</code>"),
            "inline code should protect underscores: {html}"
        );
        assert!(
            html.contains("<code>if idx == 0</code>"),
            "inline code should protect ==: {html}"
        );
        assert!(
            html.contains("<pre><code class=\"language-rust\">"),
            "fenced code should render as a block: {html}"
        );
        assert!(
            html.contains("if idx == 0 {\n"),
            "fenced code content should be preserved: {html}"
        );
        assert!(
            !html.contains("<mark>") && !html.contains("<u>"),
            "markdown code should not trigger tsk inline tags: {html}"
        );
        assert!(
            html.contains(
                "<li><code>tsk import</code> of an mbox you got from someone is its own\nattacker-controlled channel.</li>"
            ),
            "wrapped unordered list item should stay in one item: {html}"
        );
        assert!(
            html.contains("<li>Decide what stable id means\nacross wrapped lines.</li>"),
            "wrapped ordered list item should stay in one item: {html}"
        );
    }

    #[test]
    fn queue_page_links_to_log_and_log_renders_commits() {
        let (_dir, ws) = fresh_workspace();
        let task = ws.new_task("queued work".into(), "".into()).unwrap();
        let id = task.id;
        let stable = task.stable.clone();
        ws.push_task(task).unwrap();
        ws.drop(id.into()).unwrap();

        let queue_html = render_queue(&ws, "tsk", 1).unwrap();
        assert!(
            queue_html.contains("<a href=\"/queues/tsk/log\">Log</a>"),
            "queue page should link to queue log: {queue_html}"
        );

        let log_html = render_queue_log(&ws, "tsk", 1).unwrap();
        assert!(
            log_html.contains("Queue tsk Log"),
            "queue log should have a heading: {log_html}"
        );
        assert!(
            log_html.contains(&format!("drop tsk-{} {}", id.0, stable)),
            "queue log should include drop summary with both ids: {log_html}"
        );
        assert!(
            log_html.contains(&format!("push tsk-{} {}", id.0, stable)),
            "queue log should include push summary with both ids: {log_html}"
        );
    }

    #[test]
    fn namespace_page_links_to_log_and_log_renders_commits() {
        let (_dir, ws) = fresh_workspace();
        let task = ws.new_task("namespaced work".into(), "".into()).unwrap();
        ws.push_task(task).unwrap();

        let namespace_html = render_namespace(&ws, "tsk", 1).unwrap();
        assert!(
            namespace_html.contains("<a href=\"/namespaces/tsk/log\">Log</a>"),
            "namespace page should link to namespace log: {namespace_html}"
        );

        let log_html = render_namespace_log(&ws, "tsk", 1).unwrap();
        assert!(
            log_html.contains("Namespace tsk Log"),
            "namespace log should have a heading: {log_html}"
        );
        assert!(
            log_html.contains("assign-id tsk-1"),
            "namespace log should include assignment summary: {log_html}"
        );
    }

    #[test]
    fn task_page_links_to_log_and_log_renders_commits() {
        let (_dir, ws) = fresh_workspace();
        let task = ws.new_task("logged task".into(), "".into()).unwrap();
        let stable = task.stable.clone();
        ws.push_task(task).unwrap();

        let task_html = render_task(&ws, &stable.0).unwrap();
        let repo = repo(&ws).unwrap();
        let task_commit = repo
            .find_reference(&stable.refname())
            .unwrap()
            .target()
            .unwrap()
            .to_string();
        assert!(
            task_html.contains(&format!("<a href=\"/tasks/{}/log\">Log</a>", h(&stable.0))),
            "task page should link to task log: {task_html}"
        );
        assert!(
            task_html.contains(&format!(
                "<a href=\"/commits/{task_commit}\"><code title=\"{}\">{}</code></a>",
                stable.0,
                stable.short()
            )),
            "task page should render the stable id as its task commit link: {task_html}"
        );

        let log_html = render_task_log(&ws, &stable.0, 1).unwrap();
        assert!(
            log_html.contains("Task Log: logged task"),
            "task log should have a heading: {log_html}"
        );
        assert!(
            log_html.contains("create"),
            "task log should include create summary: {log_html}"
        );
        assert!(
            log_html.contains(&format!("<a href=\"/commits/{task_commit}\"><code>")),
            "task log should link commit hashes to commit pages: {log_html}"
        );
    }

    #[test]
    fn queue_tasks_paginate_when_over_threshold() {
        let (_dir, ws) = fresh_workspace();
        for n in 0..(PAGE_SIZE + 2) {
            let task = ws
                .new_task(format!("queue-task-{n:02}"), "".into())
                .unwrap();
            ws.push_task(task).unwrap();
        }

        let first = render_queue(&ws, "tsk", 1).unwrap();
        assert!(
            first.contains("Page 1 of 2"),
            "first page should show pagination: {first}"
        );
        assert!(
            first.contains("queue-task-26"),
            "newest task should be on first page: {first}"
        );
        assert!(
            !first.contains("queue-task-00"),
            "oldest task should not be on first page: {first}"
        );
        assert!(
            first.contains("<a href=\"/queues/tsk?page=2\">Next</a>"),
            "first page should link to second page: {first}"
        );

        let second = render_queue(&ws, "tsk", 2).unwrap();
        assert!(
            second.contains("Page 2 of 2"),
            "second page should show pagination: {second}"
        );
        assert!(
            second.contains("queue-task-00"),
            "oldest task should be on second page: {second}"
        );
        assert!(
            !second.contains("queue-task-26"),
            "newest task should not be on second page: {second}"
        );
        assert!(
            second.contains("<a href=\"/queues/tsk?page=1\">Previous</a>"),
            "second page should link back to first page: {second}"
        );
    }

    #[test]
    fn render_path_uses_page_query_parameter() {
        let (_dir, ws) = fresh_workspace();
        for n in 0..(PAGE_SIZE + 2) {
            let task = ws
                .new_task(format!("route-task-{n:02}"), "".into())
                .unwrap();
            ws.push_task(task).unwrap();
        }

        let Rendered::Html(html) = render_path(&ws, "/queues/tsk?page=2").unwrap() else {
            panic!("queue route should render html");
        };
        assert!(
            html.contains("Page 2 of 2"),
            "route should pass query page to renderer: {html}"
        );
        assert!(
            html.contains("route-task-00"),
            "second page should contain oldest task: {html}"
        );
    }

    #[test]
    fn namespace_tasks_paginate_when_over_threshold() {
        let (_dir, ws) = fresh_workspace();
        for n in 0..(PAGE_SIZE + 2) {
            let task = ws
                .new_task(format!("namespace-task-{n:02}"), "".into())
                .unwrap();
            ws.push_task(task).unwrap();
        }

        let first = render_namespace(&ws, "tsk", 1).unwrap();
        assert!(
            first.contains("Page 1 of 2"),
            "first page should show pagination: {first}"
        );
        assert!(
            first.contains("namespace-task-26"),
            "last namespace binding should be on first page: {first}"
        );
        assert!(
            !first.contains("namespace-task-00"),
            "first namespace binding should not be on first page: {first}"
        );

        let second = render_namespace(&ws, "tsk", 2).unwrap();
        assert!(
            second.contains("Page 2 of 2"),
            "second page should show pagination: {second}"
        );
        assert!(
            second.contains("namespace-task-00"),
            "first namespace binding should be on second page: {second}"
        );
        assert!(
            second.contains("<a href=\"/namespaces/tsk?page=1\">Previous</a>"),
            "second page should link back to first page: {second}"
        );
    }

    #[test]
    fn logs_paginate_when_over_threshold() {
        let (_dir, ws) = fresh_workspace();
        let task = ws.new_task("many log entries".into(), "".into()).unwrap();
        let id = task.id;
        let stable = task.stable.clone();
        ws.push_task(task).unwrap();
        for n in 0..(PAGE_SIZE + 2) {
            let mut task = ws.task(id.into()).unwrap();
            task.body = format!("revision {n:02}");
            ws.save_task(&task).unwrap();
        }

        let first = render_task_log(&ws, &stable.0, 1).unwrap();
        assert!(
            first.contains("Page 1 of 2"),
            "first log page should show pagination: {first}"
        );
        assert!(
            first.matches("<td>edit</td>").count() >= PAGE_SIZE,
            "first log page should contain page-size edit entries: {first}"
        );
        assert!(
            !first.contains("create tsk-1"),
            "oldest create entry should not be on first log page: {first}"
        );

        let second = render_task_log(&ws, &stable.0, 2).unwrap();
        assert!(
            second.contains("Page 2 of 2"),
            "second log page should show pagination: {second}"
        );
        assert!(
            second.contains("create tsk-1"),
            "oldest create entry should be on second log page: {second}"
        );
        assert!(
            second.contains(&format!(
                "<a href=\"/tasks/{}/log?page=1\">Previous</a>",
                h(&stable.0)
            )),
            "second log page should link back to first page: {second}"
        );
    }
}

fn write_redirect(stream: &mut TcpStream, location: &str, head: bool) -> Result<()> {
    let body = format!("redirecting to {location}\n");
    let response = format!(
        "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        location,
        if head { 0 } else { body.len() }
    );
    stream.write_all(response.as_bytes())?;
    if !head {
        stream.write_all(body.as_bytes())?;
    }
    Ok(())
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &str,
    head: bool,
) -> Result<()> {
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        if head { 0 } else { body.len() }
    );
    stream.write_all(response.as_bytes())?;
    if !head {
        stream.write_all(body.as_bytes())?;
    }
    Ok(())
}
