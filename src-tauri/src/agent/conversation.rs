//! Task 13: conversation model + session resume / history replay.
//!
//! Bridges [`crate::agent::Backend`] (the per-agent CLI runner from
//! Tasks 8/10/11/12) with the multi-conversation persistence layer
//! (Task 14 db + Task 15 chat UI). A [`Conversation`] is one
//! persisted chat thread for one provider; each turn is driven through
//! [`run_turn`], which decides between **resume** (the agent already
//! holds the conversation in its native session store — pass
//! `resume_session_id` and just the new turn) and **replay** (no native
//! session yet — embed the prior history as context followed by the new
//! prompt).
//!
//! Design ref: `docs/superpowers/specs/2026-10-21-desktop-pet-design.md`
//! ("Agent 接入" + "会话持久化"): multica-style `ResumeSessionID` mirror
//! — the agent-native session/thread id captured from a prior
//! [`crate::agent::ExecResult::session_id`] is the resume key; when
//! absent, the history is replayed as a context-prefixed prompt.
//!
//! **Review Focus #3 (survives-hide):** [`run_turn`] spawns forwarder
//! tasks that drain the backend's `messages` / `result` channels into
//! both a frontend-facing channel pair AND a persistent
//! `Arc<Mutex<TurnRecord>>` buffer (messages + final `ExecResult`). If
//! the frontend drops its receivers (panel hidden mid-stream), the
//! forwarder keeps draining the backend and buffering — the full message
//! stream + final result are always captured for history. See
//! `run_turn_survives_hide`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use super::{Backend, BackendError, ExecOptions, ExecResult, Message, Session};

// ---------------------------------------------------------------------------
// Persisted types — the on-disk shape Task 14 (db) + Task 15 (chat) consume.
// ---------------------------------------------------------------------------

/// One persisted message in a [`Conversation`]'s history. `role` is one of
/// `"user"` / `"assistant"` / `"system"` / `"tool"` (mirrors multica's
/// stored-message shape); `content` is the rendered text. This is the
/// **persisted** shape — distinct from the streamed [`Message`] (which
/// carries a [`crate::agent::MessageKind`] tag + tool metadata). The
/// controller preflight ruling places this type HERE (not in the db layer)
/// so Task 14 can `use crate::agent::conversation::StoredMessage` without
/// a circular dep.
///
/// **Task 1: `kind` field.** The [`crate::agent::MessageKind`] variant name
/// (e.g. `"Text"` / `"Thinking"` / `"ToolUse"` / `"ToolResult"` /
/// `"Status"` / `"Error"` / `"Log"`), set by [`db::save_turn_record`]
/// (Task 1) when persisting a streamed [`Message`]. `None` for rows
/// written before Task 1's migration (the db layer infers a `kind` from
/// `role` on load — see [`db::load_messages`]). Carrying `kind` on the
/// persisted shape lets history replay render thinking / tool blocks the
/// same way the live stream did (design "改动 3": reopen a chat and see
/// the processing, not just plain text).
///
/// **Task 6 fix-round-1: `tool` field.** The tool NAME (e.g. `"Bash"`)
/// carried on a streamed [`Message::tool`] — set by
/// [`db::save_turn_record`] when persisting a `ToolUse` / `ToolResult`
/// message. `None` for rows written before this fix (and for non-tool
/// kinds). Without it, history ToolUse chips read "使用工具 工具" (the
/// fallback doubled the label — self-referential + no tool identity); now
/// the reload path carries `tool` the same way the live path does, so a
/// history ToolUse chip reads "使用工具 Bash".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub role: String,
    pub content: String,
    pub kind: Option<String>,
    pub tool: Option<String>,
}

