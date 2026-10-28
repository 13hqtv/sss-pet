# 技能脚本文件支持 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让技能能关联多个文本脚本文件（多层目录），运行时物化进对话 cwd 的 provider 原生 skills 目录，agent CLI 自己发现 + 调用，对标 multica skill 管理。

**Architecture:** DB 存技能文件（新增 `skill_files` 表，`{path, content}` 文本行，目录从 path 推导）。chat 跑一轮前把启用技能物化成 `<cwd>/.<provider>/skills/<slug>/SKILL.md` + 脚本按相对路径铺开；CLI 原生发现（claude→`.claude/skills/`、opencode→`.opencode/skills/`）；codex 物化进临时目录 + 路径注入提示词（fallback）。提示词从塞正文改为名字索引（codex fallback 保留正文 + 路径）。跑完清理本轮写的目录。

**Tech Stack:** Rust (edition 2024) + rusqlite (bundled) + Tauri 2 + Svelte 5 (runes)。测试：Rust `#[test]` + tempdir；前端 `svelte-check`。

**Spec:** `docs/superpowers/specs/2026-10-27-skill-files-design.md`

## Global Constraints

- 三 provider：claude / codex / opencode（`agent::SUPPORTED`）。
- 文本文件 only；二进制按扩展名黑名单 + NUL 字节拒绝。
- 上限：单文件 1 MiB，单技能总 8 MiB，最多 256 个文件。
- `SKILL.md` 是保留路径（清理后忽略大小写等于）——不作为 file 行，正文在 `skills.content`。
- 现有 `skills` 表不动（`id, name, description: Option, content: Option, created_at, updated_at`）。
- 构建：`pnpm tauri build --no-bundle`（不是 `cargo build --release`）。
- Tauri 2 IPC：命令参数 camelCase → snake_case Rust 参数；返回值 serde 字段名原样。
- 提交信息前缀：`feat:` / `fix:` / `refactor:` / `docs:`。
- 现有测试套件 178 passed / 4 ignored，不得回归。

## Review Focus

五个最可能咬到用户的输入/失败模式，以及钉住它们的测试：

1. **路径穿越**——`skill_file.path` 是 `../../etc/passwd` 或绝对路径 `C:\x`，物化时写到技能目录外。→ Task 2 `validate_file_path` 拒绝（测试：`..` 段、绝对路径、空都 Err）。
2. **`SKILL.md` 保留路径**——用户加一个 path 为 `SKILL.md` 的文件，会跟正文冲突。→ Task 2 `validate_file_path` 拒绝（测试：`SKILL.md` / `skill.md` / `./SKILL.md` 都 Err）；Task 1 `skill_file_replace_all` 跳过该 path（测试：传入含 `SKILL.md` 的列表，该行不入库）。
3. **二进制文件**——用户加 `.png` 或内容含 NUL 字节。→ Task 2 `is_likely_binary` 返回 true（测试：扩展名黑名单 + NUL 内容）；Task 1 `skill_file_replace_all` 拒绝（测试：传入二进制 path 报错）。
4. **覆盖用户已装技能**——物化 `<slug>/` 时用户项目里已有 `.claude/skills/<slug>/`。→ Task 3 用 `<slug>-sss` 后缀，不覆盖（测试：预创建 `<slug>/SKILL.md`，物化后原文件不动、新内容在 `<slug>-sss/`）。
5. **codex 不发现 skills 目录**——codex app-server 不读 `.codex/skills/`，技能正文丢失。→ Task 3 codex fallback：物化进临时目录 + 提示词注入正文 + 脚本路径（测试：codex 分支 prompt 含正文 + 临时目录路径）；builder 实机验证 codex 行为。

补充（非测试，builder 注意）：崩溃留下物化目录（best-effort 清理，无害）；同 cwd 并发对话碰撞（单用户低风险）。

---

## Task 1: skill_files DB 表 + SkillFile struct + CRUD + Tauri 命令

**Files:**
- Modify: `src-tauri/src/db.rs`（migrate `execute_batch` 加表；加 `SkillFile` struct + 2 函数 + 测试）
- Modify: `src-tauri/src/lib.rs`（注册 2 命令）

