# 自动化（Automation）子 tab Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在智能体 tab 下加第三个子 tab"自动化"，让用户定义（name + agent + prompt + 定时触发）的自动化任务到点自动跑、可手动立即运行，并看历史 + 每次跑的完整对话。

**Architecture:** 两张新 SQLite 表（`automations`/`automation_runs`）；新 Rust 模块 `automation.rs` 承载 `compute_next_fire` + 同步 `run_automation_tick`（在 reminders 的 15s tick 里做兄弟调用）+ 异步 `automation_dispatch_async`（复用 `run_chat_turn` 执行一轮，emit 累加器收尾 run 行）；每次跑开一行普通 `conversations`（🤖 前缀 title），运行详情复用 `load_messages` 只读渲染。不新增线程、不改 `run_chat_turn`、不改 `run_reminder_tick`。

**Tech Stack:** Rust 2024 + Tauri 2.12 + rusqlite (bundled) + chrono；Svelte 5 (runes) + TypeScript。构建 `pnpm tauri build --no-bundle`，测试 `cargo test --lib` + `pnpm svelte-check`。

**Spec:** `docs/superpowers/specs/2026-10-26-automation-design.md`

## Global Constraints

- Tauri 2 IPC：command 参数 camelCase → snake_case；struct 字段 `#[serde(rename_all = "snake_case")]`。
- SQLite `PRAGMA foreign_keys = ON`（`db::open` 已开启），FK CASCADE 生效。
- 时间存储统一 UTC ISO8601（`to_rfc3339()`）；`compute_next_fire` 在 Local 时间域算，边界处转 UTC 存。
- 复用 `run_chat_turn`（`chat.rs:105`）——不得改其签名/返回/3 个测试。
- 复用 `run_reminder_tick`（`reminders.rs:63`）——不得改其签名/返回/4 个测试；automation 走兄弟调用 `run_automation_tick`。
- app 开着才触发；关机错过的定时一律不补，只把 `next_fire_at` 推到未来。
- 触发类型只两种：`daily`（config = `"HH:MM"` 本地 24h）+ `interval`（config = 整数分钟字符串，>0）。
- 构建产物 `src-tauri/target/release/sss-pet.exe`；不要用 `cargo build --release`。

## Review Focus

1. **daily 触发 HH:MM 已过今天 → next_fire 是明天同一时刻**（不能是今天，否则下一 tick 立即重发）。测试：Task 3 `compute_next_fire_daily_past_rolls_to_tomorrow` + `compute_next_fire_daily_exact_now_rolls_to_tomorrow`。
2. **disabled automation 即便 next_fire_at 在过去也不触发**。测试：Task 1 `automation_get_due_skips_disabled`（get_due 过滤 enabled=1）+ Task 4 `run_automation_tick_disabled_not_due`。
3. **新建 enabled automation 的 next_fire_at 非 NULL**（否则 tick 永远找不到）。测试：Task 6 `automation_create_command_sets_next_fire`（命令调 compute_next_fire 再 insert）。
4. **手动"立即运行"在上一次还在跑时不跳过、不报错**（允许并发，开第二个会话）。测试：Task 6 `dispatch_now_prep_ignores_running_overlap`。
5. **agent 被删 → automation + runs 级联删，调度不炸**。测试：Task 1 `automation_cascade_on_agent_delete` + Task 2 `automation_runs_cascade_on_automation_delete`。

---

### Task 1: DB — `automations` 表 + `Automation` struct + CRUD

**Files:**
- Modify: `src-tauri/src/db.rs`（`migrate()` ~245-377 加 CREATE TABLE；struct 近 `Agent` 1303；CRUD 近 `agent_*` 1355-1673）
- Test: `src-tauri/src/db.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: existing `fresh_db()` test helper（返回 `(TempDir, Connection)`）、`agent_create`（造一个 agent 拿 agent_id）、`db::open` 的 `PRAGMA foreign_keys=ON`。
- Produces（后续 task 用这些确切签名）:
```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct Automation {
    pub id: String, pub name: String, pub agent_id: String, pub prompt: String,
    pub trigger_type: String, pub trigger_config: String, pub enabled: bool,
    pub last_run_at: Option<String>, pub next_fire_at: Option<String>,
    pub created_at: String, pub updated_at: String,
}
pub fn automation_list(conn: &Connection) -> rusqlite::Result<Vec<Automation>>;
pub fn automation_get(conn: &Connection, id: &str) -> rusqlite::Result<Option<Automation>>;
pub fn automation_create(conn: &mut Connection, name: &str, agent_id: &str, prompt: &str,
    trigger_type: &str, trigger_config: &str, enabled: bool,
    next_fire_at: Option<String>) -> rusqlite::Result<Automation>;
pub fn automation_update(conn: &mut Connection, id: &str, name: &str, agent_id: &str,
    prompt: &str, trigger_type: &str, trigger_config: &str, enabled: bool,
    next_fire_at: Option<String>) -> rusqlite::Result<Automation>;
