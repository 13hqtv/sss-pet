//! System tray icon, context menu, and left-click behavior (Task 3).
//!
//! Builds a single tray icon (`id = "main"`) with a right-click context menu
//! and a left-click that toggles panel/pet visibility per the design:
//!
//! - **Right-click** → context menu: `显示/隐藏`, `设置`, `退出`.
//! - **Left-click** → [`window::show_panel_or_toggle_pet`]:
//!   panel hidden → show + focus the panel; panel visible → hide the panel
//!   and show the pet if it was hidden.
//!
//! The icon reuses the app's bundled window icon ([`AppHandle::default_window_icon`])
//! as a placeholder — the brief defers the real "sleeping robot" art to
//! Task 7's visual pass. `show_menu_on_left_click(false)` is set so the left
//! click is delivered to [`on_tray_icon_event`] instead of opening the
//! context menu (which is the default on Windows).

use tauri::menu::{MenuBuilder, MenuEvent};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Runtime};

use crate::window;

/// Tray icon identifier. Used so later tasks can look the icon up via
/// `app.tray_by_id("main")` if they need to mutate it (e.g. swapping to the
/// sleeping-robot art in Task 7).
const TRAY_ID: &str = "main";

/// Menu item id for `显示/隐藏` (toggles pet window visibility).
const MENU_ID_SHOW_HIDE: &str = "show_hide";
/// Menu item id for `设置` (shows the settings panel).
const MENU_ID_SETTINGS: &str = "settings";
/// Menu item id for `退出` (quits the app).
const MENU_ID_QUIT: &str = "quit";

/// Tray tooltip (hover text). Chinese per the global UI-copy constraint.
const TRAY_TOOLTIP: &str = "桌面宠物机器人";

/// Build the system tray icon + context menu + left-click behavior.
///
/// Called once from `lib.rs::run`'s `setup` closure after the click-through
/// poller is spawned. The returned [`tauri::Result`] propagates icon/menu
/// construction failures to the setup hook, which fails the app launch if
/// the tray could not be built (preferred over a silent no-tray state).
///
/// The icon is sourced from [`AppHandle::default_window_icon`] — the same
/// icon `tauri-build` embeds from `tauri.conf.json::bundle.icon` (Task 1
/// configured five icons including `icons/icon.png`). This is the brief's
/// prescribed placeholder; Task 7 will swap in dedicated sleeping-robot art.
pub fn build_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    // Right-click context menu: 显示/隐藏 / 设置 / 退出.
    // `MenuBuilder::text` adds a plain text item with the given id; the id
    // is what `on_menu_event` matches on, so the visible label can be
    // changed later without touching the handler. `text` returns `Self`
    // (items are stored as Result internally and surfaced by `build()`).
    let menu = MenuBuilder::new(app)
        .text(MENU_ID_SHOW_HIDE, "显示/隐藏")
        .text(MENU_ID_SETTINGS, "设置")
        .text(MENU_ID_QUIT, "退出")
        .build()?;

    // Reuse the app's embedded window icon as the tray icon (placeholder
    // per the brief — Task 7 swaps in real sleeping-robot art).
    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| tauri::Error::AssetNotFound("default window icon for tray".into()))?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip(TRAY_TOOLTIP)
        .menu(&menu)
        // Default is `true` (Windows opens the context menu on left click).
        // Disable it so the left click reaches `on_tray_icon_event` instead,
        // which drives `show_panel_or_toggle_pet`.
        .show_menu_on_left_click(false)
        .on_menu_event(handle_menu_event)
        .on_tray_icon_event(handle_tray_icon_event)
        .build(app)?;

    Ok(())
}

/// Menu event handler. Dispatched for *any* menu event from this app (the
/// handler is global per the `on_menu_event` contract), so the `_` arm
/// silently ignores ids we don't own — that lets Task 15/16 add a panel
/// menu without a conflict here.
fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, event: MenuEvent) {
    match event.id().as_ref() {
        MENU_ID_SHOW_HIDE => {
            // 显示/隐藏 → toggle the pet overlay's visibility. Errors are
            // logged to stderr (dev terminal) rather than panicking — a
            // missing window mid-launch is recoverable on the next click.
            if let Err(e) = window::toggle_pet_visibility(app) {
                eprintln!("[tray] 显示/隐藏 failed: {e}");
            }
        }
        MENU_ID_SETTINGS => {
            // 设置 → show the panel. The settings tab itself is wired in
            // Task 15/16; for Task 3 showing the panel is the whole behavior.
            if let Err(e) = window::show_panel(app) {
                eprintln!("[tray] 设置 failed: {e}");
            }
            // Switch the panel to the 设置 tab so the user sees the
            // settings view immediately (not whatever tab was last active).
            let _ = app.emit_to("panel", "switch_tab", "settings");
        }
        MENU_ID_QUIT => {
            // 退出 → exit the app. `exit(0)` runs the normal Tauri
            // shutdown path (closes windows, drops state).
            app.exit(0);
        }
        _ => {}
    }
}

/// Tray icon event handler. Only handles a left-click *release*
/// (`MouseButton::Left` + `MouseButtonState::Up`) to avoid double-firing
/// on the Down/Up pair Windows emits for a single click.
fn handle_tray_icon_event<R: Runtime>(
    tray: &tauri::tray::TrayIcon<R>,
    event: TrayIconEvent,
) {
    if let TrayIconEvent::Click {
        button: MouseButton::Left,
        button_state: MouseButtonState::Up,
        ..
    } = event
    {
        let app = tray.app_handle();
        if let Err(e) = window::show_panel_or_toggle_pet(app) {
            eprintln!("[tray] left-click (show_panel_or_toggle_pet) failed: {e}");
        }
    }
    // Right-click is handled by the OS opening the context menu; we don't
    // need to do anything for it here. Other events (DoubleClick, Enter,
    // Move, Leave) are intentionally ignored.
}
