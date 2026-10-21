//! Task 8: agent backend skeleton — `Backend` trait, `Session`, types,
//! factory, and `exec` helpers. Pure Rust, no GUI, no real agent calls
//! yet (the `execute` impls are PLACEHOLDERS — Tasks 10/11/12 fill them).
//!
//! Design (see `docs/superpowers/specs/2026-10-21-desktop-pet-design.md`,
//! "Agent 接入"): multica-style local-CLI subprocess pattern — one
//! unified `Backend` trait + one backend struct per agent (claude /
//! opencode / codex), each spawning the user's already-installed CLI and
//! parsing its native stream. No API keys — reuses the user's CLI login.
//!
//! See `task-8-brief.md` for the verbatim interfaces this module must
//! expose. Names are FROZEN — Tasks 9–13/15 consume them.

pub mod exec;
// Task 9: shared stream-json line parser → unified `Message` (consumed by
// Task 10's claude backend). Pure Rust, no I/O.
pub mod stream_json;
// Task 10: claude backend — `Backend` impl for `ClaudeBackend` (spawns the
// Claude Code CLI in stream-json mode, parses stdout via `stream_json`).
// Owns the testable `drain_stream` pure-async line-draining helper.
pub mod claude;
// Task 11: opencode backend — `Backend` impl for `OpencodeBackend` (spawns the
// opencode CLI in json-events mode, prompt via stdin, parsed via opencode's
// own json event vocabulary — NOT stream_json, which is claude-specific).
// Owns its testable `drain_stream` pure-async line-draining helper.
pub mod opencode;

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;

/// Whitelist of v1 supported agent types. The factory [`new`] matches
/// these case-sensitively; anything else returns
/// [`BackendError::UnknownAgent`]. Adding a new agent (cursor / copilot /
/// gemini in a later task) = add a string here + a backend struct + a
/// match arm in [`new`].
pub const SUPPORTED: &[&str] = &["claude", "opencode", "codex"];

/// Kind tag for a [`Message`]. Mirrors multica's unified event taxonomy so
/// the webview can render text / thinking / tool activity uniformly across
/// claude / opencode / codex despite their different native stream formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MessageKind {
    /// Plain assistant text (an assistant message / turn output).
    Text,
    /// Chain-of-thought / reasoning trace (e.g. claude `thinking` deltas).
    Thinking,
    /// The agent is invoking a tool (tool name + input pending).
    ToolUse,
    /// A tool finished and returned a result (matches a prior `ToolUse`).
    ToolResult,
    /// Lifecycle status (session started / ended / streaming, etc.).
    Status,
    /// Recoverable or terminal error from the agent or transport.
    Error,
    /// Best-effort log line (debug / info) — usually not shown to the user.
    Log,
}

/// One streamed event from an agent's `execute` session. The webview
/// renders these as they arrive (text / thinking bubbles, tool-use /
/// tool-result chips). `tool` is set for `ToolUse` / `ToolResult`;
/// `call_id` pairs a `ToolResult` with its `ToolUse`. Both are `None`
/// for `Text` / `Thinking` / `Status` / `Error` / `Log`.
#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub kind: MessageKind,
    pub content: String,
    pub tool: Option<String>,
    pub call_id: Option<String>,
}

/// Token + cost accounting for one model used during a session. A session
/// may touch multiple models (e.g. main + a small summarizer), hence the
/// `HashMap<String, TokenUsage>` in [`ExecResult`]. `cost_usd_ticks`
/// is in fixed-point ticks (1 tick = 1e-6 USD) to keep the struct
/// integer-only and avoid f64 drift in serialized history.
#[derive(Debug, Clone)]
pub struct TokenUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub cost_usd_ticks: i64,
}

/// Per-`execute` options. `cwd` is the working dir the agent CLI is
/// spawned in (so file tools resolve relative to the project). `model`
/// overrides the agent's default model. `resume_session_id` is the
/// agent-native session/thread id from a prior turn (mirrors multica's
/// `ResumeSessionID`) — when set, the backend resumes that session
/// instead of starting fresh. `timeout` caps the whole execution.
#[derive(Debug, Clone)]
pub struct ExecOptions {
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub resume_session_id: Option<String>,
    pub timeout: Option<Duration>,
}

/// Final outcome of an `execute` session. `status` is agent-native
/// (e.g. `"ok"` / `"error"` / `"interrupted"`); `output` is the
/// concatenated final text (for convenience — the webview usually
/// renders the streamed `Message`s instead); `session_id` is the
/// agent-native id to persist for [`ExecOptions::resume_session_id`] on
/// the next turn; `usage` maps model name → token accounting.
#[derive(Debug, Clone)]
pub struct ExecResult {
    pub status: String,
    pub output: String,
    pub session_id: Option<String>,
    pub usage: HashMap<String, TokenUsage>,
}

