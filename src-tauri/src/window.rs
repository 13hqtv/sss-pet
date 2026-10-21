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
//!
//! ## Pet visibility (Task 3)
//!
//! Task 3's tray menu `显示/隐藏` toggles the pet window's visibility via
//! [`show_pet`]/[`hide_pet`]/[`toggle_pet_visibility`]. A module-level
//! atomic flag `PET_VISIBLE` is the source of truth for whether the poller
//! should be doing cursor work: when the pet is hidden there is nothing to
//! make interactive, so the poller *pauses* (skips the `GetCursorPos` +
//! `set_ignore_cursor_events` work and sleeps at a longer cadence). This
//! closes the Task 2 deferred minor "poll runs unconditionally" and keeps
//! the CPU/idle constraint satisfied for the hidden case too.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use device_query::{DeviceQuery, DeviceState};
use tauri::{Manager, Runtime, WebviewWindow};

/// Poll interval for the click-through background thread while the pet is
/// visible and the poller is actively tracking the cursor.
///
/// ~33Hz. A `GetCursorPos` poll at this rate is ~0.05% CPU (measured). This
/// is independent of the "pet sleeps = static pose" animation rule, which
/// targets the pet's visual animation, not this OS-level cursor poll.
const POLL_INTERVAL: Duration = Duration::from_millis(30);

/// Poll interval used while the pet window is hidden: the poller pauses its
/// cursor work and only wakes this often to re-check the visibility flag.
/// Longer than [`POLL_INTERVAL`] because there is nothing to react to until
/// the pet reappears — resume latency is bounded by this value (~200ms),
/// imperceptible against a user-initiated menu/tray action.
const PAUSE_INTERVAL: Duration = Duration::from_millis(200);

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

/// Source-of-truth flag for whether the pet overlay window is currently
/// visible. Read by the click-through poller each tick to decide whether to
/// do cursor work; written by [`show_pet`]/[`hide_pet`].
///
/// Starts `true` because `tauri.conf.json` declares the pet window
/// `visible: true`. [`show_pet`]/[`hide_pet`] keep this in sync with the
/// actual window state by setting the flag *before* calling `show()`/`hide()`
/// so the poller observes the new state no later than its next tick.
static PET_VISIBLE: AtomicBool = AtomicBool::new(true);

/// Whether the pet overlay window is currently visible, per [`PET_VISIBLE`].
fn pet_is_visible() -> bool {
    PET_VISIBLE.load(Ordering::Acquire)
}

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
/// ## Visibility gating (Task 3)
///
/// The poller reads [`PET_VISIBLE`] each tick: when the pet is hidden (e.g.
/// the user picked `显示/隐藏` from the tray menu) there is nothing to make
/// interactive, so the poller *pauses* — it skips the `GetCursorPos` +
/// `set_ignore_cursor_events` work and sleeps at [`PAUSE_INTERVAL`] (longer
/// than [`POLL_INTERVAL`]) until the pet reappears. This closes the Task 2
/// deferred minor "poll runs unconditionally". The thread is also named
/// `pet-clickthrough-poll` so it is identifiable in a debugger / process
/// listing.
pub fn spawn_click_through_poller<R: Runtime + 'static>(app: tauri::AppHandle<R>) {
    // Name the thread so it shows up identified in debugger/thread listings
    // (closes the Task 2 deferred minor "poll thread unnamed").
    if let Err(e) = thread::Builder::new()
        .name("pet-clickthrough-poll".to_string())
        .spawn(move || poll_loop(app))
    {
        eprintln!("[pet] failed to spawn pet-clickthrough-poll thread: {e}");
    }
}

/// The body of the click-through poller, factored out of
/// [`spawn_click_through_poller`] so the thread name is set on the spawn
/// call rather than buried inside the closure.
fn poll_loop<R: Runtime + 'static>(app: tauri::AppHandle<R>) {
    let device_state = DeviceState::new();
    // Matches the startup default set in lib.rs (ignore = true →
    // pass-through). Tracked locally so we only call
    // set_ignore_cursor_events when the in-rect state actually changes.
    let mut currently_pass_through = true;
    // Tracks whether we are currently in the paused (pet hidden) state, so
    // we only log the transition once on each edge and re-evaluate the
    // robot rect fresh on resume (the cursor may have moved while we
    // slept).
    let mut paused = false;

    loop {
        let Some(pet) = app.get_webview_window("pet") else {
            // Window not yet created (or closed). Keep polling; Task 2's
            // pet window is always visible, so this is a transient state.
            thread::sleep(POLL_INTERVAL);
            continue;
        };

        // Visibility gate (Task 3). When the pet is hidden there is nothing
        // to make interactive: skip the GetCursorPos read and the
        // set_ignore_cursor_events call entirely, and sleep at the longer
        // PAUSE_INTERVAL. show_pet/hide_pet update PET_VISIBLE before
        // calling pet.show()/pet.hide() so we observe the new state within
        // one PAUSE_INTERVAL tick.
        if !pet_is_visible() {
            if !paused {
                paused = true;
                // Release prints nothing; dev terminal gets this one-line
                // trace so the brief's "verify via a log line" check has a
                // signal to look for.
                eprintln!("[pet] click-through poll: pet hidden, pausing cursor work");
            }
            thread::sleep(PAUSE_INTERVAL);
            continue;
        }
        if paused {
            paused = false;
            eprintln!("[pet] click-through poll: pet visible, resuming cursor work");
        }

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
}

