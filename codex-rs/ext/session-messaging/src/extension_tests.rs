use std::sync::Arc;

use codex_extension_api::ExtensionData;
use codex_protocol::user_input::UserInput;

use super::reset_budget_for_explicit_user_input;
use crate::budget::OUTBOUND_MESSAGE_LIMIT;
use crate::budget::SessionMessageBudget;

fn exhausted_budget_store() -> (ExtensionData, Arc<SessionMessageBudget>) {
    let thread_store = ExtensionData::new("thread-1");
    thread_store.insert(SessionMessageBudget::default());
    let budget = thread_store.get::<SessionMessageBudget>().expect("budget");
    for _ in 0..OUTBOUND_MESSAGE_LIMIT {
        budget.reserve().expect("reservation").commit();
    }
    (thread_store, budget)
}

#[test]
fn empty_peer_turn_does_not_reset_the_budget() {
    let (thread_store, budget) = exhausted_budget_store();
    reset_budget_for_explicit_user_input(&thread_store, &[]);
    assert!(budget.reserve().is_none());
}

#[test]
fn explicit_user_input_resets_the_budget() {
    let (thread_store, budget) = exhausted_budget_store();
    reset_budget_for_explicit_user_input(
        &thread_store,
        &[UserInput::Text {
            text: "Continue with another session message.".to_string(),
            text_elements: Vec::new(),
        }],
    );
    assert!(budget.reserve().is_some());
}
