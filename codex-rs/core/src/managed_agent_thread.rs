use crate::agent::AgentStatus;
use crate::agent::agent_status_from_event;
use crate::codex_thread::CodexThread;
use crate::codex_thread::ThreadConfigSnapshot;
use crate::external_host_tools::ExternalHostTools;
use codex_extension_api::AgentRuntimeOperation;
use codex_extension_api::AgentRuntimeThread;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExternalRuntimeItem;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SessionConfiguredEvent;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::TokenUsageInfo;
use codex_thread_store::LiveThread;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use tokio::sync::broadcast;
use tokio::sync::watch;

/// Runtime-neutral handle for a loaded agent thread.
///
/// Native sessions retain their full [`CodexThread`] API. External sessions expose only the
/// lifecycle capabilities shared with runtime providers; native-only callers must explicitly ask
/// for `as_codex_thread` instead of accidentally bypassing the provider boundary.
pub enum ManagedAgentThread {
    Codex(Arc<CodexThread>),
    External(ExternalAgentThread),
}

#[doc(hidden)]
pub struct ExternalAgentThread {
    runtime: Arc<dyn AgentRuntimeThread>,
    session_source: SessionSource,
    config_snapshot: ThreadConfigSnapshot,
    session_configured: SessionConfiguredEvent,
    multi_agent_version: Option<MultiAgentVersion>,
    next_submission_id: AtomicU64,
    status_tx: watch::Sender<AgentStatus>,
    completion_tx: broadcast::Sender<AgentStatus>,
    host_tools: Option<Arc<ExternalHostTools>>,
    host_tool_driver: Option<tokio::task::JoinHandle<()>>,
    persistence: Option<LiveThread>,
}

impl ManagedAgentThread {
    pub(crate) fn codex(thread: Arc<CodexThread>) -> Self {
        Self::Codex(thread)
    }

    pub(crate) async fn external(
        runtime: Arc<dyn AgentRuntimeThread>,
        session_source: SessionSource,
        config_snapshot: ThreadConfigSnapshot,
        session_configured: SessionConfiguredEvent,
        multi_agent_version: Option<MultiAgentVersion>,
        host_tools: Option<Arc<ExternalHostTools>>,
        persistence: Option<LiveThread>,
    ) -> CodexResult<Self> {
        let status = runtime.status().await.map_err(runtime_error)?;
        let (status_tx, _) = watch::channel(status);
        let (completion_tx, _) = broadcast::channel(16);
        let host_tool_driver = host_tools.as_ref().map(|host_tools| {
            tokio::spawn(run_host_tool_driver(
                Arc::clone(&runtime),
                Arc::clone(host_tools),
            ))
        });
        Ok(Self::External(ExternalAgentThread {
            runtime,
            session_source,
            config_snapshot,
            session_configured,
            multi_agent_version,
            next_submission_id: AtomicU64::new(0),
            status_tx,
            completion_tx,
            host_tools,
            host_tool_driver,
            persistence,
        }))
    }

    pub fn as_codex_thread(&self) -> Option<&Arc<CodexThread>> {
        match self {
            Self::Codex(thread) => Some(thread),
            Self::External(_) => None,
        }
    }

    pub fn thread_id(&self) -> ThreadId {
        match self {
            Self::Codex(thread) => thread.session_configured().thread_id,
            Self::External(thread) => thread.runtime.thread_id(),
        }
    }

    pub(crate) fn session_source(&self) -> &SessionSource {
        match self {
            Self::Codex(thread) => &thread.session_source,
            Self::External(thread) => &thread.session_source,
        }
    }

    pub async fn submit(&self, op: Op) -> CodexResult<String> {
        match self {
            Self::Codex(thread) => thread.submit(op).await,
            Self::External(thread) => {
                if is_host_response_op(&op) {
                    return thread
                        .host_tools
                        .as_ref()
                        .ok_or_else(|| {
                            CodexErr::UnsupportedOperation(
                                "external thread has no Codex host tool context".to_string(),
                            )
                        })?
                        .submit_response(op)
                        .await;
                }
                let is_shutdown = matches!(&op, Op::Shutdown);
                let sequence = thread.next_submission_id.fetch_add(1, Ordering::Relaxed);
                let submission_id = format!("external-{}-{sequence}", thread.runtime.thread_id());
                if matches!(
                    &op,
                    Op::UserInput { .. } | Op::InterAgentCommunication { .. }
                ) && let Some(host_tools) = thread.host_tools.as_ref()
                {
                    host_tools.reset_for_turn(&submission_id).await;
                }
                if matches!(&op, Op::Interrupt)
                    && let Some(host_tools) = thread.host_tools.as_ref()
                {
                    host_tools.interrupt().await;
                }
                thread
                    .runtime
                    .submit(AgentRuntimeOperation {
                        submission_id: submission_id.clone(),
                        op,
                    })
                    .await
                    .map_err(runtime_error)?;
                if is_shutdown {
                    thread.status_tx.send_replace(AgentStatus::Shutdown);
                }
                Ok(submission_id)
            }
        }
    }

