//! Task 12: codex backend — spawns `codex app-server --listen stdio://` as a
//! long-lived JSON-RPC 2.0 server over stdio, performs the `initialize`
//! handshake, starts (or resumes) a thread, routes unsolicited events into
//! unified [`Message`]s, and resolves the [`ExecResult`] on `turn/completed`.
//!
//! This is the heaviest of the three backends (claude / opencode are
//! one-shot stdout drainers; codex is a long-lived RPC server). Unlike the
//! drain-style backends, the testable surface is the **pure** event
//! dispatcher [`handle_event`] + the **pure** request builders
//! ([`build_initialize_request`] / [`build_thread_start_request`] /
//! [`build_thread_resume_request`]) + [`extract_session_id`]. The transport
//! ([`CodexBackend::execute`]) composes those pure functions over a real
//! `codex app-server` subprocess.
//!
//! **Protocol (sourced from multica `server/pkg/agent/codex.go`, the
//! AUTHORITATIVE reference):**
//! - JSON-RPC 2.0, one object per newline-delimited stdout line.
//! - Requests: `{"jsonrpc":"2.0","id":<n>,"method":"<m>","params":{...}}\n`.
//! - Responses: `{"jsonrpc":"2.0","id":<n>,"result":{...}}\n` (or `error`).
//! - Notifications (server→client, no `id`): `{"jsonrpc":"2.0","method":<m>,
//!   "params":{...}}\n` — routed through [`handle_event`].
//!
//! Lifecycle (per multica codex.go:1494-1927):
//! 1. `initialize` (id=1) — handshake; `clientInfo` + `capabilities`. On
//!    timeout → [`BackendError::Handshake`] (mirrors multica's
//!    `codexHandshakeTimeoutError`, 30s default).
//! 2. `thread/start` (id=2) — create a thread; response `result.thread.id`
//!    is the session id (extracted via [`extract_session_id`], mirroring
//!    multica's `extractThreadID` codex.go:4323). `thread/resume` (id=2)
//!    replaces this when `opts.resume_session_id` is set.
//! 3. `turn/start` (id=3) — submit the prompt as `input` text content;
//!    response acknowledges the turn was accepted.
//! 4. Read events → [`handle_event`]: `turn/started`→Nothing,
//!    `item/agentMessage/delta`→Text, `item/started`/`item/completed` over
//!    `commandExecution` / `fileChange` / `mcpToolCall`→ToolUse/ToolResult,
//!    `turn/completed`→Result(ExecResult), top-level `error`→Result(failed).
//!
//! Design ref: `docs/superpowers/specs/2026-10-21-desktop-pet-design.md`
//! ("Agent 接入"). The wire shapes (method names, param keys, event
//! vocabulary) are verified against multica codex.go, not invented.

use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{timeout, Duration};

use super::exec;
use super::{
    Backend, BackendError, CodexBackend, ExecOptions, ExecResult, Message, MessageKind, Session,
};

// `BackendConfig` is only named in the test module (the lib build accesses
// `cfg` via field access, never naming the type). Test-only import so the
// non-test lib build does not warn about an unused import.
#[cfg(test)]
use super::BackendConfig;

/// How long to wait for the `initialize` (and thread setup) RPC response
/// before failing with [`BackendError::Handshake`]. Mirrors multica's
/// `defaultCodexHandshakeTimeout = 30 * time.Second` (codex.go:62) and its
/// `codexHandshakeTimeoutError` (codex.go:2576-2587).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Cap on the buffered stdout line length when reading JSON-RPC frames.
/// Codex's `thread/resume` can serialize an entire conversation into one
/// response line; multica uses a 1 MiB scanner buffer for the same reason
/// (codex.go:4107 / `agentStreamMaxLineBytes`). 1 MiB matches that.
const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Outcome of dispatching one codex JSON-RPC notification through
/// [`handle_event`]. The read loop in [`CodexBackend::execute`] uses this to
/// decide whether a line produces a streaming [`Message`], a terminal
/// [`ExecResult`], or nothing worth forwarding.
#[derive(Debug, Clone)]
pub(crate) enum EventOutcome {
    /// Forward this message to the `messages` channel (text / tool_use /
    /// tool_result / status / error-as-message).
    Message(Message),
    /// Terminal: the turn ended. Send to the `result` channel and stop the
    /// read loop. `turn/completed` (any status) and unrecoverable top-level
    /// `error` notifications land here.
    Result(ExecResult),
    /// The event is not user-visible (e.g. `turn/started`, `thread/status/
    /// changed`, `thread/tokenUsage/updated`, unrecognised methods). The
    /// read loop silently advances.
    Nothing,
}

