use std::sync::Arc;
use std::sync::Weak;

use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_extension_api::ConfigContributor;
use codex_extension_api::ContextualUserFragment;
use codex_extension_api::ExtensionData;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionMetrics;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::ThreadLifecycleContributor;
use codex_extension_api::ThreadStartInput;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolContributor;
use codex_extension_api::ToolExecutor;
use codex_extension_api::TurnInputContext;
use codex_extension_api::TurnInputContributor;
use codex_features::Feature;
use codex_protocol::ThreadId;
use codex_protocol::user_input::UserInput;
use codex_utils_absolute_path::AbsolutePathBuf;

use crate::budget::SessionMessageBudget;
use crate::tools::ListSessionsTool;
use crate::tools::SendSessionMessageTool;
use crate::tools::SessionMessagingRuntime;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionMessagingConfig {
    pub(crate) enabled: bool,
    pub(crate) ephemeral: bool,
    pub(crate) cwd: AbsolutePathBuf,
}

impl SessionMessagingConfig {
    fn from_config(config: &Config) -> Self {
        Self {
            enabled: config.features.enabled(Feature::CrossSessionMessaging),
            ephemeral: config.ephemeral,
            cwd: config
                .cwd
                .canonicalize()
                .unwrap_or_else(|_| config.cwd.clone()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SessionMessagingIdentity {
    pub(crate) is_root: bool,
}

struct SessionMessagingExtension {
    thread_manager: Weak<ThreadManager>,
}

impl ThreadLifecycleContributor<Config> for SessionMessagingExtension {
    fn on_thread_start<'a>(
        &'a self,
        input: ThreadStartInput<'a, Config>,
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            input
                .thread_store
                .insert(SessionMessagingConfig::from_config(input.config));
            input.thread_store.insert(SessionMessagingIdentity {
                is_root: !input.session_source.is_non_root_agent(),
            });
            input.thread_store.insert(SessionMessageBudget::default());
        })
    }
}

impl ConfigContributor<Config> for SessionMessagingExtension {
    fn on_config_changed(
        &self,
        _session_store: &ExtensionData,
        thread_store: &ExtensionData,
        _previous_config: &Config,
        new_config: &Config,
    ) {
        thread_store.insert(SessionMessagingConfig::from_config(new_config));
    }
}

impl ToolContributor for SessionMessagingExtension {
    fn tools(
        &self,
        _session_store: &ExtensionData,
        thread_store: &ExtensionData,
    ) -> Vec<Arc<dyn ToolExecutor<ToolCall>>> {
        if !tools_enabled(thread_store) {
            return Vec::new();
        }
        let Ok(sender_thread_id) = ThreadId::from_string(thread_store.level_id()) else {
            return Vec::new();
        };
        let Some(message_budget) = thread_store.get::<SessionMessageBudget>() else {
            return Vec::new();
        };
        let runtime = Arc::new(SessionMessagingRuntime::new(
            self.thread_manager.clone(),
            sender_thread_id,
            message_budget,
        ));
        vec![
            Arc::new(ListSessionsTool::new(Arc::clone(&runtime))),
            Arc::new(SendSessionMessageTool::new(runtime)),
        ]
    }
}

impl TurnInputContributor for SessionMessagingExtension {
    fn contribute<'a>(
        &'a self,
        input: TurnInputContext,
        _extension_metrics: Option<Arc<dyn ExtensionMetrics>>,
        _session_store: &'a ExtensionData,
        thread_store: &'a ExtensionData,
        _turn_store: &'a ExtensionData,
    ) -> ExtensionFuture<'a, Vec<Box<dyn ContextualUserFragment + Send>>> {
        Box::pin(async move {
            reset_budget_for_explicit_user_input(thread_store, &input.user_input);
            Vec::new()
        })
    }
}

fn reset_budget_for_explicit_user_input(thread_store: &ExtensionData, user_input: &[UserInput]) {
    if user_input.is_empty() {
        return;
    }
    if let Some(budget) = thread_store.get::<SessionMessageBudget>() {
        budget.reset();
    }
}

fn tools_enabled(thread_store: &ExtensionData) -> bool {
    let Some(config) = thread_store.get::<SessionMessagingConfig>() else {
        return false;
    };
    let Some(identity) = thread_store.get::<SessionMessagingIdentity>() else {
        return false;
    };
    config.enabled && !config.ephemeral && identity.is_root
}

pub fn install(
    registry: &mut ExtensionRegistryBuilder<Config>,
    thread_manager: Weak<ThreadManager>,
) {
    let extension = Arc::new(SessionMessagingExtension { thread_manager });
    registry.thread_lifecycle_contributor(extension.clone());
    registry.config_contributor(extension.clone());
    registry.turn_input_contributor(extension.clone());
    registry.tool_contributor(extension);
}

#[cfg(test)]
#[path = "extension_tests.rs"]
mod tests;
