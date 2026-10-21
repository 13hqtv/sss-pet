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
//! both a frontend-facing channel pair AND a persistent `Arc<Mutex<Vec<
//! Message>>>` buffer. If the frontend drops its receivers (panel
//! hidden mid-stream), the forwarder keeps draining the backend and
//! buffering — the full message stream + final result are always
//! captured for history. See `run_turn_survives_hide`.

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub role: String,
    pub content: String,
}

/// One persisted conversation thread. `id` is the desktop-pet's own
/// conversation id (a uuid or similar — Task 14 assigns it); `provider`
/// is the agent type (`"claude"` / `"opencode"` / `"codex"`); `session_id`
/// is the **agent-native** session/thread id captured from a prior
/// [`ExecResult::session_id`] — when `Some`, [`resume_or_replay`] resumes
/// the agent's own session; when `None`, it replays the history as context
/// (the agent has no native session to resume). `title` is the
/// user-facing label; `created_at` is when the conversation started.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub provider: String,
    pub session_id: Option<String>,
    pub title: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
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
                model: None,
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
                model: None,
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
/// [`Session`] AND a persistent `Arc<Mutex<Vec<Message>>>` buffer.
///
/// 1. `(opts, prompt) = resume_or_replay(conv, history, new_prompt)`; set
///    `opts.cwd = cwd`.
/// 2. `backend.execute(&prompt, opts)` → the backend's live [`Session`].
/// 3. Create a NEW frontend channel pair (`mpsc` for messages, `oneshot` for
///    result) + an `Arc<Mutex<Vec<Message>>>` buffer.
/// 4. Spawn a **message forwarder**: take `backend_session.messages`; for
///    each msg → push to the buffer AND `frontend_tx.send(msg).await` (send
///    errors = frontend dropped its receiver — IGNORE, keep buffering +
///    draining the backend). On the backend messages channel closing → drop
///    `frontend_tx` (closes the frontend receiver cleanly).
/// 5. Spawn a **result forwarder**: `backend_session.result.await` → push
///    the `ExecResult` (as a synthetic [`Message`] of kind `Status`) to the
///    buffer; `frontend_result_tx.send(exec_result)` (ignore send error).
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
) -> Result<(Session, Arc<Mutex<Vec<Message>>>), BackendError> {
    // 1. Decide resume vs replay; set the cwd to the caller's project dir.
    let (mut opts, prompt) = resume_or_replay(conv, history, new_prompt);
    opts.cwd = cwd.to_path_buf();

    // 2. Drive the backend with the combined (or raw) prompt + the opts.
    let backend_session = backend.execute(&prompt, opts).await?;

    // 3. Create the frontend-facing channel pair + the persistent buffer.
    let (frontend_tx, frontend_rx) = mpsc::channel::<Message>(64);
    let (frontend_result_tx, frontend_result_rx) = oneshot::channel::<ExecResult>();
    let buffer: Arc<Mutex<Vec<Message>>> = Arc::new(Mutex::new(Vec::new()));

    // 4. Message forwarder: drain backend messages → buffer + frontend.
    //    A send error (frontend dropped its receiver) is IGNORED — the
    //    forwarder keeps draining the backend into the buffer so the full
    //    message stream is captured for history even if the panel hid
    //    mid-stream (Review Focus #3).
    let buffer_msgs = Arc::clone(&buffer);
    let mut backend_rx = backend_session.messages;
    tokio::spawn(async move {
        while let Some(msg) = backend_rx.recv().await {
            if let Ok(mut buf) = buffer_msgs.lock() {
                buf.push(msg.clone());
            }
            // Best-effort forward; a dropped receiver does NOT stop the
            // drain (the buffer is the source of truth for history).
            let _ = frontend_tx.send(msg).await;
        }
        // Backend messages channel closed → drop frontend_tx so the
        // frontend's `messages` receiver resolves `None` cleanly.
        drop(frontend_tx);
    });

    // 5. Result forwarder: await the backend's final ExecResult → push a
    //    synthetic Status message to the buffer (so the history captures
    //    the terminal outcome) + forward to the frontend's result
    //    oneshot. Again, send errors are ignored (frontend may be gone).
    let buffer_res = Arc::clone(&buffer);
    let backend_result_rx = backend_session.result;
    tokio::spawn(async move {
        let exec_result = match backend_result_rx.await {
            Ok(r) => r,
            // The backend dropped its result sender without sending
            // (e.g. its task panicked). Synthesize a "failed" result so
            // the frontend's `result` always resolves — matches the
            // "caller's result always resolves" contract from the claude
            // backend's drain_stream.
            Err(_) => ExecResult {
                status: "failed".into(),
                output: String::new(),
                session_id: None,
                usage: std::collections::HashMap::new(),
            },
        };
        if let Ok(mut buf) = buffer_res.lock() {
            buf.push(Message {
                kind: crate::agent::MessageKind::Status,
                content: format!(
                    "session={} status={} output={}",
                    exec_result
                        .session_id
                        .as_deref()
                        .unwrap_or(""),
                    exec_result.status,
                    exec_result.output,
                ),
                tool: None,
                call_id: None,
            });
        }
        // Best-effort forward; a dropped receiver is fine — the buffer
        // still captured the result.
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
        }
    }

    fn conv_no_session() -> Conversation {
        Conversation {
            id: "conv-2".into(),
            provider: "claude".into(),
            session_id: None,
            title: "Test".into(),
            created_at: chrono::Utc::now(),
        }
    }

    fn history() -> Vec<StoredMessage> {
        vec![
            StoredMessage {
                role: "user".into(),
                content: "what is 2+2".into(),
            },
            StoredMessage {
                role: "assistant".into(),
                content: "4".into(),
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
    /// messages + the synthetic result message, despite the drop.
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

        // Give the forwarder tasks time to drain the backend into the
        // buffer. The backend's canned batch is tiny; a few short yields
        // suffice. Cap with a timeout so a broken forwarder fails fast
        // instead of hanging the test.
        for _ in 0..200 {
            tokio::task::yield_now().await;
        }
        // Belt-and-braces: also sleep a few ms so spawned tasks get
        // scheduled even under a cooperative scheduler.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let buf = buffer.lock().expect("buffer not poisoned");
        let text_count = buf
            .iter()
            .filter(|m| m.kind == MessageKind::Text)
            .count();
        assert_eq!(
            text_count,
            2,
            "buffer should contain ALL canned Text messages despite the frontend drop, got: {buf:?}"
        );
        assert!(
            buf.iter().any(|m| m.content == "hello"),
            "buffer missing first canned message, got: {buf:?}"
        );
        assert!(
            buf.iter().any(|m| m.content == "world"),
            "buffer missing second canned message, got: {buf:?}"
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
        let buf = buffer.lock().expect("buffer not poisoned");
        assert!(
            buf.iter().any(|m| m.content == "hello"),
            "buffer should also capture first canned message, got: {buf:?}"
        );
        assert!(
            buf.iter().any(|m| m.content == "world"),
            "buffer should also capture second canned message, got: {buf:?}"
        );
    }
}
