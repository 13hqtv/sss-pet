//! Task 10: claude backend — spawns the Claude Code CLI in stream-json
//! mode, writes the prompt to its stdin as a stream-json user message,
//! and parses its stdout into unified [`Message`]s + a final
//! [`ExecResult`]. Mirrors multica's `claudeBackend` pattern
//! (`server/pkg/agent/claude.go`).
//!
//! Refactored for testability: the line-draining logic is pulled into a
//! pure async fn [`drain_stream`] that takes any `tokio::io::AsyncBufRead`
//! reader — tests feed it a `tokio::io::Cursor<Vec<u8>>` of the fixture
//! lines, no subprocess spawned. [`ClaudeBackend::execute`] wires
//! [`drain_stream`] to the spawned claude CLI's buffered stdout.
//!
//! Design ref: `docs/superpowers/specs/2026-10-21-desktop-pet-design.md`
//! ("Agent 接入"). Args / stdin shape sourced from multica's
//! `buildClaudeArgs` (claude.go:1078) and `buildClaudeInput`
//! (claude.go:1149).

use std::collections::HashMap;

use async_trait::async_trait;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use super::exec;
use super::stream_json;
use super::{
    Backend, BackendError, ClaudeBackend, ExecOptions, ExecResult, Message, MessageKind, Session,
};

/// Build the argv for the claude CLI, mirroring multica's `buildClaudeArgs`
/// (claude.go:1078). Desktop-pet is single-user interactive, so we drop
/// multica's `--permission-mode bypassPermissions` / `--disallowedTools
/// AskUserQuestion` (those are for autonomous daemon runs — the desktop
/// pet surfaces its own UI and does not auto-approve tools). Always emits
/// `-p --output-format stream-json --input-format stream-json --verbose`
/// (the `--verbose` is REQUIRED: the claude CLI refuses
/// `--output-format stream-json` under `-p` without it, erroring
/// `Error: When using --print, --output-format=stream-json requires
/// --verbose`; verified against Claude Code 2.1.228). Adds `--model M`
/// when `opts.model` is set and `--resume <id>` when
/// `opts.resume_session_id` is set (matches multica's `--resume` flag at
/// claude.go:1117 — verified against the multica source, not the `-r`
/// short form).
pub(crate) fn build_args(opts: &ExecOptions) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-p".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--input-format".into(),
        "stream-json".into(),
        "--verbose".into(),
    ];
    if let Some(model) = &opts.model {
        args.push("--model".into());
        args.push(model.clone());
    }
    if let Some(id) = &opts.resume_session_id {
        args.push("--resume".into());
        args.push(id.clone());
    }
    if let Some(prompt) = &opts.system_prompt {
        args.push("--append-system-prompt".into());
        args.push(prompt.clone());
    }
    if let Some(mcp) = &opts.mcp_config {
        if let Ok(path) = write_mcp_temp_file(mcp) {
            args.push("--mcp-config".into());
            args.push(path.to_string_lossy().to_string());
        }
    }
    args
}

/// Write the MCP config JSON to a temporary file and return its path.
/// The claude CLI's `--mcp-config` flag expects a file path, not inline JSON.
fn write_mcp_temp_file(mcp_json: &str) -> std::io::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("sss-pet-mcp");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("mcp-{}.json", std::process::id()));
    std::fs::write(&path, mcp_json)?;
    Ok(path)
}

/// Build the stream-json user-message payload written to the claude CLI's
/// stdin. Mirrors multica's `buildClaudeInput` (claude.go:1149) — a
/// single JSON object of shape
/// `{"type":"user","message":{"role":"user","content":[{"type":"text",
/// "text":"<prompt>"}]}}` followed by a newline. Claude's
/// `--input-format stream-json` reads this as the user turn.
pub(crate) fn build_input(prompt: &str) -> String {
    let escaped = json_escape(prompt);
    format!(
        r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"{escaped}"}}]}}}}{chr}"#,
        chr = '\n',
    )
}