/// One persisted conversation thread. `id` is the desktop-pet's own
/// conversation id (a uuid or similar — Task 14 assigns it); `provider`
/// is the agent type (`"claude"` / `"opencode"` / `"codex"`); `session_id`
/// is the **agent-native** session/thread id captured from a prior
/// [`ExecResult::session_id`] — when `Some`, [`resume_or_replay`] resumes
/// the agent's own session; when `None`, it replays the history as context
/// (the agent has no native session to resume). `title` is the
/// user-facing label; `created_at` is when the conversation started.
///
/// **Task 1: `model` + `cwd` fields.** Both are set at conversation
/// creation (by [`db::new_conversation`]) and IMMUTABLE thereafter — the
/// design "改动 1" ruling is "一次对话 = agent + model + cwd 三件套，
/// 开对话时定、定了不改" (one conversation = agent + model + cwd triple,
/// fixed at creation, never changed mid-conversation). `model: None`
/// means "use the agent's own default model" (Task 4 wires `conv.model`
/// through `resume_or_replay` → `ExecOptions.model`; the three backends
/// already accept `opts.model` — Tasks 10/11/12). `cwd` is the working
/// directory the agent CLI is spawned in for this conversation (so file
/// tools resolve relative to the project the user picked when opening
/// the chat). The immutability naturally closes the v0.1.0 hazard of
/// swapping provider/model mid-conversation (which mismatched the
/// agent-native `session_id`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub provider: String,
    pub session_id: Option<String>,
    pub title: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// The model selected at creation (`None` = agent's default).
    /// Immutable. Task 1.
    pub model: Option<String>,
    /// The working directory the agent CLI runs in for this conversation.
    /// Immutable. Task 1.
    pub cwd: String,
}

/// One turn's captured record: the streamed [`Message`]s the backend
/// emitted, PLUS the final [`ExecResult`] — the structured outcome of
/// the turn. This is what [`run_turn`] buffers behind the
/// `Arc<Mutex<TurnRecord>>` it returns, and what Task 14 (db) persists
/// + Task 15 (chat) replays.
///
/// **Why a struct, not a `Vec<Message>`:** the prior shape stuffed the
/// `ExecResult` into the message buffer as a synthetic
/// `Message{kind: Status, content: "session=… status=… output=…"}`. That
/// was ambiguous — (a) it was indistinguishable from a real
/// backend-emitted `Status` message (same [`crate::agent::MessageKind`]),
/// and (b) the `format!("session={} status={} output={}")` delimiter
/// mis-splits if any value contains ` status=` / ` output=`. Downstream
/// (Task 14 db + Task 15 chat) couldn't tell the result from a real
/// status line and would mis-parse it. The fix-round-1 controller ruling
/// is to **separate** the two: messages stay messages, the result lives
/// in its own `result: Option<ExecResult>` field. The result-forwarder
/// STILL forwards the `ExecResult` to the frontend's `result` oneshot
/// (so the live [`Session::result`] resolves) — it just ALSO lands it in
/// `record.result` instead of fabricating a fake `Status` message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRecord {
    /// The streamed messages (text / thinking / tool-use / tool-result /
    /// status / error / log) the backend emitted during this turn, in
    /// arrival order. Source of truth for history — Task 14 persists this
    /// `Vec` as the turn's message stream.
    pub messages: Vec<Message>,
    /// The turn's terminal outcome — `None` while the turn is in flight,
    /// `Some(exec_result)` once the backend's `result` oneshot resolved
    /// (or the forwarder synthesized a `"failed"` result when the backend
    /// dropped its sender without sending). Separated from `messages` so
    /// history consumers can read the structured result without parsing a
    /// synthetic status string out of the message stream.
    pub result: Option<ExecResult>,
}

