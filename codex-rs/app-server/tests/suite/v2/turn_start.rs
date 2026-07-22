use anyhow::Context;
use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::create_apply_patch_sse_response;
use app_test_support::create_exec_command_sse_response;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_repeating_assistant;
use app_test_support::create_mock_responses_server_sequence;
use app_test_support::create_mock_responses_server_sequence_unchecked;
use app_test_support::create_request_user_input_sse_response;
use app_test_support::create_shell_command_sse_response;
use app_test_support::format_with_current_shell_display;
use app_test_support::to_response;
use app_test_support::write_mock_responses_config_toml_with_chatgpt_base_url;
use app_test_support::write_models_cache;
use codex_app_server::INPUT_TOO_LARGE_ERROR_CODE;
use codex_app_server::INVALID_PARAMS_ERROR_CODE;
use codex_app_server_protocol::AdditionalContextEntry;
use codex_app_server_protocol::AdditionalContextKind;
use codex_app_server_protocol::AgentMessageDeltaNotification;
use codex_app_server_protocol::ByteRange;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::CollabAgentStatus;
use codex_app_server_protocol::CollabAgentTool;
use codex_app_server_protocol::CollabAgentToolCallStatus;
use codex_app_server_protocol::CommandExecutionApprovalDecision;
use codex_app_server_protocol::CommandExecutionRequestApprovalResponse;
use codex_app_server_protocol::CommandExecutionStatus;
use codex_app_server_protocol::FileChangeApprovalDecision;
use codex_app_server_protocol::FileChangePatchUpdatedNotification;
use codex_app_server_protocol::FileChangeRequestApprovalResponse;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ItemStartedNotification;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCNotification;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::PatchApplyStatus;
use codex_app_server_protocol::PatchChangeKind;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ServerRequestResolvedNotification;
use codex_app_server_protocol::SubAgentActivityKind;
use codex_app_server_protocol::TextElement;
use codex_app_server_protocol::ThreadDeleteParams;
use codex_app_server_protocol::ThreadDeleteResponse;
use codex_app_server_protocol::ThreadDeletedNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadLoadedListParams;
use codex_app_server_protocol::ThreadLoadedListResponse;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadSource;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnEnvironmentParams;
use codex_app_server_protocol::TurnInterruptParams;
use codex_app_server_protocol::TurnInterruptResponse;
use codex_app_server_protocol::TurnItemsView;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStartedNotification;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::TurnSteerParams;
use codex_app_server_protocol::UserInput as V2UserInput;
use codex_app_server_protocol::WarningNotification;
use codex_core::test_support::all_model_presets;
use codex_features::FEATURES;
use codex_features::Feature;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::MultiAgentMode;
use codex_protocol::config_types::Personality;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::config_types::Settings;
use codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_DANGER_FULL_ACCESS;
use codex_protocol::models::ImageDetail;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::MULTI_AGENT_MODE_OPEN_TAG;
use codex_protocol::user_input::MAX_USER_INPUT_TEXT_CHARS;
use codex_utils_absolute_path::test_support::PathExt;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_remote;
use core_test_support::skip_if_wine_exec;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::ResponseTemplate;

use super::analytics::mount_analytics_capture;
use super::analytics::wait_for_analytics_event;

#[cfg(windows)]
const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);
#[cfg(not(windows))]
const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const TEST_ORIGINATOR: &str = "codex_vscode";
const MULTI_AGENT_V2_NAMESPACE: &str = "collaboration";
const EXTERNAL_AGENT_RUNTIME_NAMESPACE: &str = "runtime_agents";
const INVALID_REQUEST_ERROR_CODE: i64 = -32600;
const TINY_PNG_BYTES: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 11, 73, 68, 65, 84, 120, 156, 99, 96, 0, 2, 0, 0, 5, 0, 1,
    122, 94, 171, 63, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];
const TINY_PNG_DATA_URL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

fn body_contains(req: &wiremock::Request, text: &str) -> bool {
    String::from_utf8(req.body.clone())
        .ok()
        .is_some_and(|body| body.contains(text))
}

async fn run_local_image_turn(detail: Option<ImageDetail>) -> Result<Vec<Value>> {
    // Two Codex turns hit the mock model (session start + turn/start).
    let responses = vec![
        create_final_assistant_message_sse_response("Done")?,
        create_final_assistant_message_sse_response("Done")?,
    ];
    // Use the unchecked variant because the strict matcher does not currently
    // cover image-bearing request payloads.
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::default(),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let image_path = codex_home.path().join("image.png");
    std::fs::write(&image_path, TINY_PNG_BYTES)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::LocalImage {
                path: image_path,
                detail,
            }],
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let TurnStartResponse { turn } = to_response::<TurnStartResponse>(turn_resp)?;
    assert!(!turn.id.is_empty());

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    received_response_input_images(&server).await
}

async fn received_response_input_images(server: &wiremock::MockServer) -> Result<Vec<Value>> {
    let requests = server
        .received_requests()
        .await
        .context("failed to fetch received requests")?;
    let mut input_images = Vec::new();

    for request in requests {
        if !request.url.path().ends_with("/responses") {
            continue;
        }
        let body = request
            .body_json::<Value>()
            .context("request body should be JSON")?;
        let Some(input) = body.get("input").and_then(Value::as_array) else {
            continue;
        };

        for item in input {
            if item.get("type").and_then(Value::as_str) != Some("message") {
                continue;
            }
            let Some(content) = item.get("content").and_then(Value::as_array) else {
                continue;
            };
            input_images.extend(
                content
                    .iter()
                    .filter(|span| span.get("type").and_then(Value::as_str) == Some("input_image"))
                    .cloned(),
            );
        }
    }

    Ok(input_images)
}

#[tokio::test]
async fn turn_start_with_empty_input_runs_model_request() -> Result<()> {
    let responses = vec![create_final_assistant_message_sse_response("Done")?];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::default(),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            thread_source: Some(ThreadSource::User),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: Vec::new(),
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let TurnStartResponse { turn } = to_response::<TurnStartResponse>(turn_resp)?;
    assert!(!turn.id.is_empty());

    let started_notif: JSONRPCNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/started"),
    )
    .await??;
    let started: TurnStartedNotification =
        serde_json::from_value(started_notif.params.expect("params must be present"))?;
    assert_eq!(started.thread_id, thread.id);
    assert_eq!(started.turn.id, turn.id);
    assert_eq!(started.turn.status, TurnStatus::InProgress);

    let completed_notif: JSONRPCNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let completed: TurnCompletedNotification = serde_json::from_value(
        completed_notif
            .params
            .expect("turn/completed params must be present"),
    )?;
    assert_eq!(completed.thread_id, thread.id);
    assert_eq!(completed.turn.id, turn.id);
    assert_eq!(completed.turn.status, TurnStatus::Completed);

    let requests = server
        .received_requests()
        .await
        .context("failed to fetch received requests")?;
    let response_requests = requests
        .iter()
        .filter(|request| request.url.path().ends_with("/responses"))
        .collect::<Vec<_>>();
    assert_eq!(response_requests.len(), 1);
    let body = response_requests[0]
        .body_json::<Value>()
        .context("request body should be JSON")?;
    let input = body
        .get("input")
        .and_then(Value::as_array)
        .context("request body should include input array")?;
    assert!(
        !input.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("message")
                && item.get("role").and_then(Value::as_str) == Some("user")
                && item
                    .get("content")
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty)
        }),
        "empty turn/start should not synthesize an empty user message: {input:?}"
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_additional_context_flows_to_model_input() -> Result<()> {
    let responses = vec![create_final_assistant_message_sse_response("Done")?];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::default(),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "inspect tab".to_string(),
                text_elements: Vec::new(),
            }],
            additional_context: Some(HashMap::from([(
                "custom_source".to_string(),
                AdditionalContextEntry {
                    value: "source value".to_string(),
                    kind: AdditionalContextKind::Untrusted,
                },
            )])),
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let requests = server
        .received_requests()
        .await
        .context("failed to fetch received requests")?;
    let request = requests
        .iter()
        .find(|request| request.url.path().ends_with("/responses"))
        .context("expected model request")?;
    let body = request
        .body_json::<Value>()
        .context("request body should be JSON")?;
    assert!(
        body.to_string()
            .contains("<external_custom_source>source value</external_custom_source>")
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_sends_originator_header() -> Result<()> {
    let responses = vec![create_final_assistant_message_sse_response("Done")?];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::from([(Feature::Personality, true)]),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.initialize_with_client_info(ClientInfo {
            name: TEST_ORIGINATOR.to_string(),
            title: Some("Codex VS Code Extension".to_string()),
            version: "0.1.0".to_string(),
        }),
    )
    .await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            thread_source: Some(ThreadSource::User),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let requests = server
        .received_requests()
        .await
        .expect("failed to fetch received requests");
    assert!(!requests.is_empty());
    for request in requests {
        let originator = request
            .headers
            .get("originator")
            .expect("originator header missing");
        assert_eq!(originator.to_str()?, TEST_ORIGINATOR);
    }

    Ok(())
}

#[tokio::test]
async fn turn_start_emits_user_message_item_with_text_elements() -> Result<()> {
    let responses = vec![create_final_assistant_message_sse_response("Done")?];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::from([(Feature::Personality, true)]),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            thread_source: Some(ThreadSource::User),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let text_elements = vec![TextElement::new(
        ByteRange { start: 0, end: 5 },
        Some("<note>".to_string()),
    )];
    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: Some("client-message-1".to_string()),
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: text_elements.clone(),
            }],
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;

    let user_message_item = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let notification = mcp
                .read_stream_until_notification_message("item/started")
                .await?;
            let params = notification.params.expect("item/started params");
            let item_started: ItemStartedNotification =
                serde_json::from_value(params).expect("deserialize item/started notification");
            if let ThreadItem::UserMessage { .. } = item_started.item {
                return Ok::<ThreadItem, anyhow::Error>(item_started.item);
            }
        }
    })
    .await??;

    match user_message_item {
        ThreadItem::UserMessage {
            client_id, content, ..
        } => {
            assert_eq!(client_id, Some("client-message-1".to_string()));
            assert_eq!(
                content,
                vec![V2UserInput::Text {
                    text: "Hello".to_string(),
                    text_elements,
                }]
            );
        }
        other => panic!("expected user message item, got {other:?}"),
    }

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    Ok(())
}

