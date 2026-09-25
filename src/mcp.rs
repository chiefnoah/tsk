use crate::errors::{Error, Result};
use crate::{Cli, parse_id};
use clap::{CommandFactory, Parser};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt, transport::stdio};
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

const BLOCKED_TOOLS: &[&str] = &["mcp", "serv"];

#[derive(Clone)]
struct TskServer {
    dir: PathBuf,
    queue: Option<String>,
    tools: Arc<Vec<Tool>>,
}

impl TskServer {
    fn new(dir: PathBuf, queue: Option<String>) -> Self {
        Self {
            dir,
            queue,
            tools: Arc::new(cli_tools()),
        }
    }

    async fn call(&self, request: CallToolRequestParams) -> CallToolResult {
        if !self.tools.iter().any(|tool| tool.name == request.name) {
            return tool_error(format!("Unknown tsk command: {}", request.name));
        }

        let arguments = match parse_arguments(request.arguments) {
            Ok(arguments) => arguments,
            Err(message) => return tool_error(message),
        };

        let mut command = tokio::process::Command::new(match std::env::current_exe() {
            Ok(executable) => executable,
            Err(error) => return tool_error(error.to_string()),
        });
        command.arg("-C").arg(&self.dir);
        if let Some(queue) = &self.queue {
            command.arg("--queue").arg(queue);
        }
        let args = normalize_args(request.name.as_ref(), arguments.args);
        command.arg(request.name.as_ref()).args(args);
        command
            .stdin(if arguments.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("NO_COLOR", "1")
            .env("EDITOR", "false")
            .env("GIT_EDITOR", "false");

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => return tool_error(error.to_string()),
        };
        if let Some(input) = arguments.stdin
            && let Some(mut stdin) = child.stdin.take()
            && let Err(error) = stdin.write_all(input.as_bytes()).await
        {
            return tool_error(error.to_string());
        }

        match child.wait_with_output().await {
            Ok(output) => command_result(output),
            Err(error) => tool_error(error.to_string()),
        }
    }
}

impl ServerHandler for TskServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("tsk", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Use these tools to query and modify tasks. Arguments match the corresponding tsk CLI command."
                    .to_string(),
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.tools.as_ref().clone()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, McpError> {
        Ok(self.call(request).await.into())
    }
}

struct ToolArguments {
    args: Vec<String>,
    stdin: Option<String>,
}

fn parse_arguments(
    arguments: Option<Map<String, Value>>,
) -> std::result::Result<ToolArguments, String> {
    let arguments = arguments.unwrap_or_default();
    if let Some(key) = arguments
        .keys()
        .find(|key| key.as_str() != "args" && key.as_str() != "stdin")
    {
        return Err(format!("Unknown argument: {key}"));
    }
    let args = arguments.get("args").cloned().unwrap_or_else(|| json!([]));
    let args = serde_json::from_value(args).map_err(|_| "'args' must be an array of strings")?;
    let stdin = arguments
        .get("stdin")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| "'stdin' must be a string")?;

    Ok(ToolArguments { args, stdin })
}

fn cli_tools() -> Vec<Tool> {
    Cli::command()
        .get_subcommands()
        .filter(|command| !BLOCKED_TOOLS.contains(&command.get_name()))
        .map(|command| {
            let name = command.get_name().to_string();
            let help = command.clone().render_long_help().to_string();
            Tool::new(
                name,
                format!(
                    "{help}\nPass CLI arguments in `args`. Use `stdin` for piped input. Editors and interactive selectors are disabled."
                ),
                tool_schema(),
            )
        })
        .collect()
}

fn normalize_args(command: &str, args: Vec<String>) -> Vec<String> {
    if cli_parses(command, &args) {
        return args;
    }

    for (index, value) in args.iter().enumerate() {
        if parse_id(value).is_err() {
            continue;
        }

        let mut candidate = args.clone();
        candidate.splice(index..=index, ["-T".to_string(), value.clone()]);
        if cli_parses(command, &candidate) {
            return candidate;
        }
    }

    args
}

fn cli_parses(command: &str, args: &[String]) -> bool {
    Cli::try_parse_from(
        ["tsk", command]
            .into_iter()
            .map(String::from)
            .chain(args.iter().cloned()),
    )
    .is_ok()
}

fn tool_schema() -> Arc<Map<String, Value>> {
    let Value::Object(schema) = json!({
        "type": "object",
        "properties": {
            "args": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Arguments after the tsk subcommand."
            },
            "stdin": {
                "type": "string",
                "description": "Optional standard input for commands that read it."
            }
        },
        "additionalProperties": false
    }) else {
        unreachable!();
    };
    Arc::new(schema)
}

fn command_result(output: std::process::Output) -> CallToolResult {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = match (stdout.is_empty(), stderr.is_empty()) {
        (false, true) => stdout.into_owned(),
        (true, false) => stderr.into_owned(),
        (false, false) => format!("{stdout}\nstderr:\n{stderr}"),
        (true, true) => String::new(),
    };
    let content = vec![ContentBlock::text(text)];
    if output.status.success() {
        CallToolResult::success(content)
    } else {
        CallToolResult::error(content)
    }
}

fn tool_error(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.into())])
}

pub(crate) fn serve(dir: PathBuf, queue: Option<String>) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        let service = TskServer::new(dir, queue)
            .serve(stdio())
            .await
            .map_err(|error| Error::Parse(error.to_string()))?;
        service
            .waiting()
            .await
            .map(|_| ())
            .map_err(|error| Error::Parse(error.to_string()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_exclude_servers() {
        let names: Vec<_> = cli_tools()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect();

        assert!(names.contains(&"push".to_string()));
        assert!(names.contains(&"show".to_string()));
        assert!(!names.contains(&"mcp".to_string()));
        assert!(!names.contains(&"serv".to_string()));
    }

    #[test]
    fn arguments_accept_stdin() {
        let arguments = parse_arguments(Some(
            json!({ "args": ["-b", "-", "title"], "stdin": "body" })
                .as_object()
                .unwrap()
                .clone(),
        ))
        .unwrap();

        assert_eq!(arguments.args, ["-b", "-", "title"]);
        assert_eq!(arguments.stdin.as_deref(), Some("body"));
    }

    #[test]
    fn arguments_reject_unknown_fields() {
        let arguments = json!({ "command": "mcp" }).as_object().unwrap().clone();

        assert_eq!(
            parse_arguments(Some(arguments)).err().as_deref(),
            Some("Unknown argument: command")
        );
    }

    #[test]
    fn positional_task_id_becomes_selector() {
        assert_eq!(
            normalize_args("show", vec!["tsk-53".to_string()]),
            ["-T", "tsk-53"]
        );
    }

    #[test]
    fn valid_task_id_values_remain_values() {
        let args = ["add", "-T", "tsk-1", "related", "tsk-53"]
            .map(String::from)
            .to_vec();

        assert_eq!(normalize_args("prop", args.clone()), args);
    }
}