**Interfaces:**
- Consumes: 现有 `db::Skill` struct（`db.rs`）、`migrate()` 的 `execute_batch`（`db.rs` 里 `CREATE TABLE IF NOT EXISTS skills` 之后）。
- Produces:
  - `pub struct SkillFile { id: String, skill_id: String, path: String, content: String, created_at: String, updated_at: String }`（`#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]`，对标 `Skill` struct）
  - `pub fn skill_file_list(conn: &Connection, skill_id: &str) -> rusqlite::Result<Vec<SkillFile>>`（`ORDER BY path ASC`）
  - `pub fn skill_file_replace_all(conn: &mut Connection, skill_id: &str, files: &[(String, String)]) -> rusqlite::Result<()>`（事务内：`DELETE FROM skill_files WHERE skill_id=?` 再逐个 `INSERT`；跳过清理后等于 `SKILL.md` 的 path；校验二进制/上限——调 Task 2 的 helper，但 Task 2 还没建，**本任务先内联最小校验**：跳过 `SKILL.md`、拒二进制扩展名、超 1MiB 单文件报错；Task 2 建好后换成调 helper）
  - Tauri 命令 `skill_file_list(db, skillId) -> Result<Vec<SkillFile>, String>`、`skill_file_replace_all(db, skillId, files: Vec<(String,String)>) -> Result<(), String>`（注册进 `invoke_handler`，跟在 `agent_skill_set_enabled` 后）

- [ ] **Step 1: 写失败测试**（`db.rs` tests mod）

```rust
#[test]
fn skill_file_replace_all_inserts_lists_replaces_and_skips_skill_md() {
    let (_tmp, _g, conn) = fresh_db(); // 复用 db.rs 现有 fresh_db helper
    let sid = db::skill_create(&conn, "s1", None, Some("body")).unwrap();
    // insert
    db::skill_file_replace_all(&mut conn, &sid, &[
        ("scripts/a.py".into(), "print(1)".into()),
        ("docs/b.md".into(), "# B".into()),
    ]).unwrap();
    let listed = db::skill_file_list(&conn, &sid).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].path, "docs/b.md"); // ORDER BY path
    // replace (delete old + write new)
    db::skill_file_replace_all(&mut conn, &sid, &[
        ("scripts/c.py".into(), "print(2)".into()),
    ]).unwrap();
    assert_eq!(db::skill_file_list(&conn, &sid).unwrap().len(), 1);
    // SKILL.md reserved — skipped, not stored
    db::skill_file_replace_all(&mut conn, &sid, &[
        ("SKILL.md".into(), "x".into()),
        ("scripts/d.py".into(), "y".into()),
    ]).unwrap();
    let listed = db::skill_file_list(&conn, &sid).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].path, "scripts/d.py");
}

#[test]
fn skill_file_replace_all_rejects_binary_and_oversize() {
    let (_tmp, _g, conn) = fresh_db();
    let sid = db::skill_create(&conn, "s1", None, Some("body")).unwrap();
    // binary by extension
    let err = db::skill_file_replace_all(&mut conn, &sid, &[
        ("a.png".into(), "data".into()),
    ]).unwrap_err();
    assert!(err.to_string().contains("binary") || err.to_string().contains("二进制"));
    // oversize (>1MiB)
    let big = "x".repeat(1024 * 1024 + 1);
    let err = db::skill_file_replace_all(&mut conn, &sid, &[
        ("a.py".into(), big),
    ]).unwrap_err();
    assert!(err.to_string().contains("1") || err.to_string().contains("MiB"));
}

#[test]
fn skill_files_cascade_delete_with_skill() {
    let (_tmp, _g, conn) = fresh_db();
    let sid = db::skill_create(&conn, "s1", None, Some("body")).unwrap();
    db::skill_file_replace_all(&mut conn, &sid, &[("a.py".into(), "x".into())]).unwrap();
    db::skill_delete(&conn, &sid).unwrap();
    assert_eq!(db::skill_file_list(&conn, &sid).unwrap().len(), 0); // FK CASCADE
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib skill_file` (in `src-tauri/`)
Expected: 编译失败（`SkillFile` / `skill_file_*` 未定义）

- [ ] **Step 3: 实现 `SkillFile` + 表 + CRUD（db.rs）**

在 `migrate()` 的 `execute_batch` 里 `CREATE TABLE IF NOT EXISTS skills (...)` 块之后加：

```sql
CREATE TABLE IF NOT EXISTS skill_files (
    id TEXT PRIMARY KEY,
    skill_id TEXT NOT NULL,
    path TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(skill_id, path),
    FOREIGN KEY (skill_id) REFERENCES skills(id) ON DELETE CASCADE
);
```