/// Dispatch one codex JSON-RPC notification (already parsed as a
/// [`Value`]) into an [`EventOutcome`]. Pure — no I/O, no subprocess. The
/// transport's read loop calls this for every line that is NOT a response
/// to a pending request (responses carry an `id` matching a pending
/// request id; everything else is a notification routed here).
///
/// Mapping (per multica codex.go `handleRawNotification`
/// codex.go:3465-3570 + `handleItemNotification` codex.go:3707-3818):
///
/// - `turn/started` → `Nothing`. (multica emits a `MessageStatus:"running"`
///   here; the desktop-pet surfaces status via the ExecResult lifecycle, so
///   we swallow it to keep the transcript clean. A future tightening can
///   surface it as `Message{Status,"running"}`.)
/// - `turn/completed` → `Result(ExecResult)`. `status` from
///   `params.turn.status` (completed / cancelled / failed / interrupted);
///   `session_id` from top-level `params.threadId`; `output` from the last
///   seen agent-message text (tracked by the transport, threaded in via
///   the caller — here we leave `output` empty and let the transport
///   substitute its tracked last text; for a failed turn, `output` carries
///   `params.turn.error.message` so the caller sees a real reason). Status
///   `cancelled`/`canceled`/`aborted`/`interrupted` → `"aborted"`;
///   `failed` → `"failed"`; anything else → `"completed"`.
/// - `item/agentMessage/delta` (flat shape: `{itemId, delta, threadId}`) →
///   `Message{Text, delta}`. Empty delta → `Nothing`.
/// - `item/completed` with `item.type=="agentMessage"` → `Message{Text,
///   item.text}` (the authoritative completed snapshot; empty text →
///   `Nothing`).
/// - `item/started` with `item.type=="commandExecution"` →
///   `Message{ToolUse, tool="exec_command", call_id=item.id,
///   content=item.command}`.
/// - `item/completed` with `item.type=="commandExecution"` →
///   `Message{ToolResult, tool="exec_command", call_id=item.id,
///   content=item.aggregatedOutput}`.
/// - `item/started` with `item.type=="fileChange"` →
///   `Message{ToolUse, tool="patch_apply", call_id=item.id}`.
/// - `item/completed` with `item.type=="fileChange"` →
///   `Message{ToolResult, tool="patch_apply", call_id=item.id}`.
/// - `item/started` with `item.type=="mcpToolCall"` →
///   `Message{ToolUse, tool=<item.tool or "mcp_tool">, call_id=item.id}`.
/// - `item/completed` with `item.type=="mcpToolCall"` →
///   `Message{ToolResult, tool=<...>, call_id=item.id}`.
/// - top-level `error` notification → `Result(ExecResult{status:"failed",
///   output: error.message})`. multica captures terminal errors via
///   `setTurnError`; we surface them as a failed ExecResult so the caller
///   gets a real reason. Retriable errors (`willRetry:true`) → `Nothing`
///   (transient, the turn continues).
/// - anything else (incl. `thread/status/changed`,
///   `thread/tokenUsage/updated`, `item/autoApprovalReview/*`, legacy
///   `codex/event`) → `Nothing`.
///
/// `handle_event` only inspects the notification's `method` + `params`; it
/// does NOT distinguish the legacy `codex/event` envelope (multica
/// codex.go:3007-3019 unwraps `params.msg` first). The desktop-pet targets
/// the raw v2 protocol (codex 0.154.0), so legacy wrapping is not
/// exercised; a future tightening can add the unwrap.
pub(crate) fn handle_event(value: &Value) -> EventOutcome {
    let obj = match value.as_object() {
        Some(o) => o,
        None => return EventOutcome::Nothing,
    };
    let method = match obj.get("method").and_then(|v| v.as_str()) {
        Some(m) => m,
        None => return EventOutcome::Nothing,
    };
    let params = obj.get("params").unwrap_or(&Value::Null);
    match method {
        "turn/started" => EventOutcome::Nothing,
        "turn/completed" => handle_turn_completed(params),
        "item/agentMessage/delta" => handle_agent_message_delta(params),
        "item/started" => handle_item_started(params),
        "item/completed" => handle_item_completed(params),
        "error" => handle_error_notification(params),
        // thread/status/changed, thread/tokenUsage/updated,
        // item/autoApprovalReview/*, legacy codex/event, etc. — not
        // user-visible in the desktop-pet transcript.
        _ => EventOutcome::Nothing,
    }
}

/// `turn/completed` → `Result(ExecResult)`. Mirrors multica codex.go:3491-3526.
/// `status` from `params.turn.status`; `session_id` from top-level
/// `params.threadId`; `output` from `params.turn.error.message` for a failed
/// turn (empty for completed/aborted — the transport substitutes the
/// tracked last agent text for the completed case).
fn handle_turn_completed(params: &Value) -> EventOutcome {
    let thread_id = params
        .get("threadId")
        .and_then(|v| v.as_str())
        .map(String::from);
    let turn = params.get("turn").unwrap_or(&Value::Null);
    let status = turn
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("completed");
    let (mapped_status, output) = match status {
        "failed" => {
            let err_msg = turn
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("codex turn failed");
            ("failed", err_msg.to_string())
        }
        "cancelled" | "canceled" | "aborted" | "interrupted" => {
            ("aborted", String::new())
        }
        _ => ("completed", String::new()),
    };
    EventOutcome::Result(ExecResult {
        status: mapped_status.into(),
        output,
        session_id: thread_id,
        usage: HashMap::new(),
    })
}

/// `item/agentMessage/delta` (flat v2 shape: `{itemId, delta, threadId}`) →
/// `Message{Text, delta}`. Empty delta → `Nothing`. multica codex.go:3725-3730.
fn handle_agent_message_delta(params: &Value) -> EventOutcome {
    let delta = params.get("delta").and_then(|v| v.as_str()).unwrap_or("");
    if delta.is_empty() {
        return EventOutcome::Nothing;
    }
    EventOutcome::Message(Message {
        kind: MessageKind::Text,
        content: delta.to_string(),
        tool: None,
        call_id: None,
    })
}

/// `item/started` (v2 raw notification). Routes by `item.type`:
/// `commandExecution`→ToolUse{exec_command}, `fileChange`→ToolUse{patch_apply},
/// `mcpToolCall`→ToolUse{<tool>}. multica codex.go:3732-3797.
fn handle_item_started(params: &Value) -> EventOutcome {
    let item = match params.get("item") {
        Some(i) => i,
        None => return EventOutcome::Nothing,
    };
    let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let call_id = if item_id.is_empty() {
        None
    } else {
        Some(item_id.to_string())
    };
    match item_type {
        "commandExecution" => {
            let command = item
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            EventOutcome::Message(Message {
                kind: MessageKind::ToolUse,
                content: command,
                tool: Some("exec_command".into()),
                call_id,
            })
        }
        "fileChange" => EventOutcome::Message(Message {
            kind: MessageKind::ToolUse,
            content: String::new(),
            tool: Some("patch_apply".into()),
            call_id,
        }),
        "mcpToolCall" => {
            let tool = item
                .get("tool")
                .and_then(|v| v.as_str())
                .unwrap_or("mcp_tool")
                .to_string();
            EventOutcome::Message(Message {
                kind: MessageKind::ToolUse,
                content: String::new(),
                tool: Some(tool),
                call_id,
            })
        }
        _ => EventOutcome::Nothing,
    }
}

/// `item/completed` (v2 raw notification). Routes by `item.type`:
/// `agentMessage`→Text{item.text}, `commandExecution`→ToolResult{aggregatedOutput},
/// `fileChange`→ToolResult{patch_apply}, `mcpToolCall`→ToolResult{<tool>}.
/// multica codex.go:3743-3817.
fn handle_item_completed(params: &Value) -> EventOutcome {
    let item = match params.get("item") {
        Some(i) => i,
        None => return EventOutcome::Nothing,
    };
    let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let call_id = if item_id.is_empty() {
        None
    } else {
        Some(item_id.to_string())
    };
    match item_type {
        "agentMessage" => {
            let text = item.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if text.is_empty() {
                return EventOutcome::Nothing;
            }
            EventOutcome::Message(Message {
                kind: MessageKind::Text,
                content: text.to_string(),
                tool: None,
                call_id: None,
            })
        }
        "commandExecution" => {
            let output = item
                .get("aggregatedOutput")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            EventOutcome::Message(Message {
                kind: MessageKind::ToolResult,
                content: output,
                tool: Some("exec_command".into()),
                call_id,
            })
        }
        "fileChange" => EventOutcome::Message(Message {
            kind: MessageKind::ToolResult,
            content: String::new(),
            tool: Some("patch_apply".into()),
            call_id,
        }),
        "mcpToolCall" => {
            let tool = item
                .get("tool")
                .and_then(|v| v.as_str())
                .unwrap_or("mcp_tool")
                .to_string();
            EventOutcome::Message(Message {
                kind: MessageKind::ToolResult,
                content: String::new(),
                tool: Some(tool),
                call_id,
            })
        }
        _ => EventOutcome::Nothing,
    }
}

