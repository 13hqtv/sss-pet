//! Skill runtime helpers — pure functions for path validation, binary
//! detection, size caps, slug sanitization, frontmatter synthesis, and
//! provider skills-dir mapping. Mirrors multica's skill-runtime helpers
//! (`server/internal/skill/{reserved,binary}.go`, `sanitizeSkillName`,
//! `ensureSkillFrontmatter`). Materialize/cleanup land in Task 3.
//!
//! These are the decisions the agent layer needs at chat-turn time, kept
//! pure (no I/O) so each one is unit-testable in isolation.

#![allow(dead_code)] // consumed by Task 3 (materialize) + Task 4 (chat) + db.rs

use crate::db::{Skill, SkillFile};
use std::path::{Path, PathBuf};

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

// ---------------------------------------------------------------------------
// Materialize + cleanup (Task 3) — write enabled skills to disk per chat
// turn so the agent CLI discovers them natively. Mirrors multica's
// `writeSkillFiles` (execenv/context.go:940).
// ---------------------------------------------------------------------------

/// The result of materializing one batch of skills for a chat turn.
/// `created_dirs` records every skill directory written this turn (for
/// `cleanup_skill_dirs` after the turn). `prompt_section` is the skills
/// text to append to the agent's system prompt — a name-only index for
/// providers that discover skills natively (claude/opencode), or the
/// full skill bodies + script-dir paths for the codex fallback.
#[derive(Debug, Clone)]
pub struct MaterializedSkills {
    pub created_dirs: Vec<PathBuf>,
    pub prompt_section: String,
}

/// Materialize each enabled skill as a real directory of files so the
/// agent CLI can discover + use them:
/// - claude/opencode: write under `<cwd>/.<provider>/skills/<slug>/`
///   (the CLI's native skills location); the prompt gets a name-only
///   index (the CLI reads each SKILL.md itself).
/// - codex (no native skills dir): write to a per-turn temp dir + inject
///   the skill body + the script-dir path into the prompt so the agent
///   can run the scripts.
///
/// Collisions — with a user-installed skill dir on disk OR within the
/// batch (two skills sanitizing to the same slug) — resolve to a `-sss`
/// suffix, so user-installed skills are never clobbered + cleanup only
/// touches dirs this turn created. Mirrors multica's
/// `allocateCollisionFreeSkillDir` (execenv/sidecar_manifest.go:175).
pub fn materialize_skills(
    cwd: &Path,
    provider: &str,
    skills: &[(Skill, Vec<SkillFile>)],
) -> Result<MaterializedSkills, String> {
    let provider_dir = provider_skills_dir(provider);
    let mut created_dirs: Vec<PathBuf> = Vec::new();
    let mut used_slugs: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut name_index = String::new();
    let mut codex_sections = String::new();

    for (skill, files) in skills {
        // Guard against an empty / punctuation-only name sanitizing to ""
        // — `cwd.join(".claude/skills").join("")` IS the skills dir itself,
        // so a "" slug would write SKILL.md at the skills-dir level (the
        // CLI wouldn't discover it) + created_dirs would hold the parent
        // (cleanup would remove_dir_all the whole skills tree). Substitute
        // a fallback slug so the skill lands in a real subdir. The UI
        // rejects empty names, so this is defense-in-depth for direct
        // callers / future code paths.
        let mut base_slug = sanitize_skill_name(&skill.name);
        if base_slug.is_empty() {
            base_slug = "skill".into();
        }
        let slug = resolve_collision_free_slug(cwd, provider_dir, &base_slug, &used_slugs);
        used_slugs.insert(slug.clone());

        let (root, is_native) = match provider_dir {
            Some(rel) => (cwd.join(rel).join(&slug), true),
            None => {
                // codex fallback: temp dir, not under cwd (don't pollute the project)
                let root = std::env::temp_dir()
                    .join(format!("sss-pet-skill-{}-{}", uuid::Uuid::new_v4(), slug));
                (root, false)
            }
        };

        // On a mid-batch write failure, clean up the dirs already written
        // this turn + the failing skill's partial dir, so a routine error
        // (disk full / permission / bad path) doesn't leave orphans in the
        // user's project. (Crash-orphans are still possible + harmless —
        // the doc-comment on cleanup_skill_dirs covers that.)
        if let Err(e) = write_skill_dir(&root, skill, &slug, files) {
            cleanup_skill_dirs(&created_dirs);
            let _ = std::fs::remove_dir_all(&root);
            return Err(format!(
                "write_skill_dir for skill '{}': {e}",
                skill.name
            ));
        }
        created_dirs.push(root.clone());

        if is_native {
            // name-only index — the CLI discovers + reads SKILL.md itself
            name_index.push_str(&format!("- **{}**\n", skill.name));
        } else {
            // codex fallback: inject the body + the script dir path
            let mut sec = format!("## Skill: {}\n", skill.name);
            if let Some(desc) = &skill.description {
                if !desc.trim().is_empty() {
                    sec.push_str(desc);
                    sec.push('\n');
                }
            }
            if let Some(content) = &skill.content {
                if !content.trim().is_empty() {
                    sec.push_str(content);
                }
            }
            sec.push_str(&format!("\n\n脚本目录: {}\n", root.display()));
            codex_sections.push_str(&sec);
        }
    }

    let prompt_section = if provider_dir.is_some() {
        if skills.is_empty() {
            String::new()
        } else {
            format!(
                "## Skills\n你已安装以下技能（CLI 自动发现，按各自 SKILL.md 说明使用）：\n{}",
                name_index
            )
        }
    } else {
        codex_sections
    };

    Ok(MaterializedSkills {
        created_dirs,
        prompt_section,
    })
}

