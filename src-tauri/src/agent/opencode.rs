//! Task 11: opencode backend — spawns the opencode CLI in json-events mode
//! (`opencode run --format json --auto`), writes the prompt to its stdin as
//! raw text, and parses stdout into unified [`Message`]s + a final
//! [`ExecResult`]. Mirrors multica's `opencodeBackend` pattern
//! (`server/pkg/agent/opencode.go`).
//!
//! Refactored for testability like claude.rs (Task 10): the line-draining
//! logic lives in a pure async fn [`drain_stream`] that takes any
//! `tokio::io::AsyncBufRead` reader — tests feed it a
//! `tokio::io::BufReader::new(&[u8])` of fixture lines, no subprocess.
//! [`OpencodeBackend::execute`] wires [`drain_stream`] to the spawned
//! opencode CLI's buffered stdout.
//!
//! Design ref: `docs/superpowers/specs/2026-10-21-desktop-pet-design.md`
//! ("Agent 接入"). Args / stdin shape sourced from multica's
//! `opencodeBackend.Execute` (opencode.go:71) — `opencode run --format json`
//! + the prompt piped to stdin (never argv, to dodge Windows
//! `CreateProcess` 32,767-char lpCommandLine cap and keep the prompt out
//! of process listings).
//!
//! **Arg divergence from multica:** multica (opencode.go:95) passes
//! `--dangerously-skip-permissions`. The opencode CLI installed for this
//! project is **1.18.30**, which renamed that flag to `--auto` ("auto-approve
//! permissions that are not explicitly denied") — verified via
//! `opencode run --help` (the `--dangerously-skip-permissions` flag is absent
//! from 1.18.30's help and the CLI rejects unknown flags before starting).
//! The intent is identical (non-interactive, auto-approved permission run);
//! we use `--auto` so `execute` actually works against the installed CLI
//! (and real-opencode validation can run). multica's event vocabulary is
//! otherwise unchanged across this version bump.
//!
//! **Event vocabulary** (per multica opencode.go:739-836, confirmed against
//! a real `opencode run --format json --auto` capture — see
//! `tests/fixtures/opencode_stream.txt`): each stdout line is one JSON object
//! with top-level `type` ∈ {`step_start`, `text`, `tool_use`, `error`,
//! `step_finish`}, a `sessionID`, and a `part` object. opencode has **no
//! positive terminal `result` event** (unlike claude's `type:"result"`) —
//! multica detects completion at EOF via step-bracketing
//! (opencode.go:547-569). [`parse_result_event`] therefore handles only the
//! NEGATIVE terminal signal (an `error` event → stop + "failed"); normal
//! completion is the EOF fallback in [`drain_stream`].

use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use super::exec;
use super::{
    Backend, BackendError, ExecOptions, ExecResult, Message, MessageKind, OpencodeBackend, Session,
};

/// Build the argv for the opencode CLI, mirroring multica's
/// `opencodeBackend.Execute` (opencode.go:95-140). Emits
/// `run --format json --auto` (the 1.18.30 rename of multica's
/// `--dangerously-skip-permissions` — see the module docs for the divergence
/// rationale). Adds `--model M` when `opts.model` is set and `--session <id>`
/// when `opts.resume_session_id` is set (matches multica opencode.go:138-140
/// and the 1.18.30 `opencode run --help` `-s, --session` flag).
pub(crate) fn build_args(opts: &ExecOptions) -> Vec<String> {
    let mut args: Vec<String> = vec!["run".into(), "--format".into(), "json".into(), "--auto".into()];
    if let Some(model) = &opts.model {
        args.push("--model".into());
        args.push(model.clone());
    }
    if let Some(id) = &opts.resume_session_id {
        args.push("--session".into());
        args.push(id.clone());
    }
    args
}