pub fn automation_delete(conn: &Connection, id: &str) -> rusqlite::Result<()>;
pub fn automation_set_next_fire(conn: &Connection, id: &str, next_fire_at: Option<String>) -> rusqlite::Result<()>;
pub fn automation_get_due(conn: &Connection, now_utc: &str) -> rusqlite::Result<Vec<Automation>>;
pub fn automation_set_last_run(conn: &Connection, id: &str, last_run_at: Option<String>) -> rusqlite::Result<()>;
```
  - `automation_create`/`update` 收 `next_fire_at` 参数（caller 算，db 不依赖 automation.rs）。
  - `automation_get_due`：`SELECT … WHERE enabled=1 AND next_fire_at IS NOT NULL AND next_fire_at <= ?` ORDER BY next_fire_at。
  - id 用 `uuid::Uuid::new_v4()`；created_at/updated_at 用 `Utc::now().to_rfc3339()`。
  - `enabled` 存 INTEGER 0/1，struct 里 bool，serde 自动处理（用 `i64 as bool`）。

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn automation_create_and_list() {
    let (_t, mut conn) = fresh_db();
    let aid = agent_create(&conn, "A", None, "claude", None, None, None).unwrap().id;
    let a = automation_create(&mut conn, "每日总结", &aid, "总结代办",
        "daily", "09:00", true, Some("2026-10-26T01:00:00+00:00".into())).unwrap();
    assert_eq!(a.name, "每日总结");
    assert_eq!(a.trigger_type, "daily");
    assert!(a.enabled);
    let list = automation_list(&conn).unwrap();
    assert_eq!(list.len(), 1);
}

#[test]
fn automation_get_due_skips_disabled() {
    let (_t, mut conn) = fresh_db();
    let aid = agent_create(&conn, "A", None, "claude", None, None, None).unwrap().id;
    let past = "2000-01-01T00:00:00+00:00".to_string();
    automation_create(&mut conn, "off", &aid, "p", "daily", "09:00", false, Some(past.clone())).unwrap();
    automation_create(&mut conn, "on",  &aid, "p", "daily", "09:00", true,  Some(past)).unwrap();
    let due = automation_get_due(&conn, "2026-10-26T00:00:00+00:00").unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].name, "on");
}

#[test]
fn automation_cascade_on_agent_delete() {
    let (_t, mut conn) = fresh_db();
    let agent = agent_create(&conn, "A", None, "claude", None, None, None).unwrap();
    automation_create(&mut conn, "x", &agent.id, "p", "interval", "60", true, None).unwrap();
    assert_eq!(automation_list(&conn).unwrap().len(), 1);
    agent_delete(&conn, &agent.id).unwrap();
    assert!(automation_list(&conn).unwrap().is_empty());
}

#[test]
fn automation_update_and_set_next_fire() {
    let (_t, mut conn) = fresh_db();
    let aid = agent_create(&conn, "A", None, "claude", None, None, None).unwrap().id;
    let a = automation_create(&mut conn, "n", &aid, "p", "daily", "09:00", true, None).unwrap();
    automation_set_next_fire(&conn, &a.id, Some("2026-11-01T01:00:00+00:00".into())).unwrap();
    let a2 = automation_update(&mut conn, &a.id, "n2", &aid, "p2", "interval", "30", true, None).unwrap();
    assert_eq!(a2.name, "n2");
    assert_eq!(a2.trigger_config, "30");
    // set_next_fire 不被 update 覆盖（update 传 None 时保留旧值）
    let got = automation_get(&conn, &a.id).unwrap().unwrap();
    assert_eq!(got.next_fire_at.as_deref(), Some("2026-11-01T01:00:00+00:00"));
}
```
  > `automation_update` 传 `next_fire_at=None` 时保留旧值（不写 NULL）；传 `Some` 时覆盖。这样命令层只在 trigger/enabled 变时才重算。

- [ ] **Step 2: Run tests, verify fail**

Run: `cargo test --lib automation_`
Expected: FAIL（函数/表未定义）。

- [ ] **Step 3: Implement**

- 在 `migrate()` 加 `CREATE TABLE IF NOT EXISTS automations (...)`（按 spec schema，FK `agent_id` REFERENCES agents(id) ON DELETE CASCADE）。
- 加 `Automation` struct（上方签名）。
- 实现 7 个函数。`update` 用 `UPDATE … SET …`，`next_fire_at` 通过 COALESCE 保留旧值：`COALESCE(?next_fire_at, next_fire_at)`。`get_due` 用参数化 `?now_utc`。
- `automation_delete`：依赖 FK CASCADE（不手动删 runs）。

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test --lib automation_`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/db.rs
git commit -m "feat(db): automations table + Automation CRUD"
```

---

### Task 2: DB — `automation_runs` 表 + `AutomationRun` struct + CRUD

**Files:**
- Modify: `src-tauri/src/db.rs`（`migrate()` 加表；struct 近 `Automation`；CRUD 近 `automation_*`）
- Test: `src-tauri/src/db.rs` tests

**Interfaces:**
- Consumes: Task 1 的 `automation_create` + `agent_create`、existing `new_conversation`（造一个 conv 拿 conv_id 给 `conversation_id` 列）。
- Produces:
```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct AutomationRun {
    pub id: String, pub automation_id: String, pub conversation_id: Option<String>,
    pub source: String, pub status: String, pub started_at: String,
    pub finished_at: Option<String>, pub error: Option<String>,
    pub result_summary: Option<String>, pub created_at: String,
}
pub fn automation_run_list(conn: &Connection, automation_id: &str) -> rusqlite::Result<Vec<AutomationRun>>;
pub fn automation_run_get(conn: &Connection, id: &str) -> rusqlite::Result<Option<AutomationRun>>;
pub fn automation_run_create(conn: &mut Connection, automation_id: &str, conversation_id: Option<&str>,
    source: &str, status: &str, started_at: &str) -> rusqlite::Result<AutomationRun>;
pub fn automation_run_update(conn: &mut Connection, id: &str, status: &str,
    finished_at: Option<String>, error: Option<String>,
    result_summary: Option<String>) -> rusqlite::Result<AutomationRun>;
pub fn automation_run_has_running(conn: &Connection, automation_id: &str) -> rusqlite::Result<bool>;
```
  - `automation_run_list`：`WHERE automation_id=?` ORDER BY `started_at DESC` LIMIT 100。
  - `automation_run_has_running`：`SELECT 1 FROM automation_runs WHERE automation_id=? AND status='running' LIMIT 1`。
  - `automation_run_update`：传 `None` 的字段保留旧值（COALESCE）。

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn automation_run_create_list_has_running() {
    let (_t, mut conn) = fresh_db();
    let aid = agent_create(&conn, "A", None, "claude", None, None, None).unwrap().id;
    let a = automation_create(&mut conn, "n", &aid, "p", "interval", "60", true, None).unwrap();
    let r = automation_run_create(&mut conn, &a.id, None, "schedule", "running", "2026-10-26T00:00:00+00:00").unwrap();
    assert!(automation_run_has_running(&conn, &a.id).unwrap());
    let list = automation_run_list(&conn, &a.id).unwrap();
    assert_eq!(list.len(), 1);
    let r2 = automation_run_update(&mut conn, &r.id, "completed", Some("2026-10-26T00:01:00+00:00".into()), None, Some("ok summary".into())).unwrap();
    assert_eq!(r2.status, "completed");
    assert_eq!(r2.result_summary.as_deref(), Some("ok summary"));
    assert!(!automation_run_has_running(&conn, &a.id).unwrap());
}

