//! Subprocess helpers for the agent backends (Task 8).
//!
//! [`resolve`] does a cross-platform PATH lookup for an executable name —
//! used by the factory / backend impls to find `claude` / `opencode` /
//! `codex` on the user's machine without requiring the user to configure a
//! full path (the `BackendConfig::executable_path` override takes precedence
//! when set — see `agent::new`). Backed by the `which` crate (handles
//! PATHEXT on Windows, executable bit on Unix, etc.).
//!
//! [`spawn`] is a thin wrapper around `tokio::process::Command` that sets
//! env + cwd and pipes stdin/stdout/stderr. The claude / opencode / codex
//! backends (Tasks 10/11/12) call this with their agent-specific argv to
//! get a long-lived `tokio::process::Child` whose stdout they parse as a
//! stream of `Message` events and whose stdin they write the prompt to.
//! Task 8 only ships the helper — the backend `execute` impls are
//! placeholders returning `BackendError::Exec("not yet implemented …")`.

use std::collections::HashMap;
use std::path::PathBuf;
// Note on `creation_flags` below: tokio 1.54 exposes
// `creation_flags(&mut self, flags: u32)` as an *inherent*,
// `#[cfg(windows)]`-gated method on `tokio::process::Command` (it
// internally delegates to std's `CommandExt::creation_flags` on the
// inner `std::process::Command`). So no `use std::os::windows::
// process::CommandExt;` trait import is needed in our code for
// `.creation_flags(...)` to resolve on tokio's `Command` — the
// `cfg(windows)` block in `spawn` is all that's required. (The std
// trait IS still exercised, just inside tokio, not by our call site.)

/// Look up `exe_name` on `PATH` and return its absolute path if found.
///
/// Returns `None` (not a panic) when the executable is absent — callers
/// (the factory / `BackendConfig`-aware backends) decide whether that's
/// a hard error (agent not installed) or a fallback. Backed by the `which`
/// crate so PATHEXT on Windows and the executable bit on Unix are handled
/// for us. `cargo` is a reliable probe target on any Rust dev machine.
pub fn resolve(exe_name: &str) -> Option<PathBuf> {
    which::which(exe_name).ok()
}

/// Spawn `cmd` with `args`, applying `env` (merged on top of the current
/// process environment) and `cwd`, with stdin/stdout/stderr all piped so
/// the caller can write the prompt to stdin and read the stream-json /
/// json-events / app-server JSON-RPC off stdout/stderr.
///
/// Returns the spawned [`tokio::process::Child`] on success. The caller
/// owns the child's lifetime (read until EOF, wait, reap) — Task 8 does
/// not yet call this from any backend; Tasks 10/11/12 wire it in.
///
/// `env` keys override inherited values; an empty `env` inherits the
/// parent environment unchanged.
pub fn spawn(
    cmd: PathBuf,
    args: Vec<String>,
    env: HashMap<String, String>,
    cwd: PathBuf,
) -> std::io::Result<tokio::process::Child> {
    let mut command = tokio::process::Command::new(cmd);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // `kill_on_drop(true)` so a `Child` dropped on an early-return error path
    // (e.g. a backend's stdin-write failure between `spawn` and the
    // drain/wait task taking ownership) kills the process instead of
    // orphaning it. Closes the Task 10 "process leak on early-return" minor
    // for ALL backends (claude / opencode / codex). The success path is
    // unaffected — the drain task owns the child through to `wait()`.
    command.kill_on_drop(true);
    // `env_clear` is NOT called — we inherit the parent env so the agent
    // CLI sees the user's login / PATH / config dir (matching multica's
    // "reuse the user's CLI login state" design). `env` then layers on
    // explicit overrides (e.g. agent-specific config dirs / tokens).
    for (k, v) in env {
        command.env(k, v);
    }
    // Windows: hide the console window that a console subprocess (the
    // claude / codex / opencode CLIs are all console apps) would
    // otherwise pop when spawned from this GUI app.
    // `0x0800_0000` == `CREATE_NO_WINDOW` (winbase.h). Guarded by
    // `#[cfg(windows)]` so non-Windows is byte-for-byte unaffected.
    // `tokio::process::Command` provides `creation_flags` as an
    // inherent Windows-only method (see the note above `spawn`),
    // so this call resolves without a trait import here. Applied in
    // `exec::spawn` (not in each backend) so every spawn routed through
    // it — including Task 3's model-probe spawns — inherits it.
    #[cfg(windows)]
    {
        command.creation_flags(0x0800_0000);
    }
    command.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_finds_cargo() {
        assert!(resolve("cargo").is_some());
    }

    #[test]
    fn resolve_misses_nonexistent() {
        assert!(resolve("definitely_not_a_real_exe_xyz123").is_none());
    }

    /// Functional proof that `spawn` works end-to-end after adding the
    /// `CREATE_NO_WINDOW` flag on Windows: resolve `cargo`, spawn
    /// `cargo --version`, read its stdout, and assert the version string
    /// comes back. If `creation_flags(0x0800_0000)` were set wrong
    /// (bad value / wrong trait) `spawn` would error or yield no output.
    /// On Windows this doubles as the "flag doesn't break spawning"
    /// assertion the brief asks for; on non-Windows it's just a spawn
    /// smoke test (the `#[cfg(windows)]` block is compiled out there).
    #[tokio::test]
    async fn spawn_cargo_version_succeeds_and_yields_version() {
        let cargo = resolve("cargo").expect("cargo must be on PATH for tests");
        let mut child = spawn(
            cargo,
            vec!["--version".to_string()],
            HashMap::new(),
            std::env::current_dir().unwrap(),
        )
        .expect("spawn(cargo --version) should succeed");

        let mut stdout =
            child.stdout.take().expect("stdout should be piped");
        let mut out = String::new();
        use tokio::io::AsyncReadExt;
        stdout
            .read_to_string(&mut out)
            .await
            .expect("read child stdout to EOF");
        // Reap so we don't leak a zombie in the test runner.
        let _ = child.wait().await;
        assert!(
            out.contains("cargo"),
            "expected 'cargo' in `cargo --version` output, got: {out}"
        );
        assert!(!out.is_empty(), "version output should be non-empty");
    }

    /// Code-level proof (Windows-only) that `creation_flags` is
    /// callable on `tokio::process::Command` with the
    /// `CREATE_NO_WINDOW` value without panicking. `tokio::process::
    /// Command` exposes `creation_flags` as an inherent Windows-gated
    /// method (it delegates to std's `CommandExt` on the inner std
    /// `Command`), and does NOT expose the set flag for read-back, so
    /// this trait-method-call-compile-and-run smoke test is the
    /// strongest code-level assertion available short of the
    /// functional spawn test above.
    #[cfg(windows)]
    #[test]
    fn creation_flags_is_callable_on_tokio_command() {
        let mut command =
            tokio::process::Command::new(resolve("cargo").unwrap_or_else(|| {
                std::path::PathBuf::from("cargo")
            }));
        // Must compile + run without panic. If the `#[cfg(windows)]`
        // block in `spawn` were removed or the value changed wrongly,
        // this mirrors the production call.
        command.creation_flags(0x0800_0000);
        // Sanity: the same value the production `spawn` sets.
        // (No spawn here — the functional test above covers spawn.)
    }
}