#[tokio::test]
async fn turn_start_emits_thread_scoped_warning_notification_for_trimmed_skills() -> Result<()> {
    let responses = vec![create_final_assistant_message_sse_response("Done")?];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::from([(Feature::Personality, true)]),
    )?;
    write_models_cache(codex_home.path())?;
    let cache_path = codex_home.path().join("models_cache.json");
    let mut cache: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cache_path)?)?;
    let models = cache["models"]
        .as_array_mut()
        .expect("models_cache.json models should be an array");
    let entry = models
        .first_mut()
        .expect("models cache should not be empty");
    let model = entry["slug"]
        .as_str()
        .expect("model slug should be present")
        .to_string();
    entry["context_window"] = serde_json::Value::from(100);
    std::fs::write(&cache_path, serde_json::to_string_pretty(&cache)?)?;
    let config_path = codex_home.path().join("config.toml");
    let config = std::fs::read_to_string(&config_path)?;
    std::fs::write(
        &config_path,
        config.replace("model = \"mock-model\"", &format!("model = \"{model}\"")),
    )?;
    write_test_skill(codex_home.path(), "alpha-skill")?;
    write_test_skill(codex_home.path(), "beta-skill")?;

    let isolated_home = codex_home.path().to_string_lossy();
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[
            ("HOME", Some(isolated_home.as_ref())),
            ("USERPROFILE", Some(isolated_home.as_ref())),
        ])
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;

    let notification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("warning"),
    )
    .await??;
    let params = notification.params.expect("warning params");
    let warning: WarningNotification =
        serde_json::from_value(params).expect("deserialize warning notification");
    assert_eq!(warning.thread_id.as_deref(), Some(thread.id.as_str()));
    assert_eq!(
        warning.message,
        "Exceeded skills context budget of 2%. All skill descriptions were removed and 7 additional skills were not included in the model-visible skills list."
    );

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let requests = server
        .received_requests()
        .await
        .expect("failed to fetch received requests");
    let request = requests
        .last()
        .expect("expected at least one model request");
    assert!(
        body_contains(request, "## Skills"),
        "expected outgoing request to include the skills section"
    );
    assert!(
        !body_contains(request, "- alpha-skill:") && !body_contains(request, "- beta-skill:"),
        "expected trimmed skills to be omitted from the outgoing request body"
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_sends_service_tier_id_to_model_request() -> Result<()> {
    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let response_mock = responses::mount_sse_once(&server, body).await;

    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::default(),
    )?;
    write_models_cache(codex_home.path())?;
    let service_tier_model = all_model_presets()
        .iter()
        .find(|preset| preset.show_in_picker && !preset.service_tiers.is_empty())
        .expect("bundled model catalog should include a picker model with service tiers");
    let service_tier_id = service_tier_model.service_tiers[0].id.clone();

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some(service_tier_model.id.clone()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            service_tier: Some(Some(service_tier_id.clone())),
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    assert_eq!(
        response_mock.single_request().body_json()["service_tier"],
        json!(service_tier_id)
    );

    Ok(())
}

#[tokio::test]
async fn thread_start_omits_empty_instruction_overrides_from_model_request() -> Result<()> {
    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let response_mock = responses::mount_sse_once(&server, body).await;

    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::default(),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            // TODO(aibrahim): Replace empty string instruction overrides with explicit tri-state
            // app-server semantics: omitted, explicitly none, or explicit value.
            config: Some(HashMap::from([(
                "include_permissions_instructions".to_string(),
                json!(false),
            )])),
            base_instructions: Some(String::new()),
            developer_instructions: Some(String::new()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let request_body = response_mock.single_request().body_json();
    let empty_developer_input_texts = request_body["input"]
        .as_array()
        .expect("input array")
        .iter()
        .filter(|item| item.get("role").and_then(serde_json::Value::as_str) == Some("developer"))
        .filter_map(|item| item.get("content").and_then(serde_json::Value::as_array))
        .flatten()
        .filter(|content| {
            content.get("type").and_then(serde_json::Value::as_str) == Some("input_text")
        })
        .filter_map(|content| content.get("text").and_then(serde_json::Value::as_str))
        .filter(|text| text.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        json!({
            "hasInstructions": request_body.get("instructions").is_some(),
            "emptyDeveloperInputTexts": empty_developer_input_texts,
        }),
        json!({
            "hasInstructions": false,
            "emptyDeveloperInputTexts": [],
        })
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_tracks_thread_originator_in_analytics() -> Result<()> {
    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_response_sequence(
        &server,
        vec![
            ResponseTemplate::new(500).set_body_json(json!({
                "error": {
                    "type": "server_error",
                    "message": "synthetic retryable error"
                }
            })),
            responses::sse_response(create_final_assistant_message_sse_response("Done")?),
        ],
    )
    .await;

    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml_with_chatgpt_base_url(
        codex_home.path(),
        &server.uri(),
        &server.uri(),
    )?;
    let config_path = codex_home.path().join("config.toml");
    let config = std::fs::read_to_string(&config_path)?
        .replace("stream_max_retries = 0", "stream_max_retries = 1");
    std::fs::write(config_path, config)?;
    mount_analytics_capture(&server, codex_home.path()).await?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_managed_config()
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            thread_source: Some(ThreadSource::User),
            service_name: Some("codex_work_desktop".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Image {
                url: TINY_PNG_DATA_URL.to_string(),
                detail: None,
            }],
            responsesapi_client_metadata: Some(HashMap::from([(
                "workspace_kind".to_string(),
                "projectless".to_string(),
            )])),
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let TurnStartResponse { turn } = to_response::<TurnStartResponse>(turn_resp)?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let event = wait_for_analytics_event(&server, DEFAULT_READ_TIMEOUT, "codex_turn_event").await?;
    assert_eq!(event["event_params"]["thread_id"], thread.id);
    assert_eq!(event["event_params"]["session_id"], thread.session_id);
    assert_eq!(event["event_params"]["turn_id"], turn.id);
    assert_eq!(
        event["event_params"]["app_server_client"]["product_client_id"],
        "codex_work_desktop"
    );
    assert_eq!(event["event_params"]["model"], "mock-model");
    assert_eq!(event["event_params"]["model_provider"], "mock_provider");
    assert_eq!(event["event_params"]["sandbox_policy"], "read_only");
    assert_eq!(event["event_params"]["workspace_kind"], "projectless");
    assert_eq!(event["event_params"]["ephemeral"], false);
    assert_eq!(event["event_params"]["thread_source"], "user");
    assert_eq!(event["event_params"]["initialization_mode"], "new");
    assert_eq!(
        event["event_params"]["subagent_source"],
        serde_json::Value::Null
    );
    assert_eq!(
        event["event_params"]["parent_thread_id"],
        serde_json::Value::Null
    );
    assert_eq!(event["event_params"]["num_input_images"], 1);
    assert_eq!(event["event_params"]["status"], "completed");
    assert!(event["event_params"]["started_at"].as_u64().is_some());
    assert!(event["event_params"]["completed_at"].as_u64().is_some());
    assert!(event["event_params"]["duration_ms"].as_u64().is_some());
    assert_eq!(event["event_params"]["input_tokens"], 0);
    assert_eq!(event["event_params"]["cached_input_tokens"], 0);
    assert_eq!(event["event_params"]["output_tokens"], 0);
    assert_eq!(event["event_params"]["reasoning_output_tokens"], 0);
    assert_eq!(event["event_params"]["total_tokens"], 0);
    let params = &event["event_params"];
    let timings_are_numbers = [
        "before_first_sampling_ms",
        "sampling_ms",
        "between_sampling_overhead_ms",
        "tool_blocking_ms",
        "after_last_sampling_ms",
    ]
    .into_iter()
    .all(|field| params[field].as_u64().is_some());
    assert_eq!(
        json!({
            "timingsAreNumbers": timings_are_numbers,
            "toolBlockingMs": params["tool_blocking_ms"],
            "samplingRequestCount": params["sampling_request_count"],
            "samplingRetryCount": params["sampling_retry_count"],
            "responseRequestCount": response_mock.requests().len(),
        }),
        json!({
            "timingsAreNumbers": true,
            "toolBlockingMs": 0,
            "samplingRequestCount": 2,
            "samplingRetryCount": 1,
            "responseRequestCount": 2,
        })
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn turn_profile_tracks_blocking_tool_and_follow_up_sampling() -> Result<()> {
    let responses = vec![
        create_request_user_input_sse_response("call1")?,
        create_final_assistant_message_sse_response("Done")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;

    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml_with_chatgpt_base_url(
        codex_home.path(),
        &server.uri(),
        &server.uri(),
    )?;
    mount_analytics_capture(&server, codex_home.path()).await?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_managed_config()
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "ask something".to_string(),
                text_elements: Vec::new(),
            }],
            collaboration_mode: Some(CollaborationMode {
                mode: ModeKind::Plan,
                settings: Settings {
                    model: "mock-model".to_string(),
                    reasoning_effort: Some(ReasoningEffort::Medium),
                    developer_instructions: None,
                },
            }),
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;

    let server_req = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::ToolRequestUserInput { request_id, .. } = server_req else {
        panic!("expected ToolRequestUserInput request, got: {server_req:?}");
    };
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    mcp.send_response(
        request_id,
        json!({
            "answers": {
                "confirm_path": { "answers": ["yes"] }
            }
        }),
    )
    .await?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let event = wait_for_analytics_event(&server, DEFAULT_READ_TIMEOUT, "codex_turn_event").await?;
    let params = &event["event_params"];
    assert_eq!(
        json!({
            "toolBlockingIsPositive": params["tool_blocking_ms"]
                .as_u64()
                .is_some_and(|duration| duration > 0),
            "samplingRequestCount": params["sampling_request_count"],
            "samplingRetryCount": params["sampling_retry_count"],
            "status": params["status"],
        }),
        json!({
            "toolBlockingIsPositive": true,
            "samplingRequestCount": 2,
            "samplingRetryCount": 0,
            "status": "completed",
        })
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_accepts_text_at_limit_with_mention_item() -> Result<()> {
    let responses = vec![create_final_assistant_message_sse_response("Done")?];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::from([(Feature::Personality, true)]),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            client_user_message_id: None,
            input: vec![
                V2UserInput::Text {
                    text: "x".repeat(MAX_USER_INPUT_TEXT_CHARS),
                    text_elements: Vec::new(),
                },
                V2UserInput::Mention {
                    name: "Demo App".to_string(),
                    path: "app://demo-app".to_string(),
                },
            ],
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let TurnStartResponse { turn } = to_response::<TurnStartResponse>(turn_resp)?;
    assert_eq!(turn.status, TurnStatus::InProgress);

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    Ok(())
}

#[tokio::test]
async fn turn_start_rejects_combined_oversized_text_input() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        "http://localhost/unused",
        "never",
        &BTreeMap::from([(Feature::Personality, true)]),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let first = "x".repeat(MAX_USER_INPUT_TEXT_CHARS / 2);
    let second = "y".repeat(MAX_USER_INPUT_TEXT_CHARS / 2 + 1);
    let actual_chars = first.chars().count() + second.chars().count();

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            client_user_message_id: None,
            input: vec![
                V2UserInput::Text {
                    text: first,
                    text_elements: Vec::new(),
                },
                V2UserInput::Text {
                    text: second,
                    text_elements: Vec::new(),
                },
            ],
            ..Default::default()
        })
        .await?;
    let err: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(turn_req)),
    )
    .await??;

    assert_eq!(err.error.code, INVALID_PARAMS_ERROR_CODE);
    assert_eq!(
        err.error.message,
        format!("Input exceeds the maximum length of {MAX_USER_INPUT_TEXT_CHARS} characters.")
    );
    let data = err.error.data.expect("expected structured error data");
    assert_eq!(data["input_error_code"], INPUT_TOO_LARGE_ERROR_CODE);
    assert_eq!(data["max_chars"], MAX_USER_INPUT_TEXT_CHARS);
    assert_eq!(data["actual_chars"], actual_chars);

    let turn_started = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        mcp.read_stream_until_notification_message("turn/started"),
    )
    .await;
    assert!(
        turn_started.is_err(),
        "did not expect a turn/started notification for rejected input"
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_rejects_invalid_permission_selection_before_starting_turn() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        "http://localhost/unused",
        "never",
        &BTreeMap::from([(Feature::Personality, true)]),
    )?;
    std::fs::write(
        codex_home.path().join("managed_config.toml"),
        "sandbox_mode = \"read-only\"\n",
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;
    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            permissions: Some(BUILT_IN_PERMISSION_PROFILE_DANGER_FULL_ACCESS.to_string()),
            ..Default::default()
        })
        .await?;
    let err: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(turn_req)),
    )
    .await??;

    assert_eq!(err.error.code, INVALID_REQUEST_ERROR_CODE);
    assert!(
        err.error
            .message
            .contains("`approval_policy = \"never\"` cannot be used"),
        "unexpected error message: {}",
        err.error.message
    );
    assert!(
        err.error
            .message
            .contains("requirements do not allow `sandbox_mode = \"danger-full-access\"`"),
        "unexpected error message: {}",
        err.error.message
    );
    let turn_started = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        mcp.read_stream_until_notification_message("turn/started"),
    )
    .await;
    assert!(
        turn_started.is_err(),
        "did not expect a turn/started notification after rejected permissions selection"
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_rejects_unknown_environment_before_starting_turn() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::default(),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            environments: Some(vec![TurnEnvironmentParams {
                environment_id: "missing".to_string(),
                cwd: codex_utils_absolute_path::AbsolutePathBuf::try_from(
                    codex_home.path().to_path_buf(),
                )?
                .into(),
            }]),
            ..Default::default()
        })
        .await?;
    let err: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(turn_req)),
    )
    .await??;

    assert_eq!(err.id, RequestId::Integer(turn_req));
    assert_eq!(err.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(err.error.message, "unknown turn environment id `missing`");
    let turn_started = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        mcp.read_stream_until_notification_message("turn/started"),
    )
    .await;
    assert!(
        turn_started.is_err(),
        "did not expect a turn/started notification after rejected environments"
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_emits_notifications_and_accepts_model_override() -> Result<()> {
    // Provide a mock server and config so model wiring is valid.
    // Three Codex turns hit the mock model (session start + two turn/start calls).
    let responses = vec![
        create_final_assistant_message_sse_response("Done")?,
        create_final_assistant_message_sse_response("Done")?,
        create_final_assistant_message_sse_response("Done")?,
    ];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::from([(Feature::Personality, true)]),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    // Start a thread (v2) and capture its id.
    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    // Start a turn with only input and thread_id set (no overrides).
    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let TurnStartResponse { turn } = to_response::<TurnStartResponse>(turn_resp)?;
    assert!(!turn.id.is_empty());

    // Expect a turn/started notification.
    let notif: JSONRPCNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/started"),
    )
    .await??;
    let started: TurnStartedNotification =
        serde_json::from_value(notif.params.expect("params must be present"))?;
    assert_eq!(started.thread_id, thread.id);
    assert_eq!(
        started.turn.status,
        codex_app_server_protocol::TurnStatus::InProgress
    );
    assert_eq!(started.turn.id, turn.id);
    assert_eq!(started.turn.items_view, TurnItemsView::NotLoaded);
    assert!(started.turn.items.is_empty());

    let completed_notif: JSONRPCNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let completed: TurnCompletedNotification = serde_json::from_value(
        completed_notif
            .params
            .expect("turn/completed params must be present"),
    )?;
    assert_eq!(completed.thread_id, thread.id);
    assert_eq!(completed.turn.id, turn.id);
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    assert_eq!(completed.turn.items_view, TurnItemsView::NotLoaded);
    assert!(completed.turn.items.is_empty());

    // Send a second turn that exercises the overrides path: change the model.
    let turn_req2 = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "Second".to_string(),
                text_elements: Vec::new(),
            }],
            model: Some("mock-model-override".to_string()),
            ..Default::default()
        })
        .await?;
    let turn_resp2: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req2)),
    )
    .await??;
    let TurnStartResponse { turn: turn2 } = to_response::<TurnStartResponse>(turn_resp2)?;
    assert!(!turn2.id.is_empty());
    // Ensure the second turn has a different id than the first.
    assert_ne!(turn.id, turn2.id);

    let notif2: JSONRPCNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/started"),
    )
    .await??;
    let started2: TurnStartedNotification =
        serde_json::from_value(notif2.params.expect("params must be present"))?;
    assert_eq!(started2.thread_id, thread.id);
    assert_eq!(started2.turn.id, turn2.id);
    assert_eq!(started2.turn.status, TurnStatus::InProgress);
    assert_eq!(started2.turn.items_view, TurnItemsView::NotLoaded);
    assert!(started2.turn.items.is_empty());

    let completed_notif2: JSONRPCNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let completed2: TurnCompletedNotification = serde_json::from_value(
        completed_notif2
            .params
            .expect("turn/completed params must be present"),
    )?;
    assert_eq!(completed2.thread_id, thread.id);
    assert_eq!(completed2.turn.id, turn2.id);
    assert_eq!(completed2.turn.status, TurnStatus::Completed);
    assert_eq!(completed2.turn.items_view, TurnItemsView::NotLoaded);
    assert!(completed2.turn.items.is_empty());

    Ok(())
}

#[tokio::test]
async fn turn_start_accepts_collaboration_mode_override_v2() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let response_mock = responses::mount_sse_once(&server, body).await;

    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::default(),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("gpt-5.4".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model: "mock-model-collab".to_string(),
            reasoning_effort: Some(ReasoningEffort::High),
            developer_instructions: None,
        },
    };

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            model: Some("mock-model-override".to_string()),
            effort: Some(ReasoningEffort::Low),
            summary: Some(ReasoningSummary::Auto),
            output_schema: None,
            collaboration_mode: Some(collaboration_mode),
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let _turn: TurnStartResponse = to_response::<TurnStartResponse>(turn_resp)?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let request = response_mock.single_request();
    let payload = request.body_json();
    assert_eq!(payload["model"].as_str(), Some("mock-model-collab"));
    let payload_text = payload.to_string();
    assert!(payload_text.contains(
        "Use the `request_user_input` tool only when it is listed in the available tools"
    ));

    Ok(())
}

#[tokio::test]
async fn turn_start_uses_thread_feature_overrides_for_request_user_input_tool_description_v2()
-> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let response_mock = responses::mount_sse_once(&server, body).await;

    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::default(),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("gpt-5.4".to_string()),
            config: Some(HashMap::from([(
                "features.default_mode_request_user_input".to_string(),
                json!(true),
            )])),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model: "mock-model-collab".to_string(),
            reasoning_effort: Some(ReasoningEffort::High),
            developer_instructions: None,
        },
    };

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            model: Some("mock-model-override".to_string()),
            effort: Some(ReasoningEffort::Low),
            summary: Some(ReasoningSummary::Auto),
            output_schema: None,
            collaboration_mode: Some(collaboration_mode),
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let _turn: TurnStartResponse = to_response::<TurnStartResponse>(turn_resp)?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let request = response_mock.single_request();
    let payload_text = request.body_json().to_string();
    assert!(payload_text.contains("This tool is only available in Default or Plan mode."));

    Ok(())
}

