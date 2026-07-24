use crate::client::PiRuntimeClient;
use crate::client::protocol_error;
use crate::proto;
use codex_extension_api::AgentRuntimeError;
use codex_extension_api::AgentRuntimeErrorKind;
use codex_extension_api::AgentRuntimeFuture;
use codex_extension_api::AgentRuntimeHostToolDefinition;
use codex_extension_api::AgentRuntimeId;
use codex_extension_api::AgentRuntimeOperation;
use codex_extension_api::AgentRuntimePersistence;
use codex_extension_api::AgentRuntimeResumeRequest;
use codex_extension_api::AgentRuntimeSpawnRequest;
use codex_extension_api::AgentRuntimeThread;
use codex_extension_api::AgentRuntimeToolCall;
use codex_extension_api::AgentRuntimeToolResult;
use codex_protocol::ThreadId;
use codex_protocol::items::AgentMessageContent;
use codex_protocol::items::AgentMessageItem;
use codex_protocol::items::ReasoningItem;
use codex_protocol::items::TurnItem;
use codex_protocol::models::MessagePhase;
use codex_protocol::protocol::AgentMessageContentDeltaEvent;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::ItemStartedEvent;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ReasoningContentDeltaEvent;
use codex_protocol::protocol::TokenCountEvent;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TokenUsageInfo;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;
use codex_protocol::user_input::UserInput;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use tokio::sync::Mutex;
use tokio::sync::mpsc;

pub(crate) async fn spawn_thread(
    client: Arc<PiRuntimeClient>,
    runtime_id: AgentRuntimeId,
    request: AgentRuntimeSpawnRequest,
) -> Result<Arc<dyn AgentRuntimeThread>, AgentRuntimeError> {
    let session_id = request.thread_id.to_string();
    let registration = client.register_session(&session_id).await?;
    let runtime_config = request.runtime_config;
    let agent_dir = config_string(&runtime_config, "agent_dir").unwrap_or_default();
    let command = proto::runtime_request::Command::Spawn(proto::SpawnRequest {
        codex_thread_id: session_id.clone(),
        cwd: request.cwd.as_path().display().to_string(),
        agent_dir: agent_dir.clone(),
        session_dir: config_string(&runtime_config, "session_dir").unwrap_or_default(),
        provider: config_string(&runtime_config, "provider").unwrap_or_default(),
        model: config_string(&runtime_config, "model")
            .or(request.model)
            .unwrap_or_default(),
        thinking_level: config_string(&runtime_config, "thinking_level").unwrap_or_default(),
        host_tools: encode_host_tools(request.host_tools)?,
    });
    let response = match client.request(&session_id, command).await {
        Ok(response) => response,
        Err(error) => {
            client.unregister_session(&session_id).await;
            return Err(error);
        }
    };
    let spawned = expect_spawned(response)?;
    Ok(Arc::new(PiRuntimeThread::new(
        client,
        runtime_id,
        request.thread_id,
        registration,
        spawned,
        agent_dir,
    )))
}

pub(crate) async fn resume_thread(
    client: Arc<PiRuntimeClient>,
    runtime_id: AgentRuntimeId,
    request: AgentRuntimeResumeRequest,
) -> Result<Arc<dyn AgentRuntimeThread>, AgentRuntimeError> {
    let session_id = request.thread_id.to_string();
    let registration = client.register_session(&session_id).await?;
    let agent_dir = request
        .state
        .metadata
        .get("agent_dir")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let command = proto::runtime_request::Command::Resume(proto::ResumeRequest {
        codex_thread_id: session_id.clone(),
        session_locator: request.state.session_locator,
        cwd_override: request.cwd.as_path().display().to_string(),
        host_tools: encode_host_tools(request.host_tools)?,
        agent_dir: agent_dir.clone(),
    });
    let response = match client.request(&session_id, command).await {
        Ok(response) => response,
        Err(error) => {
            client.unregister_session(&session_id).await;
            return Err(error);
        }
    };
    let spawned = expect_spawned(response)?;
    Ok(Arc::new(PiRuntimeThread::new(
        client,
        runtime_id,
        request.thread_id,
        registration,
        spawned,
        agent_dir,
    )))
}

