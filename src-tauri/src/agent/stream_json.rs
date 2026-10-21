//! Task 9: shared parser for claude-family `stream-json` event lines.
//!
//! Claude Code's `--output-format stream-json` emits one JSON object per
//! stdout line. This module turns each line into a unified [`Message`]
//! (defined in [`super`]) and exposes [`parse_result_event`] for the
//! terminal `result` event that carries the final output + session id.
//!
//! Pure Rust, no I/O — Task 10 (claude backend) wires this to the live
//! subprocess stdout. Field shapes are verified against a real `claude -p
//! --output-format stream-json --verbose` capture (see
//! `tests/fixtures/claude_stream.txt`) and cross-checked against
//! multica's `server/pkg/agent/claude.go` (`claudeSDKMessage`,
//! `claudeContentBlock`).
//!
//! Design ref: `docs/superpowers/specs/2026-10-21-desktop-pet-design.md`
//! ("Agent 接入").

use serde_json::Value;

use super::{Message, MessageKind};

/// Parse one claude stream-json line (a JSON object) into a [`Message`].
///
/// Mapping (see task-9-brief.md, "Interfaces"):
/// - `{"type":"assistant","message":{"content":[...]}}` → first meaningful
///   content item: `text`→`Text`, `thinking`→`Thinking`,
///   `tool_use`→`ToolUse{tool,call_id,content=input as JSON string}`.
/// - `{"type":"user","message":{"content":[...]}}` → first `tool_result`
///   block → `ToolResult{call_id=tool_use_id, content}`. Non-tool-result
///   user lines return `None` (pure echoes aren't worth surfacing).
/// - `{"type":"system","subtype":...}` → `Status{content=subtype}` for
///   surface-worthy subtypes (`init`), `None` for hook noise
///   (`hook_started` / `hook_response`).
/// - `{"type":"log","log":{...}}` → `Log{content=message}`.
/// - `{"type":"result",...}` → `None` here; use [`parse_result_event`].
/// - malformed / non-JSON / unrecognized → `None`.
pub fn parse_line(line: &str) -> Option<Message> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(trimmed).ok()?;
    let obj = value.as_object()?;
    let kind = obj.get("type")?.as_str()?;

    match kind {
        "assistant" => parse_assistant(obj),
        "user" => parse_user(obj),
        "system" => parse_system(obj),
        "log" => parse_log(obj),
        // The terminal `result` event is handled by parse_result_event;
        // surfaced here as None so a generic line-loop doesn't double-emit.
        "result" | "control_request" | "control_response" => None,
        _ => None,
    }
}

/// If `line` is a claude `result` event, return `(result, session_id)`.
///
/// `{"type":"result","result":R,"session_id":S,...}` →
/// `Some((R, Some(S)))`. `session_id` may be absent → `None`. Non-result
/// lines (including malformed JSON) → `None`.
pub fn parse_result_event(line: &str) -> Option<(String, Option<String>)> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(trimmed).ok()?;
    let obj = value.as_object()?;
    // `type` may appear anywhere in the object (claude's result events
    // serialize `type` last in practice); locate it by key, not position.
    let kind = obj.get("type")?.as_str()?;
    if kind != "result" {
        return None;
    }
    let result = obj
        .get("result")
        .and_then(content_to_string)
        .unwrap_or_default();
    let session_id = obj
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some((result, session_id))
}

// ── per-type helpers ──