/// Parse one opencode json event line into a [`Message`].
///
/// Mapping (per multica opencode.go's event vocabulary):
/// - `text` (part.text) → `Text` (empty text skipped).
/// - `tool_use`: if `part.state.status` is terminal (`completed`/`error`)
///   → `ToolResult` (call+result are bundled in one opencode event —
///   multica's `handleToolUseEvent` emits both, but our `parse_line` returns
///   a single [`Message`], so a terminal tool surfaces as the `ToolResult`
///   with `state.output` / `state.error`). Otherwise → `ToolUse` with
///   `part.state.input` serialized as a JSON string.
/// - `step_start` → `Status{content:"running"}` (multica emits
///   `MessageStatus:"running"`).
/// - `error` (error.data.message || error.name) → `Error`.
/// - `step_finish` → `None` (not user-visible; `drain_stream` tracks
///   completion via the absence of a later `error` / via step bracketing at
///   EOF per multica).
/// - malformed / non-JSON / unrecognized → `None`.
pub(crate) fn parse_line(line: &str) -> Option<Message> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(trimmed).ok()?;
    let obj = value.as_object()?;
    let kind = obj.get("type")?.as_str()?;
    match kind {
        "text" => parse_text(obj),
        "tool_use" => parse_tool_use(obj),
        "step_start" => Some(Message {
            kind: MessageKind::Status,
            content: "running".into(),
            tool: None,
            call_id: None,
        }),
        "error" => parse_error(obj),
        // step_finish carries usage/reason only — not user-visible. drain_stream
        // infers completion from the absence of a subsequent `error` event
        // (the EOF fallback), matching multica's step-bracketing approach.
        "step_finish" | _ => None,
    }
}

/// Parse an `error` event into the terminal failure payload
/// `(error_message, session_id)`. multica opencode.go:657-671: the message is
/// `error.data.message` falling back to `error.name`, then a generic string.
/// `session_id` comes from the top-level `sessionID` (present on most events).
pub(crate) fn parse_result_event(line: &str) -> Option<(String, Option<String>)> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(trimmed).ok()?;
    let obj = value.as_object()?;
    let kind = obj.get("type")?.as_str()?;
    if kind != "error" {
        return None;
    }
    let msg = error_message(obj).unwrap_or_else(|| "unknown opencode error".into());
    let session_id = obj
        .get("sessionID")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some((msg, session_id))
}