struct PiRuntimeThread {
    client: Arc<PiRuntimeClient>,
    runtime_id: AgentRuntimeId,
    thread_id: ThreadId,
    session_id: String,
    session_locator: String,
    provider: String,
    model: String,
    thinking_level: String,
    agent_dir: String,
    events: Mutex<ThreadEvents>,
    host_tools: Mutex<mpsc::UnboundedReceiver<proto::HostToolRequest>>,
    pending_host_tools: Mutex<HashMap<String, String>>,
    active_turn_id: Mutex<Option<String>>,
    interrupt_pending: AtomicBool,
    status: RwLock<AgentStatus>,
    token_usage: RwLock<Option<TokenUsageInfo>>,
}

struct ThreadEvents {
    receiver: mpsc::UnboundedReceiver<proto::SessionEvent>,
    queued: VecDeque<Event>,
    messages: HashMap<String, MessageState>,
}

#[derive(Default)]
struct MessageState {
    text: String,
    reasoning: String,
    agent_started: bool,
    reasoning_started: bool,
    reasoning_completed: bool,
}

impl PiRuntimeThread {
    fn new(
        client: Arc<PiRuntimeClient>,
        runtime_id: AgentRuntimeId,
        thread_id: ThreadId,
        registration: crate::client::RegisteredSession,
        spawned: proto::SpawnedSession,
        agent_dir: String,
    ) -> Self {
        Self {
            client,
            runtime_id,
            thread_id,
            session_id: thread_id.to_string(),
            session_locator: spawned.session_locator,
            provider: spawned.provider,
            model: spawned.model,
            thinking_level: spawned.thinking_level,
            agent_dir,
            events: Mutex::new(ThreadEvents {
                receiver: registration.events,
                queued: VecDeque::new(),
                messages: HashMap::new(),
            }),
            host_tools: Mutex::new(registration.host_tools),
            pending_host_tools: Mutex::new(HashMap::new()),
            active_turn_id: Mutex::new(None),
            interrupt_pending: AtomicBool::new(false),
            status: RwLock::new(AgentStatus::PendingInit),
            token_usage: RwLock::new(None),
        }
    }

    async fn submit_turn(
        &self,
        submission_id: String,
        text: String,
    ) -> Result<(), AgentRuntimeError> {
        *self.active_turn_id.lock().await = Some(submission_id);
        self.interrupt_pending.store(false, Ordering::Release);
        let running = matches!(self.read_status()?, AgentStatus::Running);
        let command = if running {
            proto::runtime_request::Command::FollowUp(proto::FollowUpRequest { text })
        } else {
            proto::runtime_request::Command::Prompt(proto::PromptRequest { text })
        };
        self.client.request(&self.session_id, command).await?;
        Ok(())
    }

    fn read_status(&self) -> Result<AgentStatus, AgentRuntimeError> {
        self.status
            .read()
            .map(|status| status.clone())
            .map_err(|_| internal_error("Pi runtime status lock is poisoned"))
    }

    fn write_status(&self, status: AgentStatus) -> Result<(), AgentRuntimeError> {
        *self
            .status
            .write()
            .map_err(|_| internal_error("Pi runtime status lock is poisoned"))? = status;
        Ok(())
    }

    fn update_token_usage(
        &self,
        usage: &proto::TokenUsage,
    ) -> Result<TokenUsageInfo, AgentRuntimeError> {
        let token_usage = TokenUsage {
            input_tokens: saturating_i64(usage.input_tokens),
            cached_input_tokens: saturating_i64(usage.cache_read_tokens),
            cache_write_input_tokens: 0,
            output_tokens: saturating_i64(usage.output_tokens),
            reasoning_output_tokens: 0,
            total_tokens: saturating_i64(usage.total_tokens),
        };
        let info = TokenUsageInfo {
            total_token_usage: token_usage.clone(),
            last_token_usage: token_usage,
            model_context_window: None,
        };
        *self
            .token_usage
            .write()
            .map_err(|_| internal_error("Pi runtime token usage lock is poisoned"))? =
            Some(info.clone());
        Ok(info)
    }