/// Minimal JSON string escaper for the prompt body. Handles the
/// characters that are syntactically significant inside a JSON string
/// (`"`, `\`, control chars). serde_json would produce the same output;
/// inlining keeps the wire shape byte-for-byte auditable against
/// multica's `buildClaudeInput` without pulling serde_json onto the
/// stdin-write hot path. Sufficient for arbitrary prompt text.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Drain `reader` line by line, forwarding parsed [`Message`]s to
/// `messages_tx` and a single [`ExecResult`] to `result_tx` when the
/// terminal `result` event arrives (or when the stream ends without
/// one).
///
/// Pure over `reader` — pass a `tokio::io::Cursor<Vec<u8>>` of fixture
/// lines in tests and no subprocess is spawned. [`ClaudeBackend::execute`]
/// wires this to the spawned claude CLI's buffered stdout.
///
/// Behavior:
/// - For each non-empty line, call [`stream_json::parse_line`]; if
///   `Some(msg)`, send to `messages_tx` (best-effort — a dropped receiver
///   just stops the send, draining continues so the result event is
///   still captured).
/// - Call [`stream_json::parse_result_event`]; if `Some((output,
///   session_id))`, send `ExecResult { status:"completed", output,
///   session_id, usage: HashMap::new() }` to `result_tx` and stop. When
///   `output` is empty, fall back to the last seen assistant `Text`
///   content so the caller still gets a non-empty final string.
/// - If the reader reaches EOF with no result event, synthesize an
///   `ExecResult { status:"completed", output: <last text or empty>,
///   session_id: None, .. }` so the caller's `result` always resolves.
///
/// `execute` reaps the spawned child separately (a `child.wait()` task).
/// The non-zero-exit-without-result-event corner case therefore reports
/// "completed" rather than "failed" here — kept simple per the task
/// brief; a future tightening would pass an exit-code signal into
/// `drain_stream` (the brief's "pass a flag" suggestion).
/// Drain `reader` line by line, forwarding parsed [`Message`]s to
/// `messages_tx` and a single [`ExecResult`] to `result_tx` when the
/// terminal `result` event arrives (or when the stream ends without
/// one).
///
/// Pure over `reader` — pass a `tokio::io::BufReader::new(&[u8])` of
/// fixture bytes in tests and no subprocess is spawned.
/// [`ClaudeBackend::execute`] wires this to the spawned claude CLI's
/// buffered stdout.
///
/// Behavior:
/// - For each non-empty line, call [`stream_json::parse_line`]; if
///   `Some(msg)`, send to `messages_tx` (best-effort — a dropped receiver
///   just stops the send, draining continues so the result event is
///   still captured).
/// - Call [`stream_json::parse_result_event`]; if `Some((output,
///   session_id))`, send `ExecResult { status:"completed", output,
///   session_id, usage: HashMap::new() }` to `result_tx` and stop. When
///   `output` is empty, fall back to the last seen assistant `Text`
///   content so the caller still gets a non-empty final string.
/// - If the reader reaches EOF with no result event, synthesize an
///   `ExecResult { status:"completed", output: <last text or empty>,
///   session_id: None, .. }` so the caller's `result` always resolves.
///
/// `execute` reaps the spawned child separately (a `child.wait()` task).
/// The non-zero-exit-without-result-event corner case therefore reports
/// "completed" rather than "failed" here — kept simple per the task
/// brief; a future tightening would pass an exit-code signal into
/// `drain_stream` (the brief's "pass a flag" suggestion).
pub(crate) async fn drain_stream<R>(
    mut reader: R,
    messages_tx: mpsc::Sender<Message>,
    result_tx: oneshot::Sender<ExecResult>,
) where
    R: AsyncBufRead + Unpin,
{
    let mut buf = String::new();
    // Track the last assistant `Text` content so the EOF-without-result
    // fallback (and a result event with an empty `result` field) can
    // still hand the caller a non-empty `output`.
    let mut last_text = String::new();

    loop {
        buf.clear();
        match reader.read_line(&mut buf).await {
            Ok(0) => break, // EOF
            Ok(_) => {}
            // Read error — treat as EOF. The result event, if any,
            // already went out; otherwise the EOF branch synthesizes.
            Err(_) => break,
        }

        let line = buf.trim();
        if line.is_empty() {
            continue;
        }

        // Message-bearing line? Forward ALL blocks (a single assistant
        // line can contain multiple content blocks — e.g. thinking +
        // text — and we want all of them, not just the first).
        let msgs = stream_json::parse_line_all(line);
        if !msgs.is_empty() {
            for msg in msgs {
                if msg.kind == MessageKind::Text && !msg.content.is_empty() {
                    last_text = msg.content.clone();
                }
                let _ = messages_tx.send(msg).await;
            }
            continue;
        }

        // Terminal result event? Emit the final ExecResult and stop.
        if let Some((output, session_id)) = stream_json::parse_result_event(line) {
            let output = if output.is_empty() {
                last_text.clone()
            } else {
                output
            };
            let _ = result_tx.send(ExecResult {
                status: "completed".into(),
                output,
                session_id,
                usage: HashMap::new(),
            });
            return;
        }
        // Anything else (control_request / control_response / unknown /
        // non-JSON) is silently skipped — Task 9's `parse_line` already
        // returns `None` for those, so we just don't surface them.
    }

    // EOF without a result event — synthesize so the caller's `result`
    // always resolves. `execute` may yet observe a non-zero exit and
    // (in a future tightening) override this; for now this is the
    // brief-mandated simple "completed" fallback.
    let _ = result_tx.send(ExecResult {
        status: "completed".into(),
        output: last_text,
        session_id: None,
        usage: HashMap::new(),
    });
}