/// Top-level `error` notification. `willRetry:true`→Nothing (transient);
/// otherwise → `Result(ExecResult{status:"failed", output:<msg>})`.
/// multica codex.go:3528-3549.
fn handle_error_notification(params: &Value) -> EventOutcome {
    let will_retry = params
        .get("willRetry")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if will_retry {
        return EventOutcome::Nothing;
    }
    let msg = params
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|v| v.as_str())
        .or_else(|| params.get("message").and_then(|v| v.as_str()))
        .unwrap_or("codex error");
    let session_id = params
        .get("threadId")
        .and_then(|v| v.as_str())
        .map(String::from);
    EventOutcome::Result(ExecResult {
        status: "failed".into(),
        output: msg.to_string(),
        session_id,
        usage: HashMap::new(),
    })
}

/// Build the `initialize` JSON-RPC request (id=`id`). Mirrors multica
/// codex.go:1497-1506:
/// `{"jsonrpc":"2.0","id":<id>,"method":"initialize","params":{"clientInfo":{"name":"sss-pet","version":"0.1.0"},"capabilities":{"experimentalApi":true}}}`.
///
/// `clientInfo.name`/`version` identify this client to the app-server (any
/// string works; multica uses "multica-agent-sdk" / "0.2.0"). The
/// `experimentalApi` capability is required for the raw v2 event protocol
/// (turn/started, item/*, turn/completed) — without it codex may emit only
/// the legacy `codex/event` envelope.
pub(crate) fn build_initialize_request(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "clientInfo": {
                "name": "sss-pet",
                "version": env!("CARGO_PKG_VERSION")
            },
            "capabilities": {
                "experimentalApi": true
            }
        }
    })
}

/// Translate the stored MCP config blob into the shape codex's app-server
/// expects on the `thread/start` / `thread/resume` `config` field.
///
/// The UI stores Claude-style `{"mcpServers": {...}}` (camelCase). Codex's
/// `Config` struct deserialises MCP servers from the `mcp_servers` field
/// (snake_case — matching `~/.codex/config.toml`'s `[mcp_servers.<name>]`
/// tables), so a camelCase `mcpServers` key is silently dropped and no
/// server is registered. Only the wrapper key differs; the per-server entry
/// shape (`command`/`args`/`env` for stdio, `url`/`headers` for http) is
/// identical between the two. Sibling keys (e.g. `version`) are preserved.
///
/// Returns `Value::Null` when `raw` is `None` or unparseable — the
/// `config` field is `null` and the app-server falls back to its default.
fn translate_mcp_for_codex(raw: Option<&str>) -> Value {
    let Some(raw) = raw else { return Value::Null };
    let Ok(mut v) = serde_json::from_str::<Value>(raw) else {
        return Value::Null;
    };
    if let Value::Object(ref mut map) = v {
        if let Some(servers) = map.remove("mcpServers") {
            map.insert("mcp_servers".to_string(), servers);
        }
    }
    v
}

/// Build the `thread/start` JSON-RPC request (id=`id`). Per the task spec
/// the request carries the prompt (as `input` text content) alongside the
/// `cwd` and optional `model`. Mirrors multica codex.go:2069-2083 for the
/// thread/start param vocabulary, and codex.go:1617-1620 / 1988-1994 for
/// the `input` content shape (`[{"type":"text","text":"<prompt>"}]`).
///
/// In the real codex protocol `thread/start` creates the thread and a
/// SEPARATE `turn/start` submits the prompt; multica does exactly that
/// (codex.go:2097 then codex.go:1672). The desktop-pet's pure function
/// folds the prompt into the thread/start params per the task spec; the
/// transport additionally sends a `turn/start` after the thread/start
/// response so a real app-server actually begins the turn (see
/// [`CodexBackend::execute`]).
///
/// Params: `cwd`, `model` (null when `None`), `input`
/// (`[{"type":"text","text":"<prompt>"}]`), plus the multica-style null
/// placeholders (`modelProvider`, `profile`, `approvalPolicy`, `sandbox`,
/// `config`, `baseInstructions`, `developerInstructions`, `compactPrompt`,
/// `includeApplyPatchTool`, `experimentalRawEvents`, `persistExtendedHistory`)
/// so the wire shape matches multica's and a real app-server sees a
/// well-formed request.
pub(crate) fn build_thread_start_request(
    id: u64,
    prompt: &str,
    cwd: &str,
    model: Option<&str>,
    system_prompt: Option<&str>,
    mcp_config: Option<&str>,
) -> Value {
    let model_val = model
        .map(String::from)
        .map(Value::String)
        .unwrap_or(Value::Null);
    let base_instructions = system_prompt.map(|s| Value::String(s.to_string())).unwrap_or(Value::Null);
    let dev_instructions = system_prompt.map(|s| Value::String(s.to_string())).unwrap_or(Value::Null);
    // codex's Config key is `mcp_servers` (snake_case) — translate the
    // stored camelCase `mcpServers` wrapper so servers are registered.
    let config_val = translate_mcp_for_codex(mcp_config);
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "thread/start",
        "params": {
            "model": model_val,
            "modelProvider": null,
            "profile": null,
            "cwd": cwd,
            "approvalPolicy": null,
            "sandbox": null,
            "config": config_val,
            "baseInstructions": base_instructions,
            "developerInstructions": dev_instructions,
            "compactPrompt": null,
            "includeApplyPatchTool": null,
            "experimentalRawEvents": false,
            "persistExtendedHistory": true,
            // The prompt folded into thread/start per the task spec. The
            // transport additionally sends a `turn/start` after the
            // thread/start response (see [`CodexBackend::execute`]) so a
            // real app-server actually begins the turn — the real codex
            // protocol splits thread/start (no prompt) from turn/start.
            "input": [{"type": "text", "text": prompt}]
        }
    })
}