#[test]
fn automation_runs_cascade_on_automation_delete() {
    let (_t, mut conn) = fresh_db();
    let aid = agent_create(&conn, "A", None, "claude", None, None, None).unwrap().id;
    let a = automation_create(&mut conn, "n", &aid, "p", "interval", "60", true, None).unwrap();
    automation_run_create(&mut conn, &a.id, None, "schedule", "running", "2026-10-26T00:00:00+00:00").unwrap();
    assert_eq!(automation_run_list(&conn, &a.id).unwrap().len(), 1);
    automation_delete(&conn, &a.id).unwrap();
    assert!(automation_run_list(&conn, &a.id).unwrap().is_empty());
}
```

- [ ] **Step 2: Run tests, verify fail** — `cargo test --lib automation_run_`

- [ ] **Step 3: Implement**

- `migrate()` 加 `CREATE TABLE IF NOT EXISTS automation_runs (...)`（FK `automation_id` CASCADE、`conversation_id` REFERENCES conversations(id) ON DELETE SET NULL）+ `idx_automation_runs_automation`。
- 加 `AutomationRun` struct + 5 函数。

- [ ] **Step 4: Run tests, verify pass** — `cargo test --lib automation_run_`

- [ ] **Step 5: Commit** — `git commit -m "feat(db): automation_runs table + AutomationRun CRUD"`

---

### Task 3: `compute_next_fire` 纯函数

**Files:**
- Create: `src-tauri/src/automation.rs`（新模块，先只放这个函数 + `#[cfg(test)]`）
- Modify: `src-tauri/src/lib.rs`（加 `pub mod automation;`，近其它 `pub mod` 声明）
- Test: `src-tauri/src/automation.rs` tests

**Interfaces:**
- Consumes: `chrono`（`DateTime<Local>`、`Local`、`NaiveDate`/`NaiveTime`、`TimeZone`）——项目已用 chrono。
- Produces:
```rust
/// 算下一次 fire 的本地时刻。daily: 下一个本地 HH:MM（严格 > from；今天已过则明天）。
/// interval: from + N 分钟。未知类型 / 非法 config → None。
pub fn compute_next_fire(trigger_type: &str, trigger_config: &str, from: DateTime<Local>) -> Option<DateTime<Local>>;
```

- [ ] **Step 1: Write failing tests**

```rust
fn local_dt(y:i32,mo:u32,d:u32,h:u32,mi:u32) -> DateTime<Local> {
    let ndt = NaiveDate::from_ymd_opt(y,mo,d).unwrap().and_hms_opt(h,mi,0).unwrap();
    Local.from_local_datetime(&ndt).single().unwrap()
}

#[test]
fn compute_next_fire_daily_future_today() {
    let n = compute_next_fire("daily", "09:00", local_dt(2026,10,26,8,0)).unwrap();
    assert_eq!(n, local_dt(2026,10,26,9,0));
}
#[test]
fn compute_next_fire_daily_past_rolls_to_tomorrow() {
    let n = compute_next_fire("daily", "09:00", local_dt(2026,10,26,9,30)).unwrap();
    assert_eq!(n, local_dt(2026,10,27,9,0));
}
#[test]
fn compute_next_fire_daily_exact_now_rolls_to_tomorrow() {
    // 严格 > from，避免下一 tick 立即重发
    let n = compute_next_fire("daily", "09:00", local_dt(2026,10,26,9,0)).unwrap();
    assert_eq!(n, local_dt(2026,10,27,9,0));
}
#[test]
fn compute_next_fire_interval_adds_minutes() {
    let n = compute_next_fire("interval", "60", local_dt(2026,10,26,8,0)).unwrap();
    assert_eq!(n, local_dt(2026,10,26,9,0));
}
#[test]
fn compute_next_fire_rejects_invalid() {
    let f = local_dt(2026,10,26,8,0);
    assert_eq!(compute_next_fire("weekly", "x", f), None);
    assert_eq!(compute_next_fire("daily", "25:00", f), None);
    assert_eq!(compute_next_fire("daily", "abc", f), None);
    assert_eq!(compute_next_fire("interval", "0", f), None);
    assert_eq!(compute_next_fire("interval", "-5", f), None);
    assert_eq!(compute_next_fire("interval", "abc", f), None);
}
```

- [ ] **Step 2: Run tests, verify fail** — `cargo test --lib compute_next_fire`

- [ ] **Step 3: Implement `compute_next_fire` in `src-tauri/src/automation.rs`**

- `daily`：解析 `"HH:MM"` → `NaiveTime`；拼今天本地日期；若 `<= from` 则 `+1 day`；返回 `Local` datetime。解析失败 → None。
- `interval`：`trigger_config.parse::<i64>()`，`<=0` → None；返回 `from + Duration::minutes(n)`。
- 其它 type → None。
- 顶部 `use chrono::{DateTime, Local, NaiveDate, TimeZone};` + `pub mod` 在 lib.rs。

- [ ] **Step 4: Run tests, verify pass** — `cargo test --lib compute_next_fire`

- [ ] **Step 5: Commit** — `git add src-tauri/src/automation.rs src-tauri/src/lib.rs` + `git commit -m "feat(automation): compute_next_fire (daily/interval)"`

