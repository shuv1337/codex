#[cfg(unix)]
mod client;
mod config;
pub mod proto;
#[cfg(unix)]
mod runtime_thread;

pub use config::PiRuntimeConfig;

use codex_extension_api::AgentRuntimeError;
#[cfg(not(unix))]
use codex_extension_api::AgentRuntimeErrorKind;
use codex_extension_api::AgentRuntimeFuture;
use codex_extension_api::AgentRuntimeId;
use codex_extension_api::AgentRuntimeProvider;
use codex_extension_api::AgentRuntimeResumeRequest;
use codex_extension_api::AgentRuntimeSpawnRequest;
use codex_extension_api::AgentRuntimeThread;
use std::sync::Arc;

pub const PI_RUNTIME_EXECUTABLE_ENV: &str = "CODEX_PI_RUNTIME_EXECUTABLE";
pub const PI_RUNTIME_ARGUMENTS_ENV: &str = "CODEX_PI_RUNTIME_ARGUMENTS_JSON";

pub struct PiRuntimeProvider {
    runtime_id: AgentRuntimeId,
    config: PiRuntimeConfig,
    #[cfg(unix)]
    client: tokio::sync::Mutex<Option<Arc<client::PiRuntimeClient>>>,
}

impl PiRuntimeProvider {
    pub fn new(config: PiRuntimeConfig) -> Self {
        Self {
            runtime_id: AgentRuntimeId::new("pi").expect("static Pi runtime id is valid"),
            config,
            #[cfg(unix)]
            client: tokio::sync::Mutex::new(None),
        }
    }

    pub fn from_environment() -> Result<Option<Self>, AgentRuntimeError> {
        PiRuntimeConfig::from_environment().map(|config| config.map(Self::new))
    }

    #[cfg(unix)]
    async fn client(&self) -> Result<Arc<client::PiRuntimeClient>, AgentRuntimeError> {
        let mut client = self.client.lock().await;
        if let Some(existing) = client.as_ref() {
            return Ok(Arc::clone(existing));
        }
        let launched = Arc::new(client::PiRuntimeClient::launch(&self.config).await?);
        *client = Some(Arc::clone(&launched));
        Ok(launched)
    }
}

impl AgentRuntimeProvider for PiRuntimeProvider {
    fn id(&self) -> &AgentRuntimeId {
        &self.runtime_id
    }

    #[cfg(unix)]
    fn spawn(
        &self,
        request: AgentRuntimeSpawnRequest,
    ) -> AgentRuntimeFuture<'_, Arc<dyn AgentRuntimeThread>> {
        Box::pin(async move {
            let client = self.client().await?;
            runtime_thread::spawn_thread(client, self.runtime_id.clone(), request).await
        })
    }

    #[cfg(not(unix))]
    fn spawn(
        &self,
        _request: AgentRuntimeSpawnRequest,
    ) -> AgentRuntimeFuture<'_, Arc<dyn AgentRuntimeThread>> {
        unsupported_platform()
    }

    #[cfg(unix)]
    fn resume(
        &self,
        request: AgentRuntimeResumeRequest,
    ) -> AgentRuntimeFuture<'_, Arc<dyn AgentRuntimeThread>> {
        Box::pin(async move {
            let client = self.client().await?;
            runtime_thread::resume_thread(client, self.runtime_id.clone(), request).await
        })
    }

    #[cfg(not(unix))]
    fn resume(
        &self,
        _request: AgentRuntimeResumeRequest,
    ) -> AgentRuntimeFuture<'_, Arc<dyn AgentRuntimeThread>> {
        unsupported_platform()
    }
}

