mod config;
pub mod state;
mod tray;
mod window;
// Task 6: pure-Rust idle detection + follow-activity throttle. Not wired
// into the Tauri app yet — Task 7 will poll `activity::idle_since()` and
// drive `timers::FollowActivityThrottle` from the activity-poll thread.
pub mod activity;
pub mod timers;
// Task 14: portable SQLite store (conversations / messages / todos /
// reminders / pet_state_log) + the path-resolution helper the config
// module shares (exe dir → %APPDATA% fallback). `pub` so config.rs
// (private mod) can call `db::resolve_data_dir`.
pub mod db;
// Task 7: the "pet brain" — a 1Hz tick loop that feeds live idle time
// into the state machine, runs the HasSomething auto-clear timer, fires
// the follow-activity nudge via the throttle, and emits `pet_state` /
// `pet_activity_nudge` to the pet webview. Spawned from `setup` below.
mod brain;
// Task 8: agent backend skeleton — `Backend` trait, `Session`/`Message`/
// `ExecResult`/`TokenUsage` types, factory `agent::new`, and
// `agent::exec::{resolve, spawn}`. The three backend impls (claude /
// opencode / codex) are PLACEHOLDERS here — Tasks 10/11/12 replace their
// `execute` bodies with real subprocess wiring. Pure Rust, no GUI, no
// real agent calls yet.
pub mod agent;
// Task 16: Windows toast notifications (thin wrapper around
// `tauri-plugin-notification`). `pub` so the reminder scheduler's
// production spawn can call `notify::toast(&app, …)`.
pub mod notify;
// Task 16: reminder scheduler — a tokio task that every ~15s scans the
// `reminders` table for due-but-not-fired rows and fires toast +
// `Signal::ReminderDue` + `reminder_mark_fired`. Extracted
// `run_reminder_tick` core takes injected `notify` + `apply_signal`
// closures so the unit test drives a due reminder through without
// waiting for a real tick or a real WinRT call.
pub mod reminders;

use std::sync::{Arc, Mutex};
use std::time::Instant;

use rusqlite::Connection;
use tauri::{AppHandle, Emitter, Manager, State};

use brain::{BrainState, PetBrain};
use db::Reminder;
use state::machine::Signal;
// Task 15: chat-turn controller — `run_chat_turn` core + the four
// Tauri commands (`list_conversations`, `new_conversation`,
// `load_history`, `chat_send`) the panel's ChatView consumes. Owns
// the `chat_survives_hide` test (Review Focus #3 — run_chat_turn +
// FakeBackend + no-op emit).
pub mod chat;

/// Toggle click-through on the pet overlay window from the frontend.
///
/// Kept as a registered Tauri command so the frontend (or Task 3's panel)
/// can explicitly toggle click-through when needed (e.g. dragging in Task 4
/// may pin the window interactive). The day-to-day hover toggling, however,
/// is driven from Rust by [`window::spawn_click_through_poller`] because the
/// frontend cannot receive pointer events while pass-through is on.
///
/// Delegates to [`window::set_click_through`] against the `"pet"` window.
#[tauri::command]
fn set_pet_click_through(app: tauri::AppHandle, ignore: bool) -> Result<(), String> {
    window::set_click_through(&app, "pet", ignore)
}

/// Frontend hook for "the user started/stopped dragging the pet". Sets the
/// [`window::DRAGGING`] flag so the click-through poller pins `ignore=false`
/// for the duration of the drag (avoids a race where the cursor momentarily
/// leaves the (stale) robot rect between OS drag ticks and the poller would
/// otherwise flip pass-through on, breaking the drag). Called by
/// `Pet.svelte`'s `mousedown` (`true`) / `mouseup` (`false`) handler around
/// the `data-tauri-drag-region` OS drag.
#[tauri::command]
fn set_pet_dragging(dragging: bool) -> Result<(), String> {
    window::set_pet_dragging_state(dragging);
    Ok(())
}

/// Frontend hook for "open the panel next to the pet, choosing flip direction
/// based on which side of the pet has room". The entry point `Pet.svelte`'s
/// click handler invokes after a non-drag click on the robot. Delegates to
/// [`window::open_panel_above_pet`], which computes the spawn position and
/// `set_position`+`show`+`set_focus` on the panel.
#[tauri::command]
fn open_panel_above_pet(app: tauri::AppHandle) -> Result<(), String> {
    window::open_panel_above_pet(&app)
}