#[tokio::test]
async fn turn_start_accepts_personality_override_v2() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let response_mock = responses::mount_sse_once(&server, body).await;

    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::from([(Feature::Personality, true)]),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("exp-codex-personality".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            personality: Some(Personality::Friendly),
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let _turn: TurnStartResponse = to_response::<TurnStartResponse>(turn_resp)?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let request = response_mock.single_request();
    let developer_texts = request.message_input_texts("developer");
    if developer_texts.is_empty() {
        eprintln!("request body: {}", request.body_json());
    }

    assert!(
        developer_texts
            .iter()
            .any(|text| text.contains("<personality_spec>")),
        "expected personality update message in developer input, got {developer_texts:?}"
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_ignores_deprecated_multi_agent_mode() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let response_mock = responses::mount_sse_once(&server, body).await;

    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::from([(Feature::MultiAgentV2, true)]),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            multi_agent_mode: Some(MultiAgentMode::Proactive),
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let _: TurnStartResponse = to_response(turn_resp)?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let developer_texts = response_mock
        .single_request()
        .message_input_texts("developer");
    assert!(developer_texts.iter().any(|text| {
        text.contains(
            "Do not spawn sub-agents unless the user or applicable AGENTS.md/skill instructions explicitly ask for sub-agents",
        )
    }));
    assert!(
        !developer_texts
            .iter()
            .any(|text| text.contains("Proactive multi-agent delegation is active."))
    );

    Ok(())
}

#[tokio::test]
async fn thread_start_ignores_deprecated_multi_agent_mode() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let response_mock = responses::mount_sse_once(&server, body).await;

    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::from([(Feature::MultiAgentV2, true)]),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            multi_agent_mode: Some(MultiAgentMode::Proactive),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse {
        thread,
        multi_agent_mode,
        ..
    } = to_response::<ThreadStartResponse>(thread_resp)?;
    assert_eq!(multi_agent_mode, MultiAgentMode::ExplicitRequestOnly);

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let _: TurnStartResponse = to_response(turn_resp)?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let developer_texts = response_mock
        .single_request()
        .message_input_texts("developer");
    assert!(developer_texts.iter().any(|text| {
        text.contains(MULTI_AGENT_MODE_OPEN_TAG)
            && text.contains(
                "Do not spawn sub-agents unless the user or applicable AGENTS.md/skill instructions explicitly ask for sub-agents",
            )
    }));
    assert!(
        !developer_texts
            .iter()
            .any(|text| text.contains("Proactive multi-agent delegation is active."))
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_change_personality_mid_thread_v2() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let sse1 = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let sse2 = responses::sse(vec![
        responses::ev_response_created("resp-2"),
        responses::ev_assistant_message("msg-2", "Done"),
        responses::ev_completed("resp-2"),
    ]);
    let response_mock = responses::mount_sse_sequence(&server, vec![sse1, sse2]).await;

    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::from([(Feature::Personality, true)]),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("exp-codex-personality".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            personality: None,
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let _turn: TurnStartResponse = to_response::<TurnStartResponse>(turn_resp)?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let turn_req2 = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "Hello again".to_string(),
                text_elements: Vec::new(),
            }],
            personality: Some(Personality::Friendly),
            ..Default::default()
        })
        .await?;
    let turn_resp2: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req2)),
    )
    .await??;
    let _turn2: TurnStartResponse = to_response::<TurnStartResponse>(turn_resp2)?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2, "expected two requests");

    let first_developer_texts = requests[0].message_input_texts("developer");
    assert!(
        first_developer_texts
            .iter()
            .all(|text| !text.contains("<personality_spec>")),
        "expected no personality update message in first request, got {first_developer_texts:?}"
    );

    let second_developer_texts = requests[1].message_input_texts("developer");
    assert!(
        second_developer_texts
            .iter()
            .any(|text| text.contains("<personality_spec>")),
        "expected personality update message in second request, got {second_developer_texts:?}"
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_defaults_local_image_detail_to_high() -> Result<()> {
    let input_images = run_local_image_turn(/*detail*/ None).await?;

    assert_eq!(input_images.len(), 1);
    assert_eq!(
        input_images[0].get("detail").and_then(Value::as_str),
        Some("high")
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_forwards_custom_local_image_detail() -> Result<()> {
    let input_images = run_local_image_turn(Some(ImageDetail::Original)).await?;

    assert_eq!(input_images.len(), 1);
    assert_eq!(
        input_images[0].get("detail").and_then(Value::as_str),
        Some("original")
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_exec_approval_toggle_v2() -> Result<()> {
    // TODO(anp): Remove after shell-command approval routing supports target-native Windows cwd.
    skip_if_wine_exec!(
        Ok(()),
        "shell-command approval routing requires a host-native cwd under Wine-exec"
    );
    skip_if_no_network!(Ok(()));

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().to_path_buf();

    // Mock server: first turn requests a shell call (elicitation), then completes.
    // Second turn same, but we'll set approval_policy=never to avoid elicitation.
    let responses = vec![
        create_shell_command_sse_response(
            vec![
                "python3".to_string(),
                "-c".to_string(),
                "print(42)".to_string(),
            ],
            /*workdir*/ None,
            Some(5000),
            "call1",
        )?,
        create_final_assistant_message_sse_response("done 1")?,
        create_shell_command_sse_response(
            vec![
                "python3".to_string(),
                "-c".to_string(),
                "print(42)".to_string(),
            ],
            /*workdir*/ None,
            Some(5000),
            "call2",
        )?,
        create_final_assistant_message_sse_response("done 2")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;
    // Default approval is untrusted to force elicitation on first turn.
    create_config_toml(
        codex_home.as_path(),
        &server.uri(),
        "untrusted",
        &BTreeMap::default(),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.as_path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let expected_environment_id = mcp.auto_env_params()?.environment_id;

    // thread/start
    let start_id = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_id)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(start_resp)?;

    // turn/start — expect CommandExecutionRequestApproval request from server
    let first_turn_id = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "run python".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    // Acknowledge RPC
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(first_turn_id)),
    )
    .await??;

    // Receive elicitation
    let server_req = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::CommandExecutionRequestApproval { request_id, params } = server_req else {
        panic!("expected CommandExecutionRequestApproval request");
    };
    assert_eq!(params.item_id, "call1");
    assert_eq!(
        params.environment_id.as_deref(),
        Some(expected_environment_id.as_str())
    );
    let resolved_request_id = request_id.clone();

    // Approve and wait for task completion
    mcp.send_response(
        request_id,
        serde_json::to_value(CommandExecutionRequestApprovalResponse {
            decision: CommandExecutionApprovalDecision::Accept,
        })?,
    )
    .await?;
    let mut saw_resolved = false;
    loop {
        let message = timeout(DEFAULT_READ_TIMEOUT, mcp.read_next_message()).await??;
        let JSONRPCMessage::Notification(notification) = message else {
            continue;
        };
        match notification.method.as_str() {
            "serverRequest/resolved" => {
                let resolved: ServerRequestResolvedNotification = serde_json::from_value(
                    notification
                        .params
                        .clone()
                        .expect("serverRequest/resolved params"),
                )?;
                assert_eq!(resolved.thread_id, thread.id);
                assert_eq!(resolved.request_id, resolved_request_id);
                saw_resolved = true;
            }
            "turn/completed" => {
                assert!(saw_resolved, "serverRequest/resolved should arrive first");
                break;
            }
            _ => {}
        }
    }

    // Second turn with approval_policy=never should not elicit approval
    let second_turn_id = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "run python again".to_string(),
                text_elements: Vec::new(),
            }],
            approval_policy: Some(codex_app_server_protocol::AskForApproval::Never),
            sandbox_policy: Some(codex_app_server_protocol::SandboxPolicy::DangerFullAccess),
            model: Some("mock-model".to_string()),
            effort: Some(ReasoningEffort::Medium),
            summary: Some(ReasoningSummary::Auto),
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(second_turn_id)),
    )
    .await??;

    // Ensure we do NOT receive a CommandExecutionRequestApproval request before task completes
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    Ok(())
}

#[tokio::test]
async fn turn_start_exec_approval_decline_v2() -> Result<()> {
    // TODO(anp): Remove after command approval routing accepts target-native Windows cwd.
    skip_if_wine_exec!(
        Ok(()),
        "command approval routing rejects the selected Windows cwd on the Linux host"
    );
    skip_if_no_network!(Ok(()));

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().to_path_buf();

    let responses = vec![
        create_shell_command_sse_response(
            vec![
                "python3".to_string(),
                "-c".to_string(),
                "print(42)".to_string(),
            ],
            /*workdir*/ None,
            Some(5000),
            "call-decline",
        )?,
        create_final_assistant_message_sse_response("done")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;
    create_config_toml(
        codex_home.as_path(),
        &server.uri(),
        "untrusted",
        &BTreeMap::default(),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.as_path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let start_id = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_id)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(start_resp)?;

    let turn_id = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "run python".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_id)),
    )
    .await??;
    let TurnStartResponse { turn } = to_response::<TurnStartResponse>(turn_resp)?;

    let started_command_execution = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let started_notif = mcp
                .read_stream_until_notification_message("item/started")
                .await?;
            let started: ItemStartedNotification =
                serde_json::from_value(started_notif.params.clone().expect("item/started params"))?;
            if let ThreadItem::CommandExecution { .. } = started.item {
                return Ok::<ThreadItem, anyhow::Error>(started.item);
            }
        }
    })
    .await??;
    let ThreadItem::CommandExecution { id, status, .. } = started_command_execution else {
        unreachable!("loop ensures we break on command execution items");
    };
    assert_eq!(id, "call-decline");
    assert_eq!(status, CommandExecutionStatus::InProgress);

    let server_req = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::CommandExecutionRequestApproval { request_id, params } = server_req else {
        panic!("expected CommandExecutionRequestApproval request")
    };
    assert_eq!(params.item_id, "call-decline");
    assert_eq!(params.thread_id, thread.id);
    assert_eq!(params.turn_id, turn.id);

    mcp.send_response(
        request_id,
        serde_json::to_value(CommandExecutionRequestApprovalResponse {
            decision: CommandExecutionApprovalDecision::Decline,
        })?,
    )
    .await?;

    let completed_command_execution = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let completed_notif = mcp
                .read_stream_until_notification_message("item/completed")
                .await?;
            let completed: ItemCompletedNotification = serde_json::from_value(
                completed_notif
                    .params
                    .clone()
                    .expect("item/completed params"),
            )?;
            if let ThreadItem::CommandExecution { .. } = completed.item {
                return Ok::<ThreadItem, anyhow::Error>(completed.item);
            }
        }
    })
    .await??;
    let ThreadItem::CommandExecution {
        id,
        status,
        exit_code,
        aggregated_output,
        ..
    } = completed_command_execution
    else {
        unreachable!("loop ensures we break on command execution items");
    };
    assert_eq!(id, "call-decline");
    assert_eq!(status, CommandExecutionStatus::Declined);
    assert!(exit_code.is_none());
    assert!(aggregated_output.is_none());

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    Ok(())
}

#[tokio::test]
async fn turn_start_updates_sandbox_and_cwd_between_turns_v2() -> Result<()> {
    // TODO(anp): Materialize cwd and shell-display fixtures in the selected remote environment.
    skip_if_remote!(Ok(()), "cwd fixtures are only materialized on the host");
    skip_if_no_network!(Ok(()));

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let workspace_root = tmp.path().join("workspace");
    std::fs::create_dir(&workspace_root)?;
    let first_cwd = workspace_root.join("turn1");
    let second_cwd = workspace_root.join("turn2");
    std::fs::create_dir(&first_cwd)?;
    std::fs::create_dir(&second_cwd)?;

    let responses = vec![
        create_shell_command_sse_response(
            vec!["echo".to_string(), "first".to_string(), "turn".to_string()],
            /*workdir*/ None,
            Some(5000),
            "call-first",
        )?,
        create_final_assistant_message_sse_response("done first")?,
        create_shell_command_sse_response(
            vec!["echo".to_string(), "second".to_string(), "turn".to_string()],
            /*workdir*/ None,
            Some(5000),
            "call-second",
        )?,
        create_final_assistant_message_sse_response("done second")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;
    create_config_toml(
        &codex_home,
        &server.uri(),
        "untrusted",
        &BTreeMap::default(),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(&codex_home)
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    // thread/start
    let start_id = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_id)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(start_resp)?;

    // first turn with workspace-write sandbox and first_cwd
    let first_turn = mcp
        .send_turn_start_request(TurnStartParams {
            environments: None,
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "first turn".to_string(),
                text_elements: Vec::new(),
            }],
            responsesapi_client_metadata: None,
            additional_context: None,
            cwd: Some(first_cwd.clone()),
            runtime_workspace_roots: None,
            approval_policy: Some(codex_app_server_protocol::AskForApproval::Never),
            approvals_reviewer: None,
            sandbox_policy: Some(codex_app_server_protocol::SandboxPolicy::WorkspaceWrite {
                writable_roots: vec![first_cwd.try_into()?],
                network_access: false,
                exclude_tmpdir_env_var: true,
                exclude_slash_tmp: true,
            }),
            permissions: None,
            model: Some("mock-model".to_string()),
            effort: Some(ReasoningEffort::Medium),
            summary: Some(ReasoningSummary::Auto),
            service_tier: None,
            personality: None,
            output_schema: None,
            collaboration_mode: None,
            multi_agent_mode: None,
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(first_turn)),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    mcp.clear_message_buffer();

    // second turn with workspace-write and second_cwd, ensure exec begins in second_cwd
    let second_turn = mcp
        .send_turn_start_request(TurnStartParams {
            environments: None,
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "second turn".to_string(),
                text_elements: Vec::new(),
            }],
            responsesapi_client_metadata: None,
            additional_context: None,
            cwd: Some(second_cwd.clone()),
            runtime_workspace_roots: None,
            approval_policy: Some(codex_app_server_protocol::AskForApproval::Never),
            approvals_reviewer: None,
            sandbox_policy: Some(codex_app_server_protocol::SandboxPolicy::DangerFullAccess),
            permissions: None,
            model: Some("mock-model".to_string()),
            effort: Some(ReasoningEffort::Medium),
            summary: Some(ReasoningSummary::Auto),
            service_tier: None,
            personality: None,
            output_schema: None,
            collaboration_mode: None,
            multi_agent_mode: None,
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(second_turn)),
    )
    .await??;

    let command_exec_item = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let item_started_notification = mcp
                .read_stream_until_notification_message("item/started")
                .await?;
            let params = item_started_notification
                .params
                .clone()
                .expect("item/started params");
            let item_started: ItemStartedNotification =
                serde_json::from_value(params).expect("deserialize item/started notification");
            if matches!(item_started.item, ThreadItem::CommandExecution { .. }) {
                return Ok::<ThreadItem, anyhow::Error>(item_started.item);
            }
        }
    })
    .await??;
    let ThreadItem::CommandExecution {
        cwd,
        command,
        status,
        ..
    } = command_exec_item
    else {
        unreachable!("loop ensures we break on command execution items");
    };
    assert_eq!(cwd.as_str(), second_cwd.to_string_lossy().as_ref());
    let expected_command = format_with_current_shell_display("echo second turn");
    assert_eq!(command, expected_command);
    assert_eq!(status, CommandExecutionStatus::InProgress);

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn turn_start_permission_profile_rebinds_runtime_workspace_roots_between_turns() -> Result<()>
{
    skip_if_no_network!(Ok(()));

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let old_root = tmp.path().join("old-root");
    let new_root = tmp.path().join("new-root");
    std::fs::create_dir(&old_root)?;
    std::fs::create_dir(&new_root)?;
    let old_root_text = old_root.to_string_lossy().into_owned();
    let new_root_text = new_root.to_string_lossy().into_owned();
    let old_root = codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(old_root)?;
    let new_root = codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(new_root)?;

    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-1"),
                responses::ev_assistant_message("msg-1", "done first"),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-2"),
                responses::ev_assistant_message("msg-2", "done second"),
                responses::ev_completed("resp-2"),
            ]),
        ],
    )
    .await;
    let server_uri = server.uri();
    std::fs::write(
        codex_home.join("config.toml"),
        format!(
            r#"
model = "mock-model"
approval_policy = "never"
default_permissions = "dev"
model_provider = "mock_provider"

[model_providers.mock_provider]
name = "Mock provider for test"
base_url = "{server_uri}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0

[permissions.dev.filesystem.":workspace_roots"]
"." = "write"
"#
        ),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(&codex_home)
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let start_id = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_id)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(start_resp)?;

    let first_turn_id = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "select dev profile".to_string(),
                text_elements: Vec::new(),
            }],
            runtime_workspace_roots: Some(vec![old_root]),
            permissions: Some("dev".to_string()),
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(first_turn_id)),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let second_turn_id = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "write in new root".to_string(),
                text_elements: Vec::new(),
            }],
            runtime_workspace_roots: Some(vec![new_root]),
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(second_turn_id)),
    )
    .await??;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2, "expected two Responses API requests");
    let latest_permissions_instructions =
        |request: &core_test_support::responses::ResponsesRequest| {
            request
                .message_input_texts("developer")
                .into_iter()
                .rev()
                .find(|text| text.contains("<permissions instructions>"))
                .expect("permissions instructions")
        };
    let first_permissions = latest_permissions_instructions(&requests[0]);
    assert!(first_permissions.contains(&old_root_text));
    assert!(
        !first_permissions.contains(&new_root_text),
        "first turn should materialize the initial runtime workspace root"
    );

    let second_permissions = latest_permissions_instructions(&requests[1]);
    assert!(second_permissions.contains(&new_root_text));
    assert!(
        !second_permissions.contains(&old_root_text),
        "second turn should rebind :workspace_roots to the updated runtime workspace root"
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_resolves_sticky_thread_local_environment_and_turn_overrides() -> Result<()> {
    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace)?;

    let server = create_mock_responses_server_repeating_assistant("done").await;
    create_config_toml(&codex_home, &server.uri(), "never", &BTreeMap::default())?;
    std::fs::write(
        codex_home.join("environments.toml"),
        r#"
[[environments]]
id = "remote"
url = "ws://127.0.0.1:1"
"#,
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(&codex_home)
        // This test owns environments.toml and explicitly compares local selections
        // with a configured remote environment, so auto env would change its subject.
        .without_auto_env()
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    for case in [
        EnvironmentSelectionCase {
            name: "sticky_unset_turn_unset",
            sticky: None,
            turn: None,
        },
        EnvironmentSelectionCase {
            name: "sticky_empty_turn_unset",
            sticky: Some(&[]),
            turn: None,
        },
        EnvironmentSelectionCase {
            name: "sticky_local_turn_unset",
            sticky: Some(&["local"]),
            turn: None,
        },
        EnvironmentSelectionCase {
            name: "sticky_local_turn_empty",
            sticky: Some(&["local"]),
            turn: Some(&[]),
        },
        EnvironmentSelectionCase {
            name: "sticky_empty_turn_local",
            sticky: Some(&[]),
            turn: Some(&["local"]),
        },
    ] {
        run_environment_selection_case(&mut mcp, &workspace, case).await?;
    }

    Ok(())
}

struct EnvironmentSelectionCase {
    name: &'static str,
    sticky: Option<&'static [&'static str]>,
    turn: Option<&'static [&'static str]>,
}