/// Parse an `assistant` line: walk `message.content[]` and return the FIRST
/// meaningful block. Empty text / unrecognized blocks are skipped; if none
/// of the blocks yield a `Message`, returns `None`.
fn parse_assistant(obj: &serde_json::Map<String, Value>) -> Option<Message> {
    let content = obj
        .get("message")?
        .as_object()?
        .get("content")?
        .as_array()?;
    for block in content {
        let block_obj = block.as_object()?;
        let block_type = block_obj.get("type")?.as_str()?;
        match block_type {
            "text" => {
                let text = block_obj.get("text").and_then(|v| v.as_str()).unwrap_or("");
                if !text.is_empty() {
                    return Some(Message {
                        kind: MessageKind::Text,
                        content: text.to_string(),
                        tool: None,
                        call_id: None,
                    });
                }
            }
            "thinking" => {
                let text = block_obj
                    .get("thinking")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !text.is_empty() {
                    return Some(Message {
                        kind: MessageKind::Thinking,
                        content: text.to_string(),
                        tool: None,
                        call_id: None,
                    });
                }
            }
            "tool_use" => {
                let tool = block_obj
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let call_id = block_obj
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // input serialized as a JSON string (or "" when absent/null).
                let content = match block_obj.get("input") {
                    Some(Value::Null) | None => String::new(),
                    Some(other) => serde_json::to_string(other).unwrap_or_default(),
                };
                return Some(Message {
                    kind: MessageKind::ToolUse,
                    content,
                    tool: if tool.is_empty() { None } else { Some(tool) },
                    call_id: if call_id.is_empty() { None } else { Some(call_id) },
                });
            }
            _ => { /* unrecognized block — skip, keep scanning */ }
        }
    }
    None
}

