# 模型选择 / cwd / 流式处理过程 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在已发布的 v0.1.0 桌面宠物上,加(1)对话级 model+cwd(创建时选、不可变,model 从 agent 探测列表选)、(2)隐藏 agent 子进程的 cmd 窗口、(3)对话实时折叠展示 agent 的思考/工具调用/工具结果(重开历史也看得到)。

**Architecture:** `Conversation` 加 `model`+`cwd` 字段(创建时定),`messages` 加 `kind` 列(持久化消息类型供重开看);新增 `agent::models::list_models(provider)` 按 agent 各自机制探测(opencode `opencode models`、claude stream-json `list_models` 控制请求、codex `codex debug models`);`exec::spawn` 加 `CREATE_NO_WINDOW`;对话层把 `conv.model`/`conv.cwd` 接进 `ExecOptions`/`run_turn`;ChatView 把思考/工具块做成可折叠(实时+历史)。

**Tech Stack:** Tauri v2、Rust(tokio/serde/rusqlite)、Svelte 5;本地 claude/codex/opencode CLI。

**Spec:** `docs/superpowers/specs/2026-10-22-model-cwd-streaming-design.md`(并参考 `2026-10-21-desktop-pet-design.md`)。

## Global Constraints

- Windows 11;Tauri v2;Rust + Svelte 5。本地有 claude 2.1 / codex 0.154 / opencode 1.18。
- 便携数据(exe 同级,只读回退 %APPDATA%);`.superpowers/` git-ignored;scoped `git add src/ src-tauri/`;在 master 上做,不切分支。
- 界面文案中文。
- 现有接口(勿改签名除非本计划明示):`Conversation{id,provider,session_id,title,created_at}`、`StoredMessage{role,content}`、`ExecOptions{cwd,model:Option<String>,resume_session_id,timeout}`、`run_turn(backend,conv,history,new_prompt,cwd)->(Session,Arc<Mutex<TurnRecord>>)`、`exec::spawn(cmd,args,env,cwd)->io::Result<Child>`、三 backend 已接 `opts.model`。

## Review Focus

1. **CREATE_NO_WINDOW 真把控制台压住**(改动2的核心)→ Task 2 验证 spawn 不弹窗。
2. **探测失败→手填兜底**(probe 不该挡住选模型)→ Task 3 测 probe 出错返空、Task 5 UI 空时能手填。
3. **重开历史对话看到思考/工具块**(用户明确要的)→ Task 1 测 kind 持久化 round-trip + Task 6 渲染历史 kind。
4. **所选 model 真到 agent**(conv.model→ExecOptions.model→backend --model)→ Task 4 测 resume_or_replay 带 model。
5. **cwd 真用到且不可变**(conv.cwd→run_turn cwd→agent 在该目录跑)→ Task 4 测 cwd 传递。

## File Structure

- `src-tauri/src/db.rs`(改:migration + `StoredMessage.kind` + `Conversation.model/cwd` + CRUD)
- `src-tauri/src/agent/conversation.rs`(改:`Conversation`+model+cwd;`StoredMessage`+kind;`resume_or_replay` 设 opts.model)
- `src-tauri/src/agent/exec.rs`(改:`CREATE_NO_WINDOW`)
- `src-tauri/src/agent/models.rs`(建:`list_models(provider)` per-agent 探测 + 手填兜底)
- `src-tauri/src/chat.rs`(改:`chat_send` 用 conv.model + conv.cwd)
- `src-tauri/src/lib.rs`(改:注册 `list_models`;`new_conversation` 签名)
- `src/panel/views/ChatView.svelte`(改:新建对话对话框 + 折叠渲染)
- `src/panel/views/SettingsView.svelte`(改:每 agent 默认 model)
- `src/panel/lib/api.ts`(改:`list_models`/`new_conversation` wrapper)

---

## Task 1: Schema 迁移 + Conversation/StoredMessage 字段 + db CRUD

**Files:** Modify `src-tauri/src/db.rs`, `src-tauri/src/agent/conversation.rs`. Test: `db.rs #[cfg(test)]`.

**Interfaces:**
- Produces: `Conversation` 加 `pub model: Option<String>` + `pub cwd: String`;`StoredMessage` 加 `pub kind: Option<String>`;`db::migrate` 加三列;`save_conversation`/`new_conversation(provider,model,cwd)`/`save_turn_record` 存 kind;`load_messages` 读 kind。

- [ ] **Step 1: 写失败测试**(db.rs tests):
  - `migrate` 跑两次不报错(幂等)。
  - `PRAGMA table_info(conversations)` 含 `model`/`cwd`;`table_info(messages)` 含 `kind`。
  - round-trip:`new_conversation("claude", Some("sonnet"), "/tmp")` → `load_conversation` 的 model==Some("sonnet")、cwd=="/tmp"。
  - `save_turn_record` 存含各 kind 的 TurnRecord → `load_messages` 回来的 `StoredMessage.kind` 分别==`Text`/`Thinking`/`ToolUse`/`ToolResult`。
  - 旧行回填:插入一条 kind=NULL 的 message → `load_messages` 返回 kind 按 role 推断(assistant→`Text`)。
