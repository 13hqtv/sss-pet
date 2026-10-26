//! AI-powered agent generation as a multi-turn conversation.
//!
//! Replaces the one-shot `agent_generate` command with a streaming,
//! conversation-based flow: the user picks a provider + model, describes
//! the agent they want, and the LLM generates an `<agent_draft>` JSON
//! block returned IN the conversation. The frontend then offers
//! 确认 / 继续修改 / 取消 buttons — only 确认 creates the agent (via
//! the existing `agent_create` command); 继续修改 sends another turn
//! (resuming the backend's native session); 取消 discards.
//!
//! State is held in memory (`GenState = Arc<Mutex<HashMap>>`) — no DB
//! pollution (the chat history sidebar never sees these turns). A
//! generation session is short-lived + interactive; if the app restarts
//! mid-generation the user simply restarts. The drain task buffers all
//! streamed messages into the in-memory history, so a hidden panel (no
//! event listener) doesn't lose data — `agent_gen_history` recovers the
//! full thread when the panel reopens.

use crate::agent::{self, BackendConfig, MessageKind};
use crate::agent::conversation::{run_turn, Conversation, StoredMessage};
use crate::config;
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, State};

/// The system prompt for the agent-builder conversation. Instructs the
/// LLM to emit an `<agent_draft>` JSON block (name / description /
/// system_prompt / provider / model) alongside its natural-language
/// explanation. Reused as the system prompt on every turn of the
/// conversation, so a follow-up modification ("把系统提示词改简洁些")
/// produces a REVISED draft in the same `<agent_draft>` shape.
pub(crate) const AGENT_BUILDER_PROMPT: &str = r#"你是一个智能体构建器。根据用户的描述，生成一个智能体配置。

你的回复必须包含一个 <agent_draft> JSON 块，格式如下：
<agent_draft>{"name":"","description":"","system_prompt":"","provider":"","model":""}</agent_draft>

字段说明：
- name: 简短、有辨识度的智能体名称（2-8个字）
- description: 一句话描述智能体的用途
- system_prompt: 详细的系统提示词，定义智能体的角色、能力、行为规范、输出格式等。要具体、可操作。
- provider: 必须是 "claude"、"codex" 或 "opencode" 之一
- model: 留空字符串 "" 表示使用 provider 的默认模型，或填写具体模型名

规则：
- JSON 必须是合法的紧凑格式（单行，不要换行）
- 字符串内的换行用 \n 转义
- 在 <agent_draft> 标签之外可以有自然语言说明
- 根据用户的需求描述，设计合适的系统提示词
- 当用户提出修改意见时，输出修订后的完整 <agent_draft>（不要只输出改动部分）"#;

/// One displayable message in a generation conversation. Mirrors the
/// streamed `agent::Message` shape PLUS a `role` (user / assistant /
/// system / tool) so the frontend can align + color bubbles without
/// inferring the role from the kind. Both the live `agent_gen_message`
/// events AND the `agent_gen_history` snapshot return this type, so the
/// frontend handles one shape for live + reloaded messages.
#[derive(Serialize, Clone, Debug)]
pub struct GenMessage {
    pub role: String,
    pub kind: String,
    pub content: String,
    pub tool: Option<String>,
}

/// In-memory state for one generation conversation.
pub struct GenSession {
    provider: String,
    model: Option<String>,
    cwd: PathBuf,
    /// The backend's native session id (stamped from the first turn's
    /// `ExecResult.session_id`). `None` until then → `run_turn` takes
    /// the replay path (history as a `Prior conversation:` block); once
    /// set, subsequent turns resume the native session (only the new
    /// prompt is sent).
    session_id: Option<String>,
    /// Full message history (user + assistant), in arrival order. Used
    /// both for the replay prompt (converted to `StoredMessage`) AND
    /// for the `agent_gen_history` snapshot the frontend reloads after
    /// a hide (recovering any messages the live listener missed).
    messages: Vec<GenMessage>,
    /// True while a turn's drain task is still running — guards against
    /// a second `agent_gen_continue` piling on top of an in-flight turn.
    streaming: bool,
}

/// Managed Tauri state: the map of active generation sessions keyed by
/// `gen_id` (a uuid v4 minted by `agent_gen_start`).
pub type GenState = Arc<Mutex<HashMap<String, GenSession>>>;

/// Map a streamed `MessageKind` to the coarse role bucket the frontend
/// uses for alignment + color. Mirrors `db::message_kind_to_role` (kept
/// private there) so chat + generation render consistently:
/// `Text`/`Thinking`/`ToolUse`/`Error` → assistant, `ToolResult` →
/// tool, `Status`/`Log` → system.
fn kind_to_role(kind: &MessageKind) -> &'static str {
    use MessageKind::*;
    match kind {
        Text | Thinking | ToolUse | Error => "assistant",
        ToolResult => "tool",
        Status | Log => "system",
    }
}