    async fn translate_event(
        &self,
        state: &mut ThreadEvents,
        session_event: proto::SessionEvent,
    ) -> Result<(), AgentRuntimeError> {
        use proto::session_event::Event as PiEvent;

        let turn_id = self
            .active_turn_id
            .lock()
            .await
            .clone()
            .unwrap_or_else(|| format!("pi-turn-{}", session_event.sequence));
        let Some(event) = session_event.event else {
            return Ok(());
        };
        if self.interrupt_pending.load(Ordering::Acquire) && !matches!(&event, PiEvent::AgentEnd(_))
        {
            return Ok(());
        }
        match event {
            PiEvent::AgentStart(_) => {}
            PiEvent::TurnStart(_) => {
                self.write_status(AgentStatus::Running)?;
                state.queued.push_back(Event {
                    id: turn_id.clone(),
                    msg: EventMsg::TurnStarted(TurnStartedEvent {
                        turn_id,
                        trace_id: None,
                        started_at: None,
                        model_context_window: None,
                        collaboration_mode_kind: Default::default(),
                    }),
                });
            }
            PiEvent::MessageStart(message) if message.role == "assistant" => {
                state.messages.entry(message.message_id).or_default();
            }
            PiEvent::TextDelta(delta) => {
                let message = state.messages.entry(delta.message_id.clone()).or_default();
                complete_reasoning_if_needed(
                    &mut state.queued,
                    self.thread_id,
                    &turn_id,
                    &delta.message_id,
                    message,
                );
                if !message.agent_started {
                    message.agent_started = true;
                    state.queued.push_back(item_started(
                        self.thread_id,
                        &turn_id,
                        TurnItem::AgentMessage(agent_message(&delta.message_id, String::new())),
                    ));
                }
                message.text.push_str(&delta.delta);
                state.queued.push_back(Event {
                    id: turn_id.clone(),
                    msg: EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent {
                        thread_id: self.thread_id.to_string(),
                        turn_id: turn_id.clone(),
                        item_id: delta.message_id,
                        delta: delta.delta,
                    }),
                });
            }
            PiEvent::ThinkingDelta(delta) => {
                let message = state.messages.entry(delta.message_id.clone()).or_default();
                let reasoning_id = reasoning_id(&delta.message_id);
                if !message.reasoning_started {
                    message.reasoning_started = true;
                    state.queued.push_back(item_started(
                        self.thread_id,
                        &turn_id,
                        TurnItem::Reasoning(ReasoningItem {
                            id: reasoning_id.clone(),
                            summary_text: Vec::new(),
                            raw_content: Vec::new(),
                        }),
                    ));
                }
                message.reasoning.push_str(&delta.delta);
                state.queued.push_back(Event {
                    id: turn_id.clone(),
                    msg: EventMsg::ReasoningContentDelta(ReasoningContentDeltaEvent {
                        thread_id: self.thread_id.to_string(),
                        turn_id: turn_id.clone(),
                        item_id: reasoning_id,
                        delta: delta.delta,
                        summary_index: 0,
                    }),
                });
            }
            PiEvent::MessageEnd(message_end) if message_end.role == "assistant" => {
                let mut message = state
                    .messages
                    .remove(&message_end.message_id)
                    .unwrap_or_default();
                complete_reasoning_if_needed(
                    &mut state.queued,
                    self.thread_id,
                    &turn_id,
                    &message_end.message_id,
                    &mut message,
                );
                if message.text.is_empty() && !message.agent_started {
                    return Ok(());
                }
                if !message.agent_started {
                    state.queued.push_back(item_started(
                        self.thread_id,
                        &turn_id,
                        TurnItem::AgentMessage(agent_message(
                            &message_end.message_id,
                            String::new(),
                        )),
                    ));
                }
                state.queued.push_back(item_completed(
                    self.thread_id,
                    &turn_id,
                    TurnItem::AgentMessage(agent_message(&message_end.message_id, message.text)),
                ));
            }
            PiEvent::AgentEnd(agent_end) => {
                if self.interrupt_pending.swap(false, Ordering::AcqRel) {
                    return Ok(());
                }
                let last_message = agent_end.last_assistant_text;
                self.write_status(AgentStatus::Completed(Some(last_message.clone())))?;
                *self.active_turn_id.lock().await = None;
                state.queued.push_back(Event {
                    id: turn_id.clone(),
                    msg: EventMsg::TurnComplete(TurnCompleteEvent {
                        turn_id,
                        last_agent_message: Some(last_message),
                        error: None,
                        started_at: None,
                        completed_at: None,
                        duration_ms: None,
                        time_to_first_token_ms: None,
                    }),
                });
            }
            PiEvent::Error(error) => {
                self.write_status(AgentStatus::Errored(error.message.clone()))?;
                state.queued.push_back(Event {
                    id: turn_id,
                    msg: EventMsg::Error(ErrorEvent {
                        message: format!("Pi runtime error ({}): {}", error.code, error.message),
                        codex_error_info: Some(CodexErrorInfo::Other),
                    }),
                });
            }
            PiEvent::TokenUsage(usage) => {
                let info = self.update_token_usage(&usage)?;
                state.queued.push_back(Event {
                    id: turn_id,
                    msg: EventMsg::TokenCount(TokenCountEvent {
                        info: Some(info),
                        rate_limits: None,
                    }),
                });
            }
            PiEvent::MessageStart(_)
            | PiEvent::MessageEnd(_)
            | PiEvent::ToolExecutionStart(_)
            | PiEvent::ToolExecutionUpdate(_)
            | PiEvent::ToolExecutionEnd(_)
            | PiEvent::TurnEnd(_)
            | PiEvent::QueueUpdate(_)
            | PiEvent::Compaction(_)
            | PiEvent::Retry(_) => {}
        }
        Ok(())
    }
}

