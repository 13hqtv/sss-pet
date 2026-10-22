//! Task 3: per-agent model discovery — `opencode` / `claude` / `codex`.
//!
//! Surfaces [`list_models`], the per-agent probe a "new conversation" UI calls
//! to populate its model picker (design: 改动 1 / 模型探测 table in
//! `docs/superpowers/specs/2026-10-22-model-cwd-streaming-design.md`).
//! Each agent has its own discovery mechanism, sourced from multica
//! (`server/pkg/agent/{claude_models.go, thinking.go, models.go}`):
//!
//! | Agent    | Probe                                                          | Parser                         |
//! | opencode | `opencode models` (subcommand exists; one `provider/model` per line) | [`parse_opencode_models`] (mirrors multica `parseOpenCodeModels` non-verbose path) |
//! | claude   | stream-json `list_models` control_request over a discovery-only `claude -p --verbose --input-format stream-json --output-format stream-json --strict-mcp-config` session | [`parse_claude_control_response`] (mirrors multica `parseClaudeModelCatalog` + `claudeControlResponse`) |
//! | codex    | one-shot `codex debug models` JSON                            | [`parse_codex_debug_models`] (mirrors multica `parseCodexModelCatalog` + `codexDebugModelsResponse`); [`codex_static_models`] final fallback |
//!
//! **All probe spawns route through [`super::exec::spawn`]** so they inherit
//! the `CREATE_NO_WINDOW` flag (改动 2) — the same path the three backends
//! (claude / opencode / codex `execute`) use for their agent spawns.
//!
//! **Failure contract:** any error / empty / unknown provider →
//! `Ok(vec![])`. The frontend offers manual text entry when the list is
//! empty (design: 兜底 — model 可选，探测失败能手填). [`list_models`]
//! therefore never returns `Err` for a probe failure — only `Ok(vec![])`.

use std::path::PathBuf;

use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::time::{timeout, Duration};

use super::exec;
use super::{BackendConfig, BackendError};

/// The `request_id` labelling our `list_models` control_request so the reply
/// can be picked out of the stream. Mirrors multica's
/// `claudeListModelsRequestID` (claude_models.go:47) verbatim — keeping the
/// same id is not load-bearing (claude answers a discovery-only session in a
/// single stdout line today) but makes the wire shape auditable against the
/// multica reference.
const CLAUDE_LIST_MODELS_REQUEST_ID: &str = "multica-list-models";

/// Timeout for one claude discovery round trip. multica uses 20s
/// (`claudeListModelsTimeout`, claude_models.go:58) because the failure it
/// exists for is not a slow answer but no answer (a wedged CLI / keychain
/// prompt). We match.
const CLAUDE_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// Timeout for `opencode models`. multica bumped its cap to 15s on 1.15+
/// (the hosted free-model catalog sync can take ~6s; see models.go:786).
const OPENCODE_PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Timeout for `codex debug models`. multica's `codexLiveCatalogTimeout` is
/// 15s (thinking.go:381); we match.
const CODEX_PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Per-agent model discovery. Dispatches by `provider`:
/// - `opencode` → spawn `opencode models`, parse stdout → model IDs.
/// - `claude` → spawn a discovery stream-json session, send the
///   `list_models` control_request, parse the `control_response` reply
///   → model IDs.
/// - `codex` → spawn `codex debug models`, parse the JSON response →
///   model IDs; [`codex_static_models`] final fallback on any failure.
/// - unknown provider → `Ok(vec![])`.
///
/// **Any error / empty → `Ok(vec![])`** — the frontend offers manual text
/// entry when the list is empty (design: 兜底). Never returns `Err` for a
/// probe failure.
pub async fn list_models(
    provider: &str,
    cfg: &BackendConfig,
) -> Result<Vec<String>, BackendError> {
    match provider {
        "opencode" => Ok(probe_opencode_models(cfg).await),
        "claude" => Ok(probe_claude_models(cfg).await),
        "codex" => Ok(probe_codex_models(cfg).await),
        _ => Ok(vec![]),
    }
}