    pub async fn next_event(&self) -> CodexResult<Event> {
        match self {
            Self::Codex(thread) => thread.next_event().await,
            Self::External(thread) => {
                let event = if let Some(host_tools) = thread.host_tools.as_ref() {
                    tokio::select! {
                        biased;
                        host_event = host_tools.next_event() => host_event?,
                        runtime_event = thread.runtime.next_event() => {
                            runtime_event.map_err(runtime_error)?.ok_or(CodexErr::InternalAgentDied)?
                        }
                    }
                } else {
                    thread
                        .runtime
                        .next_event()
                        .await
                        .map_err(runtime_error)?
                        .ok_or(CodexErr::InternalAgentDied)?
                };
                if let Some(status) = agent_status_from_event(&event.msg) {
                    thread.status_tx.send_replace(status.clone());
                    if matches!(
                        status,
                        AgentStatus::Completed(_) | AgentStatus::Errored(_) | AgentStatus::Shutdown
                    ) {
                        let _ = thread.completion_tx.send(status);
                    }
                }
                if let Some(persistence) = thread.persistence.as_ref() {
                    let mut rollout_items = vec![RolloutItem::EventMsg(event.msg.clone())];
                    if let EventMsg::ItemCompleted(completed) = &event.msg {
                        rollout_items.push(RolloutItem::ExternalRuntimeItem(ExternalRuntimeItem {
                            turn_id: completed.turn_id.clone(),
                            item: completed.item.clone(),
                        }));
                    }
                    persistence
                        .append_items(&rollout_items)
                        .await
                        .map_err(|error| {
                            CodexErr::Fatal(format!(
                                "failed to persist external runtime event: {error}"
                            ))
                        })?;
                    if matches!(
                        event.msg,
                        codex_protocol::protocol::EventMsg::TurnComplete(_)
                            | codex_protocol::protocol::EventMsg::TurnAborted(_)
                    ) {
                        persistence.flush().await.map_err(|error| {
                            CodexErr::Fatal(format!(
                                "failed to flush external runtime turn: {error}"
                            ))
                        })?;
                    }
                }
                Ok(event)
            }
        }
    }

    pub async fn agent_status(&self) -> AgentStatus {
        match self {
            Self::Codex(thread) => thread.agent_status().await,
            Self::External(thread) => thread.status_tx.borrow().clone(),
        }
    }

    pub(crate) fn subscribe_status(&self) -> watch::Receiver<AgentStatus> {
        match self {
            Self::Codex(thread) => thread.subscribe_status(),
            Self::External(thread) => thread.status_tx.subscribe(),
        }
    }

    pub(crate) fn subscribe_external_completions(
        &self,
    ) -> Option<broadcast::Receiver<AgentStatus>> {
        match self {
            Self::Codex(_) => None,
            Self::External(thread) => Some(thread.completion_tx.subscribe()),
        }
    }

    pub async fn token_usage_info(&self) -> CodexResult<Option<TokenUsageInfo>> {
        match self {
            Self::Codex(thread) => Ok(thread.token_usage_info().await),
            Self::External(thread) => thread.runtime.token_usage().await.map_err(runtime_error),
        }
    }

    pub async fn shutdown_and_wait(&self) -> CodexResult<()> {
        match self {
            Self::Codex(thread) => thread.shutdown_and_wait().await,
            Self::External(thread) => {
                if let Some(driver) = thread.host_tool_driver.as_ref() {
                    driver.abort();
                }
                thread.runtime.shutdown().await.map_err(runtime_error)?;
                if let Some(host_tools) = thread.host_tools.as_ref() {
                    host_tools.shutdown().await?;
                }
                if let Some(persistence) = thread.persistence.as_ref() {
                    persistence.shutdown().await.map_err(|error| {
                        CodexErr::Fatal(format!(
                            "failed to shutdown external runtime persistence: {error}"
                        ))
                    })?;
                }
                thread.status_tx.send_replace(AgentStatus::Shutdown);
                Ok(())
            }
        }
    }