---

### Task 4: `run_automation_tick` 同步 DB 准备

**Files:**
- Modify: `src-tauri/src/automation.rs`（加函数 + `AutomationDispatch` struct + tests）
- Test: `src-tauri/src/automation.rs` tests

**Interfaces:**
- Consumes: Task 1（`automation_get_due`/`automation_set_next_fire`）、Task 2（`automation_run_create`/`automation_run_has_running`）、existing `db::new_conversation`/`db::save_conversation`/`db::agent_get`/`db::agent_enabled_skills`/`db::skill_file_list`、Task 3 `compute_next_fire`。
- Produces:
```rust
pub struct AutomationDispatch {
    pub automation_id: String, pub automation_name: String,
    pub run_id: String, pub conv_id: String,
    pub provider: String, pub model: Option<String>, pub cwd: String,
    pub prompt: String, pub system_prompt: Option<String>, pub mcp_config: Option<String>,
    pub enabled_skills: Vec<(crate::db::Skill, Vec<crate::db::SkillFile>)>,
}
/// 共享私有：建 run 行(source/status="running"/started_at=now) + 建 conv(🤖 title) + 加载 agent/skills → (run, dispatch)。
/// 不查 overlap、不动 next_fire/last_run——调用方各自决定。Task 6 的 dispatch_now_prep 复用它。
fn prepare_dispatch(conn: &mut Connection, automation: &Automation, source: &str, now: DateTime<Utc>) -> Result<(AutomationRun, AutomationDispatch), String>;
/// 锁内、同步：查 due、应用 missed(>5min → 只推进)/overlap(running → 跳过)/in-window(派发)。
/// in-window 流程：`prepare_dispatch(conn, &auto, "schedule", now)` 拿 (run, dispatch) → push dispatch；
///   `automation_set_last_run(id, Some(now.to_rfc3339()))` + `automation_set_next_fire(id, compute_next_fire(now_local)→UTC rfc3339)`。
/// 返回待异步派发的 dispatch 列表。不调 LLM、不持锁等 IO。
pub fn run_automation_tick(conn: &mut Connection, now: DateTime<Utc>) -> Vec<AutomationDispatch>;
```
  - `now` 传 `DateTime<Utc>`（与 `run_reminder_tick` 一致）；内部 `let now_local = now.with_timezone(&Local);` 喂 `compute_next_fire`；存 `next_fire_at` 用 `compute_next_fire(...).map(|dt| dt.with_timezone(&Utc).to_rfc3339())`。
  - 建 conversation：`let conv = db::new_conversation(conn, &provider, model.clone(), &cwd, Some(agent_id))?;` 然后 `conv.title = format!("🤖 {} — {}", name, now_local.format("%Y-%m-%d %H:%M"));` + `db::save_conversation(conn, &conv)?`（upsert 改 title）。
  - 错过判定：`next_fire_at < now - 5min` → `automation_set_next_fire(id, compute_next_fire(now_local))`，continue（不建 run）。
  - 重叠：`automation_run_has_running(id)` → continue（不建 run、不推进 next_fire）。
  - in-window：`let (run, dispatch) = prepare_dispatch(conn, &auto, "schedule", now)?;` → push dispatch；`automation_set_last_run(id, Some(now.to_rfc3339()))` + `automation_set_next_fire(id, compute_next_fire(now_local) 转 UTC rfc3339)`。（prepare_dispatch 内部建 run+conv+加载 agent/skills；cwd 用 `agent.cwd.unwrap_or(".")`。）

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn run_automation_tick_in_window_creates_run_and_advances() {
    let (_t, mut conn) = fresh_db();
    let aid = db::agent_create(&conn, "A", None, "claude", Some("m".into()), Some("sys".into()), None).unwrap().id;
    let a = db::automation_create(&mut conn, "n", &aid, "do it", "interval", "60", true, None).unwrap();
    // next_fire 设成 1 分钟前（在 5min 窗口内）
    let now = Utc::now();
    let past = (now - chrono::Duration::minutes(1)).to_rfc3339();
    db::automation_set_next_fire(&conn, &a.id, Some(past)).unwrap();
    let d = run_automation_tick(&mut conn, now);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].automation_id, a.id);
    assert!(db::automation_run_has_running(&conn, &a.id).unwrap());
    // next_fire 已推进到未来
    let a2 = db::automation_get(&conn, &a.id).unwrap().unwrap();
    let nf = DateTime::parse_from_rfc3339(a2.next_fire_at.as_deref().unwrap()).unwrap().with_timezone(&Utc);
    assert!(nf > now);
    // last_run_at 已设
    assert!(a2.last_run_at.is_some());
    // conversation 建了 + title 带 🤖
    let conv = db::load_conversation(&conn, &d[0].conv_id).unwrap().unwrap();
    assert!(conv.title.starts_with("🤖"));
}

#[test]
fn run_automation_tick_overlap_skips() {
    let (_t, mut conn) = fresh_db();
    let aid = db::agent_create(&conn, "A", None, "claude", None, None, None).unwrap().id;
    let a = db::automation_create(&mut conn, "n", &aid, "p", "interval", "60", true, None).unwrap();
    // 已有 running run
    db::automation_run_create(&mut conn, &a.id, None, "schedule", "running", &Utc::now().to_rfc3339()).unwrap();
    db::automation_set_next_fire(&conn, &a.id, Some((Utc::now() - chrono::Duration::minutes(1)).to_rfc3339())).unwrap();
    let d = run_automation_tick(&mut conn, Utc::now());
    assert!(d.is_empty());
    // 不应新建 run 行（仍只有 1 条）
    assert_eq!(db::automation_run_list(&conn, &a.id).unwrap().len(), 1);
}

