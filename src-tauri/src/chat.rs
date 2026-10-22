//! Task 15: chat-turn controller — wires the agent backend (Tasks 8–13)
//! to the multi-conversation db (Task 14) + the panel frontend (Task 15
//! `ChatView.svelte` via Tauri events). Extracts a testable
//! [`run_chat_turn`] core with an **injected emit + injected backend** so
//! the "panel hidden mid-stream" (Review Focus #3) is unit-testable with
//! a `FakeBackend` + a no-op/collecting emit (simulating no listener),
//! without needing an `AppHandle` or a real agent CLI.
//!
//! Design ref: `docs/superpowers/specs/2026-10-21-desktop-pet-design.md`
//! ("对话 tab"). One [`run_chat_turn`] call = one user prompt → one
//! backend turn → one streamed `chat_message` per backend message + one
//! terminal `chat_result` + a `save_turn_record` (persists messages +
//! the agent-native `session_id` so the NEXT turn resumes).
//!
//! **Final-review fix (save-before-emit + `chat_error`):** the drain
//! task persists via `save_turn_record` BEFORE emitting `chat_result`,
//! so the emitted result reflects persistence success. On a save
//! failure (disk full / FS error / poisoned db lock), it logs the
//! error + emits a dedicated `chat_error` event
//! `{"conv_id", "message":"保存对话失败: …"}` to the frontend (which
//! surfaces a warning banner + inline bubble), then STILL emits
//! `chat_result` (the turn completed in-memory; the streamed messages
//! were visible live). No failure is swallowed.
//!
//! **Review Focus #3 (survives-hide):** [`run_chat_turn`] spawns the
//! stream-drain task and returns immediately. The task drains the
//! `Session::messages` channel into `emit("chat_message", …)` calls —
//! but the emit is **best-effort**: a hidden panel = no listener = the
//! emit closure is a no-op (the message is ALREADY buffered in the
//! `TurnRecord` by `run_turn`'s forwarders, and `save_turn_record`
//! persists that buffer on completion regardless of whether the frontend
//! ever saw the live stream). The [`chat_survives_hide`] test pins this.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde_json::{json, Value};
use tauri::{Emitter, State};

use crate::agent::{self, conversation::StoredMessage, Backend, BackendConfig};
use crate::agent::conversation::{run_turn, Conversation, TurnRecord};
use crate::db;

// ===========================================================================
// run_chat_turn — the testable core (injected backend + injected emit)
// ===========================================================================