/// Dev-only hook (Task 7) to manually trigger a state for visual
/// verification of the `!`/`?`/thinking poses without waiting for the
/// real agent/reminder wiring (Tasks 9+/16). Accepts a `target` string
/// and applies the corresponding [`Signal`] to the [`PetBrain`]'s state
/// machine, then emits the resulting [`PetState`] immediately (so the
/// frontend updates without waiting for the next 1s tick).
///
/// Recognized `target` values:
/// - `"has-something"` → `Signal::ReminderDue` (perks up, `!`)
/// - `"needs-you"` → `Signal::Error` (`?`, sticky until dismissed)
/// - `"thinking"` → `Signal::AgentBusy` (cog spins)
/// - `"dismissed"` → `Signal::Dismissed` (clears `?`)
/// - `"panel"` → `Signal::PanelOpened` (clears `!`)
///
/// **NOT for production** — this is a verification affordance for Task 7.
/// Later tasks that wire the real signals will naturally make this
/// obsolete (and it can be feature-gated or removed then).
///
/// Task 17: gated with `#[cfg(debug_assertions)]` so the command is NOT
/// compiled into release builds. The frontend's `invoke("dev_trigger_state")`
/// would fail in release — that's expected (dev-only). The registration
/// in `invoke_handler` below is also gated, so the handler list in
/// release builds simply omits this entry.
#[cfg(debug_assertions)]
#[tauri::command]
fn dev_trigger_state(app: tauri::AppHandle, target: String) -> Result<(), String> {
    // `app.state::<BrainState>()` returns `State<'_, Mutex<PetBrain>>`
    // which derefs to `&Mutex<PetBrain>`; `&*` recovers the reference.
    let brain = &*app.state::<BrainState>();
    let new_state = {
        let mut guard = brain
            .lock()
            .map_err(|e| format!("PetBrain mutex poisoned: {e}"))?;
        let signal = match target.as_str() {
            "has-something" => Signal::ReminderDue,
            "needs-you" => Signal::Error,
            "thinking" => Signal::AgentBusy,
            "dismissed" => Signal::Dismissed,
            "panel" => Signal::PanelOpened,
            other => return Err(format!("unknown dev target: {other}")),
        };
        // Apply the signal and mark it as already-emitted so the tick
        // loop doesn't double-emit on its next tick.
        guard.apply_and_stage_emit(signal)
    };
    // Emit immediately so the frontend updates without waiting for the
    // next 1s tick. Best-effort — errors are surfaced to the caller.
    app.emit_to("pet", "pet_state", new_state)
        .map_err(|e| format!("emit pet_state failed: {e}"))?;
    Ok(())
}

// ===========================================================================
// Task 16: todos / reminders / dismiss commands.
//
// All todo_* + reminder_* commands lock the shared
// `Arc<Mutex<Connection>>` (managed in `setup` below — Task 14 db,
// shared with the chat commands) for the duration of the rusqlite
// call. They're thin wrappers around `db::todo_*` / `db::reminder_*`
// that map rusqlite errors to `String` for the IPC boundary.
//
// `dismiss_pet_alert` applies `Signal::Dismissed` to the brain via
// `brain.apply_and_stage_emit` (NOT `apply_signal` — we want the
// immediate emit so the frontend's `?` clears without waiting for the
// next 1s tick). Called by `Pet.svelte`'s click handler when the pet
// state is `NeedsYou` (so clicking the `?` dismisses it instead of
// opening the panel).
// ===========================================================================

#[tauri::command]
fn todo_list(db: State<'_, Arc<Mutex<Connection>>>) -> Result<Vec<db::Todo>, String> {
    let conn = db.lock().map_err(|e| format!("db lock: {e}"))?;
    db::todo_list(&conn).map_err(|e| format!("todo_list: {e}"))
}

#[tauri::command]
fn todo_add(db: State<'_, Arc<Mutex<Connection>>>, text: String) -> Result<i64, String> {
    let conn = db.lock().map_err(|e| format!("db lock: {e}"))?;
    db::todo_add(&conn, &text).map_err(|e| format!("todo_add: {e}"))
}

#[tauri::command]
fn todo_toggle(db: State<'_, Arc<Mutex<Connection>>>, id: i64) -> Result<(), String> {
    let conn = db.lock().map_err(|e| format!("db lock: {e}"))?;
    db::todo_toggle(&conn, id).map_err(|e| format!("todo_toggle: {e}"))
}

