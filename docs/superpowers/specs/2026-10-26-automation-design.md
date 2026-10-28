# 自动化（Automation）子 tab — 设计

> 状态：brainstorming 产出，待 builder-check → 用户确认 → 转 plan
> 日期：2026-10-26
> 参考：multica `server/migrations/042_autopilot.up.sql`、`server/internal/service/autopilot.go`、`packages/views/autopilots/`（只取"定义→触发→执行→历史→详情"骨架，丢弃 webhook/issue/quota/squad/cron）

## 意图与为什么

用户希望在智能体 tab 里加一个"自动化"子 tab，能：

1. 配置一个自动化任务（选一个已配好的智能体 + 一段提示词 + 一种定时触发方式）；
2. 到点自动跑（app 开着才触发），也能手动点"立即运行"；
3. 看到简单的历史执行记录（状态、时间、结果摘要）；
4. 点开某条历史，看到那次跑的完整对话。

为什么：省去每次为同一个 recurring 需求（例如"每天早上总结代办并给建议"）重开对话、重选智能体、重写提示词。multica 的 autopilot 是同一意图的企业级版本，这里做桌面 pet 的"简单版"。

## 目标 / 非目标 / 反目标

**目标**
- 在 `AgentsView` 现有 `.sub-tabs` 里加第三个 tab"自动化"。
- 任务字段：名称、绑定智能体、提示词、触发方式（每天 HH:MM 或每 N 分钟/小时）、enabled 开关。
- 触发：定时自动 + 手动"立即运行"。
- 历史列表：状态、来源（schedule/manual）、开始/结束时间、结果摘要。
- 运行详情：点开看那次完整对话消息（只读）。

**非目标（明确不做）**
- 完整 cron 表达式、每周几、webhook、事件触发。
- multica 的 issue 创建模式、squad 派发、配额、规则版本快照、订阅者、协作者。
- 错过补跑（app 关着错过的定时一律跳过，不补）。
- 跨设备调度（仅本机、本 app 进程内）。

**反目标（做了就算失败）**
- 引入第二个常驻调度线程（必须复用现有 reminders tick）。
- 自造一套对话存储（必须复用 `conversations`/`messages` + `run_chat_turn`）。
- 自动化运行把对话历史污染到无法区分（必须能用 title 前缀 + 一个查询条件区分出自动化产生的会话）。

## 约束

- Tauri 2 IPC：command 参数 camelCase → snake_case；struct 字段 serialize snake_case。
- SQLite（rusqlite bundled），`PRAGMA foreign_keys = ON`（已开启，级联可用）。
- 时间存储统一 UTC ISO8601（与 reminders 一致）；UI 展示转本地。
- app 必须开着才会触发定时（复用的 tick 在 app 进程内）。
- 复用现有 `run_chat_turn`（`chat.rs:105`）执行一轮 agent turn；复用 `db::new_conversation` + `save_turn_record` 落库。
- 复用现有 reminders tick（`reminders.rs`，15s `tokio::time::sleep` 循环），不新增线程。

## 数据模型（新增两张表，加在 `db.rs migrate()`）

```sql
CREATE TABLE IF NOT EXISTS automations (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    agent_id      TEXT NOT NULL,                      -- FK agents(id) ON DELETE CASCADE
    prompt        TEXT NOT NULL,
    trigger_type  TEXT NOT NULL,                      -- 'daily' | 'interval'
    trigger_config TEXT NOT NULL,                     -- daily: 'HH:MM'（本地，24h）；interval: 整数分钟（字符串）
    enabled       INTEGER NOT NULL DEFAULT 1,         -- 0/1
    last_run_at   TEXT,                               -- UTC ISO8601，可为 NULL
    next_fire_at  TEXT,                               -- UTC ISO8601，可为 NULL（disabled 或待首次计算）
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    FOREIGN KEY (agent_id) REFERENCES agents(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS automation_runs (
    id              TEXT PRIMARY KEY,
    automation_id   TEXT NOT NULL,                    -- FK automations(id) ON DELETE CASCADE
    conversation_id TEXT,                              -- FK conversations(id) ON DELETE SET NULL
    source          TEXT NOT NULL,                    -- 'schedule' | 'manual'
    status          TEXT NOT NULL,                    -- 'running' | 'completed' | 'failed'
    started_at      TEXT NOT NULL,                    -- UTC ISO8601
    finished_at     TEXT,
    error           TEXT,                             -- 失败原因
    result_summary  TEXT,                             -- 最后一条 assistant 消息前 ~200 字
    created_at      TEXT NOT NULL,
    FOREIGN KEY (automation_id) REFERENCES automations(id) ON DELETE CASCADE,
    FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_automation_runs_automation ON automation_runs(automation_id, started_at DESC);
```

