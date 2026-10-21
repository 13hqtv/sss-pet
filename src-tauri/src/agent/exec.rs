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
    // `env_clear` is NOT called — we inherit the parent env so the agent
    // CLI sees the user's login / PATH / config dir (matching multica's
    // "reuse the user's CLI login state" design). `env` then layers on
    // explicit overrides (e.g. agent-specific config dirs / tokens).
    for (k, v) in env {
        command.env(k, v);
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
}