impl TurnRecord {
    /// Empty record (no messages, no result yet) — the initial state of
    /// the buffer [`run_turn`] creates.
    fn new() -> Self {
        Self {
            messages: Vec::new(),
            result: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Resume vs replay
// ---------------------------------------------------------------------------

/// Decide resume vs replay for a turn, returning the [`ExecOptions`] to pass
/// to [`Backend::execute`] AND the prompt string to feed it.
///
/// - **Resume** (`conv.session_id.is_some()`): the agent already holds the
///   conversation in its native session store (claude `--resume`, opencode
///   `--session`, codex `thread/resume` — wired in Tasks 10/11/12). Pass
///   `resume_session_id` in the opts; the prompt is just the new turn
///   (`new_prompt`) — the history is NOT replayed (the agent has it
///   natively).
///
/// - **Replay** (`conv.session_id.is_none()`): the agent has no native
///   session to resume (first turn, or the backend never surfaced a
///   `session_id`). Build a prompt that embeds the prior history as
///   context (`role: content` lines under a `Prior conversation:` header)
///   followed by the new prompt; `resume_session_id` is `None`.
///
/// Returns `(opts, prompt)`: the caller passes both to `execute` (Task 15
/// does `let (opts, prompt) = resume_or_replay(...); backend.execute(&prompt,
/// opts)`). Returning the prompt alongside the opts (rather than embedding it
/// in `ExecOptions`) keeps `ExecOptions` free of "is this prompt the raw turn
/// or a replayed context?" ambiguity — the prompt is always exactly what the
/// backend should send.
///
/// **Task 4: `opts.model = conv.model.clone()`.** The conversation's chosen
/// model (set at creation, immutable — design "改动 1") reaches the backend
/// here: both branches set `opts.model` from `conv.model` (was hardcoded
/// `None`). The three backends already thread `opts.model` (claude
/// `--model`, opencode `--model`, codex thread/start `model` — Tasks
/// 10/11/12), so once this sets it the model the user picked at creation
/// actually reaches the agent CLI. `None` (the default) means "use the
/// agent's own default model".
pub fn resume_or_replay(
    conv: &Conversation,
    history: &[StoredMessage],
    new_prompt: &str,
) -> (ExecOptions, String) {
    let cwd = std::path::PathBuf::from(".");
    match &conv.session_id {
        // Resume — the agent holds the conversation in its own session
        // store (claude `--resume` / opencode `--session` / codex
        // `thread/resume`). Pass the native id; the prompt is just the
        // new turn — the history is NOT replayed (the agent has it
        // natively).
        Some(id) => {
            let opts = ExecOptions {
                cwd,
                model: conv.model.clone(),
                resume_session_id: Some(id.clone()),
                timeout: None,
            };
            (opts, new_prompt.to_string())
        }
        // Replay — no native session. Embed the history as context
        // (`role: content` lines under a header) followed by the new
        // prompt. `resume_session_id` is `None`.
        None => {
            let prompt = build_replay_prompt(history, new_prompt);
            let opts = ExecOptions {
                cwd,
                model: conv.model.clone(),
                resume_session_id: None,
                timeout: None,
            };
            (opts, prompt)
        }
    }
}

/// Build the replay prompt: prior history as `role: content` lines under a
/// header, followed by the new prompt. Extracted as a pure helper so the
/// replay shape is testable in isolation (no `Conversation` needed).
fn build_replay_prompt(history: &[StoredMessage], new_prompt: &str) -> String {
    let mut out = String::new();
    out.push_str("Prior conversation:\n");
    for msg in history {
        out.push_str(&msg.role);
        out.push_str(": ");
        out.push_str(&msg.content);
        out.push('\n');
    }
    out.push_str("\nNew request:\n");
    out.push_str(new_prompt);
    out
}

// ---------------------------------------------------------------------------
// run_turn — drives one turn, buffering the stream for history.
// ---------------------------------------------------------------------------

/// Drive one turn of `conv` through `backend`, returning a frontend-facing
/// [`Session`] AND a persistent `Arc<Mutex<TurnRecord>>` buffer.
///
/// 1. `(opts, prompt) = resume_or_replay(conv, history, new_prompt)`; set
///    `opts.cwd = cwd`.
/// 2. `backend.execute(&prompt, opts)` → the backend's live [`Session`].
/// 3. Create a NEW frontend channel pair (`mpsc` for messages, `oneshot` for
///    result) + an `Arc<Mutex<TurnRecord>>` buffer (messages + result).
/// 4. Spawn a **message forwarder**: take `backend_session.messages`; for
///    each msg → push to `record.messages` AND `frontend_tx.send(msg).await`
///    (send errors = frontend dropped its receiver — IGNORE, keep buffering +
///    draining the backend). On the backend messages channel closing → drop
///    `frontend_tx` (closes the frontend receiver cleanly).
/// 5. Spawn a **result forwarder**: `backend_session.result.await` → set
///    `record.result = Some(exec_result)` (NOT a synthetic `Status` message
///    — the result is a structured field, distinguishable from real
///    backend-emitted status lines) + forward `exec_result` to the
///    frontend's result oneshot (ignore send error). If the backend
///    dropped its result sender without sending (e.g. its task panicked),
///    synthesize an `ExecResult{status:"failed", ..}` so the frontend's
///    `result` always resolves AND `record.result` is still set — matches
///    the "caller's result always resolves" contract from the claude
///    backend's `drain_stream`.
/// 6. Return `(Session { messages: frontend_rx, result: frontend_result_rx
///    }, buffer)`.
///
/// **Review Focus #3 (survives-hide):** the buffer is the persistent record.
/// Even if the frontend drops both its receivers (panel hidden mid-stream),
/// the forwarders keep draining the backend + buffering — the full message
/// stream + final result are captured for history. The frontend dropping
/// does NOT cancel the backend's turn.
pub async fn run_turn(
    backend: &dyn Backend,
    conv: &Conversation,
    history: &[StoredMessage],
    new_prompt: &str,
    cwd: &Path,
) -> Result<(Session, Arc<Mutex<TurnRecord>>), BackendError> {
    // 1. Decide resume vs replay; set the cwd to the caller's project dir.
    let (mut opts, prompt) = resume_or_replay(conv, history, new_prompt);
    opts.cwd = cwd.to_path_buf();

    // 2. Drive the backend with the combined (or raw) prompt + the opts.
    let backend_session = backend.execute(&prompt, opts).await?;

    // 3. Create the frontend-facing channel pair + the persistent buffer.
    let (frontend_tx, frontend_rx) = mpsc::channel::<Message>(64);
    let (frontend_result_tx, frontend_result_rx) = oneshot::channel::<ExecResult>();
    let buffer: Arc<Mutex<TurnRecord>> = Arc::new(Mutex::new(TurnRecord::new()));

    // 4. Message forwarder: drain backend messages → buffer.messages +
    //    frontend. A send error (frontend dropped its receiver) is IGNORED
    //    — the forwarder keeps draining the backend into the buffer so the
    //    full message stream is captured for history even if the panel hid
    //    mid-stream (Review Focus #3).
    let buffer_msgs = Arc::clone(&buffer);
    let mut backend_rx = backend_session.messages;
    tokio::spawn(async move {
        while let Some(msg) = backend_rx.recv().await {
            if let Ok(mut rec) = buffer_msgs.lock() {
                rec.messages.push(msg.clone());
            }
            // Best-effort forward; a dropped receiver does NOT stop the
            // drain (the buffer is the source of truth for history).
            let _ = frontend_tx.send(msg).await;
        }
        // Backend messages channel closed → drop frontend_tx so the
        // frontend's `messages` receiver resolves `None` cleanly.
        drop(frontend_tx);
    });

    // 5. Result forwarder: await the backend's final ExecResult → set
    //    `record.result = Some(exec_result)` (structured — NOT a synthetic
    //    Status message stuffed into the message buffer, which was
    //    ambiguous with real backend Status lines + delimiter-fragile) +
    //    forward to the frontend's result oneshot (so the live
    //    Session.result resolves). Again, send errors are ignored
    //    (frontend may be gone) — the buffer still captured the result.
    let buffer_res = Arc::clone(&buffer);
    let backend_result_rx = backend_session.result;
    tokio::spawn(async move {
        let exec_result = match backend_result_rx.await {
            Ok(r) => r,
            // The backend dropped its result sender without sending
            // (e.g. its task panicked). Synthesize a "failed" result so
            // the frontend's `result` always resolves AND `record.result`
            // is set — matches the "caller's result always resolves"
            // contract from the claude backend's drain_stream.
            Err(_) => ExecResult {
                status: "failed".into(),
                output: String::new(),
                session_id: None,
                usage: std::collections::HashMap::new(),
            },
        };
        if let Ok(mut rec) = buffer_res.lock() {
            rec.result = Some(exec_result.clone());
        }
        // Best-effort forward; a dropped receiver is fine — the buffer
        // still captured the result in `record.result`.
        let _ = frontend_result_tx.send(exec_result);
    });

    // 6. Hand the frontend its channel pair; keep the buffer handle so
    //    the caller (Task 15 chat) can drain it into history on turn
    //    completion.
    Ok((
        Session {
            messages: frontend_rx,
            result: frontend_result_rx,
        },
        buffer,
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{ExecResult, MessageKind};
    use std::collections::HashMap;
    use async_trait::async_trait;

    /// A fake backend that, on `execute`, sends a canned batch of messages
    /// + a canned `ExecResult` through fresh channels, then returns the
    /// `Session`. Deterministic — no real CLI. Used by the `run_turn`
    /// tests so they don't depend on claude / opencode / codex being
    /// installed.
    struct FakeBackend {
        canned: Vec<Message>,
        result: ExecResult,
    }

    #[async_trait]
    impl Backend for FakeBackend {
        async fn execute(
            &self,
            _prompt: &str,
            _opts: ExecOptions,
        ) -> Result<Session, BackendError> {
            let (messages_tx, messages_rx) = mpsc::channel::<Message>(64);
            let (result_tx, result_rx) = oneshot::channel::<ExecResult>();
            let canned = self.canned.clone();
            let result = self.result.clone();
            tokio::spawn(async move {
                for msg in canned {
                    // Best-effort send — if the receiver is gone, stop
                    // early (the real backends keep draining; the fake
                    // has nothing else to drain once canned is exhausted).
                    if messages_tx.send(msg).await.is_err() {
                        break;
                    }
                }
                drop(messages_tx);
                let _ = result_tx.send(result);
            });
            Ok(Session {
                messages: messages_rx,
                result: result_rx,
            })
        }
    }

    fn canned_messages() -> Vec<Message> {
        vec![
            Message {
                kind: MessageKind::Text,
                content: "hello".into(),
                tool: None,
                call_id: None,
            },
            Message {
                kind: MessageKind::Text,
                content: "world".into(),
                tool: None,
                call_id: None,
            },
            Message {
                kind: MessageKind::ToolUse,
                content: "ls".into(),
                tool: Some("Bash".into()),
                call_id: Some("c1".into()),
            },
        ]
    }

    fn canned_result() -> ExecResult {
        ExecResult {
            status: "completed".into(),
            output: "hello world".into(),
            session_id: Some("native-sess-42".into()),
            usage: HashMap::new(),
        }
    }

    fn fake_backend() -> FakeBackend {
        FakeBackend {
            canned: canned_messages(),
            result: canned_result(),
        }
    }

    fn conv_with_session() -> Conversation {
        Conversation {
            id: "conv-1".into(),
            provider: "claude".into(),
            session_id: Some("s1".into()),
            title: "Test".into(),
            created_at: chrono::Utc::now(),
            model: None,
            cwd: ".".into(),
        }
    }

    fn conv_no_session() -> Conversation {
        Conversation {
            id: "conv-2".into(),
            provider: "claude".into(),
            session_id: None,
            title: "Test".into(),
            created_at: chrono::Utc::now(),
            model: None,
            cwd: ".".into(),
        }
    }

    fn history() -> Vec<StoredMessage> {
        vec![
            StoredMessage {
                role: "user".into(),
                content: "what is 2+2".into(),
                kind: None,
                tool: None,
            },
            StoredMessage {
                role: "assistant".into(),
                content: "4".into(),
                kind: None,
                tool: None,
            },
        ]
    }

    // -- resume_or_replay --

    /// Resume path: `conv.session_id.is_some()` → opts carry
    /// `resume_session_id == Some("s1")` AND the prompt is exactly
    /// `new_prompt` (the history is NOT replayed).
    #[test]
    fn resume_with_session_id_does_not_replay() {
        let conv = conv_with_session();
        let hist = history();
        let new_prompt = "and 3+3?";
        let (opts, prompt) = resume_or_replay(&conv, &hist, new_prompt);
        assert_eq!(opts.resume_session_id.as_deref(), Some("s1"));
        assert_eq!(prompt, new_prompt, "resume prompt must be the new turn only");
        assert!(
            !prompt.contains("what is 2+2"),
            "history must NOT be replayed on resume, got: {prompt}"
        );
        assert!(
            !prompt.contains("assistant"),
            "history must NOT be replayed on resume, got: {prompt}"
        );
    }

    /// Replay path: `conv.session_id.is_none()` → opts
    /// `resume_session_id == None` AND the prompt CONTAINS the history
    /// message text + the new prompt.
    #[test]
    fn replay_when_no_session_id() {
        let conv = conv_no_session();
        let hist = history();
        let new_prompt = "continue";
        let (opts, prompt) = resume_or_replay(&conv, &hist, new_prompt);
        assert!(opts.resume_session_id.is_none());
        assert!(
            prompt.contains("what is 2+2"),
            "replay prompt must contain history text, got: {prompt}"
        );
        assert!(
            prompt.contains("4"),
            "replay prompt must contain history assistant text, got: {prompt}"
        );
        assert!(
            prompt.contains(new_prompt),
            "replay prompt must contain the new prompt, got: {prompt}"
        );
    }

    // -- run_turn: survives-hide (Review Focus #3) --

    /// The frontend drops its receivers immediately after `run_turn`
    /// returns (panel hidden). The forwarder must keep draining the
    /// backend and buffering — the buffer ends up with ALL the canned
    /// messages in `record.messages` AND the `ExecResult` in
    /// `record.result`, despite the drop.
    #[tokio::test]
    async fn run_turn_survives_hide() {
        let backend = fake_backend();
        let conv = conv_no_session();
        let hist = history();
        let cwd = std::env::current_dir().unwrap();

        let (session, buffer) = run_turn(
            &backend,
            &conv,
            &hist,
            "go",
            &cwd,
        )
        .await
        .expect("run_turn ok");

        // Simulate the frontend hiding: drop BOTH receivers.
        drop(session);

        // Poll the buffer's `result` field (set by the result-forwarder
        // once the backend's ExecResult arrives) up to 2s, ~10ms apart.
        // This is NOT timing-fragile — it waits exactly until the result
        // lands (or fails fast at the 2s cap). The prior `yield_now×200 +
        // sleep 50ms` was a fixed delay that could flake under a slow
        // scheduler; polling the actual completion signal is robust.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            {
                let rec = buffer.lock().expect("buffer not poisoned");
                if rec.result.is_some() {
                    break;
                }
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let rec = buffer.lock().expect("buffer not poisoned");
        // The result was captured despite the hide — the result-forwarder
        // set `record.result` even though both frontend receivers were
        // dropped before the result arrived (Review Focus #3).
        assert!(
            rec.result.is_some(),
            "result must land in TurnRecord despite the frontend drop, got: {:?}",
            rec.result
        );
        // The messages were captured despite the hide — exactly the 3
        // canned messages (2 Text + 1 ToolUse). No synthetic Status
        // message is pushed anymore (the result lives in `rec.result`),
        // so the count is exactly the canned batch size.
        assert_eq!(
            rec.messages.len(),
            3,
            "buffer should contain ALL 3 canned messages despite the frontend drop, got: {:?}",
            rec.messages
        );
        let text_count = rec
            .messages
            .iter()
            .filter(|m| m.kind == MessageKind::Text)
            .count();
        assert_eq!(
            text_count,
            2,
            "buffer should contain ALL canned Text messages despite the frontend drop, got: {:?}",
            rec.messages
        );
        assert!(
            rec.messages.iter().any(|m| m.content == "hello"),
            "buffer missing first canned message, got: {:?}",
            rec.messages
        );
        assert!(
            rec.messages.iter().any(|m| m.content == "world"),
            "buffer missing second canned message, got: {:?}",
            rec.messages
        );
    }

    // -- Review Focus #4: conv.model reaches ExecOptions.model --

    /// `resume_or_replay` must set `opts.model = conv.model.clone()` so the
    /// conversation's chosen model reaches the backend via
    /// `ExecOptions.model` (the three backends already thread `opts.model`
    /// — claude `--model`, opencode `--model`, codex thread/start `model`).
    /// With `conv.model == Some("sonnet")` the returned
    /// `ExecOptions.model == Some("sonnet")`; with `conv.model == None`
    /// it's `None` (agent's own default). Pinned on BOTH the resume path
    /// (session_id Some) AND the replay path (session_id None), since both
    /// build an `ExecOptions`.
    #[test]
    fn resume_or_replay_threads_conv_model_into_opts() {
        // Resume path — conv.model == Some("sonnet") → opts.model == Some("sonnet")
        let mut conv = conv_with_session();
        conv.model = Some("sonnet".into());
        let (opts, _prompt) = resume_or_replay(&conv, &history(), "go");
        assert_eq!(
            opts.model.as_deref(),
            Some("sonnet"),
            "resume path: conv.model must reach ExecOptions.model, got: {:?}",
            opts.model,
        );

        // Resume path — conv.model == None → opts.model == None
        let conv = conv_with_session(); // already model: None
        let (opts, _prompt) = resume_or_replay(&conv, &history(), "go");
        assert!(
            opts.model.is_none(),
            "resume path: conv.model == None must yield opts.model == None, got: {:?}",
            opts.model,
        );

        // Replay path — conv.model == Some("sonnet") → opts.model == Some("sonnet")
        let mut conv = conv_no_session();
        conv.model = Some("sonnet".into());
        let (opts, _prompt) = resume_or_replay(&conv, &history(), "go");
        assert_eq!(
            opts.model.as_deref(),
            Some("sonnet"),
            "replay path: conv.model must reach ExecOptions.model, got: {:?}",
            opts.model,
        );

        // Replay path — conv.model == None → opts.model == None
        let conv = conv_no_session(); // already model: None
        let (opts, _prompt) = resume_or_replay(&conv, &history(), "go");
        assert!(
            opts.model.is_none(),
            "replay path: conv.model == None must yield opts.model == None, got: {:?}",
            opts.model,
        );
    }

    // -- Review Focus #5: run_turn threads cwd into backend's opts.cwd --

    /// A `Backend` impl that captures the `ExecOptions` it received on
    /// `execute` (so the #5 test can assert the cwd `run_turn` was called
    /// with reached the backend's `opts.cwd`). Mirrors the canned-message
    /// `FakeBackend` above but stores the opts in an
    /// `Arc<Mutex<Option<ExecOptions>>>` instead of ignoring them. The
    /// capture is `std::sync::Mutex` (not tokio) — held only briefly inside
    /// `execute`, never across an `.await`.
    struct CapturingBackend {
        canned: Vec<Message>,
        result: ExecResult,
        seen_opts: Arc<Mutex<Option<ExecOptions>>>,
    }

    #[async_trait]
    impl Backend for CapturingBackend {
        async fn execute(
            &self,
            _prompt: &str,
            opts: ExecOptions,
        ) -> Result<Session, BackendError> {
            *self.seen_opts.lock().unwrap() = Some(opts);
            let (messages_tx, messages_rx) = mpsc::channel::<Message>(64);
            let (result_tx, result_rx) = oneshot::channel::<ExecResult>();
            let canned = self.canned.clone();
            let result = self.result.clone();
            tokio::spawn(async move {
                for msg in canned {
                    if messages_tx.send(msg).await.is_err() {
                        break;
                    }
                }
                drop(messages_tx);
                let _ = result_tx.send(result);
            });
            Ok(Session {
                messages: messages_rx,
                result: result_rx,
            })
        }
    }

    /// `run_turn` must pass its `cwd` param through to the backend's
    /// `opts.cwd` (the conversation's cwd is the source of truth — set at
    /// creation, immutable — and Task 4's `chat_send` passes `conv.cwd` as
    /// this param, removing the `config::resolve_agent_cwd()` call). The
    /// backend spawns the agent CLI in `opts.cwd`, so file tools resolve
    /// relative to the project the user picked. With a conv whose
    /// `cwd == "/tmp/proj"` + `run_turn` called with `cwd == "/tmp/proj"`,
    /// the `CapturingBackend`'s `execute` receives `opts.cwd == "/tmp/proj"`.
    #[tokio::test]
    async fn run_turn_threads_cwd_into_backend_opts() {
        let seen_opts: Arc<Mutex<Option<ExecOptions>>> = Arc::new(Mutex::new(None));
        let backend = CapturingBackend {
            canned: canned_messages(),
            result: canned_result(),
            seen_opts: Arc::clone(&seen_opts),
        };
        // The conv's cwd is the source of truth; chat_send (Task 4) passes
        // conv.cwd as run_turn's cwd param. Mirror that here.
        let mut conv = conv_no_session();
        conv.cwd = "/tmp/proj".into();
        let cwd = std::path::PathBuf::from("/tmp/proj");

        let (session, _buffer) = run_turn(
            &backend as &dyn Backend,
            &conv,
            &history(),
            "go",
            &cwd,
        )
        .await
        .expect("run_turn ok");

        // Drop the frontend session so the forwarders can finish + the
        // runtime doesn't leak. The CapturingBackend already recorded opts
        // synchronously inside execute (before the spawn), so the capture
        // is populated by this point regardless.
        drop(session);

        // Poll until the CapturingBackend's execute recorded the opts (or
        // 2s — not timing-fragile; execute runs before run_turn returns,
        // so this is effectively immediate, the poll is just defensive).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if seen_opts.lock().unwrap().is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("CapturingBackend never received opts within 2s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let opts = seen_opts
            .lock()
            .unwrap()
            .clone()
            .expect("opts captured by CapturingBackend.execute");
        assert_eq!(
            opts.cwd,
            std::path::PathBuf::from("/tmp/proj"),
            "run_turn must pass its cwd param through to opts.cwd, got: {:?}",
            opts.cwd,
        );
    }

    // -- run_turn: forwards when alive --

    /// When the frontend keeps its receivers, the canned messages arrive
    /// on `Session.messages` in order, and the final `ExecResult` arrives
    /// on `Session.result`. The buffer ALSO captures them (parallel).
    #[tokio::test]
    async fn run_turn_forwards_when_alive() {
        let backend = fake_backend();
        let conv = conv_no_session();
        let hist = history();
        let cwd = std::env::current_dir().unwrap();

        let (mut session, buffer) = run_turn(
            &backend,
            &conv,
            &hist,
            "go",
            &cwd,
        )
        .await
        .expect("run_turn ok");

        // Collect the streamed messages (with a timeout so a broken
        // forwarder fails fast instead of hanging).
        let mut got = Vec::new();
        let collect = async {
            while let Some(m) = session.messages.recv().await {
                got.push(m);
            }
        };
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), collect)
            .await;

        // The result MUST resolve (the forwarder forwards it).
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(2), session.result)
                .await
                .expect("result resolves within 2s")
                .expect("oneshot not dropped");

        assert_eq!(result.status, "completed");
        assert_eq!(result.session_id.as_deref(), Some("native-sess-42"));
        assert_eq!(result.output, "hello world");

        // The streamed messages include the canned Text + ToolUse.
        assert!(
            got.iter().any(|m| m.content == "hello"),
            "frontend should receive first canned message, got: {got:?}"
        );
        assert!(
            got.iter().any(|m| m.content == "world"),
            "frontend should receive second canned message, got: {got:?}"
        );
        assert!(
            got.iter().any(|m| m.kind == MessageKind::ToolUse),
            "frontend should receive the ToolUse message, got: {got:?}"
        );

        // The buffer mirrors the stream.
        {
            let rec = buffer.lock().expect("buffer not poisoned");
            assert!(
                rec.messages.iter().any(|m| m.content == "hello"),
                "buffer should also capture first canned message, got: {:?}",
                rec.messages
            );
            assert!(
                rec.messages.iter().any(|m| m.content == "world"),
                "buffer should also capture second canned message, got: {:?}",
                rec.messages
            );
        }

        // The result landed in the TurnRecord's `result` field (NOT just
        // the frontend channel) — the result-forwarder set `record.result`
        // when the backend's ExecResult arrived. This is the fix-round-1
        // assertion: the structured result is captured separately from the
        // message stream (no synthetic Status message to mis-parse).
        {
            let rec = buffer.lock().expect("buffer not poisoned");
            assert_eq!(
                rec.result,
                Some(result.clone()),
                "result must land in TurnRecord (not just the frontend channel), got: {:?}",
                rec.result
            );
        }
    }
}