    pub(crate) async fn wait_until_terminated(&self) {
        match self {
            Self::Codex(thread) => thread.wait_until_terminated().await,
            Self::External(thread) => {
                let mut status_rx = thread.status_tx.subscribe();
                while !matches!(*status_rx.borrow(), AgentStatus::Shutdown) {
                    if status_rx.changed().await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    pub(crate) async fn ensure_rollout_materialized(&self) {
        match self {
            Self::Codex(thread) => thread.ensure_rollout_materialized().await,
            Self::External(thread) => {
                if let Some(persistence) = thread.persistence.as_ref() {
                    let _ = persistence.persist().await;
                }
            }
        }
    }

    pub(crate) async fn flush_rollout(&self) -> std::io::Result<()> {
        match self {
            Self::Codex(thread) => thread.flush_rollout().await,
            Self::External(thread) => match thread.persistence.as_ref() {
                Some(persistence) => persistence.flush().await.map_err(std::io::Error::other),
                None => Ok(()),
            },
        }
    }

    pub(crate) async fn is_turn_active(&self) -> bool {
        match self {
            Self::Codex(thread) => thread.codex.session.active_turn.lock().await.is_some(),
            Self::External(thread) => matches!(*thread.status_tx.borrow(), AgentStatus::Running),
        }
    }

    pub(crate) async fn inject_user_message_without_turn(&self, message: String) {
        if let Self::Codex(thread) = self {
            thread.inject_user_message_without_turn(message).await;
        }
    }

    pub fn session_configured(&self) -> SessionConfiguredEvent {
        match self {
            Self::Codex(thread) => thread.session_configured(),
            Self::External(thread) => thread.session_configured.clone(),
        }
    }

    pub async fn config_snapshot(&self) -> ThreadConfigSnapshot {
        match self {
            Self::Codex(thread) => thread.config_snapshot().await,
            Self::External(thread) => thread.config_snapshot.clone(),
        }
    }

    pub(crate) fn multi_agent_version(&self) -> Option<MultiAgentVersion> {
        match self {
            Self::Codex(thread) => thread.multi_agent_version(),
            Self::External(thread) => thread.multi_agent_version,
        }
    }

    pub async fn config(&self) -> Option<Arc<crate::config::Config>> {
        match self {
            Self::Codex(thread) => Some(thread.config().await),
            Self::External(_) => None,
        }
    }

    pub async fn environment_selections(
        &self,
    ) -> Vec<codex_protocol::protocol::TurnEnvironmentSelection> {
        match self {
            Self::Codex(thread) => thread.environment_selections().await,
            Self::External(thread) => thread.config_snapshot.environments.environments.clone(),
        }
    }

    pub async fn emit_thread_idle_lifecycle_if_idle(&self) {
        if let Self::Codex(thread) = self {
            thread.emit_thread_idle_lifecycle_if_idle().await;
        }
    }

    pub async fn read_thread(
        &self,
        include_archived: bool,
        include_history: bool,
    ) -> CodexResult<codex_thread_store::StoredThread> {
        match self {
            Self::Codex(thread) => thread
                .read_thread(include_archived, include_history)
                .await
                .map_err(|error| CodexErr::Fatal(format!("failed to read thread: {error}"))),
            Self::External(thread) => thread
                .persistence
                .as_ref()
                .ok_or_else(|| {
                    CodexErr::UnsupportedOperation(format!(
                        "thread {} has no host persistence",
                        thread.runtime.thread_id()
                    ))
                })?
                .read_thread(include_archived, include_history)
                .await
                .map_err(|error| CodexErr::Fatal(format!("failed to read thread: {error}"))),
        }
    }
}

impl From<Arc<CodexThread>> for ManagedAgentThread {
    fn from(thread: Arc<CodexThread>) -> Self {
        Self::Codex(thread)
    }
}

fn runtime_error(error: codex_extension_api::AgentRuntimeError) -> CodexErr {
    CodexErr::Fatal(format!("external agent runtime failed: {error}"))
}

async fn run_host_tool_driver(
    runtime: Arc<dyn AgentRuntimeThread>,
    host_tools: Arc<ExternalHostTools>,
) {
    loop {
        let call = match runtime.next_tool_call().await {
            Ok(Some(call)) => call,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(%error, "external runtime host tool channel failed");
                break;
            }
        };
        let result = host_tools.execute(call).await;
        if let Err(error) = runtime.submit_tool_result(result).await {
            tracing::warn!(%error, "failed to return Codex host tool result to external runtime");
            break;
        }
    }
}

fn is_host_response_op(op: &Op) -> bool {
    matches!(
        op,
        Op::ExecApproval { .. }
            | Op::PatchApproval { .. }
            | Op::UserInputAnswer { .. }
            | Op::RequestPermissionsResponse { .. }
            | Op::DynamicToolResponse { .. }
            | Op::ResolveElicitation { .. }
    )
}

#[cfg(test)]
#[path = "managed_agent_thread_tests.rs"]
mod tests;