impl AgentRuntimeThread for PiRuntimeThread {
    fn thread_id(&self) -> ThreadId {
        self.thread_id
    }

    fn runtime_id(&self) -> &AgentRuntimeId {
        &self.runtime_id
    }

    fn submit(&self, operation: AgentRuntimeOperation) -> AgentRuntimeFuture<'_, ()> {
        Box::pin(async move {
            match operation.op {
                Op::UserInput { items, .. } => {
                    self.submit_turn(operation.submission_id, render_user_input(items)?)
                        .await
                }
                Op::InterAgentCommunication { communication } if communication.trigger_turn => {
                    self.submit_turn(operation.submission_id, render_communication(communication))
                        .await
                }
                Op::Interrupt => {
                    let turn_id = self.active_turn_id.lock().await.take();
                    self.interrupt_pending.store(true, Ordering::Release);
                    if let Err(error) = self
                        .client
                        .request(
                            &self.session_id,
                            proto::runtime_request::Command::Interrupt(proto::InterruptRequest {}),
                        )
                        .await
                    {
                        self.interrupt_pending.store(false, Ordering::Release);
                        *self.active_turn_id.lock().await = turn_id;
                        return Err(error);
                    }
                    self.write_status(AgentStatus::Interrupted)?;
                    self.events.lock().await.queued.push_back(Event {
                        id: turn_id
                            .clone()
                            .unwrap_or_else(|| format!("pi-interrupt-{}", self.session_id)),
                        msg: EventMsg::TurnAborted(TurnAbortedEvent {
                            turn_id,
                            reason: TurnAbortReason::Interrupted,
                            started_at: None,
                            completed_at: None,
                            duration_ms: None,
                        }),
                    });
                    Ok(())
                }
                Op::Shutdown => self.shutdown().await,
                op => Err(AgentRuntimeError::new(
                    AgentRuntimeErrorKind::UnsupportedOperation,
                    format!(
                        "Pi runtime does not support Codex operation `{}`",
                        op.kind()
                    ),
                )),
            }
        })
    }

    fn next_event(&self) -> AgentRuntimeFuture<'_, Option<Event>> {
        Box::pin(async move {
            let mut state = self.events.lock().await;
            loop {
                if let Some(event) = state.queued.pop_front() {
                    return Ok(Some(event));
                }
                let Some(session_event) = state.receiver.recv().await else {
                    return Ok(None);
                };
                self.translate_event(&mut state, session_event).await?;
            }
        })
    }

    fn next_tool_call(&self) -> AgentRuntimeFuture<'_, Option<AgentRuntimeToolCall>> {
        Box::pin(async move {
            let Some(request) = self.host_tools.lock().await.recv().await else {
                return Ok(None);
            };
            if request.session_id != self.session_id {
                return Err(protocol_error(format!(
                    "Pi host tool request targeted session {}, expected {}",
                    request.session_id, self.session_id
                )));
            }
            let arguments = serde_json::from_slice(&request.arguments_json).map_err(|error| {
                protocol_error(format!(
                    "Pi host tool {} sent invalid arguments JSON: {error}",
                    request.tool_name
                ))
            })?;
            self.pending_host_tools
                .lock()
                .await
                .insert(request.tool_call_id.clone(), request.request_id);
            Ok(Some(AgentRuntimeToolCall {
                call_id: request.tool_call_id,
                tool_name: request.tool_name,
                arguments,
            }))
        })
    }

    fn submit_tool_result(&self, result: AgentRuntimeToolResult) -> AgentRuntimeFuture<'_, ()> {
        Box::pin(async move {
            let request_id = self
                .pending_host_tools
                .lock()
                .await
                .remove(&result.call_id)
                .ok_or_else(|| {
                    protocol_error(format!(
                        "Pi host tool result has no pending call {}",
                        result.call_id
                    ))
                })?;
            let result_json = serde_json::to_vec(&result.output).map_err(|error| {
                protocol_error(format!("failed to encode Pi host tool result: {error}"))
            })?;
            self.client
                .send_host_tool_result(proto::HostToolResult {
                    request_id,
                    session_id: self.session_id.clone(),
                    tool_call_id: result.call_id,
                    result_json,
                    error: result.is_error.then(|| proto::RuntimeError {
                        code: "host_tool_failed".to_string(),
                        message: result.output.to_string(),
                        retryable: false,
                        details_json: Vec::new(),
                    }),
                })
                .await
        })
    }

    fn status(&self) -> AgentRuntimeFuture<'_, AgentStatus> {
        Box::pin(async move { self.read_status() })
    }

    fn token_usage(&self) -> AgentRuntimeFuture<'_, Option<TokenUsageInfo>> {
        Box::pin(async move {
            self.token_usage
                .read()
                .map(|usage| usage.clone())
                .map_err(|_| internal_error("Pi runtime token usage lock is poisoned"))
        })
    }

    fn persistence(&self) -> AgentRuntimeFuture<'_, AgentRuntimePersistence> {
        Box::pin(async move {
            Ok(AgentRuntimePersistence {
                runtime_id: self.runtime_id.clone(),
                session_locator: self.session_locator.clone(),
                metadata: json!({
                    "provider": self.provider,
                    "model": self.model,
                    "thinking_level": self.thinking_level,
                    "agent_dir": self.agent_dir,
                }),
            })
        })
    }

    fn shutdown(&self) -> AgentRuntimeFuture<'_, ()> {
        Box::pin(async move {
            self.client
                .request(
                    &self.session_id,
                    proto::runtime_request::Command::Close(proto::CloseRequest {}),
                )
                .await?;
            self.client.unregister_session(&self.session_id).await;
            self.write_status(AgentStatus::Shutdown)?;
            let mut events = self.events.lock().await;
            events.queued.push_back(Event {
                id: self.session_id.clone(),
                msg: EventMsg::ShutdownComplete,
            });
            Ok(())
        })
    }
}