#[tauri::command]
fn todo_delete(db: State<'_, Arc<Mutex<Connection>>>, id: i64) -> Result<(), String> {
    let conn = db.lock().map_err(|e| format!("db lock: {e}"))?;
    db::todo_delete(&conn, id).map_err(|e| format!("todo_delete: {e}"))
}

#[tauri::command]
fn reminder_list(db: State<'_, Arc<Mutex<Connection>>>) -> Result<Vec<Reminder>, String> {
    let conn = db.lock().map_err(|e| format!("db lock: {e}"))?;
    db::reminder_list(&conn).map_err(|e| format!("reminder_list: {e}"))
}

#[tauri::command]
fn reminder_add(
    db: State<'_, Arc<Mutex<Connection>>>,
    text: String,
    due_at: String,
) -> Result<i64, String> {
    let conn = db.lock().map_err(|e| format!("db lock: {e}"))?;
    db::reminder_add(&conn, &text, &due_at).map_err(|e| format!("reminder_add: {e}"))
}

#[tauri::command]
fn reminder_delete(db: State<'_, Arc<Mutex<Connection>>>, id: i64) -> Result<(), String> {
    let conn = db.lock().map_err(|e| format!("db lock: {e}"))?;
    db::reminder_delete(&conn, id).map_err(|e| format!("reminder_delete: {e}"))
}

/// Dismiss the pet's current `?` (NeedsYou) alert. Applies
/// `Signal::Dismissed` to the brain + emits the resulting state
/// immediately so the frontend's `?` clears on the same click (no
/// 1s tick latency). No-op if the brain isn't in `NeedsYou` (the
/// state machine's `Dismissed` arm is itself a no-op for other
/// states, so this is safe to call speculatively). Called by
/// `Pet.svelte`'s click handler when `state === "NeedsYou"`.
#[tauri::command]
fn dismiss_pet_alert(app: tauri::AppHandle) -> Result<(), String> {
    let brain = &*app.state::<BrainState>();
    let new_state = {
        let mut guard = brain
            .lock()
            .map_err(|e| format!("PetBrain mutex poisoned: {e}"))?;
        guard.apply_and_stage_emit(Signal::Dismissed)
    };
    app.emit_to("pet", "pet_state", new_state)
        .map_err(|e| format!("emit pet_state failed: {e}"))?;
    Ok(())
}

/// Apply `Signal::FocusMode(bool)` to the brain + emit the resulting
/// state immediately. Called from `settings_set` (via the
/// `settings_set_with_focus` command below) so toggling 勿扰 takes
/// effect right away (without waiting for the brain's 1s tick to
/// re-derive — focus is a manual toggle, not derived from idle).
/// Also called from `setup` on startup with the persisted
/// `config.focus.enabled` so the pet starts asleep if the user had
/// 勿扰 on when they last quit.
fn apply_focus_to_brain<R: tauri::Runtime>(app: &AppHandle<R>, enabled: bool) {
    let brain = &*app.state::<BrainState>();
    let new_state = {
        let Ok(mut guard) = brain.lock() else {
            eprintln!("[focus] PetBrain mutex poisoned");
            return;
        };
        guard.apply_and_stage_emit(Signal::FocusMode(enabled))
    };
    if let Err(e) = app.emit_to("pet", "pet_state", new_state) {
        eprintln!("[focus] emit pet_state failed: {e}");
    }
}