#[async_trait]
impl Backend for ClaudeBackend {
    async fn execute(
        &self,
        prompt: &str,
        opts: ExecOptions,
    ) -> Result<Session, BackendError> {
        // Resolve the claude binary. `cfg.executable_path` overrides
        // PATH lookup (matches multica's `MULTICA_CLAUDE_PATH`).
        let exe_path = match &self.cfg.executable_path {
            Some(p) => Some(p.clone()),
            None => exec::resolve("claude"),
        };
        let exe_path = exe_path.ok_or_else(|| {
            BackendError::Exec(
                "claude executable not found on PATH (set cfg.executable_path to override)".into(),
            )
        })?;

        let args = build_args(&opts);
        let mut child = exec::spawn(exe_path, args, self.cfg.env.clone(), opts.cwd.clone())
            .map_err(|e| BackendError::Exec(format!("spawn claude: {e}")))?;

        // Take the stdin + stdout pipes (stderr stays piped for the
        // child's lifetime — we ignore it for now; a future task may
        // surface the tail on failure like multica's `stderrTail`).
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| BackendError::Exec("claude stdin pipe not captured".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| BackendError::Exec("claude stdout pipe not captured".into()))?;

        // Write the prompt as a stream-json user message, then close
        // stdin so claude knows the turn's input is complete. Mirrors
        // multica's `writeClaudeInput` (claude.go:1138).
        let input = build_input(prompt);
        if let Err(e) = stdin.write_all(input.as_bytes()).await {
            return Err(BackendError::Exec(format!("write claude stdin: {e}")));
        }
        // Drop stdin — closes the pipe, signaling end-of-input to claude.
        drop(stdin);

        let (messages_tx, messages_rx) = mpsc::channel::<Message>(64);
        let (result_tx, result_rx) = oneshot::channel::<ExecResult>();

        // Spawn the drain task over the buffered stdout. The task owns
        // the stdout pipe + the two senders; on completion (result event
        // or EOF) it sends the final ExecResult and drops the senders.
        let reader = tokio::io::BufReader::new(stdout);
        tokio::spawn(async move {
            drain_stream(reader, messages_tx, result_tx).await;
            // Reap the child after draining so a wedged stdout reader
            // does not leak the process. The non-zero-exit corner case
            // (no result event → "completed" instead of "failed") is
            // the documented simplification.
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

    /// Absolute path to the committed fixture (one real-shape stream-json
    /// line per row). Built from `CARGO_MANIFEST_DIR` so the test is
    /// independent of the cwd `cargo test` happens to run from.
    fn fixture_bytes() -> Vec<u8> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("claude_stream.txt");
        std::fs::read(&path).unwrap_or_else(|e| panic!("read fixture: {e}"))
    }

    /// Drain `bytes` through [`drain_stream`] and collect the [`Message`]s
    /// + [`ExecResult`] for assertion. Wraps the boilerplate so each test
    /// reads as a one-liner.
    ///
    /// `tokio::io::BufReader::new(&[u8])` is the canonical way to get an
    /// `AsyncBufRead` over in-memory bytes — tokio implements `AsyncRead`
    /// directly for `&[u8]`, and `BufReader` adds the `AsyncBufRead`
    /// surface `drain_stream` needs for `read_line`. No subprocess.
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

    /// Feed the fixture lines through `drain_stream`, collect the
    /// `Message`s + the `ExecResult`, assert: at least one
    /// `Message{Text,...}`; the `ExecResult.status == "completed"`;
    /// `output` non-empty; `session_id` is `Some`.
    #[tokio::test]
    async fn drain_stream_fixture_yields_text_messages_and_completed_result() {
        let bytes = fixture_bytes();
        let (msgs, result) = drain(&bytes).await;
        let result = result.expect("result resolves");

        assert!(
            msgs.iter().any(|m| m.kind == MessageKind::Text),
            "expected at least one Text message, got {msgs:?}"
        );
        assert_eq!(
            result.status,
            "completed",
            "status should be completed, got {result:?}"
        );
        assert!(!result.output.is_empty(), "output non-empty: {result:?}");
        assert!(
            result.session_id.is_some(),
            "session_id should be Some, got {result:?}"
        );
    }

    /// A stream with NO result event must still resolve `result` (status
    /// "completed" or "failed", `session_id` None).
    #[tokio::test]
    async fn drain_stream_no_result_event_still_resolves_result() {
        // Two assistant text lines, no terminal `result` event.
        let bytes = b"{\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n\
                     {\"type\":\"assistant\",\"message\":{\"id\":\"m2\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"there\"}]}}\n";

        let (msgs, result) = drain(bytes).await;
        let result = result.expect("result resolves");

        assert!(
            msgs.iter().any(|m| m.kind == MessageKind::Text && m.content == "there"),
            "expected the second text message, got {msgs:?}"
        );
        assert!(
            result.session_id.is_none(),
            "session_id should be None (no result event), got {result:?}"
        );
        assert!(
            result.status == "completed" || result.status == "failed",
            "status should be completed/failed, got {:?}",
            result.status
        );
    }

    // -- pure helpers --

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
        let args = build_args(&opts);
        assert_eq!(
            args,
            vec![
                "-p",
                "--output-format",
                "stream-json",
                "--input-format",
                "stream-json",
                "--verbose",
            ]
        );
    }

    #[test]
    fn build_args_with_model_and_resume() {
        let opts = ExecOptions {
            cwd: PathBuf::from("/tmp"),
            model: Some("claude-opus-4-8".into()),
            resume_session_id: Some("abc-123".into()),
            timeout: None,
            system_prompt: None,
            mcp_config: None,
        };
        let args = build_args(&opts);
        assert_eq!(
            args,
            vec![
                "-p",
                "--output-format",
                "stream-json",
                "--input-format",
                "stream-json",
                "--verbose",
                "--model",
                "claude-opus-4-8",
                "--resume",
                "abc-123",
            ]
        );
    }

    #[test]
    fn build_input_shape_matches_multica() {
        // The wire shape must mirror multica's buildClaudeInput (claude.go:1149).
        let input = build_input("hello");
        assert!(input.ends_with('\n'), "must end with a newline");
        let parsed: serde_json::Value = serde_json::from_str(input.trim()).unwrap();
        assert_eq!(parsed["type"], "user");
        assert_eq!(parsed["message"]["role"], "user");
        assert_eq!(parsed["message"]["content"][0]["type"], "text");
        assert_eq!(parsed["message"]["content"][0]["text"], "hello");
    }

    #[test]
    fn build_input_round_trips_through_json_parser() {
        // Quotes, backslashes, newlines, tabs — all must survive the
        // round-trip through json_escape + serde_json parse.
        let prompt = "he said \"hi\"\nbackslash\\tab\there";
        let input = build_input(prompt);
        let parsed: serde_json::Value = serde_json::from_str(input.trim()).unwrap();
        assert_eq!(parsed["message"]["content"][0]["text"], prompt);
    }
}