fn expect_spawned(
    response: proto::RuntimeResponse,
) -> Result<proto::SpawnedSession, AgentRuntimeError> {
    match response.result {
        Some(proto::runtime_response::Result::Spawned(spawned)) => Ok(spawned),
        _ => Err(protocol_error(
            "Pi runtime spawn did not return spawned session metadata",
        )),
    }
}

fn config_string(config: &Value, key: &str) -> Option<String> {
    config.get(key).and_then(Value::as_str).map(str::to_string)
}

fn encode_host_tools(
    definitions: Vec<AgentRuntimeHostToolDefinition>,
) -> Result<Vec<proto::HostToolDefinition>, AgentRuntimeError> {
    definitions
        .into_iter()
        .map(|definition| {
            Ok(proto::HostToolDefinition {
                name: definition.name,
                description: definition.description,
                input_schema_json: serde_json::to_vec(&definition.input_schema).map_err(
                    |error| protocol_error(format!("failed to encode host tool schema: {error}")),
                )?,
            })
        })
        .collect()
}

fn render_user_input(items: Vec<UserInput>) -> Result<String, AgentRuntimeError> {
    let mut rendered = Vec::with_capacity(items.len());
    for item in items {
        let text = match item {
            UserInput::Text { text, .. } => text,
            UserInput::Image { image_url, .. } => format!("[image]({image_url})"),
            UserInput::LocalImage { path, .. } => format!("[local image: {}]", path.display()),
            UserInput::Skill { name, path } => format!("[skill {name}: {}]", path.display()),
            UserInput::Mention { name, path } => format!("[{name}]({path})"),
            _ => {
                return Err(AgentRuntimeError::new(
                    AgentRuntimeErrorKind::UnsupportedOperation,
                    "Pi runtime received an unsupported user input item",
                ));
            }
        };
        rendered.push(text);
    }
    Ok(rendered.join("\n\n"))
}