/// Build the `thread/resume` JSON-RPC request (id=`id`) — used when
/// `opts.resume_session_id` is `Some`. Mirrors multica codex.go:2010-2018
/// for the resume param vocabulary (`threadId`, `cwd`, `model`,
/// `developerInstructions:null`, `excludeTurns:true`) plus the folded
/// `input` text content carrying the prompt (same rationale as
/// [`build_thread_start_request`]).
pub(crate) fn build_thread_resume_request(
    id: u64,
    session_id: &str,
    prompt: &str,
    cwd: &str,
    model: Option<&str>,
    system_prompt: Option<&str>,
    mcp_config: Option<&str>,
) -> Value {
    let model_val = model
        .map(String::from)
        .map(Value::String)
        .unwrap_or(Value::Null);
    let dev_instructions = system_prompt.map(|s| Value::String(s.to_string())).unwrap_or(Value::Null);
    // codex's Config key is `mcp_servers` (snake_case) — translate the
    // stored camelCase `mcpServers` wrapper so servers are registered.
    let config_val = translate_mcp_for_codex(mcp_config);
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "thread/resume",
        "params": {
            "threadId": session_id,
            "cwd": cwd,
            "model": model_val,
            "developerInstructions": dev_instructions,
            "config": config_val,
            // Only omit history from the response, not the resumed model
            // context — mirrors multica codex.go:2010-2018.
            "excludeTurns": true,
            // Prompt folded in per the task spec; see
            // [`build_thread_start_request`] for the rationale.
            "input": [{"type": "text", "text": prompt}]
        }
    })
}

/// Pull the codex thread id out of a `thread/start` or `thread/resume`
/// JSON-RPC response. Mirrors multica's `extractThreadID` (codex.go:4323):
/// the response `result` is `{"thread":{"id":"<thread-id>"}}` — return
/// `Some(result.thread.id)`. Accepts either the bare `result` object or a
/// full `{"jsonrpc":"2.0","id":..,"result":{...}}` envelope so the
/// transport can pass the whole parsed line. Returns `None` on any shape
/// mismatch (malformed, missing `thread.id`, non-object).
pub(crate) fn extract_session_id(response: &Value) -> Option<String> {
    // Accept either a bare `result` object (`{"thread":{"id":"..."}}`, the
    // shape multica's extractThreadID sees) or a full JSON-RPC response
    // envelope (`{"jsonrpc":"2.0","id":..,"result":{"thread":{...}}}`).
    // Unwrap `result` first, then look for `thread.id`.
    let target = response.get("result").unwrap_or(response);
    let id = target.get("thread")?.get("id")?.as_str()?;
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

// ── transport ──

/// Build the `turn/start` JSON-RPC request (id=`id`) — the RPC that
/// actually submits the prompt and begins a turn. The real codex protocol
/// splits thread/start (creates thread, no prompt) from turn/start
/// (submits `input` content); the desktop-pet folds the prompt into
/// thread/start per the task spec (see [`build_thread_start_request`]) AND
/// sends this separate turn/start so a real app-server kicks off the turn.
/// Mirrors multica codex.go:1617-1620,1672.
fn build_turn_start_request(id: u64, thread_id: &str, prompt: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "turn/start",
        "params": {
            "threadId": thread_id,
            "input": [{"type": "text", "text": prompt}]
        }
    })
}

/// Serialise `req` as one JSON-RPC line (`<json>\n`) and write it to the
/// app-server's stdin. Returns [`BackendError::Exec`] on a write/serialise
/// failure so `execute` surfaces it as a transport error (not a handshake
/// error — the handshake is the read side).
async fn write_request<W: AsyncWriteExt + Unpin>(
    stdin: &mut W,
    req: &Value,
) -> Result<(), BackendError> {
    let mut data =
        serde_json::to_string(req).map_err(|e| BackendError::Exec(format!("serialize request: {e}")))?;
    data.push('\n');
    stdin
        .write_all(data.as_bytes())
        .await
        .map_err(|e| BackendError::Exec(format!("write codex stdin: {e}")))
}

/// Read stdout lines from `reader` until a JSON-RPC **response** with
/// `id == expected_id` arrives; route every **notification** (no `id`)
/// received meanwhile through [`handle_event`] → forward `Message`s to
/// `messages_tx`. Returns the matched response [`Value`] (the whole parsed
/// line, including its `result`/`error` envelope) so the caller can
/// [`extract_session_id`] / inspect it. Pure-ish over `reader` — no
/// subprocess ownership; the same `BufReader<ChildStdout>` is borrowed for
/// each handshake step then moved into the event-loop task.
///
/// Errors (EOF / read error / non-JSON) → [`BackendError::Handshake`]
/// (the caller wraps this fn in `tokio::time::timeout` and converts a
/// timeout into a `Handshake` error too — the read-side of the handshake
/// is uniformly a handshake failure).
async fn read_until_response<R>(
    reader: &mut R,
    expected_id: u64,
    messages_tx: &mpsc::Sender<Message>,
) -> Result<Value, BackendError>
where
    R: AsyncBufRead + Unpin,
{
    let mut buf = String::new();
    loop {
        buf.clear();
        match reader.read_line(&mut buf).await {
            Ok(0) => {
                return Err(BackendError::Handshake(format!(
                    "codex stdout EOF before response to id={expected_id}"
                )))
            }
            Ok(_) => {}
            Err(e) => {
                return Err(BackendError::Handshake(format!(
                    "read codex stdout: {e}"
                )))
            }
        }
        let line = buf.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // skip non-JSON lines (stderr bleed, etc.)
        };
        let obj = match value.as_object() {
            Some(o) => o,
            None => continue,
        };
        // A response carries an `id`. Match it against the pending request.
        if let Some(id_val) = obj.get("id") {
            let id = id_val
                .as_u64()
                .or_else(|| id_val.as_i64().map(|i| i as u64));
            if id == Some(expected_id) {
                return Ok(value);
            }
            // A response to a different request id — shouldn't happen with
            // one outstanding request at a time; skip defensively.
            continue;
        }
        // Notification (no `id`, has `method`) — route through handle_event.
        match handle_event(&value) {
            EventOutcome::Message(m) => {
                // Best-effort forward — a dropped receiver just stops the
                // send; the handshake keeps reading so the response lands.
                let _ = messages_tx.send(m).await;
            }
            // A turn/completed arriving during the handshake is unexpected
            // (no turn started yet); we ignore it here and rely on the
            // handshake timeout to fail a genuinely stuck app-server.
            EventOutcome::Result(_) | EventOutcome::Nothing => {}
        }
    }
}