/// Drain `reader` line by line, forwarding parsed [`Message`]s to
/// `messages_tx` and a single [`ExecResult`] to `result_tx`.
///
/// Same shape as claude.rs's `drain_stream` but calling opencode's
/// [`parse_line`] / [`parse_result_event`]. Pure over `reader` — pass a
/// `tokio::io::BufReader::new(&[u8])` of fixture bytes in tests and no
/// subprocess is spawned.
///
/// Behavior:
/// - Accumulate `session_id` from every line's `sessionID` field
///   (multica opencode.go:483).
/// - For each non-empty line, call [`parse_line`]; if `Some(msg)`, forward
///   to `messages_tx` (best-effort — a dropped receiver just stops the send,
///   draining continues). Track the last assistant `Text` content for the
///   EOF fallback's `output`.
/// - Call [`parse_result_event`]; if `Some((err, sid))`, send
///   `ExecResult { status:"failed", output:err, session_id, .. }` and stop
///   (the only opencode terminal signal is an `error` event).
/// - If the reader reaches EOF with no `error` event, synthesize
///   `ExecResult { status:"completed", output:<last text>, session_id, .. }`
///   so the caller's `result` always resolves.
pub(crate) async fn drain_stream<R>(
    mut reader: R,
    messages_tx: mpsc::Sender<Message>,
    result_tx: oneshot::Sender<ExecResult>,
) where
    R: AsyncBufRead + Unpin,
{
    let mut buf = String::new();
    // Track the last assistant `Text` so the EOF fallback (and an error
    // event whose message is empty) can still hand the caller a non-empty
    // `output`.
    let mut last_text = String::new();
    // Accumulated session id — multica opencode.go:483-488 reads it off any
    // event that carries it, since opencode has no terminal result event
    // that would otherwise surface it.
    let mut session_id: Option<String> = None;

    loop {
        buf.clear();
        match reader.read_line(&mut buf).await {
            Ok(0) => break, // EOF
            Ok(_) => {}
            // Read error — treat as EOF. The error event, if any, already
            // went out; otherwise the EOF branch synthesizes.
            Err(_) => break,
        }

        let line = buf.trim();
        if line.is_empty() {
            continue;
        }

        // Seed session_id from any event carrying it (multica opencode.go:483).
        if let Some(sid) = extract_session_id(line) {
            session_id = Some(sid);
        }

        // Message-bearing line? Forward to the messages channel.
        if let Some(msg) = parse_line(line) {
            if msg.kind == MessageKind::Text && !msg.content.is_empty() {
                last_text = msg.content.clone();
            }
            // Best-effort send — a dropped receiver just stops the send; we
            // keep draining so a terminal error event is still captured.
            let _ = messages_tx.send(msg).await;
        }

        // Terminal error event? Emit the final ExecResult (failed) and stop.
        if let Some((err, sid)) = parse_result_event(line) {
            let session_id = sid.or(session_id);
            let _ = result_tx.send(ExecResult {
                status: "failed".into(),
                output: err,
                session_id,
                usage: HashMap::new(),
            });
            return;
        }
        // Any other event type (step_start / step_finish / unknown / non-JSON)
        // is either surfaced via parse_line above or silently skipped — there
        // is no positive terminal result event in opencode's stream.
    }

    // EOF without an error event — synthesize so the caller's `result`
    // always resolves. opencode detects completion via step bracketing at EOF
    // (multica opencode.go:547-569); we simplify to "completed" since we do
    // not track step-start/step-finish open/closed pairs here.
    let _ = result_tx.send(ExecResult {
        status: "completed".into(),
        output: last_text,
        session_id,
        usage: HashMap::new(),
    });
}

/// Extract the `sessionID` field from any opencode event line. multica
/// accumulates `event.SessionID` from every event that carries it
/// (opencode.go:483-488); `drain_stream` does the same to seed the final
/// `ExecResult.session_id` at EOF. Internal helper (not part of the
/// task-11 interface) to avoid re-parsing inside `drain_stream`.
fn extract_session_id(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(trimmed).ok()?;
    value
        .as_object()?
        .get("sessionID")?
        .as_str()
        .map(|s| s.to_string())
}

// ── per-type parse helpers ──

/// Parse a `text` event: `part.text` → `Text` (empty text skipped).
fn parse_text(obj: &serde_json::Map<String, Value>) -> Option<Message> {
    let text = obj.get("part")?.get("text")?.as_str()?;
    if text.is_empty() {
        return None;
    }
    Some(Message {
        kind: MessageKind::Text,
        content: text.to_string(),
        tool: None,
        call_id: None,
    })
}

/// Parse a `tool_use` event (multica opencode.go handleToolUseEvent:621-652).
/// opencode bundles the call + result in one event: `part.state.status`
/// terminal (`completed`/`error`) → `ToolResult` (content = `state.output`
/// or `state.error`); non-terminal → `ToolUse` (content = `state.input` as a
/// JSON string). Our `parse_line` returns a single [`Message`], so a terminal
/// tool surfaces as the `ToolResult` (the actionable half).
fn parse_tool_use(obj: &serde_json::Map<String, Value>) -> Option<Message> {
    let part = obj.get("part")?.as_object()?;
    let tool = part.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    let call_id = part.get("callID").and_then(|v| v.as_str()).unwrap_or("");
    let state = part.get("state").and_then(|v| v.as_object());

    if let Some(state) = state {
        let status = state.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if status == "completed" || status == "error" {
            let content = if status == "error" {
                state
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            } else {
                extract_output(state.get("output"))
            };
            return Some(Message {
                kind: MessageKind::ToolResult,
                content,
                tool: if tool.is_empty() { None } else { Some(tool.to_string()) },
                call_id: if call_id.is_empty() { None } else { Some(call_id.to_string()) },
            });
        }
    }

    // Non-terminal → ToolUse with input serialized as a JSON string.
    let content = match state.and_then(|s| s.get("input")) {
        Some(Value::Null) | None => String::new(),
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
    };
    Some(Message {
        kind: MessageKind::ToolUse,
        content,
        tool: if tool.is_empty() { None } else { Some(tool.to_string()) },
        call_id: if call_id.is_empty() { None } else { Some(call_id.to_string()) },
    })
}