async fn run_environment_selection_case(
    mcp: &mut TestAppServer,
    workspace: &Path,
    case: EnvironmentSelectionCase,
) -> Result<()> {
    let thread_req = mcp
        .send_thread_start_request(ThreadStartParams {
            model: Some("mock-model".to_string()),
            cwd: Some(workspace.to_string_lossy().into_owned()),
            environments: environment_params(case.sticky, workspace),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: format!("run {}", case.name),
                text_elements: Vec::new(),
            }],
            environments: environment_params(case.turn, workspace),
            cwd: Some(workspace.to_path_buf()),
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let TurnStartResponse { turn } = to_response::<TurnStartResponse>(turn_resp)?;

    let started_notification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/started"),
    )
    .await??;
    let started: TurnStartedNotification = serde_json::from_value(
        started_notification
            .params
            .ok_or_else(|| anyhow::anyhow!("turn/started notification should include params"))?,
    )?;
    assert_eq!(started.turn.id, turn.id, "{}", case.name);

    let completed_notification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let completed: TurnCompletedNotification =
        serde_json::from_value(completed_notification.params.ok_or_else(|| {
            anyhow::anyhow!("turn/completed notification should include params")
        })?)?;
    assert_eq!(completed.turn.id, turn.id, "{}", case.name);
    assert_eq!(
        completed.turn.status,
        TurnStatus::Completed,
        "{}",
        case.name
    );

    mcp.clear_message_buffer();

    Ok(())
}

fn environment_params(ids: Option<&[&str]>, cwd: &Path) -> Option<Vec<TurnEnvironmentParams>> {
    ids.map(|ids| {
        ids.iter()
            .map(|id| TurnEnvironmentParams {
                environment_id: (*id).to_string(),
                cwd: cwd.abs().into(),
            })
            .collect()
    })
}

#[tokio::test]
async fn turn_start_file_change_approval_v2() -> Result<()> {
    // TODO(anp): Materialize apply-patch workspaces in the selected remote environment.
    skip_if_remote!(
        Ok(()),
        "apply-patch workspace fixture is only materialized on the host"
    );
    skip_if_no_network!(Ok(()));

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace)?;

    let patch = r#"*** Begin Patch
*** Add File: README.md
+new line
*** End Patch
"#;
    let responses = vec![
        create_apply_patch_sse_response(patch, "patch-call")?,
        create_final_assistant_message_sse_response("patch applied")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;
    create_config_toml(
        &codex_home,
        &server.uri(),
        "untrusted",
        &BTreeMap::default(),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(&codex_home)
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let start_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            cwd: Some(workspace.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(start_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "apply patch".into(),
                text_elements: Vec::new(),
            }],
            cwd: Some(workspace.clone()),
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let TurnStartResponse { turn } = to_response::<TurnStartResponse>(turn_resp)?;

    let started_file_change = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let started_notif = mcp
                .read_stream_until_notification_message("item/started")
                .await?;
            let started: ItemStartedNotification =
                serde_json::from_value(started_notif.params.clone().expect("item/started params"))?;
            if let ThreadItem::FileChange { .. } = started.item {
                return Ok::<ThreadItem, anyhow::Error>(started.item);
            }
        }
    })
    .await??;
    let ThreadItem::FileChange {
        ref id,
        status,
        ref changes,
    } = started_file_change
    else {
        unreachable!("loop ensures we break on file change items");
    };
    assert_eq!(id, "patch-call");
    assert_eq!(status, PatchApplyStatus::InProgress);
    let started_changes = changes.clone();

    let server_req = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::FileChangeRequestApproval { request_id, params } = server_req else {
        panic!("expected FileChangeRequestApproval request")
    };
    assert_eq!(params.item_id, "patch-call");
    assert_eq!(params.thread_id, thread.id);
    assert_eq!(params.turn_id, turn.id);
    let resolved_request_id = request_id.clone();
    let expected_readme_path = workspace.join("README.md");
    let expected_readme_path = expected_readme_path.to_string_lossy().into_owned();
    pretty_assertions::assert_eq!(
        started_changes,
        vec![codex_app_server_protocol::FileUpdateChange {
            path: expected_readme_path.clone(),
            kind: PatchChangeKind::Add,
            diff: "new line\n".to_string(),
        }]
    );

    mcp.send_response(
        request_id,
        serde_json::to_value(FileChangeRequestApprovalResponse {
            decision: FileChangeApprovalDecision::Accept,
        })?,
    )
    .await?;
    let mut saw_resolved = false;
    let mut completed_file_change: Option<ThreadItem> = None;
    while completed_file_change.is_none() {
        let message = timeout(DEFAULT_READ_TIMEOUT, mcp.read_next_message()).await??;
        let JSONRPCMessage::Notification(notification) = message else {
            continue;
        };
        match notification.method.as_str() {
            "serverRequest/resolved" => {
                let resolved: ServerRequestResolvedNotification = serde_json::from_value(
                    notification
                        .params
                        .clone()
                        .expect("serverRequest/resolved params"),
                )?;
                assert_eq!(resolved.thread_id, thread.id);
                assert_eq!(resolved.request_id, resolved_request_id);
                saw_resolved = true;
            }
            "item/completed" => {
                let completed: ItemCompletedNotification = serde_json::from_value(
                    notification.params.clone().expect("item/completed params"),
                )?;
                if let ThreadItem::FileChange { .. } = completed.item {
                    assert!(saw_resolved, "serverRequest/resolved should arrive first");
                    completed_file_change = Some(completed.item);
                }
            }
            _ => {}
        }
    }
    let completed_file_change =
        completed_file_change.expect("file change completion should be observed");
    let ThreadItem::FileChange { ref id, status, .. } = completed_file_change else {
        unreachable!("loop ensures we break on file change items");
    };
    assert_eq!(id, "patch-call");
    assert_eq!(status, PatchApplyStatus::Completed);

    let readme_contents = std::fs::read_to_string(expected_readme_path)?;
    assert_eq!(readme_contents, "new line\n");

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    Ok(())
}

#[tokio::test]
async fn turn_start_does_not_stream_apply_patch_change_updates_without_feature_v2() -> Result<()> {
    // TODO(anp): Materialize apply-patch workspaces in the selected remote environment.
    skip_if_remote!(
        Ok(()),
        "apply-patch workspace fixture is only materialized on the host"
    );
    skip_if_no_network!(Ok(()));

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace)?;

    let call_id = "patch-call";
    let item_id = "fc-patch-call";
    let patch = "*** Begin Patch\n*** Add File: live.txt\n+live line\n*** End Patch\n";
    let patch_delta_1 = "*** Begin Patch\n*** Add File: live.txt\n+live";
    let patch_delta_2 = " line\n*** End Patch\n";
    let responses = vec![
        responses::sse(vec![
            responses::ev_response_created("resp-1"),
            serde_json::json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "custom_tool_call",
                    "id": item_id,
                    "call_id": call_id,
                    "name": "apply_patch",
                    "input": "",
                    "status": "in_progress"
                }
            }),
            serde_json::json!({
                "type": "response.custom_tool_call_input.delta",
                "item_id": item_id,
                "call_id": call_id,
                "delta": patch_delta_1,
            }),
            serde_json::json!({
                "type": "response.custom_tool_call_input.delta",
                "item_id": item_id,
                "call_id": call_id,
                "delta": patch_delta_2,
            }),
            responses::ev_apply_patch_custom_tool_call(call_id, patch),
            responses::ev_completed("resp-1"),
        ]),
        create_final_assistant_message_sse_response("patch applied")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;
    create_config_toml(&codex_home, &server.uri(), "never", &BTreeMap::default())?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(&codex_home)
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let start_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            cwd: Some(workspace.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(start_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "apply patch".into(),
                text_elements: Vec::new(),
            }],
            cwd: Some(workspace),
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    assert!(
        !mcp.pending_notification_methods()
            .iter()
            .any(|method| method == "item/fileChange/patchUpdated")
    );

    Ok(())
}

#[tokio::test]
async fn turn_start_streams_apply_patch_change_updates_v2() -> Result<()> {
    // TODO(anp): Materialize apply-patch workspaces in the selected remote environment.
    skip_if_remote!(
        Ok(()),
        "apply-patch workspace fixture is only materialized on the host"
    );
    skip_if_no_network!(Ok(()));

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace)?;

    let call_id = "patch-call";
    let item_id = "fc-patch-call";
    let patch = "*** Begin Patch\n*** Add File: live.txt\n+live line\n*** End Patch\n";
    let patch_delta_1 = "*** Begin Patch\n*** Add File: live.txt\n+live";
    let patch_delta_2 = " line\n*** End Patch\n";
    let responses = vec![
        responses::sse(vec![
            responses::ev_response_created("resp-1"),
            serde_json::json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "function_call",
                    "id": "fc-other-call",
                    "call_id": "other-call",
                    "name": "not_apply_patch",
                    "arguments": "",
                    "status": "in_progress"
                }
            }),
            serde_json::json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "fc-other-call",
                "delta": r#"{"input":"*** Begin Patch\n*** Add File: ignored.txt\n+ignored"#,
            }),
            serde_json::json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "custom_tool_call",
                    "id": item_id,
                    "call_id": call_id,
                    "name": "apply_patch",
                    "input": "",
                    "status": "in_progress"
                }
            }),
            serde_json::json!({
                "type": "response.custom_tool_call_input.delta",
                "item_id": item_id,
                "call_id": call_id,
                "delta": patch_delta_1,
            }),
            serde_json::json!({
                "type": "response.custom_tool_call_input.delta",
                "item_id": item_id,
                "call_id": call_id,
                "delta": patch_delta_2,
            }),
            responses::ev_apply_patch_custom_tool_call(call_id, patch),
            responses::ev_completed("resp-1"),
        ]),
        create_final_assistant_message_sse_response("patch applied")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;
    create_config_toml(
        &codex_home,
        &server.uri(),
        "never",
        &BTreeMap::from([
            (Feature::ApplyPatchStreamingEvents, true),
            (Feature::Plugins, false),
            (Feature::RemoteModels, false),
            (Feature::ShellSnapshot, false),
        ]),
    )?;
    write_models_cache(&codex_home)?;
    let cache_path = codex_home.join("models_cache.json");
    let mut cache: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cache_path)?)?;
    let models = cache["models"]
        .as_array_mut()
        .expect("models_cache.json models should be an array");
    let model = models
        .first_mut()
        .expect("models_cache.json should contain at least one model");
    model["slug"] = serde_json::Value::from("mock-model");
    model["display_name"] = serde_json::Value::from("mock-model");
    model["apply_patch_tool_type"] = serde_json::Value::from("freeform");
    std::fs::write(&cache_path, serde_json::to_string_pretty(&cache)?)?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(&codex_home)
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let start_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            cwd: Some(workspace.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(start_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "apply patch".into(),
                text_elements: Vec::new(),
            }],
            cwd: Some(workspace.clone()),
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let TurnStartResponse { turn } = to_response::<TurnStartResponse>(turn_resp)?;

    let mut streamed_content = String::new();
    while streamed_content != "live line\n" {
        let delta_notif = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_notification_message("item/fileChange/patchUpdated"),
        )
        .await??;
        let delta: FileChangePatchUpdatedNotification = serde_json::from_value(
            delta_notif
                .params
                .clone()
                .expect("item/fileChange/patchUpdated params"),
        )?;
        assert_eq!(delta.thread_id, thread.id);
        assert_eq!(delta.turn_id, turn.id);
        assert_eq!(delta.item_id, call_id);
        let change = delta
            .changes
            .iter()
            .find(|change| change.path == "live.txt")
            .expect("live.txt change");
        assert!(matches!(change.kind, PatchChangeKind::Add));
        streamed_content = change.diff.clone();
    }

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    Ok(())
}