/// Pick a slug that doesn't collide with the batch (`used_slugs`) or an
/// existing non-empty dir on disk (a user-installed skill). First
/// collision → `{slug}-sss`; further → `{slug}-sss-2`, `-3`, ...
fn resolve_collision_free_slug(
    cwd: &Path,
    provider_dir: Option<&str>,
    base: &str,
    used_slugs: &std::collections::HashSet<String>,
) -> String {
    let dir_exists_nonempty = |slug: &str| -> bool {
        let rel = match provider_dir {
            Some(r) => r,
            None => return false, // temp dir — never pre-exists
        };
        let root = cwd.join(rel).join(slug);
        if !root.exists() {
            return false;
        }
        std::fs::read_dir(&root)
            .map(|mut it| it.next().is_some())
            .unwrap_or(false)
    };
    let mut candidate = base.to_string();
    if !used_slugs.contains(&candidate) && !dir_exists_nonempty(&candidate) {
        return candidate;
    }
    candidate = format!("{base}-sss");
    let mut n = 2;
    while used_slugs.contains(&candidate) || dir_exists_nonempty(&candidate) {
        candidate = format!("{base}-sss-{n}");
        n += 1;
    }
    candidate
}

/// Write one skill as a directory: `SKILL.md` (from `skill.content` with
/// synthesized frontmatter) + each supporting file at its nested relative
/// path (subdirs created as needed). Mirrors multica's `writeSkillFiles`
/// (execenv/context.go:940).
fn write_skill_dir(
    root: &Path,
    skill: &Skill,
    slug: &str,
    files: &[SkillFile],
) -> Result<(), String> {
    std::fs::create_dir_all(root)
        .map_err(|e| format!("create skill dir {}: {e}", root.display()))?;
    let body = ensure_frontmatter(
        skill.content.as_deref().unwrap_or(""),
        slug,
        skill.description.as_deref(),
    );
    std::fs::write(root.join("SKILL.md"), body)
        .map_err(|e| format!("write SKILL.md {}: {e}", root.display()))?;
    for f in files {
        // Defensive: validate before writing (db layer already validated
        // on replace_all, but this guards direct callers).
        validate_file_path(&f.path).map_err(|e| format!("skill_file path {:?}: {e}", f.path))?;
        if is_reserved_skill_md_path(&f.path) {
            continue; // shouldn't happen (db skips), but be safe
        }
        let target = root.join(&f.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create subdir {}: {e}", parent.display()))?;
        }
        std::fs::write(&target, &f.content)
            .map_err(|e| format!("write {}: {e}", target.display()))?;
    }
    Ok(())
}

