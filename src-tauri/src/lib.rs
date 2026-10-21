mod config;
pub mod state;
mod tray;
mod window;
// Task 6: pure-Rust idle detection + follow-activity throttle. Not wired
// into the Tauri app yet — Task 7 will poll `activity::idle_since()` and
// drive `timers::FollowActivityThrottle` from the activity-poll thread.
pub mod activity;
pub mod timers;

use tauri::Manager;

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
