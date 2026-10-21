mod config;
pub mod state;
mod tray;
mod window;
// Task 6: pure-Rust idle detection + follow-activity throttle. Not wired
// into the Tauri app yet — Task 7 will poll `activity::idle_since()` and
// drive `timers::FollowActivityThrottle` from the activity-poll thread.
pub mod activity;
pub mod timers;
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

use std::sync::Mutex;
use std::time::Instant;

use tauri::{Emitter, Manager};

use brain::{BrainState, PetBrain};
use state::machine::Signal;

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            set_pet_click_through,
            set_pet_dragging,
            open_panel_above_pet,
            // config.rs commands (Task 4 in-session window-state persistence).
            // Task 15 swaps the in-memory HashMap backing store for a TOML file
            // next to the exe — same signatures, durable across restarts.
            config::save_pet_pos,
            config::save_panel_state,
            config::load_window_state,
            config::load_pet_pos,
            config::load_panel_state,
            // Task 7 dev-only hook for manually triggering pet states
            // (has-something / needs-you / thinking / dismissed / panel)
            // to verify the SVG/CSS poses without the real agent/reminder
            // wiring (Tasks 9+/16). Obsolete once those land; remove then.
            dev_trigger_state,
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
            // Task 7: manage the PetBrain (state machine + follow-activity
            // throttle + HasSomething auto-clear timer) as Tauri state so
            // the tick thread and the dev_trigger_state command share one
            // Mutex-guarded instance. Constructed at `now` so the throttle's
            // first reaction is ≥ 15min after launch (no startup burst).
            app.manage(Mutex::new(PetBrain::new(Instant::now())));
            // Task 7: spawn the brain tick loop (1Hz std::thread). Reads
            // idle time each tick, drives the state machine, fires the
            // follow-activity nudge, and emits `pet_state` / `pet_activity_
            // nudge` to the pet webview. Gated on pet visibility (skips
            // while hidden). Named `pet-brain-tick` for debugging.
            brain::spawn_brain_tick(app.app_handle().clone());
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