/// `settings_set` wrapper that ALSO applies `Signal::FocusMode` to the
/// brain immediately (so toggling 勿扰 in SettingsView takes effect
/// on the same click, not the next 1s tick). The underlying
/// `config::settings_set` persists the settings; this wrapper then
/// re-applies focus from the persisted `focus.enabled`. Other
/// settings (theme / working_dir / follow_activity_min) don't need
/// immediate brain effects — they're picked up by the next consumer
/// (chat_send reads working_dir; the throttle reads
/// follow_activity_min on its next construction; the theme is applied
/// by the frontend).
#[tauri::command]
fn settings_set_with_focus(
    app: tauri::AppHandle,
    settings: config::Settings,
) -> Result<(), String> {
    let focus_enabled = settings.focus.enabled;
    config::settings_set(settings)?;
    apply_focus_to_brain(&app, focus_enabled);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        // Task 16: register the notification plugin so the reminder
        // scheduler can fire Windows toast via `app.notification()…`.
        // No capabilities entry needed for the Rust-side
        // `NotificationExt::notification().builder()…show()` path
        // (the JS-side `notification` API needs the capability; the
        // Rust path doesn't — it's all in-process).
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            set_pet_click_through,
            set_pet_dragging,
            open_panel_above_pet,
            // config.rs commands (Task 4 in-session window-state
            // persistence; Task 14 backed them with a real TOML file
            // next to the exe, %APPDATA% fallback). Same signatures —
            // Task 4's frontend callers are unchanged.
            config::save_pet_pos,
            config::save_panel_state,
            config::load_window_state,
            config::load_pet_pos,
            config::load_panel_state,
            // Task 7 dev-only hook for manually triggering pet states
            // (has-something / needs-you / thinking / dismissed / panel)
            // to verify the SVG/CSS poses without the real agent/reminder
            // wiring (Tasks 9+/16). Obsolete once those land; remove then.
            //
            // Task 17: gated `#[cfg(debug_assertions)]` — NOT registered in
            // release builds. The frontend `invoke("dev_trigger_state")`
            // would fail in release; that's expected (dev-only).
            #[cfg(debug_assertions)]
            dev_trigger_state,
            // Task 15: chat commands — `list_conversations` (sidebar),
            // `new_conversation` (新建 button), `load_history` (select
            // conversation), `chat_send` (composer send — kicks off
            // `run_chat_turn` + spawns the stream-drain task). All four
            // share a managed `Arc<Mutex<Connection>>` (opened + migrated
            // in `setup` below — Task 14 db).
            chat::list_conversations,
            chat::new_conversation,
            chat::load_history,
            chat::chat_send,
            // Task 16: todos / reminders / settings / dismiss commands.
            // `todo_*` + `reminder_*` are thin wrappers around
            // `db::todo_*` / `db::reminder_*`. `settings_get` /
            // `settings_set_with_focus` load/save the TOML config + the
            // focus wrapper applies `Signal::FocusMode` to the brain
            // immediately so toggling 勿扰 takes effect on the same
            // click. `dismiss_pet_alert` applies `Signal::Dismissed`
            // so `Pet.svelte`'s click-when-NeedsYou handler clears the
            // `?` instead of opening the panel.
            todo_list,
            todo_add,
            todo_toggle,
            todo_delete,
            reminder_list,
            reminder_add,
            reminder_delete,
            config::settings_get,
            settings_set_with_focus,
            dismiss_pet_alert,
        ])
        .setup(|app| {
            // Default the pet overlay to click-through (pass-through) so the
            // transparent frame around the robot passes clicks to windows
            // behind. The background poller below toggles this off when the
            // global cursor enters the robot rect.
            if let Some(pet) = app.get_webview_window("pet") {
                let _ = pet.set_ignore_cursor_events(true);
            }
            // Move the pet from the (1400, 760) placeholder baked into
            // `tauri.conf.json` to the real bottom-right of the primary
            // monitor's work area (closes the Task 1 deferred minor "pet
            // starts at a hardcoded placeholder"). If a saved within-session
            // position exists (the user dragged the pet, then hid+re-showed
            // it via 显示/隐藏), restore that instead. Best-effort: if the
            // monitor query fails we leave the placeholder alone rather than
            // fail launch.
            if let Some(pet) = app.get_webview_window("pet") {
                if let Some((x, y)) = window::pet_startup_position(app) {
                    let _ = pet.set_position(tauri::PhysicalPosition::new(x, y));
                }
            }
            // Task 7 + Task 16: manage the PetBrain (state machine +
            // follow-activity throttle + HasSomething auto-clear timer)
            // as Tauri state. Task 16 wraps it in `Arc` so the reminder
            // scheduler's spawned tokio task can clone the Arc + lock
            // the SAME brain — `Mutex` alone isn't `Clone`, so the
            // spawn couldn't own a handle without re-managing a second
            // brain (which would split the state). `BrainState =
            // Arc<Mutex<PetBrain>>` derefs through Arc to
            // `&Mutex<PetBrain>` transparently, so the existing
            // `brain.lock()` call sites in `tick` / `dev_trigger_state`
            // / `dismiss_pet_alert` / `apply_focus_to_brain` work
            // unchanged.
            let brain_state: Arc<Mutex<PetBrain>> =
                Arc::new(Mutex::new(PetBrain::new(Instant::now())));
            app.manage(Arc::clone(&brain_state));
            // Task 15: open + migrate the SQLite db at startup, manage it
            // as `Arc<Mutex<Connection>>` so the four chat commands
            // (`list_conversations` / `new_conversation` / `load_history`
            // / `chat_send`) + the spawned stream-drain task (which calls
            // `save_turn_record` on turn completion) all share one
            // connection. Errors are surfaced (fail launch) because a
            // desktop pet that can't persist conversations is useless;
            // the portable + %APPDATA% fallback in `db::resolve_data_dir`
            // makes a real failure here rare (Program Files → appdata).
            //
            // Task 16: the same `Arc<Mutex<Connection>>` is now ALSO
            // shared with the todo_* / reminder_* commands + the reminder
            // scheduler (cloned into the spawn below) — so all db paths
            // share one connection + one lock.
            //
            // NB: a `Mutex<Connection>` is NOT a `tauri::async_runtime`
            // blocking hazard because the chat commands hold the lock
            // only for the duration of the synchronous rusqlite calls
            // (list/load/save) — the long-running agent stream is driven
            // in a spawned task that does NOT hold the lock while
            // waiting on the backend (it only re-locks briefly to call
            // `save_turn_record` on completion).
            let conn = {
                let mut c = db::open().map_err(|e| {
                    format!("Task 15 db::open failed at setup: {e}")
                })?;
                db::migrate(&mut c).map_err(|e| {
                    format!("Task 15 db::migrate failed at setup: {e}")
                })?;
                c
            };
            let db_state = Arc::new(Mutex::new(conn));
            app.manage(Arc::clone(&db_state));
            // Task 7: spawn the brain tick loop (1Hz std::thread). Reads
            // idle time each tick, drives the state machine, fires the
            // follow-activity nudge, and emits `pet_state` /
            // `pet_activity_nudge` to the pet webview. Gated on pet
            // visibility (skips while hidden). Named `pet-brain-tick` for
            // debugging.
            brain::spawn_brain_tick(app.app_handle().clone());
            // Task 16: spawn the reminder scheduler (a tokio task). Every
            // ~15s, locks the db + brain + runs `run_reminder_tick`:
            // scans `reminder_list` for due-but-not-fired rows + fires
            // toast + `Signal::ReminderDue` + `reminder_mark_fired`.
            // Errors are logged (a transient db failure shouldn't crash
            // the scheduler). Lives for the process's lifetime (same
            // model as the brain tick + click-through poller).
            //
            // `Arc::clone(&brain_state)` yields a `BrainState` (= an
            // owned `Arc<Mutex<PetBrain>>`) — the scheduler's signature
            // takes `BrainState` by value so the spawn owns it.
            reminders::spawn_reminder_scheduler(
                app.app_handle().clone(),
                Arc::clone(&db_state),
                Arc::clone(&brain_state),
            );
            // Task 16: apply the persisted FocusMode (勿扰) to the brain
            // at startup so the pet starts asleep if the user had 勿扰
            // on when they last quit. `apply_focus_to_brain` emits the
            // resulting state immediately (so the frontend paints Sleep
            // on the first frame, not Standby-then-Sleep after the 1s
            // tick).
            let focus_enabled = config::load().focus.enabled;
            apply_focus_to_brain(app.app_handle(), focus_enabled);
            // Background cursor poll that flips the pet window out of
            // click-through when the cursor is over the robot, and back to
            // pass-through when it leaves. Runs in Rust because the frontend
            // gets no pointer events while the window is pass-through
            // (WS_EX_TRANSPARENT on Windows). See task-2-report.md §2.
            // Task 3 also gates this poll on pet visibility (see
            // window.rs::PET_VISIBLE) and names the thread
            // `pet-clickthrough-poll`. Task 4 added a DRAGGING gate that
            // pins `ignore=false` mid-drag (see window.rs::DRAGGING).
            window::spawn_click_through_poller(app.app_handle().clone());
            // Build the system tray (icon + 显示/隐藏 / 设置 / 退出 menu +
            // left-click → show_panel_or_toggle_pet). Errors propagate to
            // the setup hook and fail the launch if the tray can't be built.
            tray::build_tray(app.app_handle())?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
