//! MCP (Model Context Protocol) server for the 代办 (todos) feature.
//!
//! When swd-pet.exe is launched with the `mcp-server` argument, it runs
//! this module instead of the Tauri GUI app. The server implements the
//! MCP JSON-RPC 2.0 protocol over stdio, exposing the todo CRUD
//! operations as MCP tools that local agent CLIs (Claude, Codex, etc.)
//! can discover and call.
//!
//! ## Protocol
//!
//! The server reads line-delimited JSON-RPC 2.0 requests from stdin and
//! writes line-delimited responses to stdout. The flow:
//!
//! 1. Client sends `initialize` → server responds with capabilities.
//! 2. Client sends `notifications/initialized` → no response (notification).
//! 3. Client sends `tools/list` → server returns the 5 todo tools.
//! 4. Client sends `tools/call` → server executes the tool, returns text.
//!
//! ## MCP config (for the agent CLI)
//!
//! ```json
//! {
//!   "mcpServers": {
//!     "swd-pet-todos": {
//!       "command": "C:\\path\\to\\swd-pet.exe",
//!       "args": ["mcp-server"]
//!     }
//!   }
//! }
//! ```

use std::io::{self, BufRead, Write};

use serde::Serialize;
use serde_json::{json, Value};

use crate::db;

/// The MCP protocol version we implement.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Run the MCP server. Opens the DB (same one as the GUI app — portable
/// exe dir or %APPDATA% fallback), then enters a read-dispatch-write
/// loop on stdin/stdout. Returns when stdin closes (EOF) or on fatal
/// error.
pub fn run_mcp_server() {
    // Open + migrate the DB (same path as the GUI app — the agent sees
    // the same todos the user sees in the panel).
    let conn = match db::open().and_then(|c| db::migrate(&c).map(|_| c)) {
        Ok(c) => c,
        Err(e) => {
            // Can't proceed — send an error response on stderr (stdout
            // must stay clean for JSON-RPC).
            eprintln!("[mcp] failed to open db: {e}");
            return;
        }
    };

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[mcp] stdin read error: {e}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        // Parse the JSON-RPC request.
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                // Malformed JSON — send a parse error.
                let _ = write_response(
                    &mut stdout,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": {"code": -32700, "message": format!("Parse error: {e}")}
                    }),
                );
                continue;
            }
        };

        // Extract id + method. Notifications (no id) get no response.
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(Value::Null);
        let is_notification = req.get("id").is_none();

        let result = dispatch(&conn, method, &params);

        // Notifications don't get a response.
        if is_notification {
            continue;
        }

        let response = match result {
            Ok(value) => json!({"jsonrpc": "2.0", "id": id, "result": value}),
            Err(code_msg) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": code_msg.0, "message": code_msg.1}
            }),
        };

        if write_response(&mut stdout, &response).is_err() {
            break; // stdout closed — exit
        }
    }
}

/// Write a JSON-RPC response as a single line to stdout, flushed.
fn write_response(stdout: &mut impl Write, resp: &Value) -> io::Result<()> {
    let mut s = serde_json::to_string(resp)?;
    s.push('\n');
    stdout.write_all(s.as_bytes())?;
    stdout.flush()
}

/// Dispatch a JSON-RPC method to the appropriate handler.
/// Returns `Ok(Value)` for success, `Err((code, message))` for error.
fn dispatch(conn: &db::Connection, method: &str, params: &Value) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "swd-pet-todos", "version": "0.1.0"}
        })),
        "notifications/initialized" => Ok(Value::Null), // no-op
        "tools/list" => Ok(json!({"tools": tool_definitions()})),
        "tools/call" => handle_tool_call(conn, params),
        _ => Err((-32601, format!("Method not found: {method}"))),
    }
}

/// The 5 todo tools with their input schemas.
fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "todo_list",
            "description": "查询代办事项列表。可按状态、优先级、责任人筛选。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "status": {"type": "string", "description": "按状态筛选: pending(待办) / in_progress(进行中) / completed(完成) / cancelled(取消)"},
                    "priority": {"type": "string", "description": "按优先级筛选: high / medium / low"},
                    "assignee": {"type": "string", "description": "按责任人筛选"}
                }
            }
        }),
        json!({
            "name": "todo_add",
            "description": "新增一条代办事项。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "代办标题（必填）"},
                    "priority": {"type": "string", "description": "优先级: high / medium / low，默认 medium"},
                    "assignee": {"type": "string", "description": "责任人"},
                    "start_at": {"type": "string", "description": "开始时间 RFC3339 格式"},
                    "end_at": {"type": "string", "description": "结束时间 RFC3339 格式"},
                    "notes": {"type": "string", "description": "备注"}
                },
                "required": ["text"]
            }
        }),
        json!({
            "name": "todo_update",
            "description": "修改代办事项的标题、优先级、责任人、时间、备注。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {"type": "integer", "description": "代办 ID（必填）"},
                    "text": {"type": "string", "description": "新标题"},
                    "priority": {"type": "string", "description": "新优先级"},
                    "assignee": {"type": "string", "description": "新责任人"},
                    "start_at": {"type": "string", "description": "新开始时间 RFC3339"},
                    "end_at": {"type": "string", "description": "新结束时间 RFC3339"},
                    "notes": {"type": "string", "description": "新备注"}
                },
                "required": ["id", "text"]
            }
        }),
        json!({
            "name": "todo_set_status",
            "description": "设置代办事项的状态。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {"type": "integer", "description": "代办 ID（必填）"},
                    "status": {"type": "string", "description": "新状态: pending(待办) / in_progress(进行中) / completed(完成) / cancelled(取消)，必填"}
                },
                "required": ["id", "status"]
            }
        }),
        json!({
            "name": "todo_delete",
            "description": "删除一条代办事项。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {"type": "integer", "description": "代办 ID（必填）"}
                },
                "required": ["id"]
            }
        }),
    ]
}