- [ ] **Step 2: `cargo test db` → FAIL。**
- [ ] **Step 3: 实现** — `migrate` 用 `PRAGMA table_info` 检查后 `ALTER TABLE conversations ADD COLUMN model TEXT NULL` / `ADD COLUMN cwd TEXT NOT NULL DEFAULT '.'` / `ALTER TABLE messages ADD COLUMN kind TEXT NULL`;`StoredMessage`+`kind`;`Conversation`+`model`+`cwd`;`save_conversation`/`new_conversation`/`save_turn_record`(存 kind)/`load_messages`(读 kind, NULL 按 role 推断)。
- [ ] **Step 4: `cargo test db` → PASS。**
- [ ] **Step 5: 提交** `git commit -m "feat: schema migration — conversation model/cwd + message kind"`。

## Task 2: 隐藏 cmd 窗口(CREATE_NO_WINDOW)

**Files:** Modify `src-tauri/src/agent/exec.rs`. Test: `exec.rs #[cfg(test)]` + 手动。

**Interfaces:** Produces: `exec::spawn` 在 Windows 上设 `creation_flags(0x08000000)`,所有经它的 spawn(三 backend + 模型探测)都不弹控制台。

- [ ] **Step 1: 写测试** — `exec::spawn(cargo, ["--version"], …)` 成功返回 Child(确认 flag 加上后仍能 spawn);Windows 下断言 `Command` 的 creation_flags 含 `0x08000000`(可借 `std::os::windows::process::CommandExt` 在构造时设、单测里检查)。
- [ ] **Step 2: `cargo test exec` → FAIL。**
- [ ] **Step 3: 实现** — `#[cfg(windows)] use std::os::windows::process::CommandExt;` 在 `spawn` 里 `#[cfg(windows)] { cmd.creation_flags(0x0800_0000); }`(`0x08000000` = `CREATE_NO_WINDOW`)。
- [ ] **Step 4: `cargo test exec` → PASS;`cargo build --release` 过。**
- [ ] **Step 5: 手动验证 Review Focus #1** — 跑 dev/build exe,发一轮对话(或触发一次 spawn),确认**没有**控制台黑窗弹出;若 GUI 不可行,代码级确认 flag 设对。
- [ ] **Step 6: 提交** `git commit -m "feat: hide agent subprocess console window (CREATE_NO_WINDOW)"`。

## Task 3: 模型探测 `list_models(provider)`

**Files:** Create `src-tauri/src/agent/models.rs`;Modify `src-tauri/src/agent/mod.rs`(`pub mod models;`)、`src-tauri/src/lib.rs`(注册 `list_models` 命令)。Test: `models.rs #[cfg(test)]` + fixtures。

**Interfaces:**
- Consumes: `exec::spawn`(继承 CREATE_NO_WINDOW)、`BackendConfig`。
- Produces: `pub async fn list_models(provider: &str, cfg: &BackendConfig) -> Result<Vec<String>, BackendError>`;Tauri 命令 `list_models(provider: String) -> Vec<String>`(出错/空返 `vec![]`,前端兜手填)。

- [ ] **Step 1: 写失败测试**(用 fixture,不打真 CLI):
  - opencode:`opencode models` 输出 fixture → 解析出模型名列表。
  - claude:stream-json `list_models` 的 `control_response` fixture → 解析 models 列表(参考 multica `claude_models.go::claudeControlResponse`)。
  - codex:`codex debug models` 输出 fixture(`codexDebugModelsResponse`,`models[].slug`/`display_name`)→ 解析列表(参考 multica `thinking.go::discoverCodexCatalog`)。
  - 出错/空 → 返回 `Ok(vec![])`(兜底手填)。
