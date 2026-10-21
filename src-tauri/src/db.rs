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
//! ("数据与持久化" + "便携回退"). Single `swd-pet.db` SQLite file
//! via `rusqlite` (with `bundled` so no system SQLite install is needed
//! — the single `.exe` is self-contained).
//!
//! ## Path resolution (Review Focus #5 — portable + fallback)
//!
//! 1. Try `<exe_dir>/swd-pet.db` (the exe's directory via
//!    [`std::env::current_exe`]). If writable → use it (portable:
//!    data sits next to the `.exe`, matching the design spec).
//! 2. If the exe dir is not writable (Program Files on Windows is
//!    ACL-protected; `/usr` on Unix; a read-only mount) → fall back to
//!    `%APPDATA%\swd-pet\swd-pet.db` (Windows),
//!    `~/.local/share/swd-pet/swd-pet.db` (Unix),
//!    `~/Library/Application Support/swd-pet/swd-pet.db` (macOS) —
//!    via [`dirs::data_dir`]. Created if missing.
//! 3. If both are unavailable, [`open`] returns an error.
//!
//! The fallback is essential for the "single .exe in Program Files"
//! case: Windows ACL-protects Program Files so the exe dir is not
//! user-writable; without the appdata fallback, the app couldn't
//! persist anything. The portable case (exe in a user-writable dir)
//! keeps data next to the `.exe`.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::agent::conversation::{Conversation, StoredMessage, TurnRecord};

// ===========================================================================
// Path resolution (portable + %APPDATA% fallback) — Review Focus #5.
// ===========================================================================

/// Resolve the directory that should hold `swd-pet.db` AND `config.toml`
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

/// The `%APPDATA%\swd-pet`-style fallback, or a test override.
fn appdata_dir() -> Option<PathBuf> {
    #[cfg(test)]
    {
        if let Some(d) = appdata_dir_override() {
            return Some(d);
        }
    }
    Some(dirs::data_dir()?.join("swd-pet"))
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
/// 2. Create + delete a `.swd-pet-probe-<pid>-<seq>` file — catches the
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
    let probe = dir.join(format!(".swd-pet-probe-{}-{}", std::process::id(), seq));
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

/// Open a connection to `swd-pet.db`, creating the file if missing. Path
/// resolution per [`resolve_data_dir`]. The caller MUST call [`migrate`]
/// once after opening a fresh db (idempotent for existing dbs).
pub fn open() -> rusqlite::Result<Connection> {
    let dir = resolve_data_dir().ok_or_else(|| {
        rusqlite::Error::InvalidPath(PathBuf::from("<no writable data dir>"))
    })?;
    let path = dir.join("swd-pet.db");
    Connection::open(path)
}

/// Create all five tables + the messages index if they don't exist.
/// Idempotent (`CREATE TABLE IF NOT EXISTS`).
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
        );",
    )
}

// ===========================================================================
// Conversations + messages CRUD (used by Task 15 chat)
// ===========================================================================

/// Insert (or upsert on `id` conflict) a [`Conversation`] row. Round-trips
/// `created_at` as an RFC3339 string (chrono).
pub fn save_conversation(conn: &Connection, conv: &Conversation) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO conversations (id, provider, session_id, title, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
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
        ],
    )?;
    Ok(())
}

