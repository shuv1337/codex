use super::*;
use crate::agent::control::SpawnAgentForkMode;
use crate::agent::control::SpawnAgentOptions;
use crate::agent::next_thread_spawn_depth;
use crate::agent::role::DEFAULT_ROLE_NAME;
use crate::agent::role::apply_role_to_config;
use crate::agent::role::resolve_role_runtime_id;
use crate::agent_communication::AgentCommunicationContext;
use crate::agent_communication::AgentCommunicationKind;
use crate::tools::handlers::multi_agents::collab_tool_call_status;
use crate::tools::handlers::multi_agents_spec::SpawnAgentToolOptions;
use crate::tools::handlers::multi_agents_spec::create_external_runtime_spawn_agent_tool_v2;
use crate::tools::handlers::multi_agents_spec::create_spawn_agent_tool_v2;
use crate::tools::handlers::multi_agents_v2::message_tool::message_content;
use codex_protocol::AgentPath;
use codex_protocol::protocol::CollabAgentRef;
use codex_tools::ToolSpec;

#[derive(Clone, Copy, Default)]
enum SpawnToolKind {
    #[default]
    Reserved,
    ExternalRuntime,
}

#[derive(Default)]
pub(crate) struct Handler {
    options: SpawnAgentToolOptions,
    kind: SpawnToolKind,
}

impl Handler {
    pub(crate) fn new(options: SpawnAgentToolOptions) -> Self {
        Self {
            options,
            kind: SpawnToolKind::Reserved,
        }
    }

    pub(crate) fn new_external_runtime(options: SpawnAgentToolOptions) -> Self {
        Self {
            options,
            kind: SpawnToolKind::ExternalRuntime,
        }
    }
}

impl ToolExecutor<ToolInvocation> for Handler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("spawn_agent")
    }

    fn spec(&self) -> ToolSpec {
        match self.kind {
            SpawnToolKind::Reserved => create_spawn_agent_tool_v2(self.options.clone()),
            SpawnToolKind::ExternalRuntime => {
                create_external_runtime_spawn_agent_tool_v2(self.options.clone())
            }
        }
    }

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        let kind = self.kind;
        let hide_agent_metadata = self.options.hide_agent_type_model_reasoning;
        Box::pin(async move {
            handle_spawn_agent(invocation, kind, hide_agent_metadata)
                .await
                .map(boxed_tool_output)
        })
    }
}

