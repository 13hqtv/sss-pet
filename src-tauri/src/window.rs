//! Window utilities for the desktop pet.
//!
//! Centralizes click-through (mouse-event pass-through) control for the pet
//! overlay window. Task 2 owns [`set_click_through`] and the background
//! cursor poller [`spawn_click_through_poller`]; later tasks (panel toggling
//! in Task 3, dragging in Task 4) extend this module rather than reaching for
//! `WebviewWindow` directly elsewhere.
//!
//! ## Click-through model (Tauri 2.12.4 / Windows)
//!
//! `WebviewWindow::set_ignore_cursor_events(ignore)` is the only knob this
//! version of Tauri exposes: there is **no `forward` parameter** in the
//! 2.12.4 stack (tauri 2.12.4 / tauri-runtime-wry 2.12.2 / tao 0.37.1).
//! On Windows, ignoring cursor events sets `WS_EX_TRANSPARENT | WS_EX_LAYERED`
//! on the window, which makes the OS treat the window as transparent to the
//! mouse: clicks *and* `WM_MOUSEMOVE` are routed to the window behind.
//!
//! That means the frontend **cannot** detect the cursor entering the robot
//! while the window is in pass-through mode (no `pointermove` reaches the
//! webview). Instead [`spawn_click_through_poller`] polls the global cursor
//! position from Rust every ~30ms via the `device_query` crate (Win32
//! `GetCursorPos`) and toggles `set_ignore_cursor_events` from here based on
//! whether the screen cursor is inside the robot's screen rect. This
//! reproduces the behavior the absent `forward=true` option would have given
//! us, without a frontend event loop. See `task-2-report.md` §2.

use std::thread;
use std::time::Duration;

use device_query::{DeviceQuery, DeviceState};
use tauri::{Manager, Runtime, WebviewWindow};

/// Poll interval for the click-through background thread.
///
/// ~33Hz. A `GetCursorPos` poll at this rate is ~0.05% CPU (measured). This
/// is independent of the "pet sleeps = static pose" animation rule, which
/// targets the pet's visual animation, not this OS-level cursor poll.
const POLL_INTERVAL: Duration = Duration::from_millis(30);

/// Hysteresis applied to the robot-rect edges to stop the click-through state
/// flickering when the cursor sits on the boundary: the state only flips when
/// the cursor moves this many pixels *past* the edge in either direction.
const HYSTERESIS_PX: i32 = 2;

/// The robot SVG is a 96×96 logical region centered inside the pet window's
/// 120×120 logical surface, so it occupies the central 80% — a 10% margin on
/// every side. Expressed as a ratio of the window's *physical* outer size so
/// it stays correct under any DPI scale (outer_size is physical pixels, and
/// `GetCursorPos` returns physical pixels too, so no DPI conversion is needed).
const ROBOT_MARGIN_RATIO: i32 = 10; // margin = outer_size / 10 (parts per hundred)

/// Set whether the window with `label` ignores (passes through) cursor events.
///
/// - `ignore = true`  → the window is click-through: pointer input in its
///   area falls through to whatever window is behind it (desktop included).
/// - `ignore = false` → the window receives pointer input normally.
///
/// The pet overlay starts with `ignore = true` (see `lib.rs::run` setup) so
/// the transparent frame around the robot never blocks the desktop; the
/// background poller in [`spawn_click_through_poller`] toggles it to `false`
/// when the global cursor enters the robot rect.
///
/// Maps to `WebviewWindow::set_ignore_cursor_events(ignore)` — the single-bool
/// signature Tauri 2.12.4 exposes (no `forward` flag).
pub fn set_click_through<R: Runtime, M: Manager<R>>(
    app: &M,
    label: &str,
    ignore: bool,
) -> Result<(), String> {
    let window: Option<WebviewWindow<R>> = app.get_webview_window(label);
    let window = window.ok_or_else(|| format!("window '{label}' not found"))?;
    window
        .set_ignore_cursor_events(ignore)
        .map_err(|e| format!("set_ignore_cursor_events failed for '{label}': {e}"))
}

