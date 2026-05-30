use crate::errors::Result;
use crate::object::{self, StableId};
use crate::workspace::{Id, Workspace};
use crate::{namespace, queue};
use clap::{Args, Parser};
use git2::Repository;
use pulldown_cmark::{CowStr, Event, Options, Parser as MarkdownParser, html};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::str::FromStr as _;
use url::Url;

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
    let path = target.split('?').next().unwrap_or("/");
    let path = path.trim_end_matches('/').trim_start_matches('/');
    if path.is_empty() {
        return Ok(Rendered::Redirect(format!("/queues/{}", ws.queue()?)));
    }
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        ["queues"] => render_queues(ws).map(Rendered::Html),
        ["queues", name] => render_queue(ws, name).map(Rendered::Html),
        ["namespaces"] => render_namespaces(ws).map(Rendered::Html),
        ["namespaces", name] => render_namespace(ws, name).map(Rendered::Html),
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

fn render_queue(ws: &Workspace, name: &str) -> Result<String> {
    queue::validate_name(name)?;
    let repo = repo(ws)?;
    let q = queue::read(&repo, name)?;
    let bindings = all_bindings(ws, &repo)?;
    let mut rows = String::new();
    for (idx, stable) in q.index.iter().enumerate() {
        let title = title_for(&repo, stable)?;
        rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td><a href=\"/tasks/{}\">{}</a></td><td>{}</td></tr>",
            idx + 1,
            binding_links(bindings.get(stable)),
            h(&stable.0),
            h(&title),
            h(stable.short()),
        ));
    }
    if rows.is_empty() {
        rows.push_str("<tr><td colspan=\"4\"><em>No tasks</em></td></tr>");
    }
    let inbox = if q.inbox.is_empty() {
        "<p><em>Inbox empty</em></p>".to_string()
    } else {
        let mut items = String::new();
        for (key, stable) in q.inbox {
            items.push_str(&format!(
                "<li>{}: <a href=\"/tasks/{}\">{}</a></li>",
                h(&key),
                h(&stable.0),
                h(&title_for(&repo, &stable)?)
            ));
        }
        format!("<ul>{items}</ul>")
    };
    page(
        ws,
        &format!("Queue {name}"),
        &format!(
            "<h1>Queue {}</h1><p>can-pull: <code>{}</code></p>\
             <table><thead><tr><th>#</th><th>Binding</th><th>Title</th><th>Stable</th></tr></thead><tbody>{rows}</tbody></table>\
             <h2>Inbox</h2>{inbox}",
            h(name),
            q.can_pull
        ),
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

fn render_namespace(ws: &Workspace, name: &str) -> Result<String> {
    namespace::validate_name(name)?;
    let repo = repo(ws)?;
    let ns = namespace::read(&repo, name)?;
    let mut rows = String::new();
    for (human, stable) in ns.mapping {
        rows.push_str(&format!(
            "<tr><td>{}-{} </td><td><a href=\"/tasks/{}\">{}</a></td><td>{}</td></tr>",
            h(name),
            human,
            h(&stable.0),
            h(&title_for(&repo, &stable)?),
            h(stable.short())
        ));
    }
    if rows.is_empty() {
        rows.push_str("<tr><td colspan=\"3\"><em>No tasks</em></td></tr>");
    }
    page(
        ws,
        &format!("Namespace {name}"),
        &format!(
            "<h1>Namespace {}</h1>\
             <table><thead><tr><th>ID</th><th>Title</th><th>Stable</th></tr></thead><tbody>{rows}</tbody></table>",
            h(name)
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
            .map(|v| render_tsk_markup(ws, &repo, v))
            .collect::<Result<Vec<_>>>()?
            .join(", ");
        props.push_str(&format!(
            "<tr><td>{}</td><td>{}</td></tr>",
            h(key),
            rendered_values
        ));
    }
    if props.is_empty() {
        props.push_str("<tr><td colspan=\"2\"><em>No properties</em></td></tr>");
    }
    let (content_class, rendered_content) = render_task_content(ws, &repo, &task.content)?;
    let body = format!(
        "<h1>{}</h1>\
         <p class=\"meta\">Stable <code>{}</code></p>\
         <p>Bindings: {}</p>\
         <h2>Content</h2><div class=\"{content_class}\">{rendered_content}</div>\
         <h2>Properties</h2>\
         <table><thead><tr><th>Key</th><th>Values</th></tr></thead><tbody>{props}</tbody></table>",
        h(task.title()),
        h(&stable.0),
        binding_links(bindings.get(&stable))
    );
    page(ws, task.title(), &body)
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

fn page(ws: &Workspace, title: &str, body: &str) -> Result<String> {
    let active_queue = ws.queue()?;
    let active_namespace = ws.namespace()?;
    Ok(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"color-scheme\" content=\"light dark\">\
         <link rel=\"stylesheet\" href=\"{}\">\
         <title>{}</title>{}</head><body>\
         <nav class=\"container-fluid\"><ul><li><strong>tsk</strong></li>\
         <li><a href=\"/queues\">Queues</a></li><li><a href=\"/namespaces\">Namespaces</a></li></ul>\
         <ul><li>queue: <a href=\"/queues/{}\">{}</a></li>\
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
nav{border-bottom:var(--pico-border-width) solid var(--pico-muted-border-color)}\
td,th{vertical-align:top}\
.task-content-plain{white-space:pre-wrap}\
.task-content-markdown pre{padding:1rem;overflow:auto}\
.meta{color:var(--pico-muted-color)}\
</style>";

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