/// Drive one chat turn end-to-end. Extracted as a free function (not a
/// Tauri command) so it's unit-testable with a `FakeBackend` + a
/// no-op/collecting emit closure — no `AppHandle`, no real agent CLI
/// needed. The Tauri `chat_send` command wraps this with the real
/// `app.emit` as the emit closure + a real `agent::new(provider, cfg)`
/// backend (see [`chat_send`]).
///
/// # Arguments
/// - `backend`: injected `&dyn Backend`. The caller (`chat_send` or the
///   test) builds the backend (real or fake) and passes it in. This is
///   the seam for Review Focus #3 — the test passes a `FakeBackend`.
/// - `emit`: injected `Fn(&str, serde_json::Value)` closure. Called as
///   `emit("chat_message", {conv_id, message})` per streamed message +
///   `emit("chat_result", {conv_id, result})` on turn completion.
///   Best-effort — a no-op closure simulates a hidden panel (no
///   listener); the message is ALREADY buffered in the `TurnRecord` +
///   persisted on completion regardless.
/// - `db`: shared `Arc<Mutex<Connection>>` (the same managed state the
///   Tauri commands use — cloned into the spawn so the drain task can
///   call `save_turn_record` on completion).
/// - `conv_id`, `prompt`, `cwd`: the conversation to append to, the
///   user's new prompt, and the working dir for the agent CLI.
///
/// # Flow
/// 1. Lock db; load the [`Conversation`] + its prior history
///    (`load_messages`); append the user's prompt as a stored `"user"`
///    message NOW (so history shows the user's turn even if the agent
///    hasn't replied yet — `save_turn_record` later appends only the
///    assistant's messages, NOT the user prompt, so the user's turn
///    must be persisted separately + before the agent runs).
/// 2. `run_turn(backend, &conv, &history, prompt, cwd)` → a live
///    [`Session`](crate::agent::Session) (frontend-facing channel pair)
///    AND an `Arc<Mutex<TurnRecord>>` buffer (the persistent record —
///    even if the frontend drops its receivers mid-stream, the buffer
///    keeps capturing the full message stream + final result; Review
///    Focus #3).
/// 3. Spawn a task that drains `session.messages` →
///    `emit("chat_message", …)` (best-effort — no listener = no-op,
///    the message is ALREADY buffered), then awaits `session.result`
///    → `save_turn_record(&mut *db.lock(), conv_id, &turn_record)`
///    (persists messages + session_id) BEFORE `emit("chat_result", …)`
///    so the emitted result reflects persistence success. On save Err:
///    `eprintln!` + `emit("chat_error", {conv_id, "保存对话失败: …"})`,
///    then still `emit("chat_result", …)`.
/// 4. Return `Ok(())` immediately — the spawned task runs the stream.
///    The Tauri command's IPC call resolves; the frontend sees
///    `chat_message` / `chat_result` events as the turn progresses.
///
/// # Why `Arc<Mutex<Connection>>` not `&Mutex<Connection>`
/// The brief's literal signature had `db: &Mutex<Connection>`, but the
/// drain task needs to OWN a db handle (it outlives `run_chat_turn`'s
/// scope — the function returns immediately while the spawn keeps
/// running). `Arc<Mutex<Connection>>` is the standard wrapper for a
/// managed Tauri state shared with a spawned task. The `&mut *conn
/// .lock().unwrap()` pattern the brief calls out is used inside the
/// spawn to satisfy `save_turn_record`'s `&mut Connection` requirement.
pub async fn run_chat_turn(
    backend: &dyn Backend,
    emit: impl Fn(&str, Value) + Send + Sync + 'static,
    db: Arc<Mutex<Connection>>,
    conv_id: &str,
    prompt: &str,
    cwd: &Path,
) -> Result<(), String> {
    // 1. Load conv + history; append the user's prompt as a stored
    //    "user" message BEFORE the agent runs (so history shows the
    //    user's turn immediately + so save_turn_record, which only
    //    persists assistant messages, doesn't miss the user's turn).
    let (conv, _history) = {
        let conn = db.lock().map_err(|e| format!("db lock: {e}"))?;
        let conv = db::load_conversation(&conn, conv_id)
            .map_err(|e| format!("load_conversation: {e}"))?
            .ok_or_else(|| format!("conversation {conv_id} not found"))?;
        let history = db::load_messages(&conn, conv_id)
            .map_err(|e| format!("load_messages: {e}"))?;
        let user_msg = StoredMessage {
            role: "user".into(),
            content: prompt.into(),
            // The user's prompt is plain text — tag it `Text` so the
            // frontend renders a text bubble (NOT `ToolResult`, which
            // the NULL-kind backfill heuristic would infer for a `user`
            // role). New rows set `kind` explicitly to avoid the
            // heuristic; only pre-Task-1 NULL rows rely on it.
            kind: Some("Text".into()),
        };
        db::append_message(&conn, conv_id, &user_msg)
            .map_err(|e| format!("append user msg: {e}"))?;
        (conv, history)
    };

    // 2. Drive one turn — run_turn returns the live Session (frontend-
    //    facing channel pair) AND the Arc<Mutex<TurnRecord>> buffer
    //    (the persistent record). Even if the frontend drops its
    //    receivers, the buffer keeps capturing (Review Focus #3).
    let (session, turn_record) = run_turn(backend, &conv, &_history, prompt, cwd)
        .await
        .map_err(|e| format!("run_turn: {e}"))?;

    // 3. Spawn the stream-drain task. It owns the Session (drains
    //    messages + awaits result) + the db Arc (for save_turn_record
    //    on completion) + the turn_record Arc (reads the buffered
    //    messages + result for persistence). emit() is best-effort —
    //    a hidden panel = no listener = the closure is a no-op (the
    //    message is ALREADY buffered in turn_record by run_turn's
    //    forwarders + will be persisted by save_turn_record).
    let db_clone = Arc::clone(&db);
    let conv_id_owned = conv_id.to_string();
    tokio::spawn(async move {
        // Destructure the session so we can independently await
        // `messages` (drained in a loop) + `result` (awaited once
        // after the messages channel closes).
        let crate::agent::Session {
            messages: mut messages_rx,
            result: result_rx,
        } = session;

        // Drain messages → emit (best-effort). The run_turn forwarder
        // already buffered each message into turn_record.messages, so
        // even if emit() is a no-op (hidden panel), the message is
        // captured for history — the persistence call below will
        // save it.
        while let Some(msg) = messages_rx.recv().await {
            let payload = json!({
                "conv_id": conv_id_owned,
                "message": msg,
            });
            emit("chat_message", payload);
        }

        // Await the final result. run_turn's result-forwarder ALWAYS
        // sends (it synthesizes a "failed" result if the backend
        // dropped its sender without sending), so this `await` always
        // resolves to `Ok(r)` — the `Err` arm is defensive only.
        let result = match result_rx.await {
            Ok(r) => r,
            Err(_) => {
                // Should never happen (run_turn's forwarder always
                // sends). If it somehow does, the turn_record buffer
                // still has whatever the forwarders captured; we
                // persist that + return WITHOUT emitting `chat_result`
                // (there's no result to emit). Save errors are still
                // surfaced via `chat_error` here (the hole the
                // final-review fix closes — no failure is swallowed).
                let record = turn_record
                    .lock()
                    .map(|r| r.clone())
                    .unwrap_or_else(|_| TurnRecord {
                        messages: Vec::new(),
                        result: None,
                    });
                match db_clone.lock() {
                    Ok(mut conn) => {
                        if let Err(e) =
                            db::save_turn_record(&mut *conn, &conv_id_owned, &record)
                        {
                            eprintln!(
                                "save_turn_record failed (no-result arm, conv={conv_id_owned}): {e}"
                            );
                            emit(
                                "chat_error",
                                json!({
                                    "conv_id": conv_id_owned.clone(),
                                    "message": format!("保存对话失败: {e}"),
                                }),
                            );
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "db lock poisoned (no-result arm, conv={conv_id_owned}): {e} — cannot persist"
                        );
                        emit(
                            "chat_error",
                            json!({
                                "conv_id": conv_id_owned.clone(),
                                "message": "保存对话失败: 数据库锁获取失败",
                            }),
                        );
                    }
                }
                return;
            }
        };

        // Persist the turn (messages + session_id) BEFORE emitting
        // `chat_result`, so the emitted result reflects persistence
        // success: if the save fails, a `chat_error` event fires
        // BEFORE `chat_result` (the frontend learns the history won't
        // survive a restart by the time the result arrives). We still
        // emit `chat_result` afterward — the turn DID complete
        // in-memory + the streamed messages were visible live; the
        // user just needs to be told the save failed.
        //
        // Design choice (final-review fix): surface a save failure via
        // a dedicated `chat_error` event rather than (a) mutating the
        // `ExecResult`'s `status` to "completed_unsaved" or (b)
        // adding a `persisted: false` flag to the `chat_result`
        // payload. Rationale: the `ExecResult` is the agent layer's
        // domain type (shared with the backends + serialized via
        // serde); overloading its `status` with a persistence flag
        // conflates agent outcome with db outcome. A separate event
        // keeps the `chat_result` payload shape unchanged (no
        // `ChatResultEvent` TS change, no test churn) + gives the
        // frontend a single dedicated warning channel. The frontend's
        // `ChatView` subscribes `chat_error` + shows a banner + an
        // inline system bubble (see `stream.ts` / `ChatView.svelte`).
        let record = turn_record
            .lock()
            .map(|r| r.clone())
            .unwrap_or_else(|_| TurnRecord {
                messages: Vec::new(),
                result: None,
            });
        match db_clone.lock() {
            Ok(mut conn) => {
                // `save_turn_record` takes `&mut Connection` — obtain it
                // from the MutexGuard via `&mut *guard` (deref-mut
                // through the guard to the underlying Connection). The
                // brief calls out this exact pattern for the managed
                // `Arc<Mutex<Connection>>`.
                if let Err(e) = db::save_turn_record(&mut *conn, &conv_id_owned, &record) {
                    eprintln!(
                        "save_turn_record failed (conv={conv_id_owned}): {e}"
                    );
                    emit(
                        "chat_error",
                        json!({
                            "conv_id": conv_id_owned.clone(),
                            "message": format!("保存对话失败: {e}"),
                        }),
                    );
                }
            }
            Err(e) => {
                eprintln!(
                    "db lock poisoned (conv={conv_id_owned}): {e} — cannot persist"
                );
                emit(
                    "chat_error",
                    json!({
                        "conv_id": conv_id_owned.clone(),
                        "message": "保存对话失败: 数据库锁获取失败",
                    }),
                );
            }
        }

        // Emit the terminal result (best-effort — same hidden-panel
        // no-op semantics as chat_message). The save above already
        // fired `chat_error` if it failed, so by the time this fires
        // the frontend's state is accurate. Cloning the TurnRecord
        // above (Vec<Message> + Option<ExecResult>) is cheap relative
        // to a db transaction; we don't hold the db lock across the
        // turn_record lock (avoids any lock-ordering surprise with a
        // future code path that takes them the other way around).
        let payload = json!({
            "conv_id": conv_id_owned,
            "result": result,
        });
        emit("chat_result", payload);
    });

    // 4. Return immediately — the spawned task runs the stream.
    Ok(())
}

