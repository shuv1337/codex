use crate::codex_thread::CodexThread;
use crate::state::ActiveTurn;
use crate::tools::context::ToolPayload;
use crate::tools::router::ToolCall;
use crate::tools::router::ToolCallSource;
use crate::turn_diff_tracker::TurnDiffTracker;
use codex_extension_api::AgentRuntimeHostToolDefinition;
use codex_extension_api::AgentRuntimeToolCall;
use codex_extension_api::AgentRuntimeToolResult;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::Op;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const EXTERNAL_HOST_TOOL_NAMES: [&str; 2] = ["exec_command", "apply_patch"];

/// A native Codex session used only as the execution and event context for an external runtime.
/// It never submits model input; Pi owns sampling while Codex owns tools and approvals.
pub(crate) struct ExternalHostTools {
    thread: Arc<CodexThread>,
    cancellation: Mutex<CancellationToken>,
    turn_id: Mutex<String>,
}

impl ExternalHostTools {
    pub(crate) fn new(thread: Arc<CodexThread>) -> Self {
        Self {
            thread,
            cancellation: Mutex::new(CancellationToken::new()),
            turn_id: Mutex::new("external-host-tools".to_string()),
        }
    }

    pub(crate) async fn definitions(&self) -> CodexResult<Vec<AgentRuntimeHostToolDefinition>> {
        let (_turn, _step, router) = self.tool_context("external-host-tools-spec").await?;
        EXTERNAL_HOST_TOOL_NAMES
            .iter()
            .filter_map(|name| router.registered_tool_spec(&ToolName::plain(*name)))
            .filter_map(host_tool_definition)
            .collect()
    }

    pub(crate) async fn execute(&self, call: AgentRuntimeToolCall) -> AgentRuntimeToolResult {
        let call_id = call.call_id.clone();
        match self.execute_inner(call).await {
            Ok(output) => AgentRuntimeToolResult {
                call_id,
                output,
                is_error: false,
            },
            Err(error) => {
                let message = error.to_string();
                tracing::warn!(%call_id, %message, "Codex-hosted external runtime tool failed");
                AgentRuntimeToolResult {
                    call_id,
                    output: json!({
                        "content": [{ "type": "text", "text": message }],
                        "details": { "error": message }
                    }),
                    is_error: true,
                }
            }
        }
    }

    async fn execute_inner(&self, call: AgentRuntimeToolCall) -> CodexResult<Value> {
        if !EXTERNAL_HOST_TOOL_NAMES.contains(&call.tool_name.as_str()) {
            return Err(CodexErr::InvalidRequest(format!(
                "external runtime requested unavailable host tool `{}`",
                call.tool_name
            )));
        }
        let turn_id = self.turn_id.lock().await.clone();
        let (_turn, step, router) = self.tool_context(&turn_id).await?;
        let payload = host_tool_payload(&call.tool_name, call.arguments)?;
        let cancellation = self.cancellation.lock().await.child_token();
        let result = router
            .dispatch_tool_call_with_code_mode_result(
                Arc::clone(&self.thread.codex.session),
                step,
                cancellation,
                Arc::new(Mutex::new(TurnDiffTracker::new())),
                ToolCall {
                    tool_name: ToolName::plain(&call.tool_name),
                    call_id: call.call_id,
                    payload,
                },
                ToolCallSource::Direct,
            )
            .await
            .map_err(|error| CodexErr::Fatal(format!("host tool execution failed: {error}")))?
            .code_mode_result();
        let text = match &result {
            Value::String(text) => text.clone(),
            _ => serde_json::to_string(&result).unwrap_or_else(|_| result.to_string()),
        };
        Ok(json!({
            "content": [{ "type": "text", "text": text }],
            "details": result
        }))
    }

    async fn tool_context(
        &self,
        turn_id: &str,
    ) -> CodexResult<(
        Arc<crate::session::turn_context::TurnContext>,
        Arc<crate::session::step_context::StepContext>,
        Arc<crate::tools::router::ToolRouter>,
    )> {
        let turn = self
            .thread
            .codex
            .session
            .new_default_turn_with_sub_id(turn_id.to_string())
            .await;
        let step = self
            .thread
            .codex
            .session
            .capture_step_context(Arc::clone(&turn))
            .await;
        let cancellation = self.cancellation.lock().await.child_token();
        let router = crate::session::turn::built_tools(
            self.thread.codex.session.as_ref(),
            step.as_ref(),
            &cancellation,
        )
        .await?;
        Ok((turn, step, router))
    }

    pub(crate) async fn next_event(&self) -> CodexResult<Event> {
        self.thread.next_event().await
    }

    pub(crate) async fn submit_response(&self, op: Op) -> CodexResult<String> {
        self.thread.submit(op).await
    }

    pub(crate) async fn reset_for_turn(&self, turn_id: &str) {
        *self.cancellation.lock().await = CancellationToken::new();
        *self.turn_id.lock().await = turn_id.to_string();
        *self.thread.codex.session.active_turn.lock().await = Some(ActiveTurn::default());
    }

    pub(crate) async fn interrupt(&self) {
        self.cancellation.lock().await.cancel();
    }

    pub(crate) async fn shutdown(&self) -> CodexResult<()> {
        self.thread.shutdown_and_wait().await
    }
}

fn host_tool_definition(spec: ToolSpec) -> Option<CodexResult<AgentRuntimeHostToolDefinition>> {
    match spec {
        ToolSpec::Function(tool) if EXTERNAL_HOST_TOOL_NAMES.contains(&tool.name.as_str()) => Some(
            serde_json::to_value(tool.parameters)
                .map(|input_schema| AgentRuntimeHostToolDefinition {
                    name: tool.name,
                    description: tool.description,
                    input_schema,
                })
                .map_err(|error| {
                    CodexErr::Fatal(format!("failed to serialize host tool schema: {error}"))
                }),
        ),
        ToolSpec::Freeform(tool) if tool.name == "apply_patch" => {
            Some(Ok(AgentRuntimeHostToolDefinition {
                name: tool.name,
                description:
                    "Apply a patch through Codex's native sandbox, approval, and diff pipeline."
                        .to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "patch": {
                            "type": "string",
                            "description": tool.description
                        }
                    },
                    "required": ["patch"],
                    "additionalProperties": false
                }),
            }))
        }
        _ => None,
    }
}

fn host_tool_payload(tool_name: &str, arguments: Value) -> CodexResult<ToolPayload> {
    if tool_name == "apply_patch" {
        let patch = arguments
            .get("patch")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CodexErr::InvalidRequest(
                    "Codex-hosted apply_patch requires a string `patch` argument".to_string(),
                )
            })?;
        return Ok(ToolPayload::Custom {
            input: patch.to_string(),
        });
    }

    let arguments = serde_json::to_string(&arguments).map_err(|error| {
        CodexErr::InvalidRequest(format!("failed to encode host tool arguments: {error}"))
    })?;
    Ok(ToolPayload::Function { arguments })
}