/// Parse an `error` event → `Error{content: message}`. The message is
/// `error.data.message` falling back to `error.name`, then a generic string
/// (multica opencode.go handleErrorEvent:657-671).
fn parse_error(obj: &serde_json::Map<String, Value>) -> Option<Message> {
    let msg = error_message(obj).unwrap_or_else(|| "unknown opencode error".into());
    Some(Message {
        kind: MessageKind::Error,
        content: msg,
        tool: None,
        call_id: None,
    })
}

/// Extract the human-readable message from an opencode `error` event:
/// `error.data.message` || `error.name`. `None` if neither is present.
fn error_message(obj: &serde_json::Map<String, Value>) -> Option<String> {
    let err = obj.get("error")?.as_object()?;
    err.get("data")
        .and_then(|d| d.get("message"))
        .and_then(|v| v.as_str())
        .or_else(|| err.get("name").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
}

/// Coerce a tool state `output` value to a display string (multica
/// `extractToolOutput` opencode.go:728-737): JSON string → verbatim; any
/// other JSON value (object / array / null) → its JSON serialization.
fn extract_output(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        None => String::new(),
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
    }
}

#[async_trait]
impl Backend for OpencodeBackend {
    async fn execute(
        &self,
        prompt: &str,
        opts: ExecOptions,
    ) -> Result<Session, BackendError> {
        // Resolve the opencode binary. `cfg.executable_path` overrides PATH
        // lookup (matches multica's `cfg.ExecutablePath`).
        let exe_path = match &self.cfg.executable_path {
            Some(p) => Some(p.clone()),
            None => exec::resolve("opencode"),
        };
        let exe_path = exe_path
            .ok_or_else(|| BackendError::Exec("opencode not found".into()))?;

        let args = build_args(&opts);
        let mut child = exec::spawn(exe_path, args, self.cfg.env.clone(), opts.cwd.clone())
            .map_err(|e| BackendError::Exec(format!("spawn opencode: {e}")))?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| BackendError::Exec("opencode stdin pipe not captured".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| BackendError::Exec("opencode stdout pipe not captured".into()))?;

        // Write the prompt text to stdin, then close it. `opencode run`
        // merges its variadic `[message..]` positional with piped stdin; with
        // no positionals the piped text is the entire run message
        // (multica opencode.go:129-152). Closing stdin is what ends the prompt
        // — opencode reads stdin to EOF (`await Bun.stdin.text()`), so a stdin
        // left open hangs the run forever.
        //
        // opencode doesn't support CLI system-prompt flags, so the agent's
        // system_prompt (if set) is prepended to the prompt text.
        let effective_prompt = if let Some(sp) = &opts.system_prompt {
            format!("{sp}\n\n{prompt}")
        } else {
            prompt.to_string()
        };
        if let Err(e) = stdin.write_all(effective_prompt.as_bytes()).await {
            return Err(BackendError::Exec(format!("write opencode stdin: {e}")));
        }
        drop(stdin);

        let (messages_tx, messages_rx) = mpsc::channel::<Message>(64);
        let (result_tx, result_rx) = oneshot::channel::<ExecResult>();

        let reader = tokio::io::BufReader::new(stdout);
        tokio::spawn(async move {
            drain_stream(reader, messages_tx, result_tx).await;
            // Reap the child after draining so a wedged stdout reader does
            // not leak the process. `exec::spawn` sets `kill_on_drop(true)`
            // (Task 11 fix) so an early-return error path also kills it.
            let _ = child.wait().await;
        });

        Ok(Session {
            messages: messages_rx,
            result: result_rx,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Absolute path to the committed fixture (one real-shape opencode json
    /// event line per row). Built from `CARGO_MANIFEST_DIR` so the test is
    /// independent of the cwd `cargo test` happens to run from. The fixture
    /// is a real capture from `printf 'Reply with exactly: hello' | opencode
    /// run --format json --auto -m aliyun/glm-5.2 --pure` (opencode 1.18.30).
    fn fixture_bytes() -> Vec<u8> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("opencode_stream.txt");
        std::fs::read(&path).unwrap_or_else(|e| panic!("read fixture: {e}"))
    }

    /// Drain `bytes` through [`drain_stream`] and collect the [`Message`]s
    /// + [`ExecResult`] for assertion. Wraps the boilerplate so each test
    /// reads as a one-liner.
    async fn drain(
        bytes: &[u8],
    ) -> (
        Vec<Message>,
        std::result::Result<ExecResult, oneshot::error::RecvError>,
    ) {
        let (messages_tx, mut messages_rx) = mpsc::channel::<Message>(64);
        let (result_tx, result_rx) = oneshot::channel::<ExecResult>();
        let reader = tokio::io::BufReader::new(bytes);
        drain_stream(reader, messages_tx, result_tx).await;
        let mut msgs: Vec<Message> = Vec::new();
        while let Ok(m) = messages_rx.try_recv() {
            msgs.push(m);
        }
        let result = result_rx.await;
        (msgs, result)
    }

    // -- drain_stream: fixture path (RED→GREEN) --

    /// Feed the real-capture fixture through `drain_stream`, collect the
    /// `Message`s + the `ExecResult`, assert: at least one
    /// `Message{Text,"hello"}`; the `ExecResult.status == "completed"`;
    /// `output == "hello"`; `session_id` is `Some` (the fixture carries
    /// `ses_ea0e0e01effe3j0Or4BA1BIi96`).
    #[tokio::test]
    async fn drain_stream_fixture_yields_text_message_and_completed_result() {
        let bytes = fixture_bytes();
        let (msgs, result) = drain(&bytes).await;
        let result = result.expect("result resolves");

        assert!(
            msgs.iter()
                .any(|m| m.kind == MessageKind::Text && m.content == "hello"),
            "expected a Text message 'hello', got {msgs:?}"
        );
        assert_eq!(
            result.status, "completed",
            "status should be completed, got {result:?}"
        );
        assert_eq!(result.output, "hello", "output should be the text, got {result:?}");
        assert!(
            result.session_id.is_some(),
            "session_id should be Some (fixture carries it), got {result:?}"
        );
        assert_eq!(
            result.session_id.as_deref(),
            Some("ses_ea0e0e01effe3j0Or4BA1BIi96")
        );
    }

    /// A stream with NO `error` event and NO `sessionID` must still resolve
    /// `result` (status "completed", `session_id` None, `output` = last text).
    /// This is the opencode EOF fallback — opencode has no terminal result
    /// event, so every clean run lands here.
    #[tokio::test]
    async fn drain_stream_no_result_event_still_resolves_result() {
        // One text event, no sessionID, no step_finish — a truncated stream.
        let bytes = b"{\"type\":\"text\",\"timestamp\":1,\"part\":{\"type\":\"text\",\"text\":\"hi\"}}\n";

        let (msgs, result) = drain(bytes).await;
        let result = result.expect("result resolves");

        assert!(
            msgs.iter().any(|m| m.kind == MessageKind::Text && m.content == "hi"),
            "expected the text message, got {msgs:?}"
        );
        assert!(
            result.session_id.is_none(),
            "session_id should be None (no sessionID in stream), got {result:?}"
        );
        assert_eq!(result.status, "completed", "EOF fallback status, got {result:?}");
        assert_eq!(result.output, "hi");
    }

    /// An `error` event must terminate draining with status "failed" and the
    /// error message as `output` (multica opencode.go:657-671).
    #[tokio::test]
    async fn drain_stream_error_event_yields_failed_result() {
        let bytes = b"{\"type\":\"text\",\"sessionID\":\"ses_x\",\"part\":{\"type\":\"text\",\"text\":\"partial\"}}\n\
                     {\"type\":\"error\",\"sessionID\":\"ses_x\",\"error\":{\"name\":\"AbortError\",\"data\":{\"message\":\"model unavailable\"}}}\n";

        let (msgs, result) = drain(bytes).await;
        let result = result.expect("result resolves");

        assert_eq!(result.status, "failed", "error event -> failed, got {result:?}");
        assert_eq!(result.output, "model unavailable");
        assert_eq!(result.session_id.as_deref(), Some("ses_x"));
        // The error event also surfaces as an Error Message before drain stops.
        assert!(
            msgs.iter().any(|m| m.kind == MessageKind::Error && m.content == "model unavailable"),
            "expected an Error message, got {msgs:?}"
        );
    }

    // -- parse_line: per-event mapping --

    #[test]
    fn parse_line_text_event() {
        let line = r#"{"type":"text","sessionID":"s","part":{"type":"text","text":"hello"}}"#;
        let m = parse_line(line).expect("text -> Some");
        assert_eq!(m.kind, MessageKind::Text);
        assert_eq!(m.content, "hello");
        assert!(m.tool.is_none() && m.call_id.is_none());
    }

    #[test]
    fn parse_line_text_empty_returns_none() {
        let line = r#"{"type":"text","part":{"type":"text","text":""}}"#;
        assert!(parse_line(line).is_none());
    }

    #[test]
    fn parse_line_tool_use_terminal_completed_is_tool_result() {
        // multica opencode.go handleToolUseEvent: terminal state -> ToolResult.
        let line = r#"{"type":"tool_use","part":{"tool":"Read","callID":"call-1","state":{"status":"completed","input":{"file_path":"/tmp/x"},"output":"file body"}}}"#;
        let m = parse_line(line).expect("terminal tool_use -> Some");
        assert_eq!(m.kind, MessageKind::ToolResult);
        assert_eq!(m.tool.as_deref(), Some("Read"));
        assert_eq!(m.call_id.as_deref(), Some("call-1"));
        assert_eq!(m.content, "file body");
    }

    #[test]
    fn parse_line_tool_use_terminal_error_is_tool_result_with_error() {
        let line = r#"{"type":"tool_use","part":{"tool":"Bash","callID":"call-2","state":{"status":"error","input":{},"output":null,"error":"boom"}}}"#;
        let m = parse_line(line).expect("error tool_use -> Some");
        assert_eq!(m.kind, MessageKind::ToolResult);
        assert_eq!(m.call_id.as_deref(), Some("call-2"));
        assert_eq!(m.content, "boom");
    }

    #[test]
    fn parse_line_tool_use_non_terminal_is_tool_use() {
        let line = r#"{"type":"tool_use","part":{"tool":"Write","callID":"call-3","state":{"status":"running","input":{"path":"/p","content":"x"}}}}"#;
        let m = parse_line(line).expect("non-terminal tool_use -> Some");
        assert_eq!(m.kind, MessageKind::ToolUse);
        assert_eq!(m.tool.as_deref(), Some("Write"));
        assert_eq!(m.call_id.as_deref(), Some("call-3"));
        assert!(m.content.contains("\"path\":\"/p\""));
    }

    #[test]
    fn parse_line_step_start_is_status_running() {
        let line = r#"{"type":"step_start","sessionID":"s","part":{"type":"step-start"}}"#;
        let m = parse_line(line).expect("step_start -> Some");
        assert_eq!(m.kind, MessageKind::Status);
        assert_eq!(m.content, "running");
    }

    #[test]
    fn parse_line_step_finish_returns_none() {
        let line = r#"{"type":"step_finish","part":{"reason":"stop","tokens":{"input":1,"output":2},"cost":0}}"#;
        assert!(parse_line(line).is_none());
    }

    #[test]
    fn parse_line_error_event_is_error_message() {
        let line = r#"{"type":"error","error":{"name":"AbortError","data":{"message":"nope"}}}"#;
        let m = parse_line(line).expect("error -> Some");
        assert_eq!(m.kind, MessageKind::Error);
        assert_eq!(m.content, "nope");
    }

    #[test]
    fn parse_line_error_event_falls_back_to_name() {
        let line = r#"{"type":"error","error":{"name":"AbortError"}}"#;
        let m = parse_line(line).expect("error with name only -> Some");
        assert_eq!(m.content, "AbortError");
    }

    #[test]
    fn parse_line_malformed_returns_none() {
        assert!(parse_line("not json").is_none());
        assert!(parse_line("").is_none());
        assert!(parse_line("[1,2]").is_none());
        assert!(parse_line("42").is_none());
    }

    #[test]
    fn parse_line_unknown_type_returns_none() {
        let line = r#"{"type":"whatever","part":{}}"#;
        assert!(parse_line(line).is_none());
    }

    // -- parse_result_event --

    #[test]
    fn parse_result_event_error_yields_failed_payload() {
        let line = r#"{"type":"error","sessionID":"ses_z","error":{"name":"X","data":{"message":"model unavailable"}}}"#;
        let (output, sid) = parse_result_event(line).expect("error -> Some");
        assert_eq!(output, "model unavailable");
        assert_eq!(sid.as_deref(), Some("ses_z"));
    }

    #[test]
    fn parse_result_event_non_error_returns_none() {
        // Non-error opencode events are not terminal.
        assert!(parse_result_event(
            r#"{"type":"text","part":{"text":"hi"}}"#
        )
        .is_none());
        assert!(parse_result_event(
            r#"{"type":"step_finish","part":{"reason":"stop"}}"#
        )
        .is_none());
    }

    #[test]
    fn parse_result_event_malformed_returns_none() {
        assert!(parse_result_event("not json").is_none());
        assert!(parse_result_event("").is_none());
    }

    #[test]
    fn parse_result_event_missing_session_id_yields_none_sid() {
        let line = r#"{"type":"error","error":{"name":"X","data":{"message":"boom"}}}"#;
        let (output, sid) = parse_result_event(line).expect("error -> Some");
        assert_eq!(output, "boom");
        assert!(sid.is_none());
    }

    // -- build_args --

    #[test]
    fn build_args_minimal() {
        let opts = ExecOptions {
            cwd: PathBuf::from("/tmp"),
            model: None,
            resume_session_id: None,
            timeout: None,
            system_prompt: None,
            mcp_config: None,
        };
        assert_eq!(
            build_args(&opts),
            vec!["run", "--format", "json", "--auto"]
        );
    }

    #[test]
    fn build_args_with_model_and_session() {
        let opts = ExecOptions {
            cwd: PathBuf::from("/tmp"),
            model: Some("aliyun/glm-5.2".into()),
            resume_session_id: Some("ses_abc".into()),
            timeout: None,
            system_prompt: None,
            mcp_config: None,
        };
        assert_eq!(
            build_args(&opts),
            vec!["run", "--format", "json", "--auto", "--model", "aliyun/glm-5.2", "--session", "ses_abc"]
        );
    }

    // -- extract_session_id --

    #[test]
    fn extract_session_id_present() {
        assert_eq!(
            extract_session_id(r#"{"type":"text","sessionID":"ses_x","part":{}}"#).as_deref(),
            Some("ses_x")
        );
    }

    #[test]
    fn extract_session_id_absent() {
        assert!(extract_session_id(r#"{"type":"text","part":{}}"#).is_none());
        assert!(extract_session_id("not json").is_none());
    }
}
