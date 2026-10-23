# sss-pet

Windows 11 桌面宠物 + AI 智能体助手。基于 Tauri v2 + Rust + Svelte 5 构建。

悬浮在桌面上的小机器人，可以拖动、点击打开面板。面板包含六个页签：代办、提醒、KPI、对话、智能体、设置。

## 功能特性

### 桌面宠物

- **悬浮机器人**：260×160 透明窗口，始终置顶，不显示在任务栏（窗口尺寸容纳提醒气泡弹窗）
- **拖动**：按住机器人拖动到任意位置，自动保存位置
- **点击切换面板**：点击机器人打开/关闭面板窗口，面板自动定位在机器人附近
- **穿透点击**：机器人周围的透明区域可穿透鼠标点击到下层窗口；光标进入机器人中心区域时自动变为可交互
- **右键菜单屏蔽**：不显示浏览器默认右键菜单
- **状态动画**：根据闲置时间和活动状态切换表情（有空闲、思考、需要你等状态）

### 对话（Chat）

- **多 Provider 支持**：Claude、Codex、OpenCode 三个 AI 后端，通过各自的 CLI 执行
- **流式输出**：实时显示 AI 的思考、工具调用、文本回复，类似 multica 的 `/chat` 体验
- **对话历史**：SQLite 持久化，重新打开可查看完整历史（含工具调用记录）
- **会话恢复**：支持 agent 原生的 session resume（claude `--resume`、codex `thread/resume`、opencode `--session`）
- **Enter 发送**：Enter 直接发送消息，Shift+Enter 换行
- **消息时间戳**：每条消息显示本地时间（年月日时分秒）
- **新建对话**：选择 Provider → 探测模型列表 → 选择/手动输入模型 → 指定工作目录
- **可绑定智能体**：新建对话时可选择已保存的智能体配置（见下方智能体管理）

### 智能体管理（Agents）

- **智能体配置**：保存命名的智能体配置 = Provider + Model + 系统提示词 + 技能 + MCP 配置 + 工作目录
- **手动创建**：填写名称、描述、Provider、Model（支持探测）、系统提示词、MCP 配置 JSON、工作目录、技能分配
- **AI 创建**：描述你想要的智能体，AI 自动生成配置（系统提示词、Provider、Model），解析 `<agent_draft>` JSON 填充表单
- **技能管理**：创建/编辑/删除技能（SKILL.md 正文），分配给智能体（可启用/禁用）
- **MCP 配置**：每个智能体可配置 MCP 服务器（JSON 格式），支持「+ 代办 MCP」一键添加代办接口
- **对话集成**：对话创建时选智能体 → provider/model/cwd 从智能体拷贝（不可变）；system_prompt/skills/mcp 每次对话实时读取（可更新）
- **系统提示词接线**：
  - Claude：`--append-system-prompt` 旗标
  - Codex：`baseInstructions` / `developerInstructions` 字段
  - OpenCode：前拼到 prompt 文本
- **MCP 接线**：
  - Claude：`--mcp-config` 旗标（写临时文件）
  - Codex：`config` 字段
  - OpenCode：v1 跳过

### 代办（Todos）

- **完整任务跟踪**：标题、优先级（高/中/低）、责任人、开始时间、结束时间、备注
- **4 种状态**：待办（pending）、进行中（in_progress）、完成（completed）、取消（cancelled）
- **状态底色区分**：待办默认、进行中琥珀底、完成绿底、取消半透明
- **时间自动判断进行中**：待办状态且当前时间在开始-结束时间段内，自动显示为进行中，无需手动点「开始」
- **逾期检测**：超过结束时间且未完成 → 红色「逾期」徽章 + 左侧红线
- **多维筛选**：标题/备注模糊搜索、状态 chips（含逾期）、优先级下拉、责任人下拉
- **汇总栏**：待办/进行中/完成/逾期 数量一览
- **智能排序**：活跃优先 → 优先级高→低 → 截止时间早→晚
- **内联表单**：新增/编辑表单就地展开，含 datetime-local 时间选择
- **两步删除确认**：第一次点击显示「确认?」，3 秒内再点才删除
- **责任人配置**：在设置页配置责任人花名册（逗号分隔文本），代办页下拉读取
- **MCP 接口**：通过 MCP 协议暴露代办 CRUD 操作，本地 agent 可直接操作代办数据（见下方 MCP 服务器）

### 提醒（Reminders）

