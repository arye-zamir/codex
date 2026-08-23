use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Weak;

use codex_core::CodexThread;
use codex_core::ThreadManager;
use codex_extension_api::FunctionCallError;
use codex_extension_api::JsonToolOutput;
use codex_extension_api::ResponsesApiTool;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolExecutor;
use codex_extension_api::ToolName;
use codex_extension_api::ToolOutput;
use codex_extension_api::ToolSpec;
use codex_protocol::ThreadId;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::Op;
use codex_tools::JsonSchema;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_string::approx_token_count;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;

use crate::budget::OUTBOUND_MESSAGE_LIMIT;
use crate::budget::SessionMessageBudget;
use crate::budget::SessionMessageReservation;
use crate::extension::SessionMessagingConfig;
use crate::extension::SessionMessagingIdentity;

pub(crate) const LIST_SESSIONS_TOOL_NAME: &str = "list_sessions";
pub(crate) const SEND_SESSION_MESSAGE_TOOL_NAME: &str = "send_session_message";
/// Upper bound on a single message body, leaving headroom for the provenance
/// envelope rendered around it.
pub(crate) const MESSAGE_MAX_APPROX_TOKENS: usize = 8_000;

pub(crate) struct SessionMessagingRuntime {
    thread_manager: Weak<ThreadManager>,
    sender_thread_id: ThreadId,
    message_budget: Arc<SessionMessageBudget>,
}

impl SessionMessagingRuntime {
    pub(crate) fn new(
        thread_manager: Weak<ThreadManager>,
        sender_thread_id: ThreadId,
        message_budget: Arc<SessionMessageBudget>,
    ) -> Self {
        Self {
            thread_manager,
            sender_thread_id,
            message_budget,
        }
    }

    /// Upgrades the thread manager and confirms the sending session is still
    /// eligible for cross-session messaging.
    async fn manager(&self) -> Result<Arc<ThreadManager>, FunctionCallError> {
        let manager = self.thread_manager.upgrade().ok_or_else(|| {
            FunctionCallError::Fatal("shared app-server process is no longer available".to_string())
        })?;
        let sender_unavailable = || {
            FunctionCallError::RespondToModel(
                "current session is no longer eligible for cross-session messaging".to_string(),
            )
        };
        let sender_thread = manager
            .get_thread(self.sender_thread_id)
            .await
            .map_err(|_| sender_unavailable())?;
        let SessionAvailability::Eligible(_) = inspect_session(sender_thread).await else {
            return Err(sender_unavailable());
        };
        Ok(manager)
    }

    fn reserve_outbound_message(&self) -> Result<SessionMessageReservation, FunctionCallError> {
        self.message_budget.reserve().ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "outbound session message limit reached; ask the user for new explicit input before sending another session message"
                    .to_string(),
            )
        })
    }
}

pub(crate) struct ListSessionsTool {
    runtime: Arc<SessionMessagingRuntime>,
}

impl ListSessionsTool {
    pub(crate) fn new(runtime: Arc<SessionMessagingRuntime>) -> Self {
        Self { runtime }
    }

    async fn handle_call(&self, call: ToolCall) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        parse_list_args(call.function_arguments()?)?;
        let manager = self.runtime.manager().await?;
        let mut sessions = Vec::new();
        for thread_id in manager.list_thread_ids().await {
            if thread_id == self.runtime.sender_thread_id {
                continue;
            }
            let Ok(thread) = manager.get_thread(thread_id).await else {
                continue;
            };
            let SessionAvailability::Eligible(session) = inspect_session(thread).await else {
                continue;
            };
            sessions.push(SessionSummary {
                thread_id: session.thread_id.to_string(),
                name: session.name,
                cwd: session.cwd.display().to_string(),
                status: session.status,
            });
        }
        sessions.sort_by(|left, right| left.thread_id.cmp(&right.thread_id));
        let response = ListSessionsResponse {
            scope: "live_root_sessions",
            transport: "shared_app_server_process",
            sessions,
        };
        Ok(Box::new(JsonToolOutput::new(json!(response))))
    }
}