#[tokio::test]
async fn turn_start_emits_spawn_agent_item_with_model_metadata_v2() -> Result<()> {
    skip_if_no_network!(Ok(()));

    const CHILD_PROMPT: &str = "child: do work";
    const PARENT_PROMPT: &str = "spawn a child and continue";
    const SPAWN_CALL_ID: &str = "spawn-call-1";
    const REQUESTED_MODEL: &str = "gpt-5.2";
    const REQUESTED_REASONING_EFFORT: ReasoningEffort = ReasoningEffort::Low;

    let server = responses::start_mock_server().await;
    let spawn_args = serde_json::to_string(&json!({
        "message": CHILD_PROMPT,
        "model": REQUESTED_MODEL,
        "reasoning_effort": REQUESTED_REASONING_EFFORT,
    }))?;
    let _parent_turn = responses::mount_sse_once_match(
        &server,
        |req: &wiremock::Request| body_contains(req, PARENT_PROMPT),
        responses::sse(vec![
            responses::ev_response_created("resp-turn1-1"),
            responses::ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                "multi_agent_v1",
                "spawn_agent",
                &spawn_args,
            ),
            responses::ev_completed("resp-turn1-1"),
        ]),
    )
    .await;
    let _child_turn = responses::mount_sse_once_match(
        &server,
        |req: &wiremock::Request| {
            body_contains(req, CHILD_PROMPT) && !body_contains(req, SPAWN_CALL_ID)
        },
        responses::sse(vec![
            responses::ev_response_created("resp-child-1"),
            responses::ev_assistant_message("msg-child-1", "child done"),
            responses::ev_completed("resp-child-1"),
        ]),
    )
    .await;
    let _parent_follow_up = responses::mount_sse_once_match(
        &server,
        |req: &wiremock::Request| body_contains(req, SPAWN_CALL_ID),
        responses::sse(vec![
            responses::ev_response_created("resp-turn1-2"),
            responses::ev_assistant_message("msg-turn1-2", "parent done"),
            responses::ev_completed("resp-turn1-2"),
        ]),
    )
    .await;

    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::from([(Feature::Collab, true)]),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("gpt-5.4".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: PARENT_PROMPT.to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let turn: TurnStartResponse = to_response::<TurnStartResponse>(turn_resp)?;

    let spawn_started = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let started_notif = mcp
                .read_stream_until_notification_message("item/started")
                .await?;
            let started: ItemStartedNotification =
                serde_json::from_value(started_notif.params.expect("item/started params"))?;
            if let ThreadItem::CollabAgentToolCall { id, .. } = &started.item
                && id == SPAWN_CALL_ID
            {
                return Ok::<ThreadItem, anyhow::Error>(started.item);
            }
        }
    })
    .await??;
    assert_eq!(
        spawn_started,
        ThreadItem::CollabAgentToolCall {
            id: SPAWN_CALL_ID.to_string(),
            tool: CollabAgentTool::SpawnAgent,
            status: CollabAgentToolCallStatus::InProgress,
            sender_thread_id: thread.id.clone(),
            receiver_thread_ids: Vec::new(),
            prompt: Some(CHILD_PROMPT.to_string()),
            model: Some(REQUESTED_MODEL.to_string()),
            reasoning_effort: Some(REQUESTED_REASONING_EFFORT),
            agents_states: HashMap::new(),
        }
    );

    let spawn_completed = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let completed_notif = mcp
                .read_stream_until_notification_message("item/completed")
                .await?;
            let completed: ItemCompletedNotification =
                serde_json::from_value(completed_notif.params.expect("item/completed params"))?;
            if let ThreadItem::CollabAgentToolCall { id, .. } = &completed.item
                && id == SPAWN_CALL_ID
            {
                return Ok::<ThreadItem, anyhow::Error>(completed.item);
            }
        }
    })
    .await??;
    let ThreadItem::CollabAgentToolCall {
        id,
        tool,
        status,
        sender_thread_id,
        receiver_thread_ids,
        prompt,
        model,
        reasoning_effort,
        agents_states,
    } = spawn_completed
    else {
        unreachable!("loop ensures we break on collab agent tool call items");
    };
    let receiver_thread_id = receiver_thread_ids
        .first()
        .cloned()
        .expect("spawn completion should include child thread id");
    assert_eq!(id, SPAWN_CALL_ID);
    assert_eq!(tool, CollabAgentTool::SpawnAgent);
    assert_eq!(status, CollabAgentToolCallStatus::Completed);
    assert_eq!(sender_thread_id, thread.id);
    assert_eq!(receiver_thread_ids, vec![receiver_thread_id.clone()]);
    assert_eq!(prompt, Some(CHILD_PROMPT.to_string()));
    assert_eq!(model, Some(REQUESTED_MODEL.to_string()));
    assert_eq!(reasoning_effort, Some(REQUESTED_REASONING_EFFORT));
    let agent_state = agents_states
        .get(&receiver_thread_id)
        .expect("spawn completion should include child agent state");
    assert!(
        matches!(
            agent_state.status,
            CollabAgentStatus::PendingInit | CollabAgentStatus::Running
        ),
        "child agent should still be initializing or already running, got {:?}",
        agent_state.status
    );
    assert_eq!(agent_state.message, None);

    // Characterize the native app-server contract that an external runtime adapter must
    // preserve. Spawn completion identifies an independently addressable child thread, which
    // then emits its own turn and item lifecycle rather than an opaque final result. The spawn
    // path does not emit a separate `thread/started` notification for the child.
    let child_turn_started = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let started_notif = mcp
                .read_stream_until_notification_message("turn/started")
                .await?;
            let started: TurnStartedNotification =
                serde_json::from_value(started_notif.params.expect("turn/started params"))?;
            if started.thread_id == receiver_thread_id {
                return Ok::<TurnStartedNotification, anyhow::Error>(started);
            }
        }
    })
    .await??;
    assert_eq!(child_turn_started.turn.status, TurnStatus::InProgress);
    let child_turn_id = child_turn_started.turn.id;

    // The parent is allowed to finish while its child continues independently.
    let turn_completed = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let turn_completed_notif = mcp
                .read_stream_until_notification_message("turn/completed")
                .await?;
            let turn_completed: TurnCompletedNotification = serde_json::from_value(
                turn_completed_notif.params.expect("turn/completed params"),
            )?;
            if turn_completed.thread_id == thread.id && turn_completed.turn.id == turn.turn.id {
                return Ok::<TurnCompletedNotification, anyhow::Error>(turn_completed);
            }
        }
    })
    .await??;
    assert_eq!(turn_completed.thread_id, thread.id);
    assert_eq!(turn_completed.turn.id, turn.turn.id);

    let child_item_started = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let started_notif = mcp
                .read_stream_until_notification_message("item/started")
                .await?;
            let started: ItemStartedNotification = serde_json::from_value(
                started_notif.params.expect("item/started params"),
            )?;
            if started.thread_id == receiver_thread_id
                && matches!(&started.item, ThreadItem::AgentMessage { id, .. } if id == "msg-child-1")
            {
                return Ok::<ItemStartedNotification, anyhow::Error>(started);
            }
        }
    })
    .await??;
    assert_eq!(child_item_started.turn_id, child_turn_id);
    assert_eq!(
        child_item_started.item,
        ThreadItem::AgentMessage {
            id: "msg-child-1".to_string(),
            text: "child done".to_string(),
            phase: None,
            memory_citation: None,
        }
    );

    let child_item_completed = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let completed_notif = mcp
                .read_stream_until_notification_message("item/completed")
                .await?;
            let completed: ItemCompletedNotification = serde_json::from_value(
                completed_notif.params.expect("item/completed params"),
            )?;
            if completed.thread_id == receiver_thread_id
                && matches!(&completed.item, ThreadItem::AgentMessage { id, .. } if id == "msg-child-1")
            {
                return Ok::<ItemCompletedNotification, anyhow::Error>(completed);
            }
        }
    })
    .await??;
    assert_eq!(child_item_completed.turn_id, child_turn_id);
    assert_eq!(
        child_item_completed.item,
        ThreadItem::AgentMessage {
            id: "msg-child-1".to_string(),
            text: "child done".to_string(),
            phase: None,
            memory_citation: None,
        }
    );

    let child_turn_completed = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let completed_notif = mcp
                .read_stream_until_notification_message("turn/completed")
                .await?;
            let completed: TurnCompletedNotification =
                serde_json::from_value(completed_notif.params.expect("turn/completed params"))?;
            if completed.thread_id == receiver_thread_id && completed.turn.id == child_turn_id {
                return Ok::<TurnCompletedNotification, anyhow::Error>(completed);
            }
        }
    })
    .await??;
    assert_eq!(child_turn_completed.turn.status, TurnStatus::Completed);

    // Reuse this live spawn setup to cover thread/delete's ThreadManager descendant path.
    let delete_req = mcp
        .send_thread_delete_request(ThreadDeleteParams {
            thread_id: thread.id.clone(),
        })
        .await?;
    let delete_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(delete_req)),
    )
    .await??;
    let _: ThreadDeleteResponse = to_response::<ThreadDeleteResponse>(delete_resp)?;

    let mut deleted_thread_ids = Vec::new();
    for _ in 0..2 {
        let deleted_notif = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_notification_message("thread/deleted"),
        )
        .await??;
        let deleted: ThreadDeletedNotification = serde_json::from_value(
            deleted_notif
                .params
                .expect("thread/deleted notification params"),
        )?;
        deleted_thread_ids.push(deleted.thread_id);
    }
    assert_eq!(
        deleted_thread_ids,
        vec![receiver_thread_id, thread.id.clone()]
    );

    let list_req = mcp
        .send_thread_loaded_list_request(ThreadLoadedListParams::default())
        .await?;
    let list_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(list_req)),
    )
    .await??;
    let ThreadLoadedListResponse { data, .. } = to_response::<ThreadLoadedListResponse>(list_resp)?;
    assert_eq!(data, Vec::<String>::new());

    Ok(())
}

#[tokio::test]
async fn fake_external_runtime_child_emits_native_app_server_events_v2() -> Result<()> {
    assert_external_runtime_child_emits_native_app_server_events_v2(
        "fake_worker",
        r#"[agents.fake_worker]
description = "Deterministic external runtime test worker"
runtime = "fake""#,
        "Hello from the fake external runtime.",
        None,
        None,
        None,
        None,
        None,
        "never",
        true,
        None,
        &[("CODEX_ENABLE_FAKE_AGENT_RUNTIME_FOR_TESTS", Some("1"))],
    )
    .await
}

#[tokio::test]
async fn fake_external_runtime_spawn_emits_role_and_model_metadata_v2() -> Result<()> {
    skip_if_no_network!(Ok(()));

    const AGENT_ROLE: &str = "fake_worker";
    const CHILD_PROMPT: &str = "child: run outside Codex";
    const PARENT_PROMPT: &str = "spawn the fake external child";
    const SPAWN_CALL_ID: &str = "spawn-fake-runtime-metadata";
    const MODEL: &str = "gpt-5.4";

    let server = responses::start_mock_server().await;
    let spawn_args = serde_json::to_string(&json!({
        "message": CHILD_PROMPT,
        "agent_type": AGENT_ROLE,
        "task_name": "external_metadata",
    }))?;
    let _parent_turn = responses::mount_sse_once_match(
        &server,
        |req: &wiremock::Request| body_contains(req, PARENT_PROMPT),
        responses::sse(vec![
            responses::ev_response_created("resp-fake-metadata-parent"),
            responses::ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                EXTERNAL_AGENT_RUNTIME_NAMESPACE,
                "spawn_agent",
                &spawn_args,
            ),
            responses::ev_completed("resp-fake-metadata-parent"),
        ]),
    )
    .await;

    let codex_home = TempDir::new()?;
    let workspace = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::from([(Feature::MultiAgentV2, true)]),
    )?;
    let config_path = codex_home.path().join("config.toml");
    let base_config = std::fs::read_to_string(&config_path)?;
    std::fs::write(
        &config_path,
        format!(
            r#"{base_config}

[agents.{AGENT_ROLE}]
description = "Deterministic external runtime test worker"
runtime = "fake"
"#,
        ),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("CODEX_ENABLE_FAKE_AGENT_RUNTIME_FOR_TESTS", Some("1"))])
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some(MODEL.to_string()),
            cwd: Some(workspace.path().to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![V2UserInput::Text {
                text: PARENT_PROMPT.to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let _: TurnStartResponse = to_response::<TurnStartResponse>(turn_resp)?;

    let spawn_completed = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let completed_notif = mcp
                .read_stream_until_notification_message("item/completed")
                .await?;
            let completed: ItemCompletedNotification =
                serde_json::from_value(completed_notif.params.expect("item/completed params"))?;
            if let ThreadItem::CollabAgentToolCall { id, .. } = &completed.item
                && id == SPAWN_CALL_ID
            {
                return Ok::<ThreadItem, anyhow::Error>(completed.item);
            }
        }
    })
    .await??;
    let ThreadItem::CollabAgentToolCall {
        tool,
        status,
        receiver_thread_ids,
        prompt,
        model,
        agents_states,
        ..
    } = spawn_completed
    else {
        unreachable!("loop only returns the external spawn item");
    };
    let child_thread_id = receiver_thread_ids
        .first()
        .cloned()
        .expect("external spawn should include the child thread id");
    assert_eq!(tool, CollabAgentTool::SpawnAgent);
    assert_eq!(status, CollabAgentToolCallStatus::Completed);
    assert_eq!(prompt.as_deref(), Some(CHILD_PROMPT));
    assert_eq!(model.as_deref(), Some(MODEL));
    assert!(agents_states.contains_key(&child_thread_id));

    let read_id = mcp
        .send_thread_read_request(ThreadReadParams {
            thread_id: child_thread_id,
            include_turns: false,
        })
        .await?;
    let read_response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(read_id)),
    )
    .await??;
    let ThreadReadResponse { thread: child } = to_response::<ThreadReadResponse>(read_response)?;
    assert_eq!(child.agent_role.as_deref(), Some(AGENT_ROLE));
    assert_eq!(child.model_provider, "mock_provider");
    assert_eq!(child.model.as_deref(), Some(MODEL));

    Ok(())
}

#[tokio::test]
async fn pi_sdk_child_emits_native_app_server_events_v2() -> Result<()> {
    let (Some(executable), Some(script)) = (
        std::env::var_os("PI_RUNTIME_TEST_EXECUTABLE"),
        std::env::var_os("PI_RUNTIME_TEST_SCRIPT"),
    ) else {
        return Ok(());
    };
    let executable = executable.to_string_lossy().into_owned();
    let arguments = serde_json::to_string(&[script.to_string_lossy().into_owned()])?;
    let faux_responses = serde_json::to_string(&json!([
        {
            "toolCall": {
                "name": "exec_command",
                "arguments": { "cmd": "printf host-bridge-ok" },
                "id": "pi-host-exec"
            }
        },
        {
            "toolCall": {
                "name": "apply_patch",
                "arguments": {
                    "patch": "*** Begin Patch\n*** Add File: pi-host-patch.txt\n+patched by pi\n*** End Patch"
                },
                "id": "pi-host-patch"
            }
        },
        "Pi host tool completed."
    ]))?;
    let environment = [
        ("CODEX_PI_RUNTIME_EXECUTABLE", Some(executable.as_str())),
        ("CODEX_PI_RUNTIME_ARGUMENTS_JSON", Some(arguments.as_str())),
        (
            "PI_CODEX_RUNTIME_FAUX_RESPONSES_JSON",
            Some(faux_responses.as_str()),
        ),
    ];
    assert_external_runtime_child_emits_native_app_server_events_v2(
        "pi_worker",
        r#"[agents.pi_worker]
description = "Pi SDK external runtime test worker"
runtime = "pi"

[agents.pi_worker.runtime_config]
provider = "faux"
model = "faux-1"
thinking_level = "high""#,
        "Pi host tool completed.",
        Some((
            "pi-host-exec",
            "host-bridge-ok",
            CommandExecutionStatus::Completed,
        )),
        Some((
            "pi-host-patch",
            "pi-host-patch.txt",
            Some("patched by pi\n"),
            PatchApplyStatus::Completed,
        )),
        None,
        None,
        None,
        "never",
        true,
        None,
        &environment,
    )
    .await
}

#[tokio::test]
async fn pi_sdk_child_routes_native_command_approval_v2() -> Result<()> {
    let (Some(executable), Some(script)) = (
        std::env::var_os("PI_RUNTIME_TEST_EXECUTABLE"),
        std::env::var_os("PI_RUNTIME_TEST_SCRIPT"),
    ) else {
        return Ok(());
    };
    let executable = executable.to_string_lossy().into_owned();
    let arguments = serde_json::to_string(&[script.to_string_lossy().into_owned()])?;
    let faux_responses = serde_json::to_string(&json!([
        {
            "toolCall": {
                "name": "exec_command",
                "arguments": {
                    "cmd": "python3 -c 'print(\"approved-host-command\")'"
                },
                "id": "pi-host-approved-exec"
            }
        },
        "Pi approved host tool completed."
    ]))?;
    let environment = [
        ("CODEX_PI_RUNTIME_EXECUTABLE", Some(executable.as_str())),
        ("CODEX_PI_RUNTIME_ARGUMENTS_JSON", Some(arguments.as_str())),
        (
            "PI_CODEX_RUNTIME_FAUX_RESPONSES_JSON",
            Some(faux_responses.as_str()),
        ),
    ];
    assert_external_runtime_child_emits_native_app_server_events_v2(
        "pi_worker",
        r#"[agents.pi_worker]
description = "Pi SDK external runtime approval test worker"
runtime = "pi"

[agents.pi_worker.runtime_config]
provider = "faux"
model = "faux-1""#,
        "Pi approved host tool completed.",
        Some((
            "pi-host-approved-exec",
            "approved-host-command",
            CommandExecutionStatus::Completed,
        )),
        None,
        None,
        Some((
            "pi-host-approved-exec",
            CommandExecutionApprovalDecision::Accept,
        )),
        None,
        "untrusted",
        true,
        None,
        &environment,
    )
    .await
}

#[tokio::test]
async fn pi_sdk_child_continues_after_declined_native_command_v2() -> Result<()> {
    let (Some(executable), Some(script)) = (
        std::env::var_os("PI_RUNTIME_TEST_EXECUTABLE"),
        std::env::var_os("PI_RUNTIME_TEST_SCRIPT"),
    ) else {
        return Ok(());
    };
    let executable = executable.to_string_lossy().into_owned();
    let arguments = serde_json::to_string(&[script.to_string_lossy().into_owned()])?;
    let faux_responses = serde_json::to_string(&json!([
        {
            "toolCall": {
                "name": "exec_command",
                "arguments": {
                    "cmd": "python3 -c 'print(\"must-not-run\")'"
                },
                "id": "pi-host-declined-exec"
            }
        },
        "Pi handled the declined host tool."
    ]))?;
    let environment = [
        ("CODEX_PI_RUNTIME_EXECUTABLE", Some(executable.as_str())),
        ("CODEX_PI_RUNTIME_ARGUMENTS_JSON", Some(arguments.as_str())),
        (
            "PI_CODEX_RUNTIME_FAUX_RESPONSES_JSON",
            Some(faux_responses.as_str()),
        ),
    ];
    assert_external_runtime_child_emits_native_app_server_events_v2(
        "pi_worker",
        r#"[agents.pi_worker]
description = "Pi SDK external runtime declined approval test worker"
runtime = "pi"

[agents.pi_worker.runtime_config]
provider = "faux"
model = "faux-1""#,
        "Pi handled the declined host tool.",
        Some((
            "pi-host-declined-exec",
            "",
            CommandExecutionStatus::Declined,
        )),
        None,
        None,
        Some((
            "pi-host-declined-exec",
            CommandExecutionApprovalDecision::Cancel,
        )),
        None,
        "untrusted",
        true,
        None,
        &environment,
    )
    .await
}