/// The `MessageKind` variant name (the serialized form), allocation-free
/// — mirrors `db::message_kind_to_name`.
fn kind_to_name(kind: &MessageKind) -> &'static str {
    use MessageKind::*;
    match kind {
        Text => "Text",
        Thinking => "Thinking",
        ToolUse => "ToolUse",
        ToolResult => "ToolResult",
        Status => "Status",
        Error => "Error",
        Log => "Log",
    }
}

/// Convert the in-memory display history to `StoredMessage` for
/// `run_turn`'s replay path (which takes `&[StoredMessage]`). Only
/// `role` + `content` + `kind` + `tool` matter for the replay prompt;
/// `created_at` is `None` (the backend's replay builder doesn't read
/// it).
fn to_stored(msgs: &[GenMessage]) -> Vec<StoredMessage> {
    msgs.iter()
        .map(|m| StoredMessage {
            role: m.role.clone(),
            content: m.content.clone(),
            kind: Some(m.kind.clone()),
            tool: m.tool.clone(),
            created_at: None,
        })
        .collect()
}

/// Spawn the stream-drain task for one generation turn. Mirrors
/// `chat::run_chat_turn`'s drain body but writes to the in-memory
/// `GenSession` instead of the DB, + emits `agent_gen_message` /
/// `agent_gen_result` events (keyed by `gen_id`) instead of `chat_*`.
/// Best-effort emit (a hidden panel = no listener = no-op); the message
/// is buffered into the in-memory history regardless, so
/// `agent_gen_history` recovers it when the panel reopens.
fn spawn_gen_drain(
    app: tauri::AppHandle,
    gen_state: GenState,
    gen_id: String,
    session: agent::Session,
) {
    let agent::Session {
        messages: mut messages_rx,
        result: result_rx,
    } = session;
    tokio::spawn(async move {
        // Drain streamed messages → buffer into the in-memory history +
        // emit (best-effort). Buffering FIRST means a hidden panel still
        // captures the full thread; the emit is the live-view bonus.
        while let Some(msg) = messages_rx.recv().await {
            let gm = GenMessage {
                role: kind_to_role(&msg.kind).to_string(),
                kind: kind_to_name(&msg.kind).to_string(),
                content: msg.content.clone(),
                tool: msg.tool.clone(),
            };
            if let Ok(mut map) = gen_state.lock() {
                if let Some(s) = map.get_mut(&gen_id) {
                    s.messages.push(gm.clone());
                }
            }
            let _ = app.emit_to(
                "panel",
                "agent_gen_message",
                json!({ "gen_id": gen_id, "message": gm }),
            );
        }
        // Await the final result. run_turn's result-forwarder ALWAYS
        // sends (it synthesizes a "failed" result if the backend dropped
        // its sender without sending), so this always resolves to `Ok`.
        let result = match result_rx.await {
            Ok(r) => r,
            Err(_) => agent::ExecResult {
                status: "failed".into(),
                output: String::new(),
                session_id: None,
                usage: HashMap::new(),
            },
        };
        // Stamp the native session_id (so the next turn resumes instead
        // of replaying) + clear the streaming flag so a follow-up
        // `agent_gen_continue` isn't rejected.
        if let Ok(mut map) = gen_state.lock() {
            if let Some(s) = map.get_mut(&gen_id) {
                if let Some(ref sid) = result.session_id {
                    s.session_id = Some(sid.clone());
                }
                s.streaming = false;
            }
        }
        let _ = app.emit_to(
            "panel",
            "agent_gen_result",
            json!({ "gen_id": gen_id, "result": result }),
        );
    });
}

// ===========================================================================
// Tauri commands (frontend-facing) — managed state: GenState
// ===========================================================================

/// Start a generation conversation: mint a `gen_id`, buffer the user's
/// description, run the first turn with `AGENT_BUILDER_PROMPT` as the
/// system prompt, spawn the drain task, return the `gen_id`. The
/// frontend subscribes to `agent_gen_message` / `agent_gen_result`
/// keyed by this id + renders the streamed draft. `provider` + `model`
/// are pinned at creation (the created agent uses them too on 确认).
///
/// Returns immediately after spawning the drain (the stream arrives via
/// events), mirroring `chat_send`'s non-blocking contract.
#[tauri::command]
pub async fn agent_gen_start(
    app: tauri::AppHandle,
    gen_state: State<'_, GenState>,
    provider: String,
    model: Option<String>,
    description: String,
) -> Result<String, String> {
    let desc = description.trim().to_string();
    if desc.is_empty() {
        return Err("描述不能为空".into());
    }
    if !agent::SUPPORTED.contains(&provider.as_str()) {
        return Err(format!("不支持的 provider: {provider}"));
    }
    let gen_id = uuid::Uuid::new_v4().to_string();
    let cwd = config::resolve_agent_cwd();
    // Insert the session with the user's description already buffered
    // (so `agent_gen_history` returns it immediately + the replay prompt
    // includes it on the next turn if no native session is established).
    {
        let mut map = gen_state.lock().map_err(|e| format!("gen lock: {e}"))?;
        map.insert(
            gen_id.clone(),
            GenSession {
                provider: provider.clone(),
                model: model.clone(),
                cwd: cwd.clone(),
                session_id: None,
                messages: vec![GenMessage {
                    role: "user".into(),
                    kind: "Text".into(),
                    content: desc.clone(),
                    tool: None,
                }],
                streaming: true,
            },
        );
    }
    // Build the backend for the chosen provider.
    let backend = agent::new(&provider, BackendConfig::default())
        .map_err(|e| format!("agent::new({provider}): {e}"))?;
    // Construct an in-memory Conversation (NOT persisted) so `run_turn`
    // can decide resume vs replay from `session_id` + thread `model`.
    let conv = Conversation {
        id: gen_id.clone(),
        provider: provider.clone(),
        session_id: None,
        title: "agent-gen".into(),
        created_at: chrono::Utc::now(),
        model: model.clone(),
        cwd: cwd.to_string_lossy().to_string(),
        agent_id: None,
    };
    // First turn: no prior history (the description IS the prompt). The
    // replay builder wraps it in a harmless "Prior conversation:\n\nNew
    // request:\n<desc>" — the system prompt carries the real intent.
    let history: Vec<StoredMessage> = Vec::new();
    let (session, _buffer) =
        run_turn(backend.as_ref(), &conv, &history, &desc, &cwd,
                 Some(AGENT_BUILDER_PROMPT.to_string()), None)
            .await
            .map_err(|e| format!("run_turn: {e}"))?;
    spawn_gen_drain(app, gen_state.inner().clone(), gen_id.clone(), session);
    Ok(gen_id)
}

