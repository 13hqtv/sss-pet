//! Skill runtime helpers — pure functions for path validation, binary
//! detection, size caps, slug sanitization, frontmatter synthesis, and
//! provider skills-dir mapping. Mirrors multica's skill-runtime helpers
//! (`server/internal/skill/{reserved,binary}.go`, `sanitizeSkillName`,
//! `ensureSkillFrontmatter`). Materialize/cleanup land in Task 3.
//!
//! These are the decisions the agent layer needs at chat-turn time, kept
//! pure (no I/O) so each one is unit-testable in isolation.

#![allow(dead_code)] // consumed by Task 3 (materialize) + Task 4 (chat) + db.rs

use std::path::Path;

/// Single-file size cap: 1 MiB (mirrors multica's `maxImportFileSize`).
pub const MAX_FILE_BYTES: usize = 1024 * 1024;
/// Per-skill total content cap: 8 MiB (multica's `maxImportTotalSize`).
pub const MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;
/// Per-skill file count cap (multica's `maxImportFileCount`).
pub const MAX_FILE_COUNT: usize = 256;

/// True iff `path` cleans to exactly `SKILL.md` at the skill root
/// (case-insensitive) — the reserved primary-content filename. A nested
/// `scripts/SKILL.md` is NOT reserved (it's a legit supporting file).
/// Mirrors multica's `IsReservedContentPath` (server/internal/skill/reserved.go),
/// which checks `filepath.Clean(p) == "SKILL.md"`.
pub fn is_reserved_skill_md_path(path: &str) -> bool {
    // Collect components, skipping CurDir (`.`); the path is reserved iff
    // exactly one component remains and it's `Normal("SKILL.md")`.
    let mut comps = Path::new(path)
        .components()
        .filter(|c| !matches!(c, std::path::Component::CurDir));
    match (comps.next(), comps.next()) {
        (Some(std::path::Component::Normal(name)), None) => {
            name.to_string_lossy().eq_ignore_ascii_case("SKILL.md")
        }
        _ => false,
    }
}

/// Validate a supporting-file path. Rejects:
/// - empty / whitespace-only
/// - absolute paths (`C:\x`, `\\server\x`) via `is_absolute`
/// - any rooted path (`/x`, `\x`) or drive prefix (`C:`) — on Windows,
///   `is_absolute()` is false for drive-less rooted paths, so we also
///   reject `RootDir` + `Prefix` components explicitly (otherwise
///   `/etc/x` would write to the current drive's root, outside the skill)
/// - any `..` (ParentDir) component — traversal outside the skill dir
/// - a root-level `SKILL.md` (reserved — see `is_reserved_skill_md_path`)
/// Mirrors multica's `validateFilePath` (skill.go:310), hardened for
/// Windows path semantics.
pub fn validate_file_path(path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("path is empty".into());
    }
    let p = Path::new(path);
    if p.is_absolute() {
        return Err(format!("absolute path not allowed: {path}"));
    }
    for comp in p.components() {
        match comp {
            std::path::Component::ParentDir => {
                return Err(format!("`..` not allowed in path: {path}"));
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(format!("rooted/absolute path not allowed: {path}"));
            }
            _ => {}
        }
    }
    if is_reserved_skill_md_path(path) {
        return Err(format!("SKILL.md is reserved (lives in skills.content): {path}"));
    }
    Ok(())
}

/// True iff the path's extension is binary OR the content contains a NUL
/// byte. Extension blacklist mirrors multica's `IsLikelyBinaryFilePath`
/// (server/internal/skill/binary.go): images, fonts, archives, office
/// docs, media, executables, databases. The NUL check catches binary
/// content masquerading under a text extension.
pub fn is_likely_binary(path: &str, content: &[u8]) -> bool {
    if content.contains(&0u8) {
        return true;
    }
    let ext = match Path::new(path).extension() {
        Some(e) => e.to_string_lossy().to_ascii_lowercase(),
        None => return false,
    };
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "ico" | "webp" | "pdf"
            | "zip" | "tar" | "gz" | "rar" | "7z"
            | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx"
            | "mp3" | "mp4" | "avi" | "mov" | "wav" | "flac"
            | "exe" | "dll" | "so" | "dylib" | "bin"
            | "woff" | "woff2" | "ttf" | "otf"
            | "db" | "sqlite" | "sqlite3"
    )
}

/// Enforce the per-skill caps: file count, single-file size, total size.
/// Returns the first violation as an error string. Mirrors multica's
/// import caps (skill.go:781).
pub fn check_caps(files: &[(String, String)]) -> Result<(), String> {
    if files.len() > MAX_FILE_COUNT {
        return Err(format!(
            "too many files: {} > {MAX_FILE_COUNT}",
            files.len()
        ));
    }
    let mut total: usize = 0;
    for (path, content) in files {
        if content.len() > MAX_FILE_BYTES {
            return Err(format!("file > 1 MiB: {path} ({} bytes)", content.len()));
        }
        total += content.len();
    }
    if total > MAX_TOTAL_BYTES {
        return Err(format!("skill total > 8 MiB: {total} bytes"));
    }
    Ok(())
}

