# 桌面宠物助手 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 Windows 11 上做一个低占用的桌面宠物（Tauri + Rust + Svelte）：一个悬浮机器人，姿态即状态指示；点开是一个标签面板，含多 agent 对话（Claude/Codex/OpenCode，本地 CLI、流式、多会话历史）、代办、提醒。

**Architecture:** Tauri v2 多窗口——一个透明置顶的宠物浮层 + 一个可拖可缩的标签面板——之上有 Rust 核心（状态机、活动/空闲检测、照搬 multica 的 Backend/Session 的 agent provider 层、SQLite 持久化、定时器、托盘）。前端 Svelte；Rust 后端持有所有 agent 子进程，向 webview 流式推送统一 Message 事件。

**Tech Stack:** Tauri v2、Rust(stable)、tokio、rusqlite、serde；前端 TypeScript + Svelte + Vite；本地 `claude`/`codex`/`opencode` CLI。

**Spec:** `docs/superpowers/specs/2026-10-21-desktop-pet-design.md`

## Global Constraints

- Windows 11；WebView2 系统自带（打包仍附引导程序）。
- 工具链：Rust stable + Node 20+ + pnpm；集成任务需本机有 `claude`/`codex`/`opencode` CLI。
- 便携：数据目录 = exe 目录；只读则回退 `%APPDATA%\sss-pet`。
- API key 不进二进制、不进 webview；agent 只走本地 CLI 子进程。
- 睡觉态 = 静态帧，不开动画循环（空闲 CPU ≈ 0）。
- 界面文案用中文。
- 仓库分支当前为 `master` 且无提交；首个任务做初次提交，之后每个任务收尾提交。

## Review Focus

spec 隐含、但单个任务的测试未必覆盖、最容易坑到真实用户的五类输入/失败：

1. 宠物长时间空闲后保持静态（≈0 CPU），没有残留的 rAF/定时器。→ Task 7 测试 + Task 17 验证。
2. 机器人周围透明区的点击穿透到后面的窗口，不被吞掉。→ Task 2 手动验证。
3. 面板在流跑到一半时隐藏 → 不丢消息；重开能看到已完成/进行中的文本。→ Task 13/15 测试。
4. 续接对话真带上历史（agent 看到之前几轮，不是新会话）。→ Task 13 测试。
5. exe 目录只读 → 回退 `%APPDATA%` 且写入仍成功。→ Task 14 测试。

## File Structure

```
sss-pet/
├── package.json / pnpm-workspace.yaml / vite.config.ts / svelte.config.js / tsconfig.json
├── index.html                      # 面板入口
├── .gitignore
├── src/                            # 前端 (Svelte)
│   ├── panel/{main.ts, App.svelte}
│   ├── panel/views/{ChatView,TodosView,RemindersView,SettingsView}.svelte
│   ├── panel/lib/{api.ts, stream.ts}
│   ├── pet/{main.ts, Pet.svelte}
├── src-tauri/
│   ├── Cargo.toml / tauri.conf.json / build.rs / icons/
│   └── src/
│       ├── main.rs / lib.rs        # Builder、插件、命令注册
│       ├── window.rs               # 窗口管理（宠物浮层、面板生成/定位/翻转/拖动）
│       ├── tray.rs                 # 托盘图标 + 菜单
│       ├── config.rs               # TOML 配置（便携 + 回退）
│       ├── db.rs                   # SQLite schema + 查询
│       ├── notify.rs               # Windows toast
│       ├── activity.rs             # GetLastInputInfo 空闲检测
│       ├── timers.rs               # 提醒调度 + 跟随活动节流
│       ├── state/{mod.rs, machine.rs}   # 宠物状态机
│       └── agent/
│           ├── mod.rs              # Backend trait、Session、Message、ExecOptions、Result、SupportedTypes、factory
│           ├── exec.rs             # 子进程 spawn + LookPath + env
│           ├── stream_json.rs      # stream-json 解析（claude 等共用）
│           ├── claude.rs / opencode.rs / codex.rs
│           └── conversation.rs     # 多会话 + session_id 续接/历史重放
```

文件按职责切，不按技术层切。Rust 核心可单测的部分（state、activity、agent、db、config）重 TDD；窗口/UI 任务以编译 + 手动/快照验证为主。

