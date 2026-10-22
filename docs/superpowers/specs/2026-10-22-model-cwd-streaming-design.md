# 桌面宠物助手 — 模型选择 / cwd / 流式处理过程 设计增补

- 日期：2026-10-22
- 状态：草稿（待 builder 审查，再交用户确认）
- 扩展：`docs/superpowers/specs/2026-10-21-desktop-pet-design.md`（v0.1.0 已实现的那套）

## 意图

在已可用的 v0.1.0 上做 3 个改动：(1) 一次对话 = agent + model + cwd 三件套，开对话时定、定了不改，model 能从该 agent 探测出的列表里选（像 multica）；(2) 调 agent 子进程时不再弹 cmd 黑窗；(3) 对话里实时看到 agent 的处理过程（思考 / 工具调用 / 工具结果），像 multica 聊天那样边到边滚出来。

## 改动 1：对话级 model + cwd（创建时定，不可变）

- 新建对话时选：agent（claude/codex/opencode）、model（从该 agent 探测列表选，或手填）、cwd（工作目录）。三者随此对话走、创建后不变。
- `Conversation` 加 `model: Option<String>`（None=用 agent 默认）+ `cwd: String`。`new_conversation(provider, model, cwd) -> Conversation`。
- `chat_send` 用对话自己的 `conv.model`（→ `ExecOptions.model`）+ `conv.cwd`（→ `run_turn` cwd），**不再用全局 config 的 working_dir**。
- 设置页：每个 agent 配一个**默认 model**（新建对话时预填）；全局"工作目录"改成新建对话 cwd 的默认预填值。
- 这天然消解 v0.1.0 遗留的"对话中途换 provider 致 session_id 错配"隐患（对话的 provider+model+cwd 创建时固定，无中途切换）。

### 模型探测（per-agent，照 multica）

`list_models(provider) -> Vec<String>` Tauri 命令，按 agent 各自的机制探测：

| Agent | 探测方式 | 参考 |
|---|---|---|
| opencode | 跑 `opencode models`（有此子命令），解析输出 | opencode CLI |
| claude | stream-json 的 `list_models` 控制请求：起一个 discovery claude 会话（stream-json 模式），发 list_models 控制请求，解析 models 回复 | multica `server/pkg/agent/claude_models.go`（`claudeListModelsArgs`/`discoverClaudeCatalog`，验过 2.1.223/2.1.258） |
| codex | 一次性 `codex debug models` 子命令（argv `["debug","models"]`，或 `--bundled` 离线兜底），解析 `codexDebugModelsResponse`；`codexStaticModels()` 最终兜底；需 codex ≥ 0.122.0 | multica `server/pkg/agent/thinking.go::discoverCodexCatalog`（**不是** app-server JSON-RPC——初稿写错了，builder 纠正） |

- **兜底**：探测失败 / 超时 / 空 → model 选择器允许**手填**模型名（文本输入）。model 可选（None=agent 自有默认）。
- 所有探测走 `exec.rs::spawn`（带改动 2 的 `CREATE_NO_WINDOW`）。

### 把所选 model 传给 agent

- 三个 backend **已经**接 `opts.model`：claude `--model`（claude.rs）、opencode `--model`（opencode.rs）、codex 经 thread/start|resume 的 `model` 参数（codex.rs）。**但对话层没接**：`conversation.rs::resume_or_replay` 现在写死 `model: None`、`run_turn` 没读 `conv.model`。要改：`resume_or_replay` 设 `opts.model = conv.model.clone()`（Conversation 加了 `model` 字段后）。
- opencode 的 model 格式是 `provider/model`（如 `anthropic/claude-…`）；探测出的列表若已是该格式就直接用，否则在传给 opencode 时按需拼。

## 改动 2：隐藏 cmd 窗口