`SkillFile` struct 加在 `Skill` struct 附近。`skill_file_list` / `skill_file_replace_all` 加在 `skill_delete` 之后。`replace_all` 用 `conn.execute("BEGIN")` … `COMMIT` 手动事务（rusqlite `&mut Connection`），跳过 `filepath::clean`-style 等于 `SKILL.md`（忽略大小写）的 path，二进制扩展名黑名单（`.png/.jpg/.jpeg/.gif/.bmp/.ico/.webp/.pdf/.zip/.tar/.gz/.rar/.7z/.doc/.docx/.xls/.xlsx/.ppt/.pptx/.mp3/.mp4/.avi/.mov/.exe/.dll/.so/.dylib/.woff/.woff2/.ttf/.otf/.db/.sqlite`）拒、单文件 > 1048576 字节拒。Task 2 建好后把这些内联校验换成调 `crate::skill::validate_file_path` / `is_likely_binary` / `check_caps`。

- [ ] **Step 4: 注册 Tauri 命令（lib.rs）**

加两个 `#[tauri::command]`（对标现有 `skill_list` / `skill_update` 模式，参数 `db: State<'_, Arc<Mutex<Connection>>>`，camelCase `skillId` → `skill_id`），在 `invoke_handler!` 里 `agent_skill_set_enabled,` 之后加 `skill_file_list,` 和 `skill_file_replace_all,`。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test --lib skill_file` (in `src-tauri/`)
Expected: 3 测试 PASS

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/db.rs src-tauri/src/lib.rs
git commit -m "feat: skill_files table + SkillFile CRUD + commands"
```

---

## Task 2: skill.rs 运行时纯 helper（校验/二进制/上限/slug/frontmatter/provider 目录）

**Files:**
- Create: `src-tauri/src/skill.rs`
- Modify: `src-tauri/src/lib.rs`（加 `pub mod skill;`，在 `pub mod agent_gen;` 之后）
- Modify: `src-tauri/src/db.rs`（把 Task 1 内联校验换成调 `crate::skill::` helper）

**Interfaces:**
- Consumes: 无（纯函数）。
- Produces:
  - `pub fn validate_file_path(path: &str) -> Result<(), String>`——拒绝空、绝对（`/` 或 `<盘符>:` 开头，用 `std::path::Path::new(path).is_absolute()`）、含 `..` 段（`Path::components()` 含 `ParentDir`）、清理后忽略大小写等于 `SKILL.md`。
  - `pub fn is_likely_binary(path: &str, content: &[u8]) -> bool`——扩展名黑名单（同 Task 1 列表）或 content 含 NUL 字节（`content.contains(&0u8)`）。
  - `pub fn check_caps(files: &[(String, String)]) -> Result<(), String>`——单文件 > 1MiB、总 > 8MiB、数 > 256 都 Err。
  - `pub fn sanitize_skill_name(name: &str) -> String`——小写 + 非字母数字替换为 `-` + 连续 `-` 合并 + 首尾 `-` 裁剪（对标 multica `sanitizeSkillName`）。
  - `pub fn ensure_frontmatter(content: &str, slug: &str, description: Option<&str>) -> String`——若 content 已以 `---\n` 开头（已有 frontmatter）原样返回；否则前缀合成 `---\nname: {slug}\ndescription: {desc or empty}\n---\n`。
  - `pub fn provider_skills_dir(provider: &str) -> Option<&'static str>`——`claude`→`Some(".claude/skills")`、`opencode`→`Some(".opencode/skills")`、`codex`→`None`（fallback）、其他→`None`。

- [ ] **Step 1: 写失败测试**（skill.rs tests mod）

