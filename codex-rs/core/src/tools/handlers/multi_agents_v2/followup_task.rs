use super::message_tool::FollowupTaskArgs;
use super::message_tool::MessageDeliveryMode;
use super::message_tool::MessageTargetKind;
use super::message_tool::handle_message_string_tool;
use super::*;
use crate::tools::handlers::multi_agents_spec::create_external_runtime_followup_task_tool;
use crate::tools::handlers::multi_agents_spec::create_followup_task_tool;
use codex_tools::ToolSpec;

#[derive(Clone, Copy, Default)]
enum FollowupToolKind {
    #[default]
    Reserved,
    ExternalRuntime,
}

#[derive(Default)]
pub(crate) struct Handler {
    kind: FollowupToolKind,
}

impl Handler {
    pub(crate) fn new_external_runtime() -> Self {
        Self {
            kind: FollowupToolKind::ExternalRuntime,
        }
    }
}

impl ToolExecutor<ToolInvocation> for Handler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("followup_task")
    }

    fn spec(&self) -> ToolSpec {
        match self.kind {
            FollowupToolKind::Reserved => create_followup_task_tool(),
            FollowupToolKind::ExternalRuntime => create_external_runtime_followup_task_tool(),
        }
    }

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        let kind = self.kind;
        Box::pin(async move { Self::handle_call(invocation, kind).await })
    }
}

impl Handler {
    async fn handle_call(
        invocation: ToolInvocation,
        kind: FollowupToolKind,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let arguments = function_arguments(invocation.payload.clone())?;
        let args: FollowupTaskArgs = parse_arguments(&arguments)?;
        handle_message_string_tool(
            invocation,
            MessageDeliveryMode::TriggerTurn,
            match kind {
                FollowupToolKind::Reserved => MessageTargetKind::Any,
                FollowupToolKind::ExternalRuntime => MessageTargetKind::ExternalRuntime,
            },
            args.target,
            args.message,
        )
        .await
        .map(boxed_tool_output)
    }
}

impl CoreToolRuntime for Handler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }
}