// ---------------------------------------------------------------------------
// Pet / panel visibility helpers (Task 3 — tray menu + left-click behavior).
//
// These extend the click-through module rather than scattering window
// manipulation across `tray.rs`. All functions are best-effort and return a
// `Result<(), String>` so a missing window (e.g. the panel was destroyed)
// surfaces as a readable error in the dev terminal rather than a panic.
// ---------------------------------------------------------------------------

/// Show the pet overlay window and mark it visible so the click-through
/// poller resumes its cursor work.
///
/// Sets [`PET_VISIBLE`] = `true` *before* calling `show()` so the poller's
/// next tick observes the new state and switches back to [`POLL_INTERVAL`].
pub fn show_pet<R: Runtime, M: Manager<R>>(app: &M) -> Result<(), String> {
    PET_VISIBLE.store(true, Ordering::Release);
    let pet: Option<WebviewWindow<R>> = app.get_webview_window("pet");
    let pet = pet.ok_or_else(|| "window 'pet' not found".to_string())?;
    pet.show().map_err(|e| format!("pet.show() failed: {e}"))?;
    Ok(())
}

/// Hide the pet overlay window and pause the click-through poller.
///
/// Sets [`PET_VISIBLE`] = `false` *before* calling `hide()` so the poller's
/// next tick observes the new state and stops doing cursor work on a window
/// the user can no longer see.
pub fn hide_pet<R: Runtime, M: Manager<R>>(app: &M) -> Result<(), String> {
    PET_VISIBLE.store(false, Ordering::Release);
    let pet: Option<WebviewWindow<R>> = app.get_webview_window("pet");
    let pet = pet.ok_or_else(|| "window 'pet' not found".to_string())?;
    pet.hide().map_err(|e| format!("pet.hide() failed: {e}"))?;
    Ok(())
}

/// Toggle the pet overlay window's visibility. Called by the tray menu's
/// `显示/隐藏` item. Reads [`PET_VISIBLE`] for the current state so it
/// stays correct even if the window's actual visibility was flipped out of
/// band (defensive — nothing in Task 3 does that).
pub fn toggle_pet_visibility<R: Runtime, M: Manager<R>>(app: &M) -> Result<(), String> {
    if pet_is_visible() {
        hide_pet(app)
    } else {
        show_pet(app)
    }
}

/// Show the panel window and focus it. Used by:
/// - the tray menu `设置` item (Task 3 — the settings tab itself is wired
///   in Task 15/16; for Task 3 just showing the panel is enough);
/// - [`show_panel_or_toggle_pet`] when the panel is currently hidden.
pub fn show_panel<R: Runtime, M: Manager<R>>(app: &M) -> Result<(), String> {
    let panel: Option<WebviewWindow<R>> = app.get_webview_window("panel");
    let panel = panel.ok_or_else(|| "window 'panel' not found".to_string())?;
    panel
        .show()
        .map_err(|e| format!("panel.show() failed: {e}"))?;
    panel
        .set_focus()
        .map_err(|e| format!("panel.set_focus() failed: {e}"))?;
    Ok(())
}

/// Hide the panel window. Used by [`show_panel_or_toggle_pet`] when the
/// panel is currently visible (a left-click on the tray icon dismisses it).
pub fn hide_panel<R: Runtime, M: Manager<R>>(app: &M) -> Result<(), String> {
    let panel: Option<WebviewWindow<R>> = app.get_webview_window("panel");
    let panel = panel.ok_or_else(|| "window 'panel' not found".to_string())?;
    panel.hide().map_err(|e| format!("panel.hide() failed: {e}"))?;
    Ok(())
}

/// Left-click behavior for the tray icon, per the design:
///
/// - if the `panel` window is **hidden** → `show()` + `set_focus()` (the user
///   wants the panel back);
/// - if the `panel` window is **visible** → hide the panel, and if the `pet`
///   window is currently hidden, show it (so dismissing the panel also
///   brings the floating robot back).
///
/// This is the single entry point the tray left-click handler calls; it
/// never touches the pet when the panel is hidden (that path is for the
/// `显示/隐藏` menu item via [`toggle_pet_visibility`]).
pub fn show_panel_or_toggle_pet<R: Runtime, M: Manager<R>>(app: &M) -> Result<(), String> {
    let panel: Option<WebviewWindow<R>> = app.get_webview_window("panel");
    let panel = panel.ok_or_else(|| "window 'panel' not found".to_string())?;
    // `is_visible` returns Result<bool, _> on Tauri 2.12.4.
    let panel_visible = panel
        .is_visible()
        .map_err(|e| format!("panel.is_visible() failed: {e}"))?;

    if !panel_visible {
        // Panel hidden → show + focus. The pet is not touched here — the
        // user may have intentionally hidden it via 显示/隐藏.
        show_panel(app)
    } else {
        // Panel visible → hide it. Also bring the pet back if it was hidden
        // so the floating robot reappears alongside the dismissed panel.
        hide_panel(app)?;
        if !pet_is_visible() {
            show_pet(app)?;
        }
        Ok(())
    }
}