/// The post-handshake event loop: own the stdout `reader` + `stdin` (kept
/// open so codex does not treat stdin EOF as shutdown), route every
/// notification through [`handle_event`] → forward `Message`s to
/// `messages_tx`, and return the [`ExecResult`] on `turn/completed`
/// (or a synthesized "completed" on EOF / a "timeout" on overall timeout).
/// The caller wraps this in `tokio::time::timeout` when `opts.timeout`
/// is set and sends the returned result via `result_tx`.
async fn run_event_loop<R>(
    mut reader: R,
    // `_stdin` is owned but never written to in the loop — holding it keeps
    // the app-server's stdin pipe open so codex does not treat stdin EOF as
    // a shutdown signal mid-turn. Dropped (pipe closed) when the loop
    // returns / is cancelled by the overall timeout, which is exactly the
    // signal we want to send codex once the turn is done.
    _stdin: tokio::process::ChildStdin,
    messages_tx: mpsc::Sender<Message>,
) -> ExecResult
where
    R: AsyncBufRead + Unpin,
{
    let mut buf = String::new();
    // Track the last assistant Text so the completed-turn / EOF fallback
    // can hand the caller a non-empty `output` (mirrors claude.rs /
    // opencode.rs drain_stream). `handle_event` leaves completed-turn
    // `output` empty by design (it can't see the accumulated text), so
    // the loop substitutes this tracked value.
    let mut last_text = String::new();
    let mut session_id: Option<String> = None;

    loop {
        buf.clear();
        match reader.read_line(&mut buf).await {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(_) => break,
        }
        let line = buf.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Skip responses (they carry `id`). The handshake already consumed
        // the initialize + thread/start responses; a late turn/start ack
        // arriving mid-stream is just an acknowledgement, not user-visible.
        if value
            .as_object()
            .map(|o| o.contains_key("id"))
            .unwrap_or(false)
        {
            continue;
        }
        match handle_event(&value) {
            EventOutcome::Message(m) => {
                if m.kind == MessageKind::Text && !m.content.is_empty() {
                    last_text = m.content.clone();
                }
                // Seed session_id from any threadId on routed messages
                // (best-effort — turn/completed usually carries it).
                let _ = messages_tx.send(m).await;
            }
            EventOutcome::Result(mut r) => {
                if r.output.is_empty() {
                    r.output = last_text.clone();
                }
                if r.session_id.is_none() {
                    r.session_id = session_id.take();
                }
                return r;
            }
            EventOutcome::Nothing => {}
        }
    }

    // EOF without turn/completed — synthesize so the caller's `result`
    // always resolves. Status "completed" matches the claude/opencode
    // EOF-fallback contract; a future tightening can pass the child's exit
    // code in to report "failed" on a non-zero exit.
    ExecResult {
        status: "completed".into(),
        output: last_text,
        session_id,
        usage: HashMap::new(),
    }
}

#[async_trait]
impl Backend for CodexBackend {
    async fn execute(
        &self,
        prompt: &str,
        opts: ExecOptions,
    ) -> Result<Session, BackendError> {
        // 1. Resolve the codex binary. `cfg.executable_path` overrides
        // PATH lookup (matches multica's `cfg.ExecutablePath`).
        let exe_path = match &self.cfg.executable_path {
            Some(p) => Some(p.clone()),
            None => exec::resolve("codex"),
        };
        let exe_path =
            exe_path.ok_or_else(|| BackendError::Exec("codex not found".into()))?;

        let args = build_app_server_args();
        let mut child = exec::spawn(exe_path, args, self.cfg.env.clone(), opts.cwd.clone())
            .map_err(|e| BackendError::Exec(format!("spawn codex: {e}")))?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| BackendError::Exec("codex stdin pipe not captured".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| BackendError::Exec("codex stdout pipe not captured".into()))?;

        let (messages_tx, messages_rx) = mpsc::channel::<Message>(64);
        let (result_tx, result_rx) = oneshot::channel::<ExecResult>();

        let cwd_str = opts.cwd.to_str().unwrap_or("").to_string();
        let model_str = opts.model.as_deref();

        // The stdout reader is shared across the handshake steps (borrowed
        // here) and the post-handshake event loop (moved into the task).
        let mut reader = BufReader::with_capacity(MAX_LINE_BYTES, stdout);

        // 2. initialize handshake (id=1). Timeout → BackendError::Handshake
        // (mirrors multica's codexHandshakeTimeoutError, 30s default).
        let init_req = build_initialize_request(1);
        write_request(&mut stdin, &init_req)
            .await
            .map_err(|e| BackendError::Handshake(format!("write initialize: {e}")))?;
        let init_result =
            timeout(HANDSHAKE_TIMEOUT, read_until_response(&mut reader, 1, &messages_tx)).await;
        let _init_response = match init_result {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => {
                // read_until_response already returns a Handshake error.
                return Err(e);
            }
            Err(_) => {
                return Err(BackendError::Handshake(format!(
                    "codex initialize did not respond within {}s",
                    HANDSHAKE_TIMEOUT.as_secs()
                )));
            }
        };
        // Send the `initialized` notification (no id) — multica codex.go:1546.
        let initialized_notify = json!({"jsonrpc": "2.0", "method": "initialized"});
        // Best-effort: a failure here is a transport error, not a handshake.
        let _ = write_request(&mut stdin, &initialized_notify).await;

        // 3. thread/start (id=2) or thread/resume (id=2). Extract the
        // session/thread id from the response (multica's extractThreadID).
        let thread_req = match &opts.resume_session_id {
            Some(sid) => build_thread_resume_request(2, sid, prompt, &cwd_str, model_str, opts.system_prompt.as_deref(), opts.mcp_config.as_deref()),
            None => build_thread_start_request(2, prompt, &cwd_str, model_str, opts.system_prompt.as_deref(), opts.mcp_config.as_deref()),
        };
        write_request(&mut stdin, &thread_req)
            .await
            .map_err(|e| BackendError::Handshake(format!("write thread/start: {e}")))?;
        let thread_result =
            timeout(HANDSHAKE_TIMEOUT, read_until_response(&mut reader, 2, &messages_tx)).await;
        let thread_response = match thread_result {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                let method = if opts.resume_session_id.is_some() {
                    "thread/resume"
                } else {
                    "thread/start"
                };
                return Err(BackendError::Handshake(format!(
                    "codex {method} did not respond within {}s",
                    HANDSHAKE_TIMEOUT.as_secs()
                )));
            }
        };
        // If the app-server rejected the request (error response), fail
        // the handshake with the server's message rather than a missing-id
        // "no thread id" — gives the caller a real reason.
        if let Some(err_obj) = thread_response.get("error") {
            let msg = err_obj
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("codex thread setup rejected the request");
            return Err(BackendError::Handshake(msg.to_string()));
        }
        let session_id = extract_session_id(&thread_response).ok_or_else(|| {
            BackendError::Handshake(
                "codex thread/start response carried no thread.id".into(),
            )
        })?;

