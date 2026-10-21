//! Task 11 real-opencode end-to-end validation. NOT run by default —
//! `#[ignore]`'d so the normal `cargo test` suite stays offline / free.
//! Invoke explicitly:
//!   cargo test --manifest-path src-tauri/Cargo.toml \
//!     --test opencode_real -- --ignored --nocapture
//!
//! Spawns the real `opencode` CLI via `OpencodeBackend::execute`, asks it to
//! reply with the literal token "hello", and asserts:
//!   - the streamed `Message`s include at least one `Text` containing
//!     "hello";
//!   - the final `ExecResult.status == "completed"`;
//!   - the `ExecResult.session_id` is `Some` (opencode stamps every event
//!     with a `ses_…` id).
//!
//! Consumes a small amount of opencode quota (aliyun/glm-5.2). The model is
//! passed explicitly because the project's opencode config declares models
//! but no top-level default — without `opts.model`, opencode has no model to
//! pick and the run hangs. (Note: build_args does NOT pass `--pure`, so
//! plugin / MCP startup adds a few seconds of latency; the generous 120s
//! timeout covers it. If plugin startup ever hangs on a given host, this
//! test times out — the offline `drain_stream` fixture test already covers
//! the parser against a real-captured stream.)

use std::time::Duration;

use swd_pet_lib::agent::{new, BackendConfig, ExecOptions, MessageKind};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real-opencode validation — consumes quota; run explicitly"]
async fn real_opencode_replies_hello() {
    // Skip automatically if opencode isn't installed — `cargo test
    // --ignored` shouldn't hard-fail on a dev machine that lacks the CLI.
    if which::which("opencode").is_err() {
        eprintln!("opencode not on PATH — skipping real-opencode validation");
        return;
    }

    let backend = new("opencode", BackendConfig::default()).expect("opencode backend");
    let opts = ExecOptions {
        cwd: std::env::temp_dir(),
        model: Some("aliyun/glm-5.2".into()),
        resume_session_id: None,
        timeout: Some(Duration::from_secs(120)),
    };

    let mut session = backend
        .execute("Reply with exactly the word: hello", opts)
        .await
        .expect("execute ok");

    let mut text_messages: Vec<String> = Vec::new();
    while let Ok(Some(msg)) =
        tokio::time::timeout(Duration::from_secs(120), session.messages.recv()).await
    {
        if msg.kind == MessageKind::Text {
            text_messages.push(msg.content);
        }
    }

    let result = tokio::time::timeout(Duration::from_secs(120), session.result)
        .await
        .expect("result resolves within 120s")
        .expect("result ok");

    eprintln!("real-opencode text messages: {text_messages:?}");
    eprintln!("real-opencode result: {result:?}");

    assert!(
        text_messages.iter().any(|t| t.contains("hello")),
        "expected a Text message containing 'hello', got {text_messages:?}"
    );
    assert_eq!(result.status, "completed", "status: {result:?}");
    assert!(result.session_id.is_some(), "session_id Some: {result:?}");
}
