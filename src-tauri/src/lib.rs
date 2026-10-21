mod tray;
mod window;

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![set_pet_click_through])
        .setup(|app| {
            // Default the pet overlay to click-through (pass-through) so the
            // transparent frame around the robot passes clicks to windows
            // behind. The background poller below toggles this off when the
            // global cursor enters the robot rect.
            if let Some(pet) = app.get_webview_window("pet") {
                let _ = pet.set_ignore_cursor_events(true);
            }
            // Background cursor poll that flips the pet window out of
            // click-through when the cursor is over the robot, and back to
            // pass-through when it leaves. Runs in Rust because the frontend
            // gets no pointer events while the window is pass-through
            // (WS_EX_TRANSPARENT on Windows). See task-2-report.md §2.
            // Task 3 also gates this poll on pet visibility (see
            // window.rs::PET_VISIBLE) and names the thread
            // `pet-clickthrough-poll`.
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