#[tokio::test]
async fn pi_sdk_child_inherits_read_only_sandbox_v2() -> Result<()> {
    let (Some(executable), Some(script)) = (
        std::env::var_os("PI_RUNTIME_TEST_EXECUTABLE"),
        std::env::var_os("PI_RUNTIME_TEST_SCRIPT"),
    ) else {
        return Ok(());
    };
    let executable = executable.to_string_lossy().into_owned();
    let arguments = serde_json::to_string(&[script.to_string_lossy().into_owned()])?;
    let faux_responses = serde_json::to_string(&json!([
        {
            "toolCall": {
                "name": "exec_command",
                "arguments": {
                    "cmd": "printf blocked > pi-must-not-write.txt"
                },
                "id": "pi-host-read-only-exec"
            }
        },
        "Pi handled the sandbox denial."
    ]))?;
    let environment = [
        ("CODEX_PI_RUNTIME_EXECUTABLE", Some(executable.as_str())),
        ("CODEX_PI_RUNTIME_ARGUMENTS_JSON", Some(arguments.as_str())),
        (
            "PI_CODEX_RUNTIME_FAUX_RESPONSES_JSON",
            Some(faux_responses.as_str()),
        ),
    ];
    assert_external_runtime_child_emits_native_app_server_events_v2(
        "pi_worker",
        r#"[agents.pi_worker]
description = "Pi SDK external runtime read-only sandbox test worker"
runtime = "pi"

[agents.pi_worker.runtime_config]
provider = "faux"
        model = "faux-1""#,
        "Pi handled the sandbox denial.",
        None,
        None,
        Some("pi-must-not-write.txt"),
        None,
        None,
        "never",
        false,
        None,
        &environment,
    )
    .await
}

#[tokio::test]
async fn pi_sdk_child_inherits_network_denial_v2() -> Result<()> {
    let (Some(executable), Some(script)) = (
        std::env::var_os("PI_RUNTIME_TEST_EXECUTABLE"),
        std::env::var_os("PI_RUNTIME_TEST_SCRIPT"),
    ) else {
        return Ok(());
    };
    let executable = executable.to_string_lossy().into_owned();
    let arguments = serde_json::to_string(&[script.to_string_lossy().into_owned()])?;
    let faux_responses = serde_json::to_string(&json!([
        {
            "toolCall": {
                "name": "exec_command",
                "arguments": {
                    "cmd": "python3 -c 'import socket,time; time.sleep(0.25); socket.create_connection((\"1.1.1.1\", 80), 1)'"
                },
                "id": "pi-host-network-denied"
            }
        },
        "Pi handled the network denial."
    ]))?;
    let environment = [
        ("CODEX_PI_RUNTIME_EXECUTABLE", Some(executable.as_str())),
        ("CODEX_PI_RUNTIME_ARGUMENTS_JSON", Some(arguments.as_str())),
        (
            "PI_CODEX_RUNTIME_FAUX_RESPONSES_JSON",
            Some(faux_responses.as_str()),
        ),
    ];
    assert_external_runtime_child_emits_native_app_server_events_v2(
        "pi_worker",
        r#"[agents.pi_worker]
description = "Pi SDK external runtime network sandbox test worker"
runtime = "pi"

[agents.pi_worker.runtime_config]
provider = "faux"
model = "faux-1""#,
        "Pi handled the network denial.",
        Some(("pi-host-network-denied", "", CommandExecutionStatus::Failed)),
        None,
        None,
        None,
        None,
        "never",
        true,
        None,
        &environment,
    )
    .await
}

#[tokio::test]
async fn pi_sdk_child_interrupt_cancels_native_host_command_v2() -> Result<()> {
    let (Some(executable), Some(script)) = (
        std::env::var_os("PI_RUNTIME_TEST_EXECUTABLE"),
        std::env::var_os("PI_RUNTIME_TEST_SCRIPT"),
    ) else {
        return Ok(());
    };
    let executable = executable.to_string_lossy().into_owned();
    let arguments = serde_json::to_string(&[script.to_string_lossy().into_owned()])?;
    let faux_responses = serde_json::to_string(&json!([
        {
            "toolCall": {
                "name": "exec_command",
                "arguments": {
                    "cmd": "sleep 60; printf must-not-exist > pi-interrupt-marker.txt"
                },
                "id": "pi-host-interrupted-exec"
            }
        },
        "Pi must not reach this final response."
    ]))?;
    let environment = [
        ("CODEX_PI_RUNTIME_EXECUTABLE", Some(executable.as_str())),
        ("CODEX_PI_RUNTIME_ARGUMENTS_JSON", Some(arguments.as_str())),
        (
            "PI_CODEX_RUNTIME_FAUX_RESPONSES_JSON",
            Some(faux_responses.as_str()),
        ),
    ];
    assert_external_runtime_child_emits_native_app_server_events_v2(
        "pi_worker",
        r#"[agents.pi_worker]
description = "Pi SDK external runtime interruption test worker"
runtime = "pi"

[agents.pi_worker.runtime_config]
provider = "faux"
model = "faux-1""#,
        "",
        Some((
            "pi-host-interrupted-exec",
            "",
            CommandExecutionStatus::Failed,
        )),
        None,
        Some("pi-interrupt-marker.txt"),
        None,
        None,
        "never",
        true,
        Some("pi-host-interrupted-exec"),
        &environment,
    )
    .await
}

#[tokio::test]
async fn pi_sdk_child_routes_native_patch_approval_v2() -> Result<()> {
    let (Some(executable), Some(script)) = (
        std::env::var_os("PI_RUNTIME_TEST_EXECUTABLE"),
        std::env::var_os("PI_RUNTIME_TEST_SCRIPT"),
    ) else {
        return Ok(());
    };
    let executable = executable.to_string_lossy().into_owned();
    let arguments = serde_json::to_string(&[script.to_string_lossy().into_owned()])?;
    let faux_responses = serde_json::to_string(&json!([
        {
            "toolCall": {
                "name": "apply_patch",
                "arguments": {
                    "patch": "*** Begin Patch\n*** Add File: pi-approved-patch.txt\n+approved patch\n*** End Patch"
                },
                "id": "pi-host-approved-patch"
            }
        },
        "Pi approved patch completed."
    ]))?;
    let environment = [
        ("CODEX_PI_RUNTIME_EXECUTABLE", Some(executable.as_str())),
        ("CODEX_PI_RUNTIME_ARGUMENTS_JSON", Some(arguments.as_str())),
        (
            "PI_CODEX_RUNTIME_FAUX_RESPONSES_JSON",
            Some(faux_responses.as_str()),
        ),
    ];
    assert_external_runtime_child_emits_native_app_server_events_v2(
        "pi_worker",
        r#"[agents.pi_worker]
description = "Pi SDK external runtime patch approval test worker"
runtime = "pi"

[agents.pi_worker.runtime_config]
provider = "faux"
model = "faux-1""#,
        "Pi approved patch completed.",
        None,
        Some((
            "pi-host-approved-patch",
            "pi-approved-patch.txt",
            Some("approved patch\n"),
            PatchApplyStatus::Completed,
        )),
        None,
        None,
        Some(("pi-host-approved-patch", FileChangeApprovalDecision::Accept)),
        "untrusted",
        true,
        None,
        &environment,
    )
    .await
}

#[tokio::test]
async fn pi_sdk_child_continues_after_declined_native_patch_v2() -> Result<()> {
    let (Some(executable), Some(script)) = (
        std::env::var_os("PI_RUNTIME_TEST_EXECUTABLE"),
        std::env::var_os("PI_RUNTIME_TEST_SCRIPT"),
    ) else {
        return Ok(());
    };
    let executable = executable.to_string_lossy().into_owned();
    let arguments = serde_json::to_string(&[script.to_string_lossy().into_owned()])?;
    let faux_responses = serde_json::to_string(&json!([
        {
            "toolCall": {
                "name": "apply_patch",
                "arguments": {
                    "patch": "*** Begin Patch\n*** Add File: pi-declined-patch.txt\n+must not exist\n*** End Patch"
                },
                "id": "pi-host-declined-patch"
            }
        },
        "Pi handled the declined patch."
    ]))?;
    let environment = [
        ("CODEX_PI_RUNTIME_EXECUTABLE", Some(executable.as_str())),
        ("CODEX_PI_RUNTIME_ARGUMENTS_JSON", Some(arguments.as_str())),
        (
            "PI_CODEX_RUNTIME_FAUX_RESPONSES_JSON",
            Some(faux_responses.as_str()),
        ),
    ];
    assert_external_runtime_child_emits_native_app_server_events_v2(
        "pi_worker",
        r#"[agents.pi_worker]
description = "Pi SDK external runtime declined patch test worker"
runtime = "pi"

[agents.pi_worker.runtime_config]
provider = "faux"
model = "faux-1""#,
        "Pi handled the declined patch.",
        None,
        Some((
            "pi-host-declined-patch",
            "pi-declined-patch.txt",
            None,
            PatchApplyStatus::Declined,
        )),
        Some("pi-declined-patch.txt"),
        None,
        Some((
            "pi-host-declined-patch",
            FileChangeApprovalDecision::Decline,
        )),
        "untrusted",
        true,
        None,
        &environment,
    )
    .await
}