/// A live `execute` session. `messages` streams [`Message`] events as
/// the agent produces them (text / thinking / tool-use / tool-result);
/// `result` resolves once with the final [`ExecResult`] when the session
/// ends. Both are `tokio` channels owned by the caller — the backend
/// holds the sender halves internally and drives them from the spawned
/// subprocess's stdout. Mirrors multica's `Session` struct.
///
/// `Debug` is implemented manually (not derived) because the `tokio`
/// channel receivers don't impl `Debug`; the manual impl prints a stable
/// placeholder so test assert messages (`{res:?}`) compile.
pub struct Session {
    pub messages: tokio::sync::mpsc::Receiver<Message>,
    pub result: tokio::sync::oneshot::Receiver<ExecResult>,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("messages", &"<mpsc::Receiver<Message>>")
            .field("result", &"<oneshot::Receiver<ExecResult>>")
            .finish()
    }
}

/// Per-backend configuration. `executable_path` overrides the PATH
/// lookup ([`exec::resolve`]) when set — corresponds to multica's
/// `MULTICA_<X>_PATH`. `env` is layered on top of the inherited
/// process environment when spawning (see [`exec::spawn`]).
#[derive(Debug, Clone, Default)]
pub struct BackendConfig {
    pub executable_path: Option<PathBuf>,
    pub env: HashMap<String, String>,
}

/// Errors from the agent backend layer. `UnknownAgent` = the factory was
/// asked for an agent not in [`SUPPORTED`]. `Exec` = the spawn / run /
/// (placeholder) failed for a known agent. `Handshake` = the long-lived
/// app-server handshake (codex's `initialize`) failed — reserved for
/// Task 12.
#[derive(Debug)]
pub enum BackendError {
    UnknownAgent(String),
    Exec(String),
    Handshake(String),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendError::UnknownAgent(name) => {
                write!(f, "unknown agent type: {name} (not in SUPPORTED)")
            }
            BackendError::Exec(msg) => write!(f, "agent exec error: {msg}"),
            BackendError::Handshake(msg) => {
                write!(f, "agent handshake error: {msg}")
            }
        }
    }
}

impl std::error::Error for BackendError {}

/// Unified agent backend trait. One impl per supported agent (claude /
/// opencode / codex) — each spawns the user's CLI, parses its native
/// stream into [`Message`]s, and returns a [`Session`] the caller drives
/// to completion.
///
/// `execute` is `async` and the trait is `Send + Sync` so the factory
/// can return `Box<dyn Backend>` (object-safe). Rust 1.97's native
/// `async fn` in traits is NOT object-safe, so this trait is annotated
/// with `#[async_trait]` — it desugars `execute` to
/// `Pin<Box<dyn Future<Output = ...> + Send>>`, restoring object safety.
/// This is the standard solution; see `task-8-brief.md` "Ruling".
#[async_trait]
pub trait Backend: Send + Sync {
    /// Spawn the agent CLI for `prompt` and return a streaming [`Session`].
    /// The session stays live after this returns — the caller drives
    /// `messages` and `result` to completion. Tasks 10/11/12 fill the
    /// claude / opencode / codex bodies; Task 8 leaves them as
    /// placeholders returning `Err(BackendError::Exec("not yet implemented"))`.
    async fn execute(
        &self,
        prompt: &str,
        opts: ExecOptions,
    ) -> Result<Session, BackendError>;
}

// ---------------------------------------------------------------------------
// Placeholder backends — Tasks 10/11/12 replace the `execute` bodies.
// ---------------------------------------------------------------------------

/// Placeholder. Task 10 fills `execute` with the claude CLI subprocess
/// (`claude -p --output-format stream-json --input-format stream-json`,
/// prompt via stdin, stream-json events parsed into `Message`s).
pub struct ClaudeBackend {
    pub cfg: BackendConfig,
}

/// Placeholder. Task 11 fills `execute` with the opencode CLI subprocess
/// (`opencode run --format json --dangerously-skip-permissions`, prompt
/// via stdin, json events parsed into `Message`s).
pub struct OpencodeBackend {
    pub cfg: BackendConfig,
}

/// Placeholder. Task 12 fills `execute` with the codex app-server
/// (`codex app-server --listen stdio://` — long-lived JSON-RPC with
/// `initialize` handshake + turn lifecycle; the heaviest of the three).
pub struct CodexBackend {
    pub cfg: BackendConfig,
}

// `ClaudeBackend`'s and `OpencodeBackend`'s `Backend` impls live in
// `claude.rs` (Task 10) and `opencode.rs` (Task 11) respectively, so the
// stream-json / json-events line-draining + subprocess wiring stays
// colocated with their tests. The structs themselves remain defined here so
// the frozen factory (`new`) and the re-exported names stay stable.

#[async_trait]
impl Backend for CodexBackend {
    async fn execute(
        &self,
        _prompt: &str,
        _opts: ExecOptions,
    ) -> Result<Session, BackendError> {
        Err(BackendError::Exec(
            "codex execute not yet implemented (Task 12)".into(),
        ))
    }
}