#[test]
fn run_automation_tick_missed_only_advances() {
    let (_t, mut conn) = fresh_db();
    let aid = db::agent_create(&conn, "A", None, "claude", None, None, None).unwrap().id;
    let a = db::automation_create(&mut conn, "n", &aid, "p", "interval", "60", true, None).unwrap();
    // next_fire 1 小时前（超出 5min 窗口）
    db::automation_set_next_fire(&conn, &a.id, Some((Utc::now() - chrono::Duration::hours(1)).to_rfc3339())).unwrap();
    let d = run_automation_tick(&mut conn, Utc::now());
    assert!(d.is_empty());
    assert!(!db::automation_run_has_running(&conn, &a.id).unwrap());
    // next_fire 推进到未来
    let a2 = db::automation_get(&conn, &a.id).unwrap().unwrap();
    let nf = DateTime::parse_from_rfc3339(a2.next_fire_at.as_deref().unwrap()).unwrap().with_timezone(&Utc);
    assert!(nf > Utc::now());
}

#[test]
fn run_automation_tick_disabled_not_due() {
    let (_t, mut conn) = fresh_db();
    let aid = db::agent_create(&conn, "A", None, "claude", None, None, None).unwrap().id;
    let a = db::automation_create(&mut conn, "n", &aid, "p", "interval", "60", false, None).unwrap();
    db::automation_set_next_fire(&conn, &a.id, Some((Utc::now() - chrono::Duration::minutes(1)).to_rfc3339())).unwrap();
    let d = run_automation_tick(&mut conn, Utc::now());
    assert!(d.is_empty());
    let _ = a;
}
```

- [ ] **Step 2: Run tests, verify fail** — `cargo test --lib run_automation_tick`

- [ ] **Step 3: Implement `AutomationDispatch` + `run_automation_tick`**

- 按上面 Interfaces 块的逻辑。用 `log::warn!` 记 skipped 原因。`agent_get` 返回 None（agent 被删但 FK 没级联？不会发生）→ 跳过该条 + log。

- [ ] **Step 4: Run tests, verify pass** — `cargo test --lib run_automation_tick`

- [ ] **Step 5: Commit** — `git commit -m "feat(automation): run_automation_tick sync DB prep + AutomationDispatch"`

---

### Task 5: run 行收尾 helpers（emit 累加器调）

**Files:**
- Modify: `src-tauri/src/automation.rs`（加两个 finalize 函数 + tests）
- Test: `src-tauri/src/automation.rs` tests

**Interfaces:**
- Consumes: Task 2 `automation_run_update`、existing `db::StoredMessage`（`{ role, content, kind, tool, created_at }`）。
- Produces:
```rust
/// chat_result 时调：status='completed', finished_at=now, result_summary = 最后一条 assistant 消息 content[:200]。
pub fn finalize_run_on_result(conn: &mut Connection, run_id: &str, accumulated: &[StoredMessage]) -> rusqlite::Result<()>;
/// chat_error 时调：status='failed', finished_at=now, error=msg。
pub fn finalize_run_on_error(conn: &mut Connection, run_id: &str, error: &str) -> rusqlite::Result<()>;
```
  - `result_summary`：取 `accumulated.iter().rev().find(|m| m.role == "assistant").map(|m| m.content.chars().take(200).collect::<String>())`。无 assistant 消息 → None。
  - `finished_at` = `Utc::now().to_rfc3339()`。

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn finalize_run_on_result_summary_from_last_assistant() {
    let (_t, mut conn) = fresh_db();
    let aid = db::agent_create(&conn, "A", None, "claude", None, None, None).unwrap().id;
    let a = db::automation_create(&mut conn, "n", &aid, "p", "interval", "60", true, None).unwrap();
    let r = db::automation_run_create(&mut conn, &a.id, None, "schedule", "running", &Utc::now().to_rfc3339()).unwrap();
    let msgs = vec![
        StoredMessage { role: "user".into(), content: "hi".into(), kind: None, tool: None, created_at: None },
        StoredMessage { role: "assistant".into(), content: "这是回复，比较长的内容…".into(), kind: None, tool: None, created_at: None },
    ];
    finalize_run_on_result(&mut conn, &r.id, &msgs).unwrap();
    let got = db::automation_run_get(&conn, &r.id).unwrap().unwrap();
    assert_eq!(got.status, "completed");
    assert!(got.finished_at.is_some());
    assert!(got.result_summary.as_deref().unwrap().contains("这是回复"));
}

#[test]
fn finalize_run_on_result_no_assistant_keeps_none() {
    let (_t, mut conn) = fresh_db();
    let aid = db::agent_create(&conn, "A", None, "claude", None, None, None).unwrap().id;
    let a = db::automation_create(&mut conn, "n", &aid, "p", "interval", "60", true, None).unwrap();
    let r = db::automation_run_create(&mut conn, &a.id, None, "schedule", "running", &Utc::now().to_rfc3339()).unwrap();
    finalize_run_on_result(&mut conn, &r.id, &[StoredMessage { role: "user".into(), content: "x".into(), kind: None, tool: None, created_at: None }]).unwrap();
    let got = db::automation_run_get(&conn, &r.id).unwrap().unwrap();
    assert_eq!(got.status, "completed");
    assert!(got.result_summary.is_none());
}

#[test]
fn finalize_run_on_error_sets_failed() {
    let (_t, mut conn) = fresh_db();
    let aid = db::agent_create(&conn, "A", None, "claude", None, None, None).unwrap().id;
    let a = db::automation_create(&mut conn, "n", &aid, "p", "interval", "60", true, None).unwrap();
    let r = db::automation_run_create(&mut conn, &a.id, None, "schedule", "running", &Utc::now().to_rfc3339()).unwrap();
    finalize_run_on_error(&mut conn, &r.id, "agent::new failed").unwrap();
    let got = db::automation_run_get(&conn, &r.id).unwrap().unwrap();
    assert_eq!(got.status, "failed");
    assert_eq!(got.error.as_deref(), Some("agent::new failed"));
}
```