impl ToolExecutor<ToolCall> for ListSessionsTool {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(LIST_SESSIONS_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        list_sessions_tool_spec()
    }

    fn handle(&self, call: ToolCall) -> codex_extension_api::ToolExecutorFuture<'_> {
        Box::pin(self.handle_call(call))
    }
}

pub(crate) struct SendSessionMessageTool {
    runtime: Arc<SessionMessagingRuntime>,
}

impl SendSessionMessageTool {
    pub(crate) fn new(runtime: Arc<SessionMessagingRuntime>) -> Self {
        Self { runtime }
    }

    async fn handle_call(&self, call: ToolCall) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let args = parse_send_args(call.function_arguments()?)?;
        validate_message(&args.message)?;
        let recipient_thread_id = ThreadId::from_string(&args.thread_id).map_err(|_| {
            FunctionCallError::RespondToModel("thread_id must be a valid session UUID".to_string())
        })?;
        if recipient_thread_id == self.runtime.sender_thread_id {
            return Err(FunctionCallError::RespondToModel(
                "cannot send a session message to the current session".to_string(),
            ));
        }
        let manager = self.runtime.manager().await?;
        let target_unavailable = || {
            FunctionCallError::RespondToModel(
                "target session is not live in this shared app-server process".to_string(),
            )
        };
        let recipient_thread = manager
            .get_thread(recipient_thread_id)
            .await
            .map_err(|_| target_unavailable())?;
        let recipient = match inspect_session(recipient_thread).await {
            SessionAvailability::Eligible(recipient) => recipient,
            SessionAvailability::FeatureDisabled => {
                return Err(FunctionCallError::RespondToModel(
                    "target session has not enabled cross-session messaging".to_string(),
                ));
            }
            SessionAvailability::OutsideScope => {
                return Err(FunctionCallError::RespondToModel(
                    "target must be a non-ephemeral independent root session".to_string(),
                ));
            }
            SessionAvailability::NotLive => return Err(target_unavailable()),
        };
        let communication = InterAgentCommunication::new_session_message(
            self.runtime.sender_thread_id,
            recipient_thread_id,
            args.message,
        );
        let reservation = self.runtime.reserve_outbound_message()?;
        let receiver_submission_id = recipient
            .thread
            .submit(Op::InterAgentCommunication { communication })
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "failed to enqueue session message for target session dispatch: {err}"
                ))
            })?;
        reservation.commit();
        let receipt = SendSessionMessageReceipt {
            state: "enqueued_for_target_session_dispatch",
            sender_thread_id: self.runtime.sender_thread_id.to_string(),
            recipient_thread_id: recipient_thread_id.to_string(),
            receiver_submission_id,
            processed_by_model: false,
            detail: "The message was accepted and enqueued for target session dispatch; it has not necessarily been consumed by the target session or processed by its model.",
        };
        Ok(Box::new(JsonToolOutput::new(json!(receipt))))
    }
}

impl ToolExecutor<ToolCall> for SendSessionMessageTool {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(SEND_SESSION_MESSAGE_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        send_session_message_tool_spec()
    }

    fn handle(&self, call: ToolCall) -> codex_extension_api::ToolExecutorFuture<'_> {
        Box::pin(self.handle_call(call))
    }
}

struct LiveSession {
    thread: Arc<CodexThread>,
    thread_id: ThreadId,
    name: Option<String>,
    cwd: AbsolutePathBuf,
    status: &'static str,
}

enum SessionAvailability {
    Eligible(LiveSession),
    FeatureDisabled,
    OutsideScope,
    NotLive,
}