/// Load one conversation by id, or `None` if no such row.
pub fn load_conversation(conn: &Connection, id: &str) -> rusqlite::Result<Option<Conversation>> {
    let row = conn.query_row(
        "SELECT id, provider, session_id, title, created_at FROM conversations WHERE id = ?1",
        [id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        },
    );
    match row {
        Ok((id, provider, session_id, title, created_at_str)) => {
            let created_at = parse_dt(&created_at_str)?;
            Ok(Some(Conversation {
                id,
                provider,
                session_id,
                title,
                created_at,
            }))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// List all conversations, oldest-first (Task 15's history sidebar).
pub fn list_conversations(conn: &Connection) -> rusqlite::Result<Vec<Conversation>> {
    let mut stmt = conn.prepare(
        "SELECT id, provider, session_id, title, created_at FROM conversations ORDER BY created_at",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (id, provider, session_id, title, created_at_str) = r?;
        out.push(Conversation {
            id,
            provider,
            session_id,
            title,
            created_at: parse_dt(&created_at_str)?,
        });
    }
    Ok(out)
}

/// Append one stored message to a conversation. `created_at` is the
/// current UTC time (the message's row id preserves insertion order —
/// `load_messages` orders by id).
pub fn append_message(conn: &Connection, conv_id: &str, msg: &StoredMessage) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO messages (conv_id, role, content, created_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            conv_id,
            msg.role,
            msg.content,
            chrono::Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(())
}

/// Load all stored messages for a conversation, in arrival order.
pub fn load_messages(conn: &Connection, conv_id: &str) -> rusqlite::Result<Vec<StoredMessage>> {
    let mut stmt = conn.prepare(
        "SELECT role, content FROM messages WHERE conv_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([conv_id], |row| {
        Ok(StoredMessage {
            role: row.get(0)?,
            content: row.get(1)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Persist a whole turn: append every [`Message`](crate::agent::Message)
/// in [`TurnRecord::messages`] (mapped to a [`StoredMessage`] —
/// kind→role, content verbatim), AND if
/// `record.result.session_id` is `Some`, update the conversation's
/// `session_id` so the NEXT turn resumes the agent's native session
/// (mirrors multica's `ResumeSessionID`). Called by Task 15's chat
/// controller after a turn's stream + result resolve.
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
    // 1. Append all messages as StoredMessages (kind → role string).
    for m in &record.messages {
        let sm = StoredMessage {
            role: message_kind_to_role(m.kind),
            content: m.content.clone(),
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

/// Map a streamed [`MessageKind`](crate::agent::MessageKind) to a stored
/// role string. The persisted shape ([`StoredMessage`]) keeps only
/// role+content — the kind tag / tool / call_id are dropped (the design
/// spec calls this the "persisted shape, distinct from the streamed
/// Message"). Mapping:
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

/// One persisted todo row. `done` is `bool` at the Rust boundary, `INTEGER`
/// (0/1) on disk (SQLite has no native bool).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Todo {
    pub id: i64,
    pub text: String,
    pub done: bool,
    pub created_at: String,
}

/// List all todos, oldest-first.
pub fn todo_list(conn: &Connection) -> rusqlite::Result<Vec<Todo>> {
    let mut stmt =
        conn.prepare("SELECT id, text, done, created_at FROM todos ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        Ok(Todo {
            id: row.get(0)?,
            text: row.get(1)?,
            done: row.get::<_, i64>(2)? != 0,
            created_at: row.get(3)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Add a todo. Returns the new row id.
pub fn todo_add(conn: &Connection, text: &str) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO todos (text, done, created_at) VALUES (?1, 0, ?2)",
        rusqlite::params![text, chrono::Utc::now().to_rfc3339()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Flip a todo's `done` flag.
pub fn todo_toggle(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE todos SET done = CASE done WHEN 0 THEN 1 ELSE 0 END WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// Delete a todo by id.
pub fn todo_delete(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM todos WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

// ===========================================================================
// Reminders (Task 16 reminders tab)
// ===========================================================================

/// One persisted reminder row. `fired` is `bool` at the Rust boundary,
/// `INTEGER` (0/1) on disk. `due_at` is stored as an RFC3339 string.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Reminder {
    pub id: i64,
    pub text: String,
    pub due_at: String,
    pub fired: bool,
}

/// List all reminders, soonest-due-first.
pub fn reminder_list(conn: &Connection) -> rusqlite::Result<Vec<Reminder>> {
    let mut stmt =
        conn.prepare("SELECT id, text, due_at, fired FROM reminders ORDER BY due_at")?;
    let rows = stmt.query_map([], |row| {
        Ok(Reminder {
            id: row.get(0)?,
            text: row.get(1)?,
            due_at: row.get(2)?,
            fired: row.get::<_, i64>(3)? != 0,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Add a reminder. `due_at` is an RFC3339 string (caller formats — Task
/// 16 will use `chrono::Utc::now() + Duration`). Returns the new row id.
pub fn reminder_add(conn: &Connection, text: &str, due_at: &str) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO reminders (text, due_at, fired) VALUES (?1, ?2, 0)",
        rusqlite::params![text, due_at],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Delete a reminder by id. (Toggling `fired` is left to Task 16 — not
/// needed yet.)
pub fn reminder_delete(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM reminders WHERE id = ?1", rusqlite::params![id])?;
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
        };
        save_conversation(&conn, &conv).expect("save");
        let msgs = vec![
            StoredMessage { role: "user".into(), content: "hello".into() },
            StoredMessage { role: "assistant".into(), content: "hi".into() },
            StoredMessage { role: "tool".into(), content: "{\"result\":42}".into() },
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
        // statement that the transaction must roll back from.
        conn.execute_batch(
            "DROP TABLE conversations;
             CREATE TABLE conversations (
                id TEXT PRIMARY KEY,
                provider TEXT NOT NULL,
                session_id TEXT CHECK (session_id IS NULL OR session_id != '__POISON_SESS__'),
                title TEXT NOT NULL,
                created_at TEXT NOT NULL
             );",
        )
        .expect("rebuild conversations with CHECK");

        let conv = Conversation {
            id: unique_id("atom"),
            provider: "claude".into(),
            session_id: Some("original-sess".into()), // a valid (non-poison) value
            title: "Atomicity".into(),
            created_at: chrono::Utc::now(),
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
        let id1 = todo_add(&conn, "buy milk").expect("add1");
        let id2 = todo_add(&conn, "walk dog").expect("add2");
        assert!(id1 < id2, "ids must increase");
        let list = todo_list(&conn).expect("list");
        assert_eq!(list.len(), 2);
        assert!(!list[0].done);
        assert!(!list[1].done);
        // Toggle the first.
        todo_toggle(&conn, id1).expect("toggle");
        let list = todo_list(&conn).expect("list2");
        let first = list.iter().find(|t| t.id == id1).expect("found1");
        assert!(first.done, "first todo now done");
        // Toggle back.
        todo_toggle(&conn, id1).expect("toggle2");
        let list = todo_list(&conn).expect("list3");
        let first = list.iter().find(|t| t.id == id1).expect("found2");
        assert!(!first.done, "first todo back to not-done");
        // Delete the second.
        todo_delete(&conn, id2).expect("delete");
        let list = todo_list(&conn).expect("list4");
        assert_eq!(list.len(), 1);
        assert!(list.iter().all(|t| t.id != id2));
    }

    // -- reminders CRUD --

    #[test]
    fn reminder_crud_round_trip() {
        let (_tmp, conn) = fresh_db();
        assert!(reminder_list(&conn).expect("list").is_empty());
        let due1 = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let due2 = (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
        let id1 = reminder_add(&conn, "take out trash", &due1).expect("add1");
        let id2 = reminder_add(&conn, "feed cat", &due2).expect("add2");
        let list = reminder_list(&conn).expect("list");
        assert_eq!(list.len(), 2);
        // Ordered by due_at ascending.
        assert_eq!(list[0].id, id1);
        assert_eq!(list[1].id, id2);
        assert!(!list[0].fired);
        // Delete.
        reminder_delete(&conn, id1).expect("delete");
        let list = reminder_list(&conn).expect("list2");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id2);
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
        let db_in_appdata = appdata_dir.path().join("swd-pet.db");
        let db_in_exe = exe_dir.path().join("swd-pet.db");
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
        let db_in_exe = exe_dir.path().join("swd-pet.db");
        assert!(db_in_exe.exists(), "db file should be in the exe dir (portable case)");
    }
}