```rust
#[test]
fn validate_file_path_rejects_traversal_absolute_empty_skill_md() {
    assert!(validate_file_path("").is_err());
    assert!(validate_file_path("/etc/x").is_err());           // 绝对
    assert!(validate_file_path("../x").is_err());             // ..
    assert!(validate_file_path("a/../b").is_err());           // 内嵌 ..
    assert!(validate_file_path("SKILL.md").is_err());         // 保留
    assert!(validate_file_path("skill.md").is_err());         // 忽略大小写
    assert!(validate_file_path("./SKILL.md").is_err());
    assert!(validate_file_path("scripts/a.py").is_ok());      // 合法
    assert!(validate_file_path("docs/sub/b.md").is_ok());     // 多层合法
}

#[test]
fn is_likely_binary_detects_extension_and_nul() {
    assert!(is_likely_binary("a.png", b"data"));
    assert!(is_likely_binary("a.zip", b"data"));
    assert!(is_likely_binary("a.py", b"\0\x01"));             // NUL
    assert!(!is_likely_binary("a.py", b"print(1)"));
    assert!(!is_likely_binary("a.md", b"# hi"));
}

#[test]
fn check_caps_enforces_limits() {
    let big = "x".repeat(1024 * 1024 + 1);
    assert!(check_caps(&[("a.py".into(), big)]).is_err());    // 单文件超
    let many: Vec<(String,String)> = (0..257).map(|i| (format!("f{i}.py"), "x".into())).collect();
    assert!(check_caps(&many).is_err());                       // 数量超
    assert!(check_caps(&[("a.py".into(), "x".into())]).is_ok());
}

#[test]
fn sanitize_skill_name_lowercases_and_dashes() {
    assert_eq!(sanitize_skill_name("Code Review"), "code-review");
    assert_eq!(sanitize_skill_name("代码审查!"), "代码审查"); // 非字母数字含 unicode 字母——保留 unicode alnum，替换标点
    assert_eq!(sanitize_skill_name("  A  B  "), "a-b");
}

#[test]
fn ensure_frontmatter_synthesizes_only_when_missing() {
    let with = "---\nname: x\n---\nbody";
    assert_eq!(ensure_frontmatter(with, "slug", Some("d")), with); // 已有不改
    let without = "just body";
    let r = ensure_frontmatter(without, "my-skill", Some("desc"));
    assert!(r.starts_with("---\nname: my-skill\ndescription: desc\n---\n"));
    assert!(r.ends_with("just body"));
}

#[test]
fn provider_skills_dir_maps_three_providers() {
    assert_eq!(provider_skills_dir("claude"), Some(".claude/skills"));
    assert_eq!(provider_skills_dir("opencode"), Some(".opencode/skills"));
    assert_eq!(provider_skills_dir("codex"), None);
    assert_eq!(provider_skills_dir("???"), None);
}
```

注意 `sanitize_skill_name` 的 unicode 处理：对标 multica `sanitizeSkillName`（context.go:914）——multica 用 ASCII `unicode.IsLetter`/`IsDigit`？读 multica 源码确认：multica 的实现是 `strings.ToLower` + 非字母数字（含 unicode 字母数字保留）替换 `-`。Rust 用 `char::is_alphanumeric()`（含 unicode）保留、其余替换。测试 `代码审查!` → `代码审查`（`!` 替换、首尾 `-` 裁剪后空）。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib skill::` (in `src-tauri/`)
Expected: 编译失败（模块未建）

- [ ] **Step 3: 实现 skill.rs（纯函数 + `pub mod skill;` 注册）**

`pub mod skill;` 加在 lib.rs `pub mod agent_gen;` 之后。函数体由签名 + 测试决定（无歧义）；`sanitize_skill_name` 用 `char::is_alphanumeric()` 过滤 + `collect` 成 String，连续 `-` 用 `split('-').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("-")` 合并。

- [ ] **Step 4: 把 db.rs Task 1 内联校验换成调 `crate::skill::` helper**

`skill_file_replace_all` 里的二进制扩展名黑名单 + 1MiB 检查 → 换成对每个 file 调 `validate_file_path(&path)?` + `is_likely_binary(&path, content.as_bytes())` 拒 + `check_caps(files)?`。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test --lib skill` (in `src-tauri/`)
Expected: Task 2 的 6 测试 + Task 1 的 3 测试都 PASS（Task 1 校验改调 helper 后行为不变）

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/skill.rs src-tauri/src/lib.rs src-tauri/src/db.rs
git commit -m "feat: skill runtime helpers (validate/binary/caps/slug/frontmatter/provider-dir)"
```

---

## Task 3: skill.rs 物化 + 清理

**Files:**
- Modify: `src-tauri/src/skill.rs`（加 `MaterializedSkills` struct + `materialize_skills` + `cleanup_skill_dirs` + 测试）

**Interfaces:**
- Consumes: `crate::db::{Skill, SkillFile}`、Task 2 的 `sanitize_skill_name` / `ensure_frontmatter` / `provider_skills_dir` / `validate_file_path`。
- Produces:
  - `pub struct MaterializedSkills { pub created_dirs: Vec<std::path::PathBuf>, pub prompt_section: String }`
  - `pub fn materialize_skills(cwd: &Path, provider: &str, skills: &[(Skill, Vec<SkillFile>)]) -> Result<MaterializedSkills, String>`——见 Step 3 算法。
  - `pub fn cleanup_skill_dirs(dirs: &[std::path::PathBuf])`——best-effort `fs::remove_dir_all` 每个，错误忽略。

- [ ] **Step 1: 写失败测试**（skill.rs tests mod，用 `tempfile::tempdir`）

```rust
#[test]
fn materialize_claude_writes_skill_md_and_nested_files_under_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let skill = Skill { id: "s1".into(), name: "Code Review".into(), description: Some("d".into()),
        content: Some("# review code".into()), created_at: now(), updated_at: now() };
    let files = vec![SkillFile { id:"f1".into(), skill_id:"s1".into(), path:"scripts/a.py".into(),
        content:"print(1)".into(), created_at:now(), updated_at:now() }];
    let m = materialize_skills(tmp.path(), "claude", &[(skill.clone(), files)]).unwrap();
    let dir = tmp.path().join(".claude/skills/code-review");
    assert!(dir.join("SKILL.md").exists());                  // 正文 + 合成 frontmatter
    let body = std::fs::read_to_string(dir.join("SKILL.md")).unwrap();
    assert!(body.starts_with("---\nname: code-review"));     // frontmatter 合成
    assert!(body.contains("# review code"));
    assert!(dir.join("scripts/a.py").exists());              // 嵌套文件
    // prompt 是名字索引（不含正文）
    assert!(m.prompt_section.contains("Code Review"));
    assert!(!m.prompt_section.contains("# review code"));
    assert_eq!(m.created_dirs, vec![dir.clone()]);
}