async fn handle_spawn_agent(
    invocation: ToolInvocation,
    kind: SpawnToolKind,
    hide_agent_metadata: bool,
) -> Result<SpawnAgentResult, FunctionCallError> {
    let ToolInvocation {
        session,
        turn,
        step_context,
        payload,
        call_id,
        ..
    } = invocation;
    let arguments = function_arguments(payload)?;
    let args: SpawnAgentArgs = parse_arguments(&arguments)?;
    let role_name = args
        .agent_type
        .as_deref()
        .map(str::trim)
        .filter(|role| !role.is_empty());
    let fork_mode = args.fork_mode(kind)?;

    let message = message_content(args.message)?;
    let prompt = message.clone();
    let session_source = turn.session_source.clone();
    let child_depth = next_thread_spawn_depth(&session_source);
    let mut config =
        build_agent_spawn_config(&session.get_base_instructions().await, turn.as_ref())?;
    if matches!(kind, SpawnToolKind::ExternalRuntime) {
        let role_name = role_name.ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "runtime_agents.spawn_agent requires `agent_type`".to_string(),
            )
        })?;
        let runtime_id = resolve_role_runtime_id(&config, Some(role_name))
            .map_err(FunctionCallError::RespondToModel)?;
        if runtime_id.is_codex() {
            return Err(FunctionCallError::RespondToModel(format!(
                "runtime_agents.spawn_agent requires an external runtime role; agent_type `{role_name}` resolves to native Codex"
            )));
        }
    }
    if let Some(service_tier) = args.service_tier.as_ref() {
        config.service_tier = Some(service_tier.clone());
    }
    if matches!(fork_mode, Some(SpawnAgentForkMode::FullHistory)) {
        reject_full_fork_spawn_overrides(
            role_name,
            args.model.as_deref(),
            args.reasoning_effort.clone(),
        )?;
    } else {
        apply_requested_spawn_agent_model_overrides(
            &session,
            turn.as_ref(),
            &mut config,
            args.model.as_deref(),
            args.reasoning_effort.clone(),
        )
        .await?;
        apply_role_to_config(&mut config, role_name)
            .await
            .map_err(FunctionCallError::RespondToModel)?;
    }
    apply_spawn_agent_service_tier(
        &session,
        &mut config,
        turn.config.service_tier.as_deref(),
        args.service_tier.as_deref(),
    )
    .await?;
    apply_spawn_agent_runtime_overrides(&mut config, turn.as_ref())?;

    let spawn_source = thread_spawn_source(
        session.thread_id,
        &turn.session_source,
        child_depth,
        role_name,
        Some(args.task_name.clone()),
    )?;
    let new_agent_path = spawn_source.get_agent_path().ok_or_else(|| {
        FunctionCallError::RespondToModel(
            "spawned agent is missing a canonical task name".to_string(),
        )
    })?;
    let author = turn
        .session_source
        .get_agent_path()
        .unwrap_or_else(AgentPath::root);
    let communication = match kind {
        SpawnToolKind::Reserved => {
            communication_from_tool_message(author, new_agent_path.clone(), message)
        }
        SpawnToolKind::ExternalRuntime => InterAgentCommunication::new(
            author,
            new_agent_path.clone(),
            Vec::new(),
            message,
            /*trigger_turn*/ true,
        ),
    };
    let context = AgentCommunicationContext::new(AgentCommunicationKind::Spawn, session.thread_id);
    let spawned_agent = Box::pin(
        session
            .services
            .agent_control
            .spawn_agent_with_communication(
                config,
                communication,
                context,
                Some(spawn_source),
                SpawnAgentOptions {
                    fork_parent_spawn_call_id: fork_mode.as_ref().map(|_| call_id.clone()),
                    fork_mode,
                    parent_thread_id: Some(session.thread_id),
                    environments: Some(step_context.environments.to_selections()),
                },
            ),
    )
    .await
    .map_err(collab_spawn_error)?;
    let new_thread_id = spawned_agent.thread_id;
    let agent_snapshot = session
        .services
        .agent_control
        .get_agent_config_snapshot(new_thread_id)
        .await;
    let nickname = agent_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.session_source.get_nickname())
        .or(spawned_agent.metadata.agent_nickname);
    match kind {
        SpawnToolKind::Reserved => {
            emit_sub_agent_activity(
                &session,
                &turn,
                SubAgentActivityItem {
                    id: call_id,
                    agent_thread_id: new_thread_id,
                    agent_path: new_agent_path.clone(),
                    kind: SubAgentActivityKind::Started,
                },
            )
            .await;
        }
        SpawnToolKind::ExternalRuntime => {
            let status = spawned_agent.status;
            let model = agent_snapshot
                .as_ref()
                .map(|snapshot| snapshot.model.clone())
                .unwrap_or_default();
            let reasoning_effort = agent_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.reasoning_effort.clone())
                .unwrap_or_default();
            let agent_role = agent_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.session_source.get_agent_role())
                .or_else(|| role_name.map(str::to_string));
            session
                .emit_turn_item_completed(
                    &turn,
                    TurnItem::CollabAgentToolCall(CollabAgentToolCallItem {
                        id: call_id,
                        tool: CollabAgentTool::SpawnAgent,
                        status: collab_tool_call_status(&status, Some(new_thread_id)),
                        sender_thread_id: session.thread_id,
                        receiver_thread_ids: vec![new_thread_id],
                        receiver_agents: vec![CollabAgentRef {
                            thread_id: new_thread_id,
                            agent_nickname: nickname.clone(),
                            agent_role,
                        }],
                        prompt: Some(prompt),
                        model: Some(model),
                        reasoning_effort: Some(reasoning_effort),
                        agents_states: [(new_thread_id, status)].into_iter().collect(),
                    }),
                )
                .await;
        }
    }
    let role_tag = role_name.unwrap_or(DEFAULT_ROLE_NAME);
    turn.session_telemetry.counter(
        "codex.multi_agent.spawn",
        /*inc*/ 1,
        &[("role", role_tag), ("version", "v2")],
    );
    let task_name = String::from(new_agent_path);

    if hide_agent_metadata {
        Ok(SpawnAgentResult::HiddenMetadata { task_name })
    } else {
        Ok(SpawnAgentResult::WithNickname {
            task_name,
            nickname,
        })
    }
}

