//! Task 14: SQLite persistence (portable + `%APPDATA%` fallback).
//!
//! Owns the on-disk store for everything the desktop pet needs to
//! survive a restart: conversations + their streamed messages + the
//! agent-native `session_id` to resume next turn (Task 13 types, Task
//! 15 chat UI consumes), todos, reminders, and the pet-state log.
//! Replaces Task 4's in-memory placeholder for window-state — that
//! moves to [`crate::config`] (TOML).
//!
//! Design ref: `docs/superpowers/specs/2026-10-21-desktop-pet-design.md`
//! ("数据与持久化" + "便携回退"). Single `sss-pet.db` SQLite file
//! via `rusqlite` (with `bundled` so no system SQLite install is needed
//! — the single `.exe` is self-contained).
//!
//! ## Path resolution (Review Focus #5 — portable + fallback)
//!
//! 1. Try `<exe_dir>/sss-pet.db` (the exe's directory via
//!    [`std::env::current_exe`]). If writable → use it (portable:
//!    data sits next to the `.exe`, matching the design spec).
//! 2. If the exe dir is not writable (Program Files on Windows is
//!    ACL-protected; `/usr` on Unix; a read-only mount) → fall back to
//!    `%APPDATA%\sss-pet\sss-pet.db` (Windows),
//!    `~/.local/share/sss-pet/sss-pet.db` (Unix),
//!    `~/Library/Application Support/sss-pet/sss-pet.db` (macOS) —
//!    via [`dirs::data_dir`]. Created if missing.
//! 3. If both are unavailable, [`open`] returns an error.
//!
//! The fallback is essential for the "single .exe in Program Files"
//! case: Windows ACL-protects Program Files so the exe dir is not
//! user-writable; without the appdata fallback, the app couldn't
//! persist anything. The portable case (exe in a user-writable dir)
//! keeps data next to the `.exe`.

use std::path::{Path, PathBuf};

pub use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::agent::conversation::{Conversation, StoredMessage, TurnRecord};

// ===========================================================================
// Path resolution (portable + %APPDATA% fallback) — Review Focus #5.
// ===========================================================================

/// Resolve the directory that should hold `sss-pet.db` AND `config.toml`
/// (shared with [`crate::config`] so the two files always live together).
///
/// See the module docs for the resolution order (exe dir → appdata
/// fallback). Returns `None` if neither is available; the caller
/// ([`open`] / `config::save`) surfaces that as an error.
pub fn resolve_data_dir() -> Option<PathBuf> {
    #[cfg(test)]
    {
        // Test-only short-circuit: skip the resolution entirely. Used
        // by round-trip tests that just need a writable data dir without
        // caring which path was chosen.
        if let Some(d) = data_dir_override() {
            return Some(d);
        }
    }
    // 1. Try the exe dir.
    if let Some(exe) = exe_dir() {
        if is_writable(&exe) {
            return Some(exe);
        }
    }
    // 2. Fall back to %APPDATA% / XDG / Library. Create the dir if
    //    missing; if creation fails (e.g. the platform has no
    //    data_dir), treat as unavailable.
    let appdata = appdata_dir()?;
    std::fs::create_dir_all(&appdata).ok()?;
    Some(appdata)
}

/// The exe's directory (`current_exe().parent()`), or a test override.
fn exe_dir() -> Option<PathBuf> {
    #[cfg(test)]
    {
        if let Some(d) = exe_dir_override() {
            return Some(d);
        }
    }
    std::env::current_exe().ok()?.parent().map(PathBuf::from)
}

/// The `%APPDATA%\sss-pet`-style fallback, or a test override.
fn appdata_dir() -> Option<PathBuf> {
    #[cfg(test)]
    {
        if let Some(d) = appdata_dir_override() {
            return Some(d);
        }
    }
    Some(dirs::data_dir()?.join("sss-pet"))
}

