use codex_extension_api::FunctionCallError;
use codex_protocol::ThreadId;
use codex_protocol::models::AgentMessageInputContent;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::InterAgentCommunication;
use codex_tools::ToolSpec;
use codex_utils_string::approx_token_count;
use pretty_assertions::assert_eq;

use super::LIST_SESSIONS_TOOL_NAME;
use super::MESSAGE_MAX_APPROX_TOKENS;
use super::SEND_SESSION_MESSAGE_TOOL_NAME;
use super::coarse_status;
use super::list_sessions_tool_spec;
use super::parse_list_args;
use super::parse_send_args;
use super::send_session_message_tool_spec;
use super::validate_message;

/// Mirrors `MAX_RETAINED_AGENT_MESSAGE_TOKENS` in codex-core's remote compaction:
/// a rendered session message must stay under it to survive compaction.
const MAX_RETAINED_AGENT_MESSAGE_TOKENS: usize = 10_000;

#[test]
fn tool_specs_expose_the_expected_routing_contract() {
    let ToolSpec::Function(list) = list_sessions_tool_spec() else {
        panic!("list_sessions should be a function tool");
    };
    assert_eq!(list.name, LIST_SESSIONS_TOOL_NAME);
    assert_eq!(list.parameters.required, Some(Vec::new()));

    let ToolSpec::Function(send) = send_session_message_tool_spec() else {
        panic!("send_session_message should be a function tool");
    };
    assert_eq!(send.name, SEND_SESSION_MESSAGE_TOOL_NAME);
    assert_eq!(
        send.parameters.required,
        Some(vec!["thread_id".to_string(), "message".to_string()])
    );
}

#[test]
fn send_arguments_parse_as_a_uuid_and_message_pair() {
    let args = parse_send_args(
        r#"{"thread_id":"00000000-0000-0000-0000-000000000002","message":"Can you review the API contract?"}"#,
    )
    .expect("valid send arguments");
    assert_eq!(args.thread_id, ThreadId::from_u128(2).to_string());
    assert_eq!(args.message, "Can you review the API contract?");
}

#[test]
fn list_arguments_accept_an_empty_object_and_reject_unknown_fields() {
    parse_list_args("{}").expect("empty object");
    let FunctionCallError::RespondToModel(error) =
        parse_list_args(r#"{"unexpected":true}"#).expect_err("unknown field")
    else {
        panic!("unknown fields should produce a model-visible error");
    };
    assert!(error.starts_with("invalid list_sessions arguments: "));
}

#[test]
fn invalid_send_arguments_return_a_model_visible_error() {
    let FunctionCallError::RespondToModel(error) =
        parse_send_args("{").expect_err("malformed json")
    else {
        panic!("malformed arguments should produce a model-visible error");
    };
    assert!(error.starts_with("invalid send_session_message arguments: "));
}

#[test]
fn message_validation_rejects_empty_and_oversized_payloads() {
    assert_eq!(
        validate_message(" \n"),
        Err(FunctionCallError::RespondToModel(
            "message must not be empty".to_string()
        ))
    );
    let maximum = "xxxx".repeat(MESSAGE_MAX_APPROX_TOKENS);
    assert_eq!(validate_message(&maximum), Ok(()));
    let oversized = "xxxx".repeat(MESSAGE_MAX_APPROX_TOKENS + 1);
    let FunctionCallError::RespondToModel(error) =
        validate_message(&oversized).expect_err("oversized message")
    else {
        panic!("oversized messages should produce a model-visible error");
    };
    assert!(error.starts_with("message exceeds the maximum approximate token count of "));
}

#[test]
fn maximum_session_message_preserves_item_cap_headroom() {
    let communication = InterAgentCommunication::new_session_message(
        ThreadId::from_u128(1),
        ThreadId::from_u128(2),
        "xxxx".repeat(MESSAGE_MAX_APPROX_TOKENS),
    );
    let ResponseItem::AgentMessage { content, .. } = communication.to_model_input_item() else {
        panic!("session message should render as an agent message");
    };
    let [AgentMessageInputContent::InputText { text }] = content.as_slice() else {
        panic!("session message should render as a single text part");
    };
    assert!(approx_token_count(text) <= MAX_RETAINED_AGENT_MESSAGE_TOKENS);
}

#[test]
fn agent_status_maps_to_live_session_status() {
    let cases = [
        (AgentStatus::PendingInit, Some("starting")),
        (AgentStatus::Running, Some("running")),
        (AgentStatus::Interrupted, Some("idle")),
        (
            AgentStatus::Completed(Some("done".to_string())),
            Some("idle"),
        ),
        (AgentStatus::Errored("failure".to_string()), Some("error")),
        (AgentStatus::Shutdown, None),
        (AgentStatus::NotFound, None),
    ];
    for (status, expected) in cases {
        assert_eq!(coarse_status(&status), expected);
    }
}
