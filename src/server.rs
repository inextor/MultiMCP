use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
        ToolAnnotations,
    },
    service::RequestContext,
};
use tokio::sync::RwLock;

use crate::config::{CommandSpec, ServerDefinition, save_server_file};
use crate::exec::{format_output, resolve_args, run, substitute};
use crate::register::{REGISTER_TOOL_NAME, build_spec};
use crate::schema::{input_schema_for, register_command_schema};

struct State {
    file_path: PathBuf,
    server_name: String,
    instructions: Option<String>,
    allow_register: bool,
    commands: RwLock<Vec<CommandSpec>>,
}

#[derive(Clone)]
pub struct MultiMcp {
    state: Arc<State>,
}

impl MultiMcp {
    pub fn new(def: ServerDefinition, file_path: PathBuf, allow_register: bool) -> Self {
        Self {
            state: Arc::new(State {
                file_path,
                server_name: def.name,
                instructions: def.instructions,
                allow_register,
                commands: RwLock::new(def.commands),
            }),
        }
    }

    fn register_tool() -> Tool {
        let mut tool = Tool::default();
        tool.name = Cow::Borrowed(REGISTER_TOOL_NAME);
        tool.description = Some(Cow::Borrowed(
            "Register a new command as a tool on this server. The command is \
             available immediately and persisted to the server file.",
        ));
        tool.input_schema = register_command_schema();
        tool
    }

    fn tool_for(spec: &CommandSpec) -> Tool {
        let mut tool = Tool::default();
        tool.name = Cow::Owned(spec.name.clone());
        tool.description = Some(Cow::Owned(spec.description.clone()));
        tool.input_schema = input_schema_for(&spec.params);
        if spec.read_only {
            let mut annotations = ToolAnnotations::new();
            annotations.read_only_hint = Some(true);
            tool.annotations = Some(annotations);
        }
        tool
    }

    async fn handle_register(
        &self,
        params: &CallToolRequestParams,
        context: &RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let empty = Default::default();
        let args = params.arguments.as_ref().unwrap_or(&empty);
        let spec = match build_spec(args) {
            Ok(spec) => spec,
            Err(e) => return Ok(tool_error(format!("invalid command definition: {e}"))),
        };
        let mut commands = self.state.commands.write().await;
        if commands.iter().any(|c| c.name == spec.name) {
            return Ok(tool_error(format!(
                "a command named {:?} already exists",
                spec.name
            )));
        }
        commands.push(spec.clone());
        let def = ServerDefinition {
            name: self.state.server_name.clone(),
            instructions: self.state.instructions.clone(),
            commands: commands.clone(),
        };
        if let Err(e) = save_server_file(&self.state.file_path, &def) {
            commands.pop();
            return Ok(tool_error(format!(
                "registered in memory but failed to persist: {e}"
            )));
        }
        drop(commands);
        // Best effort: tell the client to re-list tools.
        let _ = context.peer.notify_tool_list_changed().await;
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "registered tool {:?}: {}\nIt is available now and was saved to {}.",
            spec.name,
            spec.description,
            self.state.file_path.display()
        ))]))
    }
}

fn tool_error(message: String) -> CallToolResult {
    eprintln!("multimcp: {message}");
    CallToolResult::error(vec![ContentBlock::text(message)])
}

impl ServerHandler for MultiMcp {
    fn get_info(&self) -> ServerConfig {
        let info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                self.state.server_name.clone(),
                env!("CARGO_PKG_VERSION"),
            ));
        match &self.state.instructions {
            Some(text) => info.with_instructions(text.clone()),
            None => info,
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let commands = self.state.commands.read().await;
        let mut tools: Vec<Tool> = commands.iter().map(Self::tool_for).collect();
        if self.state.allow_register {
            tools.push(Self::register_tool());
        }
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        if params.name.as_ref() == REGISTER_TOOL_NAME {
            if !self.state.allow_register {
                return Err(McpError::invalid_params(
                    format!("unknown tool {:?}", params.name.as_ref()),
                    None,
                ));
            }
            return self
                .handle_register(&params, &context)
                .await
                .map(CallToolResponse::from);
        }
        let spec = {
            let commands = self.state.commands.read().await;
            commands
                .iter()
                .find(|c| c.name == params.name.as_ref())
                .cloned()
        };
        let Some(spec) = spec else {
            return Err(McpError::invalid_params(
                format!("unknown tool {:?}", params.name.as_ref()),
                None,
            ));
        };
        let values = match resolve_args(&spec, params.arguments.as_ref()) {
            Ok(values) => values,
            Err(e) => {
                return Err(McpError::invalid_params(e, None));
            }
        };
        let argv = match substitute(&spec.argv, &values) {
            Ok(argv) => argv,
            Err(e) => return Ok(tool_error(e).into()),
        };
        eprintln!("multimcp: tool {:?} -> {}", spec.name, argv.join(" "));
        let output = match run(&argv, spec.timeout_secs).await {
            Ok(output) => output,
            Err(e) => return Ok(tool_error(e).into()),
        };
        if output.timed_out {
            return Ok(tool_error(format!(
                "tool {:?} timed out after {}s: {}",
                spec.name,
                spec.timeout_secs,
                argv.join(" ")
            ))
            .into());
        }
        let text = format_output(&spec.name, &argv, &output);
        if output.code == Some(0) {
            Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
        } else {
            Ok(CallToolResult::error(vec![ContentBlock::text(text)]).into())
        }
    }
}