- [ ] **Step 2: Run tests, verify fail** — `cargo test --lib finalize_run`

- [ ] **Step 3: Implement two helpers** — 见 Interfaces 块。

- [ ] **Step 4: Run tests, verify pass** — `cargo test --lib finalize_run`

- [ ] **Step 5: Commit** — `git commit -m "feat(automation): finalize_run_on_result/on_error helpers"`

---

### Task 6: 异步派发 + 手动立即运行 + Tauri 命令 + 注册

**Files:**
- Modify: `src-tauri/src/automation.rs`（加 `automation_dispatch_async`/`automation_dispatch_now_prep`/`automation_dispatch_now` + 命令）
- Modify: `src-tauri/src/lib.rs`（`invoke_handler!` 757 注册命令）
- Test: `src-tauri/src/automation.rs` tests（`dispatch_now_prep` 可测；`dispatch_async` 编译验证）

**Interfaces:**
- Consumes: Task 4 `AutomationDispatch`/`run_automation_tick`、Task 5 `finalize_run_on_result`/`finalize_run_on_error`、existing `crate::skill::materialize_skills`/`cleanup_skill_dirs`、`crate::chat::run_chat_turn`、`agent::new`/`BackendConfig`、`db::load_messages`、Tauri `AppHandle`/`emit_to`/`Manager`。
- Produces（命令签名，camelCase 入参）:
```rust
#[tauri::command]
pub async fn automation_list_command(db: State<'_, DbState>) -> Result<Vec<Automation>, String>;
#[tauri::command]
pub async fn automation_create_command(db: State<'_, DbState>, name: String, agent_id: String,
    prompt: String, trigger_type: String, trigger_config: String, enabled: bool) -> Result<Automation, String>;
#[tauri::command]
pub async fn automation_update_command(db: State<'_, DbState>, id: String, name: String, agent_id: String,
    prompt: String, trigger_type: String, trigger_config: String, enabled: bool) -> Result<Automation, String>;
#[tauri::command]
pub async fn automation_delete_command(db: State<'_, DbState>, id: String) -> Result<(), String>;
#[tauri::command]
pub async fn automation_set_enabled_command(db: State<'_, DbState>, id: String, enabled: bool) -> Result<Automation, String>;
#[tauri::command]
pub async fn automation_run_now_command(app: AppHandle, db: State<'_, DbState>, id: String) -> Result<AutomationRun, String>;
#[tauri::command]
pub async fn automation_run_list_command(db: State<'_, DbState>, automation_id: String) -> Result<Vec<AutomationRun>, String>;
#[tauri::command]
pub async fn automation_run_messages_command(db: State<'_, DbState>, run_id: String) -> Result<Vec<StoredMessage>, String>;
```
  > 命令名加 `_command` 后缀避免与 `db::automation_*` 同名冲突；前端 invoke 字符串用去后缀的 `automation_list` 等（在 `generate_handler!` 里注册时用 `automation_list_command as automation_list` 别名，或直接注册全名——按 lib.rs 现有 agent 命令的别名约定走）。
  > `automation_create_command`/`update_command`/`set_enabled_command` 在落库前调 `compute_next_fire`（enabled=true 时）算 `next_fire_at`；enabled=false 时 `next_fire_at=None`。
  > `automation_run_now_command`：调 `automation_dispatch_now_prep(conn, id, now) -> Result<(AutomationRun, AutomationDispatch), String>`（= load automation + 复用 Task 4 的 `prepare_dispatch(conn, &auto, "manual", now)`，**不**查 overlap、**不**动 next_fire/last_run），拿 `(run, dispatch)`；返回 run 行给前端；`tauri::async_runtime::spawn(automation_dispatch_async(app, db_arc, dispatch))`。
  > `automation_run_messages_command`：run_id → `automation_run_get` → `conversation_id` → `db::load_messages(conv_id)`。conversation_id 为 None → 返回空 vec。

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn dispatch_now_prep_ignores_running_overlap() {
    let (_t, mut conn) = fresh_db();
    let aid = db::agent_create(&conn, "A", None, "claude", Some("m".into()), Some("sys".into()), None).unwrap().id;
    let a = db::automation_create(&mut conn, "n", &aid, "p", "interval", "60", true, None).unwrap();
    // 已有 running run（定时触发的）
    db::automation_run_create(&mut conn, &a.id, None, "schedule", "running", &Utc::now().to_rfc3339()).unwrap();
    let (run, _dispatch) = automation_dispatch_now_prep(&mut conn, &a.id, Utc::now()).unwrap();
    assert_eq!(run.source, "manual");
    assert_eq!(run.status, "running");
    let runs = db::automation_run_list(&conn, &a.id).unwrap();
    assert_eq!(runs.len(), 2); // 旧 running(schedule) + 新 manual
    assert_eq!(runs.iter().filter(|r| r.source == "manual").count(), 1);
    // manual 不动调度：next_fire/last_run 仍为 None（a 建时未设）
    let a2 = db::automation_get(&conn, &a.id).unwrap().unwrap();
    assert!(a2.next_fire_at.is_none());
    assert!(a2.last_run_at.is_none());
}

