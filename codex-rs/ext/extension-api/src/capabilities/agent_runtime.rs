use std::fmt;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::Arc;

use codex_protocol::ThreadId;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::TokenUsageInfo;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde_json::Value as JsonValue;

/// Stable identifier used to select an agent runtime provider.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentRuntimeId(String);

impl AgentRuntimeId {
    pub const CODEX: &'static str = "codex";

    pub fn codex() -> Self {
        Self(Self::CODEX.to_string())
    }

    pub fn new(value: impl Into<String>) -> Result<Self, AgentRuntimeError> {
        let value = value.into();
        let normalized = value.trim();
        if normalized.is_empty()
            || !normalized.chars().all(|character| {
                character.is_ascii_lowercase()
                    || character.is_ascii_digit()
                    || matches!(character, '-' | '_' | '.')
            })
        {
            return Err(AgentRuntimeError::new(
                AgentRuntimeErrorKind::InvalidRuntimeId,
                format!(
                    "invalid agent runtime id `{value}`; expected lowercase ASCII letters, digits, dots, hyphens, or underscores"
                ),
            ));
        }
        Ok(Self(normalized.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_codex(&self) -> bool {
        self.0 == Self::CODEX
    }
}

impl fmt::Display for AgentRuntimeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl TryFrom<&str> for AgentRuntimeId {
    type Error = AgentRuntimeError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for AgentRuntimeId {
    type Error = AgentRuntimeError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// Stable error categories shared by runtime providers and the Codex host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentRuntimeErrorKind {
    InvalidRuntimeId,
    UnknownRuntime,
    DuplicateRuntime,
    UnsupportedOperation,
    Unavailable,
    Protocol,
    Internal,
}

/// Structured failure returned by an agent runtime provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRuntimeError {
    pub kind: AgentRuntimeErrorKind,
    pub message: String,
}

impl AgentRuntimeError {
    pub fn new(kind: AgentRuntimeErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for AgentRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(formatter)
    }
}

impl std::error::Error for AgentRuntimeError {}

/// Host-owned metadata supplied when a provider creates a child thread.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRuntimeSpawnRequest {
    pub thread_id: ThreadId,
    pub parent_thread_id: Option<ThreadId>,
    pub role: Option<String>,
    pub cwd: AbsolutePathBuf,
    pub model: Option<String>,
    pub model_provider: String,
    pub runtime_config: JsonValue,
    pub host_tools: Vec<AgentRuntimeHostToolDefinition>,
}

/// Model-visible description of one tool whose execution remains owned by Codex.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRuntimeHostToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: JsonValue,
}

/// Durable provider state needed to reconstruct an external child thread.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRuntimePersistence {
    pub runtime_id: AgentRuntimeId,
    pub session_locator: String,
    pub metadata: JsonValue,
}

/// Host-owned metadata supplied when a provider resumes a persisted child.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRuntimeResumeRequest {
    pub thread_id: ThreadId,
    pub parent_thread_id: Option<ThreadId>,
    pub cwd: AbsolutePathBuf,
    pub state: AgentRuntimePersistence,
    pub host_tools: Vec<AgentRuntimeHostToolDefinition>,
}

/// Native operation submitted to a runtime-backed thread.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRuntimeOperation {
    pub submission_id: String,
    pub op: Op,
}

/// Request from an external runtime to execute one Codex-hosted tool.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRuntimeToolCall {
    pub call_id: String,
    pub tool_name: String,
    pub arguments: JsonValue,
}

/// Structured result returned to an external runtime after host tool execution.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRuntimeToolResult {
    pub call_id: String,
    pub output: JsonValue,
    pub is_error: bool,
}

pub type AgentRuntimeFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, AgentRuntimeError>> + Send + 'a>>;

/// Runtime-owned thread handle consumed by Codex thread management.
///
/// Implementations translate their native lifecycle into Codex `Event` values and accept the
/// same `Op` values used by native threads. Sensitive tool calls are surfaced separately through
/// `AgentRuntimeToolCall`; providers must not execute them outside the Codex host.
pub trait AgentRuntimeThread: Send + Sync {
    fn thread_id(&self) -> ThreadId;
    fn runtime_id(&self) -> &AgentRuntimeId;
    fn submit(&self, operation: AgentRuntimeOperation) -> AgentRuntimeFuture<'_, ()>;
    fn next_event(&self) -> AgentRuntimeFuture<'_, Option<Event>>;
    fn next_tool_call(&self) -> AgentRuntimeFuture<'_, Option<AgentRuntimeToolCall>> {
        Box::pin(async { Ok(None) })
    }
    fn submit_tool_result(&self, _result: AgentRuntimeToolResult) -> AgentRuntimeFuture<'_, ()> {
        Box::pin(async {
            Err(AgentRuntimeError::new(
                AgentRuntimeErrorKind::UnsupportedOperation,
                "agent runtime does not accept host tool results",
            ))
        })
    }
    fn status(&self) -> AgentRuntimeFuture<'_, AgentStatus>;
    fn token_usage(&self) -> AgentRuntimeFuture<'_, Option<TokenUsageInfo>>;
    fn persistence(&self) -> AgentRuntimeFuture<'_, AgentRuntimePersistence>;
    fn shutdown(&self) -> AgentRuntimeFuture<'_, ()>;
}

/// Provider capable of spawning and resuming non-Codex child threads.
///
/// Codex owns child identity, parent metadata, persistence, and tool execution. Providers own
/// their agent SDK session and must return a runtime thread whose ID matches the host request.
pub trait AgentRuntimeProvider: Send + Sync {
    fn id(&self) -> &AgentRuntimeId;
    fn spawn(
        &self,
        request: AgentRuntimeSpawnRequest,
    ) -> AgentRuntimeFuture<'_, Arc<dyn AgentRuntimeThread>>;
    fn resume(
        &self,
        request: AgentRuntimeResumeRequest,
    ) -> AgentRuntimeFuture<'_, Arc<dyn AgentRuntimeThread>>;
}