/// Spawn a background thread that polls the global cursor position and toggles
/// click-through on the `"pet"` window so the robot is interactive while the
/// cursor is over it, and the surrounding transparent area passes clicks
/// through to windows behind.
///
/// This runs in **Rust**, not the frontend, because while the window is in
/// pass-through mode (`set_ignore_cursor_events(true)` → `WS_EX_TRANSPARENT`
/// on Windows) no mouse messages — including `WM_MOUSEMOVE` — reach the
/// webview, so a frontend `pointermove` listener can never fire to toggle it
/// back off. Polling the global cursor from the OS side sidesteps that
/// chicken-and-egg. See `task-2-report.md` §2 for the full rationale.
///
/// The robot's screen rect is derived from the window's outer position/size:
/// it is the central 80% (`ROBOT_MARGIN_RATIO` = 10% margin each side), which
/// matches the centered 96×96 SVG inside the 120×120 window and is correct at
/// any DPI since both `outer_position`/`outer_size` and `GetCursorPos` are in
/// physical pixels.
///
/// Edge hysteresis of [`HYSTERESIS_PX`] prevents flicker when the cursor sits
/// on the boundary: the state only flips once the cursor is 2px past the edge.
///
/// The pet window is always visible in Task 2, so the poll runs for the life
/// of the app. Later tasks gate visibility and can pause this poll.
pub fn spawn_click_through_poller<R: Runtime + 'static>(app: tauri::AppHandle<R>) {
    thread::spawn(move || {
        let device_state = DeviceState::new();
        // Matches the startup default set in lib.rs (ignore = true →
        // pass-through). Tracked locally so we only call
        // set_ignore_cursor_events when the in-rect state actually changes.
        let mut currently_pass_through = true;

        loop {
            let Some(pet) = app.get_webview_window("pet") else {
                // Window not yet created (or closed). Keep polling; Task 2's
                // pet window is always visible, so this is a transient state.
                thread::sleep(POLL_INTERVAL);
                continue;
            };
            // outer_position / outer_size both return physical pixels, matching
            // GetCursorPos's coordinate space — no DPI scaling needed.
            let (Ok(pos), Ok(size)) = (pet.outer_position(), pet.outer_size()) else {
                thread::sleep(POLL_INTERVAL);
                continue;
            };

            let mouse = device_state.get_mouse();
            let (mx, my) = mouse.coords;

            let w = size.width as i32;
            let h = size.height as i32;
            // Robot rect = central 80% of the window (10% margin each side).
            let margin_x = w / ROBOT_MARGIN_RATIO;
            let margin_y = h / ROBOT_MARGIN_RATIO;
            let left = pos.x + margin_x;
            let right = pos.x + w - margin_x;
            let top = pos.y + margin_y;
            let bottom = pos.y + h - margin_y;

            // Hysteresis: only flip when the cursor clears the edge by ±2px.
            let want_pass_through = if currently_pass_through {
                // Pass-through now → only go interactive once the cursor is
                // comfortably inside the robot rect.
                let entered = mx >= left + HYSTERESIS_PX
                    && mx <= right - HYSTERESIS_PX
                    && my >= top + HYSTERESIS_PX
                    && my <= bottom - HYSTERESIS_PX;
                !entered
            } else {
                // Interactive now → only go back to pass-through once the
                // cursor has left a slightly expanded rect.
                mx < left - HYSTERESIS_PX
                    || mx > right + HYSTERESIS_PX
                    || my < top - HYSTERESIS_PX
                    || my > bottom + HYSTERESIS_PX
            };

            if want_pass_through != currently_pass_through {
                match pet.set_ignore_cursor_events(want_pass_through) {
                    Ok(()) => currently_pass_through = want_pass_through,
                    Err(e) => {
                        // Leave the flag alone so the next iteration retries.
                        // Visible only in the dev terminal (release uses the
                        // windows subsystem, which has no stderr).
                        eprintln!(
                            "[pet] click-through poll: set_ignore_cursor_events({want_pass_through}) failed: {e}"
                        );
                    }
                }
            }

            thread::sleep(POLL_INTERVAL);
        }
    });
}