- `exec.rs::spawn`：Windows 上给 `Command` 设 `.creation_flags(0x08000000)`（`CREATE_NO_WINDOW`），agent 子进程（claude/codex/opencode + 模型探测的 spawn）静默运行、不弹控制台黑窗。
- 用 `std::os::windows::process::CommandExt`（tokio 的 `Command` 经此支持 `creation_flags`）；`#[cfg(windows)]` 守卫，非 Windows 不影响。
- 应用到**所有** spawn（三个 backend 的 execute + 模型探测命令）。

## 改动 3：流式处理过程

- 后端**已经在**把 text/thinking/tool-use/tool-result/status 经 `chat_message` 事件推过来（Task 10/11/12 解析、Task 13/15 透传）；前端 `ChatView` 其实**已经把所有 MessageKind 都渲成气泡**了（有 `data-kind` 样式）——所以这里的活不是"从零加渲染"，而是把思考/工具块**做成可折叠 + 好看**（像 multica 那样），让处理过程一目了然。
- `ChatView` 把每个 `MessageKind` 实时渲出来（事件到一条渲一条，按序追加）：
  - `Text` → 正常助手气泡。
  - `Thinking` → 灰色、默认折叠的"思考"块（点开看全文）。
  - `ToolUse` → 一条"使用工具 X"的条（工具名 + 可折叠的 input）。
  - `ToolResult` → 折叠的"工具结果"块（output）。
  - `Status` → 小状态行（如"运行中…"）。
  - `Error` → 红色错误气泡。
- 像 multica 那样边到边滚出，用户实时看到 agent 在思考什么、调了什么工具、工具回了什么。

## Schema 迁移

- `conversations` 加列 `model TEXT NULL` + `cwd TEXT NOT NULL DEFAULT '.'`。
- `messages` 加列 `kind TEXT NULL`（存 `MessageKind` 的名字，如 `Text`/`Thinking`/`ToolUse`/`ToolResult`/`Status`/`Error`）——**用户要重开历史对话也能看到思考/工具块**，所以 kind 必须持久化。
- `StoredMessage` 加 `kind: Option<String>` 字段；`save_turn_record` 把每条 `Message` 的 `kind` 一起存；`load_messages` 读回 `kind`。
- 已有行回填：`kind` NULL 时按 `role` 推断（assistant→`Text`，user/tool→`ToolResult`），或前端 NULL→当 `Text` 渲染。
- `db::migrate` 用 `ALTER TABLE … ADD COLUMN`（SQLite 支持）；幂等：`PRAGMA table_info` 检查列是否存在再加。

## 留给实现者

- 各 agent 探测命令 + 解析的精确实现（按上表参考 multica）；手填兜底的 UI。
- ChatView 各 MessageKind 的渲染细节（思考/工具块默认折叠、chip 样式）。
- 探测结果是否缓存（每次开新建对话时探，或短暂缓存）——实现者判断。

## builder 审查后的修订/澄清（已并入上文，列此备查）

- **codex 探测**：改成 `codex debug models` 一次性子命令（初稿"app-server models-manager JSON-RPC"是错的，multica 实际用 `thinking.go::discoverCodexCatalog`）。
- **对话层接 model**：`resume_or_replay`/`run_turn` 要把 `conv.model` 接进 `ExecOptions.model`（现写死 None）。
- **ChatView**：已渲所有 kind，工作是折叠+美化，不是加渲染。
- **claude list_models**：回复是 `control_response` 事件，现有 `stream_json::parse_line` 故意跳过它——探测要单独写个解析（参考 multica `claudeControlResponse`）。
- **探测 spawn**：模型探测的子进程也要走 `exec.rs::spawn`（不是裸 `Command`），才能继承 `CREATE_NO_WINDOW`。
- **messages.kind 持久化**：用户要重开历史对话看到思考/工具块 → `messages` 表加 `kind` 列 + `StoredMessage.kind` + 存取带上 + 旧行回填。

## 非目标

- 对话中途换 model/cwd（要换就开新对话）。
- 把 multica 的探测 100% 复刻（含版本探测/缓存 TTL 等细节）——只取"拉到模型列表、用户选"的核心，失败能手填。