**Rust structs**（`db.rs`，snake_case serialize）：
- `Automation { id, name, agent_id, prompt, trigger_type, trigger_config, enabled: bool, last_run_at: Option<String>, next_fire_at: Option<String>, created_at, updated_at }`
- `AutomationRun { id, automation_id, conversation_id: Option<String>, source, status, started_at, finished_at: Option<String>, error: Option<String>, result_summary: Option<String>, created_at }`

**CRUD 函数**（`db.rs`）：`automation_list`、`automation_get`、`automation_create`、`automation_update`、`automation_delete`、`automation_set_next_fire`、`automation_run_list(automation_id)`、`automation_run_create`、`automation_run_update`、`automation_run_get`。

**对话打标**：自动化创建的会话用 `title = format!("🤖 {} — {}", automation_name, local_ts)`，`agent_id = automation.agent_id`。这样既在 `list_conversations` 里可区分（🤖 前缀），又绑定了 agent profile（`chat_send` 每轮重读 agent 拿最新 system_prompt/skills）。注意 `new_conversation` 不收 title 参数、硬编码"新对话"，所以建完用 `save_conversation` 把 title 改成 🤖 前缀串（upsert 的 DO UPDATE 会更新 title）。

## 触发与调度（复用 reminders tick）

在 `reminders.rs` 的 `spawn_reminder_scheduler` 循环里（`reminders.rs:138-174`），紧跟现有 `run_reminder_tick(&conn, …)` 调用、**同一个持锁块内**，加一个兄弟调用 `automation::run_automation_tick(&conn, now)`。`run_reminder_tick` 本身不动（签名/返回/4 个测试都不变）；automation 逻辑全在新函数里，它做完同步 DB 准备后返回待派发元组，由 scheduler 在锁释放后 `tokio::spawn` 异步执行：

```text
// spawn_reminder_scheduler 循环体（伪代码）
let dispatches = {
    let conn = db_state.lock()?;
    let now = Utc::now();
    let _ = run_reminder_tick(&conn, now, &mut notify_fn, &mut apply_fn, &mut emit_fn);
    automation::run_automation_tick(&conn, now)?   // 返回 Vec<AutomationDispatch>
}; // ← 锁在这里释放
for d in dispatches {
    // run_automation_tick 已在锁内做完：建 run 行(running) + 建 conversation +
    // 算 next_fire + 加载 agent profile + 已启用 skills
    let app = app.clone();
    let db_state = db_state.clone();
    tokio::spawn(async move { automation_dispatch_async(app, db_state, d).await; });
}
```

`run_automation_tick` 内部（锁内、同步）：
- `automation_get_due(conn, now)`：`enabled=1 AND next_fire_at <= now`。
- 错过判定：`next_fire_at < now - 5min` 视为关机期间过期 → 只 `automation_set_next_fire` 推到 `compute_next_fire(now)`，不派发、不记 run 行。
- 仍在 5 分钟窗口内（`now-5min <= next_fire_at <= now`）的才真派发：
  - 重叠：`automation_run_has_running(conn, id)` → 跳过，不记行。
  - 否则：`new_conversation`（title 用 🤖 前缀，见下"对话打标"）+ `automation_run_create(status='running', source='schedule', conversation_id)` + `automation_set_next_fire(id, compute_next_fire(now))` + `automation.last_run_at=now`。
  - 加载 agent profile（provider/model/cwd/system_prompt/mcp_config）+ 已启用 skills 列表，塞进 `AutomationDispatch` 元组返回。