/// Resolve the binary path. `cfg.executable_path` overrides the PATH lookup
/// (matches the three backends' `execute` wiring + multica's
/// `cfg.ExecutablePath`).
fn resolve_exe(name: &str, cfg: &BackendConfig) -> Option<PathBuf> {
    match &cfg.executable_path {
        Some(p) => Some(p.clone()),
        None => exec::resolve(name),
    }
}

/// Spawn `opencode models`, read stdout, parse with [`parse_opencode_models`].
/// Returns empty on any failure (binary missing, spawn fail, timeout, parse
/// failure, empty list). Mirrors multica's `discoverOpenCodeModels` 1.x
/// non-verbose path (models.go:815-819) — we do not pass `--verbose` because
/// the desktop-pet does not surface reasoning-level pickers in v0.1.0
/// (plain `provider/model` rows suffice).
async fn probe_opencode_models(cfg: &BackendConfig) -> Vec<String> {
    let exe = match resolve_exe("opencode", cfg) {
        Some(p) => p,
        None => return vec![],
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut child = match exec::spawn(
        exe,
        vec!["models".into()],
        cfg.env.clone(),
        cwd,
    ) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut stdout = match child.stdout.take() {
        Some(s) => s,
        None => return vec![],
    };
    let mut buf = String::new();
    let read_result = timeout(OPENCODE_PROBE_TIMEOUT, stdout.read_to_string(&mut buf)).await;
    let _ = child.wait().await; // reap so we don't leak a zombie
    match read_result {
        Ok(Ok(_)) => parse_opencode_models(&buf),
        _ => vec![],
    }
}

/// Spawn a discovery-only claude stream-json session, write the
/// `list_models` control_request to stdin, read stdout, parse the
/// `control_response` reply. Returns empty on any failure. Mirrors
/// multica's `runClaudeListModels` (claude_models.go:241-262): `--print
/// --verbose --input-format stream-json --output-format stream-json
/// --strict-mcp-config` + the control_request payload on stdin + close
/// stdin → claude answers + exits 0 (no user message is ever written, so
/// no model is invoked and nothing is billed).
async fn probe_claude_models(cfg: &BackendConfig) -> Vec<String> {
    let exe = match resolve_exe("claude", cfg) {
        Some(p) => p,
        None => return vec![],
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let args = claude_list_models_args();
    let mut child = match exec::spawn(exe, args, cfg.env.clone(), cwd) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut stdin = match child.stdin.take() {
        Some(s) => s,
        None => return vec![],
    };
    let mut stdout = match child.stdout.take() {
        Some(s) => s,
        None => return vec![],
    };
    // Write the control_request + close stdin so claude takes EOF as
    // end-of-session, answers the request, and exits 0.
    let req = claude_list_models_request();
    if stdin.write_all(req.as_bytes()).await.is_err() {
        drop(stdin);
        let _ = child.wait().await;
        return vec![];
    }
    drop(stdin);
    let mut buf = String::new();
    let read_result = timeout(CLAUDE_PROBE_TIMEOUT, stdout.read_to_string(&mut buf)).await;
    let _ = child.wait().await;
    match read_result {
        Ok(Ok(_)) => parse_claude_control_response(&buf),
        _ => vec![],
    }
}

/// Spawn `codex debug models`, parse the JSON response. Falls back to
/// [`codex_static_models`] on any failure (binary missing, spawn fail,
/// timeout, parse failure, empty list). Mirrors multica's
/// `discoverCodexCatalog` (thinking.go:309-349), minus the version gate —
/// we just try the command; older codex prints "unknown command" → parse
/// fails → static fallback. The `--bundled` offline retry multica does is
/// folded into the static fallback (a bundled snapshot is what
/// [`codex_static_models`] already is).
async fn probe_codex_models(cfg: &BackendConfig) -> Vec<String> {
    let exe = match resolve_exe("codex", cfg) {
        Some(p) => p,
        None => return codex_static_models(),
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut child = match exec::spawn(
        exe,
        vec!["debug".into(), "models".into()],
        cfg.env.clone(),
        cwd,
    ) {
        Ok(c) => c,
        Err(_) => return codex_static_models(),
    };
    let mut stdout = match child.stdout.take() {
        Some(s) => s,
        None => return codex_static_models(),
    };
    let mut buf = String::new();
    let read_result = timeout(CODEX_PROBE_TIMEOUT, stdout.read_to_string(&mut buf)).await;
    let _ = child.wait().await;
    match read_result {
        Ok(Ok(_)) => {
            let parsed = parse_codex_debug_models(&buf);
            if parsed.is_empty() {
                codex_static_models()
            } else {
                parsed
            }
        }
        _ => codex_static_models(),
    }
}

// ── argv / request builders ──

/// Build the argv for a discovery-only claude stream-json session. Mirrors
/// multica's `claudeListModelsArgs` (claude_models.go:36-42) verbatim:
/// `--print --verbose --input-format stream-json --output-format
/// stream-json --strict-mcp-config`. `--strict-mcp-config` without any
/// `--mcp-config` resolves to "no MCP servers at all" — discovery has no
/// use for the user's servers and every reason not to boot them (a stdio
/// server that spawns a container would make enumerating a model list
/// arbitrarily slow and side-effecting).
pub(crate) fn claude_list_models_args() -> Vec<String> {
    vec![
        "--print".into(),
        "--verbose".into(),
        "--input-format".into(),
        "stream-json".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--strict-mcp-config".into(),
    ]
}

/// Build the stream-json `list_models` control_request payload written to
/// claude's stdin. Mirrors multica's `runClaudeListModels`
/// (claude_models.go:245-252): a single JSON object of shape
/// `{"type":"control_request","request_id":"multica-list-models",
/// "request":{"subtype":"list_models"}}` followed by a newline. Closing
/// stdin after writing tells claude the session is over → it answers the
/// control_request and exits 0.
pub(crate) fn claude_list_models_request() -> String {
    format!(
        r#"{{"type":"control_request","request_id":"{rid}","request":{{"subtype":"list_models"}}}}{chr}"#,
        rid = CLAUDE_LIST_MODELS_REQUEST_ID,
        chr = '\n',
    )
}

// ── pure parsers ──

/// Parse `opencode models` stdout (one `provider/model` per line). Mirrors
/// multica's `parseOpenCodeModels` (models.go:872-910) for the non-verbose
/// path: take the first whitespace-separated token of each line, keep it
/// iff it contains `/` and is not all-uppercase (skips the
/// `PROVIDER/MODEL` header row). Blank / JSON / bare-id lines are skipped.
/// De-duplicates by ID.
pub(crate) fn parse_opencode_models(stdout: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // First whitespace-separated token is the model id. multica's
        // `parseOpenCodeModelIDLine` (models.go:912-929) does the same.
        let first = match trimmed.split_whitespace().next() {
            Some(s) => s,
            None => continue,
        };
        // Skip JSON-object / array / quoted lines — verbose mode appends a
        // pretty-printed JSON object after each id, and that object's own
        // lines (starting with `{` / `[` / `"`) are not model ids.
        if first.starts_with('{') || first.starts_with('[') || first.starts_with('"') {
            continue;
        }
        // A valid opencode model id is `provider/model` (e.g.
        // `opencode/beam-free`, `alibaba-cn/glm-5.2`). A bare id with no
        // `/` is not an opencode model id (multica's parser returns "" for
        // ids without a `/`).
        if !first.contains('/') {
            continue;
        }
        // Skip the `PROVIDER/MODEL` header row — multica's parser skips
        // all-uppercase ids (models.go:925-927).
        if first == first.to_uppercase() {
            continue;
        }
        if seen.insert(first.to_string()) {
            out.push(first.to_string());
        }
    }
    out
}

/// Parse the stdout of a discovery claude stream-json session for the
/// `control_response` reply to our `list_models` control_request. Mirrors
/// multica's `parseClaudeModelCatalog` (claude_models.go:270-301) +
/// `claudeControlResponse` (claude_models.go:154-164). Iterates each line,
/// parses as the `claudeControlResponse` envelope
/// (`{type, response: {subtype, request_id, error, response: {models:
/// [claudeModelInfo]}}}`), picks the one whose `request_id ==
/// "multica-list-models"` and `subtype == "success"`, returns the
/// de-duplicated `resolvedModel` of each row (falling back to `value` when
/// `resolvedModel` is empty — mirrors multica's `claudeModelID`).
///
/// `stream_json::parse_line` deliberately returns `None` for
/// `control_response` events (Task 9), so this is a SEPARATE parser —
/// per the task brief + multica's own `claudeControlResponse` type.
///
/// Error / unsupported / non-success subtype / no matching response →
/// empty Vec.
pub(crate) fn parse_claude_control_response(stdout: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Non-JSON lines (the discovery session emits `system` hook events
        // before the control_response) are skipped — multica does the same
        // (claude_models.go:277-279 "Non-JSON and unrelated lines are
        // skipped rather than treated as corruption").
        let value: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Only a `control_response` top-level type carries our reply.
        if value.get("type").and_then(|v| v.as_str()) != Some("control_response") {
            continue;
        }
        let response = match value.get("response") {
            Some(r) => r,
            None => continue,
        };
        let request_id = response
            .get("request_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if request_id != CLAUDE_LIST_MODELS_REQUEST_ID {
            // A control_response for a different request_id is not our
            // reply; keep scanning.
            continue;
        }
        let subtype = response
            .get("subtype")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if subtype != "success" {
            // multica's parser: subtype != "success" is a probe failure
            // (e.g. "Unsupported control request subtype: list_models" on
            // older CLIs) → return empty so the caller falls back to
            // manual entry (claude_models.go:283-296).
            return Vec::new();
        }
        // The doubled `response.response` nesting is claude's shape, not a
        // transcription slip (multica `claudeControlResponse`, claude_models.
        // go:154-164) — the outer `response` is the envelope
        // (subtype/request_id/error), the inner is the payload.
        if let Some(models) = response
            .get("response")
            .and_then(|r| r.get("models"))
            .and_then(|m| m.as_array())
        {
            for m in models {
                // multica's claudeModelID (claude_models.go:379-384):
                // resolvedModel (trimmed) || value (trimmed).
                let resolved = m
                    .get("resolvedModel")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let value_field =
                    m.get("value").and_then(|v| v.as_str()).unwrap_or("");
                let id = if !resolved.is_empty() {
                    resolved
                } else {
                    value_field
                };
                if id.is_empty() {
                    continue;
                }
                if seen.insert(id.to_string()) {
                    out.push(id.to_string());
                }
            }
        }
        // Found our reply — return. (multica's parser returns on the first
        // matching line; claude answers a discovery-only session in a single
        // stdout line today.)
        return out;
    }
    Vec::new()
}

/// Parse `codex debug models` stdout (a single JSON object
/// `{"models":[{"slug":"...","display_name":"...","visibility":"...",
/// ...}]}`) into a list of model slugs. Mirrors multica's
/// `parseCodexModelCatalog` (thinking.go:400-427): keep `slug` iff non-empty
/// AND `visibility != "hide"`. Tries the whole stdout as one JSON object
/// first (the common single-line case), then falls back to line-by-line
/// (handles a banner line preceding the JSON). Empty/garbage/non-JSON →
/// empty Vec.
pub(crate) fn parse_codex_debug_models(stdout: &str) -> Vec<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    // Try the whole stdout as one JSON object first (the common case —
    // `codex debug models` 0.122+ emits one JSON line).
    if let Some(models) = parse_codex_models_value(trimmed) {
        return models;
    }
    // Fall back to line-by-line — handles a banner line preceding the JSON
    // (defensive against a future codex version that prints a version banner
    // on stdout before the JSON object).
    for line in trimmed.lines() {
        let l = line.trim();
        if l.is_empty() || !l.starts_with('{') {
            continue;
        }
        if let Some(models) = parse_codex_models_value(l) {
            return models;
        }
    }
    Vec::new()
}

/// Extract the visible-model slugs from a single JSON object shaped like
/// `{"models":[{"slug":"...","visibility":"...",...}]}`. Returns `None` on
/// any shape mismatch (non-object, no `models` array, etc.).
fn parse_codex_models_value(s: &str) -> Option<Vec<String>> {
    let value: serde_json::Value = serde_json::from_str(s).ok()?;
    let models = value.get("models")?.as_array()?;
    let mut out: Vec<String> = Vec::new();
    for m in models {
        let slug = m.get("slug").and_then(|v| v.as_str()).unwrap_or("");
        if slug.is_empty() {
            continue;
        }
        // multica's parseCodexModelCatalog: visibility == "hide" → skipped
        // (thinking.go:407). Anything else (list / unset) is kept.
        let visibility = m.get("visibility").and_then(|v| v.as_str()).unwrap_or("");
        if visibility == "hide" {
            continue;
        }
        out.push(slug.to_string());
    }
    Some(out)
}

/// Static codex model fallback when `codex debug models` is unavailable or
/// returns an empty/garbage result. Mirrors multica's `codexStaticModels()`
/// (models.go:621-670) — only the IDs, not the thinking metadata (the
/// desktop-pet does not surface reasoning-level pickers in v0.1.0). The
/// snapshot reflects a recent bundled catalog; if a saved model is absent
/// from this list it is still passed to the CLI as-is (the catalog is a
/// picker convenience, not a gate — multica's `Catalog.Verified` / MUL-7691
/// contract). Order matches multica (flagship `gpt-6-astra` first).
pub(crate) fn codex_static_models() -> Vec<String> {
    vec![
        "gpt-6-astra".into(),
        "gpt-6.1-sol".into(),
        "gpt-6-sol".into(),
        "gpt-6-luna".into(),
        "gpt-5.6-sol".into(),
        "gpt-5.6-terra".into(),
        "gpt-5.6-luna".into(),
        "gpt-5.5".into(),
        "gpt-5.4".into(),
        "gpt-5.4-mini".into(),
        "gpt-5.3-codex".into(),
        "gpt-5.2".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ── claude_list_models_args (multica wire shape) ──

    #[test]
    fn claude_list_models_args_matches_multica() {
        // Mirrors multica's `claudeListModelsArgs` (claude_models.go:36-42)
        // verbatim — the argv a real `claude` discovery invocation receives.
        let args = claude_list_models_args();
        assert_eq!(
            args,
            vec![
                "--print",
                "--verbose",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--strict-mcp-config",
            ]
        );
    }

    // ── claude_list_models_request (multica wire shape) ──

    #[test]
    fn claude_list_models_request_shape_matches_multica() {
        // Mirrors multica's `runClaudeListModels` payload
        // (claude_models.go:245-252).
        let req = claude_list_models_request();
        assert!(req.ends_with('\n'), "must end with a newline");
        let parsed: serde_json::Value =
            serde_json::from_str(req.trim()).expect("valid JSON");
        assert_eq!(parsed["type"], "control_request");
        assert_eq!(parsed["request_id"], "multica-list-models");
        assert_eq!(parsed["request"]["subtype"], "list_models");
    }

    // ── parse_opencode_models ──

    #[test]
    fn parse_opencode_models_extracts_provider_slash_model_ids() {
        // Real `opencode models` 1.18.30 shape: one `provider/model` per line.
        let stdout = "opencode/beam-free\nopencode/big-pickle\nalibaba-cn/glm-5.2\n";
        let models = parse_opencode_models(stdout);
        assert_eq!(
            models,
            vec![
                "opencode/beam-free".to_string(),
                "opencode/big-pickle".to_string(),
                "alibaba-cn/glm-5.2".to_string(),
            ]
        );
    }

    #[test]
    fn parse_opencode_models_skips_header_row() {
        // multica's `parseOpenCodeModelIDLine` skips the `PROVIDER/MODEL`
        // header (all-uppercase).
        let stdout = "PROVIDER/MODEL\nopencode/beam-free\n";
        let models = parse_opencode_models(stdout);
        assert_eq!(models, vec!["opencode/beam-free".to_string()]);
    }

    #[test]
    fn parse_opencode_models_skips_lines_without_slash() {
        // A bare id (no `/`) is not a valid opencode model id.
        let stdout = "some-bare-id\nopencode/beam-free\n";
        let models = parse_opencode_models(stdout);
        assert_eq!(models, vec!["opencode/beam-free".to_string()]);
    }

    #[test]
    fn parse_opencode_models_skips_json_object_lines() {
        // Verbose mode appends a JSON object after each ID; we only want the
        // ID line. multica's parser handles this by checking the first token
        // — we only take the ID line and skip any line whose first token
        // starts with `{` / `[` / `"`.
        let stdout = "opencode/beam-free\n{\"reasoning\":true}\nalibaba-cn/glm-5.2\n";
        let models = parse_opencode_models(stdout);
        assert_eq!(
            models,
            vec![
                "opencode/beam-free".to_string(),
                "alibaba-cn/glm-5.2".to_string(),
            ]
        );
    }

    #[test]
    fn parse_opencode_models_deduplicates() {
        let stdout = "opencode/beam-free\nopencode/beam-free\nopencode/beam-free\n";
        let models = parse_opencode_models(stdout);
        assert_eq!(models, vec!["opencode/beam-free".to_string()]);
    }

    #[test]
    fn parse_opencode_models_garbage_returns_empty() {
        assert!(parse_opencode_models("").is_empty());
        assert!(parse_opencode_models("garbage no slash here\nmore garbage").is_empty());
        assert!(parse_opencode_models("PROVIDER/MODEL\n").is_empty());
    }

    // ── parse_claude_control_response ──

    /// Real claude 2.1.228 control_response shape (captured). Includes
    /// preceding `system` hook events that the discovery session emits
    /// before the control_response — the parser must skip those and pick
    /// out the reply by `request_id`.
    #[test]
    fn parse_claude_control_response_extracts_resolved_models() {
        let stdout = "{\"type\":\"system\",\"subtype\":\"hook_started\",\"hook_id\":\"abc\"}\n\
            {\"type\":\"system\",\"subtype\":\"hook_response\",\"hook_id\":\"abc\"}\n\
            {\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"multica-list-models\",\"response\":{\"models\":[\
                {\"value\":\"default\",\"resolvedModel\":\"claude-opus-4-8[1m]\",\"displayName\":\"Default (recommended)\",\"description\":\"Use the default model (currently Opus 4.8 (1M context))\"},\
                {\"value\":\"opus\",\"resolvedModel\":\"claude-opus-4-8\",\"displayName\":\"claude-opus-4-8\",\"description\":\"Custom Opus model\"},\
                {\"value\":\"sonnet\",\"resolvedModel\":\"claude-opus-4-8\",\"displayName\":\"claude-opus-4-8\",\"description\":\"Custom Sonnet model\"},\
                {\"value\":\"haiku\",\"resolvedModel\":\"claude-opus-4-8\",\"displayName\":\"claude-opus-4-8\",\"description\":\"Custom Haiku model\"}\
            ]}}}\n";
        let models = parse_claude_control_response(stdout);
        // De-duplicated by resolvedModel (multica's claudeModelID): default
        // -> claude-opus-4-8[1m], opus/sonnet/haiku -> claude-opus-4-8.
        assert_eq!(
            models,
            vec![
                "claude-opus-4-8[1m]".to_string(),
                "claude-opus-4-8".to_string(),
            ]
        );
    }

    #[test]
    fn parse_claude_control_response_falls_back_to_value_when_resolved_empty() {
        // multica's claudeModelID: resolvedModel || value. A row with no
        // resolvedModel falls back to its picker token.
        let stdout = "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"multica-list-models\",\"response\":{\"models\":[\
            {\"value\":\"custom-token\",\"resolvedModel\":\"\",\"displayName\":\"Custom\"}\
        ]}}}\n";
        let models = parse_claude_control_response(stdout);
        assert_eq!(models, vec!["custom-token".to_string()]);
    }

    #[test]
    fn parse_claude_control_response_skips_unrelated_request_id() {
        // A control_response for a different request_id is skipped.
        let stdout = "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"other-request\",\"response\":{\"models\":[\
            {\"value\":\"x\",\"resolvedModel\":\"x\"}\
        ]}}}\n";
        let models = parse_claude_control_response(stdout);
        assert!(models.is_empty(), "expected empty, got {models:?}");
    }

    #[test]
    fn parse_claude_control_response_error_subtype_returns_empty() {
        // multica's parser: subtype != "success" is a probe failure
        // (e.g. "Unsupported control request subtype: list_models" on older
        // CLIs) → empty (caller falls back to manual entry).
        let stdout = "{\"type\":\"control_response\",\"response\":{\"subtype\":\"error\",\"request_id\":\"multica-list-models\",\"error\":\"Unsupported control request subtype: list_models\"}}\n";
        let models = parse_claude_control_response(stdout);
        assert!(models.is_empty(), "error subtype -> empty, got {models:?}");
    }

    #[test]
    fn parse_claude_control_response_no_matching_line_returns_empty() {
        assert!(parse_claude_control_response("").is_empty());
        assert!(parse_claude_control_response("garbage").is_empty());
        assert!(
            parse_claude_control_response(
                "{\"type\":\"system\",\"subtype\":\"hook_started\"}\n"
            )
            .is_empty()
        );
        // A control_response with no models array → empty.
        let stdout = "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"multica-list-models\",\"response\":{}}}\n";
        assert!(parse_claude_control_response(stdout).is_empty());
    }

    // ── parse_codex_debug_models ──

    /// Real codex 0.154.0 `codex debug models` shape (captured + trimmed to
    /// 2 models for the fixture — the real output is one ~500KB JSON line
    /// with ~12 models).
    #[test]
    fn parse_codex_debug_models_extracts_slugs() {
        let stdout = "{\"models\":[\
            {\"slug\":\"gpt-6-astra\",\"display_name\":\"GPT-6-Astra\",\"visibility\":\"list\"},\
            {\"slug\":\"gpt-5.5\",\"display_name\":\"GPT-5.5\",\"visibility\":\"list\"}\
        ]}";
        let models = parse_codex_debug_models(stdout);
        assert_eq!(models, vec!["gpt-6-astra".to_string(), "gpt-5.5".to_string()]);
    }

    #[test]
    fn parse_codex_debug_models_skips_hidden_and_empty_slug() {
        // multica's parseCodexModelCatalog: slug == "" OR visibility == "hide"
        // → skipped.
        let stdout = "{\"models\":[\
            {\"slug\":\"visible-model\",\"visibility\":\"list\"},\
            {\"slug\":\"hidden-model\",\"visibility\":\"hide\"},\
            {\"slug\":\"\",\"visibility\":\"list\"}\
        ]}";
        let models = parse_codex_debug_models(stdout);
        assert_eq!(models, vec!["visible-model".to_string()]);
    }

    #[test]
    fn parse_codex_debug_models_handles_banner_line() {
        // A banner line preceding the JSON (codex may print one on some
        // installs) must not break parsing — fall back to line-by-line.
        let stdout = "codex-cli 0.154.0\n{\"models\":[{\"slug\":\"gpt-6-astra\",\"visibility\":\"list\"}]}\n";
        let models = parse_codex_debug_models(stdout);
        assert_eq!(models, vec!["gpt-6-astra".to_string()]);
    }

    #[test]
    fn parse_codex_debug_models_garbage_returns_empty() {
        assert!(parse_codex_debug_models("").is_empty());
        assert!(parse_codex_debug_models("garbage").is_empty());
        assert!(parse_codex_debug_models("{\"not_models\":[1,2]}").is_empty());
        assert!(parse_codex_debug_models("{\"models\":\"not an array\"}").is_empty());
    }

    // ── codex_static_models ──

    #[test]
    fn codex_static_models_returns_nonempty_bundled_catalog() {
        let static_models = codex_static_models();
        assert!(!static_models.is_empty(), "static catalog must be non-empty");
        // multica's snapshot flagship is gpt-6-astra (models.go:657).
        assert!(
            static_models.contains(&"gpt-6-astra".to_string()),
            "expected gpt-6-astra in static catalog, got {static_models:?}"
        );
        // The desktop-pet's brief specifies "use codexStaticModels()-style
        // static fallback" — so the entries mirror multica's snapshot.
        assert!(static_models.len() >= 5);
    }

    // ── list_models dispatch (mocked via cfg.executable_path) ──

    /// Bogus `executable_path` → spawn fails → `list_models` returns
    /// `Ok(vec![])` (opencode path). Probes the failure contract without
    /// depending on any real CLI being installed.
    #[tokio::test]
    async fn list_models_opencode_spawn_failure_returns_empty() {
        let cfg = BackendConfig {
            executable_path: Some(PathBuf::from(
                "/definitely/not/opencode_xyz123",
            )),
            env: HashMap::new(),
        };
        let result = list_models("opencode", &cfg).await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert!(result.unwrap().is_empty());
    }

    /// Bogus `executable_path` → spawn fails → `list_models` returns
    /// `Ok(vec![])` (claude path).
    #[tokio::test]
    async fn list_models_claude_spawn_failure_returns_empty() {
        let cfg = BackendConfig {
            executable_path: Some(PathBuf::from(
                "/definitely/not/claude_xyz123",
            )),
            env: HashMap::new(),
        };
        let result = list_models("claude", &cfg).await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert!(result.unwrap().is_empty());
    }

    /// Bogus `executable_path` → spawn fails → `list_models` returns the
    /// codex static fallback (codex path is the only one with a static
    /// fallback per the brief).
    #[tokio::test]
    async fn list_models_codex_spawn_failure_returns_static_fallback() {
        let cfg = BackendConfig {
            executable_path: Some(PathBuf::from(
                "/definitely/not/codex_xyz123",
            )),
            env: HashMap::new(),
        };
        let result = list_models("codex", &cfg).await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        let models = result.unwrap();
        assert!(!models.is_empty(), "codex should fall back to static catalog");
        assert!(
            models.contains(&"gpt-6-astra".to_string()),
            "expected gpt-6-astra in codex fallback, got {models:?}"
        );
    }

    /// Unknown provider → `Ok(vec![])`.
    #[tokio::test]
    async fn list_models_unknown_provider_returns_empty() {
        let cfg = BackendConfig::default();
        let result = list_models("some-unknown-agent", &cfg).await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert!(result.unwrap().is_empty());
    }

    // ── real-CLI validation (#[ignore]) ──

    /// Real opencode probe — non-empty list. Run with
    /// `cargo test --manifest-path src-tauri/Cargo.toml models -- --ignored --nocapture`.
    /// opencode 1.18.30 verified.
    #[tokio::test]
    #[ignore]
    async fn real_opencode_models_probe_yields_nonempty_list() {
        let cfg = BackendConfig::default();
        let models = list_models("opencode", &cfg)
            .await
            .expect("list_models never errors");
        assert!(
            !models.is_empty(),
            "opencode probe should return a non-empty list, got {models:?}"
        );
        // Real format: provider/model (e.g. opencode/beam-free,
        // alibaba-cn/glm-5.2).
        assert!(
            models.iter().any(|m| m.contains('/')),
            "expected provider/model format, got {models:?}"
        );
    }

    /// Real codex probe — non-empty list. codex 0.154.0 verified. Run with
    /// `cargo test --manifest-path src-tauri/Cargo.toml models -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn real_codex_models_probe_yields_nonempty_list() {
        let cfg = BackendConfig::default();
        let models = list_models("codex", &cfg)
            .await
            .expect("list_models never errors");
        assert!(
            !models.is_empty(),
            "codex probe should return a non-empty list, got {models:?}"
        );
        // Real codex returns slugs like gpt-6-astra.
        assert!(
            models.iter().any(|m| m.contains("gpt")),
            "expected a gpt-* slug, got {models:?}"
        );
    }

    /// Real claude probe — non-empty list. The heaviest probe (a
    /// discovery-only stream-json session). claude 2.1.228 verified. Run
    /// with
    /// `cargo test --manifest-path src-tauri/Cargo.toml models -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn real_claude_models_probe_yields_nonempty_list() {
        let cfg = BackendConfig::default();
        let models = list_models("claude", &cfg)
            .await
            .expect("list_models never errors");
        assert!(
            !models.is_empty(),
            "claude probe should return a non-empty list, got {models:?}"
        );
        // Real claude control_response resolves to claude-* model ids.
        assert!(
            models.iter().any(|m| m.contains("claude")),
            "expected a claude-* resolved id, got {models:?}"
        );
    }
}