---

## Task 1: 项目脚手架

**Files:** Create `package.json`, `pnpm-workspace.yaml`, `vite.config.ts`, `svelte.config.js`, `tsconfig.json`, `index.html`, `.gitignore`, `src/panel/main.ts`, `src/pet/main.ts`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, `src-tauri/build.rs`, `src-tauri/src/main.rs`, `src-tauri/src/lib.rs`, `src-tauri/icons/*`.

**Interfaces:** Produces a runnable `cargo tauri dev` with two configured windows (`pet` 透明置顶无框、`panel` 普通可缩放），定义 `tauri.conf.json` 里两个 window label：`"pet"` 与 `"panel"`（panel 默认 hidden）。

- [ ] **Step 1: 用 `pnpm create tauri-app` 起架（模板 Svelte + TypeScript），命名为 sss-pet。**
- [ ] **Step 2: 在 `src-tauri/tauri.conf.json` 配两个窗口**：`pet`（`transparent:true, decorations:false, alwaysOnTop:true, skipTaskbar:true, resizable:false, width:120, height:120, x:屏右下`）；`panel`（`visible:false, resizable:true, width:380, height:560`）。`app.withGlobalTauri:true`。
- [ ] **Step 3: `src/panel/main.ts`、`src/pet/main.ts` 各挂一个占位 Svelte 组件**（"panel"/"pet" 文字）。
- [ ] **Step 4: 跑 `cargo tauri dev`，确认两个窗口都出现（panel 不可见）。** Expected: dev 启动、无报错、pet 小窗透明置顶。
- [ ] **Step 5: 写 `.gitignore`（`node_modules`、`dist`、`src-tauri/target`、`.DS_Store`），首次提交。**
```bash
git add -A && git commit -m "chore: scaffold tauri+svelte app with pet+panel windows"
```

---

## Task 2: 宠物浮层窗口（透明 + 置顶 + 点击穿透 + 静态机器人）

**Files:** Create `src/pet/Pet.svelte`；Modify `src-tauri/src/lib.rs`（启动时设点击穿透）、`src-tauri/src/window.rs`。

**Interfaces:** Produces `window.rs::set_click_through(label, ignore)`（调 `window.set_ignore_cursor_events(ignore)`），以及 `Pet.svelte` 渲染一个静态机器人 SVG（用 Lucide `bot` 或自绘简单 SVG）。

- [ ] **Step 1: `Pet.svelte` 渲染机器人**：一个 96px 的机器人 SVG，居中；容器透明。先画"待命"静态姿势。
- [ ] **Step 2: `lib.rs` 启动后对 pet 窗口调 `set_ignore_cursor_events(true)`**（整窗穿透），并在 `Pet.svelte` 里给机器人本体一个 `mouseenter/leave`→通过 `invoke('set_pet_click_through', {ignore:false/true})` 切换：鼠标在机器人上时不穿透（可点/可拖），离开恢复穿透。
- [ ] **Step 3: 注册命令 `set_pet_click_through`** in `lib.rs`，调用 `window.rs::set_click_through`。
- [ ] **Step 4: 跑 dev，手动验证 Review Focus #2**：机器人可点；周围透明区点击落到后面窗口。Expected: 穿透行为正确。
- [ ] **Step 5: 提交。** `git commit -m "feat: transparent always-on-top pet overlay with click-through"`

---

## Task 3: 托盘图标 + 菜单

**Files:** Create `src-tauri/src/tray.rs`；Modify `src-tauri/src/lib.rs`、`src-tauri/icons/tray-*`。

**Interfaces:** Produces `tray.rs::build_tray(app) -> tauri::Result<()>`：托盘图标=睡着的机器人；菜单项 `显示/隐藏`、`设置`、`退出`；左键 = 面板隐藏则恢复、否则切换宠物可见；右键 = 菜单。

- [ ] **Step 1: 用 `tauri::tray::TrayIconBuilder` 建托盘**，图标用睡觉态机器人图（临时用 bot 图标占位）。
- [ ] **Step 2: 菜单 `MenuBuilder` 加 `show_hide`/`settings`/`quit`**；`on_tray_icon_event` 处理左键（`show_panel_or_toggle_pet`）；`on_menu_event` 处理三项。
- [ ] **Step 3: `window.rs` 加 `show_panel_or_toggle_pet`**：panel hidden → show+focus；panel visible → hide panel，宠物切可见（若隐藏）。
- [ ] **Step 4: 跑 dev，验证托盘出现、左键恢复/切换、右键菜单三项可用。** Expected: 托盘行为符合设计。
- [ ] **Step 5: 提交。** `git commit -m "feat: system tray with menu and left-click behavior"`

---

## Task 4: 窗口管理——面板生成/定位/翻转 + 双向拖动 + 位置持久化

**Files:** Create `src-tauri/src/window.rs`（补全）；Modify `src-tet/Pet.svelte`（拖动）、`src-tauri/src/config.rs`（占位读写，Task 15 补全）。

**Interfaces:** Produces `window.rs::open_panel_above_pet(app)`：按宠物当前位置 + 离屏边距离，决定面板翻到宠物 上/下/左/右；`persist_window_state()` / `load_window_state()`（pet 位置、panel 尺寸/位置）。Pet 拖动：在 `Pet.svelte` 用 `data-tauri-drag-region` + `start_dragging` 让机器人本体可拖；拖完 `invoke('save_pet_pos', {x,y})`。

- [ ] **Step 1: `open_panel_above_pet`** 计算翻向：默认上方；若上方空间不足面板高度则下方；左右同理按宠物离左右边距离。设 panel 的 `position` + `show` + `set_focus`。
- [ ] **Step 2: Pet 拖动**：机器人根元素加 `data-tauri-drag-region`；`mouseup` 后 `invoke('save_pet_pos',{x,y})`。面板窗口 `tauri.conf` 已 `resizable:true`；面板顶栏加 `data-tauri-drag-region`。
- [ ] **Step 3: `config.rs` 占位** `save_pet_pos`/`load_window_state`/`save_panel_state`（先存内存 HashMap，Task 15 接 TOML）。
- [ ] **Step 4: 跑 dev，手动验证**：点机器人面板从上方弹出；把宠物拖到屏顶，面板翻到下方；拖宠物后位置记住（重启 dev 仍在上次位置，待 Task 15 持久化落盘后真正生效）。Expected: 翻转 + 拖动 + 尺寸调整都正常。
- [ ] **Step 5: 提交。** `git commit -m "feat: panel spawn/flip, draggable pet+panel, position state"`

---

## Task 5: 宠物状态机（Rust，TDD）

**Files:** Create `src-tauri/src/state/mod.rs`, `src-tauri/src/state/machine.rs`。

**Interfaces:**
- Produces `PetState`（枚举：`Sleep, Standby, Thinking, HasSomething, NeedsYou`）。
- Produces `Signal`（枚举：`IdleFor(Duration), AgentBusy, AgentDone, ReminderDue, Error, NeedsInput, UserActivity, FocusMode(bool), PanelOpened, Dismissed`）。
- Produces `StateMachine::new() -> Self`、`fn apply(&mut self, signal: Signal) -> PetState`、`fn current(&self) -> PetState`。
- 规则：`FocusMode(true)` → 强制 `Sleep`（压制氛围）；`HasSomething` 被 `PanelOpened` 或定时清除；`NeedsYou` 只被 `Dismissed` 清；`IdleFor(长)` → `Sleep`。

- [ ] **Step 1: 写失败测试**（`#[cfg(test)] mod tests` in `machine.rs`）：
```rust
#[test] fn focus_forces_sleep() { let mut m = StateMachine::new(); m.apply(Signal::AgentBusy); assert_eq!(m.apply(Signal::FocusMode(true)), PetState::Sleep); }
#[test] fn needs_you_persists_until_dismissed() { let mut m = StateMachine::new(); m.apply(Signal::NeedsInput); assert_eq!(m.current(), PetState::NeedsYou); m.apply(Signal::PanelOpened); assert_eq!(m.current(), PetState::NeedsYou); assert_eq!(m.apply(Signal::Dismissed), PetState::Standby); }
#[test] fn has_something_clears_on_panel_open() { let mut m = StateMachine::new(); m.apply(Signal::ReminderDue); assert_eq!(m.current(), PetState::HasSomething); assert_eq!(m.apply(Signal::PanelOpened), PetState::Standby); }
```
- [ ] **Step 2: `cargo test -p sss-pet state::machine` → FAIL（类型未定义）。**
- [ ] **Step 3: 实现 `PetState`、`Signal`、`StateMachine`**，按上面规则。`apply` 返回新 `current()`。
- [ ] **Step 4: `cargo test state::machine` → PASS。**
- [ ] **Step 5: 提交。** `git commit -m "feat: pet state machine with focus/clearance rules"`

---

## Task 6: 活动/空闲检测 + 跟随活动节流（Rust，TDD）

**Files:** Create `src-tauri/src/activity.rs`, `src-tauri/src/timers.rs`。

**Interfaces:**
- Produces `activity::idle_since() -> Duration`（Win32 `GetLastInputInfo`；非 Win 用 `Instant::now()` 占位）。
- Produces `timers::FollowActivityThrottle { min: Duration, max: Option<Duration>, rng }`：`fn should_react(&mut self, now: Instant, last_input: Duration) -> Option<Instant>`——返回下次允许反应的时刻（随机/固定，下限 `min`=15min）；`fn on_reacted(&mut self, now)` 记录上次反应。
- 跟随活动：当用户有输入且距上次反应 ≥ 当前阈值 → 发 `Signal::UserActivity`（瞬时动画），并 `on_reacted`。

- [ ] **Step 1: 写失败测试**（用 fake clock）：`min=15min`，第一次 `should_react` 在有输入时返回一个 ≥15min 后的时刻；距上次反应 10min 时返回 None（不允许）；14min59s 仍 None；15min01s 允许。
- [ ] **Step 2: `cargo test follow_activity` → FAIL。**
- [ ] **Step 3: 实现 `FollowActivityThrottle`**（随机阈值在 `[min, max.unwrap_or(min*2)]`，重置 `on_reacted`）。
- [ ] **Step 4: `cargo test follow_activity` → PASS。**
- [ ] **Step 5: 提交。** `git commit -m "feat: idle detection and follow-activity throttle (>=15m, random)"`

---

## Task 7: 宠物渲染——SVG/CSS 状态 + 过渡，睡觉=静态

**Files:** Modify `src/pet/Pet.svelte`；Create `src/pet/states.css`；Modify `src-tauri/src/lib.rs`（向 pet 窗口 emit `pet_state` 事件）。

**Interfaces:** Consumes `PetState`（经 `listen('pet_state')`）。Produces：每个状态一套 SVG 姿势 + CSS 类；`Sleep`/`NeedsYou`/`HasSomething` 静态（无 keyframe）；`Standby`/`Thinking` 轻量循环。Rust 状态机变化时 `app.emit_to("pet","pet_state", state)`。

- [ ] **Step 1: `Pet.svelte` 订阅 `pet_state`**，给根元素切 class（`state-sleep` 等）；每个 class 对应一套 SVG 姿势（睡觉=垂头+Zzz、待命=睁眼、思考=齿轮、有动静=!、需要你=?）。
- [ ] **Step 2: `states.css`**：`Standby`/`Thinking` 各一段短 keyframe（眨眼/齿轮转）；`Sleep`/`NeedsYou`/`HasSomething` 无 animation。
- [ ] **Step 3: `lib.rs` 起一个 tokio 任务**：周期（如 1s）跑状态机：读 `activity::idle_since()`、agent 是否 busy、是否有到期提醒 → `state_machine.apply(...)` → 若 `current()` 变了则 `emit_to("pet","pet_state", new)`。**睡觉时不发动画循环**（Review Focus #1）。
- [ ] **Step 4: 写测试**：在 `state` 模块测"连续 20 个 `IdleFor` tick 后 `current()==Sleep` 且无周期任务在跑动画"（断言 `Pet.svelte` 的 sleep class 无 animation——前端用 vitest 快照或手验；Rust 侧断言状态机不再 emit 非 Sleep）。`cargo test sleep_is_static` → PASS。
- [ ] **Step 5: 手动验证 dev**：空闲一段时间后机器人变睡眠静态、无动画。提交。`git commit -m "feat: pet SVG/CSS states, sleep=static (idle cpu ~0)"`

---

## Task 8: agent 模块骨架——Backend trait / Session / 类型 / 工厂（Rust，TDD）

**Files:** Create `src-tauri/src/agent/mod.rs`, `src-tauri/src/agent/exec.rs`。

**Interfaces（后续任务依赖，名称/类型须稳定）:**
- `MessageKind` 枚举 `Text|Thinking|ToolUse|ToolResult|Status|Error|Log`。
- `struct Message { kind: MessageKind, content: String, tool: Option<String>, call_id: Option<String> }`。
- `struct ExecOptions { cwd: PathBuf, model: Option<String>, resume_session_id: Option<String>, timeout: Option<Duration> }`。
- `struct ExecResult { status: String, output: String, session_id: Option<String>, usage: HashMap<String,TokenUsage> }`。
- `struct Session { messages: tokio::sync::mpsc::Receiver<Message>, result: tokio::sync::oneshot::Receiver<ExecResult> }`。
- `trait Backend: Send + Sync { async fn execute(&self, prompt: &str, opts: ExecOptions) -> Result<Session, BackendError>; }`。
- `const SUPPORTED: &[&str] = &["claude","opencode","codex"];`、`fn new(agent_type: &str, cfg: BackendConfig) -> Result<Box<dyn Backend>>`。
- `exec.rs::resolve(exe_name) -> Option<PathBuf>`（`which`/PATH 探测）、`spawn(cmd, args, env, cwd) -> tokio::process::Child`。

- [ ] **Step 1: 写失败测试**：`new("claude", cfg)` 返回 `Ok`（非 panic）；`new("unknown", _)` 返回 `Err`；`SUPPORTED` 含三个。`resolve` 在 PATH 有 `cargo` 时能找到。
- [ ] **Step 2: `cargo test agent::new_factory` → FAIL。**
- [ ] **Step 3: 实现类型 + trait + `new`**（先返回占位 backend，Task 10-12 填实现）。`new` 的 match 三个分支分别 `Box::new(ClaudeBackend{cfg})` 等（结构体可先空，编译过即可）。
- [ ] **Step 4: `cargo test agent::new_factory` → PASS。**
- [ ] **Step 5: 提交。** `git commit -m "feat: agent backend trait, session/types, factory skeleton"`

---

## Task 9: stream-json 解析器（共用，TDD）

**Files:** Create `src-tauri/src/agent/stream_json.rs`。

**Interfaces:** Produces `fn parse_line(line: &str) -> Option<Message>`：把 claude stream-json 的每行 JSON（`{"type":"assistant","message":{...}}` 等）映射成 `Message`；`fn parse_result_event(line) -> Option<(String /*output*/, Option<String> /*session_id*/)>`（识别 `result` 事件）。

- [ ] **Step 1: 写失败测试**，用真实 claude stream-json 行片段作 fixture（`tests/fixtures/claude_stream.txt`）：assistant 文本行 → `Message{Text,...}`；tool_use 行 → `ToolUse`；result 行 → 提取 output + session_id。
- [ ] **Step 2: `cargo test stream_json` → FAIL。**
- [ ] **Step 3: 实现 `parse_line`/`parse_result_event`**（serde_json 按字段分派；对 claude 的 `message.content[].type` 文本/tool_use 分派）。
- [ ] **Step 4: `cargo test stream_json` → PASS。**
- [ ] **Step 5: 提交。** `git commit -m "feat: shared stream-json parser for claude-family backends"`

---

## Task 10: claude backend（TDD，stub 二进制）

**Files:** Create `src-tauri/src/agent/claude.rs`, `tests/fixtures/fake_claude.rs`（或 `tests/stubs/claude` 脚本）。

**Interfaces:** Consumes Task 8/9。Produces `struct ClaudeBackend { cfg: BackendConfig }` 实现 `Backend`。

- [ ] **Step 1: 写失败测试**：用一个 fake `claude`（测试用 Rust 小程序或脚本，读 stdin、吐固定 stream-json 行到 stdout、退出 0）。`execute("hi", opts)` 收到的 `messages` 流出 `Text("hello")`，`result` 得 `status="completed"`, `session_id=Some`。断言 argv 含 `-p --output-format stream-json --input-format stream-json`，prompt 经 stdin。
- [ ] **Step 2: `cargo test agent::claude` → FAIL。**
- [ ] **Step 3: 实现 `ClaudeBackend::execute`**：`resolve("claude")`→`spawn`→写 prompt 到 stdin→逐行读 stdout→`stream_json::parse_line`→发 `Message`；识别 result 事件→`ExecResult`。
- [ ] **Step 4: `cargo test agent::claude` → PASS。**
- [ ] **Step 5: 提交。** `git commit -m "feat: claude backend (stream-json, stdin prompt)"`

---

## Task 11: opencode backend（TDD，stub）

**Files:** Create `src-tauri/src/agent/opencode.rs`, `tests/stubs/opencode`。

**Interfaces:** Consumes Task 8。Produces `struct OpencodeBackend` 实现 `Backend`。

- [ ] **Step 1: 写失败测试**：fake `opencode`（读 stdin、吐 json 事件行）。断言 argv 含 `run --format json --dangerously-skip-permissions`，prompt 经 stdin（不在 argv）；`messages` 流出文本；`result` 完成。
- [ ] **Step 2: `cargo test agent::opencode` → FAIL。**
- [ ] **Step 3: 实现**：与 claude 同构，解析 opencode json 事件（字段名不同：`type:"message"` 等，按 opencode 实际格式分派——参考 multica `opencode.go` 的事件词汇）。
- [ ] **Step 4: `cargo test agent::opencode` → PASS。**
- [ ] **Step 5: 提交。** `git commit -m "feat: opencode backend (json, stdin prompt)"`

---

## Task 12: codex backend——app-server JSON-RPC（TDD，stub）

**Files:** Create `src-tauri/src/agent/codex.rs`, `tests/stubs/codex_appserver`。

**Interfaces:** Consumes Task 8。Produces `struct CodexBackend` 实现 `Backend`。这是最重的：长驻 `codex app-server --listen stdio://`，JSON-RPC `initialize` 握手 → `thread/start` 或 `thread/resume`（带 `resume_session_id`）→ 收 `turn/started`/消息事件 → `turn/completed`。

- [ ] **Step 1: 写失败测试**：fake app-server（读 JSON-RPC 行、回 `initialize` 结果、回 `thread/start` 结果 + 几条消息事件 + `turn/completed`）。断言 argv 含 `app-server --listen stdio://`；`execute` 完成握手、流出消息、`result.status="completed"`、`session_id` 来自 `thread/start` 响应。另测：`initialize` 超时 → `BackendError::Handshake`。
- [ ] **Step 2: `cargo test agent::codex` → FAIL。**
- [ ] **Step 3: 实现**：`spawn`→写 `initialize`→等响应（`HandshakeTimeout`）→`thread/start`(或 `thread/resume` 若 `opts.resume_session_id`）→循环读事件分派成 `Message`→`turn/completed` → `ExecResult`。参考 multica `codex.go` 的握手/turn 序列与超时。
- [ ] **Step 4: `cargo test agent::codex` → PASS。**
- [ ] **Step 5: 提交。** `git commit -m "feat: codex backend (app-server json-rpc, handshake, turn lifecycle)"`

---

## Task 13: 会话模型 + 续接/历史重放（Rust，TDD）

**Files:** Create `src-tauri/src/agent/conversation.rs`。

**Interfaces:** Produces `struct Conversation { id, provider, session_id: Option<String>, title, created_at }`；`fn resume_or_replay(conv: &Conversation, history: &[StoredMessage]) -> ExecOptions`：若 `session_id.is_some()` → `ExecOptions{resume_session_id: session_id}`（续接）；否则把 `history` 拼成上下文 prompt（重放）。Consumes Task 8 的 `ExecOptions`。

- [ ] **Step 1: 写失败测试**（Review Focus #4）：`conv` 有 `session_id` → `resume_or_replay` 返回的 `opts.resume_session_id==Some` 且 prompt 是空/当前轮（不重放）；`conv` 无 `session_id` → `opts.resume_session_id==None` 且 prompt 含历史消息文本（重放）。另测：面板隐藏时流继续——`Session` 的 `messages` channel 在接收端 drop 后，后端仍把消息写进 `ExecResult.output`/历史（Review Focus #3 的后端侧）。
- [ ] **Step 2: `cargo test conversation` → FAIL。**
- [ ] **Step 3: 实现 `resume_or_replay`** + 一个 `run_turn(backend, conv, history, prompt) -> (Session, Vec<Message> /*buffered*/)`：后端持有 session，把流同时发 channel 和内部 buffer；channel 关闭（前端隐藏）不中断流，buffer 在结果里落库。
- [ ] **Step 4: `cargo test conversation` → PASS。**
- [ ] **Step 5: 提交。** `git commit -m "feat: conversation model with session resume and history replay"`

---

## Task 14: SQLite + 配置（便携 + 回退，TDD）

**Files:** Create `src-tauri/src/db.rs`, `src-tauri/src/config.rs`。

**Interfaces:**
- `db::open() -> rusqlite::Connection`：路径 = `exe_dir/sss-pet.db`；若 exe 目录只读 → `%APPDATA%\sss-pet\sss-pet.db`（Review Focus #5）。`migrate()` 建表：`conversations(id,text pk, provider, session_id, title, created_at)`、`messages(id, conv_id, role, content, created_at)`、`todos`、`reminders`、`pet_state_log`。
- `config::Config { follow_activity_min_secs, providers, theme, focus:{enabled, window}, pet_pos, panel_rect }`；`load()/save()` 读写 exe 同级 `config.toml`（同 db 的回退规则）。接通 Task 4 的 `save_pet_pos/load_window_state`。

- [ ] **Step 1: 写失败测试**：`open()` 在可写临时目录建库 + `migrate` 后表存在；用 `std::env` 临时把 exe 目录指到只读目录（或 mock `is_writable`）→ 路径落到 `%APPDATA%` 且 `execute` 写入成功（Review Focus #5）。`Config` round-trip：`save(c)` 后 `load()` 等于 `c`。
- [ ] **Step 2: `cargo test db_and_config` → FAIL。**
- [ ] **Step 3: 实现 `db::open`/`migrate`/查询（CRUD for 5 表）** + `config` TOML round-trip + 可写性探测回退。
- [ ] **Step 4: `cargo test db_and_config` → PASS。**
- [ ] **Step 5: 提交。** `git commit -m "feat: sqlite store + toml config (portable, appdata fallback)"`

---

## Task 15: 面板 tab 壳 + 对话视图（流式 / 多会话 / 历史）

**Files:** Create `src/panel/App.svelte`（tab 壳）、`src/panel/views/ChatView.svelte`、`src/panel/lib/{api.ts, stream.ts}`；Modify `src-tauri/src/lib.rs`（注册命令）、`src-tauri/src/agent/conversation.rs`（接 db）。

**Interfaces:** Consumes Task 8/13/14。Tauri 命令：`list_conversations()`, `new_conversation(provider)`, `load_history(conv_id)`, `chat_send({conv_id, prompt})`；事件 `chat_message{conv_id, message}`、`chat_result{conv_id, result}`。前端 `stream.ts` 订阅事件按 conv_id 分派。

- [ ] **Step 1: `App.svelte` 顶部药丸标签 `[对话][代办][提醒][设置]`**，切换显示对应视图（设置/代办/提醒先占位）。
- [ ] **Step 2: `ChatView`**：左侧会话列表（`list_conversations`）+ 新建按钮（选 provider）；右侧消息气泡 + provider 选择器 + 输入框。
- [ ] **Step 3: `chat_send`**：后端 `new(backend)`→`resume_or_replay`→`run_turn`，流式 `emit("chat_message", {conv_id, message})`，结束 `emit("chat_result",...)` 并落库（消息 + session_id）。
- [ ] **Step 4: 写测试（Review Focus #3）**：`chat_send` 发起后立即让前端"隐藏"（drop 订阅）→ 后端仍完成、消息落库；重新 `load_history(conv_id)` 能看到完整回复。`cargo test chat_survives_hide` → PASS。
- [ ] **Step 5: 手动验证**：用本机真实 `claude` 跑一轮，流式逐字刷出、历史可在列表翻看、续接第二轮带上文。提交。`git commit -m "feat: panel tab shell + chat view (streaming, multi-session, history)"`

---

## Task 16: 代办 / 提醒 / 设置 三个 tab

**Files:** Create `src/panel/views/{TodosView,RemindersView,SettingsView}.svelte`；Create `src-tauri/src/notify.rs`；Modify `src-tauri/src/timers.rs`（提醒调度）、`src-tauri/src/state/machine.rs`（提醒→`HasSomething`、`?` 清除）。

**Interfaces:** Consumes Task 5/14。Tauri 命令：`todo_{list,add,toggle,delete}`、`reminder_{list,add,delete}`、`settings_get/set`。提醒到期：`timers` 触发 `notify::toast` + `state_machine.apply(ReminderDue)` → 宠物 `!`；用户开面板 → `apply(PanelOpened)` 清 `!`；`?`（agent 出错）需 `apply(Dismissed)`。

- [ ] **Step 1: `TodosView`**：增删改勾，调 `todo_*`，存 SQLite。
- [ ] **Step 2: `notify.rs`**：Windows toast（`tauri-plugin-notification` 或 `notify-rust`）。
- [ ] **Step 3: `timers.rs` 提醒调度**：每分钟扫 `reminders` 到期项 → toast + `Signal::ReminderDue`。`SettingsView`：provider 配置、`follow_activity_min`（≥900）、勿扰开关 + 时段、主题；存 `config.toml`。
- [ ] **Step 4: 测试**：`timers` 用 fake clock 测到期→`ReminderDue` + toast 调用；`state_machine` 测 `ReminderDue`→`HasSomething`、`PanelOpened`→清。`cargo test reminders_and_settings` → PASS。
- [ ] **Step 5: 手动验证**：加一条 1 分钟后提醒，到点 toast + 宠物冒 `!`，开面板后 `!` 消。提交。`git commit -m "feat: todos, reminders (toast+pet!), settings tabs"`

---

## Task 17: 打包 + 资源审计

**Files:** Modify `src-tauri/tauri.conf.json`（bundle）、`src-tauri/Cargo.toml`（release profile）；Create `docs/superpowers/plans/` 下无新增。

**Interfaces:** 无新接口；产出可分发 `.exe`。

- [ ] **Step 1: `tauri.conf.json` bundle** 配 `app://` 单 exe；附 WebView2 bootstrapper（`bundle.windows.webviewInstallMode: bootstrapper`）。
- [ ] **Step 2: `cargo tauri build`**，产出 `src-tauri/target/release/bundle/` 下 `.exe`。
- [ ] **Step 3: 资源审计（Review Focus #1）**：把 `.exe` 拷到独立目录跑，空闲 5 分钟，用任务管理器测：内存 < ~120MB、CPU ≈ 0（睡觉态无动画）。便携验证：数据落在 exe 同级；把 exe 拷到只读目录跑 → 回退 `%APPDATA%`（Review Focus #5 端到端）。
- [ ] **Step 4: 修任何漏网（如某状态仍跑 rAF、穿透失效、流式在隐藏时丢）。** 重测。
- [ ] **Step 5: 提交 + 打 tag。** `git commit -m "build: single-exe bundle with webview2 bootstrapper; resource audit"` ; `git tag v0.1.0`

---

## Self-Review 笔记

- **Spec 覆盖**：状态机(5)、活动/节流(6)、宠物渲染+静态(7)、agent trait/工厂(8)、三个 backend(10-12)、多会话+续接+流式(13,15)、SQLite+config 便携(14)、代办/提醒/设置/通知(16)、托盘(3)、窗口管理含拖动+翻转+持久化(4)、打包+资源(17) 均有任务。多会话历史(13/14/15)、流式(13/15)、勿扰(5/16) 已落。
- **类型一致**：`PetState`/`Signal`(5) 在 7/16 复用；`Backend`/`Session`/`Message`/`ExecOptions`/`ExecResult`(8) 在 10-13/15 复用；`Conversation`(13) 在 14/15 复用；`Config`(4 占位→14 落地)。
- **比例**：plan 以签名 + 测试名 + 断言为主，实现体留给实现者。
