//! Task 10 real-claude end-to-end validation. NOT run by default —
//! `#[ignore]`'d so the normal `cargo test` suite stays offline / free.
//! Invoke explicitly:
//!   cargo test --manifest-path src-tauri/Cargo.toml \
//!     --test claude_real -- --ignored --nocapture
//!
//! Spawns the real `claude` CLI via `ClaudeBackend::execute`, asks it to
//! reply with the literal token "hello", and asserts:
//!   - the streamed `Message`s include at least one `Text` containing
//!     "hello";
//!   - the final `ExecResult.status == "completed"`;
//!   - the `ExecResult.session_id` is `Some`.
//!
//! This consumes a small amount of Claude quota — acceptable for a
//! one-shot validation per the task brief.

use std::time::Duration;

use swd_pet_lib::agent::{new, BackendConfig, ExecOptions, MessageKind};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real-claude validation — consumes quota; run explicitly"]
async fn real_claude_replies_hello() {
    // Skip automatically if claude isn't installed — `cargo test --ignored`
    // shouldn't hard-fail on a dev machine that lacks the CLI.
    if which::which("claude").is_err() {
        eprintln!("claude not on PATH — skipping real-claude validation");
        return;
    }

    let backend = new("claude", BackendConfig::default()).expect("claude backend");
    let opts = ExecOptions {
        cwd: std::env::temp_dir(),
        model: None,
        resume_session_id: None,
        timeout: Some(Duration::from_secs(60)),
        system_prompt: None,
        mcp_config: None,
    };

    let mut session = backend
        .execute("Reply with exactly the word: hello", opts)
        .await
        .expect("execute ok");

    let mut text_messages: Vec<String> = Vec::new();
    while let Ok(Some(msg)) =
        tokio::time::timeout(Duration::from_secs(60), session.messages.recv()).await
    {
        if msg.kind == MessageKind::Text {
            text_messages.push(msg.content);
        }
    }

    let result = tokio::time::timeout(Duration::from_secs(60), session.result)
        .await
        .expect("result resolves within 60s")
        .expect("result ok");

    eprintln!("real-claude text messages: {text_messages:?}");
    eprintln!("real-claude result: {result:?}");

    assert!(
        text_messages.iter().any(|t| t.contains("hello")),
        "expected a Text message containing 'hello', got {text_messages:?}"
    );
    assert_eq!(result.status, "completed", "status: {result:?}");
    assert!(result.session_id.is_some(), "session_id Some: {result:?}");
}