注意：tick 持锁时间要短，所以**锁内只做"建 run 行 + 建 conversation + 算 next_fire"**，`run_chat_turn`（含 LLM 调用 + 流式落库）放到锁外异步执行。`run_chat_turn` 内部已经用自己的 `Arc<Mutex<Connection>>` 在 drain task 里落库，与 tick 不冲突。

**next_fire 计算**（新函数 `automation::compute_next_fire(trigger_type, trigger_config, from_utc) -> Option<DateTime<Utc>>`）：
- `daily 'HH:MM'`：把 HH:MM 当本地时间；下一个本地 HH:MM（今天已过则明天）→ 转 UTC。
- `interval N`（分钟）：`from_utc + N minutes`。
- disabled → None。

## 执行一轮（复用 run_chat_turn）

新模块 `src-tauri/src/automation.rs`，核心 `automation_dispatch(app, db_state, automation, source) -> Result<(run_id, conv_id), String>`：

1. 锁内：`new_conversation(conn, provider, model, cwd, Some(agent_id))`（provider/model/cwd 从绑定的 agent profile 取；title 用 `🤖 <name> — <local_ts>`）。建 `automation_runs` 行（status='running', source, conversation_id=conv.id, started_at=now）。若是 schedule 源，更新 `automation.last_run_at` + `next_fire_at`。
2. 锁外：像 `chat_send`（`chat.rs:462-500`）那样从 agent profile 取 `system_prompt` + `mcp_config` + 已启用 skills，调 `skill::materialize_skills` 拿 `created_dirs` + prompt 段拼进 `base_system_prompt`。
3. 调 `run_chat_turn(backend, emit, db_arc, &conv_id, &prompt, &cwd, system_prompt, mcp_config, created_dirs)`。
   - `backend = agent::new(provider, BackendConfig::default())?`
   - `emit` 是一个**带状态的累加器闭包**（`automation_dispatch_async` 持有，不是 no-op）：
     - `chat_message {conv_id, message}`：转发为 `automation_run_message {runId, automationId, message}` 到 panel；同时把 message 累进一个 `Arc<Mutex<Vec<StoredMessage>>>`（或只记最后一条 assistant 文本）。
     - `chat_result {conv_id, result}`：lock db → `automation_run_update(status='completed', finished_at=now, result_summary=累加的最后一条 assistant 文本前 200 字)` → emit `automation_runs_changed {automationId}`。
     - `chat_error {conv_id, message}`：lock db → `automation_run_update(status='failed', finished_at=now, error=message)` → emit `automation_runs_changed`。
   - drain task 落库仍走 `save_turn_record`（与普通 chat 一致），消息在 `chat_result` 触发前已持久化——所以累加器也可选择在 `chat_result` 时直接 `load_messages(conv_id)` 取最后一条 assistant 文本，二选一，builder 定。
4. `cleanup_skill_dirs(&created_dirs)`：`run_chat_turn` 内部 drain task 在 emit `chat_result` 后已经自动调过（`chat.rs`，上一轮 skill 改造），所以 `automation_dispatch_async` **不需要**再清——传进去的 `created_dirs` 会被自动清理。

> 关键：`run_chat_turn` 是 fire-and-forget（立即返回 `Ok(())`，drain task 异步跑），不向调用方返回 `TurnRecord` 或完成句柄。所以**完成判定 + run 行收尾必须走 emit 闭包**这条路（上面的 `chat_result`/`chat_error` 分支），不去改 `run_chat_turn` 的签名或返回值——`chat_send` 与其 3 个测试都不受影响。

## 重叠 / 失败策略