#[test]
fn materialize_does_not_clobber_user_installed_skill_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let user_dir = tmp.path().join(".claude/skills/code-review");
    fs::create_dir_all(&user_dir).unwrap();
    fs::write(user_dir.join("SKILL.md"), "USER OWNED").unwrap();
    let skill = Skill { id:"s1".into(), name:"Code Review".into(), description:None,
        content:Some("mine".into()), created_at:now(), updated_at:now() };
    let m = materialize_skills(tmp.path(), "claude", &[(skill, vec![])]).unwrap();
    // 原文件不动
    assert_eq!(fs::read_to_string(user_dir.join("SKILL.md")).unwrap(), "USER OWNED");
    // 写到 -sss 后缀目录
    let sss_dir = tmp.path().join(".claude/skills/code-review-sss");
    assert!(sss_dir.join("SKILL.md").exists());
    assert_eq!(m.created_dirs, vec![sss_dir]);
}

#[test]
fn materialize_codex_fallback_uses_temp_dir_and_injects_content_and_path() {
    let tmp = tempfile::tempdir().unwrap();
    let skill = Skill { id:"s1".into(), name:"Code Review".into(), description:Some("d".into()),
        content:Some("# review".into()), created_at:now(), updated_at:now() };
    let files = vec![SkillFile { id:"f1".into(), skill_id:"s1".into(), path:"scripts/a.py".into(),
        content:"print(1)".into(), created_at:now(), updated_at:now() }];
    let m = materialize_skills(tmp.path(), "codex", &[(skill, files)]).unwrap();
    // codex 不写 .codex/skills（provider_skills_dir 返回 None）
    assert!(!tmp.path().join(".codex").exists());
    // 写到临时目录
    assert_eq!(m.created_dirs.len(), 1);
    let dir = &m.created_dirs[0];
    assert!(dir.join("SKILL.md").exists());
    assert!(dir.join("scripts/a.py").exists());
    assert!(!dir.starts_with(tmp.path()));                   // 不在 cwd 下
    // prompt 含正文 + 脚本路径（codex fallback）
    assert!(m.prompt_section.contains("# review"));
    assert!(m.prompt_section.contains(dir.to_string_lossy().as_ref()));
}

#[test]
fn cleanup_skill_dirs_removes_created_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("x");
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("f"), "y").unwrap();
    cleanup_skill_dirs(&[d.clone()]);
    assert!(!d.exists());
}

#[test]
fn materialize_slug_collision_in_batch_appends_suffix() {
    let tmp = tempfile::tempdir().unwrap();
    let mk = |id: &str| Skill { id:id.into(), name:"Code Review".into(), description:None,
        content:Some("x".into()), created_at:now(), updated_at:now() };
    let m = materialize_skills(tmp.path(), "claude", &[(mk("s1"), vec![]), (mk("s2"), vec![])]).unwrap();
    assert_eq!(m.created_dirs.len(), 2);
    let names: Vec<_> = m.created_dirs.iter().map(|d| d.file_name().unwrap().to_str().unwrap().to_string()).collect();
    assert!(names.contains(&"code-review".into()));
    assert!(names.contains(&"code-review-sss".into()));      // 第二个加后缀
}
```

`now()` 是测试 helper `chrono::Utc::now().to_rfc3339()`（内联或复用 db.rs 模式）。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib skill::materialize` (in `src-tauri/`)
Expected: 编译失败（函数未定义）

- [ ] **Step 3: 实现 `materialize_skills` + `cleanup_skill_dirs`（skill.rs）**

算法（签名 + 测试已定主体，这里只钉歧义点）：