#[test]
fn automation_create_command_sets_next_fire() {
    // 通过 automation_create_command 的纯逻辑部分测：enabled=true → next_fire 非 None
    //（若命令难直接测，测抽出的纯函数 compute_initial_next_fire(trigger_type, trigger_config, enabled, now)）
    let nf = compute_initial_next_fire("daily", "09:00", true, Utc::now());
    assert!(nf.is_some());
    let nf2 = compute_initial_next_fire("daily", "09:00", false, Utc::now());
    assert!(nf2.is_none());
}
```
  > 若 `compute_initial_next_fire` 抽出来更可测，就抽成纯函数（trigger_type, trigger_config, enabled, now: DateTime<Utc>) → Option<String>（RFC3339），命令调它。

- [ ] **Step 2: Run tests, verify fail** — `cargo test --lib dispatch_now_prep automation_create_command_sets_next_fire`

- [ ] **Step 3: Implement**

- `automation_dispatch_now_prep(conn, id, now) -> Result<(AutomationRun, AutomationDispatch), String>`：load automation（`automation_get`，None → Err）→ 复用 Task 4 的 `prepare_dispatch(conn, &auto, "manual", now)` → 返回 `(run, dispatch)`。**不**查 overlap、**不**动 next_fire/last_run。
- `automation_dispatch_async(app, db_state, dispatch) -> ()`：照 `chat_send` 的 skill 装配（`materialize_skills(&cwd, &provider, &enabled_skills)` + 拼 `system_prompt`）→ `let backend = agent::new(&provider, BackendConfig::default())?;` → 构造 emit 累加器闭包：
  - 持有 `Arc<Mutex<Vec<StoredMessage>>>`（累 `chat_message` 的 message）+ `app` + `run_id`/`automation_id` + `db_arc`。
  - `chat_message {conv_id, message}`：`accumulated.push(message)` + `app.emit_to("panel", "automation_run_message", {runId, automationId, message})`。
  - `chat_result {conv_id, result}`：`lock db → finalize_run_on_result(run_id, &accumulated) → drop`；`app.emit_to("panel", "automation_run_result", {runId, automationId, result})` + `emit("automation_runs_changed", {automationId})`。
  - `chat_error {conv_id, message}`：`lock db → finalize_run_on_error(run_id, message)`；emit `automation_run_error` + `automation_runs_changed`。
  - 调 `run_chat_turn(backend.as_ref(), emit, db_arc, &conv_id, &prompt, &cwd, system_prompt, mcp_config, materialized.created_dirs).await`。
- 命令实现按 Interfaces 块。`State<'_, DbState>` 按 lib.rs 现有命令的 state 取法（`db.inner().clone()`）。
- lib.rs `generate_handler!` 注册 8 个命令（按现有 agent 命令别名约定）。

- [ ] **Step 4: Run tests + cargo check**

Run: `cargo test --lib dispatch_now_prep automation_create_command_sets_next_fire && cargo check`
Expected: tests PASS + check 通过。

- [ ] **Step 5: Commit** — `git commit -m "feat(automation): dispatch_async + run-now + 8 commands + registration"`

---

### Task 7: `reminders.rs` 调度器接线

**Files:**
- Modify: `src-tauri/src/reminders.rs`（`spawn_reminder_scheduler` 循环体 141-172）
- Test: 现有 4 个 reminders 测试仍通过 + 编译

**Interfaces:**
- Consumes: Task 4 `automation::run_automation_tick`、Task 6 `automation::automation_dispatch_async`、existing `run_reminder_tick`（不动）。
- Produces: scheduler 每 tick 调 `run_automation_tick` + 异步派发。

- [ ] **Step 1: 无新单测（行为由 Task 4 覆盖；此处是接线）—— 先确认基线绿**

Run: `cargo test --lib reminders::`
Expected: PASS（现有 4 个测试）。

- [ ] **Step 2: Implement 接线**

把 `let fired = { … run_reminder_tick(&conn, now, …) };`（141-169）改成同时返回 dispatches：
```rust
let (fired, dispatches) = {
    let conn = match db_state.lock() { … };
    let now = Utc::now();
    // … 现有三个闭包不变 …
    let fired = run_reminder_tick(&conn, now, &mut notify_fn, &mut apply_fn, &mut emit_fn);
    let dispatches = automation::run_automation_tick(&mut conn, now);
    (fired, dispatches)
}; // 锁释放
if !fired.is_empty() { log::error!("[reminders] fired {} reminder(s)", fired.len()); }
for d in dispatches {
    let app = app.clone();
    let db_state = db_state.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = automation::automation_dispatch_async(app, db_state, d).await {
            log::error!("[automation] dispatch failed: {e}");
        }
    });
}
```
  > `run_automation_tick` 收 `&mut Connection`，而此处 `conn` 是 `MutexGuard`（自动 DerefMut）——传 `&mut *conn` 或直接 `&mut conn`。若编译报借用冲突，把 `run_reminder_tick` 调用与 `run_automation_tick` 调用拆成先后两次 `&mut`（同一 guard 内顺序借用合法）。

- [ ] **Step 3: Run tests + cargo check**

Run: `cargo test --lib reminders:: && cargo check`
Expected: 现有 4 测试 PASS + check 通过。

- [ ] **Step 4: Commit** — `git commit -m "feat(reminders): hook automation tick into scheduler loop"`

---

### Task 8: 前端 api.ts — 接口 + 包装 + 流订阅

**Files:**
- Modify: `src/panel/lib/api.ts`
- Test: `pnpm svelte-check`

**Interfaces:**
- Consumes: Task 6 的 8 个 invoke 命令名 + 4 个事件名。
- Produces:
```ts
export interface Automation { id: string; name: string; agent_id: string; prompt: string;
  trigger_type: string; trigger_config: string; enabled: boolean;
  last_run_at: string | null; next_fire_at: string | null; created_at: string; updated_at: string; }
export interface AutomationRun { id: string; automation_id: string; conversation_id: string | null;
  source: string; status: string; started_at: string; finished_at: string | null;
  error: string | null; result_summary: string | null; created_at: string; }
export async function automationList(): Promise<Automation[]>;
export async function automationCreate(name: string, agentId: string, prompt: string,
  triggerType: string, triggerConfig: string, enabled: boolean): Promise<Automation>;
export async function automationUpdate(id: string, name: string, agentId: string, prompt: string,
  triggerType: string, triggerConfig: string, enabled: boolean): Promise<Automation>;
export async function automationDelete(id: string): Promise<void>;
export async function automationSetEnabled(id: string, enabled: boolean): Promise<Automation>;
export async function automationRunNow(id: string): Promise<AutomationRun>;
export async function automationRunList(automationId: string): Promise<AutomationRun[]>;
export async function automationRunMessages(runId: string): Promise<StoredMessage[]>;
export function subscribeAutomationRunStream(opts: {
  onMessage?: (e: { runId: string; automationId: string; message: StoredMessage }) => void;
  onResult?:  (e: { runId: string; automationId: string; result: unknown }) => void;
  onError?:   (e: { runId: string; automationId: string; message: string }) => void;
  onRunsChanged?: (e: { automationId: string }) => void;
}): () => void;   // 返回 unsubscribe
```
  > invoke 字符串与 lib.rs `generate_handler!` 注册名一致（去 `_command` 后缀，按现有 agent 命令别名约定）。事件 listen 用 `appWindow.listen`（参考现有 `subscribeAgentGenStream` 写法）。