#[cfg(not(unix))]
fn unsupported_platform<T>() -> AgentRuntimeFuture<'static, T> {
    Box::pin(async {
        Err(AgentRuntimeError::new(
            AgentRuntimeErrorKind::UnsupportedOperation,
            "the Pi runtime sidecar currently requires Unix domain sockets",
        ))
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use codex_extension_api::AgentRuntimeHostToolDefinition;
    use codex_extension_api::AgentRuntimeOperation;
    use codex_extension_api::AgentRuntimeProvider;
    use codex_extension_api::AgentRuntimeToolResult;
    use codex_protocol::AgentPath;
    use codex_protocol::ThreadId;
    use codex_protocol::protocol::AgentStatus;
    use codex_protocol::protocol::EventMsg;
    use codex_protocol::user_input::UserInput;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use serde_json::json;
    use std::ffi::OsString;

    #[tokio::test]
    async fn launches_real_sidecar_and_round_trips_spawn_metadata_when_configured() {
        let (Some(executable), Some(script)) = (
            std::env::var_os("PI_RUNTIME_TEST_EXECUTABLE"),
            std::env::var_os("PI_RUNTIME_TEST_SCRIPT"),
        ) else {
            return;
        };
        let working_directory = tempfile::tempdir().expect("test working directory");
        let mut config = PiRuntimeConfig::new(executable);
        config.arguments = vec![script];
        config.environment.insert(
            OsString::from("PI_CODEX_RUNTIME_FAUX_RESPONSES_JSON"),
            OsString::from("[\"Hello from the real Pi SDK sidecar.\"]"),
        );
        let provider = PiRuntimeProvider::new(config);
        let thread_id = ThreadId::new();
        let thread = provider
            .spawn(AgentRuntimeSpawnRequest {
                thread_id,
                parent_thread_id: Some(ThreadId::new()),
                role: Some("pi_worker".to_string()),
                cwd: AbsolutePathBuf::from_absolute_path(working_directory.path())
                    .expect("absolute test path"),
                model: None,
                model_provider: "pi".to_string(),
                runtime_config: json!({
                    "provider": "faux",
                    "model": "faux-1",
                    "thinking_level": "high"
                }),
                host_tools: Vec::new(),
            })
            .await
            .expect("Pi sidecar spawn should succeed");

        assert_eq!(thread.thread_id(), thread_id);
        assert_eq!(
            thread.status().await.expect("status"),
            AgentStatus::PendingInit
        );
        thread
            .submit(AgentRuntimeOperation {
                submission_id: "pi-turn-1".to_string(),
                op: vec![UserInput::Text {
                    text: "Reply through Pi".to_string(),
                    text_elements: Vec::new(),
                }]
                .into(),
            })
            .await
            .expect("submit Pi prompt");

        let mut saw_turn_started = false;
        let mut saw_item_completed = false;
        let mut saw_turn_completed = false;
        let mut saw_token_count = false;
        let mut streamed_text = String::new();
        for _ in 0..32 {
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(5), thread.next_event())
                    .await
                    .expect("Pi event timeout")
                    .expect("Pi event result")
                    .expect("Pi event stream should remain open");
            match event.msg {
                EventMsg::TurnStarted(event) => {
                    saw_turn_started = true;
                    assert_eq!(event.turn_id, "pi-turn-1");
                }
                EventMsg::AgentMessageContentDelta(event) => streamed_text.push_str(&event.delta),
                EventMsg::ItemCompleted(event) => {
                    if matches!(event.item, codex_protocol::items::TurnItem::AgentMessage(_)) {
                        saw_item_completed = true;
                    }
                }
                EventMsg::TurnComplete(event) => {
                    saw_turn_completed = true;
                    assert_eq!(
                        event.last_agent_message.as_deref(),
                        Some("Hello from the real Pi SDK sidecar.")
                    );
                }
                EventMsg::TokenCount(_) => saw_token_count = true,
                _ => {}
            }
            if saw_turn_completed && saw_token_count {
                break;
            }
        }
        assert!(saw_turn_started);
        assert!(saw_item_completed);
        assert!(saw_turn_completed);
        assert!(saw_token_count);
        assert_eq!(streamed_text, "Hello from the real Pi SDK sidecar.");
        assert_eq!(
            thread.status().await.expect("completed status"),
            AgentStatus::Completed(Some("Hello from the real Pi SDK sidecar.".to_string()))
        );

        let persistence = thread.persistence().await.expect("persistence metadata");
        assert_eq!(persistence.runtime_id.as_str(), "pi");
        assert!(!persistence.session_locator.is_empty());
        assert_eq!(persistence.metadata["provider"], "faux");
        assert_eq!(persistence.metadata["model"], "faux-1");
        thread.shutdown().await.expect("close Pi sidecar session");
    }

    #[tokio::test]
    async fn resumes_real_sidecar_session_from_persisted_locator_when_configured() {
        let (Some(executable), Some(script)) = (
            std::env::var_os("PI_RUNTIME_TEST_EXECUTABLE"),
            std::env::var_os("PI_RUNTIME_TEST_SCRIPT"),
        ) else {
            return;
        };
        let working_directory = tempfile::tempdir().expect("test working directory");
        let cwd = AbsolutePathBuf::from_absolute_path(working_directory.path())
            .expect("absolute test path");
        let parent_thread_id = ThreadId::new();
        let thread_id = ThreadId::new();
        let mut config = PiRuntimeConfig::new(executable);
        config.arguments = vec![script];
        config.environment.insert(
            OsString::from("PI_CODEX_RUNTIME_FAUX_RESPONSES_JSON"),
            OsString::from("[\"first reply\",\"resumed reply\"]"),
        );
        let provider = PiRuntimeProvider::new(config);
        let thread = provider
            .spawn(AgentRuntimeSpawnRequest {
                thread_id,
                parent_thread_id: Some(parent_thread_id),
                role: Some("pi_worker".to_string()),
                cwd: cwd.clone(),
                model: None,
                model_provider: "pi".to_string(),
                runtime_config: json!({ "provider": "faux", "model": "faux-1" }),
                host_tools: Vec::new(),
            })
            .await
            .expect("spawn persistent Pi session");
        thread
            .submit(AgentRuntimeOperation {
                submission_id: "pi-turn-before-reload".to_string(),
                op: vec![UserInput::Text {
                    text: "say first".to_string(),
                    text_elements: Vec::new(),
                }]
                .into(),
            })
            .await
            .expect("submit first Pi turn");
        loop {
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(5), thread.next_event())
                    .await
                    .expect("first Pi event timeout")
                    .expect("first Pi event result")
                    .expect("first Pi event stream should remain open");
            if let EventMsg::TurnComplete(event) = event.msg {
                assert_eq!(event.last_agent_message.as_deref(), Some("first reply"));
                break;
            }
        }
        let state = thread.persistence().await.expect("persisted Pi locator");
        thread.shutdown().await.expect("close first Pi session");

        let resumed = provider
            .resume(AgentRuntimeResumeRequest {
                thread_id,
                parent_thread_id: Some(parent_thread_id),
                cwd,
                state,
                host_tools: Vec::new(),
            })
            .await
            .expect("resume Pi session from persisted locator");
        assert_eq!(resumed.thread_id(), thread_id);
        resumed
            .submit(AgentRuntimeOperation {
                submission_id: "pi-turn-after-reload".to_string(),
                op: codex_protocol::protocol::Op::InterAgentCommunication {
                    communication: codex_protocol::protocol::InterAgentCommunication::new(
                        AgentPath::root(),
                        AgentPath::try_from("/root/pi_worker").expect("agent path"),
                        Vec::new(),
                        "say resumed".to_string(),
                        /*trigger_turn*/ true,
                    ),
                },
            })
            .await
            .expect("submit resumed Pi turn");
        loop {
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(5), resumed.next_event())
                    .await
                    .expect("resumed Pi event timeout")
                    .expect("resumed Pi event result")
                    .expect("resumed Pi event stream should remain open");
            if let EventMsg::TurnComplete(event) = event.msg {
                assert_eq!(event.last_agent_message.as_deref(), Some("resumed reply"));
                break;
            }
        }
        resumed.shutdown().await.expect("close resumed Pi session");
    }

    #[tokio::test]
    async fn round_trips_host_tool_calls_through_the_real_sidecar_when_configured() {
        let (Some(executable), Some(script)) = (
            std::env::var_os("PI_RUNTIME_TEST_EXECUTABLE"),
            std::env::var_os("PI_RUNTIME_TEST_SCRIPT"),
        ) else {
            return;
        };
        let working_directory = tempfile::tempdir().expect("test working directory");
        let mut config = PiRuntimeConfig::new(executable);
        config.arguments = vec![script];
        config.environment.insert(
            OsString::from("PI_CODEX_RUNTIME_FAUX_RESPONSES_JSON"),
            OsString::from(
                r#"[{"toolCall":{"name":"exec_command","arguments":{"cmd":"pwd"},"id":"host-call-1"}},"host tool done"]"#,
            ),
        );
        let provider = PiRuntimeProvider::new(config);
        let thread = provider
            .spawn(AgentRuntimeSpawnRequest {
                thread_id: ThreadId::new(),
                parent_thread_id: Some(ThreadId::new()),
                role: Some("pi_worker".to_string()),
                cwd: AbsolutePathBuf::from_absolute_path(working_directory.path())
                    .expect("absolute test path"),
                model: None,
                model_provider: "pi".to_string(),
                runtime_config: json!({ "provider": "faux", "model": "faux-1" }),
                host_tools: vec![AgentRuntimeHostToolDefinition {
                    name: "exec_command".to_string(),
                    description: "Run a command through Codex".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": { "cmd": { "type": "string" } },
                        "required": ["cmd"]
                    }),
                }],
            })
            .await
            .expect("spawn Pi host-tool session");
        thread
            .submit(AgentRuntimeOperation {
                submission_id: "pi-host-turn-1".to_string(),
                op: vec![UserInput::Text {
                    text: "use the host tool".to_string(),
                    text_elements: Vec::new(),
                }]
                .into(),
            })
            .await
            .expect("submit Pi prompt");

        let call = tokio::time::timeout(std::time::Duration::from_secs(5), thread.next_tool_call())
            .await
            .expect("host tool request timeout")
            .expect("host tool request")
            .expect("host tool channel open");
        assert_eq!(call.call_id, "host-call-1");
        assert_eq!(call.tool_name, "exec_command");
        assert_eq!(call.arguments, json!({ "cmd": "pwd" }));
        thread
            .submit_tool_result(AgentRuntimeToolResult {
                call_id: call.call_id,
                output: json!({
                    "content": [{ "type": "text", "text": working_directory.path().display().to_string() }],
                    "details": { "exit_code": 0 }
                }),
                is_error: false,
            })
            .await
            .expect("return host tool result");

        let mut completed = false;
        for _ in 0..32 {
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(5), thread.next_event())
                    .await
                    .expect("Pi event timeout")
                    .expect("Pi event result")
                    .expect("Pi event stream open");
            if let EventMsg::TurnComplete(event) = event.msg {
                assert_eq!(event.last_agent_message.as_deref(), Some("host tool done"));
                completed = true;
                break;
            }
        }
        assert!(completed);
        thread.shutdown().await.expect("close Pi host-tool session");
    }
}