async fn assert_external_runtime_child_emits_native_app_server_events_v2(
    agent_type: &str,
    agent_config: &str,
    expected_message: &str,
    expected_command: Option<(&str, &str, CommandExecutionStatus)>,
    expected_patch: Option<(&str, &str, Option<&str>, PatchApplyStatus)>,
    expected_missing_file: Option<&str>,
    expected_command_approval: Option<(&str, CommandExecutionApprovalDecision)>,
    expected_patch_approval: Option<(&str, FileChangeApprovalDecision)>,
    approval_policy: &str,
    workspace_write: bool,
    interrupt_after_command_start: Option<&str>,
    environment: &[(&str, Option<&str>)],
) -> Result<()> {
    skip_if_no_network!(Ok(()));

    const CHILD_PROMPT: &str = "child: run outside Codex";
    const PARENT_PROMPT: &str = "spawn the fake external child";
    const SPAWN_CALL_ID: &str = "spawn-fake-runtime";

    let server = responses::start_mock_server().await;
    let spawn_args = serde_json::to_string(&json!({
        "message": CHILD_PROMPT,
        "agent_type": agent_type,
    }))?;
    let _parent_turn = responses::mount_sse_once_match(
        &server,
        |req: &wiremock::Request| body_contains(req, PARENT_PROMPT),
        responses::sse(vec![
            responses::ev_response_created("resp-fake-parent-1"),
            responses::ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                "multi_agent_v1",
                "spawn_agent",
                &spawn_args,
            ),
            responses::ev_completed("resp-fake-parent-1"),
        ]),
    )
    .await;
    let _parent_follow_up = responses::mount_sse_once_match(
        &server,
        |req: &wiremock::Request| body_contains(req, SPAWN_CALL_ID),
        responses::sse(vec![
            responses::ev_response_created("resp-fake-parent-2"),
            responses::ev_assistant_message("msg-fake-parent", "parent done"),
            responses::ev_completed("resp-fake-parent-2"),
        ]),
    )
    .await;

    let codex_home = TempDir::new()?;
    let workspace = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        approval_policy,
        &BTreeMap::from([(Feature::Collab, true)]),
    )?;
    let config_path = codex_home.path().join("config.toml");
    let base_config = std::fs::read_to_string(&config_path)?;
    std::fs::write(&config_path, format!("{base_config}\n\n{agent_config}\n"))?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(environment)
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("gpt-5.4".to_string()),
            cwd: Some(workspace.path().to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![V2UserInput::Text {
                text: PARENT_PROMPT.to_string(),
                text_elements: Vec::new(),
            }],
            cwd: Some(workspace.path().to_path_buf()),
            sandbox_policy: Some(if workspace_write {
                codex_app_server_protocol::SandboxPolicy::WorkspaceWrite {
                    writable_roots: vec![workspace.path().try_into()?],
                    network_access: false,
                    exclude_tmpdir_env_var: true,
                    exclude_slash_tmp: true,
                }
            } else {
                codex_app_server_protocol::SandboxPolicy::ReadOnly {
                    network_access: false,
                }
            }),
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let _: TurnStartResponse = to_response::<TurnStartResponse>(turn_resp)?;

    let (spawn_completed, early_item_completions) = timeout(DEFAULT_READ_TIMEOUT, async {
        let mut early_item_completions = Vec::new();
        loop {
            let notification = mcp
                .read_stream_until_notification_message("item/completed")
                .await?;
            let completed: ItemCompletedNotification =
                serde_json::from_value(notification.params.expect("item/completed params"))?;
            if matches!(&completed.item, ThreadItem::CollabAgentToolCall { id, .. } if id == SPAWN_CALL_ID)
            {
                return Ok::<(ThreadItem, Vec<ItemCompletedNotification>), anyhow::Error>((
                    completed.item,
                    early_item_completions,
                ));
            }
            early_item_completions.push(completed);
        }
    })
    .await??;
    let ThreadItem::CollabAgentToolCall {
        receiver_thread_ids,
        status,
        ..
    } = spawn_completed
    else {
        unreachable!("loop only returns the spawn tool item");
    };
    assert_eq!(status, CollabAgentToolCallStatus::Completed);
    let child_thread_id = receiver_thread_ids
        .first()
        .cloned()
        .expect("fake runtime spawn should expose its child thread id");

    let child_turn_started = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let notification = mcp
                .read_stream_until_notification_message("turn/started")
                .await?;
            let started: TurnStartedNotification =
                serde_json::from_value(notification.params.expect("turn/started params"))?;
            if started.thread_id == child_thread_id {
                return Ok::<TurnStartedNotification, anyhow::Error>(started);
            }
        }
    })
    .await??;
    assert_eq!(child_turn_started.turn.status, TurnStatus::InProgress);
    let child_turn_id = child_turn_started.turn.id;

    let (child_item_started, mut child_command_started, mut child_patch_started) =
        timeout(DEFAULT_READ_TIMEOUT, async {
            let mut child_command_started = None;
            let mut child_patch_started = None;
            let mut pending_command_approval = expected_command_approval;
            let mut pending_patch_approval = expected_patch_approval;
            loop {
                let notification = mcp
                    .read_stream_until_notification_message("item/started")
                    .await?;
                let started: ItemStartedNotification =
                    serde_json::from_value(notification.params.expect("item/started params"))?;
                if started.thread_id == child_thread_id
                    && matches!(&started.item, ThreadItem::AgentMessage { .. })
                {
                    return Ok::<_, anyhow::Error>((
                        Some(started),
                        child_command_started,
                        child_patch_started,
                    ));
                }
                if started.thread_id == child_thread_id
                    && matches!(&started.item, ThreadItem::CommandExecution { .. })
                {
                    if let Some((expected_id, decision)) = pending_command_approval.take() {
                        let server_req = mcp.read_stream_until_request_message().await?;
                        let ServerRequest::CommandExecutionRequestApproval { request_id, params } =
                            server_req
                        else {
                            panic!("expected CommandExecutionRequestApproval request")
                        };
                        let ThreadItem::CommandExecution { ref id, .. } = started.item else {
                            unreachable!("branch only accepts command execution items")
                        };
                        assert_eq!(params.thread_id, child_thread_id);
                        assert_eq!(params.turn_id, child_turn_id);
                        assert_eq!(params.item_id, *id);
                        assert_eq!(params.item_id, expected_id);
                        mcp.send_response(
                            request_id,
                            serde_json::to_value(CommandExecutionRequestApprovalResponse {
                                decision,
                            })?,
                        )
                        .await?;
                    }
                    child_command_started = Some(started);
                    if matches!(
                        (&child_command_started, interrupt_after_command_start),
                        (
                            Some(ItemStartedNotification {
                                item: ThreadItem::CommandExecution { id, .. },
                                ..
                            }),
                            Some(expected_id)
                        ) if id == expected_id
                    ) {
                        return Ok::<_, anyhow::Error>((
                            None,
                            child_command_started,
                            child_patch_started,
                        ));
                    }
                } else if started.thread_id == child_thread_id
                    && matches!(&started.item, ThreadItem::FileChange { .. })
                {
                    if let Some((expected_id, decision)) = pending_patch_approval.take() {
                        let server_req = mcp.read_stream_until_request_message().await?;
                        let ServerRequest::FileChangeRequestApproval { request_id, params } =
                            server_req
                        else {
                            panic!("expected FileChangeRequestApproval request")
                        };
                        let ThreadItem::FileChange { ref id, .. } = started.item else {
                            unreachable!("branch only accepts file change items")
                        };
                        assert_eq!(params.thread_id, child_thread_id);
                        assert_eq!(params.turn_id, child_turn_id);
                        assert_eq!(params.item_id, *id);
                        assert_eq!(params.item_id, expected_id);
                        mcp.send_response(
                            request_id,
                            serde_json::to_value(FileChangeRequestApprovalResponse { decision })?,
                        )
                        .await?;
                    }
                    child_patch_started = Some(started);
                }
            }
        })
        .await??;
    if child_item_started.is_none() {
        let interrupt_id = mcp
            .send_turn_interrupt_request(TurnInterruptParams {
                thread_id: child_thread_id.clone(),
                turn_id: child_turn_id.clone(),
            })
            .await?;
        let interrupt_response: JSONRPCResponse = timeout(
            std::time::Duration::from_secs(25),
            mcp.read_stream_until_response_message(RequestId::Integer(interrupt_id)),
        )
        .await??;
        let _: TurnInterruptResponse = to_response::<TurnInterruptResponse>(interrupt_response)?;
        let completed = timeout(DEFAULT_READ_TIMEOUT, async {
            loop {
                let notification = mcp
                    .read_stream_until_notification_message("turn/completed")
                    .await?;
                let completed: TurnCompletedNotification =
                    serde_json::from_value(notification.params.expect("turn/completed params"))?;
                if completed.thread_id == child_thread_id && completed.turn.id == child_turn_id {
                    return Ok::<TurnCompletedNotification, anyhow::Error>(completed);
                }
            }
        })
        .await??;
        assert_eq!(completed.turn.status, TurnStatus::Interrupted);
        if let Some(expected_missing_file) = expected_missing_file {
            assert!(!workspace.path().join(expected_missing_file).exists());
        }
        return Ok(());
    }
    let child_item_started = child_item_started.expect("checked above");
    assert_eq!(child_item_started.turn_id, child_turn_id);
    let ThreadItem::AgentMessage { id: item_id, .. } = child_item_started.item else {
        unreachable!("loop only returns an agent message item");
    };
    if expected_command.is_some() && child_command_started.is_none() {
        child_command_started = Some(
            timeout(DEFAULT_READ_TIMEOUT, async {
                loop {
                    let notification = mcp
                        .read_stream_until_notification_message("item/started")
                        .await?;
                    let started: ItemStartedNotification =
                        serde_json::from_value(notification.params.expect("item/started params"))?;
                    if started.thread_id == child_thread_id
                        && matches!(&started.item, ThreadItem::CommandExecution { .. })
                    {
                        return Ok::<ItemStartedNotification, anyhow::Error>(started);
                    }
                }
            })
            .await??,
        );
    }
    match (expected_command.clone(), child_command_started) {
        (Some((expected_id, _, _)), Some(started)) => {
            assert_eq!(started.turn_id, child_turn_id);
            assert!(
                matches!(started.item, ThreadItem::CommandExecution { id, status: CommandExecutionStatus::InProgress, .. } if id == expected_id)
            );
        }
        (Some(_), None) => panic!("expected a Codex-hosted command item"),
        (None, _) => {}
    }
    if expected_patch.is_some() && child_patch_started.is_none() {
        child_patch_started = Some(
            timeout(DEFAULT_READ_TIMEOUT, async {
                loop {
                    let notification = mcp
                        .read_stream_until_notification_message("item/started")
                        .await?;
                    let started: ItemStartedNotification =
                        serde_json::from_value(notification.params.expect("item/started params"))?;
                    if started.thread_id == child_thread_id
                        && matches!(&started.item, ThreadItem::FileChange { .. })
                    {
                        return Ok::<ItemStartedNotification, anyhow::Error>(started);
                    }
                }
            })
            .await??,
        );
    }
    match (expected_patch.clone(), child_patch_started) {
        (Some((expected_id, expected_path, _, _)), Some(started)) => {
            assert_eq!(started.turn_id, child_turn_id);
            assert!(matches!(
                started.item,
                ThreadItem::FileChange { id, status: PatchApplyStatus::InProgress, changes }
                    if id == expected_id
                        && changes.iter().any(|change| change.path.ends_with(expected_path))
            ));
        }
        (Some(_), None) => panic!("expected a Codex-hosted file change item"),
        (None, _) => {}
    }

    let delta_notification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("item/agentMessage/delta"),
    )
    .await??;
    let delta: AgentMessageDeltaNotification = serde_json::from_value(
        delta_notification
            .params
            .expect("item/agentMessage/delta params"),
    )?;
    assert_eq!(delta.thread_id, child_thread_id);
    assert_eq!(delta.turn_id, child_turn_id);
    assert_eq!(delta.item_id, item_id);
    assert!(!delta.delta.is_empty());

    let (child_item_completed, mut child_command_completed, mut child_patch_completed) = match early_item_completions.into_iter().find(|completed| {
        completed.thread_id == child_thread_id
            && matches!(&completed.item, ThreadItem::AgentMessage { id, .. } if id == &item_id)
    }) {
        Some(completed) => (completed, None, None),
        None => timeout(DEFAULT_READ_TIMEOUT, async {
            let mut child_command_completed = None;
            let mut child_patch_completed = None;
            loop {
                let notification = mcp
                    .read_stream_until_notification_message("item/completed")
                    .await?;
                let completed: ItemCompletedNotification =
                    serde_json::from_value(notification.params.expect("item/completed params"))?;
                if completed.thread_id == child_thread_id
                    && matches!(&completed.item, ThreadItem::AgentMessage { id, .. } if id == &item_id)
                {
                    return Ok::<_, anyhow::Error>((
                        completed,
                        child_command_completed,
                        child_patch_completed,
                    ));
                }
                if completed.thread_id == child_thread_id
                    && matches!(&completed.item, ThreadItem::CommandExecution { .. })
                {
                    child_command_completed = Some(completed);
                }
                else if completed.thread_id == child_thread_id
                    && matches!(&completed.item, ThreadItem::FileChange { .. })
                {
                    child_patch_completed = Some(completed);
                }
            }
        })
        .await??,
    };
    assert_eq!(child_item_completed.turn_id, child_turn_id);
    assert!(matches!(
        child_item_completed.item,
        ThreadItem::AgentMessage { text, .. } if text == expected_message
    ));
    if expected_command.is_some() && child_command_completed.is_none() {
        child_command_completed = Some(
            timeout(DEFAULT_READ_TIMEOUT, async {
                loop {
                    let notification = mcp
                        .read_stream_until_notification_message("item/completed")
                        .await?;
                    let completed: ItemCompletedNotification = serde_json::from_value(
                        notification.params.expect("item/completed params"),
                    )?;
                    if completed.thread_id == child_thread_id
                        && matches!(&completed.item, ThreadItem::CommandExecution { .. })
                    {
                        return Ok::<ItemCompletedNotification, anyhow::Error>(completed);
                    }
                }
            })
            .await??,
        );
    }
    match (expected_command, child_command_completed) {
        (Some((expected_id, expected_output, expected_status)), Some(completed)) => {
            assert_eq!(completed.turn_id, child_turn_id);
            let ThreadItem::CommandExecution {
                id,
                status,
                aggregated_output,
                ..
            } = completed.item
            else {
                unreachable!("branch only accepts command execution items")
            };
            assert_eq!(id, expected_id);
            assert_eq!(status, expected_status);
            if !expected_output.is_empty() {
                assert!(
                    aggregated_output
                        .as_deref()
                        .is_some_and(|output| output.contains(expected_output))
                );
            }
        }
        (Some(_), None) => panic!("expected a completed Codex-hosted command item"),
        (None, _) => {}
    }
    if expected_patch.is_some() && child_patch_completed.is_none() {
        child_patch_completed = Some(
            timeout(DEFAULT_READ_TIMEOUT, async {
                loop {
                    let notification = mcp
                        .read_stream_until_notification_message("item/completed")
                        .await?;
                    let completed: ItemCompletedNotification = serde_json::from_value(
                        notification.params.expect("item/completed params"),
                    )?;
                    if completed.thread_id == child_thread_id
                        && matches!(&completed.item, ThreadItem::FileChange { .. })
                    {
                        return Ok::<ItemCompletedNotification, anyhow::Error>(completed);
                    }
                }
            })
            .await??,
        );
    }
    match (expected_patch, child_patch_completed) {
        (
            Some((expected_id, expected_path, expected_contents, expected_status)),
            Some(completed),
        ) => {
            assert_eq!(completed.turn_id, child_turn_id);
            assert!(matches!(
                completed.item,
                ThreadItem::FileChange { id, status, changes }
                    if id == expected_id
                        && status == expected_status
                        && changes.iter().any(|change| change.path.ends_with(expected_path))
            ));
            if let Some(expected_contents) = expected_contents {
                assert_eq!(
                    std::fs::read_to_string(workspace.path().join(expected_path))?,
                    expected_contents
                );
            }
        }
        (Some(_), None) => panic!("expected a completed Codex-hosted file change item"),
        (None, _) => {}
    }

    let child_turn_completed = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let notification = mcp
                .read_stream_until_notification_message("turn/completed")
                .await?;
            let completed: TurnCompletedNotification =
                serde_json::from_value(notification.params.expect("turn/completed params"))?;
            if completed.thread_id == child_thread_id && completed.turn.id == child_turn_id {
                return Ok::<TurnCompletedNotification, anyhow::Error>(completed);
            }
        }
    })
    .await??;
    assert_eq!(child_turn_completed.turn.status, TurnStatus::Completed);

    let read_id = mcp
        .send_thread_read_request(ThreadReadParams {
            thread_id: child_thread_id.clone(),
            include_turns: true,
        })
        .await?;
    let read_response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(read_id)),
    )
    .await??;
    let ThreadReadResponse {
        thread: child_thread,
    } = to_response::<ThreadReadResponse>(read_response)?;
    assert_eq!(child_thread.id, child_thread_id);
    assert_eq!(
        child_thread.parent_thread_id.as_deref(),
        Some(thread.id.as_str())
    );
    assert_eq!(child_thread.agent_role.as_deref(), Some(agent_type));
    if agent_type == "pi_worker" {
        assert_eq!(child_thread.model_provider, "faux");
        assert_eq!(child_thread.model.as_deref(), Some("faux-1"));
    }
    assert!(
        child_thread.turns.iter().any(|turn| {
            turn.items.iter().any(|item| {
                matches!(item, ThreadItem::AgentMessage { text, .. } if text == expected_message)
            })
        }),
        "thread/read should reconstruct the external agent message from durable history"
    );
    if let Some(expected_missing_file) = expected_missing_file {
        assert!(!workspace.path().join(expected_missing_file).exists());
    }

    Ok(())
}

#[tokio::test]
async fn direct_input_to_multi_agent_v2_subagent_is_rejected() -> Result<()> {
    const CHILD_PROMPT: &str = "child: do work";
    const PARENT_PROMPT: &str = "spawn a child and continue";
    const SPAWN_CALL_ID: &str = "spawn-call-direct-input-rejection";
    const ERROR_MESSAGE: &str =
        "direct app-server input is not allowed for multi-agent v2 sub-agents";

    let server = responses::start_mock_server().await;
    let spawn_args = serde_json::to_string(&json!({
        "message": CHILD_PROMPT,
        "task_name": "worker",
    }))?;
    let _parent_turn = responses::mount_sse_once_match(
        &server,
        |req: &wiremock::Request| body_contains(req, PARENT_PROMPT),
        responses::sse(vec![
            responses::ev_response_created("resp-parent-1"),
            responses::ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                MULTI_AGENT_V2_NAMESPACE,
                "spawn_agent",
                &spawn_args,
            ),
            responses::ev_completed("resp-parent-1"),
        ]),
    )
    .await;
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::from([(Feature::MultiAgentV2, true)]),
    )?;
    write_models_cache(codex_home.path())?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("gpt-5.4".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            input: vec![V2UserInput::Text {
                text: PARENT_PROMPT.to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let _: TurnStartResponse = to_response(turn_resp)?;

    let child_thread_id = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let completed_notif = mcp
                .read_stream_until_notification_message("item/completed")
                .await?;
            let completed: ItemCompletedNotification =
                serde_json::from_value(completed_notif.params.expect("item/completed params"))?;
            if let ThreadItem::SubAgentActivity {
                id,
                kind: SubAgentActivityKind::Started,
                agent_thread_id,
                ..
            } = completed.item
                && id == SPAWN_CALL_ID
            {
                return Ok::<String, anyhow::Error>(agent_thread_id);
            }
        }
    })
    .await??;

    let direct_turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: child_thread_id.clone(),
            input: vec![V2UserInput::Text {
                text: "direct app-server turn".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let direct_turn_error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(direct_turn_req)),
    )
    .await??;
    assert_eq!(direct_turn_error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(direct_turn_error.error.message, ERROR_MESSAGE);

    let direct_steer_req = mcp
        .send_turn_steer_request(TurnSteerParams {
            thread_id: child_thread_id,
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "direct app-server steer".to_string(),
                text_elements: Vec::new(),
            }],
            responsesapi_client_metadata: None,
            additional_context: None,
            expected_turn_id: "any-active-turn".to_string(),
        })
        .await?;
    let direct_steer_error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(direct_steer_req)),
    )
    .await??;
    assert_eq!(direct_steer_error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(direct_steer_error.error.message, ERROR_MESSAGE);

    Ok(())
}