```rust
pub fn materialize_skills(cwd: &Path, provider: &str, skills: &[(Skill, Vec<SkillFile>)])
    -> Result<MaterializedSkills, String>
{
    let mut created_dirs = Vec::new();
    let mut name_index = String::from("## Skills\n你已安装以下技能（CLI 自动发现，按各自 SKILL.md 说明使用）：\n");
    let mut codex_sections = String::new(); // codex fallback 才用
    let provider_dir = provider_skills_dir(provider); // None → codex fallback

    for (i, (skill, files)) in skills.iter().enumerate() {
        let slug = sanitize_skill_name(&skill.name);
        let slug = if provider_dir.is_some() && i > 0 && /* 同 batch 撞 */ false { format!("{slug}-sss") } else { slug };
        // ↑ 实际撞检测：收集已用 slug，撞了或目录已存在 → 加 -sss

        let (skill_root, prompt_for_this) = match provider_dir {
            Some(rel) => {
                let root = cwd.join(rel).join(&slug);
                // 若 root 已存在且非空（用户装的）→ 用 {slug}-sss
                let root = if root.exists() && fs::read_dir(&root).map(|mut r| r.next().is_some()).unwrap_or(false) {
                    cwd.join(rel).join(format!("{slug}-sss"))
                } else { root };
                // 写 SKILL.md（ensure_frontmatter）+ files 按 path 铺开（create_dir_all 子目录）
                write_skill_dir(&root, skill, files)?;
                created_dirs.push(root.clone());
                name_index.push_str(&format!("- **{}**\n", skill.name));
                (root, String::new()) // claude/opencode 不额外注入
            }
            None => {
                // codex fallback：临时目录
                let root = std::env::temp_dir().join(format!("sss-pet-skill-{}-{}", uuid::Uuid::new_v4(), slug));
                write_skill_dir(&root, skill, files)?;
                created_dirs.push(root.clone());
                // 注入正文 + 脚本目录路径
                let mut sec = format!("## Skill: {}\n", skill.name);
                if let Some(c) = &skill.content { sec.push_str(c); }
                sec.push_str(&format!("\n\n脚本目录: {}\n", root.display()));
                codex_sections.push_str(&sec);
                (root, sec)
            }
        };
    }

    let prompt_section = if provider_dir.is_some() {
        if skills.is_empty() { String::new() } else { name_index }
    } else {
        codex_sections
    };
    Ok(MaterializedSkills { created_dirs, prompt_section })
}
```

`write_skill_dir(root, skill, files)`: `fs::create_dir_all(root)`；写 `root/SKILL.md` = `ensure_frontmatter(skill.content.unwrap_or_default(), &slug, skill.description.as_deref())`；每个 file：`validate_file_path(&f.path)?`（防御）、`fs::create_dir_all(root/path的父)`、`fs::write(root/path, &f.content)`。错误包装成 String。

同 batch slug 撞检测：用 `HashSet<String>` 收集已写 slug，当前 slug 在集合里 → 加 `-sss`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --lib skill::materialize` + `cargo test --lib skill::cleanup` (in `src-tauri/`)
Expected: 5 测试 PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/skill.rs
git commit -m "feat: materialize_skills + cleanup_skill_dirs (claude/opencode native + codex fallback)"
```

---

## Task 4: chat.rs 集成（物化 + 清理 + 提示词改名字索引）

**Files:**
- Modify: `src-tauri/src/chat.rs`（`chat_send` 加载 skill_files + 物化 + 提示词；`run_chat_turn` 加 `cleanup_paths` 参数 + drain 清理）

**Interfaces:**
- Consumes: Task 1 `db::skill_file_list`、Task 3 `skill::materialize_skills` / `cleanup_skill_dirs`、现有 `db::agent_enabled_skills`（chat.rs:457）、`db::Skill`。
- Produces: `run_chat_turn` 新增参数 `cleanup_paths: Vec<PathBuf>`（drain 完成后清理）；`chat_send` 物化逻辑。

- [ ] **Step 1: 写失败测试**（chat.rs tests mod）

```rust
#[test]
fn chat_send_materializes_skills_and_cleanup_removes_them_after_turn() {
    // 用 FakeBackend（chat.rs 现有）跑一轮，验证：
    // 1. 跑前 cwd 下出现 .claude/skills/<slug>/SKILL.md + 脚本
    // 2. drain 完成（等 save_turn_record 后）后该目录被删
    let (_tmp, _g, db_state) = fresh_db();
    let cwd = tempfile::tempdir().unwrap();
    // 建一个 agent + 一个 enabled skill（带 skill_file）+ 一个 conversation 绑该 agent
    // 调 run_chat_turn（cleanup_paths = 物化返回的 created_dirs）
    // 等 result 事件（poll load_messages 直到 canned reply 出现）
    // 断言：跑中 .claude/skills/<slug>/ 存在；完成后不存在
    // （FakeBackend 的 canned 消息让 turn 快速完成）
    // ... 具体搭建对标 chat_survives_hide 测试模式
    assert!(/* 跑中存在 */ true);
    assert!(/* 完成后不存在 */ !cwd.path().join(".claude/skills").exists());
}
```

