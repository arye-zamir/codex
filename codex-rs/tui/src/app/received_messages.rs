use std::collections::HashMap;
use std::collections::VecDeque;

use codex_app_server_protocol::SessionMessageReceivedNotification;
use codex_protocol::ThreadId;
use ratatui::style::Stylize;
use ratatui::text::Line;

use super::App;
use crate::pager_overlay::Overlay;
use crate::tui;

/// Maximum number of received messages retained per thread.
const MAX_MESSAGES_PER_THREAD: usize = 20;

struct ReceivedMessage {
    sender_thread_id: String,
    message: String,
}

#[derive(Default)]
struct ThreadReceivedMessages {
    /// Newest first.
    messages: VecDeque<ReceivedMessage>,
    /// Oldest messages dropped to stay within `MAX_MESSAGES_PER_THREAD`.
    omitted_count: usize,
}

/// Cross-session messages received by each thread, shown in the received
/// messages overlay.
#[derive(Default)]
pub(super) struct ReceivedMessageHistory {
    messages_by_thread: HashMap<ThreadId, ThreadReceivedMessages>,
}

impl ReceivedMessageHistory {
    fn record(&mut self, notification: &SessionMessageReceivedNotification) {
        let Ok(thread_id) = ThreadId::from_string(&notification.thread_id) else {
            return;
        };
        let thread_history = self.messages_by_thread.entry(thread_id).or_default();
        thread_history.messages.push_front(ReceivedMessage {
            sender_thread_id: notification.sender_thread_id.clone(),
            message: notification.message.clone(),
        });
        if thread_history.messages.len() > MAX_MESSAGES_PER_THREAD {
            thread_history.messages.pop_back();
            thread_history.omitted_count += 1;
        }
    }

    fn viewer_lines(&self, thread_id: Option<ThreadId>) -> Vec<Line<'static>> {
        let Some(thread_history) = thread_id
            .and_then(|id| self.messages_by_thread.get(&id))
            .filter(|history| !history.messages.is_empty())
        else {
            return vec!["No received messages for this session.".italic().into()];
        };

        let mut lines = vec![
            history_disclosure_line(thread_history.omitted_count),
            Line::default(),
        ];
        for (index, message) in thread_history.messages.iter().enumerate() {
            if index > 0 {
                lines.push(Line::default());
            }
            lines.push(
                vec![
                    "From session: ".dim(),
                    message.sender_thread_id.clone().cyan(),
                ]
                .into(),
            );
            lines.extend(
                message
                    .message
                    .lines()
                    .map(|line| Line::from(line.to_owned())),
            );
        }
        lines
    }

    pub(super) fn remove_thread(&mut self, thread_id: ThreadId) {
        self.messages_by_thread.remove(&thread_id);
    }

    pub(super) fn clear(&mut self) {
        self.messages_by_thread.clear();
    }
}

fn history_disclosure_line(omitted_count: usize) -> Line<'static> {
    let disclosure = if omitted_count == 0 {
        format!("Newest first · up to {MAX_MESSAGES_PER_THREAD} messages retained.")
    } else {
        let noun = if omitted_count == 1 {
            "message"
        } else {
            "messages"
        };
        format!(
            "Newest first · {omitted_count} older {noun} omitted ({MAX_MESSAGES_PER_THREAD}-message cap)."
        )
    };
    Line::from(disclosure).dim()
}

impl App {
    pub(super) fn record_received_message(
        &mut self,
        notification: &SessionMessageReceivedNotification,
    ) {
        self.received_messages.record(notification);
    }

    pub(super) fn open_received_messages_overlay(&mut self, tui: &mut tui::Tui) {
        let lines = self
            .received_messages
            .viewer_lines(self.current_displayed_thread_id());
        let _ = tui.enter_alt_screen();
        self.overlay = Some(Overlay::new_static_with_lines(
            lines,
            "R E C E I V E D".to_string(),
            self.keymap.pager.clone(),
        ));
        tui.frame_requester().schedule_frame();
    }
}

#[cfg(test)]
#[path = "received_messages_tests.rs"]
mod tests;