// ===========================================================================
// Tauri commands (frontend-facing) — managed state: Arc<Mutex<Connection>>
// ===========================================================================

/// List all persisted conversations for the history sidebar (oldest
/// first). Locks the managed `Arc<Mutex<Connection>>` for the read.
#[tauri::command]
pub fn list_conversations(
    db: State<'_, Arc<Mutex<Connection>>>,
) -> Result<Vec<Conversation>, String> {
    let conn = db.lock().map_err(|e| format!("db lock: {e}"))?;
    db::list_conversations(&conn).map_err(|e| format!("list_conversations: {e}"))
}

/// Create a new conversation row (`id=uuid v4`, `provider` from the
/// caller, `session_id=None`, `title="新对话"`, `created_at=now`),
/// persist it via `save_conversation`, return the new `Conversation`.
/// The frontend's "新建" button calls this after the user picks a
/// provider + model + cwd in the new-conversation dialog (Task 5).
///
/// Task 1: delegates to `db::new_conversation` so the uuid +
/// default-title + persist logic lives in one place (the db layer).
/// Task 5: the frontend now passes `model` (Option<String>, None =
/// "use the agent's own default model") + `cwd` (the working directory
/// the agent CLI runs in, pinned at creation — immutable). If `cwd` is
/// empty/whitespace, fall back to `config::resolve_agent_cwd()` (the
/// global config's `working_dir`, else the process's cwd) — the
/// new-conv dialog pre-fills cwd from `working_dir` so this fallback
/// only fires if the user blanked the field.
#[tauri::command]
pub fn new_conversation(
    db: State<'_, Arc<Mutex<Connection>>>,
    provider: String,
    model: Option<String>,
    cwd: String,
) -> Result<Conversation, String> {
    let cwd_str = if cwd.trim().is_empty() {
        crate::config::resolve_agent_cwd()
            .to_string_lossy()
            .to_string()
    } else {
        cwd
    };
    let conn = db.lock().map_err(|e| format!("db lock: {e}"))?;
    db::new_conversation(&conn, &provider, model, &cwd_str)
        .map_err(|e| format!("new_conversation: {e}"))
}

