//! Deterministic external runtime used by debug app-server integration tests.

use codex_extension_api::AgentRuntimeError;
use codex_extension_api::AgentRuntimeErrorKind;
use codex_extension_api::AgentRuntimeFuture;
use codex_extension_api::AgentRuntimeId;
use codex_extension_api::AgentRuntimeOperation;
use codex_extension_api::AgentRuntimePersistence;
use codex_extension_api::AgentRuntimeProvider;
use codex_extension_api::AgentRuntimeResumeRequest;
use codex_extension_api::AgentRuntimeSpawnRequest;
use codex_extension_api::AgentRuntimeThread;
use codex_protocol::ThreadId;
use codex_protocol::items::AgentMessageContent;
use codex_protocol::items::AgentMessageItem;
use codex_protocol::items::TurnItem;
use codex_protocol::models::MessagePhase;
use codex_protocol::protocol::AgentMessageContentDeltaEvent;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::ItemStartedEvent;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TokenUsageInfo;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc;

pub(crate) const ENABLE_FAKE_AGENT_RUNTIME_ENV: &str = "CODEX_ENABLE_FAKE_AGENT_RUNTIME_FOR_TESTS";

pub(crate) struct FakeAgentRuntimeProvider {
    runtime_id: AgentRuntimeId,
}

impl FakeAgentRuntimeProvider {
    pub(crate) fn new() -> Self {
        Self {
            runtime_id: AgentRuntimeId::new("fake").expect("static fake runtime id is valid"),
        }
    }
}

impl AgentRuntimeProvider for FakeAgentRuntimeProvider {
    fn id(&self) -> &AgentRuntimeId {
        &self.runtime_id
    }

    fn spawn(
        &self,
        request: AgentRuntimeSpawnRequest,
    ) -> AgentRuntimeFuture<'_, Arc<dyn AgentRuntimeThread>> {
        let runtime_id = self.runtime_id.clone();
        Box::pin(async move {
            let (event_tx, event_rx) = mpsc::unbounded_channel();
            let thread: Arc<dyn AgentRuntimeThread> = Arc::new(FakeAgentRuntimeThread {
                thread_id: request.thread_id,
                runtime_id,
                event_tx,
                event_rx: Mutex::new(event_rx),
            });
            Ok(thread)
        })
    }

    fn resume(
        &self,
        _request: AgentRuntimeResumeRequest,
    ) -> AgentRuntimeFuture<'_, Arc<dyn AgentRuntimeThread>> {
        Box::pin(async {
            Err(AgentRuntimeError::new(
                AgentRuntimeErrorKind::UnsupportedOperation,
                "the fake runtime does not persist sessions",
            ))
        })
    }
}

struct FakeAgentRuntimeThread {
    thread_id: ThreadId,
    runtime_id: AgentRuntimeId,
    event_tx: mpsc::UnboundedSender<Event>,
    event_rx: Mutex<mpsc::UnboundedReceiver<Event>>,
}

impl FakeAgentRuntimeThread {
    fn send_scripted_turn(&self, submission_id: &str) -> Result<(), AgentRuntimeError> {
        let turn_id = submission_id.to_string();
        let item_id = format!("{submission_id}-message");
        let message = "Hello from the fake external runtime.";
        let events = [
            Event {
                id: turn_id.clone(),
                msg: EventMsg::TurnStarted(TurnStartedEvent {
                    turn_id: turn_id.clone(),
                    trace_id: None,
                    started_at: None,
                    model_context_window: None,
                    collaboration_mode_kind: Default::default(),
                }),
            },
            Event {
                id: turn_id.clone(),
                msg: EventMsg::ItemStarted(ItemStartedEvent {
                    thread_id: self.thread_id,
                    turn_id: turn_id.clone(),
                    item: TurnItem::AgentMessage(AgentMessageItem {
                        id: item_id.clone(),
                        content: Vec::new(),
                        phase: Some(MessagePhase::FinalAnswer),
                        memory_citation: None,
                    }),
                    started_at_ms: 0,
                }),
            },
            Event {
                id: turn_id.clone(),
                msg: EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent {
                    thread_id: self.thread_id.to_string(),
                    turn_id: turn_id.clone(),
                    item_id: item_id.clone(),
                    delta: message.to_string(),
                }),
            },
            Event {
                id: turn_id.clone(),
                msg: EventMsg::ItemCompleted(ItemCompletedEvent {
                    thread_id: self.thread_id,
                    turn_id: turn_id.clone(),
                    item: TurnItem::AgentMessage(AgentMessageItem {
                        id: item_id,
                        content: vec![AgentMessageContent::Text {
                            text: message.to_string(),
                        }],
                        phase: Some(MessagePhase::FinalAnswer),
                        memory_citation: None,
                    }),
                    completed_at_ms: 0,
                }),
            },
            Event {
                id: turn_id.clone(),
                msg: EventMsg::TurnComplete(TurnCompleteEvent {
                    turn_id,
                    last_agent_message: Some(message.to_string()),
                    error: None,
                    started_at: None,
                    completed_at: None,
                    duration_ms: None,
                    time_to_first_token_ms: None,
                }),
            },
        ];
        for event in events {
            self.event_tx.send(event).map_err(|_| {
                AgentRuntimeError::new(
                    AgentRuntimeErrorKind::Unavailable,
                    "fake runtime event listener closed",
                )
            })?;
        }
        Ok(())
    }
}

impl AgentRuntimeThread for FakeAgentRuntimeThread {
    fn thread_id(&self) -> ThreadId {
        self.thread_id
    }

    fn runtime_id(&self) -> &AgentRuntimeId {
        &self.runtime_id
    }

    fn submit(&self, operation: AgentRuntimeOperation) -> AgentRuntimeFuture<'_, ()> {
        Box::pin(async move {
            if !matches!(operation.op, Op::Shutdown { .. }) {
                self.send_scripted_turn(&operation.submission_id)?;
            }
            Ok(())
        })
    }

    fn next_event(&self) -> AgentRuntimeFuture<'_, Option<Event>> {
        Box::pin(async move { Ok(self.event_rx.lock().await.recv().await) })
    }

    fn status(&self) -> AgentRuntimeFuture<'_, AgentStatus> {
        Box::pin(async { Ok(AgentStatus::PendingInit) })
    }

    fn token_usage(&self) -> AgentRuntimeFuture<'_, Option<TokenUsageInfo>> {
        Box::pin(async {
            Ok(Some(TokenUsageInfo {
                total_token_usage: TokenUsage::default(),
                last_token_usage: TokenUsage::default(),
                model_context_window: None,
            }))
        })
    }

    fn persistence(&self) -> AgentRuntimeFuture<'_, AgentRuntimePersistence> {
        Box::pin(async move {
            Ok(AgentRuntimePersistence {
                runtime_id: self.runtime_id.clone(),
                session_locator: self.thread_id.to_string(),
                metadata: json!({"kind": "fake"}),
            })
        })
    }

    fn shutdown(&self) -> AgentRuntimeFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