/// Factory: build a boxed [`Backend`] for an `agent_type` in
/// [`SUPPORTED`]. Returns [`BackendError::UnknownAgent`] for anything
/// else. `cfg` carries an optional explicit executable path (overrides
/// PATH lookup) and extra env vars. Tasks 9+ call this to obtain a
/// backend, then drive `execute` → `Session` → `messages` / `result`.
pub fn new(agent_type: &str, cfg: BackendConfig) -> Result<Box<dyn Backend>, BackendError> {
    match agent_type {
        "claude" => Ok(Box::new(ClaudeBackend { cfg })),
        "opencode" => Ok(Box::new(OpencodeBackend { cfg })),
        "codex" => Ok(Box::new(CodexBackend { cfg })),
        other => Err(BackendError::UnknownAgent(other.into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn factory_ok_for_supported_agents() {
        assert!(new("claude", BackendConfig::default()).is_ok());
        assert!(new("opencode", BackendConfig::default()).is_ok());
        assert!(new("codex", BackendConfig::default()).is_ok());
    }

    #[test]
    fn factory_err_for_unknown_agent() {
        let res = new("unknown", BackendConfig::default());
        assert!(matches!(res, Err(BackendError::UnknownAgent(_))));
        if let Err(BackendError::UnknownAgent(name)) = res {
            assert_eq!(name, "unknown");
        }
    }

    #[test]
    fn supported_whitelist_lists_all_three() {
        assert!(SUPPORTED.contains(&"claude"));
        assert!(SUPPORTED.contains(&"opencode"));
        assert!(SUPPORTED.contains(&"codex"));
        assert_eq!(SUPPORTED.len(), 3);
    }

    #[test]
    fn resolve_finds_cargo_on_path() {
        // `cargo` is on PATH in any Rust dev env (rustc 1.97 verified).
        assert!(exec::resolve("cargo").is_some());
    }

    #[test]
    fn resolve_returns_none_for_nonexistent_exe() {
        assert!(exec::resolve("definitely_not_a_real_exe_xyz123").is_none());
    }

    #[test]
    fn backend_config_default_is_empty() {
        let cfg = BackendConfig::default();
        assert!(cfg.executable_path.is_none());
        assert!(cfg.env.is_empty());
    }

    #[test]
    fn message_kind_serializes_as_expected() {
        // Smoke-test the serde::Serialize derive on the enum variants.
        let k = MessageKind::ToolUse;
        let s = serde_json::to_string(&k).expect("serialize MessageKind");
        assert_eq!(s, "\"ToolUse\"");
    }

    #[test]
    fn message_serializes_fields() {
        let m = Message {
            kind: MessageKind::Text,
            content: "hello".into(),
            tool: Some("Read".into()),
            call_id: Some("call_42".into()),
        };
        let s = serde_json::to_string(&m).expect("serialize Message");
        assert!(s.contains("\"kind\":\"Text\""));
        assert!(s.contains("\"content\":\"hello\""));
        assert!(s.contains("\"tool\":\"Read\""));
        assert!(s.contains("\"call_id\":\"call_42\""));
    }

    #[test]
    fn backend_error_display_is_informative() {
        assert_eq!(
            BackendError::UnknownAgent("foo".into()).to_string(),
            "unknown agent type: foo (not in SUPPORTED)"
        );
        assert!(std::error::Error::source(&BackendError::Exec("x".into())).is_none());
    }

    /// The last remaining placeholder backend impl (codex — claude landed
    /// in Task 10, opencode in Task 11) must return `BackendError::Exec` so
    /// Task 12 can route the error before its impl lands.
    #[tokio::test]
    async fn codex_placeholder_returns_exec_err() {
        let opts = ExecOptions {
            cwd: std::env::current_dir().unwrap(),
            model: None,
            resume_session_id: None,
            timeout: None,
        };
        let backend = new("codex", BackendConfig::default()).expect("supported");
        let res = backend.execute("hi", opts).await;
        assert!(
            matches!(res, Err(BackendError::Exec(_))),
            "codex placeholder should return Exec, got {res:?}"
        );
    }

    /// `spawn` must succeed for a real command. We spawn `cargo
    /// --version` with an empty env (inherits parent env) and the current
    /// cwd, assert it spawns, then drop the child (tokio reaps it). This
    /// exercises the `Command` + env + cwd + stdin/stdout/stderr piped
    /// wiring without depending on any agent CLI being installed.
    #[tokio::test]
    async fn spawn_runs_a_real_command() {
        let cargo_path = exec::resolve("cargo").expect("cargo on PATH");
        let child = exec::spawn(
            cargo_path,
            vec!["--version".into()],
            HashMap::new(),
            std::env::current_dir().unwrap(),
        );
        assert!(child.is_ok(), "spawn err: {:?}", child.err());
    }
}
