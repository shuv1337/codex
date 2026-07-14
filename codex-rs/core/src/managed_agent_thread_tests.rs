use super::*;
use codex_extension_api::AgentRuntimeFuture;
use codex_extension_api::AgentRuntimeId;
use codex_extension_api::AgentRuntimePersistence;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnEnvironmentSelections;
use codex_protocol::protocol::TurnStartedEvent;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

struct FakeRuntimeThread {
    thread_id: ThreadId,
    runtime_id: AgentRuntimeId,
    events: Mutex<VecDeque<Event>>,
    operations: Mutex<Vec<AgentRuntimeOperation>>,
    shutdown: AtomicBool,
}

impl FakeRuntimeThread {
    fn new(thread_id: ThreadId, events: Vec<Event>) -> Self {
        Self {
            thread_id,
            runtime_id: AgentRuntimeId::new("fake").expect("valid runtime id"),
            events: Mutex::new(events.into()),
            operations: Mutex::new(Vec::new()),
            shutdown: AtomicBool::new(false),
        }
    }
}

impl AgentRuntimeThread for FakeRuntimeThread {
    fn thread_id(&self) -> ThreadId {
        self.thread_id
    }

    fn runtime_id(&self) -> &AgentRuntimeId {
        &self.runtime_id
    }

    fn submit(&self, operation: AgentRuntimeOperation) -> AgentRuntimeFuture<'_, ()> {
        Box::pin(async move {
            self.operations
                .lock()
                .expect("operations lock")
                .push(operation);
            Ok(())
        })
    }

    fn next_event(&self) -> AgentRuntimeFuture<'_, Option<Event>> {
        Box::pin(async move { Ok(self.events.lock().expect("events lock").pop_front()) })
    }

    fn status(&self) -> AgentRuntimeFuture<'_, AgentStatus> {
        Box::pin(async { Ok(AgentStatus::PendingInit) })
    }

    fn token_usage(&self) -> AgentRuntimeFuture<'_, Option<TokenUsageInfo>> {
        Box::pin(async { Ok(None) })
    }

    fn persistence(&self) -> AgentRuntimeFuture<'_, AgentRuntimePersistence> {
        Box::pin(async move {
            Ok(AgentRuntimePersistence {
                runtime_id: self.runtime_id.clone(),
                session_locator: self.thread_id.to_string(),
                metadata: json!({}),
            })
        })
    }

    fn shutdown(&self) -> AgentRuntimeFuture<'_, ()> {
        Box::pin(async move {
            self.shutdown.store(true, Ordering::Relaxed);
            Ok(())
        })
    }
}

fn turn_started(turn_id: &str) -> Event {
    Event {
        id: turn_id.to_string(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: turn_id.to_string(),
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: ModeKind::Default,
        }),
    }
}

fn turn_completed(turn_id: &str) -> Event {
    Event {
        id: turn_id.to_string(),
        msg: EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: turn_id.to_string(),
            last_agent_message: Some("done".to_string()),
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
    }
}

fn external_metadata(thread_id: ThreadId) -> (ThreadConfigSnapshot, SessionConfiguredEvent) {
    let cwd = AbsolutePathBuf::try_from(std::env::current_dir().expect("current directory"))
        .expect("absolute current directory");
    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model: "fake-model".to_string(),
            reasoning_effort: None,
            developer_instructions: None,
        },
    };
    let permission_profile = PermissionProfile::read_only();
    let config_snapshot = ThreadConfigSnapshot {
        model: "fake-model".to_string(),
        model_provider_id: "fake-provider".to_string(),
        service_tier: None,
        approval_policy: AskForApproval::OnRequest,
        approvals_reviewer: ApprovalsReviewer::User,
        permission_profile: permission_profile.clone(),
        active_permission_profile: None,
        environments: TurnEnvironmentSelections::new(cwd.clone(), Vec::new()),
        workspace_roots: Vec::new(),
        profile_workspace_roots: Vec::new(),
        ephemeral: true,
        reasoning_effort: None,
        reasoning_summary: None,
        personality: None,
        collaboration_mode,
        session_source: SessionSource::Exec,
        history_mode: ThreadHistoryMode::Legacy,
        forked_from_thread_id: None,
        parent_thread_id: None,
        thread_source: None,
        originator: "managed-agent-thread-test".to_string(),
    };
    let session_configured = SessionConfiguredEvent {
        session_id: thread_id.into(),
        thread_id,
        forked_from_id: None,
        parent_thread_id: None,
        thread_source: None,
        thread_name: None,
        model: "fake-model".to_string(),
        model_provider_id: "fake-provider".to_string(),
        service_tier: None,
        approval_policy: AskForApproval::OnRequest,
        approvals_reviewer: ApprovalsReviewer::User,
        permission_profile,
        active_permission_profile: None,
        cwd,
        reasoning_effort: None,
        initial_messages: None,
        network_proxy: None,
        rollout_path: None,
    };
    (config_snapshot, session_configured)
}

#[tokio::test]
async fn external_thread_maps_operations_events_status_and_shutdown() {
    let thread_id = ThreadId::new();
    let runtime = Arc::new(FakeRuntimeThread::new(
        thread_id,
        vec![turn_started("turn-1"), turn_completed("turn-1")],
    ));
    let runtime_trait: Arc<dyn AgentRuntimeThread> = runtime.clone();
    let (config_snapshot, session_configured) = external_metadata(thread_id);
    let thread = ManagedAgentThread::external(
        runtime_trait,
        SessionSource::Exec,
        config_snapshot,
        session_configured,
        Some(MultiAgentVersion::V2),
        None,
        None,
    )
    .await
    .expect("external thread");

    assert_eq!(thread.thread_id(), thread_id);
    assert_eq!(thread.agent_status().await, AgentStatus::PendingInit);
    assert!(thread.as_codex_thread().is_none());

    let submission_id = thread
        .submit(Op::Interrupt)
        .await
        .expect("submit operation");
    assert_eq!(submission_id, format!("external-{thread_id}-0"));
    let operations = runtime.operations.lock().expect("operations lock");
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].op, Op::Interrupt);
    drop(operations);

    let mut status_rx = thread.subscribe_status();
    assert!(matches!(
        thread.next_event().await.expect("turn started").msg,
        EventMsg::TurnStarted(_)
    ));
    status_rx.changed().await.expect("running status");
    assert_eq!(*status_rx.borrow(), AgentStatus::Running);
    assert!(matches!(
        thread.next_event().await.expect("turn completed").msg,
        EventMsg::TurnComplete(_)
    ));
    status_rx.changed().await.expect("completed status");
    assert_eq!(
        *status_rx.borrow(),
        AgentStatus::Completed(Some("done".to_string()))
    );

    thread.shutdown_and_wait().await.expect("shutdown");
    assert!(runtime.shutdown.load(Ordering::Relaxed));
    assert_eq!(thread.agent_status().await, AgentStatus::Shutdown);
}

#[tokio::test]
async fn external_thread_reports_closed_event_stream_as_dead() {
    let thread_id = ThreadId::new();
    let runtime: Arc<dyn AgentRuntimeThread> =
        Arc::new(FakeRuntimeThread::new(thread_id, Vec::new()));
    let (config_snapshot, session_configured) = external_metadata(thread_id);
    let thread = ManagedAgentThread::external(
        runtime,
        SessionSource::Exec,
        config_snapshot,
        session_configured,
        None,
        None,
        None,
    )
    .await
    .expect("external thread");

    assert!(matches!(
        thread.next_event().await,
        Err(CodexErr::InternalAgentDied)
    ));
}
