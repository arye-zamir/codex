## Cross-session messaging

Cross-session messaging lets the model in one live Codex session send a short message to
another live session served by the same app-server process. The sending model discovers
targets with `list_sessions` and delivers with `send_session_message`; the receiving session
gets the text as inter-agent input and, when idle, starts a turn to handle it.

The feature is opt-in and under development. It must be enabled in **both** sessions.

### Enable it

```toml
# ~/.codex/config.toml
[features]
cross_session_messaging = true
```

App-server clients can override the flag per thread through the `config` map of
`thread/start` (`"features.cross_session_messaging": false`).

### Which sessions can reach each other

| Requirement | Details |
| --- | --- |
| Same app-server process | Only threads of one app-server process see each other. A TUI attaches to an app-server daemon when one is listening on the default control socket under `CODEX_HOME`; a TUI running its embedded app-server reaches only its own threads. |
| Feature enabled on both sides | A target that has not enabled the feature is not listed and refuses messages. |
| Root, non-ephemeral sessions | Sub-agent threads spawned by another session and ephemeral sessions are neither listed nor addressable, and cannot send. |
| Live target | The target must be starting, running, idle, or in an error state; shut-down sessions are not live. |

### Tools

`list_sessions` takes no arguments and returns the eligible peers, sorted by thread id:

```json
{
  "scope": "live_root_sessions",
  "transport": "shared_app_server_process",
  "sessions": [
    { "thread_id": "<uuid>", "name": "api-worker", "cwd": "/work/api", "status": "idle" }
  ]
}
```

`status` is one of `starting`, `running`, `idle`, or `error`. The calling session is never
listed.

`send_session_message` takes `{ "thread_id": "<uuid>", "message": "<text>" }`. The message
must be non-empty and at most 8,000 approximate tokens (about 32 KB). Sending to the current
session, to an unknown thread, or to an ineligible target returns a model-visible error and
delivers nothing. On success the tool returns a receipt:

```json
{
  "state": "enqueued_for_target_session_dispatch",
  "sender_thread_id": "<uuid>",
  "recipient_thread_id": "<uuid>",
  "receiver_submission_id": "<id>",
  "processed_by_model": false,
  "detail": "The message was accepted and enqueued for target session dispatch; it has not necessarily been consumed by the target session or processed by its model."
}
```

The receipt means *enqueued*, not *read*. An answer, if any, arrives as a message from the
other session.

### Delivery

An idle target starts a new turn with the message. A busy target picks it up at its next
mailbox check; that may end the current model response early but never cancels a running
tool. The receiving model sees:

```text
Message Type: MESSAGE
Sender Session: <sender uuid>
Recipient Session: <recipient uuid>
Payload:
<message>
```

The message is inter-agent input, not user input: the receiving session's approval policy and
sandbox apply unchanged, and it is recorded in that session's conversation history like any
other input.

### Outbound budget

A session may send at most four messages that the target accepts per explicit user input; the
budget resets when the user submits new input. Refused sends do not count. When the budget is
spent the tool returns an error telling the model to wait for new user input.

### Viewing received messages in the TUI

Press `ctrl + q` to open the received-messages overlay for the current session. It shows the
newest messages first, retains the last 20 per session while that TUI session is open, and
says how many older messages were dropped. Rebind it with `tui.keymap.global.received_messages`
or through `/keymap` (Global → Received Messages).

### App-server clients

The receiving thread emits the experimental `sessionMessage/received` notification —
`{ threadId, turnId, senderThreadId, message }` — when a message is delivered, independently
of `thread/start.experimentalRawEvents`. It is not persisted or replayed. See the
[app-server README](../codex-rs/app-server/README.md) for the notification reference.