/// Parse a `user` line: surface the FIRST `tool_result` block as
/// `ToolResult`. Non-tool-result user content (pure text echoes) → `None`.
fn parse_user(obj: &serde_json::Map<String, Value>) -> Option<Message> {
    let content = obj
        .get("message")?
        .as_object()?
        .get("content")?
        .as_array()?;
    for block in content {
        let block_obj = block.as_object()?;
        if block_obj.get("type").and_then(|v| v.as_str()) != Some("tool_result") {
            continue;
        }
        let call_id = block_obj
            .get("tool_use_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let result_content = block_obj
            .get("content")
            .and_then(content_to_string)
            .unwrap_or_default();
        return Some(Message {
            kind: MessageKind::ToolResult,
            content: result_content,
            tool: None,
            call_id: if call_id.is_empty() { None } else { Some(call_id) },
        });
    }
    None
}

/// Parse a `system` line: `init` (and other surface-worthy subtypes) →
/// `Status{content=subtype}`; `hook_started` / `hook_response` are hook
/// noise and return `None`. A system line with no subtype also returns
/// `None` (nothing identifying to surface).
fn parse_system(obj: &serde_json::Map<String, Value>) -> Option<Message> {
    let subtype = obj.get("subtype")?.as_str()?;
    match subtype {
        "hook_started" | "hook_response" => None,
        _ => Some(Message {
            kind: MessageKind::Status,
            content: subtype.to_string(),
            tool: None,
            call_id: None,
        }),
    }
}

/// Parse a `log` line: `log.message` → `Log{content=message}`.
fn parse_log(obj: &serde_json::Map<String, Value>) -> Option<Message> {
    let msg = obj.get("log")?.as_object()?;
    let content = msg.get("message").and_then(|v| v.as_str())?.to_string();
    Some(Message {
        kind: MessageKind::Log,
        content,
        tool: None,
        call_id: None,
    })
}

/// Best-effort coercion of a content field to a display string.
///
/// - JSON string → used verbatim.
/// - JSON array (the Anthropic tool_result content-block list shape:
///   `[{"type":"text","text":"..."}, ...]`) → the `text` fields of any
///   text items joined by `\n`; if no text items, falls back to the JSON
///   serialization of the whole array.
/// - Anything else → its JSON serialization.
fn content_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => {
            let joined: Vec<&str> = items
                .iter()
                .filter_map(|item| {
                    let o = item.as_object()?;
                    if o.get("type").and_then(|v| v.as_str()) == Some("text") {
                        o.get("text").and_then(|v| v.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            if !joined.is_empty() {
                Some(joined.join("\n"))
            } else {
                Some(serde_json::to_string(v).unwrap_or_default())
            }
        }
        _ => Some(serde_json::to_string(v).unwrap_or_default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Absolute path to the committed fixture (one real-shape stream-json
    /// line per row). Built from `CARGO_MANIFEST_DIR` so the test is
    /// independent of the cwd `cargo test` happens to run from.
    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("claude_stream.txt")
    }

    /// Read the fixture into trimmed non-empty lines.
    fn fixture_lines() -> Vec<String> {
        let raw = std::fs::read_to_string(fixture())
            .unwrap_or_else(|e| panic!("read fixture: {e}"));
        raw.lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }

    // -- parse_line: assistant content blocks --

    #[test]
    fn parse_line_assistant_text_block() {
        // Fixture line index 3: assistant message with one text block.
        let lines = fixture_lines();
        let line = &lines[3];
        assert!(line.contains(r#""type":"assistant""#));
        let m = parse_line(line).expect("assistant text -> Some");
        assert_eq!(m.kind, MessageKind::Text);
        assert_eq!(m.content, "hello");
        assert!(m.tool.is_none());
        assert!(m.call_id.is_none());
    }

    #[test]
    fn parse_line_assistant_thinking_block() {
        // Fixture line index 2: assistant message with one thinking block.
        let lines = fixture_lines();
        let line = &lines[2];
        let m = parse_line(line).expect("assistant thinking -> Some");
        assert_eq!(m.kind, MessageKind::Thinking);
        assert_eq!(m.content, "Let me think about the reply.");
        assert!(m.tool.is_none());
        assert!(m.call_id.is_none());
    }

    #[test]
    fn parse_line_assistant_tool_use_block() {
        // Fixture line index 4: assistant message with one tool_use block.
        let lines = fixture_lines();
        let line = &lines[4];
        let m = parse_line(line).expect("assistant tool_use -> Some");
        assert_eq!(m.kind, MessageKind::ToolUse);
        assert_eq!(m.tool.as_deref(), Some("Read"));
        assert_eq!(m.call_id.as_deref(), Some("call-1"));
        // content = input serialized as JSON string.
        assert!(m.content.contains("/tmp/foo.txt"));
        assert!(m.content.contains("file_path"));
    }

    #[test]
    fn parse_line_assistant_first_block_wins_when_multiple() {
        // A single assistant line with [thinking, text] must surface the
        // thinking block first (brief: "FIRST meaningful Message").
        let line = r#"{"type":"assistant","message":{"id":"m","role":"assistant","content":[{"type":"thinking","thinking":"ponder"},{"type":"text","text":"answer"}]},"session_id":"s"}"#;
        let m = parse_line(line).expect("first block wins");
        assert_eq!(m.kind, MessageKind::Thinking);
        assert_eq!(m.content, "ponder");
    }

    #[test]
    fn parse_line_assistant_empty_text_block_skipped() {
        // An empty text block is not meaningful; if it's the only block,
        // the line yields None.
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":""}]}}"#;
        assert!(parse_line(line).is_none());
    }

    #[test]
    fn parse_line_assistant_tool_use_no_input_yields_empty_content() {
        // tool_use with no `input` field → content="" (empty), tool+call_id set.
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"c9","name":"Bash"}]}}"#;
        let m = parse_line(line).expect("tool_use no input -> Some");
        assert_eq!(m.kind, MessageKind::ToolUse);
        assert_eq!(m.tool.as_deref(), Some("Bash"));
        assert_eq!(m.call_id.as_deref(), Some("c9"));
        assert_eq!(m.content, "");
    }

    // -- parse_line: user / tool_result --

    #[test]
    fn parse_line_user_tool_result_block() {
        // Fixture line index 5: user message with one tool_result block.
        let lines = fixture_lines();
        let line = &lines[5];
        let m = parse_line(line).expect("user tool_result -> Some");
        assert_eq!(m.kind, MessageKind::ToolResult);
        assert_eq!(m.call_id.as_deref(), Some("call-1"));
        assert_eq!(m.content, "file contents here");
        assert!(m.tool.is_none());
    }

    #[test]
    fn parse_line_user_non_tool_result_returns_none() {
        // A pure user-text echo is not worth surfacing.
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}"#;
        assert!(parse_line(line).is_none());
    }

    // -- parse_line: system / log / result / malformed --

    #[test]
    fn parse_line_system_init_is_status() {
        // Fixture line index 0: system init.
        let lines = fixture_lines();
        let line = &lines[0];
        let m = parse_line(line).expect("system init -> Status");
        assert_eq!(m.kind, MessageKind::Status);
        assert_eq!(m.content, "init");
    }

    #[test]
    fn parse_line_system_hook_noise_returns_none() {
        // Fixture line index 1: system hook_started (noise) → None.
        let lines = fixture_lines();
        assert!(parse_line(&lines[1]).is_none());
        // Fixture line index 10: system hook_response → None.
        assert!(parse_line(&lines[10]).is_none());
    }

    #[test]
    fn parse_line_log_event_is_log_message() {
        // Fixture line index 9: log event.
        let lines = fixture_lines();
        let line = &lines[9];
        let m = parse_line(line).expect("log -> Log");
        assert_eq!(m.kind, MessageKind::Log);
        assert_eq!(m.content, "stream started");
    }

    #[test]
    fn parse_line_result_event_returns_none() {
        // The terminal result line is handled by parse_result_event, not
        // parse_line — so parse_line yields None for it.
        let lines = fixture_lines();
        let line = &lines[7];
        assert!(line.contains(r#""type":"result""#));
        assert!(parse_line(line).is_none());
    }

    #[test]
    fn parse_line_malformed_non_json_returns_none() {
        // Fixture line index 8: a plain non-JSON string.
        let lines = fixture_lines();
        assert_eq!(lines[8], "this is not json at all");
        assert!(parse_line(&lines[8]).is_none());
    }

    #[test]
    fn parse_line_empty_string_returns_none() {
        assert!(parse_line("").is_none());
    }

    #[test]
    fn parse_line_json_non_object_returns_none() {
        // A JSON array is not a stream event object.
        assert!(parse_line("[1,2,3]").is_none());
        assert!(parse_line("\"hello\"").is_none());
        assert!(parse_line("42").is_none());
    }

    #[test]
    fn parse_line_unrecognized_event_type_returns_none() {
        let line = r#"{"type":"control_request","request_id":"r"}"#;
        assert!(parse_line(line).is_none());
    }

    // -- parse_result_event --

    #[test]
    fn parse_result_event_ok_with_session_id() {
        // Fixture line index 7: result event.
        let lines = fixture_lines();
        let line = &lines[7];
        let (output, session) = parse_result_event(line).expect("result -> Some");
        assert_eq!(output, "done");
        assert_eq!(session.as_deref(), Some("abc-123"));
    }

    #[test]
    fn parse_result_event_without_session_id() {
        let line = r#"{"type":"result","result":"ok","is_error":false}"#;
        let (output, session) = parse_result_event(line).expect("result no session -> Some");
        assert_eq!(output, "ok");
        assert!(session.is_none());
    }

    #[test]
    fn parse_result_event_missing_result_field_yields_empty() {
        // type=="result" but no `result` text → empty output, not None.
        let line = r#"{"type":"result","session_id":"s","is_error":false}"#;
        let (output, session) = parse_result_event(line).expect("result missing field -> Some");
        assert_eq!(output, "");
        assert_eq!(session.as_deref(), Some("s"));
    }

    #[test]
    fn parse_result_event_non_result_line_returns_none() {
        // An assistant line is not a result event.
        let lines = fixture_lines();
        assert!(parse_result_event(&lines[3]).is_none());
    }

    #[test]
    fn parse_result_event_malformed_returns_none() {
        let lines = fixture_lines();
        assert!(parse_result_event(&lines[8]).is_none());
        assert!(parse_result_event("").is_none());
        assert!(parse_result_event("not json").is_none());
    }
}
