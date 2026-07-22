use crate::PiRuntimeConfig;
use crate::proto;
use crate::proto::envelope;
use codex_extension_api::AgentRuntimeError;
use codex_extension_api::AgentRuntimeErrorKind;
use prost::Message;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use tempfile::TempDir;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::net::unix::OwnedReadHalf;
use tokio::net::unix::OwnedWriteHalf;
use tokio::process::Child;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::Instant;
use tokio::time::sleep;
use tokio::time::timeout;

const PROTOCOL_VERSION: u32 = 2;
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

pub(crate) struct PiRuntimeClient {
    writer: Mutex<OwnedWriteHalf>,
    shared: Arc<ClientShared>,
    _child: Mutex<Child>,
    _socket_directory: TempDir,
    request_timeout: std::time::Duration,
    next_request_id: AtomicU64,
}

#[derive(Default)]
struct ClientShared {
    pending: Mutex<HashMap<String, oneshot::Sender<proto::RuntimeResponse>>>,
    sessions: Mutex<HashMap<String, mpsc::UnboundedSender<proto::SessionEvent>>>,
    host_tools: Mutex<HashMap<String, mpsc::UnboundedSender<proto::HostToolRequest>>>,
}

pub(crate) struct RegisteredSession {
    pub events: mpsc::UnboundedReceiver<proto::SessionEvent>,
    pub host_tools: mpsc::UnboundedReceiver<proto::HostToolRequest>,
}

impl PiRuntimeClient {
    pub(crate) async fn launch(config: &PiRuntimeConfig) -> Result<Self, AgentRuntimeError> {
        let socket_directory = tempfile::Builder::new()
            .prefix("codex-pi-runtime-")
            .tempdir()
            .map_err(|error| {
                unavailable(format!(
                    "failed to create Pi runtime socket directory: {error}"
                ))
            })?;
        let socket_path = socket_directory.path().join("runtime.sock");
        let mut command = Command::new(&config.executable);
        command
            .args(&config.arguments)
            .envs(&config.environment)
            .arg("--socket")
            .arg(&socket_path)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit());
        let mut child = command.spawn().map_err(|error| {
            unavailable(format!(
                "failed to start Pi runtime executable {}: {error}",
                config.executable.display()
            ))
        })?;
        let stream = connect_when_ready(&socket_path, &mut child, config.startup_timeout).await?;
        let (mut reader, mut writer) = stream.into_split();
        write_envelope(&mut writer, handshake_request()).await?;
        let response = read_envelope(&mut reader)
            .await?
            .ok_or_else(|| protocol_error("Pi runtime closed during handshake"))?;
        validate_handshake(response)?;

        let shared = Arc::new(ClientShared::default());
        tokio::spawn(read_loop(reader, Arc::clone(&shared)));
        Ok(Self {
            writer: Mutex::new(writer),
            shared,
            _child: Mutex::new(child),
            _socket_directory: socket_directory,
            request_timeout: config.request_timeout,
            next_request_id: AtomicU64::new(0),
        })
    }

    pub(crate) async fn register_session(
        &self,
        session_id: &str,
    ) -> Result<RegisteredSession, AgentRuntimeError> {
        let (event_sender, events) = mpsc::unbounded_channel();
        let (tool_sender, host_tools) = mpsc::unbounded_channel();
        if self
            .shared
            .sessions
            .lock()
            .await
            .insert(session_id.to_string(), event_sender)
            .is_some()
        {
            return Err(protocol_error(format!(
                "Pi runtime session already registered: {session_id}"
            )));
        }
        self.shared
            .host_tools
            .lock()
            .await
            .insert(session_id.to_string(), tool_sender);
        Ok(RegisteredSession { events, host_tools })
    }

    pub(crate) async fn unregister_session(&self, session_id: &str) {
        self.shared.sessions.lock().await.remove(session_id);
        self.shared.host_tools.lock().await.remove(session_id);
    }

    pub(crate) async fn send_host_tool_result(
        &self,
        result: proto::HostToolResult,
    ) -> Result<(), AgentRuntimeError> {
        write_envelope(
            &mut *self.writer.lock().await,
            proto::Envelope {
                protocol_version: PROTOCOL_VERSION,
                payload: Some(envelope::Payload::HostToolResult(result)),
            },
        )
        .await
    }

    pub(crate) async fn request(
        &self,
        session_id: &str,
        command: proto::runtime_request::Command,
    ) -> Result<proto::RuntimeResponse, AgentRuntimeError> {
        let request_id = format!(
            "pi-runtime-{}",
            self.next_request_id.fetch_add(1, Ordering::Relaxed)
        );
        let request = proto::RuntimeRequest {
            request_id: request_id.clone(),
            session_id: session_id.to_string(),
            command: Some(command),
        };
        let envelope = proto::Envelope {
            protocol_version: PROTOCOL_VERSION,
            payload: Some(envelope::Payload::Request(request)),
        };
        let (sender, receiver) = oneshot::channel();
        self.shared
            .pending
            .lock()
            .await
            .insert(request_id.clone(), sender);
        if let Err(error) = write_envelope(&mut *self.writer.lock().await, envelope).await {
            self.shared.pending.lock().await.remove(&request_id);
            return Err(error);
        }
        let response = timeout(self.request_timeout, receiver)
            .await
            .map_err(|_| {
                unavailable(format!(
                    "timed out waiting for Pi runtime request {request_id}"
                ))
            })?
            .map_err(|_| unavailable("Pi runtime connection closed before responding"))?;
        if let Some(proto::runtime_response::Result::Error(error)) = response.result.as_ref() {
            return Err(protocol_error(format!(
                "Pi runtime request failed ({}): {}",
                error.code, error.message
            )));
        }
        Ok(response)
    }
}

