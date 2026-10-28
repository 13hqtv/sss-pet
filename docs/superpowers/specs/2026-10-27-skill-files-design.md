# 技能支持脚本文件（对标 multica skill 管理）

日期：2026-10-27
状态：待确认

## 意图与动机

现在一个技能只有 SKILL.md 正文（存在 `skills.content`），chat 跑一轮时把正文拼进系统提示词。真实技能（Claude Code / multica 的技能）常带 supporting scripts——SKILL.md 说明何时怎么调脚本，脚本放在子目录里。当前模型挂不了脚本，技能能力被限死在一段文字。

目标：让技能能关联多个文本脚本文件（支持多层目录），运行时把启用的技能物化成对话 cwd 下 provider 原生的 skills 目录里的真实文件，让 agent CLI 自己发现 + 调用。对标 multica 0.6.5 的 skill 管理模型（已读源码确认）。

## multica 的模型（已读源码，作为对标基准）

- **DB 存，不是磁盘目录存**。`skill` 表一行（`content` = SKILL.md 正文）+ `skill_file` 表 N 行（`{path, content}`，path 斜杠分隔可带子目录，目录从 path 推导不单独存）。`SKILL.md` 路径保留（不作为 file 行）。纯文本，二进制按扩展名拒绝，单文件 1MiB / 总 8MiB / 256 个上限。
- **运行时不塞正文进提示词**。把每个启用技能物化成 `<cwd>/.<provider>/skills/<slug>/SKILL.md` + 脚本按相对路径铺开，CLI 原生发现。提示词只放名字索引（`## Skills\n- **名字**`）。
- **UI**：技能详情页 overview（编辑 name/description/SKILL.md 正文）+ files（文件树从 path 推导，保存时整组 file 全量替换）。

## 目标

1. 技能可关联多个文本脚本文件，支持多层目录（path 字符串推导目录树）。
2. 运行时把启用的技能物化进对话 cwd 的 provider 原生 skills 目录（claude → `.claude/skills/`、opencode → `.opencode/skills/`），CLI 原生发现 + 调脚本。
3. 系统提示词从"塞技能正文"改为"只放名字索引"（跟 multica 一致）。
4. 技能表单加文件树 + 脚本在线编辑。
5. 文本文件 only，二进制拒绝，容量上限。

## 非目标（v1 不做）

- 不做技能从 URL / archive / GitHub 导入（multica 的 clawhub / skills.sh / zip 导入）。
- 不做 origin / provenance 追踪（multica 的 `config.origin` JSONB）。
- 不做 runtime-local 技能发现（扫描 `~/.claude/skills/`、`~/.agents/skills/`）。
- 不做 bundle hash 缓存（单用户本地，不需要）。
- 不做 slash-command 技能（`[/label](slash://skill/<id>)`）。
- 不做技能市场 / 搜索。

## 反目标

- 不把技能文件存成 app 拥有的磁盘目录（multica 不是这么做的；DB 存更简单、好管理）。
- 不把脚本内容塞进系统提示词（长、重复；CLI 原生发现就够）。
- 不支持二进制文件（图片 / 字体 / 压缩包 / 可执行 / 办公文档等）。

## 约束

- sss-pet：Tauri v2 + Rust + SQLite (rusqlite) + Svelte 5，单用户本地。
- 三个 provider：claude / codex / opencode。
- 对话 cwd 是用户真实项目目录（不是临时任务目录——这点跟 multica 不同，影响清理策略，见下）。
- 现有 `skills` 表已有 `content` 字段（SKILL.md 正文），保留不动。

## 数据模型

`skills` 表不动（`id, name, description, content, created_at, updated_at`）。`content` = SKILL.md 正文。

新增 `skill_files` 表（在 `db.rs` 的 `migrate()` `execute_batch` 里）：

```sql
CREATE TABLE IF NOT EXISTS skill_files (
    id TEXT PRIMARY KEY,
    skill_id TEXT NOT NULL,
    path TEXT NOT NULL,              -- 相对路径，斜杠分隔，可带子目录
    content TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(skill_id, path),
    FOREIGN KEY (skill_id) REFERENCES skills(id) ON DELETE CASCADE
);
```

`agent_skills` 关联表不动。

新增 `SkillFile` struct + CRUD（`db.rs`）：`skill_file_list(conn, skill_id) -> Vec<SkillFile>`、`skill_file_replace_all(conn, skill_id, files: Vec<(String,String)>)`（全量替换：删旧的、写新的，事务内）。整组替换对标 multica 的保存语义，比逐个 upsert/delete 简单。

## 路径校验 + 二进制拒绝 + 上限

- path 拒绝空、绝对路径（`/` 开头或带盘符 `C:`）、`..` 段。
- `SKILL.md`（`filepath.Clean` 后忽略大小写等于）是保留路径——不能作为 file 行（正文在 `skills.content`），传入时跳过。
- 目录从 path 推导，不存目录行。
- 二进制检测：扩展名黑名单（图片 / 字体 / 压缩包 / 办公文档 / 媒体 / 可执行 / 数据库）+ 内容含 NUL 字节判二进制。
- 上限：单文件 1 MiB，单技能总 8 MiB，最多 256 个文件。超限报错。

## 运行时物化（核心）

在 `chat.rs` 的 `chat_send` / `run_chat_turn` 跑 agent 之前，对当前智能体启用的每个技能：