/// Load all stored messages for a conversation (arrival order). Called
/// by `ChatView` when the user selects a conversation in the sidebar —
/// renders the persisted history (prior turns' user prompts + assistant
/// replies + tool results).
#[tauri::command]
pub fn load_history(
    db: State<'_, Arc<Mutex<Connection>>>,
    conv_id: String,
) -> Result<Vec<StoredMessage>, String> {
    let conn = db.lock().map_err(|e| format!("db lock: {e}"))?;
    db::load_messages(&conn, &conv_id).map_err(|e| format!("load_messages: {e}"))
}

/// The core chat command. Builds a real backend via `agent::new(provider,
/// cfg)`, wraps the real `app.emit` as the injected emit closure, then
/// delegates to [`run_chat_turn`] which spawns the stream-drain task +
/// returns immediately. The frontend then receives `chat_message` +
/// `chat_result` events (dispatched by conv_id via `stream.ts`).
///
/// `cwd` is the working directory the agent CLI is spawned in (so file
/// tools resolve relative to the project). **Task 4: read it from
/// `conv.cwd`** (the conversation's own cwd — set at creation, immutable —
/// design "改动 1") by loading the conversation row, NOT from the global
/// `config::resolve_agent_cwd()`. The global config's `working_dir` is now
/// just the new-conversation default (Task 5 wires the UI to pre-fill it at
/// creation); once the conversation exists, its own `cwd` is the source of
/// truth. This closes the v0.1.0 hazard of the global working_dir changing
/// under a running conversation's feet.
///
/// **Task 5 carry: `provider` comes from `conv.provider`, NOT the
/// frontend.** The frontend's `chat_send` invoke no longer passes a
/// `provider` arg — the conversation's agent is fixed at creation (the
/// design's "定了不改" ruling). Loading the conversation row yields both
/// `cwd` AND `provider` here; we use `conv.provider` for `agent::new`.
/// This closes the v0.1.0 hazard of swapping provider mid-conversation
/// (which mismatched the agent-native `session_id`).
#[tauri::command]
pub async fn chat_send(
    app: tauri::AppHandle,
    db: State<'_, Arc<Mutex<Connection>>>,
    conv_id: String,
    prompt: String,
) -> Result<(), String> {
    // Load the conversation; use conv.cwd + conv.provider (source of
    // truth — set at creation, immutable) for run_turn + agent::new. NO
    // more config::resolve_agent_cwd() here — the conversation's own
    // cwd + provider are authoritative. (new_conversation still reads
    // config::resolve_agent_cwd() as the default cwd when CREATING a
    // conversation; this is fine — that's exactly the "new-conversation
    // default" role the global config plays after Task 4.)
    let (cwd, provider) = {
        let conn = db.lock().map_err(|e| format!("db lock: {e}"))?;
        let conv = db::load_conversation(&conn, &conv_id)
            .map_err(|e| format!("load_conversation: {e}"))?
            .ok_or_else(|| format!("conversation {conv_id} not found"))?;
        (std::path::PathBuf::from(conv.cwd), conv.provider)
    };
    let cfg = BackendConfig::default();
    let backend =
        agent::new(&provider, cfg).map_err(|e| format!("agent::new({provider}): {e}"))?;
    // The emit closure: wraps `app.emit(event, payload)`. Captures the
    // AppHandle by value (it's Clone + Send + Sync + 'static); the
    // closure is `Fn` (AppHandle::emit takes &self). Best-effort —
    // `emit` returns Err if there's no webview listener (panel hidden),
    // we swallow that (the message is buffered in the TurnRecord +
    // persisted regardless — Review Focus #3).
    let app_handle = app.clone();
    let emit = move |event: &str, payload: Value| {
        let _ = app_handle.emit(event, payload);
    };
    // The managed State<'_, Arc<Mutex<Connection>>> only lives as long
    // as the command call; clone the Arc out so the spawn (which
    // outlives this command's IPC call) keeps a valid handle.
    let db_arc = db.inner().clone();
    run_chat_turn(backend.as_ref(), emit, db_arc, &conv_id, &prompt, &cwd).await
}