/// Handle a `tools/call` request. Extracts the tool name + arguments,
/// dispatches to the appropriate db function, wraps the result as MCP
/// text content.
fn handle_tool_call(conn: &db::Connection, params: &Value) -> Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or((-32602, "missing 'name' in tools/call params".to_string()))?;
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or(json!({}));

    match name {
        "todo_list" => tool_todo_list(conn, &args),
        "todo_add" => tool_todo_add(conn, &args),
        "todo_update" => tool_todo_update(conn, &args),
        "todo_set_status" => tool_todo_set_status(conn, &args),
        "todo_delete" => tool_todo_delete(conn, &args),
        _ => Err((-32602, format!("unknown tool: {name}"))),
    }
}

/// Wrap a text result as MCP content (the standard `tools/call` result shape).
fn text_result(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}]})
}

/// Wrap a serializable value as MCP text content (JSON-serialized).
fn json_result(value: &impl Serialize) -> Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"));
    text_result(text)
}

fn get_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(String::from)
}

fn get_i64(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(|v| v.as_i64())
}

// -- Tool implementations --

fn tool_todo_list(conn: &db::Connection, args: &Value) -> Result<Value, (i64, String)> {
    let mut todos = db::todo_list(conn).map_err(|e| (-32603, format!("todo_list: {e}")))?;

    // Optional filters
    if let Some(status) = get_str(args, "status") {
        todos.retain(|t| t.status == status);
    }
    if let Some(priority) = get_str(args, "priority") {
        todos.retain(|t| t.priority == priority);
    }
    if let Some(assignee) = get_str(args, "assignee") {
        todos.retain(|t| t.assignee.as_deref() == Some(assignee.as_str()));
    }

    Ok(json_result(&todos))
}

fn tool_todo_add(conn: &db::Connection, args: &Value) -> Result<Value, (i64, String)> {
    let text = get_str(args, "text").ok_or((-32602, "missing required field: text".to_string()))?;
    let priority = get_str(args, "priority").unwrap_or_else(|| "medium".to_string());
    let assignee = get_str(args, "assignee");
    let start_at = get_str(args, "start_at");
    let end_at = get_str(args, "end_at");
    let notes = get_str(args, "notes");

    let id = db::todo_add(
        conn,
        &text,
        &priority,
        assignee.as_deref(),
        start_at.as_deref(),
        end_at.as_deref(),
        notes.as_deref(),
    )
    .map_err(|e| (-32603, format!("todo_add: {e}")))?;

    Ok(text_result(format!("已创建代办事项，ID = {id}")))
}

fn tool_todo_update(conn: &db::Connection, args: &Value) -> Result<Value, (i64, String)> {
    let id = get_i64(args, "id").ok_or((-32602, "missing required field: id".to_string()))?;
    let text = get_str(args, "text").ok_or((-32602, "missing required field: text".to_string()))?;
    let priority = get_str(args, "priority").unwrap_or_else(|| "medium".to_string());
    let assignee = get_str(args, "assignee");
    let start_at = get_str(args, "start_at");
    let end_at = get_str(args, "end_at");
    let notes = get_str(args, "notes");

    db::todo_update(
        conn,
        id,
        &text,
        &priority,
        assignee.as_deref(),
        start_at.as_deref(),
        end_at.as_deref(),
        notes.as_deref(),
    )
    .map_err(|e| (-32603, format!("todo_update: {e}")))?;

    Ok(text_result(format!("已更新代办事项 {id}")))
}

fn tool_todo_set_status(conn: &db::Connection, args: &Value) -> Result<Value, (i64, String)> {
    let id = get_i64(args, "id").ok_or((-32602, "missing required field: id".to_string()))?;
    let status = get_str(args, "status").ok_or((-32602, "missing required field: status".to_string()))?;

    // Validate status
    if !["pending", "in_progress", "completed", "cancelled"].contains(&status.as_str()) {
        return Err((-32602, format!("invalid status: {status}. Must be one of: pending, in_progress, completed, cancelled")));
    }

    db::todo_set_status(conn, id, &status)
        .map_err(|e| (-32603, format!("todo_set_status: {e}")))?;

    Ok(text_result(format!("已将代办事项 {id} 状态设置为 {status}")))
}

fn tool_todo_delete(conn: &db::Connection, args: &Value) -> Result<Value, (i64, String)> {
    let id = get_i64(args, "id").ok_or((-32602, "missing required field: id".to_string()))?;

    db::todo_delete(conn, id)
        .map_err(|e| (-32603, format!("todo_delete: {e}")))?;

    Ok(text_result(format!("已删除代办事项 {id}")))
}