async fn connect_when_ready(
    socket_path: &Path,
    child: &mut Child,
    startup_timeout: std::time::Duration,
) -> Result<UnixStream, AgentRuntimeError> {
    let deadline = Instant::now() + startup_timeout;
    loop {
        match UnixStream::connect(socket_path).await {
            Ok(stream) => return Ok(stream),
            Err(connect_error) => {
                if let Some(status) = child.try_wait().map_err(|error| {
                    unavailable(format!("failed to inspect Pi runtime process: {error}"))
                })? {
                    return Err(unavailable(format!(
                        "Pi runtime exited before opening its socket ({status})"
                    )));
                }
                if Instant::now() >= deadline {
                    return Err(unavailable(format!(
                        "timed out connecting to Pi runtime socket: {connect_error}"
                    )));
                }
                sleep(std::time::Duration::from_millis(25)).await;
            }
        }
    }
}

async fn read_loop(mut reader: OwnedReadHalf, shared: Arc<ClientShared>) {
    loop {
        match read_envelope(&mut reader).await {
            Ok(Some(envelope)) => match envelope.payload {
                Some(envelope::Payload::Response(response)) => {
                    if let Some(sender) = shared.pending.lock().await.remove(&response.request_id) {
                        let _ = sender.send(response);
                    }
                }
                Some(envelope::Payload::Event(event)) => {
                    if let Some(sender) =
                        shared.sessions.lock().await.get(&event.session_id).cloned()
                    {
                        let _ = sender.send(event);
                    }
                }
                Some(envelope::Payload::HostToolRequest(request)) => {
                    if let Some(sender) = shared
                        .host_tools
                        .lock()
                        .await
                        .get(&request.session_id)
                        .cloned()
                    {
                        let _ = sender.send(request);
                    }
                }
                _ => tracing::warn!("ignoring unexpected Pi runtime envelope"),
            },
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(error = %error, "Pi runtime connection failed");
                break;
            }
        }
    }
    shared.pending.lock().await.clear();
    shared.sessions.lock().await.clear();
    shared.host_tools.lock().await.clear();
}

fn handshake_request() -> proto::Envelope {
    proto::Envelope {
        protocol_version: PROTOCOL_VERSION,
        payload: Some(envelope::Payload::HandshakeRequest(
            proto::HandshakeRequest {
                minimum_version: PROTOCOL_VERSION,
                maximum_version: PROTOCOL_VERSION,
                client_name: "codex-pi-runtime-provider".to_string(),
                client_version: env!("CARGO_PKG_VERSION").to_string(),
            },
        )),
    }
}

fn validate_handshake(envelope: proto::Envelope) -> Result<(), AgentRuntimeError> {
    let Some(envelope::Payload::HandshakeResponse(response)) = envelope.payload else {
        return Err(protocol_error("expected Pi runtime handshake response"));
    };
    if let Some(error) = response.error {
        return Err(protocol_error(format!(
            "Pi runtime handshake failed ({}): {}",
            error.code, error.message
        )));
    }
    if response.selected_version != PROTOCOL_VERSION {
        return Err(protocol_error(format!(
            "Pi runtime selected protocol {}, expected {PROTOCOL_VERSION}",
            response.selected_version
        )));
    }
    Ok(())
}

async fn write_envelope(
    writer: &mut OwnedWriteHalf,
    envelope: proto::Envelope,
) -> Result<(), AgentRuntimeError> {
    let bytes = envelope.encode_to_vec();
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(protocol_error(format!(
            "Pi runtime frame exceeds {MAX_FRAME_BYTES} bytes"
        )));
    }
    writer
        .write_u32(bytes.len() as u32)
        .await
        .map_err(|error| {
            unavailable(format!("failed to write Pi runtime frame length: {error}"))
        })?;
    writer
        .write_all(&bytes)
        .await
        .map_err(|error| unavailable(format!("failed to write Pi runtime frame: {error}")))?;
    writer
        .flush()
        .await
        .map_err(|error| unavailable(format!("failed to flush Pi runtime frame: {error}")))
}

async fn read_envelope(
    reader: &mut OwnedReadHalf,
) -> Result<Option<proto::Envelope>, AgentRuntimeError> {
    let length = match reader.read_u32().await {
        Ok(length) => length as usize,
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => {
            return Err(unavailable(format!(
                "failed to read Pi runtime frame length: {error}"
            )));
        }
    };
    if length > MAX_FRAME_BYTES {
        return Err(protocol_error(format!(
            "Pi runtime frame length {length} exceeds {MAX_FRAME_BYTES}"
        )));
    }
    let mut bytes = vec![0; length];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(|error| unavailable(format!("failed to read Pi runtime frame: {error}")))?;
    proto::Envelope::decode(bytes.as_slice())
        .map(Some)
        .map_err(|error| protocol_error(format!("failed to decode Pi runtime frame: {error}")))
}

fn unavailable(message: impl Into<String>) -> AgentRuntimeError {
    AgentRuntimeError::new(AgentRuntimeErrorKind::Unavailable, message)
}

pub(crate) fn protocol_error(message: impl Into<String>) -> AgentRuntimeError {
    AgentRuntimeError::new(AgentRuntimeErrorKind::Protocol, message)
}