async fn inspect_session(thread: Arc<CodexThread>) -> SessionAvailability {
    let Some(config) = thread
        .thread_extension_data()
        .get::<SessionMessagingConfig>()
    else {
        return SessionAvailability::NotLive;
    };
    let Some(identity) = thread
        .thread_extension_data()
        .get::<SessionMessagingIdentity>()
    else {
        return SessionAvailability::NotLive;
    };
    if !identity.is_root || config.ephemeral {
        return SessionAvailability::OutsideScope;
    }
    if !config.enabled {
        return SessionAvailability::FeatureDisabled;
    }
    let Some(status) = coarse_status(&thread.agent_status().await) else {
        return SessionAvailability::NotLive;
    };
    let configured = thread.session_configured();
    SessionAvailability::Eligible(LiveSession {
        thread,
        thread_id: configured.thread_id,
        name: configured.thread_name,
        cwd: config.cwd.clone(),
        status,
    })
}

fn coarse_status(status: &AgentStatus) -> Option<&'static str> {
    match status {
        AgentStatus::PendingInit => Some("starting"),
        AgentStatus::Running => Some("running"),
        AgentStatus::Interrupted | AgentStatus::Completed(_) => Some("idle"),
        AgentStatus::Errored(_) => Some("error"),
        AgentStatus::Shutdown | AgentStatus::NotFound => None,
    }
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct ListSessionsArgs {}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct SendSessionMessageArgs {
    thread_id: String,
    message: String,
}

fn parse_list_args(arguments: &str) -> Result<ListSessionsArgs, FunctionCallError> {
    serde_json::from_str(arguments).map_err(|err| {
        FunctionCallError::RespondToModel(format!("invalid list_sessions arguments: {err}"))
    })
}

fn parse_send_args(arguments: &str) -> Result<SendSessionMessageArgs, FunctionCallError> {
    serde_json::from_str(arguments).map_err(|err| {
        FunctionCallError::RespondToModel(format!("invalid send_session_message arguments: {err}"))
    })
}

fn validate_message(message: &str) -> Result<(), FunctionCallError> {
    if message.trim().is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "message must not be empty".to_string(),
        ));
    }
    let approx_tokens = approx_token_count(message);
    if approx_tokens > MESSAGE_MAX_APPROX_TOKENS {
        return Err(FunctionCallError::RespondToModel(format!(
            "message exceeds the maximum approximate token count of {MESSAGE_MAX_APPROX_TOKENS} ({approx_tokens} approximate tokens provided)"
        )));
    }
    Ok(())
}

fn list_sessions_tool_spec() -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: LIST_SESSIONS_TOOL_NAME.to_string(),
        description: "List live independent root sessions that share this app-server daemon/process. Session UUIDs are the stable routing keys for send_session_message; each entry includes its working directory for identification."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(BTreeMap::new(), Some(Vec::new()), Some(false.into())),
        output_schema: None,
    })
}

fn send_session_message_tool_spec() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "thread_id".to_string(),
            JsonSchema::string(Some(
                "Stable UUID of a target returned by list_sessions.".to_string(),
            )),
        ),
        (
            "message".to_string(),
            JsonSchema::string(Some(format!(
                "Non-empty peer message with a maximum of {MESSAGE_MAX_APPROX_TOKENS} approximate tokens."
            ))),
        ),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: SEND_SESSION_MESSAGE_TOOL_NAME.to_string(),
        description: format!(
            "Send a peer message to a live independent root session in the shared app-server daemon/process. Use the stable session UUID returned by list_sessions. Delivery wakes an idle target. For a busy target, the message is queued for next mailbox check; it may end a current model response early but does not cancel running tools. At most {OUTBOUND_MESSAGE_LIMIT} successful messages may be sent after each explicit user input."
        ),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            /*required*/ Some(vec!["thread_id".to_string(), "message".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

#[derive(Serialize)]
struct ListSessionsResponse {
    scope: &'static str,
    transport: &'static str,
    sessions: Vec<SessionSummary>,
}

#[derive(Serialize)]
struct SessionSummary {
    thread_id: String,
    name: Option<String>,
    cwd: String,
    status: &'static str,
}

#[derive(Serialize)]
struct SendSessionMessageReceipt {
    state: &'static str,
    sender_thread_id: String,
    recipient_thread_id: String,
    receiver_submission_id: String,
    processed_by_model: bool,
    detail: &'static str,
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