/// True iff `dir` exists AND we can write a probe file to it.
///
/// Two-stage check (cheap → expensive):
/// 1. `metadata().permissions().readonly()` — catches the *explicit*
///    read-only case cross-platform (Windows `FILE_ATTRIBUTE_READONLY`,
///    Unix owner-write bit cleared). Short-circuits before touching the
///    disk again. NB: on Windows the readonly *bit* on a directory does
///    NOT actually prevent file creation inside — but treating the bit
///    as authoritative respects the user's explicit "I marked this
///    read-only" intent.
/// 2. Create + delete a `.sss-pet-probe-<pid>-<seq>` file — catches the
///    ACL-protected-but-not-flagged case (Program Files on Windows: the
///    dir isn't `readonly`-flagged, but writes still fail). The probe
///    filename is PID + a per-process monotonic counter so two threads
///    in the SAME process probing the SAME dir simultaneously (e.g.
///    parallel `cargo test` threads, or two callers racing on the same
///    exe dir) get DISTINCT filenames — previously PID-only, so the
///    second `File::create` would fail → false negative.
fn is_writable(dir: &Path) -> bool {
    if !dir.exists() {
        return false;
    }
    if std::fs::metadata(dir)
        .map(|m| m.permissions().readonly())
        .unwrap_or(true)
    {
        return false;
    }
    // PID + per-process counter: concurrent probes never collide on the
    // probe filename (the collision used to make the second `File::create`
    // fail spuriously, reporting a writable dir as read-only).
    static PROBE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = PROBE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let probe = dir.join(format!(".sss-pet-probe-{}-{}", std::process::id(), seq));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Test overrides (thread-local so parallel `cargo test` threads don't race).
// ---------------------------------------------------------------------------

#[cfg(test)]
thread_local! {
    static TEST_OVERRIDES: std::cell::RefCell<TestOverrides> = std::cell::RefCell::new(
        TestOverrides { data_dir: None, exe_dir: None, appdata_dir: None }
    );
}

#[cfg(test)]
#[derive(Default)]
struct TestOverrides {
    /// If set, `resolve_data_dir` returns this directly (skips resolution).
    data_dir: Option<PathBuf>,
    /// If set, overrides `current_exe().parent()`.
    exe_dir: Option<PathBuf>,
    /// If set, overrides `dirs::data_dir()`.
    appdata_dir: Option<PathBuf>,
}

#[cfg(test)]
fn data_dir_override() -> Option<PathBuf> {
    TEST_OVERRIDES.with(|c| c.borrow().data_dir.clone())
}

#[cfg(test)]
fn exe_dir_override() -> Option<PathBuf> {
    TEST_OVERRIDES.with(|c| c.borrow().exe_dir.clone())
}

#[cfg(test)]
fn appdata_dir_override() -> Option<PathBuf> {
    TEST_OVERRIDES.with(|c| c.borrow().appdata_dir.clone())
}

/// Test-only: override the path resolution. Returns a guard whose `Drop`
/// clears ALL three overrides (so the next test on the same thread
/// starts clean — important because `cargo test` reuses threads).
///
/// Pass `(Some(dir), None, None)` to short-circuit to a known writable
/// data dir; pass `(None, Some(readonly_dir), Some(writable_dir))` to
/// exercise the fallback (read-only exe dir → writable appdata).
#[cfg(test)]
pub fn set_test_overrides(
    data_dir: Option<PathBuf>,
    exe_dir: Option<PathBuf>,
    appdata_dir: Option<PathBuf>,
) -> TestOverrideGuard {
    TEST_OVERRIDES.with(|c| {
        let mut o = c.borrow_mut();
        o.data_dir = data_dir;
        o.exe_dir = exe_dir;
        o.appdata_dir = appdata_dir;
    });
    TestOverrideGuard { _private: () }
}

/// RAII guard — clearing the overrides on `Drop` so the next test on the
/// same `cargo test` thread starts clean. Returned by [`set_test_overrides`].
#[cfg(test)]
pub struct TestOverrideGuard {
    _private: (),
}

#[cfg(test)]
impl Drop for TestOverrideGuard {
    fn drop(&mut self) {
        TEST_OVERRIDES.with(|c| {
            *c.borrow_mut() = TestOverrides::default();
        });
    }
}

// ===========================================================================
// Open + migrate
// ===========================================================================

/// Open a connection to `sss-pet.db`, creating the file if missing. Path
/// resolution per [`resolve_data_dir`]. The caller MUST call [`migrate`]
/// once after opening a fresh db (idempotent for existing dbs).
pub fn open() -> rusqlite::Result<Connection> {
    let dir = resolve_data_dir().ok_or_else(|| {
        rusqlite::Error::InvalidPath(PathBuf::from("<no writable data dir>"))
    })?;
    let path = dir.join("sss-pet.db");
    let conn = Connection::open(path)?;
    // Enable FK enforcement so the schema's `ON DELETE CASCADE`
    // declarations (agent_skills, skill_files) actually fire. SQLite
    // defaults foreign_keys OFF per-connection; without this, deleting
    // an agent/skill leaves orphaned junction/file rows.
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

/// Create all five tables + the messages index if they don't exist, then
/// idempotently add the Task 1 columns (`conversations.model` /
/// `conversations.cwd` / `messages.kind`). Idempotent throughout —
/// `CREATE TABLE IF NOT EXISTS` for the tables, and the `ALTER TABLE …
/// ADD COLUMN` statements are guarded by a `PRAGMA table_info` check so
/// re-running `migrate` on an already-migrated db is a no-op (the
/// `ADD COLUMN` would otherwise error with "duplicate column name").
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS conversations (
            id TEXT PRIMARY KEY,
            provider TEXT NOT NULL,
            session_id TEXT,
            title TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            conv_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_messages_conv ON messages(conv_id);
        CREATE TABLE IF NOT EXISTS todos (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            text TEXT NOT NULL,
            done INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS reminders (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            text TEXT NOT NULL,
            due_at TEXT NOT NULL,
            fired INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS pet_state_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            state TEXT NOT NULL,
            at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS agents (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT,
            provider TEXT NOT NULL,
            model TEXT,
            system_prompt TEXT,
            mcp_config TEXT,
            cwd TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS skills (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT,
            content TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS agent_skills (
            agent_id TEXT NOT NULL,
            skill_id TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 1,
            PRIMARY KEY (agent_id, skill_id),
            FOREIGN KEY (agent_id) REFERENCES agents(id) ON DELETE CASCADE,
            FOREIGN KEY (skill_id) REFERENCES skills(id) ON DELETE CASCADE
        );
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
        CREATE TABLE IF NOT EXISTS kpis (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            year INTEGER NOT NULL,
            kpi_type TEXT NOT NULL,
            title TEXT NOT NULL,
            details TEXT,
            planned_at TEXT,
            status TEXT NOT NULL DEFAULT 'not_started',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS kpi_targets (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            year INTEGER NOT NULL,
            kpi_type TEXT NOT NULL,
            target_count INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(year, kpi_type)
        );",
    )?;
    // Task 1: idempotent column adds. SQLite's `ALTER TABLE … ADD COLUMN`
    // errors on a pre-existing column ("duplicate column name"), so guard
    // each with a `PRAGMA table_info` lookup first. The default for `cwd`
    // is `'.'` (matches `ExecOptions`'s pre-Task-1 `cwd: PathBuf::from(".")`
    // placeholder in `resume_or_replay`) so old conversation rows — which
    // predate the column — backfill to the process's cwd rather than NULL
    // (the column is NOT NULL).
    add_column_if_missing(conn, "conversations", "model", "TEXT")?;
    add_column_if_missing(conn, "conversations", "cwd", "TEXT NOT NULL DEFAULT '.'")?;
    add_column_if_missing(conn, "messages", "kind", "TEXT")?;
    // Task 6 fix-round-1: persist the tool name on ToolUse/ToolResult
    // rows so history reload shows "使用工具 Bash" instead of the
    // doubled "使用工具 工具" fallback. Idempotent like the others
    // (PRAGMA table_info guard → ADD COLUMN skipped if already present).
    add_column_if_missing(conn, "messages", "tool", "TEXT")?;

    // Todos v2: extend the minimal todos table (id, text, done, created_at)
    // into a full task tracker with status, priority, assignee, time range,
    // and notes. The `status` column replaces the boolean `done` column:
    // existing rows with done=1 are migrated to status='completed'. The
    // `done` column itself is kept (SQLite < 3.35 can't DROP COLUMN, and
    // leaving it is harmless — we just stop reading it).
    add_column_if_missing(conn, "todos", "status", "TEXT NOT NULL DEFAULT 'pending'")?;
    add_column_if_missing(conn, "todos", "priority", "TEXT NOT NULL DEFAULT 'medium'")?;
    add_column_if_missing(conn, "todos", "assignee", "TEXT")?;
    add_column_if_missing(conn, "todos", "start_at", "TEXT")?;
    add_column_if_missing(conn, "todos", "end_at", "TEXT")?;
    add_column_if_missing(conn, "todos", "notes", "TEXT")?;
    // Deadline notification: tracks whether the "approaching deadline"
    // reminder (24h before end_at) has already been fired for this todo.
    // Reset to 0 by todo_update when end_at changes.
    add_column_if_missing(conn, "todos", "deadline_notified", "INTEGER NOT NULL DEFAULT 0")?;
    // One-time data migration: rows that were done=1 (the old boolean)
    // get status='completed'. Idempotent — only touches rows where
    // status is still the default 'pending' AND done=1.
    conn.execute_batch(
        "UPDATE todos SET status = 'completed' WHERE done = 1 AND status = 'pending';",
    )?;
    // Agent management: link a conversation to a saved agent profile.
    // null = no agent (raw provider+model+cwd, the original behavior).
    // Immutable once set at conversation creation time.
    add_column_if_missing(conn, "conversations", "agent_id", "TEXT")?;

    // Reminders v2: extend the minimal reminders table (id, text, due_at,
    // fired) into a full reminder system with status, recurrence, snooze,
    // and next-fire time. Old data is migrated: due_at → next_fire_at,
    // fired=1 → status='done'.
    add_column_if_missing(conn, "reminders", "status", "TEXT NOT NULL DEFAULT 'pending'")?;
    add_column_if_missing(conn, "reminders", "reminder_type", "TEXT NOT NULL DEFAULT 'once'")?;
    add_column_if_missing(conn, "reminders", "interval_value", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(conn, "reminders", "interval_unit", "TEXT NOT NULL DEFAULT 'minutes'")?;
    add_column_if_missing(conn, "reminders", "next_fire_at", "TEXT")?;
    add_column_if_missing(conn, "reminders", "snoozed_until", "TEXT")?;
    add_column_if_missing(conn, "reminders", "created_at", "TEXT")?;
    add_column_if_missing(conn, "reminders", "updated_at", "TEXT")?;
    // One-time data migration: old rows with fired=1 → status='done';
    // old rows with due_at → next_fire_at = due_at (if next_fire_at is NULL).
    conn.execute_batch(
        "UPDATE reminders SET status = 'done' WHERE fired = 1 AND status = 'pending'; \
         UPDATE reminders SET next_fire_at = due_at WHERE next_fire_at IS NULL AND due_at IS NOT NULL AND fired = 0; \
         UPDATE reminders SET created_at = datetime('now') WHERE created_at IS NULL; \
         UPDATE reminders SET updated_at = datetime('now') WHERE updated_at IS NULL;",
    )?;
    Ok(())
}

/// Add a column to `table` if (and only if) it's not already present —
/// the idempotency guard for [`migrate`]'s `ALTER TABLE … ADD COLUMN`
/// statements. `def` is the full column definition after the name
/// (e.g. `"TEXT"`, `"TEXT NOT NULL DEFAULT '.'"`). Returns `Ok(())`
/// whether the column was added or already present.
fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    def: &str,
) -> rusqlite::Result<()> {
    if has_column(conn, table, column)? {
        return Ok(());
    }
    // `def` and `column` are call-site string literals (not user input),
    // so format!-interpolation is safe here.
    let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {def};");
    match conn.execute_batch(&sql) {
        Ok(()) => Ok(()),
        // A race between the `has_column` check and the `ALTER` (two
        // `migrate` calls on the same db in two processes) can land
        // here with a "duplicate column name" error. Re-check and
        // swallow it if the column is now present — idempotent. (The
        // error binding is intentionally unused: if the column IS now
        // present we discard the error; if not, the original error is
        // returned via the catch-all arm below.)
        Err(_) if has_column(conn, table, column).unwrap_or(false) => Ok(()),
        Err(e) => Err(e),
    }
}

/// True iff `table` has a column named `column`. Uses `PRAGMA
/// table_info` (the name column is index 1).
fn has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    for r in rows {
        if r? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

// ===========================================================================
// Conversations + messages CRUD (used by Task 15 chat)
// ===========================================================================

/// Insert (or upsert on `id` conflict) a [`Conversation`] row. Round-trips
/// `created_at` as an RFC3339 string (chrono). Task 1: also stores the
/// conversation's immutable `model` + `cwd` (set at creation by
/// [`new_conversation`], never updated thereafter — the design's "定了不
/// 改" ruling — so the UPSERT's `DO UPDATE` arm does NOT touch them).
pub fn save_conversation(conn: &Connection, conv: &Conversation) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO conversations (id, provider, session_id, title, created_at, model, cwd, agent_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
            provider = excluded.provider,
            session_id = excluded.session_id,
            title = excluded.title,
            created_at = excluded.created_at",
        rusqlite::params![
            conv.id,
            conv.provider,
            conv.session_id,
            conv.title,
            conv.created_at.to_rfc3339(),
            conv.model,
            conv.cwd,
            conv.agent_id,
        ],
    )?;
    Ok(())
}

/// Create a fresh [`Conversation`] row and persist it: `id` = a fresh
/// uuid v4, `provider` + `model` + `cwd` from the caller, `session_id =
/// None` (the agent has no native session yet — the first turn either
/// starts one or replays), `title = "新对话"`, `created_at = now`.
/// Returns the new [`Conversation`] (with the assigned id + timestamp).
///
/// Task 1 helper — the design's "改动 1" ruling is that a conversation's
/// `agent + model + cwd` triple is fixed at creation and never changes.
/// This is the single construction site for that triple; the chat
/// command (`chat::new_conversation`) delegates here so the uuid +
/// default-title + persist logic lives in one place.
///
/// **Signature note:** the brief lists `new_conversation(provider, model,
/// cwd) -> Conversation` (no connection arg). The implemented signature
/// takes a `&Connection` first — matching every other helper in this
/// module (`save_conversation(conn, …)`, `load_conversation(conn, …)`,
/// …) AND the chat command's managed `Arc<Mutex<Connection>>` (single
/// connection, serialized writes). A no-arg variant that called `open()`
/// internally would bypass that serialization and, in tests, miss the
/// thread-local path override (the `fresh_db()` guard drops before the
/// call). Returns `rusqlite::Result<Conversation>` (not a bare
/// `Conversation`) because [`save_conversation`] is fallible I/O.
pub fn new_conversation(
    conn: &Connection,
    provider: &str,
    model: Option<String>,
    cwd: &str,
    agent_id: Option<String>,
) -> rusqlite::Result<Conversation> {
    let conv = Conversation {
        id: uuid::Uuid::new_v4().to_string(),
        provider: provider.to_string(),
        session_id: None,
        title: "新对话".to_string(),
        created_at: chrono::Utc::now(),
        model,
        cwd: cwd.to_string(),
        agent_id,
    };
    save_conversation(conn, &conv)?;
    Ok(conv)
}

/// Load one conversation by id, or `None` if no such row. Task 1: also
/// reads the immutable `model` + `cwd` columns.
pub fn load_conversation(conn: &Connection, id: &str) -> rusqlite::Result<Option<Conversation>> {
    let row = conn.query_row(
        "SELECT id, provider, session_id, title, created_at, model, cwd, agent_id \
         FROM conversations WHERE id = ?1",
        [id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        },
    );
    match row {
        Ok((id, provider, session_id, title, created_at_str, model, cwd, agent_id)) => {
            let created_at = parse_dt(&created_at_str)?;
            Ok(Some(Conversation {
                id,
                provider,
                session_id,
                title,
                created_at,
                model,
                cwd,
                agent_id,
            }))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// List all conversations, oldest-first (Task 15's history sidebar).
/// Task 1: also reads the immutable `model` + `cwd` columns (the
/// sidebar labels threads by project dir + model).
pub fn list_conversations(conn: &Connection) -> rusqlite::Result<Vec<Conversation>> {
    let mut stmt = conn.prepare(
        "SELECT id, provider, session_id, title, created_at, model, cwd, agent_id \
         FROM conversations ORDER BY created_at",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, Option<String>>(7)?,
        ))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (id, provider, session_id, title, created_at_str, model, cwd, agent_id) = r?;
        out.push(Conversation {
            id,
            provider,
            session_id,
            title,
            created_at: parse_dt(&created_at_str)?,
            model,
            cwd,
            agent_id,
        });
    }
    Ok(out)
}

/// Append one stored message to a conversation. `created_at` is the
/// current UTC time (the message's row id preserves insertion order —
/// `load_messages` orders by id). Task 1: also stores the message's
/// `kind` (the [`MessageKind`](crate::agent::MessageKind) variant name,
/// or `None` for rows written without one — e.g. the user-prompt append
/// in `chat::run_chat_turn` which has no `MessageKind` tag). Task 6
/// fix-round-1: also stores the `tool` name (the streamed
/// [`Message::tool`], `None` for non-tool kinds + pre-fix rows) so
/// history ToolUse chips show the tool identity on reload.
pub fn append_message(conn: &Connection, conv_id: &str, msg: &StoredMessage) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO messages (conv_id, role, content, created_at, kind, tool) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            conv_id,
            msg.role,
            msg.content,
            chrono::Utc::now().to_rfc3339(),
            msg.kind,
            msg.tool,
        ],
    )?;
    Ok(())
}

/// Load all stored messages for a conversation, in arrival order.
/// Task 1: reads the `kind` column. For rows where `kind IS NULL` (pre-
/// Task-1 data, OR new rows written without a kind), infer a `kind`
/// from `role`: assistant→`Some("Text")`, user/tool→
/// `Some("ToolResult")`, else `None`. The backfill is a heuristic so
/// reopening a pre-Task-1 history still renders sensible kinds (text
/// bubbles for assistant turns, tool-result chips for the user/tool
/// side) instead of all-`None`. Task 6 fix-round-1: also reads the
/// `tool` column (`None` for pre-fix rows + non-tool kinds); `tool` is
/// NOT inferred from role (only `kind` is) — old ToolUse rows keep
/// `tool: None` and the frontend's fallback wording handles that.
pub fn load_messages(conn: &Connection, conv_id: &str) -> rusqlite::Result<Vec<StoredMessage>> {
    let mut stmt = conn.prepare(
        "SELECT role, content, kind, tool, created_at FROM messages WHERE conv_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([conv_id], |row| {
        let role: String = row.get(0)?;
        let content: String = row.get(1)?;
        let kind: Option<String> = row.get(2)?;
        let tool: Option<String> = row.get(3)?;
        let created_at: Option<String> = row.get(4).ok();
        Ok((role, content, kind, tool, created_at))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (role, content, kind, tool, created_at) = r?;
        let kind = kind.or_else(|| infer_kind_from_role(&role));
        out.push(StoredMessage { role, content, kind, tool, created_at });
    }
    Ok(out)
}

/// Backfill heuristic for pre-Task-1 `messages` rows (whose `kind`
/// column is NULL after the migration). Maps `role` → a sensible
/// `MessageKind` name so reopening an old history still renders
/// thinking/tool blocks: assistant turns → plain text bubbles
/// (`"Text"`); user prompts + tool results → tool-result chips
/// (`"ToolResult"`); everything else (system / unknown) → `None` (the
/// frontend renders `None`-kind rows as plain text).
fn infer_kind_from_role(role: &str) -> Option<String> {
    match role {
        "assistant" => Some("Text".into()),
        "user" | "tool" => Some("ToolResult".into()),
        _ => None,
    }
}

/// Persist a whole turn: append every [`Message`](crate::agent::Message)
/// in [`TurnRecord::messages`] (mapped to a [`StoredMessage`] —
/// kind→role AND now also kind→kind name string, content verbatim), AND if
/// `record.result.session_id` is `Some`, update the conversation's
/// `session_id` so the NEXT turn resumes the agent's native session
/// (mirrors multica's `ResumeSessionID`). Called by Task 15's chat
/// controller after a turn's stream + result resolve.
///
/// **Task 1: `kind` persistence.** Each appended `StoredMessage` now
/// carries `kind = Some(message_kind_to_name(m.kind))` — the variant
/// name string (`"Text"` / `"Thinking"` / `"ToolUse"` / `"ToolResult"` /
/// `"Status"` / `"Error"` / `"Log"`). This is what lets history replay
/// render thinking / tool blocks the same way the live stream did
/// (design "改动 3"). The `role` column is STILL derived from kind via
/// `message_kind_to_role` (backward compat with the v0.1.0 role-based
/// rendering); `kind` is supplementary, not a replacement.
///
/// **Atomicity (fix-round-1, brief's SQL-hygiene "transactions for
/// multi-statement"):** this runs N message INSERTs followed by a
/// conditional `UPDATE conversations SET session_id`. All of it is
/// wrapped in ONE rusqlite `Transaction` — either every statement
/// commits together (`tx.commit()`) OR, if any statement after the
/// first fails, the whole batch rolls back (rusqlite drops the
/// `Transaction` on the early-return `?` path, which issues
/// `ROLLBACK`). Without the transaction, an autocommit-mode failure
/// mid-way would leave earlier message INSERTs committed while the
/// `session_id` UPDATE was lost — partial messages + a stale/missing
/// resume key (the bug this fix closes).
pub fn save_turn_record(
    conn: &mut Connection,
    conv_id: &str,
    record: &TurnRecord,
) -> rusqlite::Result<()> {
    // `Transaction` derefs to `&Connection` so `append_message(&tx, …)`
    // and `tx.execute(…)` work via deref coercion. The `?` below
    // propagates any error and, in doing so, DROPS `tx` — rusqlite's
    // `Drop for Transaction` issues `ROLLBACK`, so no partial commit.
    let tx = conn.transaction()?;
    // 1. Append all messages as StoredMessages (kind → role string AND
    //    kind → kind-name string, content verbatim). Task 6 fix-round-1:
    //    also carry `m.tool` so the tool name persists + history reload
    //    shows it (was dropped → history ToolUse chips read "工具"
    //    fallback with no identity).
    for m in &record.messages {
        let sm = StoredMessage {
            role: message_kind_to_role(m.kind),
            content: m.content.clone(),
            kind: Some(message_kind_to_name(m.kind)),
            tool: m.tool.clone(),
            // `created_at` is set by `append_message` internally (now()).
            created_at: None,
        };
        append_message(&tx, conv_id, &sm)?;
    }
    // 2. If the result surfaced a new agent-native session_id, update
    //    the conversation row so the next turn resumes that session.
    if let Some(result) = &record.result {
        if let Some(sid) = &result.session_id {
            tx.execute(
                "UPDATE conversations SET session_id = ?1 WHERE id = ?2",
                rusqlite::params![sid, conv_id],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Map a streamed [`MessageKind`](crate::agent::MessageKind) to its
/// variant-name string for the `messages.kind` column — the design
/// "改动 3" ruling is that `kind` stores the `MessageKind` NAME so
/// reopening history shows thinking / tool blocks. Matches the serde
/// representation (`MessageKind` serializes as the bare variant name,
/// e.g. `"ToolUse"` — see `agent::tests::message_kind_serializes_as_expected`).
/// A dedicated match (rather than `serde_json::to_string` + trim) keeps
/// this allocation-free for the hot append path and avoids a serde
/// dependency in the db layer.
fn message_kind_to_name(kind: crate::agent::MessageKind) -> String {
    use crate::agent::MessageKind::*;
    match kind {
        Text => "Text",
        Thinking => "Thinking",
        ToolUse => "ToolUse",
        ToolResult => "ToolResult",
        Status => "Status",
        Error => "Error",
        Log => "Log",
    }
    .into()
}

/// Map a streamed [`MessageKind`](crate::agent::MessageKind) to a stored
/// role string. The persisted shape ([`StoredMessage`]) keeps
/// role+content+kind — the kind tag carries the precise variant; `role`
/// is the v0.1.0-compatible coarse bucket (assistant / tool / system).
/// Mapping:
/// - `Text` / `Thinking` / `ToolUse` / `Error` → `"assistant"` (all
///   are emissions from the assistant side of the turn).
/// - `ToolResult` → `"tool"` (matches multica's tool-result role).
/// - `Status` / `Log` → `"system"` (lifecycle / debug, not a turn
///   emission).
fn message_kind_to_role(kind: crate::agent::MessageKind) -> String {
    use crate::agent::MessageKind::*;
    match kind {
        Text | Thinking | ToolUse | Error => "assistant".into(),
        ToolResult => "tool".into(),
        Status | Log => "system".into(),
    }
}

/// Parse an RFC3339 datetime string (stored by [`save_conversation`] /
/// [`append_message`]) back to a `DateTime<Utc>`. Extracted as a helper
/// so both load paths share one error mapping.
fn parse_dt(s: &str) -> rusqlite::Result<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })
}

// ===========================================================================
// Todos (Task 16 todos tab)
// ===========================================================================

/// One persisted todo row. The `status` field replaces the old boolean
/// `done` — four states: `pending`, `in_progress`, `completed`,
/// `cancelled`. `priority` is `high` | `medium` | `low`. `assignee`,
/// `start_at`, `end_at`, `notes` are all optional (NULL on disk). The
/// `done` column still exists in the table for back-compat but is no
/// longer read or written.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Todo {
    pub id: i64,
    pub text: String,
    pub status: String,
    pub priority: String,
    pub assignee: Option<String>,
    pub start_at: Option<String>,
    pub end_at: Option<String>,
    pub notes: Option<String>,
    pub created_at: String,
}

/// List all todos, oldest-first.
pub fn todo_list(conn: &Connection) -> rusqlite::Result<Vec<Todo>> {
    let mut stmt = conn.prepare(
        "SELECT id, text, status, priority, assignee, start_at, end_at, notes, created_at \
         FROM todos ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Todo {
            id: row.get(0)?,
            text: row.get(1)?,
            status: row.get(2)?,
            priority: row.get(3)?,
            assignee: row.get(4)?,
            start_at: row.get(5)?,
            end_at: row.get(6)?,
            notes: row.get(7)?,
            created_at: row.get(8)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Add a todo with full metadata. Returns the new row id.
pub fn todo_add(
    conn: &Connection,
    text: &str,
    priority: &str,
    assignee: Option<&str>,
    start_at: Option<&str>,
    end_at: Option<&str>,
    notes: Option<&str>,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO todos (text, status, priority, assignee, start_at, end_at, notes, created_at) \
         VALUES (?1, 'pending', ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            text,
            priority,
            assignee,
            start_at,
            end_at,
            notes,
            chrono::Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Update all editable fields of a todo (everything except `status` —
/// use [`todo_set_status`] for that, and `created_at` which is immutable).
pub fn todo_update(
    conn: &Connection,
    id: i64,
    text: &str,
    priority: &str,
    assignee: Option<&str>,
    start_at: Option<&str>,
    end_at: Option<&str>,
    notes: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE todos SET text = ?2, priority = ?3, assignee = ?4, start_at = ?5, end_at = ?6, notes = ?7, \
         deadline_notified = 0 WHERE id = ?1",
        rusqlite::params![id, text, priority, assignee, start_at, end_at, notes],
    )?;
    Ok(())
}

/// Set the status of a todo (pending | in_progress | completed | cancelled).
/// Replaces the old `todo_toggle` which flipped a boolean `done` flag.
pub fn todo_set_status(conn: &Connection, id: i64, status: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE todos SET status = ?2 WHERE id = ?1",
        rusqlite::params![id, status],
    )?;
    Ok(())
}

/// Delete a todo by id.
pub fn todo_delete(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM todos WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

/// Query todos whose deadline (end_at) is within 24 hours from `now`
/// but not yet past, and whose deadline_notified flag is 0.
/// Used by the scheduler to fire a "deadline approaching" reminder.
pub fn todo_get_deadline_approaching(conn: &Connection, now: &str, horizon: &str) -> rusqlite::Result<Vec<Todo>> {
    let mut stmt = conn.prepare(
        "SELECT id, text, status, priority, assignee, start_at, end_at, notes, created_at \
         FROM todos \
         WHERE status IN ('pending', 'in_progress') \
         AND end_at IS NOT NULL \
         AND end_at > ?1 \
         AND end_at <= ?2 \
         AND deadline_notified = 0",
    )?;
    let rows = stmt.query_map([now, horizon], |row| {
        Ok(Todo {
            id: row.get(0)?,
            text: row.get(1)?,
            status: row.get(2)?,
            priority: row.get(3)?,
            assignee: row.get(4)?,
            start_at: row.get(5)?,
            end_at: row.get(6)?,
            notes: row.get(7)?,
            created_at: row.get(8)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Mark a todo's deadline notification as fired (so it doesn't repeat).
pub fn todo_set_deadline_notified(conn: &Connection, id: i64, notified: bool) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE todos SET deadline_notified = ?2 WHERE id = ?1",
        rusqlite::params![id, notified as i64],
    )?;
    Ok(())
}

// ===========================================================================
// Reminders (v2: status, recurrence, snooze, next-fire)
// ===========================================================================

/// One persisted reminder row. `status` is pending(待触发) / active(已触发
/// 待处理) / done(已完成). `reminder_type` is once(一次性) / recurring(周期性).
/// For recurring, `interval_value` + `interval_unit` define the cycle.
/// `next_fire_at` is the next due time (RFC3339); null = no more fires.
/// `snoozed_until` is the snooze expiry (RFC3339); null = not snoozed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Reminder {
    pub id: i64,
    pub text: String,
    pub status: String,
    pub reminder_type: String,
    pub interval_value: i64,
    pub interval_unit: String,
    pub next_fire_at: Option<String>,
    pub snoozed_until: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// List all reminders, soonest next-fire first (nulls last).
pub fn reminder_list(conn: &Connection) -> rusqlite::Result<Vec<Reminder>> {
    let mut stmt = conn.prepare(
        "SELECT id, text, status, reminder_type, interval_value, interval_unit, \
         next_fire_at, snoozed_until, created_at, updated_at \
         FROM reminders ORDER BY \
         CASE WHEN next_fire_at IS NULL THEN 1 ELSE 0 END, next_fire_at",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Reminder {
            id: row.get(0)?,
            text: row.get(1)?,
            status: row.get(2)?,
            reminder_type: row.get(3)?,
            interval_value: row.get(4)?,
            interval_unit: row.get(5)?,
            next_fire_at: row.get(6)?,
            snoozed_until: row.get(7)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Add a reminder with full metadata. Returns the new row id.
/// `next_fire_at` is the first fire time (RFC3339).
pub fn reminder_add(
    conn: &Connection,
    text: &str,
    reminder_type: &str,
    interval_value: i64,
    interval_unit: &str,
    next_fire_at: Option<&str>,
) -> rusqlite::Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO reminders \
         (text, due_at, fired, status, reminder_type, interval_value, interval_unit, \
         next_fire_at, snoozed_until, created_at, updated_at) \
         VALUES (?1, ?2, 0, 'pending', ?3, ?4, ?5, ?6, NULL, ?7, ?7)",
        rusqlite::params![text, next_fire_at.unwrap_or(""), reminder_type, interval_value, interval_unit, next_fire_at, now],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Update all editable fields of a reminder.
pub fn reminder_update(
    conn: &Connection,
    id: i64,
    text: &str,
    reminder_type: &str,
    interval_value: i64,
    interval_unit: &str,
    next_fire_at: Option<&str>,
) -> rusqlite::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE reminders SET text = ?2, reminder_type = ?3, interval_value = ?4, \
         interval_unit = ?5, next_fire_at = ?6, updated_at = ?7 WHERE id = ?1",
        rusqlite::params![id, text, reminder_type, interval_value, interval_unit, next_fire_at, now],
    )?;
    Ok(())
}

/// Set the status of a reminder (pending / active / done).
pub fn reminder_set_status(conn: &Connection, id: i64, status: &str) -> rusqlite::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE reminders SET status = ?2, updated_at = ?3 WHERE id = ?1",
        rusqlite::params![id, status, now],
    )?;
    Ok(())
}

/// Snooze a reminder until `snoozed_until` (RFC3339). Clears any previous
/// snooze and sets status back to pending.
pub fn reminder_snooze(conn: &Connection, id: i64, snoozed_until: &str) -> rusqlite::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE reminders SET snoozed_until = ?2, status = 'pending', updated_at = ?3 WHERE id = ?1",
        rusqlite::params![id, snoozed_until, now],
    )?;
    Ok(())
}

/// Delete a reminder by id.
pub fn reminder_delete(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM reminders WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

/// Query reminders that are due now: status != 'done' AND
/// (next_fire_at <= now AND (snoozed_until IS NULL OR snoozed_until <= now)).
/// Returns the due reminders for the scheduler to fire.
pub fn reminder_get_due(conn: &Connection, now: &str) -> rusqlite::Result<Vec<Reminder>> {
    let mut stmt = conn.prepare(
        "SELECT id, text, status, reminder_type, interval_value, interval_unit, \
         next_fire_at, snoozed_until, created_at, updated_at \
         FROM reminders \
         WHERE status != 'done' \
         AND next_fire_at IS NOT NULL \
         AND next_fire_at <= ?1 \
         AND (snoozed_until IS NULL OR snoozed_until <= ?1)",
    )?;
    let rows = stmt.query_map([now], |row| {
        Ok(Reminder {
            id: row.get(0)?,
            text: row.get(1)?,
            status: row.get(2)?,
            reminder_type: row.get(3)?,
            interval_value: row.get(4)?,
            interval_unit: row.get(5)?,
            next_fire_at: row.get(6)?,
            snoozed_until: row.get(7)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Update the next_fire_at for a reminder (used by the scheduler after
/// firing a recurring reminder to schedule the next cycle).
pub fn reminder_set_next_fire(conn: &Connection, id: i64, next_fire_at: Option<&str>) -> rusqlite::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE reminders SET next_fire_at = ?2, updated_at = ?3 WHERE id = ?1",
        rusqlite::params![id, next_fire_at, now],
    )?;
    Ok(())
}

/// Clear the snooze on a reminder (set snoozed_until = NULL).
pub fn reminder_clear_snooze(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE reminders SET snoozed_until = NULL WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

// ===========================================================================
// KPI management (team KPI tracking)
// ===========================================================================

/// One KPI item row. `status` is not_started(未进行) / in_progress(进行中)
/// / completed(已完成). `year` is the target year (e.g. 2026). `kpi_type`
/// is a label from the comma-separated `kpi_types` config. `planned_at`
/// is an RFC3339 date (planned completion time, day-granularity). `details`
/// is an optional longer description.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Kpi {
    pub id: i64,
    pub year: i64,
    pub kpi_type: String,
    pub title: String,
    pub details: Option<String>,
    pub planned_at: Option<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

/// One per-year-per-type KPI target. The `target_count` is the annual goal
/// number of completed KPIs for this type+year. Progress is computed as
/// completed_count / target_count (can exceed 100%). UNIQUE(year, kpi_type)
/// ensures one target per pair; `kpi_target_upsert` uses INSERT OR REPLACE.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KpiTarget {
    pub id: i64,
    pub year: i64,
    pub kpi_type: String,
    pub target_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// List all KPI items, newest-year-first, then planned_at ascending
/// (nulls last), then by id.
pub fn kpi_list(conn: &Connection) -> rusqlite::Result<Vec<Kpi>> {
    let mut stmt = conn.prepare(
        "SELECT id, year, kpi_type, title, details, planned_at, status, created_at, updated_at \
         FROM kpis ORDER BY year DESC, COALESCE(planned_at, '9999') ASC, id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Kpi {
            id: row.get(0)?,
            year: row.get(1)?,
            kpi_type: row.get(2)?,
            title: row.get(3)?,
            details: row.get(4)?,
            planned_at: row.get(5)?,
            status: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Add a KPI item. Returns the new row id. Status defaults to
/// 'not_started' in the DB schema.
pub fn kpi_add(
    conn: &Connection,
    year: i64,
    kpi_type: &str,
    title: &str,
    details: Option<&str>,
    planned_at: Option<&str>,
) -> rusqlite::Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO kpis (year, kpi_type, title, details, planned_at, status, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'not_started', ?6, ?6)",
        rusqlite::params![year, kpi_type, title, details, planned_at, now],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Update all editable fields of a KPI (everything except `status` —
/// use [`kpi_set_status`] for that, and `created_at` which is immutable).
pub fn kpi_update(
    conn: &Connection,
    id: i64,
    year: i64,
    kpi_type: &str,
    title: &str,
    details: Option<&str>,
    planned_at: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE kpis SET year = ?2, kpi_type = ?3, title = ?4, details = ?5, planned_at = ?6, \
         updated_at = ?7 WHERE id = ?1",
        rusqlite::params![id, year, kpi_type, title, details, planned_at, chrono::Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

/// Set the status of a KPI (not_started | in_progress | completed).
pub fn kpi_set_status(conn: &Connection, id: i64, status: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE kpis SET status = ?2, updated_at = ?3 WHERE id = ?1",
        rusqlite::params![id, status, chrono::Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

/// Delete a KPI by id.
pub fn kpi_delete(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM kpis WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

/// List all KPI targets for a given year.
pub fn kpi_target_list(conn: &Connection, year: i64) -> rusqlite::Result<Vec<KpiTarget>> {
    let mut stmt = conn.prepare(
        "SELECT id, year, kpi_type, target_count, created_at, updated_at \
         FROM kpi_targets WHERE year = ?1 ORDER BY kpi_type ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![year], |row| {
        Ok(KpiTarget {
            id: row.get(0)?,
            year: row.get(1)?,
            kpi_type: row.get(2)?,
            target_count: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Upsert a KPI target for a given year+type. Uses INSERT OR REPLACE
/// so the UNIQUE(year, kpi_type) constraint replaces an existing row.
pub fn kpi_target_upsert(
    conn: &Connection,
    year: i64,
    kpi_type: &str,
    target_count: i64,
) -> rusqlite::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR REPLACE INTO kpi_targets (year, kpi_type, target_count, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?4)",
        rusqlite::params![year, kpi_type, target_count, now],
    )?;
    Ok(())
}

// ===========================================================================
// Pet state log (Task 7/16)
// ===========================================================================

/// Append a row to `pet_state_log` recording that the pet entered
/// `state` at the current UTC time. Used by Task 16 to audit state
/// transitions over time (and potentially by Task 7's brain for
/// debug). Append-only — no `load_*` helper yet.
pub fn log_pet_state(conn: &Connection, state: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO pet_state_log (state, at) VALUES (?1, ?2)",
        rusqlite::params![state, chrono::Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

// ===========================================================================
// Agents + Skills + AgentSkills (agent management feature)
// ===========================================================================

/// A saved agent profile. `provider` is one of the SUPPORTED whitelist
/// (claude/codex/opencode). `model` is `None` = use the provider's default.
/// `system_prompt` is the agent's persona/role definition, threaded into
/// `ExecOptions.system_prompt` on each turn. `mcp_config` is a JSON string
/// (the `mcpServers` object). `cwd` is `None` = use the global working dir.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub provider: String,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub mcp_config: Option<String>,
    pub cwd: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A reusable skill (SKILL.md body + metadata). Skills are assigned to
/// agents via the `agent_skills` junction table; at execution time their
/// content is concatenated into the agent's effective system prompt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub content: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A supporting script file belonging to a skill (`skill_files` table).
/// `path` is a slash-delimited relative path that may include subdirs
/// (e.g. `scripts/analyze.py`); directories are inferred from paths, never
/// stored as rows. `SKILL.md` is reserved (the skill's primary body lives
/// in `skills.content`) — a file whose path cleans to `SKILL.md` is
/// refused by `skill_file_replace_all`. Text-only (binary files rejected
/// by extension + content check, capped at 1 MiB/file).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillFile {
    pub id: String,
    pub skill_id: String,
    pub path: String,
    pub content: String,
    pub created_at: String,
    pub updated_at: String,
}

/// A skill assignment row — links an agent to a skill with an enabled flag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentSkill {
    pub agent_id: String,
    pub skill_id: String,
    pub enabled: bool,
}

/// List all agents, most-recently-updated first.
pub fn agent_list(conn: &Connection) -> rusqlite::Result<Vec<Agent>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, description, provider, model, system_prompt, \
         mcp_config, cwd, created_at, updated_at \
         FROM agents ORDER BY updated_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Agent {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            provider: row.get(3)?,
            model: row.get(4)?,
            system_prompt: row.get(5)?,
            mcp_config: row.get(6)?,
            cwd: row.get(7)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Load one agent by id, or `None` if no such row.
pub fn agent_get(conn: &Connection, id: &str) -> rusqlite::Result<Option<Agent>> {
    let row = conn.query_row(
        "SELECT id, name, description, provider, model, system_prompt, \
         mcp_config, cwd, created_at, updated_at \
         FROM agents WHERE id = ?1",
        [id],
        |row| {
            Ok(Agent {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                provider: row.get(3)?,
                model: row.get(4)?,
                system_prompt: row.get(5)?,
                mcp_config: row.get(6)?,
                cwd: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
            })
        },
    );
    match row {
        Ok(a) => Ok(Some(a)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Create a new agent. Returns the new uuid id.
pub fn agent_create(
    conn: &Connection,
    name: &str,
    description: Option<&str>,
    provider: &str,
    model: Option<&str>,
    system_prompt: Option<&str>,
    mcp_config: Option<&str>,
    cwd: Option<&str>,
) -> rusqlite::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO agents (id, name, description, provider, model, \
         system_prompt, mcp_config, cwd, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![id, name, description, provider, model, system_prompt, mcp_config, cwd, now, now],
    )?;
    Ok(id)
}

/// Update all fields of an agent (except id, created_at).
pub fn agent_update(
    conn: &Connection,
    id: &str,
    name: &str,
    description: Option<&str>,
    provider: &str,
    model: Option<&str>,
    system_prompt: Option<&str>,
    mcp_config: Option<&str>,
    cwd: Option<&str>,
) -> rusqlite::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE agents SET name = ?2, description = ?3, provider = ?4, \
         model = ?5, system_prompt = ?6, mcp_config = ?7, cwd = ?8, \
         updated_at = ?9 WHERE id = ?1",
        rusqlite::params![id, name, description, provider, model, system_prompt, mcp_config, cwd, now],
    )?;
    Ok(())
}

/// Delete an agent by id. The `agent_skills` junction rows are deleted
/// automatically via the ON DELETE CASCADE foreign key.
pub fn agent_delete(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM agents WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

/// List all skills, most-recently-updated first.
pub fn skill_list(conn: &Connection) -> rusqlite::Result<Vec<Skill>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, description, content, created_at, updated_at \
         FROM skills ORDER BY updated_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Skill {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            content: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Create a new skill. Returns the new uuid id.
pub fn skill_create(
    conn: &Connection,
    name: &str,
    description: Option<&str>,
    content: Option<&str>,
) -> rusqlite::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO skills (id, name, description, content, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![id, name, description, content, now, now],
    )?;
    Ok(id)
}

/// Update all fields of a skill (except id, created_at).
pub fn skill_update(
    conn: &Connection,
    id: &str,
    name: &str,
    description: Option<&str>,
    content: Option<&str>,
) -> rusqlite::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE skills SET name = ?2, description = ?3, content = ?4, updated_at = ?5 \
         WHERE id = ?1",
        rusqlite::params![id, name, description, content, now],
    )?;
    Ok(())
}

/// Delete a skill by id. The `agent_skills` junction rows + `skill_files`
/// rows are deleted automatically via the ON DELETE CASCADE foreign key
/// (requires `PRAGMA foreign_keys = ON`, set in `open()`).
pub fn skill_delete(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM skills WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

/// List all supporting files for a skill, ordered by path ASC (so the
/// file tree renders stably). Called by the skill-form UI on edit + by
/// the chat-turn materializer.
pub fn skill_file_list(conn: &Connection, skill_id: &str) -> rusqlite::Result<Vec<SkillFile>> {
    let mut stmt = conn.prepare(
        "SELECT id, skill_id, path, content, created_at, updated_at \
         FROM skill_files WHERE skill_id = ?1 ORDER BY path ASC",
    )?;
    let rows = stmt.query_map([skill_id], |row| {
        Ok(SkillFile {
            id: row.get(0)?,
            skill_id: row.get(1)?,
            path: row.get(2)?,
            content: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Replace the full file set of a skill in one transaction: delete every
/// existing `skill_files` row for `skill_id`, then insert each `(path,
/// content)` from `files`. Mirrors multica's full-replace-on-save
/// semantics (simpler than per-file upsert/delete).
///
/// Validation (delegated to `crate::skill::`):
/// - `check_caps` — file count (256), single-file size (1 MiB), total (8 MiB).
/// - per non-reserved file: `validate_file_path` (no absolute/rooted/`..`/
///   reserved `SKILL.md`) + `is_likely_binary` (extension blacklist + NUL).
/// - a root-level `SKILL.md` path is silently skipped (the body lives in
///   `skills.content`, not as a file row).
///
/// Returns `Ok(())` on success. On any validation failure the
/// transaction rolls back (the `Transaction` guard drops on `?`).
pub fn skill_file_replace_all(
    conn: &mut Connection,
    skill_id: &str,
    files: &[(String, String)],
) -> rusqlite::Result<()> {
    // Pre-validate before opening the transaction so a rejection doesn't
    // leave the connection in a begun-transaction state.
    crate::skill::check_caps(files)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
    for (path, content) in files {
        if crate::skill::is_reserved_skill_md_path(path) {
            continue; // silently skip — SKILL.md is the content column
        }
        crate::skill::validate_file_path(path)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        if crate::skill::is_likely_binary(path, content.as_bytes()) {
            return Err(rusqlite::Error::ToSqlConversionFailure(
                format!("binary file not allowed: {path}").into(),
            ));
        }
    }
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM skill_files WHERE skill_id = ?1",
        rusqlite::params![skill_id],
    )?;
    let now = chrono::Utc::now().to_rfc3339();
    for (path, content) in files {
        if crate::skill::is_reserved_skill_md_path(path) {
            continue;
        }
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO skill_files (id, skill_id, path, content, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, skill_id, path, content, now, now],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// List all skills assigned to an agent, with their enabled flag.
/// Returns `(Skill, enabled: bool)` tuples.
pub fn agent_skills_list(conn: &Connection, agent_id: &str) -> rusqlite::Result<Vec<(Skill, bool)>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.name, s.description, s.content, s.created_at, s.updated_at, \
         a.enabled FROM skills s \
         JOIN agent_skills a ON a.skill_id = s.id \
         WHERE a.agent_id = ?1 ORDER BY s.name",
    )?;
    let rows = stmt.query_map([agent_id], |row| {
        Ok((
            Skill {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                content: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            },
            row.get::<_, i64>(6)? != 0,
        ))
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Assign a skill to an agent (enabled by default). Idempotent — if the
/// assignment already exists, this is a no-op.
pub fn agent_skill_add(conn: &Connection, agent_id: &str, skill_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO agent_skills (agent_id, skill_id, enabled) VALUES (?1, ?2, 1)",
        rusqlite::params![agent_id, skill_id],
    )?;
    Ok(())
}

/// Remove a skill assignment from an agent.
pub fn agent_skill_remove(conn: &Connection, agent_id: &str, skill_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM agent_skills WHERE agent_id = ?1 AND skill_id = ?2",
        rusqlite::params![agent_id, skill_id],
    )?;
    Ok(())
}

/// Toggle the enabled flag on an agent-skill assignment.
pub fn agent_skill_set_enabled(
    conn: &Connection,
    agent_id: &str,
    skill_id: &str,
    enabled: bool,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE agent_skills SET enabled = ?3 WHERE agent_id = ?1 AND skill_id = ?2",
        rusqlite::params![agent_id, skill_id, enabled as i64],
    )?;
    Ok(())
}

/// Load all enabled skills for an agent, ordered by name. Used by
/// `chat_send` to build the effective system prompt (agent's system_prompt
/// + concatenated skill content).
pub fn agent_enabled_skills(conn: &Connection, agent_id: &str) -> rusqlite::Result<Vec<Skill>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.name, s.description, s.content, s.created_at, s.updated_at \
         FROM skills s \
         JOIN agent_skills a ON a.skill_id = s.id \
         WHERE a.agent_id = ?1 AND a.enabled = 1 ORDER BY s.name",
    )?;
    let rows = stmt.query_map([agent_id], |row| {
        Ok(Skill {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            content: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{ExecResult, Message, MessageKind};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Per-test unique conversation id, so parallel tests in the same
    /// process don't collide on the conversations PRIMARY KEY (the data
    /// dir is per-test-via-`tempdir`, so collisions are unlikely — but
    /// being explicit avoids future flake if a test forgets its own
    /// tempdir and reuses a shared one).
    static CONV_SEQ: AtomicU64 = AtomicU64::new(0);

    fn unique_id(label: &str) -> String {
        let n = CONV_SEQ.fetch_add(1, Ordering::SeqCst);
        format!("conv-{label}-{n}")
    }

    /// Open + migrate a fresh db in a writable temp dir.
    fn fresh_db() -> (tempfile::TempDir, Connection) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _g = set_test_overrides(Some(tmp.path().to_path_buf()), None, None);
        let conn = open().expect("open");
        migrate(&conn).expect("migrate");
        (tmp, conn)
    }

    // -- open + migrate: tables exist --

    #[test]
    fn open_and_migrate_creates_all_tables() {
        let (_tmp, conn) = fresh_db();
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .expect("prepare");
        let tables: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect();
        for t in ["conversations", "messages", "pet_state_log", "reminders", "todos"] {
            assert!(
                tables.contains(&t.to_string()),
                "missing table {t}: got {tables:?}"
            );
        }
        // The messages index also exists.
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='index' AND name='idx_messages_conv'")
            .expect("prepare");
        let idx: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect();
        assert!(!idx.is_empty(), "missing idx_messages_conv");
    }

    // -- conversation round-trip --

    #[test]
    fn conversation_round_trip() {
        let (_tmp, conn) = fresh_db();
        let conv = Conversation {
            id: unique_id("rt"),
            provider: "claude".into(),
            session_id: None,
            title: "Round trip".into(),
            created_at: chrono::Utc::now(),
            model: None,
            cwd: ".".into(),
            agent_id: None,
        };
        save_conversation(&conn, &conv).expect("save");
        let loaded = load_conversation(&conn, &conv.id).expect("load");
        assert!(loaded.is_some(), "conversation should load after save");
        let loaded = loaded.unwrap();
        assert_eq!(loaded.id, conv.id);
        assert_eq!(loaded.provider, conv.provider);
        assert!(loaded.session_id.is_none());
        assert_eq!(loaded.title, conv.title);
        // RFC3339 round-trips with nanosecond precision.
        assert_eq!(loaded.created_at, conv.created_at);
        // list_conversations returns at least this one.
        let list = list_conversations(&conn).expect("list");
        assert!(list.iter().any(|c| c.id == conv.id), "list must contain c");
    }

    // -- messages round-trip --

    #[test]
    fn append_and_load_messages_round_trip() {
        let (_tmp, conn) = fresh_db();
        let conv = Conversation {
            id: unique_id("msg"),
            provider: "claude".into(),
            session_id: None,
            title: "Msgs".into(),
            created_at: chrono::Utc::now(),
            model: None,
            cwd: ".".into(),
            agent_id: None,
        };
        save_conversation(&conn, &conv).expect("save");
        let msgs = vec![
            StoredMessage { role: "user".into(), content: "hello".into(), kind: None, tool: None, created_at: None },
            StoredMessage { role: "assistant".into(), content: "hi".into(), kind: None, tool: None, created_at: None },
            StoredMessage { role: "tool".into(), content: "{\"result\":42}".into(), kind: None, tool: None, created_at: None },
        ];
        for m in &msgs {
            append_message(&conn, &conv.id, m).expect("append");
        }
        let loaded = load_messages(&conn, &conv.id).expect("load");
        assert_eq!(loaded.len(), msgs.len());
        for (i, m) in msgs.iter().enumerate() {
            assert_eq!(loaded[i].role, m.role, "role mismatch at {i}");
            assert_eq!(loaded[i].content, m.content, "content mismatch at {i}");
        }
        // Empty for an unknown conv_id.
        let none = load_messages(&conn, "nope").expect("load none");
        assert!(none.is_empty());
    }

    // -- save_turn_record: appends messages + updates session_id --

    #[test]
    fn save_turn_record_appends_messages_and_updates_session_id() {
        let (_tmp, mut conn) = fresh_db();
        let conv = Conversation {
            id: unique_id("tr"),
            provider: "claude".into(),
            session_id: None, // first turn — no native session yet
            title: "Turn".into(),
            created_at: chrono::Utc::now(),
            model: None,
            cwd: ".".into(),
            agent_id: None,
        };
        save_conversation(&conn, &conv).expect("save");
        // TurnRecord with two messages + a result that surfaces a new
        // agent-native session_id (Task 13's run_turn produces this).
        let record = TurnRecord {
            messages: vec![
                Message {
                    kind: MessageKind::Text,
                    content: "user prompt".into(),
                    tool: None,
                    call_id: None,
                },
                Message {
                    kind: MessageKind::ToolUse,
                    content: "ls".into(),
                    tool: Some("Bash".into()),
                    call_id: Some("c1".into()),
                },
                Message {
                    kind: MessageKind::ToolResult,
                    content: "file.txt".into(),
                    tool: Some("Bash".into()),
                    call_id: Some("c1".into()),
                },
                Message {
                    kind: MessageKind::Text,
                    content: "assistant reply".into(),
                    tool: None,
                    call_id: None,
                },
            ],
            result: Some(ExecResult {
                status: "completed".into(),
                output: "done".into(),
                session_id: Some("native-sess-42".into()),
                usage: HashMap::new(),
            }),
        };
        save_turn_record(&mut conn, &conv.id, &record).expect("save_turn_record");

        // The conversation's session_id is now the agent-native id →
        // the NEXT turn will resume (Task 13's resume_or_replay sees
        // session_id.is_some()).
        let loaded = load_conversation(&conn, &conv.id).expect("load").expect("present");
        assert_eq!(
            loaded.session_id.as_deref(),
            Some("native-sess-42"),
            "session_id must be updated from result.session_id"
        );

        // All four messages were appended as StoredMessages (in order).
        let msgs = load_messages(&conn, &conv.id).expect("load");
        assert_eq!(msgs.len(), 4, "all turn messages persisted");
        assert_eq!(msgs[0].content, "user prompt");
        // ToolUse maps to "assistant" (per message_kind_to_role).
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].content, "ls");
        // ToolResult maps to "tool".
        assert_eq!(msgs[2].role, "tool");
        assert_eq!(msgs[2].content, "file.txt");
        assert_eq!(msgs[3].content, "assistant reply");
    }

    /// `save_turn_record` with a result that has NO session_id (e.g.
    /// the backend dropped its sender without sending — Task 13
    /// synthesizes a "failed" result with `session_id: None`) must NOT
    /// clear an existing session_id.
    #[test]
    fn save_turn_record_with_none_session_id_preserves_existing() {
        let (_tmp, mut conn) = fresh_db();
        let conv = Conversation {
            id: unique_id("preserved"),
            provider: "claude".into(),
            session_id: Some("existing-sess".into()),
            title: "Preserved".into(),
            created_at: chrono::Utc::now(),
            model: None,
            cwd: ".".into(),
            agent_id: None,
        };
        save_conversation(&conn, &conv).expect("save");
        let record = TurnRecord {
            messages: vec![Message {
                kind: MessageKind::Text,
                content: "again".into(),
                tool: None,
                call_id: None,
            }],
            result: Some(ExecResult {
                status: "failed".into(),
                output: String::new(),
                session_id: None, // no session to resume
                usage: HashMap::new(),
            }),
        };
        save_turn_record(&mut conn, &conv.id, &record).expect("save");
        let loaded = load_conversation(&conn, &conv.id).expect("load").expect("present");
        assert_eq!(
            loaded.session_id.as_deref(),
            Some("existing-sess"),
            "None session_id must NOT clear the existing one"
        );
    }

    /// **Atomicity (fix-round-1)** — `save_turn_record` runs N message
    /// INSERTs followed by a conditional `UPDATE conversations SET
    /// session_id`. All of that MUST be one transaction: if the UPDATE
    /// fails mid-way, the earlier message INSERTs must roll back (no
    /// partial commit) AND the conversation's `session_id` must stay
    /// where it was. Without the transaction (autocommit mode), the
    /// message INSERTs would be committed and the lost UPDATE would
    /// leave a partial-messages + stale-resume-key state — exactly the
    /// bug this test pins.
    ///
    /// Failure is injected by rebuilding `conversations` with a CHECK
    /// constraint that rejects a poison `session_id` value. The turn's
    /// `result.session_id` is set to that poison value: the 2 message
    /// INSERTs succeed, then the UPDATE violates the CHECK and errors
    /// out. The transaction rolls back, so `load_messages` returns
    /// empty AND the conversation's `session_id` is unchanged.
    #[test]
    fn save_turn_record_atomicity() {
        let (_tmp, mut conn) = fresh_db();
        // Rebuild `conversations` with a CHECK that rejects the poison
        // session_id. The CHECK turns the UPDATE into the failing
        // statement that the transaction must roll back from. Includes
        // the Task 1 `model` / `cwd` columns so `save_conversation`'s
        // INSERT (which now writes them) doesn't itself fail before the
        // poison UPDATE ever runs (which would make the test pass for
        // the wrong reason).
        conn.execute_batch(
            "DROP TABLE conversations;
             CREATE TABLE conversations (
                id TEXT PRIMARY KEY,
                provider TEXT NOT NULL,
                session_id TEXT CHECK (session_id IS NULL OR session_id != '__POISON_SESS__'),
                title TEXT NOT NULL,
                created_at TEXT NOT NULL,
                model TEXT,
                cwd TEXT NOT NULL DEFAULT '.',
                agent_id TEXT
             );",
        )
        .expect("rebuild conversations with CHECK");

        let conv = Conversation {
            id: unique_id("atom"),
            provider: "claude".into(),
            session_id: Some("original-sess".into()), // a valid (non-poison) value
            title: "Atomicity".into(),
            created_at: chrono::Utc::now(),
            model: None,
            cwd: ".".into(),
            agent_id: None,
        };
        save_conversation(&conn, &conv).expect("save conversation with valid session_id");

        // TurnRecord: 2 messages + a result whose session_id is the
        // poison value. The loop appends both messages (both INSERTs
        // succeed), then the `UPDATE conversations SET session_id`
        // fails the CHECK → `save_turn_record` returns Err.
        let record = TurnRecord {
            messages: vec![
                Message {
                    kind: MessageKind::Text,
                    content: "first".into(),
                    tool: None,
                    call_id: None,
                },
                Message {
                    kind: MessageKind::Text,
                    content: "second".into(),
                    tool: None,
                    call_id: None,
                },
            ],
            result: Some(ExecResult {
                status: "completed".into(),
                output: "done".into(),
                session_id: Some("__POISON_SESS__".into()),
                usage: HashMap::new(),
            }),
        };
        let err = save_turn_record(&mut conn, &conv.id, &record).expect_err(
            "UPDATE must fail the CHECK constraint, surfacing an error from save_turn_record",
        );
        // Sanity: the error is indeed a CHECK constraint violation, not
        // some unrelated failure that would make the test pass for the
        // wrong reason.
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("check")
                || msg.to_lowercase().contains("constraint"),
            "expected a CHECK-constraint failure, got: {msg}"
        );

        // (a) The transaction rolled back the 2 message INSERTs —
        // without the transaction they would have been committed in
        // autocommit mode.
        let msgs = load_messages(&conn, &conv.id).expect("load messages");
        assert!(
            msgs.is_empty(),
            "transaction must roll back the message INSERTs on UPDATE failure; got {msgs:?}"
        );

        // (b) The conversation's session_id is unchanged — the failed
        // UPDATE was never persisted (rolled back, AND it errored out).
        let loaded = load_conversation(&conn, &conv.id)
            .expect("load conversation")
            .expect("conversation still present");
        assert_eq!(
            loaded.session_id.as_deref(),
            Some("original-sess"),
            "session_id must be unchanged after the transaction rolled back"
        );
    }

    // -- todos CRUD --

    #[test]
    fn todo_crud_round_trip() {
        let (_tmp, conn) = fresh_db();
        assert!(todo_list(&conn).expect("list").is_empty());

        // Add two todos with full metadata.
        let id1 = todo_add(
            &conn,
            "buy milk",
            "high",
            Some("alice"),
            None,
            Some("2026-10-22T18:00:00Z"),
            Some("2L organic"),
        )
        .expect("add1");
        let id2 = todo_add(
            &conn,
            "walk dog",
            "low",
            None,
            None,
            None,
            None,
        )
        .expect("add2");
        assert!(id1 < id2, "ids must increase");

        let list = todo_list(&conn).expect("list");
        assert_eq!(list.len(), 2);

        // Defaults: pending status, priority + assignee + times + notes
        // round-trip through SELECT.
        let t1 = &list[0];
        assert_eq!(t1.id, id1);
        assert_eq!(t1.text, "buy milk");
        assert_eq!(t1.status, "pending");
        assert_eq!(t1.priority, "high");
        assert_eq!(t1.assignee.as_deref(), Some("alice"));
        assert!(t1.start_at.is_none());
        assert_eq!(t1.end_at.as_deref(), Some("2026-10-22T18:00:00Z"));
        assert_eq!(t1.notes.as_deref(), Some("2L organic"));

        let t2 = &list[1];
        assert_eq!(t2.priority, "low");
        assert!(t2.assignee.is_none());
        assert!(t2.notes.is_none());

        // Set status: pending → in_progress → completed.
        todo_set_status(&conn, id1, "in_progress").expect("set in_progress");
        let list = todo_list(&conn).expect("list2");
        let first = list.iter().find(|t| t.id == id1).expect("found1");
        assert_eq!(first.status, "in_progress");

        todo_set_status(&conn, id1, "completed").expect("set completed");
        let list = todo_list(&conn).expect("list3");
        let first = list.iter().find(|t| t.id == id1).expect("found2");
        assert_eq!(first.status, "completed");

        // Update all editable fields.
        todo_update(
            &conn,
            id1,
            "buy oat milk",
            "medium",
            Some("bob"),
            Some("2026-10-21T09:00:00Z"),
            Some("2026-10-23T20:00:00Z"),
            Some("updated note"),
        )
        .expect("update");
        let list = todo_list(&conn).expect("list4");
        let first = list.iter().find(|t| t.id == id1).expect("found3");
        assert_eq!(first.text, "buy oat milk");
        assert_eq!(first.priority, "medium");
        assert_eq!(first.assignee.as_deref(), Some("bob"));
        assert_eq!(first.start_at.as_deref(), Some("2026-10-21T09:00:00Z"));
        assert_eq!(first.end_at.as_deref(), Some("2026-10-23T20:00:00Z"));
        assert_eq!(first.notes.as_deref(), Some("updated note"));
        // Status is NOT changed by todo_update.
        assert_eq!(first.status, "completed");

        // Delete the second.
        todo_delete(&conn, id2).expect("delete");
        let list = todo_list(&conn).expect("list5");
        assert_eq!(list.len(), 1);
        assert!(list.iter().all(|t| t.id != id2));
    }

    /// Migration: a pre-v2 todo row with done=1 (the old boolean) is
    /// migrated to status='completed' by the `migrate` function's
    /// `UPDATE todos SET status = 'completed' WHERE done = 1` step.
    /// A row with done=0 stays 'pending' (the column default).
    #[test]
    fn todo_migration_done_to_status() {
        let (_tmp, conn) = fresh_db();
        // fresh_db already ran migrate once. Insert a row the old way
        // (done column still exists) to simulate pre-v2 data.
        conn.execute(
            "INSERT INTO todos (text, done, created_at) VALUES (?1, 1, ?2)",
            rusqlite::params!["legacy done task", "2026-01-01T00:00:00Z"],
        )
        .expect("insert legacy done row");
        conn.execute(
            "INSERT INTO todos (text, done, created_at) VALUES (?1, 0, ?2)",
            rusqlite::params!["legacy not-done task", "2026-01-02T00:00:00Z"],
        )
        .expect("insert legacy not-done row");

        // Re-run migrate — the done→status UPDATE is idempotent.
        migrate(&conn).expect("migrate");

        let list = todo_list(&conn).expect("list");
        assert_eq!(list.len(), 2, "two rows total");
        // The done=1 row → status='completed'.
        let done_row = list
            .iter()
            .find(|t| t.text == "legacy done task")
            .expect("found done row");
        assert_eq!(done_row.status, "completed");
        // The done=0 row → status='pending' (the column default).
        let notdone_row = list
            .iter()
            .find(|t| t.text == "legacy not-done task")
            .expect("found not-done row");
        assert_eq!(notdone_row.status, "pending");
        // New columns default correctly on legacy rows.
        assert_eq!(notdone_row.priority, "medium");
        assert!(notdone_row.assignee.is_none());
        assert!(notdone_row.notes.is_none());
    }

    /// The migration adds all six new columns to the `todos` table.
    #[test]
    fn migrate_adds_todo_v2_columns() {
        let (_tmp, conn) = fresh_db();
        let cols = column_names(&conn, "todos");
        for expected in [
            "status",
            "priority",
            "assignee",
            "start_at",
            "end_at",
            "notes",
        ] {
            assert!(
                cols.contains(&expected.to_string()),
                "todos.{expected} missing: got {cols:?}"
            );
        }
        // The old `done` column is still present (not dropped).
        assert!(
            cols.contains(&"done".to_string()),
            "todos.done should still exist (kept for back-compat): got {cols:?}"
        );
    }

    // -- reminders CRUD --

    #[test]
    fn reminder_crud_round_trip() {
        let (_tmp, conn) = fresh_db();
        assert!(reminder_list(&conn).expect("list").is_empty());
        let due1 = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let due2 = (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
        let id1 = reminder_add(&conn, "take out trash", "once", 0, "minutes", Some(&due1))
            .expect("add1");
        let id2 = reminder_add(&conn, "feed cat", "recurring", 30, "minutes", Some(&due2))
            .expect("add2");
        let list = reminder_list(&conn).expect("list");
        assert_eq!(list.len(), 2);
        // Ordered by next_fire_at ascending.
        assert_eq!(list[0].id, id1);
        assert_eq!(list[1].id, id2);
        assert_eq!(list[0].status, "pending");
        assert_eq!(list[0].reminder_type, "once");
        assert_eq!(list[1].reminder_type, "recurring");
        assert_eq!(list[1].interval_value, 30);
        assert_eq!(list[1].interval_unit, "minutes");
        assert!(list[0].snoozed_until.is_none());
        // Set status.
        reminder_set_status(&conn, id1, "done").expect("set done");
        let list = reminder_list(&conn).expect("list2");
        let r1 = list.iter().find(|r| r.id == id1).expect("found1");
        assert_eq!(r1.status, "done");
        // Snooze.
        let snooze = (chrono::Utc::now() + chrono::Duration::minutes(10)).to_rfc3339();
        reminder_snooze(&conn, id2, &snooze).expect("snooze");
        let list = reminder_list(&conn).expect("list3");
        let r2 = list.iter().find(|r| r.id == id2).expect("found2");
        assert!(r2.snoozed_until.is_some());
        assert_eq!(r2.status, "pending"); // snooze resets to pending
        // Delete.
        reminder_delete(&conn, id1).expect("delete");
        let list = reminder_list(&conn).expect("list4");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id2);
    }

    #[test]
    fn reminder_get_due_finds_due_reminders() {
        let (_tmp, conn) = fresh_db();
        let now = chrono::Utc::now();
        let past = (now - chrono::Duration::hours(1)).to_rfc3339();
        let future = (now + chrono::Duration::hours(1)).to_rfc3339();
        // Due (past time, pending).
        reminder_add(&conn, "overdue task", "once", 0, "minutes", Some(&past)).expect("add1");
        // Not yet due (future time).
        reminder_add(&conn, "future task", "once", 0, "minutes", Some(&future)).expect("add2");
        // Done (should not be returned).
        let id_done = reminder_add(&conn, "done task", "once", 0, "minutes", Some(&past)).expect("add3");
        reminder_set_status(&conn, id_done, "done").expect("set done");
        // Snoozed (should not be returned — snooze is in the future).
        let id_snoozed = reminder_add(&conn, "snoozed task", "once", 0, "minutes", Some(&past)).expect("add4");
        let snooze_future = (now + chrono::Duration::minutes(30)).to_rfc3339();
        reminder_snooze(&conn, id_snoozed, &snooze_future).expect("snooze");

        let due = reminder_get_due(&conn, &now.to_rfc3339()).expect("get_due");
        assert_eq!(due.len(), 1, "only the overdue pending non-snoozed reminder is due");
        assert_eq!(due[0].text, "overdue task");
    }

    // -- pet state log --

    #[test]
    fn log_pet_state_appends() {
        let (_tmp, conn) = fresh_db();
        log_pet_state(&conn, "thinking").expect("log1");
        log_pet_state(&conn, "needs-you").expect("log2");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM pet_state_log", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 2);
    }

    // -- is_writable --

    #[test]
    fn is_writable_true_for_temp_dir() {
        let tmp = tempfile::tempdir().expect("tmp");
        assert!(is_writable(tmp.path()));
    }

    #[test]
    fn is_writable_false_for_nonexistent_path() {
        assert!(!is_writable(std::path::Path::new("/this/path/definitely/does/not/exist/xyz")));
    }

    // -- Review Focus #5: read-only exe dir → %APPDATA% fallback --

    /// Make `db::open` target a READ-ONLY exe dir → the path must
    /// resolve to the fake-appdata fallback AND a write must succeed
    /// there. This is the portable-fallback test (Review Focus #5).
    ///
    /// On Windows the readonly *bit* on a directory does NOT prevent
    /// file creation inside — but our `is_writable` treats the bit as
    /// authoritative (returns false when it's set), so the fallback
    /// engages regardless. On Unix, `set_readonly(true)` clears the
    /// owner-write bit, genuinely preventing writes.
    #[test]
    fn open_falls_back_to_appdata_when_exe_dir_readonly() {
        let exe_dir = tempfile::tempdir().expect("exe tmp");
        let appdata_dir = tempfile::tempdir().expect("appdata tmp");
        // Mark the exe dir read-only. `set_readonly(true)` on Windows
        // sets FILE_ATTRIBUTE_READONLY; on Unix it clears the owner
        // write bit.
        let mut perms = std::fs::metadata(exe_dir.path()).expect("meta").permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(exe_dir.path(), perms).expect("set_readonly");

        let _g = set_test_overrides(
            None,
            Some(exe_dir.path().to_path_buf()),
            Some(appdata_dir.path().to_path_buf()),
        );

        // open() must succeed by falling back to appdata.
        let conn = open().expect("open must fall back to appdata, not error");
        migrate(&conn).expect("migrate");

        // The db file is in the appdata fallback, NOT in the read-only exe dir.
        let db_in_appdata = appdata_dir.path().join("sss-pet.db");
        let db_in_exe = exe_dir.path().join("sss-pet.db");
        assert!(
            db_in_appdata.exists(),
            "db file should be at appdata fallback: {}",
            db_in_appdata.display()
        );
        assert!(
            !db_in_exe.exists(),
            "db file should NOT be in the read-only exe dir: {}",
            db_in_exe.display()
        );

        // A write succeeds (proves the fallback path is writable).
        let conv = Conversation {
            id: unique_id("fallback"),
            provider: "claude".into(),
            session_id: None,
            title: "Fallback".into(),
            created_at: chrono::Utc::now(),
            model: None,
            cwd: ".".into(),
            agent_id: None,
        };
        save_conversation(&conn, &conv).expect("save must succeed on the fallback path");
        let loaded = load_conversation(&conn, &conv.id).expect("load").expect("present");
        assert_eq!(loaded.id, conv.id);

        // Restore permissions so tempfile can clean up the read-only dir.
        let mut perms = std::fs::metadata(exe_dir.path()).expect("meta2").permissions();
        perms.set_readonly(false);
        let _ = std::fs::set_permissions(exe_dir.path(), perms);
    }

    /// When the exe dir IS writable, no fallback occurs — the db file
    /// lands in the exe dir (portable case, the design spec's default).
    #[test]
    fn open_uses_exe_dir_when_writable() {
        let exe_dir = tempfile::tempdir().expect("exe tmp");
        let _g = set_test_overrides(None, Some(exe_dir.path().to_path_buf()), None);
        let conn = open().expect("open");
        migrate(&conn).expect("migrate");
        let db_in_exe = exe_dir.path().join("sss-pet.db");
        assert!(db_in_exe.exists(), "db file should be in the exe dir (portable case)");
    }

    // =======================================================================
    // Task 1: schema migration — conversation model/cwd + message kind
    // (TDD: written first, RED before implementation.)
    // =======================================================================

    /// Helper: the column names of `table` via `PRAGMA table_info`. The
    /// name column is index 1 (`cid` is 0). Used by the migration tests
    /// to assert that `model` / `cwd` / `kind` are present.
    fn column_names(conn: &Connection, table: &str) -> Vec<String> {
        // PRAGMA doesn't bind the table name as a parameter in bundled
        // SQLite; the table names here are test-only string literals
        // (no injection surface).
        let sql = format!("PRAGMA table_info({table})");
        let mut stmt = conn.prepare(&sql).expect("prepare pragma");
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .expect("query");
        let mut out = Vec::new();
        for r in rows {
            out.push(r.expect("row"));
        }
        out
    }

    /// `migrate` is idempotent: running it again on an already-migrated
    /// db MUST NOT error. The Task 1 migration's `ALTER TABLE … ADD
    /// COLUMN` path checks `PRAGMA table_info` for the column first, so
    /// the second (and third) run is a no-op for already-present columns
    /// — without that guard, `ADD COLUMN` on an existing column errors
    /// with "duplicate column name".
    #[test]
    fn migrate_is_idempotent() {
        let (_tmp, conn) = fresh_db();
        // fresh_db already ran migrate once; running it again must not
        // error (this is the regression test for the idempotency guard).
        migrate(&conn).expect("migrate twice must not error");
        // A third time catches "second-add succeeds but third fails"
        // regressions on the idempotency check.
        migrate(&conn).expect("migrate thrice must not error");
    }

    /// The Task 1 migration adds three columns: `conversations.model`
    /// (TEXT NULL), `conversations.cwd` (TEXT NOT NULL DEFAULT '.'), and
    /// `messages.kind` (TEXT NULL). PRAGMA table_info must list all
    /// three after `migrate` runs.
    #[test]
    fn migrate_adds_model_cwd_kind_columns() {
        let (_tmp, conn) = fresh_db();

        let conv_cols = column_names(&conn, "conversations");
        assert!(
            conv_cols.contains(&"model".to_string()),
            "conversations.model missing: got {conv_cols:?}"
        );
        assert!(
            conv_cols.contains(&"cwd".to_string()),
            "conversations.cwd missing: got {conv_cols:?}"
        );

        let msg_cols = column_names(&conn, "messages");
        assert!(
            msg_cols.contains(&"kind".to_string()),
            "messages.kind missing: got {msg_cols:?}"
        );
    }

    /// `new_conversation(provider, model, cwd)` persists a Conversation
    /// row with the given model + cwd; `load_conversation` round-trips
    /// both fields back (model as Option, cwd as the exact string). The
    /// created row has `session_id=None`, `title="新对话"`, a uuid v4 id,
    /// and `created_at=now`.
    #[test]
    fn new_conversation_round_trips_model_and_cwd() {
        let (_tmp, conn) = fresh_db();
        let conv = new_conversation(&conn, "claude", Some("sonnet".into()), "/tmp", None)
            .expect("new_conversation");
        assert_eq!(conv.provider, "claude");
        assert_eq!(conv.model.as_deref(), Some("sonnet"));
        assert_eq!(conv.cwd, "/tmp");
        assert!(conv.session_id.is_none(), "fresh conv has no session_id");
        assert_eq!(conv.title, "新对话");
        assert!(!conv.id.is_empty(), "id must be a non-empty uuid");

        let loaded = load_conversation(&conn, &conv.id)
            .expect("load")
            .expect("conversation present after new_conversation");
        assert_eq!(loaded.id, conv.id);
        assert_eq!(loaded.model.as_deref(), Some("sonnet"), "model must round-trip");
        assert_eq!(loaded.cwd, "/tmp", "cwd must round-trip");
        assert_eq!(loaded.provider, "claude");
        assert_eq!(loaded.title, "新对话");
        assert!(loaded.session_id.is_none());
    }

    /// `new_conversation` with `model: None` round-trips as `None` —
    /// the agent will use its own default model (Task 4 wires that path
    /// through `resume_or_replay` → `ExecOptions.model`). The `cwd`
    /// default of `"."` matches the column's `DEFAULT '.'`.
    #[test]
    fn new_conversation_none_model_round_trips() {
        let (_tmp, conn) = fresh_db();
        let conv = new_conversation(&conn, "codex", None, ".", None).expect("new_conversation");
        assert!(conv.model.is_none(), "None model stored as None");
        let loaded = load_conversation(&conn, &conv.id)
            .expect("load")
            .expect("present");
        assert!(loaded.model.is_none(), "None model must round-trip as None");
        assert_eq!(loaded.cwd, ".");
    }

    /// `save_turn_record` persists each `Message`'s `MessageKind` as a
    /// `kind` string (the variant name); `load_messages` returns
    /// `StoredMessage`s whose `kind` matches the original `MessageKind`
    /// name (Text/Thinking/ToolUse/ToolResult). The `role` column is
    /// STILL derived from `MessageKind` via `message_kind_to_role`
    /// (backward compat with the v0.1.0 role-based rendering) — `kind`
    /// is ADDITIONAL, not a replacement.
    #[test]
    fn save_turn_record_persists_message_kinds() {
        let (_tmp, mut conn) = fresh_db();
        let conv = Conversation {
            id: unique_id("kind"),
            provider: "claude".into(),
            model: None,
            cwd: ".".into(),
            agent_id: None,
            session_id: None,
            title: "Kinds".into(),
            created_at: chrono::Utc::now(),
        };
        save_conversation(&conn, &conv).expect("save");

        let record = TurnRecord {
            messages: vec![
                Message {
                    kind: MessageKind::Text,
                    content: "hello".into(),
                    tool: None,
                    call_id: None,
                },
                Message {
                    kind: MessageKind::Thinking,
                    content: "hmm".into(),
                    tool: None,
                    call_id: None,
                },
                Message {
                    kind: MessageKind::ToolUse,
                    content: "ls".into(),
                    tool: Some("Bash".into()),
                    call_id: Some("c1".into()),
                },
                Message {
                    kind: MessageKind::ToolResult,
                    content: "file.txt".into(),
                    tool: Some("Bash".into()),
                    call_id: Some("c1".into()),
                },
            ],
            // No result → save_turn_record only appends messages (no
            // session_id update). The kind-persistence path is what's
            // under test here.
            result: None,
        };
        save_turn_record(&mut conn, &conv.id, &record).expect("save_turn_record");

        let msgs = load_messages(&conn, &conv.id).expect("load");
        assert_eq!(msgs.len(), 4, "all four messages persisted");
        assert_eq!(msgs[0].kind.as_deref(), Some("Text"), "first msg kind (Text)");
        assert_eq!(msgs[1].kind.as_deref(), Some("Thinking"), "second msg kind (Thinking)");
        assert_eq!(msgs[2].kind.as_deref(), Some("ToolUse"), "third msg kind (ToolUse)");
        assert_eq!(msgs[3].kind.as_deref(), Some("ToolResult"), "fourth msg kind (ToolResult)");
        // role mapping is unchanged (Text/Thinking/ToolUse→assistant,
        // ToolResult→tool) — kind is supplementary, role preserved.
        assert_eq!(msgs[0].role, "assistant");
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[2].role, "assistant");
        assert_eq!(msgs[3].role, "tool");
        // Task 6 fix-round-1: the tool NAME round-trips too — a ToolUse
        // Message with tool: Some("Bash") persists it, + load_messages
        // returns StoredMessage.tool == Some("Bash"). Without this, history
        // ToolUse chips read "使用工具 工具" (the doubled fallback). Also
        // pins that non-tool kinds carry tool: None through the round-trip.
        assert_eq!(msgs[0].tool, None, "Text msg tool must be None");
        assert_eq!(msgs[1].tool, None, "Thinking msg tool must be None");
        assert_eq!(
            msgs[2].tool.as_deref(),
            Some("Bash"),
            "ToolUse msg tool must round-trip as Some(\"Bash\") — the core fix, got: {:?}",
            msgs[2].tool,
        );
        assert_eq!(
            msgs[3].tool.as_deref(),
            Some("Bash"),
            "ToolResult msg tool must round-trip as Some(\"Bash\"), got: {:?}",
            msgs[3].tool,
        );
    }

    /// Old rows (pre-Task-1 migration, `kind IS NULL`) get a `kind`
    /// INFERRED from `role` on load: assistant→`Some("Text")`,
    /// user/tool→`Some("ToolResult")`, else `None`. This is the
    /// backfill heuristic so reopening a pre-Task-1 history still
    /// renders sensible kinds (text bubbles for assistant turns, tool-
    /// result chips for the user/tool side) instead of all-None.
    #[test]
    fn load_messages_infers_kind_for_null_rows() {
        let (_tmp, conn) = fresh_db();
        let conv = Conversation {
            id: unique_id("old"),
            provider: "claude".into(),
            model: None,
            cwd: ".".into(),
            agent_id: None,
            session_id: None,
            title: "Old".into(),
            created_at: chrono::Utc::now(),
        };
        save_conversation(&conn, &conv).expect("save");

        // Insert four rows with kind=NULL, simulating pre-Task-1 data
        // (one per role branch of the inference rule). The backfill is
        // what's under test — the rows are written directly, bypassing
        // save_turn_record (which would now set kind explicitly).
        let now = chrono::Utc::now().to_rfc3339();
        for (role, content) in [
            ("assistant", "hi"),
            ("user", "go"),
            ("tool", "{\"r\":1}"),
            ("system", "started"),
        ] {
            conn.execute(
                "INSERT INTO messages (conv_id, role, content, created_at, kind) \
                 VALUES (?1, ?2, ?3, ?4, NULL)",
                rusqlite::params![conv.id, role, content, now],
            )
            .unwrap_or_else(|e| panic!("insert {role} null-kind row: {e}"));
        }

        let msgs = load_messages(&conn, &conv.id).expect("load");
        assert_eq!(msgs.len(), 4);
        // assistant → Text
        assert_eq!(msgs[0].role, "assistant");
        assert_eq!(msgs[0].kind.as_deref(), Some("Text"), "assistant null→Text");
        // user → ToolResult
        assert_eq!(msgs[1].role, "user");
        assert_eq!(msgs[1].kind.as_deref(), Some("ToolResult"), "user null→ToolResult");
        // tool → ToolResult
        assert_eq!(msgs[2].role, "tool");
        assert_eq!(msgs[2].kind.as_deref(), Some("ToolResult"), "tool null→ToolResult");
        // system → None (no sensible inference for a system row)
        assert_eq!(msgs[3].role, "system");
        assert_eq!(msgs[3].kind, None, "system null→None");
    }

    /// `list_conversations` returns the `model` + `cwd` fields too (not
    /// just `load_conversation`) — the history sidebar labels threads by
    /// their project dir (cwd) + model, so the list path must surface
    /// both.
    #[test]
    fn list_conversations_returns_model_and_cwd() {
        let (_tmp, conn) = fresh_db();
        let conv = new_conversation(&conn, "claude", Some("sonnet".into()), "/proj", None)
            .expect("new_conversation");
        let list = list_conversations(&conn).expect("list");
        let found = list
            .iter()
            .find(|c| c.id == conv.id)
            .expect("new conv appears in list");
        assert_eq!(found.model.as_deref(), Some("sonnet"));
        assert_eq!(found.cwd, "/proj");
    }

    // -- KPI CRUD --

    #[test]
    fn kpi_add_and_list() {
        let (_tmp, conn) = fresh_db();
        let id = kpi_add(&conn, 2026, "需求开发", "用户登录", Some("实现登录页面"), Some("2026-03-01T00:00:00Z"))
            .expect("kpi_add");
        assert!(id > 0);
        let list = kpi_list(&conn).expect("kpi_list");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].year, 2026);
        assert_eq!(list[0].kpi_type, "需求开发");
        assert_eq!(list[0].title, "用户登录");
        assert_eq!(list[0].status, "not_started");
        assert_eq!(list[0].details.as_deref(), Some("实现登录页面"));
        assert_eq!(list[0].planned_at.as_deref(), Some("2026-03-01T00:00:00Z"));
    }

    #[test]
    fn kpi_update_fields() {
        let (_tmp, conn) = fresh_db();
        let id = kpi_add(&conn, 2026, "Bug修复", "修复崩溃", None, None)
            .expect("kpi_add");
        kpi_update(&conn, id, 2025, "需求开发", "修复登录崩溃", Some("详情更新"), Some("2026-01-15T00:00:00Z"))
            .expect("kpi_update");
        let list = kpi_list(&conn).expect("kpi_list");
        assert_eq!(list.len(), 1);
        let k = &list[0];
        assert_eq!(k.year, 2025);
        assert_eq!(k.kpi_type, "需求开发");
        assert_eq!(k.title, "修复登录崩溃");
        assert_eq!(k.details.as_deref(), Some("详情更新"));
        assert_eq!(k.planned_at.as_deref(), Some("2026-01-15T00:00:00Z"));
    }

    #[test]
    fn kpi_set_status_updates() {
        let (_tmp, conn) = fresh_db();
        let id = kpi_add(&conn, 2026, "技术分享", "Rust分享", None, None)
            .expect("kpi_add");
        kpi_set_status(&conn, id, "in_progress").expect("kpi_set_status");
        let list = kpi_list(&conn).expect("kpi_list");
        assert_eq!(list[0].status, "in_progress");
        kpi_set_status(&conn, id, "completed").expect("kpi_set_status");
        let list = kpi_list(&conn).expect("kpi_list");
        assert_eq!(list[0].status, "completed");
    }

    #[test]
    fn kpi_delete_removes() {
        let (_tmp, conn) = fresh_db();
        let id = kpi_add(&conn, 2026, "Bug修复", "修复崩溃", None, None)
            .expect("kpi_add");
        assert_eq!(kpi_list(&conn).expect("list").len(), 1);
        kpi_delete(&conn, id).expect("kpi_delete");
        assert_eq!(kpi_list(&conn).expect("list").len(), 0);
    }

    #[test]
    fn kpi_target_upsert_and_list() {
        let (_tmp, conn) = fresh_db();
        // Insert a new target.
        kpi_target_upsert(&conn, 2026, "需求开发", 10).expect("upsert");
        let targets = kpi_target_list(&conn, 2026).expect("target_list");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].kpi_type, "需求开发");
        assert_eq!(targets[0].target_count, 10);
        // Upsert replaces (same year+type → UPDATE via INSERT OR REPLACE).
        kpi_target_upsert(&conn, 2026, "需求开发", 15).expect("upsert2");
        let targets = kpi_target_list(&conn, 2026).expect("target_list2");
        assert_eq!(targets.len(), 1, "UNIQUE(year, kpi_type) → still 1 row");
        assert_eq!(targets[0].target_count, 15, "target_count updated");
        // A different type is a new row.
        kpi_target_upsert(&conn, 2026, "Bug修复", 20).expect("upsert3");
        let targets = kpi_target_list(&conn, 2026).expect("target_list3");
        assert_eq!(targets.len(), 2);
        // A different year is a new row.
        kpi_target_upsert(&conn, 2025, "需求开发", 5).expect("upsert4");
        assert_eq!(kpi_target_list(&conn, 2026).expect("list2026").len(), 2);
        assert_eq!(kpi_target_list(&conn, 2025).expect("list2025").len(), 1);
    }

    // -- skill_files (Task 1: script files) --

    #[test]
    fn skill_file_replace_all_inserts_lists_replaces_and_skips_skill_md() {
        let (_tmp, mut conn) = fresh_db();
        let sid = skill_create(&conn, "s1", None, Some("body")).unwrap();
        // insert
        skill_file_replace_all(&mut conn, &sid, &[
            ("scripts/a.py".into(), "print(1)".into()),
            ("docs/b.md".into(), "# B".into()),
        ])
        .unwrap();
        let listed = skill_file_list(&conn, &sid).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].path, "docs/b.md"); // ORDER BY path ASC
        // replace (delete old + write new)
        skill_file_replace_all(&mut conn, &sid, &[
            ("scripts/c.py".into(), "print(2)".into()),
        ])
        .unwrap();
        assert_eq!(skill_file_list(&conn, &sid).unwrap().len(), 1);
        // SKILL.md reserved — skipped, not stored
        skill_file_replace_all(&mut conn, &sid, &[
            ("SKILL.md".into(), "x".into()),
            ("scripts/d.py".into(), "y".into()),
        ])
        .unwrap();
        let listed = skill_file_list(&conn, &sid).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path, "scripts/d.py");
    }

    #[test]
    fn skill_file_replace_all_rejects_binary_and_oversize() {
        let (_tmp, mut conn) = fresh_db();
        let sid = skill_create(&conn, "s1", None, Some("body")).unwrap();
        // binary by extension
        let err = skill_file_replace_all(&mut conn, &sid, &[
            ("a.png".into(), "data".into()),
        ])
        .unwrap_err();
        assert!(
            err.to_string().contains("binary") || err.to_string().contains("二进制"),
            "got: {err}"
        );
        // oversize (>1 MiB)
        let big = "x".repeat(1024 * 1024 + 1);
        let err = skill_file_replace_all(&mut conn, &sid, &[("a.py".into(), big)])
            .unwrap_err();
        assert!(
            err.to_string().contains('1') || err.to_string().contains("MiB"),
            "got: {err}"
        );
    }

    #[test]
    fn skill_files_cascade_delete_with_skill() {
        let (_tmp, mut conn) = fresh_db();
        let sid = skill_create(&conn, "s1", None, Some("body")).unwrap();
        skill_file_replace_all(&mut conn, &sid, &[("a.py".into(), "x".into())]).unwrap();
        assert_eq!(skill_file_list(&conn, &sid).unwrap().len(), 1);
        skill_delete(&conn, &sid).unwrap();
        // FK ON DELETE CASCADE (foreign_keys pragma ON) → rows gone
        assert_eq!(skill_file_list(&conn, &sid).unwrap().len(), 0);
    }
}