impl CoreToolRuntime for Handler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnAgentArgs {
    message: String,
    task_name: String,
    agent_type: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
    service_tier: Option<String>,
    fork_turns: Option<String>,
    fork_context: Option<bool>,
}

impl SpawnAgentArgs {
    fn fork_mode(
        &self,
        kind: SpawnToolKind,
    ) -> Result<Option<SpawnAgentForkMode>, FunctionCallError> {
        if self.fork_context.is_some() {
            return Err(FunctionCallError::RespondToModel(
                "fork_context is not supported in MultiAgentV2; use fork_turns instead".to_string(),
            ));
        }

        if matches!(kind, SpawnToolKind::ExternalRuntime) {
            let requested = self
                .fork_turns
                .as_deref()
                .map(str::trim)
                .filter(|fork_turns| !fork_turns.is_empty());
            if requested.is_none_or(|fork_turns| fork_turns.eq_ignore_ascii_case("none")) {
                return Ok(None);
            }
            return Err(FunctionCallError::RespondToModel(
                "runtime_agents.spawn_agent cannot fork Codex history; omit `fork_turns` or pass `none`"
                    .to_string(),
            ));
        }

        let fork_turns = self
            .fork_turns
            .as_deref()
            .map(str::trim)
            .filter(|fork_turns| !fork_turns.is_empty())
            .unwrap_or("all");

        if fork_turns.eq_ignore_ascii_case("none") {
            return Ok(None);
        }
        if fork_turns.eq_ignore_ascii_case("all") {
            return Ok(Some(SpawnAgentForkMode::FullHistory));
        }

        let last_n_turns = fork_turns.parse::<usize>().map_err(|_| {
            FunctionCallError::RespondToModel(
                "fork_turns must be `none`, `all`, or a positive integer string".to_string(),
            )
        })?;
        if last_n_turns == 0 {
            return Err(FunctionCallError::RespondToModel(
                "fork_turns must be `none`, `all`, or a positive integer string".to_string(),
            ));
        }

        Ok(Some(SpawnAgentForkMode::LastNTurns(last_n_turns)))
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum SpawnAgentResult {
    WithNickname {
        task_name: String,
        nickname: Option<String>,
    },
    HiddenMetadata {
        task_name: String,
    },
}

impl ToolOutput for SpawnAgentResult {
    fn log_preview(&self) -> String {
        tool_output_json_text(self, "spawn_agent")
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        tool_output_response_item(call_id, payload, self, Some(true), "spawn_agent")
    }

    fn code_mode_result(&self, _payload: &ToolPayload) -> JsonValue {
        tool_output_code_mode_result(self, "spawn_agent")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_with_fork_turns(fork_turns: Option<&str>) -> SpawnAgentArgs {
        SpawnAgentArgs {
            message: "task".to_string(),
            task_name: "task".to_string(),
            agent_type: Some("pi_worker".to_string()),
            model: None,
            reasoning_effort: None,
            service_tier: None,
            fork_turns: fork_turns.map(str::to_string),
            fork_context: None,
        }
    }

    #[test]
    fn external_runtime_spawn_defaults_to_no_fork() {
        assert!(
            args_with_fork_turns(None)
                .fork_mode(SpawnToolKind::ExternalRuntime)
                .expect("omitted fork_turns should be accepted")
                .is_none()
        );
        assert!(
            args_with_fork_turns(Some("none"))
                .fork_mode(SpawnToolKind::ExternalRuntime)
                .expect("fork_turns=none should be accepted")
                .is_none()
        );
    }

    #[test]
    fn external_runtime_spawn_rejects_history_fork() {
        let error = args_with_fork_turns(Some("all"))
            .fork_mode(SpawnToolKind::ExternalRuntime)
            .expect_err("external runtime history forks must be rejected");
        assert_eq!(
            error,
            FunctionCallError::RespondToModel(
                "runtime_agents.spawn_agent cannot fork Codex history; omit `fork_turns` or pass `none`"
                    .to_string()
            )
        );
    }

    #[test]
    fn reserved_spawn_keeps_full_history_default() {
        assert!(matches!(
            args_with_fork_turns(None)
                .fork_mode(SpawnToolKind::Reserved)
                .expect("reserved default should remain valid"),
            Some(SpawnAgentForkMode::FullHistory)
        ));
    }
}
