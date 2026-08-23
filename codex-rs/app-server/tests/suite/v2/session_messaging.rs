use std::collections::HashMap;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::SessionMessageReceivedNotification;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use codex_features::Feature;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

const TEST_TIMEOUT: Duration = Duration::from_secs(30);
const LIST_CALL_ID: &str = "list-live-sessions";
const SEND_CALL_ID: &str = "send-live-session-message";
const PEER_MESSAGE: &str = "Please verify the shared API contract.";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_discover_and_message_an_idle_root_session_with_peer_provenance() -> Result<()> {
    let server = responses::start_mock_server().await;
    let codex_home = TempDir::new()?;
    let recipient_cwd = TempDir::new()?;
    let sender_cwd = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .enable_feature(Feature::CrossSessionMessaging)
        .with_root_config("include_environment_context = false")
        .write(codex_home.path())?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse {
        thread: recipient, ..
    } = app_server
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            cwd: Some(recipient_cwd.path().display().to_string()),
            ..Default::default()
        })
        .await?;
    let ThreadStartResponse { thread: sender, .. } = app_server
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            cwd: Some(sender_cwd.path().display().to_string()),
            ..Default::default()
        })
        .await?;

    let send_arguments = json!({
        "thread_id": recipient.id,
        "message": PEER_MESSAGE,
    })
    .to_string();
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-1"),
                responses::ev_function_call(LIST_CALL_ID, "list_sessions", "{}"),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-2"),
                responses::ev_function_call(SEND_CALL_ID, "send_session_message", &send_arguments),
                responses::ev_completed("resp-2"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("msg-1", "Message accepted."),
                responses::ev_completed("resp-3"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("msg-2", "Contract checked."),
                responses::ev_completed("resp-4"),
            ]),
        ],
    )
    .await;

    let _: TurnStartResponse = app_server
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: sender.id.clone(),
                input: vec![UserInput::Text {
                    text: "Coordinate with the other live session.".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;

    let requests = timeout(TEST_TIMEOUT, async {
        loop {
            let requests = response_mock.requests();
            if requests.len() >= 4 {
                return requests;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert_eq!(requests.len(), 4);
    let first_request = requests.first().context("missing sender request")?;
    assert!(first_request.body_contains_text("list_sessions"));
    assert!(first_request.body_contains_text("send_session_message"));

    let list_output = response_mock
        .function_call_output_text(LIST_CALL_ID)
        .context("missing list_sessions output")?;
    assert!(list_output.contains(&recipient.id));

    let recipient_turn_started = timeout(
        TEST_TIMEOUT,
        app_server.read_stream_until_matching_notification(
            "recipient turn/started",
            |notification| {
                notification.method == "turn/started"
                    && notification
                        .params
                        .as_ref()
                        .and_then(|params| params.get("threadId"))
                        .and_then(serde_json::Value::as_str)
                        == Some(recipient.id.as_str())
            },
        ),
    )
    .await??;
    let recipient_turn_started: ServerNotification = recipient_turn_started.try_into()?;
    let ServerNotification::TurnStarted(recipient_turn_started) = recipient_turn_started else {
        unreachable!()
    };

    let notification = timeout(
        TEST_TIMEOUT,
        app_server.read_stream_until_notification_message("sessionMessage/received"),
    )
    .await??;
    let notification: ServerNotification = notification.try_into()?;
    let ServerNotification::SessionMessageReceived(notification) = notification else {
        unreachable!()
    };
    assert_eq!(
        notification,
        SessionMessageReceivedNotification {
            thread_id: recipient.id.clone(),
            turn_id: recipient_turn_started.turn.id,
            sender_thread_id: sender.id.clone(),
            message: PEER_MESSAGE.to_string(),
        }
    );

    let recipient_request = requests
        .iter()
        .find(|request| !request.inputs_of_type("agent_message").is_empty())
        .context("missing recipient peer-message request")?;
    assert!(recipient_request.body_contains_text(PEER_MESSAGE));
    assert!(recipient_request.body_contains_text(&sender.id));
    assert!(recipient_request.body_contains_text(&recipient.id));
    assert!(
        recipient_request
            .message_input_texts("user")
            .iter()
            .all(|text| !text.contains(PEER_MESSAGE))
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_refuse_a_recipient_that_has_not_opted_in() -> Result<()> {
    let server = responses::start_mock_server().await;
    let codex_home = TempDir::new()?;
    let recipient_cwd = TempDir::new()?;
    let sender_cwd = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .enable_feature(Feature::CrossSessionMessaging)
        .with_root_config("include_environment_context = false")
        .write(codex_home.path())?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let ThreadStartResponse {
        thread: recipient, ..
    } = app_server
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            cwd: Some(recipient_cwd.path().display().to_string()),
            config: Some(HashMap::from([(
                "features.cross_session_messaging".to_string(),
                json!(false),
            )])),
            ..Default::default()
        })
        .await?;
    let ThreadStartResponse { thread: sender, .. } = app_server
        .start_thread(ThreadStartParams {
            model: Some("mock-model".to_string()),
            cwd: Some(sender_cwd.path().display().to_string()),
            ..Default::default()
        })
        .await?;

    let send_arguments = json!({
        "thread_id": recipient.id,
        "message": PEER_MESSAGE,
    })
    .to_string();
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-1"),
                responses::ev_function_call(LIST_CALL_ID, "list_sessions", "{}"),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-2"),
                responses::ev_function_call(SEND_CALL_ID, "send_session_message", &send_arguments),
                responses::ev_completed("resp-2"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("msg-1", "Delivery refused."),
                responses::ev_completed("resp-3"),
            ]),
        ],
    )
    .await;

    let _: TurnStartResponse = app_server
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: sender.id.clone(),
                input: vec![UserInput::Text {
                    text: "Coordinate with the other live session.".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;
    timeout(
        TEST_TIMEOUT,
        app_server.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let list_output = response_mock
        .function_call_output_text(LIST_CALL_ID)
        .context("missing list_sessions output")?;
    assert!(!list_output.contains(&recipient.id));
    let send_output = response_mock
        .function_call_output_text(SEND_CALL_ID)
        .context("missing send_session_message output")?;
    assert!(send_output.contains("target session has not enabled cross-session messaging"));
    assert_eq!(response_mock.requests().len(), 3);

    Ok(())
}