- **一次性/周期性**：支持 once（一次性，到指定时间点触发）和 recurring（周期性，按间隔重复触发）
- **周期配置**：周期性提醒可设置间隔值 + 单位（分钟/小时/天/周）
- **3 种状态**：pending（待触发）、active（已触发待处理）、done（已完成）
- **推迟操作**：支持推迟 10 分钟 / 1 小时 / 1 天
- **气泡弹窗**：提醒触发时在宠物窗口显示气泡弹窗，包含提醒内容 + 4 个操作按钮（不再提醒/推迟 10min/推迟 1hour/推迟 1day）
- **多行内容**：提醒内容支持多行文本
- **代办截止提醒**：代办任务在结束时间前 24 小时自动触发提醒气泡，复用提醒弹窗能力
- **后台调度**：独立 tokio 任务定期检查到期提醒，一次性触发后转 active 待处理，周期性计算下次触发时间
- **多维筛选**：状态 chips + 搜索 + 内联编辑表单
- **MCP 接口**：通过 MCP 协议暴露提醒 CRUD 操作（见下方 MCP 服务器）

### KPI 管理（KPIs）

- **团队 KPI 跟踪**：按年度管理 KPI 事项，字段包含年度、KPI类型、事项标题、事项详情、计划完成时间、状态
- **3 种状态**：未进行（not_started）、进行中（in_progress）、已完成（completed）
- **进度条概览**：按 KPI 类型显示该类型的年度完成率（已完成数/目标数×100%），进度可超过 100%
  - 颜色分级：≥100% 绿色、50-99% 蓝色、<50% 琥珀
  - 超过 100% 时显示「超额」徽章 + 真实百分比
- **年度目标数**：每个类型的年度目标数在 KPI 页面内直接编辑（进度条旁边的小输入框），按年度独立配置，存储在数据库
- **KPI 类型配置**：在设置页配置 KPI 类型列表（逗号分隔文本），KPI 页的类型下拉和进度条从配置读取
- **年度选择器**：顶部下拉框切换查看不同年份的 KPI 数据
- **多维筛选**：搜索 + 状态 chips + 类型下拉
- **卡片列表**：状态圆点 + 标题 + 类型徽章 + 计划日期，状态底色区分（进行中琥珀、已完成绿+删除线）
- **内联表单**：新增/编辑就地展开，含年度输入、类型选择、日期选择器
- **两步删除确认**：同代办模式

### 设置（Settings）

- **默认 Provider**：选择默认 AI 后端
- **代办间隔**：宠物跟随活动的最小间隔（≥900 秒）
- **主题**：跟随系统 / 浅色 / 深色
- **勿扰模式**：开启后宠物睡眠 + 静音，可选限定时段
- **开机自启动**：Windows 注册表（HKCU\…\Run），即时生效
- **工作目录**：agent CLI 运行目录，新建对话时作为默认预填值
- **代办责任人**：逗号分隔的责任人花名册
- **KPI类型**：逗号分隔的 KPI 类型列表（如：需求开发,Bug修复,技术分享）
- **Agent 默认模型**：每个 Provider 探测并选择默认模型，新建对话时预填

### MCP 服务器

sss-pet.exe 可作为 MCP（Model Context Protocol）服务器运行，让本地 agent CLI（Claude Code、Codex 等）通过 MCP 协议直接操作代办和提醒数据。

**启动方式**：
```bash
sss-pet.exe mcp-server
```

**MCP 配置**（在智能体管理的 MCP 配置中添加，或点击「+ 代办 MCP」按钮自动生成）：
```json
{
  "mcpServers": {
    "sss-pet-todos": {
      "command": "C:\\path\\to\\sss-pet.exe",
      "args": ["mcp-server"]
    }
  }
}
```

**暴露的 10 个 MCP Tools**：

| Tool | 参数 | 功能 |
|------|------|------|
| `todo_list` | status?, priority?, assignee? | 查询代办列表，支持筛选 |
| `todo_add` | text(必填), priority?, assignee?, start_at?, end_at?, notes? | 新增代办 |
| `todo_update` | id(必填), text(必填), priority?, assignee?, start_at?, end_at?, notes? | 修改代办 |
| `todo_set_status` | id(必填), status(必填) | 设置代办状态 |
| `todo_delete` | id(必填) | 删除代办 |
| `reminder_list` | status? | 查询提醒列表，支持筛选 |
| `reminder_add` | text(必填), reminder_type?, interval_value?, interval_unit?, fire_at? | 新增提醒 |
| `reminder_update` | id(必填), text(必填), reminder_type?, interval_value?, interval_unit?, fire_at? | 修改提醒 |
| `reminder_set_status` | id(必填), status(必填) | 设置提醒状态 |
| `reminder_delete` | id(必填) | 删除提醒 |