// ===========================================================================
// Tests — run_chat_turn + chat_survives_hide (Review Focus #3)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::conversation::Conversation;
    use crate::agent::{Backend, BackendError, ExecOptions, ExecResult, Message, MessageKind, Session};
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::sync::{mpsc, oneshot};

    /// A fake backend that, on `execute`, sends a canned batch of
    /// messages + a canned `ExecResult` through fresh channels, then
    /// returns the `Session`. Deterministic — no real CLI. Used by
    /// `chat_survives_hide` so the test doesn't depend on
    /// claude / opencode / codex being installed. Mirrors the
    /// `FakeBackend` in `agent/conversation.rs`'s tests.
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

    /// Per-test unique conversation id (the data dir is per-test via
    /// `tempdir`, so collisions are unlikely — but being explicit avoids
    /// future flake if a test forgets its own tempdir).
    static CONV_SEQ: AtomicU64 = AtomicU64::new(0);

    fn unique_conv_id() -> String {
        let n = CONV_SEQ.fetch_add(1, Ordering::SeqCst);
        format!("conv-chat-{n}")
    }

    /// Open + migrate a fresh db in a writable temp dir, returning the
    /// `Arc<Mutex<Connection>>` the test passes to `run_chat_turn`,
    /// the tempdir (kept alive so the db file isn't cleaned up
    /// mid-test), AND the `db::TestOverrideGuard` (kept alive so the
    /// path-resolution override stays active for the test's duration —
    /// its `Drop` clears the override so the next test on this thread
    /// starts clean).
    fn fresh_db() -> (
        tempfile::TempDir,
        db::TestOverrideGuard,
        Arc<Mutex<Connection>>,
    ) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let g = db::set_test_overrides(Some(tmp.path().to_path_buf()), None, None);
        let mut conn = db::open().expect("open");
        db::migrate(&mut conn).expect("migrate");
        (tmp, g, Arc::new(Mutex::new(conn)))
    }

    /// **Review Focus #3 — chat survives hide.** Call `run_chat_turn`
    /// with a `FakeBackend` (canned messages + result) + a no-op emit
    /// closure (simulating a hidden panel — no listener). Drop / never
    /// consume the session's frontend channels (it's fine — `run_turn`
    /// buffers). Await `run_chat_turn`'s return (it returns immediately
    /// after spawning the drain task). Poll `load_messages(conv_id)`
    /// until the canned reply is persisted (the drain task completed +
    /// `save_turn_record` ran despite the "hide"). Assert the full
    /// canned reply is in history + the conversation's `session_id` was
    /// updated from `result.session_id` (so the next turn will resume).
    #[tokio::test]
    async fn chat_survives_hide() {
        let (_tmp, _g, db_state) = fresh_db();
        let conv = Conversation {
            id: unique_conv_id(),
            provider: "claude".into(),
            session_id: None, // first turn — no native session yet
            title: "Survives hide".into(),
            created_at: chrono::Utc::now(),
            model: None,
            cwd: ".".into(),
        };
        {
            let conn = db_state.lock().unwrap();
            db::save_conversation(&conn, &conv).expect("save_conversation");
        }

        let backend = FakeBackend {
            canned: vec![
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
            ],
            result: ExecResult {
                status: "completed".into(),
                output: "hello world".into(),
                session_id: Some("native-sess-42".into()),
                usage: HashMap::new(),
            },
        };
        let cwd = std::env::current_dir().unwrap();

        // No-op emit — simulates a hidden panel (no listener). The
        // closure captures nothing + does nothing; it's Send + Sync +
        // 'static trivially. The message stream is ALREADY buffered in
        // the TurnRecord by run_turn's forwarders + will be persisted
        // by save_turn_record on completion regardless of the emit.
        let emit = |_: &str, _: Value| {};

        // Run the turn — this kicks off the spawn + returns Ok(())
        // immediately. The drain task runs in the background on this
        // tokio runtime.
        run_chat_turn(
            &backend as &dyn Backend,
            emit,
            Arc::clone(&db_state),
            &conv.id,
            "go",
            &cwd,
        )
        .await
        .expect("run_chat_turn ok");

        // Poll load_messages until the canned reply appears (or 2s
        // timeout — not timing-fragile, waits exactly until the
        // persistence completes or fails fast at the cap).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            {
                let conn = db_state.lock().unwrap();
                let msgs = db::load_messages(&conn, &conv.id).unwrap();
                let assistant_text: Vec<_> = msgs
                    .iter()
                    .filter(|m| m.role == "assistant")
                    .collect();
                if assistant_text.iter().any(|m| m.content.contains("hello"))
                    && assistant_text.iter().any(|m| m.content.contains("world"))
                {
                    break;
                }
            }
            if std::time::Instant::now() >= deadline {
                let conn = db_state.lock().unwrap();
                let msgs = db::load_messages(&conn, &conv.id).unwrap();
                panic!(
                    "chat_survives_hide: canned reply not persisted within 2s, got: {:?}",
                    msgs
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        // Assert the FULL canned reply is in history + the user's
        // prompt was persisted BEFORE the agent ran (run_chat_turn
        // appends it as a "user" message in step 1).
        let conn = db_state.lock().unwrap();
        let msgs = db::load_messages(&conn, &conv.id).unwrap();
        assert!(
            msgs.iter().any(|m| m.role == "user" && m.content == "go"),
            "user prompt must be persisted before the agent runs, got: {:?}",
            msgs
        );
        assert!(
            msgs.iter()
                .any(|m| m.role == "assistant" && m.content.contains("hello")),
            "first canned assistant message must be persisted, got: {:?}",
            msgs
        );
        assert!(
            msgs.iter()
                .any(|m| m.role == "assistant" && m.content.contains("world")),
            "second canned assistant message must be persisted, got: {:?}",
            msgs
        );

        // The conversation's session_id was updated from
        // result.session_id — the next turn will resume the agent's
        // native session (mirrors multica's ResumeSessionID).
        let loaded = db::load_conversation(&conn, &conv.id)
            .unwrap()
            .expect("conversation still present");
        assert_eq!(
            loaded.session_id.as_deref(),
            Some("native-sess-42"),
            "session_id must be updated from result.session_id, got: {:?}",
            loaded.session_id
        );
    }

    /// `chat_send`-equivalent happy path WITH a collecting emit (so we
    /// can assert the emit was called with the right events). This
    /// pins the emit wiring (chat_message per message + chat_result
    /// once) independently of the survives-hide test, which uses a
    /// no-op emit.
    #[tokio::test]
    async fn run_chat_turn_emits_events_and_persists() {
        let (_tmp, _g, db_state) = fresh_db();
        let conv = Conversation {
            id: unique_conv_id(),
            provider: "claude".into(),
            session_id: None,
            title: "Emits".into(),
            created_at: chrono::Utc::now(),
            model: None,
            cwd: ".".into(),
        };
        {
            let conn = db_state.lock().unwrap();
            db::save_conversation(&conn, &conv).unwrap();
        }

        let backend = FakeBackend {
            canned: vec![Message {
                kind: MessageKind::Text,
                content: "ping".into(),
                tool: None,
                call_id: None,
            }],
            result: ExecResult {
                status: "completed".into(),
                output: "ping".into(),
                session_id: Some("sess-emit-1".into()),
                usage: HashMap::new(),
            },
        };
        let cwd = std::env::current_dir().unwrap();

        // Collecting emit — records every (event, conv_id) pair so we
        // can assert the wiring. The Mutex<Vec<...>> is Send + Sync +
        // 'static via Arc.
        let captured: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = Arc::clone(&captured);
        let conv_id_for_emit = conv.id.clone();
        let emit = move |event: &str, payload: Value| {
            let conv_id_in_payload = payload
                .get("conv_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let mut guard = captured_clone.lock().unwrap();
            guard.push((event.to_string(), conv_id_in_payload));
        };

        run_chat_turn(
            &backend as &dyn Backend,
            emit,
            Arc::clone(&db_state),
            &conv.id,
            "ping",
            &cwd,
        )
        .await
        .expect("run_chat_turn ok");

        // Poll until at least 2 events captured (1 chat_message + 1
        // chat_result) — the spawn completes the stream.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if captured.lock().unwrap().len() >= 2 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "emit did not capture >=2 events within 2s, got: {:?}",
                    captured.lock().unwrap()
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let events = captured.lock().unwrap().clone();
        assert!(
            events
                .iter()
                .any(|(e, c)| e.as_str() == "chat_message" && c.as_str() == conv_id_for_emit),
            "expected a chat_message event for conv_id, got: {:?}",
            events
        );
        assert!(
            events
                .iter()
                .any(|(e, c)| e.as_str() == "chat_result" && c.as_str() == conv_id_for_emit),
            "expected a chat_result event for conv_id, got: {:?}",
            events
        );

        // The message + session_id were persisted (same as survives-hide).
        let conn = db_state.lock().unwrap();
        let msgs = db::load_messages(&conn, &conv.id).unwrap();
        assert!(msgs.iter().any(|m| m.role == "assistant" && m.content == "ping"));
        let loaded = db::load_conversation(&conn, &conv.id).unwrap().unwrap();
        assert_eq!(loaded.session_id.as_deref(), Some("sess-emit-1"));
    }
}