/// Sanitize a skill name into a directory slug: lowercase, non-
/// alphanumeric → `-`, collapse runs, trim ends. Unicode alphanumeric
/// chars (e.g. CJK) are preserved. Mirrors multica's `sanitizeSkillName`
/// (execenv/context.go:914).
pub fn sanitize_skill_name(name: &str) -> String {
    let dashed: String = name
        .chars()
        .map(|c| {
            let lower = c.to_ascii_lowercase();
            if lower.is_alphanumeric() {
                lower
            } else {
                '-'
            }
        })
        .collect();
    dashed
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Ensure a SKILL.md body carries YAML frontmatter (name + description)
/// before it's written to disk for CLI discovery. If `content` already
/// starts with a `---` frontmatter block, return it unchanged; else
/// synthesize `---\nname: {slug}\ndescription: {desc}\n---\n` as a prefix.
/// Mirrors multica's `ensureSkillFrontmatter` (execenv/context.go:967).
pub fn ensure_frontmatter(content: &str, slug: &str, description: Option<&str>) -> String {
    if content.starts_with("---\n") || content.starts_with("---\r\n") {
        return content.to_string();
    }
    let desc = description.unwrap_or("");
    format!("---\nname: {slug}\ndescription: {desc}\n---\n{content}")
}

/// The provider-native skills directory (relative to the conversation
/// cwd) the agent CLI discovers at runtime. claude → `.claude/skills`,
/// opencode → `.opencode/skills`. codex + unknown → `None` (the
/// materializer falls back to a temp dir + path injection). Mirrors
/// multica's `skillsDirPath` mapping (execenv/context.go:329).
pub fn provider_skills_dir(provider: &str) -> Option<&'static str> {
    match provider {
        "claude" => Some(".claude/skills"),
        "opencode" => Some(".opencode/skills"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_file_path_rejects_traversal_absolute_empty_skill_md() {
        assert!(validate_file_path("").is_err());
        assert!(validate_file_path("   ").is_err());
        assert!(validate_file_path("/etc/x").is_err()); // absolute
        assert!(validate_file_path("../x").is_err()); // ..
        assert!(validate_file_path("a/../b").is_err()); // nested ..
        assert!(validate_file_path("SKILL.md").is_err()); // reserved
        assert!(validate_file_path("skill.md").is_err()); // case-insensitive
        assert!(validate_file_path("./SKILL.md").is_err());
        assert!(validate_file_path("scripts/a.py").is_ok()); // legal
        assert!(validate_file_path("docs/sub/b.md").is_ok()); // nested legal
    }

    #[test]
    fn is_likely_binary_detects_extension_and_nul() {
        assert!(is_likely_binary("a.png", b"data"));
        assert!(is_likely_binary("a.zip", b"data"));
        assert!(is_likely_binary("a.py", b"\0\x01")); // NUL byte
        assert!(!is_likely_binary("a.py", b"print(1)"));
        assert!(!is_likely_binary("a.md", b"# hi"));
        assert!(!is_likely_binary("noext", b"text")); // no extension, no NUL
    }

    #[test]
    fn check_caps_enforces_limits() {
        let big = "x".repeat(MAX_FILE_BYTES + 1);
        assert!(check_caps(&[("a.py".into(), big)]).is_err()); // single-file over
        let many: Vec<(String, String)> = (0..(MAX_FILE_COUNT + 1))
            .map(|i| (format!("f{i}.py"), "x".into()))
            .collect();
        assert!(check_caps(&many).is_err()); // count over
        assert!(check_caps(&[("a.py".into(), "x".into())]).is_ok()); // under all caps
    }

    #[test]
    fn sanitize_skill_name_lowercases_and_dashes() {
        assert_eq!(sanitize_skill_name("Code Review"), "code-review");
        assert_eq!(sanitize_skill_name("代码审查!"), "代码审查"); // CJK preserved, ! dropped
        assert_eq!(sanitize_skill_name("  A  B  "), "a-b");
        assert_eq!(sanitize_skill_name("---already-dashed---"), "already-dashed");
        assert_eq!(sanitize_skill_name("UPPER"), "upper");
    }

    #[test]
    fn ensure_frontmatter_synthesizes_only_when_missing() {
        let with = "---\nname: x\ndescription: y\n---\nbody";
        assert_eq!(ensure_frontmatter(with, "slug", Some("d")), with); // unchanged
        let without = "just body";
        let r = ensure_frontmatter(without, "my-skill", Some("desc"));
        assert!(
            r.starts_with("---\nname: my-skill\ndescription: desc\n---\n"),
            "got: {r}"
        );
        assert!(r.ends_with("just body"));
        // None description → empty
        let r2 = ensure_frontmatter("body", "s", None);
        assert!(r2.starts_with("---\nname: s\ndescription: \n---\n"));
    }

    #[test]
    fn provider_skills_dir_maps_three_providers() {
        assert_eq!(provider_skills_dir("claude"), Some(".claude/skills"));
        assert_eq!(provider_skills_dir("opencode"), Some(".opencode/skills"));
        assert_eq!(provider_skills_dir("codex"), None); // fallback
        assert_eq!(provider_skills_dir("???"), None); // unknown
    }

    #[test]
    fn is_reserved_skill_md_path_case_insensitive() {
        assert!(is_reserved_skill_md_path("SKILL.md"));
        assert!(is_reserved_skill_md_path("skill.md"));
        assert!(is_reserved_skill_md_path("./SKILL.md"));
        assert!(!is_reserved_skill_md_path("scripts/SKILL.md")); // not the final component
        assert!(!is_reserved_skill_md_path("a.py"));
    }
}