MCP 服务器打开与 GUI 应用相同的 SQLite 数据库，agent 看到的代办/提醒数据与用户在面板中看到的完全同步。

## 技术栈

| 层 | 技术 |
|----|------|
| 框架 | Tauri v2 (2.12.4) |
| 后端 | Rust (edition 2024) |
| 前端 | Svelte 5 (runes 模式) + Vite |
| 数据库 | SQLite (rusqlite, bundled) |
| 配置 | TOML (config.toml) |
| AI 后端 | Claude CLI / Codex CLI / OpenCode CLI |
| 协议 | MCP (Model Context Protocol) JSON-RPC 2.0 over stdio |

### 关键依赖

- `tauri` + `tauri-plugin-opener` / `tauri-plugin-notification` / `tauri-plugin-autostart`
- `rusqlite` (bundled SQLite, 单 exe 自包含)
- `tokio` (async runtime, subprocess 管理)
- `serde` / `serde_json` / `toml` (序列化)
- `chrono` (时间戳)
- `log` (日志门面，自定义 FileLogger 按天轮转)
- `uuid` (对话 ID)
- `which` (CLI 路径查找)
- `device_query` + `windows-sys` (Win32 光标/空闲检测)

## 项目结构

```
sss-pet/
├── src/                          # 前端 (Svelte 5)
│   ├── panel/                    # 面板窗口
│   │   ├── App.svelte            # 面板壳：拖拽栏 + 页签 + 视图
│   │   ├── views/
│   │   │   ├── ChatView.svelte    # 对话页
│   │   │   ├── AgentsView.svelte  # 智能体管理页
│   │   │   ├── TodosView.svelte   # 代办页
│   │   │   ├── RemindersView.svelte # 提醒页
│   │   │   ├── KpisView.svelte     # KPI管理页
│   │   │   └── SettingsView.svelte # 设置页
│   │   ├── lib/
│   │   │   ├── api.ts            # IPC 封装 (对话/智能体/技能/MCP)
│   │   │   ├── settings.ts       # IPC 封装 (代办/提醒/设置/自启)
│   │   │   ├── stream.ts         # 流式事件订阅
│   │   │   └── theme.ts          # 主题切换
│   │   └── theme.css             # 设计系统 token + 基础样式
│   └── pet/                      # 宠物窗口
│       ├── Pet.svelte             # SVG 机器人 + 拖动 + 点击
│       └── states.css             # 状态动画
├── src-tauri/                    # 后端 (Rust)
│   ├── src/
│   │   ├── main.rs               # 入口：mcp-server 子命令 / GUI
│   │   ├── lib.rs                 # Tauri builder + 命令注册
│   │   ├── db.rs                  # SQLite: 迁移 + 全部 CRUD
│   │   ├── config.rs              # TOML 配置 + Settings 投影
│   │   ├── chat.rs                # 对话流程 (chat_send → run_chat_turn)
│   │   ├── mcp_server.rs          # MCP JSON-RPC 服务器 (代办+提醒 tools)
│   │   ├── window.rs              # 窗口管理 (穿透/拖动/面板定位)
│   │   ├── tray.rs                # 系统托盘菜单
│   │   ├── brain.rs               # 宠物状态机 (1Hz tick)
│   │   ├── activity.rs            # 闲置检测 (Win32 GetLastInputInfo)
│   │   ├── timers.rs              # 跟随活动节流
│   │   ├── state.rs               # 宠物状态枚举
│   │   ├── notify.rs              # Windows toast 通知
│   │   ├── reminders.rs           # 提醒调度器 (一次性/周期性/推迟/弹窗)
│   │   ├── logger.rs              # 文件日志 (按天轮转)
│   │   └── agent/                 # AI 后端
│   │       ├── mod.rs             # Backend trait + ExecOptions + 工厂
│   │       ├── claude.rs          # Claude CLI 后端
│   │       ├── codex.rs           # Codex CLI 后端 (JSON-RPC app-server)
│   │       ├── opencode.rs        # OpenCode CLI 后端
│   │       ├── conversation.rs    # 会话模型 + resume/replay + run_turn
│   │       ├── exec.rs            # 子进程 spawn (CREATE_NO_WINDOW)
│   │       └── models.rs          # 模型探测 (list_models)
│   ├── Cargo.toml
│   └── tauri.conf.json            # 窗口/打包/权限配置
├── package.json
└── vite.config.ts
```