        // 4. turn/start (id=3) — submit the prompt and kick off the turn.
        // The real codex protocol requires this as a separate RPC; the
        // pure function `build_thread_start_request` folds the prompt into
        // thread/start per the task spec, and the transport sends this
        // turn/start so a real app-server actually begins the turn.
        let turn_req = build_turn_start_request(3, &session_id, prompt);
        if let Err(e) = write_request(&mut stdin, &turn_req).await {
            return Err(BackendError::Exec(format!("write turn/start: {e}")));
        }

        // 5. Spawn the event-loop task. It owns reader + stdin + child +
        // the two senders; on completion (turn/completed / EOF / overall
        // timeout) it sends the final ExecResult and reaps the child.
        // `execute` returns immediately with the channels.
        let overall_timeout = opts.timeout;
        let messages_tx_loop = messages_tx;
        tokio::spawn(async move {
            let result = match overall_timeout {
                Some(d) => match timeout(d, run_event_loop(reader, stdin, messages_tx_loop)).await {
                    Ok(r) => r,
                    Err(_) => ExecResult {
                        status: "timeout".into(),
                        output: format!("codex timed out after {}s", d.as_secs()),
                        session_id: None,
                        usage: HashMap::new(),
                    },
                },
                None => run_event_loop(reader, stdin, messages_tx_loop).await,
            };
            // messages_tx was moved into run_event_loop; when that future
            // returns (or is dropped on timeout) the channel closes.
            let _ = result_tx.send(result);
            // Reap the child so a wedged stdout reader does not leak the
            // process. `exec::spawn` sets `kill_on_drop(true)` so an
            // early-return error path also kills it.
            let _ = child.wait().await;
        });

        Ok(Session {
            messages: messages_rx,
            result: result_rx,
        })
    }
}