#[tokio::test]
async fn turn_start_emits_spawn_agent_item_with_effective_role_model_metadata_v2() -> Result<()> {
    skip_if_no_network!(Ok(()));

    const CHILD_PROMPT: &str = "child: do work";
    const PARENT_PROMPT: &str = "spawn a child and continue";
    const SPAWN_CALL_ID: &str = "spawn-call-1";
    const REQUESTED_MODEL: &str = "gpt-5.2";
    const REQUESTED_REASONING_EFFORT: ReasoningEffort = ReasoningEffort::Low;
    const ROLE_MODEL: &str = "gpt-5.4";
    const ROLE_REASONING_EFFORT: ReasoningEffort = ReasoningEffort::High;

    let server = responses::start_mock_server().await;
    let spawn_args = serde_json::to_string(&json!({
        "message": CHILD_PROMPT,
        "agent_type": "custom",
        "model": REQUESTED_MODEL,
        "reasoning_effort": REQUESTED_REASONING_EFFORT,
    }))?;
    let _parent_turn = responses::mount_sse_once_match(
        &server,
        |req: &wiremock::Request| body_contains(req, PARENT_PROMPT),
        responses::sse(vec![
            responses::ev_response_created("resp-turn1-1"),
            responses::ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                "multi_agent_v1",
                "spawn_agent",
                &spawn_args,
            ),
            responses::ev_completed("resp-turn1-1"),
        ]),
    )
    .await;
    let _child_turn = responses::mount_sse_once_match(
        &server,
        |req: &wiremock::Request| {
            body_contains(req, CHILD_PROMPT) && !body_contains(req, SPAWN_CALL_ID)
        },
        responses::sse(vec![
            responses::ev_response_created("resp-child-1"),
            responses::ev_assistant_message("msg-child-1", "child done"),
            responses::ev_completed("resp-child-1"),
        ]),
    )
    .await;
    let _parent_follow_up = responses::mount_sse_once_match(
        &server,
        |req: &wiremock::Request| body_contains(req, SPAWN_CALL_ID),
        responses::sse(vec![
            responses::ev_response_created("resp-turn1-2"),
            responses::ev_assistant_message("msg-turn1-2", "parent done"),
            responses::ev_completed("resp-turn1-2"),
        ]),
    )
    .await;

    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::from([(Feature::Collab, true)]),
    )?;
    std::fs::write(
        codex_home.path().join("custom-role.toml"),
        format!("model = \"{ROLE_MODEL}\"\nmodel_reasoning_effort = \"{ROLE_REASONING_EFFORT}\"\n",),
    )?;
    let config_path = codex_home.path().join("config.toml");
    let base_config = std::fs::read_to_string(&config_path)?;
    std::fs::write(
        &config_path,
        format!(
            r#"{base_config}

[agents.custom]
description = "Custom role"
config_file = "./custom-role.toml"
"#
        ),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("gpt-5.4".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: PARENT_PROMPT.to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let turn: TurnStartResponse = to_response::<TurnStartResponse>(turn_resp)?;

    let spawn_completed = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let completed_notif = mcp
                .read_stream_until_notification_message("item/completed")
                .await?;
            let completed: ItemCompletedNotification =
                serde_json::from_value(completed_notif.params.expect("item/completed params"))?;
            if let ThreadItem::CollabAgentToolCall { id, .. } = &completed.item
                && id == SPAWN_CALL_ID
            {
                return Ok::<ThreadItem, anyhow::Error>(completed.item);
            }
        }
    })
    .await??;
    let ThreadItem::CollabAgentToolCall {
        id,
        tool,
        status,
        sender_thread_id,
        receiver_thread_ids,
        prompt,
        model,
        reasoning_effort,
        agents_states,
    } = spawn_completed
    else {
        unreachable!("loop ensures we break on collab agent tool call items");
    };
    let receiver_thread_id = receiver_thread_ids
        .first()
        .cloned()
        .expect("spawn completion should include child thread id");
    assert_eq!(id, SPAWN_CALL_ID);
    assert_eq!(tool, CollabAgentTool::SpawnAgent);
    assert_eq!(status, CollabAgentToolCallStatus::Completed);
    assert_eq!(sender_thread_id, thread.id);
    assert_eq!(receiver_thread_ids, vec![receiver_thread_id.clone()]);
    assert_eq!(prompt, Some(CHILD_PROMPT.to_string()));
    assert_eq!(model, Some(ROLE_MODEL.to_string()));
    assert_eq!(reasoning_effort, Some(ROLE_REASONING_EFFORT));
    let agent_state = agents_states
        .get(&receiver_thread_id)
        .expect("spawn completion should include child agent state");
    assert!(
        matches!(
            agent_state.status,
            CollabAgentStatus::PendingInit | CollabAgentStatus::Running
        ),
        "child agent should still be initializing or already running, got {:?}",
        agent_state.status
    );
    assert_eq!(agent_state.message, None);

    let turn_completed = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let turn_completed_notif = mcp
                .read_stream_until_notification_message("turn/completed")
                .await?;
            let turn_completed: TurnCompletedNotification = serde_json::from_value(
                turn_completed_notif.params.expect("turn/completed params"),
            )?;
            if turn_completed.thread_id == thread.id && turn_completed.turn.id == turn.turn.id {
                return Ok::<TurnCompletedNotification, anyhow::Error>(turn_completed);
            }
        }
    })
    .await??;
    assert_eq!(turn_completed.thread_id, thread.id);

    Ok(())
}

#[tokio::test]
async fn turn_start_file_change_approval_accept_for_session_persists_v2() -> Result<()> {
    // TODO(anp): Materialize apply-patch workspaces in the selected remote environment.
    skip_if_remote!(
        Ok(()),
        "apply-patch workspace fixture is only materialized on the host"
    );
    skip_if_no_network!(Ok(()));

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace)?;

    let patch_1 = r#"*** Begin Patch
*** Add File: README.md
+new line
*** End Patch
"#;
    let patch_2 = r#"*** Begin Patch
*** Update File: README.md
@@
-new line
+updated line
*** End Patch
"#;

    let responses = vec![
        create_apply_patch_sse_response(patch_1, "patch-call-1")?,
        create_final_assistant_message_sse_response("patch 1 applied")?,
        create_apply_patch_sse_response(patch_2, "patch-call-2")?,
        create_final_assistant_message_sse_response("patch 2 applied")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;
    create_config_toml(
        &codex_home,
        &server.uri(),
        "untrusted",
        &BTreeMap::default(),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(&codex_home)
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let start_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            cwd: Some(workspace.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(start_resp)?;

    // First turn: expect FileChangeRequestApproval, respond with AcceptForSession, and verify the file exists.
    let turn_1_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "apply patch 1".into(),
                text_elements: Vec::new(),
            }],
            cwd: Some(workspace.clone()),
            ..Default::default()
        })
        .await?;
    let turn_1_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_1_req)),
    )
    .await??;
    let TurnStartResponse { turn: turn_1 } = to_response::<TurnStartResponse>(turn_1_resp)?;

    let started_file_change_1 = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let started_notif = mcp
                .read_stream_until_notification_message("item/started")
                .await?;
            let started: ItemStartedNotification =
                serde_json::from_value(started_notif.params.clone().expect("item/started params"))?;
            if let ThreadItem::FileChange { .. } = started.item {
                return Ok::<ThreadItem, anyhow::Error>(started.item);
            }
        }
    })
    .await??;
    let ThreadItem::FileChange { id, status, .. } = started_file_change_1 else {
        unreachable!("loop ensures we break on file change items");
    };
    assert_eq!(id, "patch-call-1");
    assert_eq!(status, PatchApplyStatus::InProgress);

    let server_req = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::FileChangeRequestApproval { request_id, params } = server_req else {
        panic!("expected FileChangeRequestApproval request")
    };
    assert_eq!(params.item_id, "patch-call-1");
    assert_eq!(params.thread_id, thread.id);
    assert_eq!(params.turn_id, turn_1.id);

    mcp.send_response(
        request_id,
        serde_json::to_value(FileChangeRequestApprovalResponse {
            decision: FileChangeApprovalDecision::AcceptForSession,
        })?,
    )
    .await?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("item/completed"),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let readme_path = workspace.join("README.md");
    assert_eq!(std::fs::read_to_string(&readme_path)?, "new line\n");

    // Second turn: apply a patch to the same file. Approval should be skipped due to AcceptForSession.
    let turn_2_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "apply patch 2".into(),
                text_elements: Vec::new(),
            }],
            cwd: Some(workspace.clone()),
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_2_req)),
    )
    .await??;

    let started_file_change_2 = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let started_notif = mcp
                .read_stream_until_notification_message("item/started")
                .await?;
            let started: ItemStartedNotification =
                serde_json::from_value(started_notif.params.clone().expect("item/started params"))?;
            if let ThreadItem::FileChange { .. } = started.item {
                return Ok::<ThreadItem, anyhow::Error>(started.item);
            }
        }
    })
    .await??;
    let ThreadItem::FileChange { id, status, .. } = started_file_change_2 else {
        unreachable!("loop ensures we break on file change items");
    };
    assert_eq!(id, "patch-call-2");
    assert_eq!(status, PatchApplyStatus::InProgress);

    // If the server incorrectly emits FileChangeRequestApproval, the helper below will error
    // (it bails on unexpected JSONRPCMessage::Request), causing the test to fail.
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("item/completed"),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    assert_eq!(std::fs::read_to_string(readme_path)?, "updated line\n");

    Ok(())
}

#[tokio::test]
async fn turn_start_file_change_approval_decline_v2() -> Result<()> {
    // TODO(anp): Materialize apply-patch workspaces in the selected remote environment.
    skip_if_remote!(
        Ok(()),
        "apply-patch workspace fixture is only materialized on the host"
    );
    skip_if_no_network!(Ok(()));

    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace)?;

    let patch = r#"*** Begin Patch
*** Add File: README.md
+new line
*** End Patch
"#;
    let responses = vec![
        create_apply_patch_sse_response(patch, "patch-call")?,
        create_final_assistant_message_sse_response("patch declined")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;
    create_config_toml(
        &codex_home,
        &server.uri(),
        "untrusted",
        &BTreeMap::default(),
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(&codex_home)
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let start_req = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            cwd: Some(workspace.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(start_resp)?;

    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "apply patch".into(),
                text_elements: Vec::new(),
            }],
            cwd: Some(workspace.clone()),
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    let TurnStartResponse { turn } = to_response::<TurnStartResponse>(turn_resp)?;

    let started_file_change = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let started_notif = mcp
                .read_stream_until_notification_message("item/started")
                .await?;
            let started: ItemStartedNotification =
                serde_json::from_value(started_notif.params.clone().expect("item/started params"))?;
            if let ThreadItem::FileChange { .. } = started.item {
                return Ok::<ThreadItem, anyhow::Error>(started.item);
            }
        }
    })
    .await??;
    let ThreadItem::FileChange {
        ref id,
        status,
        ref changes,
    } = started_file_change
    else {
        unreachable!("loop ensures we break on file change items");
    };
    assert_eq!(id, "patch-call");
    assert_eq!(status, PatchApplyStatus::InProgress);
    let started_changes = changes.clone();

    let server_req = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::FileChangeRequestApproval { request_id, params } = server_req else {
        panic!("expected FileChangeRequestApproval request")
    };
    assert_eq!(params.item_id, "patch-call");
    assert_eq!(params.thread_id, thread.id);
    assert_eq!(params.turn_id, turn.id);
    let expected_readme_path = workspace.join("README.md");
    let expected_readme_path_str = expected_readme_path.to_string_lossy().into_owned();
    pretty_assertions::assert_eq!(
        started_changes,
        vec![codex_app_server_protocol::FileUpdateChange {
            path: expected_readme_path_str.clone(),
            kind: PatchChangeKind::Add,
            diff: "new line\n".to_string(),
        }]
    );

    mcp.send_response(
        request_id,
        serde_json::to_value(FileChangeRequestApprovalResponse {
            decision: FileChangeApprovalDecision::Decline,
        })?,
    )
    .await?;

    let completed_file_change = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let completed_notif = mcp
                .read_stream_until_notification_message("item/completed")
                .await?;
            let completed: ItemCompletedNotification = serde_json::from_value(
                completed_notif
                    .params
                    .clone()
                    .expect("item/completed params"),
            )?;
            if let ThreadItem::FileChange { .. } = completed.item {
                return Ok::<ThreadItem, anyhow::Error>(completed.item);
            }
        }
    })
    .await??;
    let ThreadItem::FileChange { ref id, status, .. } = completed_file_change else {
        unreachable!("loop ensures we break on file change items");
    };
    assert_eq!(id, "patch-call");
    assert_eq!(status, PatchApplyStatus::Declined);

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    assert!(
        !expected_readme_path.exists(),
        "declined patch should not be applied"
    );

    Ok(())
}

#[tokio::test]
#[cfg_attr(windows, ignore = "process id reporting differs on Windows")]
async fn command_execution_notifications_include_process_id() -> Result<()> {
    // TODO(anp): Add target-Windows process-id expectations for remote executors.
    skip_if_wine_exec!(
        Ok(()),
        "process id reporting differs for a Windows executor"
    );
    skip_if_no_network!(Ok(()));

    let responses = vec![
        create_exec_command_sse_response("uexec-1")?,
        create_final_assistant_message_sse_response("done")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;
    let codex_home = TempDir::new()?;
    create_config_toml_with_sandbox(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::from([(Feature::UnifiedExec, true)]),
        "danger-full-access",
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let start_id = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_id)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(start_resp)?;

    let turn_id = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "run a command".to_string(),
                text_elements: Vec::new(),
            }],
            sandbox_policy: Some(codex_app_server_protocol::SandboxPolicy::DangerFullAccess),
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_id)),
    )
    .await??;
    let TurnStartResponse { turn: _turn } = to_response::<TurnStartResponse>(turn_resp)?;

    let started_command = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let notif = mcp
                .read_stream_until_notification_message("item/started")
                .await?;
            let started: ItemStartedNotification = serde_json::from_value(
                notif
                    .params
                    .clone()
                    .expect("item/started should include params"),
            )?;
            if let ThreadItem::CommandExecution { .. } = started.item {
                return Ok::<ThreadItem, anyhow::Error>(started.item);
            }
        }
    })
    .await??;
    let ThreadItem::CommandExecution {
        id,
        process_id: started_process_id,
        status,
        ..
    } = started_command
    else {
        unreachable!("loop ensures we break on command execution items");
    };
    assert_eq!(id, "uexec-1");
    assert_eq!(status, CommandExecutionStatus::InProgress);
    let started_process_id = started_process_id.expect("process id should be present");

    let completed_command = timeout(DEFAULT_READ_TIMEOUT, async {
        loop {
            let notif = mcp
                .read_stream_until_notification_message("item/completed")
                .await?;
            let completed: ItemCompletedNotification = serde_json::from_value(
                notif
                    .params
                    .clone()
                    .expect("item/completed should include params"),
            )?;
            if let ThreadItem::CommandExecution { .. } = completed.item {
                return Ok::<ThreadItem, anyhow::Error>(completed.item);
            }
        }
    })
    .await??;
    let ThreadItem::CommandExecution {
        id: completed_id,
        process_id: completed_process_id,
        status: completed_status,
        exit_code,
        ..
    } = completed_command
    else {
        unreachable!("loop ensures we break on command execution items");
    };
    assert_eq!(completed_id, "uexec-1");
    assert!(
        matches!(
            completed_status,
            CommandExecutionStatus::Completed | CommandExecutionStatus::Failed
        ),
        "unexpected command execution status: {completed_status:?}"
    );
    if completed_status == CommandExecutionStatus::Completed {
        assert_eq!(exit_code, Some(0));
    } else {
        assert!(exit_code.is_some(), "expected exit_code for failed command");
    }
    assert_eq!(
        completed_process_id.as_deref(),
        Some(started_process_id.as_str())
    );

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    Ok(())
}

#[tokio::test]
async fn turn_start_with_elevated_override_does_not_persist_project_trust() -> Result<()> {
    let responses = vec![create_final_assistant_message_sse_response("Done")?];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;

    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        "never",
        &BTreeMap::from([(Feature::Personality, true)]),
    )?;

    let workspace = TempDir::new()?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_request = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            cwd: Some(workspace.path().display().to_string()),
            ..Default::default()
        })
        .await?;
    let thread_response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_request)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_response)?;

    let turn_request = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            cwd: Some(workspace.path().to_path_buf()),
            sandbox_policy: Some(codex_app_server_protocol::SandboxPolicy::DangerFullAccess),
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_request)),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let config_toml = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
    assert!(!config_toml.contains("trust_level = \"trusted\""));
    assert!(!config_toml.contains(&workspace.path().display().to_string()));

    Ok(())
}

// Helper to create a config.toml pointing at the mock model server.
fn create_config_toml(
    codex_home: &Path,
    server_uri: &str,
    approval_policy: &str,
    feature_flags: &BTreeMap<Feature, bool>,
) -> std::io::Result<()> {
    create_config_toml_with_sandbox(
        codex_home,
        server_uri,
        approval_policy,
        feature_flags,
        "read-only",
    )
}

fn create_config_toml_with_sandbox(
    codex_home: &Path,
    server_uri: &str,
    approval_policy: &str,
    feature_flags: &BTreeMap<Feature, bool>,
    sandbox_mode: &str,
) -> std::io::Result<()> {
    let mut features = BTreeMap::new();
    for (feature, enabled) in feature_flags {
        features.insert(*feature, *enabled);
    }
    let feature_entries = features
        .into_iter()
        .map(|(feature, enabled)| {
            let key = FEATURES
                .iter()
                .find(|spec| spec.id == feature)
                .map(|spec| spec.key)
                .expect("feature should have a config key");
            format!("{key} = {enabled}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let config_toml = codex_home.join("config.toml");
    std::fs::write(
        config_toml,
        format!(
            r#"
model = "mock-model"
approval_policy = "{approval_policy}"
sandbox_mode = "{sandbox_mode}"

model_provider = "mock_provider"

[features]
{feature_entries}

[model_providers.mock_provider]
name = "Mock provider for test"
base_url = "{server_uri}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
"#
        ),
    )
}

fn write_test_skill(codex_home: &Path, name: &str) -> std::io::Result<()> {
    let skill_dir = codex_home.join("skills").join(name);
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {name} description\n---\n\n# Body\n"),
    )
}