## 开发环境

### 前置要求

- [Node.js](https://nodejs.org/) 18+ + [pnpm](https://pnpm.io/)
- [Rust](https://rustup.rs/) (stable, edition 2024)
- [Tauri v2 前置依赖](https://v2.tauri.app/start/prerequisites/)（Windows 需要 WebView2 + MSVC）
- 至少一个 AI CLI 安装并登录：[Claude Code](https://docs.anthropic.com/en/docs/claude-code) / [Codex](https://github.com/openai/codex) / [OpenCode](https://github.com/sst/opencode)

### 安装依赖

```bash
pnpm install
```

### 开发模式

```bash
pnpm tauri dev
```

启动 Vite dev server (localhost:1420) + Tauri 开发窗口，支持前端热更新。

### 类型检查

```bash
pnpm svelte-check
```

### 运行测试

```bash
cd src-tauri && cargo test
```

176 个单元测试覆盖：数据库 CRUD、迁移幂等性、对话流程、配置往返、提醒调度、KPI 管理等。

## 打包构建

### 构建 release exe

```bash
pnpm tauri build --no-bundle
```

> **重要**：必须用 `pnpm tauri build` 而非 `cargo build --release`。Tauri CLI 会先运行 `pnpm build`（前端构建），然后用正确的环境变量编译 Rust 二进制，确保前端资源嵌入 exe。直接用 `cargo build --release` 不会嵌入前端，exe 会尝试连接 dev server 导致 "localhost 拒绝连接"。

构建产物：`src-tauri/target/release/sss-pet.exe`（约 7.3MB，自包含 SQLite + 前端资源）。

### 构建安装包

```bash
pnpm tauri build
```

生成 NSIS 安装包（`src-tauri/target/release/bundle/nsis/`），包含 WebView2 bootstrapper。

### MCP 服务器模式

构建出的 exe 可直接作为 MCP 服务器使用：

```bash
sss-pet.exe mcp-server
```

从 stdin 读取 JSON-RPC 请求，向 stdout 写入响应。详见上方「MCP 服务器」章节。

## 数据存储

所有数据存储在 exe 同目录（便携模式）或 `%APPDATA%\sss-pet\`（exe 目录只读时回退）：

| 路径 | 内容 |
|------|------|
| `sss-pet.db` | SQLite 数据库：conversations、messages、agents、skills、agent_skills、todos、reminders、kpis、kpi_targets、pet_state_log |
| `config.toml` | TOML 配置：主题、provider 默认值、工作目录、勿扰、宠物/面板位置、责任人花名册、KPI类型列表 |
| `log/sss-pet-YYYY-MM-DD.log` | 按天轮转的日志文件，记录生命周期事件、错误、调度器状态等 |

## 日志

应用运行时将日志写入 `{data_dir}/log/` 目录，按天轮转（文件名含日期）。GUI 模式同时输出到 stderr，MCP 模式仅写文件。日志级别包括 INFO（生命周期事件：DB 迁移、调度器启动等）和 ERROR（异常、崩溃信息），便于定位问题。

## 窗口配置

| 窗口 | 尺寸 | 特性 |
|------|------|------|
| pet | 260×160 | 透明、无装饰、无阴影、始终置顶、不在任务栏、不可缩放（容纳提醒气泡弹窗） |
| panel | 760×560 | 无装饰、可缩放、默认隐藏、点击宠物显示/隐藏 |

## 设计系统

CSS token 系统（`src/panel/theme.css`），支持浅色/深色/跟随系统三套主题：

- 颜色：`--bg`、`--surface`、`--ink`、`--mut`、`--bd`、`--acc`/`--acc-soft`、`--err`/`--err-soft`、`--ok`/`--ok-soft`、`--warn`/`--warn-soft`
- 间距：`--space-1`(4px) ~ `--space-5`(24px)
- 圆角：`--r-panel`(14px)、`--r-bubble`(12px)、`--r-chip`(999px)、`--r-code`(8px)
- 字体：`--font`（system-ui + PingFang SC + Microsoft YaHei）、`--mono`
- 动画：`--t-fast`(120ms)、`--t-base`(180ms)，`prefers-reduced-motion` 全局静默

## License

MIT
