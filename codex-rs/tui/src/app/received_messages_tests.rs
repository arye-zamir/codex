use super::*;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

const RECIPIENT_THREAD_ID: &str = "00000000-0000-4000-8000-000000000001";
const SENDER_THREAD_ID: &str = "00000000-0000-4000-8000-000000000002";

fn recipient_thread_id() -> ThreadId {
    ThreadId::from_string(RECIPIENT_THREAD_ID).expect("valid thread id")
}

fn notification(message: &str) -> SessionMessageReceivedNotification {
    SessionMessageReceivedNotification {
        thread_id: RECIPIENT_THREAD_ID.to_string(),
        turn_id: "turn-1".to_string(),
        sender_thread_id: SENDER_THREAD_ID.to_string(),
        message: message.to_string(),
    }
}

#[test]
fn received_message_history_keeps_newest_entries_in_order() {
    let mut history = ReceivedMessageHistory::default();
    for index in 0..=MAX_MESSAGES_PER_THREAD {
        history.record(&notification(&index.to_string()));
    }

    let thread_history = history
        .messages_by_thread
        .get(&recipient_thread_id())
        .expect("thread history");
    let retained = thread_history
        .messages
        .iter()
        .map(|message| message.message.clone())
        .collect::<Vec<_>>();
    let expected = (1..=MAX_MESSAGES_PER_THREAD)
        .rev()
        .map(|index| index.to_string())
        .collect::<Vec<_>>();
    assert_eq!(retained, expected);
    assert_eq!(thread_history.omitted_count, 1);
}

#[test]
fn received_messages_viewer_shows_empty_state() {
    let history = ReceivedMessageHistory::default();

    assert_eq!(
        history.viewer_lines(Some(recipient_thread_id())),
        vec!["No received messages for this session.".italic().into()]
    );
}

#[tokio::test]
async fn discard_thread_local_state_removes_received_messages() {
    let mut app = crate::app::test_support::make_test_app().await;
    app.record_received_message(&notification("hello"));

    app.discard_thread_local_state(recipient_thread_id()).await;

    assert_eq!(
        app.received_messages
            .viewer_lines(Some(recipient_thread_id())),
        vec!["No received messages for this session.".italic().into()]
    );
}

#[test]
fn received_messages_viewer_narrow_snapshot() {
    let mut history = ReceivedMessageHistory::default();
    for index in 0..MAX_MESSAGES_PER_THREAD - 1 {
        history.record(&notification(&index.to_string()));
    }
    history.record(&notification("Older message"));
    history.record(&notification("Latest line one\nLatest line two"));
    let mut overlay = crate::pager_overlay::StaticOverlay::with_title(
        history.viewer_lines(Some(recipient_thread_id())),
        "R E C E I V E D".to_string(),
        crate::keymap::RuntimeKeymap::defaults().pager,
    );
    let mut terminal = Terminal::new(TestBackend::new(24, 22)).expect("test terminal");

    terminal
        .draw(|frame| overlay.render(frame.area(), frame.buffer_mut()))
        .expect("draw received messages viewer");

    assert_snapshot!(terminal.backend());
}

#[tokio::test]
async fn ctrl_q_opens_received_messages_overlay() -> color_eyre::Result<()> {
    let mut app = crate::app::test_support::make_test_app().await;
    let mut app_server =
        crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref()).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;

    app.handle_key_event(
        &mut tui,
        &mut app_server,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
    )
    .await;

    assert!(matches!(app.overlay, Some(Overlay::Static(_))));
    Ok(())
}