/// Build the fixed codex app-server argv — mirrors multica's
/// `buildCodexArgs` (codex.go:381-388): always `app-server --listen
/// stdio://`. The desktop-pet drops multica's per-task `ExtraArgs` /
/// `CustomArgs` / `McpConfig` plumbing (single-user interactive, not a
/// daemon).
fn build_app_server_args() -> Vec<String> {
    vec!["app-server".into(), "--listen".into(), "stdio://".into()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;


    // ── handle_event ──

    /// `turn/started` is not user-visible → Nothing.
    #[test]
    fn handle_event_turn_started_is_nothing() {
        let v = json!({
            "jsonrpc": "2.0",
            "method": "turn/started",
            "params": { "threadId": "th_1", "turn": { "id": "tn_1" } }
        });
        assert!(matches!(handle_event(&v), EventOutcome::Nothing));
    }

    /// `item/agentMessage/delta` (flat v2 shape: `{itemId, delta, threadId}`)
    /// → Message{Text, delta}. multica codex.go:3725-3730.
    #[test]
    fn handle_event_agent_message_delta_is_text() {
        let v = json!({
            "jsonrpc": "2.0",
            "method": "item/agentMessage/delta",
            "params": { "itemId": "it_1", "delta": "hello", "threadId": "th_1" }
        });
        let m = match handle_event(&v) {
            EventOutcome::Message(m) => m,
            other => panic!("expected Message, got {other:?}"),
        };
        assert_eq!(m.kind, MessageKind::Text);
        assert_eq!(m.content, "hello");
        assert!(m.tool.is_none() && m.call_id.is_none());
    }

    /// Empty delta → Nothing (no point surfacing an empty Text).
    #[test]
    fn handle_event_agent_message_delta_empty_is_nothing() {
        let v = json!({
            "jsonrpc": "2.0",
            "method": "item/agentMessage/delta",
            "params": { "itemId": "it_1", "delta": "", "threadId": "th_1" }
        });
        assert!(matches!(handle_event(&v), EventOutcome::Nothing));
    }

    /// `item/completed` with `item.type=="agentMessage"` → Message{Text,
    /// item.text} (the authoritative completed snapshot). multica
    /// codex.go:3798-3803.
    #[test]
    fn handle_event_agent_message_completed_is_text() {
        let v = json!({
            "jsonrpc": "2.0",
            "method": "item/completed",
            "params": {
                "threadId": "th_1",
                "item": { "id": "it_1", "type": "agentMessage", "text": "final answer" }
            }
        });
        let m = match handle_event(&v) {
            EventOutcome::Message(m) => m,
            other => panic!("expected Message, got {other:?}"),
        };
        assert_eq!(m.kind, MessageKind::Text);
        assert_eq!(m.content, "final answer");
    }

    /// `item/started` with `item.type=="commandExecution"` →
    /// Message{ToolUse, tool="exec_command", call_id=item.id,
    /// content=item.command}. multica codex.go:3732-3741.
    #[test]
    fn handle_event_command_started_is_tool_use() {
        let v = json!({
            "jsonrpc": "2.0",
            "method": "item/started",
            "params": {
                "threadId": "th_1",
                "item": { "id": "call_1", "type": "commandExecution", "command": "ls -la" }
            }
        });
        let m = match handle_event(&v) {
            EventOutcome::Message(m) => m,
            other => panic!("expected Message, got {other:?}"),
        };
        assert_eq!(m.kind, MessageKind::ToolUse);
        assert_eq!(m.tool.as_deref(), Some("exec_command"));
        assert_eq!(m.call_id.as_deref(), Some("call_1"));
        assert_eq!(m.content, "ls -la");
    }

    /// `item/completed` with `item.type=="commandExecution"` →
    /// Message{ToolResult, tool="exec_command", call_id=item.id,
    /// content=item.aggregatedOutput}. multica codex.go:3743-3752.
    #[test]
    fn handle_event_command_completed_is_tool_result() {
        let v = json!({
            "jsonrpc": "2.0",
            "method": "item/completed",
            "params": {
                "threadId": "th_1",
                "item": { "id": "call_1", "type": "commandExecution", "aggregatedOutput": "file_a\nfile_b" }
            }
        });
        let m = match handle_event(&v) {
            EventOutcome::Message(m) => m,
            other => panic!("expected Message, got {other:?}"),
        };
        assert_eq!(m.kind, MessageKind::ToolResult);
        assert_eq!(m.tool.as_deref(), Some("exec_command"));
        assert_eq!(m.call_id.as_deref(), Some("call_1"));
        assert_eq!(m.content, "file_a\nfile_b");
    }

    /// `item/started` with `item.type=="fileChange"` →
    /// Message{ToolUse, tool="patch_apply"}. multica codex.go:3754-3762.
    #[test]
    fn handle_event_file_change_started_is_tool_use() {
        let v = json!({
            "jsonrpc": "2.0",
            "method": "item/started",
            "params": {
                "threadId": "th_1",
                "item": { "id": "call_2", "type": "fileChange", "changes": [] }
            }
        });
        let m = match handle_event(&v) {
            EventOutcome::Message(m) => m,
            other => panic!("expected Message, got {other:?}"),
        };
        assert_eq!(m.kind, MessageKind::ToolUse);
        assert_eq!(m.tool.as_deref(), Some("patch_apply"));
        assert_eq!(m.call_id.as_deref(), Some("call_2"));
    }

    /// `item/completed` with `item.type=="fileChange"` →
    /// Message{ToolResult, tool="patch_apply"}. multica codex.go:3764-3774.
    #[test]
    fn handle_event_file_change_completed_is_tool_result() {
        let v = json!({
            "jsonrpc": "2.0",
            "method": "item/completed",
            "params": {
                "threadId": "th_1",
                "item": { "id": "call_2", "type": "fileChange", "status": "completed", "changes": [] }
            }
        });
        let m = match handle_event(&v) {
            EventOutcome::Message(m) => m,
            other => panic!("expected Message, got {other:?}"),
        };
        assert_eq!(m.kind, MessageKind::ToolResult);
        assert_eq!(m.tool.as_deref(), Some("patch_apply"));
        assert_eq!(m.call_id.as_deref(), Some("call_2"));
    }

    /// `turn/completed` with status `completed` → Result(ExecResult) with
    /// status "completed" and session_id from `params.threadId`. multica
    /// codex.go:3491-3526.
    #[test]
    fn handle_event_turn_completed_is_result() {
        let v = json!({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": {
                "threadId": "th_42",
                "turn": { "id": "tn_1", "status": "completed" }
            }
        });
        let r = match handle_event(&v) {
            EventOutcome::Result(r) => r,
            other => panic!("expected Result, got {other:?}"),
        };
        assert_eq!(r.status, "completed");
        assert_eq!(r.session_id.as_deref(), Some("th_42"));
    }

    /// `turn/completed` with status `failed` → Result with status "failed"
    /// and output = `params.turn.error.message`. multica codex.go:3506-3511.
    #[test]
    fn handle_event_turn_completed_failed_is_failed_result() {
        let v = json!({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": {
                "threadId": "th_42",
                "turn": { "id": "tn_1", "status": "failed", "error": { "message": "model unavailable" } }
            }
        });
        let r = match handle_event(&v) {
            EventOutcome::Result(r) => r,
            other => panic!("expected Result, got {other:?}"),
        };
        assert_eq!(r.status, "failed");
        assert_eq!(r.output, "model unavailable");
        assert_eq!(r.session_id.as_deref(), Some("th_42"));
    }

    /// `turn/completed` with status `cancelled` → Result with status
    /// "aborted" (multica codex.go:3501-3502: cancelled/canceled/aborted/
    /// interrupted → aborted).
    #[test]
    fn handle_event_turn_completed_cancelled_is_aborted() {
        let v = json!({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": {
                "threadId": "th_42",
                "turn": { "id": "tn_1", "status": "cancelled" }
            }
        });
        let r = match handle_event(&v) {
            EventOutcome::Result(r) => r,
            other => panic!("expected Result, got {other:?}"),
        };
        assert_eq!(r.status, "aborted");
        assert_eq!(r.session_id.as_deref(), Some("th_42"));
    }

    /// Top-level `error` notification with `willRetry:false` → Result
    /// (failed). multica codex.go:3528-3549.
    #[test]
    fn handle_event_terminal_error_is_failed_result() {
        let v = json!({
            "jsonrpc": "2.0",
            "method": "error",
            "params": { "willRetry": false, "error": { "message": "boom" } }
        });
        let r = match handle_event(&v) {
            EventOutcome::Result(r) => r,
            other => panic!("expected Result, got {other:?}"),
        };
        assert_eq!(r.status, "failed");
        assert_eq!(r.output, "boom");
    }

    /// Top-level `error` with `willRetry:true` → Nothing (transient; the
    /// turn continues). multica codex.go:3532,3540-3541.
    #[test]
    fn handle_event_retriable_error_is_nothing() {
        let v = json!({
            "jsonrpc": "2.0",
            "method": "error",
            "params": { "willRetry": true, "error": { "message": "transient" } }
        });
        assert!(matches!(handle_event(&v), EventOutcome::Nothing));
    }

    /// Unknown / unrecognised method → Nothing.
    #[test]
    fn handle_event_unknown_method_is_nothing() {
        let v = json!({
            "jsonrpc": "2.0",
            "method": "thread/tokenUsage/updated",
            "params": { "threadId": "th_1" }
        });
        assert!(matches!(handle_event(&v), EventOutcome::Nothing));
    }

    /// Malformed (no method) → Nothing (no panic).
    #[test]
    fn handle_event_no_method_is_nothing() {
        let v = json!({ "jsonrpc": "2.0", "params": {} });
        assert!(matches!(handle_event(&v), EventOutcome::Nothing));
    }

    // ── build_initialize_request ──

    #[test]
    fn build_initialize_request_shape() {
        let req = build_initialize_request(1);
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["id"], 1);
        assert_eq!(req["method"], "initialize");
        assert_eq!(req["params"]["clientInfo"]["name"], "sss-pet");
        assert!(req["params"]["clientInfo"]["version"].is_string());
        assert_eq!(req["params"]["capabilities"]["experimentalApi"], true);
    }

    // ── build_thread_start_request ──

    #[test]
    fn build_thread_start_request_shape() {
        let req = build_thread_start_request(2, "hello", "/tmp/proj", None, None, None);
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["id"], 2);
        assert_eq!(req["method"], "thread/start");
        assert_eq!(req["params"]["cwd"], "/tmp/proj");
        assert_eq!(req["params"]["model"], serde_json::Value::Null);
        assert_eq!(req["params"]["input"][0]["type"], "text");
        assert_eq!(req["params"]["input"][0]["text"], "hello");
    }

    #[test]
    fn build_thread_start_request_with_model() {
        let req = build_thread_start_request(2, "hi", "/tmp", Some("gpt-5.5"), None, None);
        assert_eq!(req["params"]["model"], "gpt-5.5");
    }

    // ── build_thread_resume_request ──

    #[test]
    fn build_thread_resume_request_shape() {
        let req = build_thread_resume_request(2, "th_prior", "again", "/tmp", None, None, None);
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["id"], 2);
        assert_eq!(req["method"], "thread/resume");
        assert_eq!(req["params"]["threadId"], "th_prior");
        assert_eq!(req["params"]["cwd"], "/tmp");
        assert_eq!(req["params"]["model"], serde_json::Value::Null);
        assert_eq!(req["params"]["input"][0]["type"], "text");
        assert_eq!(req["params"]["input"][0]["text"], "again");
        assert_eq!(req["params"]["excludeTurns"], true);
    }

    #[test]
    fn build_thread_resume_request_with_model() {
        let req = build_thread_resume_request(2, "th_prior", "hi", "/tmp", Some("o3"), None, None);
        assert_eq!(req["params"]["model"], "o3");
    }

    // ── MCP config translation (mcpServers → mcp_servers) ──

    #[test]
    fn build_thread_start_request_translates_mcp_to_snake_case() {
        let mcp = r#"{"mcpServers":{"todos":{"command":"x","args":["y"]}}}"#;
        let req = build_thread_start_request(2, "hi", "/tmp", None, None, Some(mcp));
        let config = &req["params"]["config"];
        assert!(config.is_object(), "config must be an object when mcp set");
        assert!(
            config.get("mcp_servers").is_some(),
            "config must use the snake_case mcp_servers key codex reads"
        );
        assert!(
            config.get("mcpServers").is_none(),
            "camelCase wrapper must be removed (codex would otherwise drop it)"
        );
        assert_eq!(config["mcp_servers"]["todos"]["command"], "x");
        assert_eq!(config["mcp_servers"]["todos"]["args"][0], "y");
    }

    #[test]
    fn build_thread_resume_request_translates_mcp_to_snake_case() {
        let mcp = r#"{"mcpServers":{"todos":{"command":"x","args":[]}}}"#;
        let req = build_thread_resume_request(2, "th", "hi", "/tmp", None, None, Some(mcp));
        let config = &req["params"]["config"];
        assert!(config.get("mcp_servers").is_some());
        assert!(config.get("mcpServers").is_none());
    }

    #[test]
    fn build_thread_start_request_null_config_when_mcp_absent() {
        let req = build_thread_start_request(2, "hi", "/tmp", None, None, None);
        assert_eq!(req["params"]["config"], Value::Null);
    }

    #[test]
    fn build_thread_start_request_preserves_sibling_keys() {
        // A `version` sibling key rides along unchanged; only the wrapper
        // key is renamed.
        let mcp = r#"{"version":1,"mcpServers":{"todos":{"command":"x"}}}"#;
        let req = build_thread_start_request(2, "hi", "/tmp", None, None, Some(mcp));
        let config = &req["params"]["config"];
        assert_eq!(config["version"], 1);
        assert!(config.get("mcp_servers").is_some());
    }

    // ── extract_session_id ──

    #[test]
    fn extract_session_id_from_bare_result() {
        // Bare `result` object (the shape multica's extractThreadID sees).
        let v = json!({ "thread": { "id": "th_abc" } });
        assert_eq!(extract_session_id(&v).as_deref(), Some("th_abc"));
    }

    #[test]
    fn extract_session_id_from_full_response() {
        // Full JSON-RPC response envelope — the transport parses a whole
        // stdout line and passes it here.
        let v = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": { "thread": { "id": "th_xyz" } }
        });
        assert_eq!(extract_session_id(&v).as_deref(), Some("th_xyz"));
    }

    #[test]
    fn extract_session_id_absent_returns_none() {
        assert!(extract_session_id(&json!({})).is_none());
        assert!(extract_session_id(&json!({ "thread": {} })).is_none());
        assert!(extract_session_id(&json!({ "thread": "not an object" })).is_none());
        assert!(extract_session_id(&json!({ "result": {} })).is_none());
        assert!(extract_session_id(&json!("not even an object")).is_none());
    }

    // ── execute: error paths (no real codex) ──

    /// With a bogus `executable_path`, `spawn` fails → BackendError::Exec.
    /// Exercises the resolve/spawn wiring without depending on codex being
    /// installed (the path is set explicitly so PATH lookup is bypassed).
    #[tokio::test]
    async fn execute_returns_exec_err_when_executable_path_missing() {
        let cfg = BackendConfig {
            executable_path: Some(PathBuf::from(
                "/definitely/not/a/real/codex_binary_xyz123",
            )),
            env: HashMap::new(),
        };
        let backend = CodexBackend { cfg };
        let opts = ExecOptions {
            cwd: std::env::current_dir().unwrap(),
            model: None,
            resume_session_id: None,
            timeout: None,
            system_prompt: None,
            mcp_config: None,
        };
        let res = backend.execute("hi", opts).await;
        assert!(
            matches!(res, Err(BackendError::Exec(_))),
            "expected Exec error for missing executable, got {res:?}"
        );
    }

    /// Real codex app-server end-to-end. `#[ignore]` because it needs a
    /// real codex login (auth) + network for the model, and the app-server
    /// is heavier/flakier than the one-shot CLIs. Run with
    /// `cargo test --manifest-path src-tauri/Cargo.toml codex -- --ignored --nocapture`.
    ///
    /// Asserts: execute returns Ok(Session); at least one Text message
    /// streams across `messages`; `result` resolves with status "completed"
    /// (or "failed" with a real error message — still proves the handshake
    /// + turn lifecycle wired up); `session_id` is Some.
    #[tokio::test]
    #[ignore]
    async fn real_codex_app_server_end_to_end() {
        let backend = CodexBackend {
            cfg: BackendConfig::default(),
        };
        let opts = ExecOptions {
            cwd: std::env::current_dir().unwrap(),
            model: None,
            resume_session_id: None,
            timeout: Some(Duration::from_secs(120)),
            system_prompt: None,
            mcp_config: None,
        };
        let mut session = backend
            .execute("Reply with exactly: hello", opts)
            .await
            .expect("execute should return a Session");

        let mut got_text = false;
        let mut got_status = false;
        // Drain messages concurrently with awaiting the result. Cap the
        // wait so a hung app-server fails the test instead of hanging CI.
        let result = tokio::select! {
            r = (&mut session.result) => r.expect("result resolves"),
            _ = tokio::time::sleep(Duration::from_secs(180)) => {
                panic!("timed out waiting for turn/completed");
            }
        };

        while let Ok(m) = session.messages.try_recv() {
            if m.kind == MessageKind::Text {
                got_text = true;
            }
            if m.kind == MessageKind::Status {
                got_status = true;
            }
        }

        log::error!(
            "real-codex result: status={:?} output={:?} session_id={:?} got_text={} got_status={}",
            result.status,
            result.output,
            result.session_id,
            got_text,
            got_status
        );
        assert!(
            got_text,
            "expected at least one Text message from real codex"
        );
        assert!(
            result.session_id.is_some(),
            "expected a real session_id, got {:?}",
            result.session_id
        );
        // Status may be "completed" (happy path) or "failed" (auth /
        // network). Either proves the handshake + turn lifecycle wired up.
        assert!(
            result.status == "completed" || result.status == "failed" || result.status == "aborted",
            "expected a terminal status, got {:?}",
            result.status
        );
        let _ = got_status;
    }
}