- **定时触发重叠**：该 automation 已有 `status='running'` 的 run → 跳过本次定时，不记 run 行，仅 log。（手动"立即运行"不受此限——用户显式操作，允许并发。）
- **app 关着错过**：tick 时只查 `next_fire_at <= now`；如果某 automation 的 `next_fire_at` 远早于 now（说明关机期间错过了），按"算到下一个未来 fire 时间"处理：先把 `next_fire_at` 推进到 `compute_next_fire(now)`，不补跑过去那一次。实现上：`automation_get_due` 返回的行里，先重算 next_fire（如果 `next_fire_at < now - 5min` 视为"已过期"，直接推进不跑），再对仍在窗口内（`now - 5min <= next_fire_at <= now`）的才真正派发。5 分钟窗口容忍 tick 抖动，但不补几小时前的。
- **agent 已删除**：`agent_id` FK CASCADE 会删 automation 行，不会留孤儿。
- **run 失败**：记 `status='failed'` + `error`，next_fire 仍照常推进（不因失败暂停；用户自己看到失败再 disable）。

## UI（`AgentsView.svelte`）

**接入点**（已确认现有模式）：
- `subView` 联合类型加一项：`"agents" | "skills" | "automation"`（`AgentsView.svelte:40`）。
- `.sub-tabs`（690-697）加一个按钮"自动化"。
- `{#if}` switch（699-873）加一个 `{:else if subView === "automation"}` 分支。

**自动化分支布局**（文字草图）：
```
┌─ 顶部工具条 ──────────────────────────────────────┐
│  [+ 新建自动化]                                    │
├─ 左列：自动化列表 ────┬─ 右列：选中项的详情/编辑 ──┤
│ ▸ 每日总结代办       │  名称: [___________]        │
│   每天 09:00 · 启用   │  智能体: [下拉选]            │
│   上次: 成功 08:50    │  提示词: [textarea]          │
│ ▸ 每小时检查          │  触发: (•)每天 [09:00]       │
│   每 60 分钟 · 启用   │        ( )每 [60][分钟▾]    │
│   上次: 运行中…       │  [✓] 启用定时                │
│ ▸ …                  │  [保存] [删除] [立即运行]    │
├──────────────────────┴────────────────────────────┤
│ 选中项的运行历史（右列下方或独立区）              │
│ ▸ 10-26 09:00 · schedule · 成功 · "今天有3件代办…"│
│ ▸ 10-25 09:00 · schedule · 成功 · …               │
│ ▸ 10-25 14:02 · manual   · 失败 · agent::new err  │
│   点一行 → 展开/弹层显示那次完整对话消息（只读）  │
└──────────────────────────────────────────────────┘
```

- 列表项：名称、触发摘要（"每天 09:00" / "每 60 分钟"）、enabled toggle、上次 run 状态+时间、下次 fire 时间。
- 编辑表单：name、agent 下拉（复用现有 agent 列表）、prompt textarea、trigger type radio（daily/interval）、trigger_config（daily=时间输入；interval=数字+单位下拉分钟/小时）、enabled checkbox、保存/删除/立即运行按钮。
- 历史列表：每行 status 图标、source、started_at、finished_at、result_summary（截断+tooltip 全文）、点击展开。
- **运行详情**：点开某条 run → 加载 `automation_run_messages(run_id)`（= `load_messages(conv_id)`）→ 只读渲染消息列表（role + content，assistant 文本 + 工具调用折叠）。复用 chat 里已有的消息渲染样式；不做输入框。
- **实时进度**：若某 automation 有 `status='running'` 的 run，列表项显示"运行中…"；订阅 `automation_run_message`/`automation_run_result`/`automation_run_error`，在详情区流式显示当前 run 的消息。

## Rust 命令（Tauri 2，camelCase 入参）

```
automation_list()                                          → Vec<Automation>
automation_create(name, agentId, prompt, triggerType, triggerConfig, enabled)
                                                           → Automation
automation_update(id, name, agentId, prompt, triggerType, triggerConfig, enabled)
                                                           → Automation
automation_delete(id)                                      → ()
automation_set_enabled(id, enabled)                        → Automation   // 列表项上的快速开关
automation_run_now(id)                                     → AutomationRun // 手动触发，立即返回 run 行（异步执行）
automation_run_list(automationId)                          → Vec<AutomationRun>
automation_run_messages(runId)                             → Vec<StoredMessage>  // 详情区用
```

全部注册到 `lib.rs` 的 `invoke_handler!`。

## 事件（panel 订阅）