/// Best-effort remove every directory created by `materialize_skills`.
/// Errors are ignored — a dir left by a crash is harmless (the user can
/// delete it; it's a valid skill dir). Called from the chat-turn drain
/// after the turn completes.
pub fn cleanup_skill_dirs(dirs: &[PathBuf]) {
    for d in dirs {
        let _ = std::fs::remove_dir_all(d);
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
        assert!(!is_reserved_skill_md_path("scripts/SKILL.md")); // nested — not reserved
        assert!(!is_reserved_skill_md_path("a.py"));
    }

    // -- materialize + cleanup (Task 3) --

    fn now_ts() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    fn mk_skill(id: &str, name: &str, desc: Option<&str>, content: Option<&str>) -> Skill {
        Skill {
            id: id.into(),
            name: name.into(),
            description: desc.map(|s| s.into()),
            content: content.map(|s| s.into()),
            created_at: now_ts(),
            updated_at: now_ts(),
        }
    }

    fn mk_file(skill_id: &str, path: &str, content: &str) -> SkillFile {
        SkillFile {
            id: format!("f-{path}"),
            skill_id: skill_id.into(),
            path: path.into(),
            content: content.into(),
            created_at: now_ts(),
            updated_at: now_ts(),
        }
    }

    #[test]
    fn materialize_claude_writes_skill_md_and_nested_files_under_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let skill = mk_skill("s1", "Code Review", Some("d"), Some("# review code"));
        let files = vec![mk_file("s1", "scripts/a.py", "print(1)")];
        let m = materialize_skills(tmp.path(), "claude", &[(skill, files)]).unwrap();
        let dir = tmp.path().join(".claude/skills/code-review");
        assert!(dir.join("SKILL.md").exists(), "SKILL.md not written");
        let body = std::fs::read_to_string(dir.join("SKILL.md")).unwrap();
        assert!(body.starts_with("---\nname: code-review"), "frontmatter missing: {body}");
        assert!(body.contains("# review code"), "content missing: {body}");
        assert!(dir.join("scripts/a.py").exists(), "nested file not written");
        // prompt is a name-only index — no content
        assert!(m.prompt_section.contains("Code Review"));
        assert!(!m.prompt_section.contains("# review code"));
        assert_eq!(m.created_dirs, vec![dir.clone()]);
    }

    #[test]
    fn materialize_does_not_clobber_user_installed_skill_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let user_dir = tmp.path().join(".claude/skills/code-review");
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::write(user_dir.join("SKILL.md"), "USER OWNED").unwrap();
        let skill = mk_skill("s1", "Code Review", None, Some("mine"));
        let m = materialize_skills(tmp.path(), "claude", &[(skill, vec![])]).unwrap();
        // original untouched
        assert_eq!(
            std::fs::read_to_string(user_dir.join("SKILL.md")).unwrap(),
            "USER OWNED"
        );
        // written to the -sss suffix dir
        let sss_dir = tmp.path().join(".claude/skills/code-review-sss");
        assert!(sss_dir.join("SKILL.md").exists(), "sss dir not written");
        assert_eq!(m.created_dirs, vec![sss_dir]);
    }

    #[test]
    fn materialize_codex_fallback_uses_temp_dir_and_injects_content_and_path() {
        let tmp = tempfile::tempdir().unwrap();
        let skill = mk_skill("s1", "Code Review", Some("d"), Some("# review"));
        let files = vec![mk_file("s1", "scripts/a.py", "print(1)")];
        let m = materialize_skills(tmp.path(), "codex", &[(skill, files)]).unwrap();
        // codex doesn't write a .codex dir in cwd
        assert!(!tmp.path().join(".codex").exists());
        // written to a temp dir (not under the cwd tempdir)
        assert_eq!(m.created_dirs.len(), 1);
        let dir = &m.created_dirs[0];
        assert!(dir.join("SKILL.md").exists());
        assert!(dir.join("scripts/a.py").exists());
        assert!(
            !dir.starts_with(tmp.path()),
            "codex fallback dir should be outside cwd"
        );
        // prompt carries the body + the script dir path
        assert!(m.prompt_section.contains("# review"), "prompt missing body: {}", m.prompt_section);
        assert!(
            m.prompt_section.contains(dir.to_string_lossy().as_ref()),
            "prompt missing script dir path: {}",
            m.prompt_section
        );
    }

    #[test]
    fn cleanup_skill_dirs_removes_created_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("x");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("f"), "y").unwrap();
        cleanup_skill_dirs(&[d.clone()]);
        assert!(!d.exists());
    }

    #[test]
    fn materialize_slug_collision_in_batch_appends_suffix() {
        let tmp = tempfile::tempdir().unwrap();
        let m = materialize_skills(
            tmp.path(),
            "claude",
            &[
                (mk_skill("s1", "Code Review", None, Some("x")), vec![]),
                (mk_skill("s2", "Code Review", None, Some("y")), vec![]),
            ],
        )
        .unwrap();
        assert_eq!(m.created_dirs.len(), 2);
        let names: Vec<String> = m
            .created_dirs
            .iter()
            .map(|d| d.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"code-review".to_string()));
        assert!(names.contains(&"code-review-sss".to_string()));
    }

    // -- review-fix pass: Important #2 (empty-slug clobber) + #3 (partial
    //    failure orphans). These start RED + go GREEN when the fixes land. --

    #[test]
    fn materialize_empty_name_skill_uses_fallback_slug_not_skills_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // An empty / punctuation-only name sanitizes to "" — without the
        // guard, `cwd.join(".claude/skills").join("")` IS the skills dir
        // itself, so write_skill_dir writes SKILL.md at the skills-dir
        // level + created_dirs gets the parent → cleanup would
        // remove_dir_all the whole .claude/skills tree. The guard must
        // substitute a fallback slug so the skill lands in a subdir.
        let skill = mk_skill("s1", "", None, Some("body"));
        let m = materialize_skills(tmp.path(), "claude", &[(skill, vec![])]).unwrap();
        assert_eq!(m.created_dirs.len(), 1);
        let dir = &m.created_dirs[0];
        let skills_dir = tmp.path().join(".claude/skills");
        assert_ne!(
            dir, &skills_dir,
            "empty-name skill must NOT materialize into the skills dir itself"
        );
        assert!(
            dir.starts_with(&skills_dir),
            "fallback slug dir should still be under .claude/skills"
        );
        assert!(dir.join("SKILL.md").exists());
    }

    #[test]
    fn materialize_partial_failure_cleans_up_already_written_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        // First skill valid; second skill has a file with a traversal
        // path (`..`) — validate_file_path rejects it inside
        // write_skill_dir, so the second write fails mid-batch. The
        // first skill's dir must be cleaned up (no orphans in the user's
        // project), + the failing skill's partial dir too.
        let good = mk_skill("s1", "Good Skill", None, Some("body"));
        let bad = mk_skill("s2", "Bad Skill", None, Some("body"));
        let bad_files = vec![mk_file("s2", "../evil.py", "x")]; // traversal → Err
        let result = materialize_skills(
            tmp.path(),
            "claude",
            &[(good, vec![]), (bad, bad_files)],
        );
        assert!(result.is_err(), "expected materialize to fail on the bad file");
        assert!(
            !tmp.path().join(".claude/skills/good-skill").exists(),
            "orphan dir left after partial failure (good skill not cleaned up)"
        );
        assert!(
            !tmp.path().join(".claude/skills/bad-skill").exists(),
            "failing skill's partial dir not cleaned up"
        );
    }
}