/// Continue a generation conversation: append the user's modification
/// prompt, run another turn (resuming the backend's native session if
/// the first turn yielded a `session_id`, else replaying the in-memory
/// history), spawn the drain task. Returns immediately; the stream
/// arrives via `agent_gen_message` / `agent_gen_result`.
#[tauri::command]
pub async fn agent_gen_continue(
    app: tauri::AppHandle,
    gen_state: State<'_, GenState>,
    gen_id: String,
    prompt: String,
) -> Result<(), String> {
    let p = prompt.trim().to_string();
    if p.is_empty() {
        return Err("消息不能为空".into());
    }
    // Snapshot what `run_turn` needs + append the user message + flip
    // streaming on — all under one lock so a concurrent continue can't
    // race (a second call sees `streaming == true` + is rejected).
    let (conv, history, cwd, backend) = {
        let mut map = gen_state.lock().map_err(|e| format!("gen lock: {e}"))?;
        let s = map
            .get_mut(&gen_id)
            .ok_or_else(|| format!("gen session {gen_id} not found"))?;
        if s.streaming {
            return Err("正在生成中，请稍候".into());
        }
        s.messages.push(GenMessage {
            role: "user".into(),
            kind: "Text".into(),
            content: p.clone(),
            tool: None,
        });
        s.streaming = true;
        let conv = Conversation {
            id: gen_id.clone(),
            provider: s.provider.clone(),
            session_id: s.session_id.clone(),
            title: "agent-gen".into(),
            created_at: chrono::Utc::now(),
            model: s.model.clone(),
            cwd: s.cwd.to_string_lossy().to_string(),
            agent_id: None,
        };
        let history = to_stored(&s.messages);
        let cwd = s.cwd.clone();
        let backend = agent::new(&s.provider, BackendConfig::default())
            .map_err(|e| format!("agent::new: {e}"))?;
        (conv, history, cwd, backend)
    };
    let (session, _buffer) =
        run_turn(backend.as_ref(), &conv, &history, &p, &cwd,
                 Some(AGENT_BUILDER_PROMPT.to_string()), None)
            .await
            .map_err(|e| format!("run_turn: {e}"))?;
    spawn_gen_drain(app, gen_state.inner().clone(), gen_id, session);
    Ok(())
}

/// Reload the full in-memory message history for a generation session.
/// Used by the frontend when the panel reopens after a hide, to recover
/// any messages the live event listener missed while no listener was
/// attached (Review Focus #3 parity with chat's `load_history`). Returns
/// the messages in arrival order. Errors if the gen_id is unknown (the
/// session was cancelled / never started / lost to a restart).
#[tauri::command]
pub fn agent_gen_history(
    gen_state: State<'_, GenState>,
    gen_id: String,
) -> Result<Vec<GenMessage>, String> {
    let map = gen_state.lock().map_err(|e| format!("gen lock: {e}"))?;
    let s = map
        .get(&gen_id)
        .ok_or_else(|| format!("gen session {gen_id} not found"))?;
    Ok(s.messages.clone())
}

/// Cancel a generation conversation: drop the in-memory session. If a
/// turn is mid-stream the drain task keeps running to completion (it
/// holds its own `Arc` to the map; the removed entry just means its
/// buffer updates + emit become no-ops) — the backend subprocess
/// finishes naturally. No agent is created (the frontend never called
/// `agent_create`). Safe to call multiple times / on an unknown id.
#[tauri::command]
pub fn agent_gen_cancel(
    gen_state: State<'_, GenState>,
    gen_id: String,
) -> Result<(), String> {
    let mut map = gen_state.lock().map_err(|e| format!("gen lock: {e}"))?;
    map.remove(&gen_id);
    Ok(())
}