- [ ] **Step 2: `cargo test models` → FAIL。**
- [ ] **Step 3: 实现** — `list_models` 按 provider 分派:opencode `spawn(["models"])` 解析;claude 起一个 discovery stream-json 会话发 list_models 控制请求(单独解析 `control_response`,不复用 `stream_json::parse_line`);codex `spawn(["debug","models"])` 解析。全部经 `exec::spawn`。出错/空 → `vec![]`。
- [ ] **Step 4: `cargo test models` → PASS。**
- [ ] **Step 5: 真 CLI 验证(best-effort,#[ignore])** — 对 opencode 跑真 `opencode models` 过一遍;claude/codex 探测若重则代码级 + fixture 为准。
- [ ] **Step 6: 提交** `git commit -m "feat: per-agent model discovery (opencode/claude/codex) + manual fallback"`。

## Task 4: 对话层接 model + cwd

**Files:** Modify `src-tauri/src/agent/conversation.rs`、`src-tauri/src/chat.rs`。Test: `conversation.rs #[cfg(test)]`。

**Interfaces:**
- Consumes: Task 1 的 `Conversation.model`/`cwd`;`run_turn(…, cwd)`。
- Produces: `resume_or_replay` 设 `opts.model = conv.model.clone()`;`chat_send` 用 `conv.cwd`(传给 `run_turn`),不再用 `config::resolve_agent_cwd()`。

- [ ] **Step 1: 写失败测试**:
  - `resume_or_replay` 对 `conv.model=Some("sonnet")` → 返回的 `ExecOptions.model==Some("sonnet")`;`conv.model=None` → `None`。
  - `run_turn` 收到的 cwd == `conv.cwd`(用一个 FakeBackend + 断言传给 execute 的 opts.cwd)。
- [ ] **Step 2: `cargo test conversation` → FAIL。**
- [ ] **Step 3: 实现** — `resume_or_replay` 设 `opts.model`;`chat.rs::chat_send` `load_conversation` 后用 `conv.cwd.clone()` 作 `run_turn` 的 cwd(去掉 `config::resolve_agent_cwd()` 调用)。
- [ ] **Step 4: `cargo test conversation` → PASS;`cargo test chat` 仍绿(chat_survives_hide 等)。**
- [ ] **Step 5: 提交** `git commit -m "feat: wire conversation model + cwd into the chat flow"`。

## Task 5: 新建对话对话框(agent + model 探测 + cwd)+ 设置默认 model

**Files:** Modify `src/panel/views/ChatView.svelte`、`src/panel/views/SettingsView.svelte`、`src/panel/lib/api.ts`。Verify: build + 手动。

**Interfaces:**
- Consumes: `list_models(provider)`(Task 3)、`new_conversation(provider, model, cwd)`(Task 1)、`Config.providers[agent].default_model`。
- Produces: 新建对话时选 agent→探测 model 列表→选 model(空能手填)→设 cwd(默认填全局 working_dir)→创建;SettingsView 每 agent 配默认 model。

- [ ] **Step 1: `api.ts`** 加 `listModels(provider)` + `newConversation(provider, model, cwd)` wrapper。
- [ ] **Step 2: `ChatView` 新建对话对话框** — provider 选择→`listModels(provider)`→model 下拉(空则文本输入手填)+ cwd 输入(默认 `config.working_dir`);确定→`newConversation(provider, model, cwd)`;新对话的 provider+model+cwd 不可改(UI 不暴露中途换)。
- [ ] **Step 3: `SettingsView`** — 每 agent:探测+选一个默认 model,存 `Config.providers[agent].default_model`;新建对话时预填。
- [ ] **Step 4: 验证** — `cargo check` + `pnpm build` + `pnpm check` 干净;跑 dev,新建一个对话选 model+cwd,确认创建后 `load_conversation` 带 model+cwd(可在对话列表/详情显示)。
- [ ] **Step 5: 提交** `git commit -m "feat: new-conversation dialog (agent+model probe+cwd) + per-agent default model"`。

## Task 6: ChatView 折叠渲染思考/工具块(实时 + 历史)

**Files:** Modify `src/panel/views/ChatView.svelte`(+ 其 CSS)。Verify: build + 手动。

**Interfaces:**
- Consumes: `chat_message` 事件(各 MessageKind,Task 13/15 透传)、`load_messages` 带 `kind`(Task 1)。
- Produces: 思考(灰、默认折叠)、工具调用("使用工具 X" 条 + 可折叠 input)、工具结果(折叠 output)、状态行、错误红气泡、文字气泡;实时 + 重开历史都这么渲。

- [ ] **Step 1: 渲染分支** — 按 `message.kind`(实时事件 / 历史 `StoredMessage.kind`)分派:Text→气泡;Thinking→折叠块;ToolUse→工具条+折叠 input;ToolResult→折叠结果;Status→状态行;Error→红气泡;kind NULL→当 Text。
- [ ] **Step 2: CSS** — 折叠块(默认 `details`/点击展开)、灰色思考、工具条样式。
- [ ] **Step 3: 历史渲染** — `loadHistory` 回的 `StoredMessage`(带 kind)按同分支渲(Review Focus #3)。
- [ ] **Step 4: 验证** — `pnpm build` + `pnpm check` 干净;跑 dev 发一轮真 agent(或 FakeBackend 触发各 kind),确认思考/工具调用/工具结果实时滚出 + 折叠;重开该对话确认块还在。
- [ ] **Step 5: 提交** `git commit -m "feat: collapsible thinking/tool rendering in chat (live + history)"`。

---

## Self-Review

- **Spec 覆盖**:model+cwd 不可变(T1/T4/T5)、探测 like multica(T3)、隐藏窗(T2)、流式折叠实时+历史(T1 kind+T6)、codex 用 `codex debug models`(T3)、对话层接 model(T4)、探测 spawn 走 exec(T3) 均有任务。
- **类型一致**:`Conversation.model/cwd`、`StoredMessage.kind` 在 T1 定义,T4/T5/T6 消费;`list_models` T3 定义 T5 消费;`new_conversation(provider,model,cwd)` T1 定义 T5 消费。
- **Review Focus 五条**各有任务测试(T2/T3/T1+T6/T4/T4)。
- **比例**:计划以签名+测试断言为主,实现体留给实现者。