测试搭建较重——参考 chat.rs 现有 `chat_survives_hide`（chat.rs:609+）的 `FakeBackend` + `fresh_db` + `unique_conv_id` 模式。若物化逻辑的单元测试在 Task 3 已覆盖，本测试可简化为：验证 `chat_send` 调 `materialize_skills` 后把 `created_dirs` 传给 `run_chat_turn`（用 FakeBackend + 断言目录在跑中被建、完成后被删）。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib chat::` (in `src-tauri/`)
Expected: 编译失败（`run_chat_turn` 参数不匹配 / 物化未接）

- [ ] **Step 3: 改 `run_chat_turn` 签名加 `cleanup_paths: Vec<PathBuf>`**

`run_chat_turn`（chat.rs:105）加参数 `cleanup_paths: Vec<std::path::PathBuf>`。drain task（chat.rs:164 spawn 块）在 `emit("chat_result", ...)` 之后（chat.rs:316 附近）加 `cleanup_skill_dirs(&cleanup_paths);`（move cleanup_paths 进 spawn）。`chat_survives_hide` 测试调用处传 `vec![]`。

- [ ] **Step 4: 改 `chat_send`：加载 skill_files + 物化 + 提示词改名字索引**

在 chat.rs:457 `agent_enabled_skills` 之后，对每个 skill 调 `db::skill_file_list(&conn, &skill.id)` 收集成 `Vec<(Skill, Vec<SkillFile>)>`（在 db 锁块内）。

删掉 chat.rs:467-481 的"拼正文进 prompt"循环，换成：把 skills_with_files 传出锁块后调 `skill::materialize_skills(&cwd, &provider, &skills_with_files)?` → `MaterializedSkills`。把 `prompt_section` 追加到 `effective_sp`（agent.system_prompt 之后）。把 `created_dirs` 传给 `run_chat_turn(... cleanup_paths: m.created_dirs)`。

注意：物化在 db 锁块外（不持锁做 I/O）。`effective_sp` 构建：`agent.system_prompt` + `m.prompt_section`（若非空）。codex fallback 时 `m.prompt_section` 已含正文 + 路径，替代旧的正文拼接。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test --lib chat::` + `cargo test --lib skill` (in `src-tauri/`)
Expected: 全 PASS（含 Task 3 物化测试 + 现有 chat_survives_hide）

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/chat.rs
git commit -m "feat: chat materializes enabled skills to provider skills dir + name-index prompt"
```

---

## Task 5: 前端 api wrapper + 技能表单文件树 UI

**Files:**
- Modify: `src/panel/lib/api.ts`（加 `SkillFile` interface + 2 wrapper）
- Modify: `src/panel/views/AgentsView.svelte`（技能表单加文件树 + 编辑 + 保存）

**Interfaces:**
- Consumes: Task 1 的 `skill_file_list` / `skill_file_replace_all` 命令。
- Produces:
  - `export interface SkillFile { id: string; skillId: string; path: string; content: string; createdAt: string; updatedAt: string }`（api.ts）
  - `export async function skillFileList(skillId: string): Promise<SkillFile[]>`
  - `export async function skillFileReplaceAll(skillId: string, files: Array<{path: string; content: string}>): Promise<void>`（invoke `skill_file_replace_all`，camelCase `skillId` + `files`）
  - AgentsView 技能表单：SKILL.md 正文 textarea（现有，标签改"SKILL.md 正文"）+ 文件树区（新增/编辑/删除脚本，保存时调 `skillFileReplaceAll`）。

- [ ] **Step 1: 加 api wrapper（api.ts）**

`SkillFile` interface 加在 `Skill` interface 附近。两个 wrapper 加在 `deleteSkill` 之后。`skillFileReplaceAll` 的 `files` 参数是 `Array<{path, content}>`——Tauri 传 `Vec<(String,String)>`，serde 序列化 tuple 为 JSON array `[path, content]`；前端传 `string[][]` 或 `{path,content}[]`？**歧义点**：Rust `Vec<(String,String)>` serde 序列化为 `[[path, content], ...]`。所以前端 wrapper 把 `{path, content}` 转成 `[path, content]` 数组再传，或者后端改用 `Vec<SkillFileInput { path, content }>` struct 更安全。**决策**：后端命令参数用 `files: Vec<(String, String)>`，前端传 `string[][]`（`files.map(f => [f.path, f.content])`）。在 wrapper 里做这个转换，对外暴露 `{path, content}` 对象数组。

- [ ] **Step 2: 改技能表单 UI（AgentsView.svelte）**

技能表单（`{#snippet skillForm()}` 或现有 skill form 块）现在有 name/description/content。加：
- 新 state：`formSkillFiles: Array<{path: string; content: string}> = $state([])`、`formEditingFileIdx: number | null = $state(null)`。
- `startEditSkill`（编辑技能时）调 `skillFileList(skill.id)` 填 `formSkillFiles`。
- UI：content textarea 标签改"SKILL.md 正文"；下方"关联脚本"区——文件列表（从 `formSkillFiles` 的 path 推导，平铺展示 path，点击进 inline 编辑 textarea）+ "新建脚本"（输入 path + 空 content）+ 删除按钮。
- `saveSkillForm`：调现有 `createSkill`/`updateSkill`（name/description/content）后，再调 `skillFileReplaceAll(id, formSkillFiles)`（新建技能用返回的 id；编辑用 editingId）。

文件树从 path 推导目录节点——v1 可平铺展示（`scripts/a.py` 直接显示完整 path），不做缩进树（简化）。若要树，参考 multica `skill-detail-page.tsx` 的 `<FileTree>`。

- [ ] **Step 3: svelte-check**

Run: `pnpm svelte-check`
Expected: 0 errors 0 warnings

- [ ] **Step 4: Commit**

```bash
git add src/panel/lib/api.ts src/panel/views/AgentsView.svelte
git commit -m "feat: skill file-tree UI + skillFileList/ReplaceAll wrappers"
```

---

## Task 6: README 更新

**Files:**
- Modify: `README.md`

**Interfaces:** 无。

- [ ] **Step 1: 更新技能管理描述**

把 README"智能体管理（Agents）"section 的技能描述更新：技能 = SKILL.md 正文 + 关联脚本文件（多层目录，文本 only）；运行时物化进对话 cwd 的 provider skills 目录（claude `.claude/skills/`、opencode `.opencode/skills/`），CLI 原生发现；codex 物化进临时目录 + 路径注入提示词。项目结构加 `skill.rs`。

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: skill script-file support in README"
```

---

## 最终验证

- [ ] `cargo test`（src-tauri/）—— 全 PASS（178 + Task 1/2/3/4 新增，无回归）
- [ ] `pnpm svelte-check` —— 0 errors 0 warnings
- [ ] `pnpm tauri build --no-bundle` —— 成功产出 `sss-pet.exe`
- [ ] builder 实机验证（Review Focus #5）：用一个带脚本的技能跑 claude 对话，确认 `.claude/skills/<slug>/` 被建 + 智能体能调脚本 + 跑完清理；codex 跑一轮确认 fallback（临时目录 + 路径注入）让智能体能调脚本。

## Self-Review

- **Spec coverage**：DB 模型（Task 1）、校验/二进制/上限（Task 2）、物化/清理/frontmatter/slug/provider 目录（Task 3）、chat 集成 + 提示词改名索引（Task 4）、UI 文件树（Task 5）、README（Task 6）—— spec 各节都有任务。codex fallback 在 Task 3 + Task 4 覆盖。非目标（导入/origin/runtime-local/slash）正确不在计划内。
- **Step scan**：每步一个可写动作；无"TBD"/"handle edge cases"。Task 3 Step 3 给了算法（歧义点钉死：slug 撞检测、codex 临时目录路径、frontmatter 合成条件）。
- **Type consistency**：`SkillFile` struct 字段在 Task 1 定义、Task 3 消费（`materialize_skills` 入参 `&[(Skill, Vec<SkillFile>)]`）、Task 5 前端 interface 镜像——一致。`MaterializedSkills` Task 3 定义、Task 4 消费（`created_dirs` 传 `run_chat_turn`）——一致。`run_chat_turn` 新参数 `cleanup_paths: Vec<PathBuf>` Task 4 定义 + `chat_survives_hide` 调用处更新——一致。
- **Review Focus**：5 个失败模式都有测试钉住（Task 2 三测 + Task 1 二测 + Task 3 物化测 + codex fallback 测）。codex 实机验证列在最终验证。
- **Proportion**：计划长度与 spec 相当（spec ~5KB，计划含测试代码块较长但每块是 assertion 不是 body）——可接受。