fn render_communication(communication: InterAgentCommunication) -> String {
    communication.content
}

fn agent_message(item_id: &str, text: String) -> AgentMessageItem {
    AgentMessageItem {
        id: item_id.to_string(),
        content: if text.is_empty() {
            Vec::new()
        } else {
            vec![AgentMessageContent::Text { text }]
        },
        phase: Some(MessagePhase::FinalAnswer),
        memory_citation: None,
    }
}

fn reasoning_id(message_id: &str) -> String {
    format!("{message_id}-reasoning")
}

fn complete_reasoning_if_needed(
    events: &mut VecDeque<Event>,
    thread_id: ThreadId,
    turn_id: &str,
    message_id: &str,
    message: &mut MessageState,
) {
    if !message.reasoning_started || message.reasoning_completed {
        return;
    }
    message.reasoning_completed = true;
    events.push_back(item_completed(
        thread_id,
        turn_id,
        TurnItem::Reasoning(ReasoningItem {
            id: reasoning_id(message_id),
            summary_text: if message.reasoning.is_empty() {
                Vec::new()
            } else {
                vec![message.reasoning.clone()]
            },
            raw_content: Vec::new(),
        }),
    ));
}

fn item_started(thread_id: ThreadId, turn_id: &str, item: TurnItem) -> Event {
    Event {
        id: turn_id.to_string(),
        msg: EventMsg::ItemStarted(ItemStartedEvent {
            thread_id,
            turn_id: turn_id.to_string(),
            item,
            started_at_ms: 0,
        }),
    }
}

fn item_completed(thread_id: ThreadId, turn_id: &str, item: TurnItem) -> Event {
    Event {
        id: turn_id.to_string(),
        msg: EventMsg::ItemCompleted(ItemCompletedEvent {
            thread_id,
            turn_id: turn_id.to_string(),
            item,
            completed_at_ms: 0,
        }),
    }
}

fn saturating_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn internal_error(message: impl Into<String>) -> AgentRuntimeError {
    AgentRuntimeError::new(AgentRuntimeErrorKind::Internal, message)
}