1. 算 slug = `sanitize_skill_name(skill.name)`（小写 + 非字母数字替换为 `-`，对标 multica `sanitizeSkillName`）。
2. 解析 provider 的 skills 目录（相对对话 cwd）：
   - claude → `.claude/skills/`
   - opencode → `.opencode/skills/`
   - codex → **待 builder 验证**：codex app-server 是否原生发现 skills 目录？若否，codex 走 fallback：物化进临时目录 + 把路径注入提示词（仅 codex）。
3. 写 `<skills_dir>/<slug>/SKILL.md`（从 `skills.content`，必要时合成 frontmatter，见下）+ 每个 `skill_file` 按 path 铺开（`fs::create_dir_all` 建子目录）。
4. 跑完 turn 后清理：删掉本轮写的 `<slug>/` 目录（best-effort，drain task 完成后）。若崩溃留下残余，无害（用户可手动删）。

**不覆盖用户已装的技能**：写之前检查 `<skills_dir>/<slug>/` 是否已存在且非空。若是（用户自己装的技能），用 `<slug>-sss/` 代替（对标 multica 的 `allocateCollisionFreeSkillDir`）。清理时只删本轮创建的目录（记录创建的路径列表）。

**frontmatter 合成**：SKILL.md 写盘时，若 `skills.content` 无 YAML frontmatter，前缀合成 `---\nname: {slug}\ndescription: {description}\n---\n`，确保 CLI 发现时显示对的 name/description。

## 提示词改动

`chat.rs` 现在把每个启用技能的正文拼进 system_prompt（`## Skill: {name}\n{description}\n{content}`）。改为只放名字索引：

```
## Skills
你已安装以下技能（CLI 自动发现，按各自 SKILL.md 的说明使用）：
- **{name1}**
- **{name2}**
```

正文不再塞提示词——CLI 从物化的 `.claude/skills/<slug>/SKILL.md` 自己读。

**前提**：claude / opencode 原生发现 `.claude/skills/` / `.opencode/skills/`。codex 待验证（见上）。若某 provider 不发现，该 provider 的技能正文会丢失——builder 验证 + fallback。

## UI（AgentsView 技能表单）

现在技能表单有 name / description / content (textarea)。改为：

- SKILL.md 正文：保留现有 textarea（标签改"SKILL.md 正文"）。
- 文件区（新增）：文件树（从 `skill_file.path` 推导目录节点）+ 点击文件 inline 编辑（textarea）+ 新建文件（输入 path + content）/ 删除文件。
- 保存技能时：name / description / content 走现有 `skill_update`；files 走新的 `skill_file_replace_all`（全量替换）。

二进制文件 UI 不支持上传（v1 文本 only）。

## 我已定的设计决策（state as my call）

1. **DB 存**（不存磁盘目录）——对标 multica。
2. **物化进对话 cwd 的 provider skills 目录**（用户选的 A）。
3. **跑完清理**（best-effort，只删本轮写的目录）。
4. **提示词换名字索引**（不塞正文）。
5. **frontmatter 合成**（写盘时若无则补）。
6. **文本 only + 二进制拒绝 + 上限**（1MiB/8MiB/256）。
7. **v1 不做** 导入 / origin / runtime-local 发现 / slash-command。
8. **现有技能无迁移**——`skill_files` 空表，老技能物化时只有 SKILL.md、无脚本，照常工作。

## 留给 builder 的

- **codex skills 发现验证**：codex app-server 是否读 skills 目录？若否，codex fallback（临时目录 + 注入路径到提示词，仅 codex）。
- **物化实现位置**：在 `run_turn` 之前写文件、drain 完成后清理。cwd 是 `conv.cwd`（PathBuf）。需要拿到启用技能列表（`db::agent_enabled_skills` 已有）+ 它们的 `skill_files`。
- **slug 冲突处理**：参考 multica `allocateCollisionFreeSkillDir`（context.go:175）。记录本轮创建的目录路径，清理时只删这些。
- **文件树 UI 组件**：Svelte 5，从 path 推导树、新增 / 编辑 / 删除文件、inline 文本编辑器。参考 multica `packages/views/skills/components/skill-detail-page.tsx` 的 `<FileTree>`。
- **校验 Rust 实现**：path 校验（无绝对 / 无 `..` / 非 `SKILL.md`）+ 二进制检测（扩展名黑名单 + NUL）+ 上限。参考 multica `server/internal/skill/binary.go` + `reserved.go`。
- **新 Tauri 命令**：`skill_file_list`、`skill_file_replace_all`，注册进 `invoke_handler`。
- **前端 api wrapper**：`skillFileList`、`skillFileReplaceAll`。

## 迁移 / 向后兼容

- `skill_files` 新表，现有技能不受影响（物化时只有 SKILL.md、无脚本）。
- `agent_skills` 关联不动。
- 提示词从"塞正文"变"名字索引"——现有技能正文改为通过 CLI 发现到达智能体。前提是 claude/opencode 原生发现 skills 目录（builder 验证）。
- 不需要数据迁移脚本。

## 涉及文件（预估）

- `src-tauri/src/db.rs`：`skill_files` 表 + `SkillFile` struct + 2 CRUD 函数 + 测试。
- `src-tauri/src/chat.rs`：物化逻辑（写盘 + 清理）+ 提示词改名字索引。
- `src-tauri/src/lib.rs`：注册 `skill_file_list` / `skill_file_replace_all` 命令。
- 新模块 `src-tauri/src/skill_files.rs`（或加进 db.rs / 一个 skill runtime 模块）：path 校验 + 二进制检测 + 物化/清理 helper。
- `src/panel/lib/api.ts`：`SkillFile` interface + 2 wrapper。
- `src/panel/views/AgentsView.svelte`：技能表单加文件树 UI。
- `README.md`：更新技能管理描述。