- [ ] **Step 1: Implement**（先写，svelte-check 兜底类型）

- [ ] **Step 2: Run svelte-check**

Run: `pnpm svelte-check`
Expected: 0 errors / 0 warnings。

- [ ] **Step 3: Commit** — `git commit -m "feat(panel): automation api wrappers + run stream subscription"`

---

### Task 9: AgentsView — 自动化子 tab + 列表 + 编辑表单 + 立即运行

**Files:**
- Modify: `src/panel/views/AgentsView.svelte`（`subView` 40；`.sub-tabs` 690-697；`{#if}` switch 699-873 加分支；`<script>` 加 state/actions；`<style>` 加样式）
- Test: `pnpm svelte-check`

**Interfaces:**
- Consumes: Task 8 全部 api；existing agent 列表（`agents` state）给表单下拉。
- Produces: 第三个 sub-tab"自动化"，含列表 + 新建/编辑表单 + 启用开关 + 立即运行。

- [ ] **Step 1: Implement**

- `subView` 联合加 `"automation"`；`.sub-tabs` 加 `<button class="sub-tab" class:active={subView === "automation"} onclick={() => (subView = "automation")}>自动化</button>`。
- `<script>` 加：`let automations = $state<Automation[]>([])`、`let selAuto = $state<Automation | null>(null)`、`let autoForm = $state<{name, agentId, prompt, triggerType, triggerConfig, enabled}>(...)`、`let autoEditing = $state(false)`；actions：`loadAutomations`（调 `automationList`）、`startNewAuto`/`startEditAuto(auto)`/`saveAuto`（create 或 update，按有无 id）、`deleteAuto(id)`、`toggleAutoEnabled(auto)`、`runNow(auto)`。
- UI 分支：左列 automations 列表（name + 触发摘要 "每天 09:00" / "每 60 分钟" + enabled toggle + 上次状态 + 下次 fire + 立即运行/编辑/删除）；右列编辑表单（name input、agent 下拉用 `agents`、prompt textarea、trigger type radio daily/interval、trigger_config = daily 时间输入 / interval 数字+单位下拉分钟/小时存分钟、enabled checkbox、保存/删除/立即运行）。
- 触发摘要显示：`daily` → `每天 ${config}`；`interval` → `每 ${config} 分钟`（小时换算：>=60 且整除 → 显示小时）。
- `onMount` 或切到 automation sub-tab 时 `loadAutomations`。

- [ ] **Step 2: Run svelte-check**

Run: `pnpm svelte-check`
Expected: 0 errors / 0 warnings。

- [ ] **Step 3: Commit** — `git commit -m "feat(panel): automation sub-tab — list + form + run-now"`

---

### Task 10: AgentsView — 历史 + 运行详情 + 实时进度

**Files:**
- Modify: `src/panel/views/AgentsView.svelte`（自动化分支加历史区 + 详情）
- Test: `pnpm svelte-check`

**Interfaces:**
- Consumes: Task 8 `automationRunList`/`automationRunMessages`/`subscribeAutomationRunStream`；Task 9 `selAuto`。
- Produces: 选中 automation 的历史列表 + 点开看完整对话（只读）+ 实时进度。

- [ ] **Step 1: Implement**

- state：`let runs = $state<AutomationRun[]>([])`、`let selRun = $state<AutomationRun | null>(null)`、`let runMsgs = $state<StoredMessage[]>([])`、`let liveMsgs = $state<StoredMessage[]>([])`（当前运行中的实时消息）。
- `selAuto` 变化时 `loadRuns(selAuto.id)`；`selRun` 变化时 `loadRunMsgs(selRun.id)` → `automationRunMessages`。
- 历史区：每行 status 图标（running/completed/failed）、source（schedule/manual）、started_at、finished_at、result_summary（截断 + title 全文）、点击展开/选中。
- 详情区：`runMsgs` 只读渲染（role + content，assistant 文本 + tool 折叠）；复用现有 chat 消息样式。
- 实时进度：`onMount` 调 `subscribeAutomationRunStream`：`onMessage` → 若 `automationId === selAuto?.id` 且 `runId` 属于当前 running run → push 到 `liveMsgs`；`onResult`/`onError` → `loadRuns` 刷新；`onRunsChanged` → `loadRuns`。`onDestroy` 取消订阅。
- 列表项若有 `status='running'` 的 run，显示"运行中…"。

- [ ] **Step 2: Run svelte-check**

Run: `pnpm svelte-check`
Expected: 0 errors / 0 warnings。

- [ ] **Step 3: Commit** — `git commit -m "feat(panel): automation run history + detail + live stream"`

---

### 最终验证

- `cargo test --lib` — 全绿（现有 ~196 + 新增 automation 测试）。
- `pnpm svelte-check` — 0/0。
- `pnpm tauri build --no-bundle` — 成功产出 `src-tauri/target/release/sss-pet.exe`。
- 手动：建一个 interval=1 分钟的 automation（绑一个真实 agent），开着 app 等 1 分钟看自动跑 + 历史多一行 + 点开看对话；再点"立即运行"看手动跑。