- `automation_run_message` `{ runId, automationId, message }` —— 流式消息（对应 `chat_message`）。
- `automation_run_result` `{ runId, automationId, result }` —— 一轮完成（对应 `chat_result`）。
- `automation_run_error` `{ runId, automationId, message }` —— 落库/执行失败。
- `automation_runs_changed` `{ automationId }` —— 一次 run 落定，历史列表刷新。

## 模块划分（builder 可调整）

- `src-tauri/src/automation.rs`（新）：`compute_next_fire`、`run_automation_tick`（同步，锁内做 DB 准备，返回 `Vec<AutomationDispatch>`）、`automation_dispatch_async`（锁外异步：materialize + run_chat_turn + emit 累加器收尾）、`automation_dispatch_now`（手动"立即运行"入口）、命令实现委托到 db + chat。
- `src-tauri/src/db.rs`：两张表 + structs + CRUD。
- `src-tauri/src/reminders.rs`：`spawn_reminder_scheduler` 循环体里、`run_reminder_tick` 调用之后、同一持锁块内加 `automation::run_automation_tick(&conn, now)`；锁释放后 `tokio::spawn` 各 dispatch。**`run_reminder_tick` 本身不动**。
- `src-tauri/src/lib.rs`：`pub mod automation;`、注册命令。
- `src/panel/lib/api.ts`：`Automation`/`AutomationRun` 接口 + 命令包装 + `subscribeAutomationRunStream`。
- `src/panel/views/AgentsView.svelte`：第三个 sub-tab + 自动化分支 UI。

## 留给 builder 的

- 运行详情是"内联只读消息列表"还是"跳到 chat tab 加载该 conv"——我选了内联只读（更符合"在自动化 tab 里看完"），但若消息渲染复用成本高，builder 可降级为"跳到 chat tab"。
- `result_summary` 取累加的最后一条 assistant 文本前 200 字（或在 `chat_result` 时 `load_messages` 取）；具体截取长度 builder 可微调。
- 编辑表单是"右列固定面板"还是"弹层"——我画的是右列面板；builder 按 `AgentsView` 现有 skill 编辑面板的交互风格保持一致即可。
- 单位下拉（分钟/小时）存成"分钟整数"还是"值+单位"——我选存分钟整数（小时=N*60），UI 翻译。builder 可改。

## builder-check 后的裁定（两处结构性歧义，我已定）

1. **run 行收尾走 emit 累加器，不改 `run_chat_turn`**：`run_chat_turn` fire-and-forget，立即返回 `Ok(())`，drain task 异步跑，不向调用方返回 `TurnRecord` 或完成句柄。裁定用 emit 闭包做带状态的累加器——`chat_message` 累消息、`chat_result`/`chat_error` 收尾 run 行（status/finished_at/result_summary/error）。`chat_send` + 其 3 个测试零改动。
2. **`run_automation_tick` 是 `spawn_reminder_scheduler` 循环里的兄弟调用，不嵌进 `run_reminder_tick`**：`run_reminder_tick` 是同步、只拿 `&Connection`+三个闭包、无 `AppHandle`/`Arc<Mutex<Connection>>` 作用域。裁定在 scheduler 循环体里、`run_reminder_tick` 之后、同一持锁块内调 `run_automation_tick(&conn, now)`（返回 `Vec<AutomationDispatch>`），锁释放后 `tokio::spawn` 异步派发。`run_reminder_tick` 签名/返回/4 个测试零改动。

## 我替你做的几个判断（看到不对就否）

1. **重叠策略**：定时重叠跳过（不记行）、手动"立即运行"允许并发。
2. **错过不补**：关机错过的定时一律不补跑，只把 next_fire 推到未来；5 分钟窗口内容忍 tick 抖动照跑。
3. **失败不暂停**：run 失败只记行，定时照常推进；用户自己 disable。
4. **对话复用**：自动化 run = 一行普通 `conversations`，title 带 🤖 前缀；运行详情内联只读渲染，不另建存储。
5. **实时进度**：emit `automation_run_*` 事件，tab 开着能看流式；不开就只看历史。
6. **触发单位**：interval 存分钟整数，UI 给"分钟/小时"下拉。
