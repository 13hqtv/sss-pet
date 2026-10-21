//! In-memory placeholder for per-window state (Task 4).
//!
//! Persists the pet's position and the panel's rect (`x`/`y`/`w`/`h`) for the
//! lifetime of the process so the same-session behaviors the brief requires
//! work end-to-end:
//!
//! - drag the pet to a new spot → on `mouseup` the frontend calls
//!   [`save_pet_pos`] with the new coordinates;
//! - hide the pet via the tray `显示/隐藏`, then show it again → [`load_pet_pos`]
//!   returns the dragged spot (not the bottom-right default);
//! - drag/resize the panel → [`save_panel_state`] records the new rect;
//! - reopen the panel → it reappears at the saved rect (within the session).
//!
//! ## Why in-memory (and what Task 15 will change)
//!
//! The store is a single `Mutex<Option<HashMap<String, WindowState>>>` static.
//! That makes the persistence wiring verifiable for Task 4 without the
//! additional surface area of picking a path + a TOML encoder. Task 15 will
//! replace *only* [`get`]/[`put`] (and grow a `flush` helper) with a TOML file
//! next to the executable; the five Tauri commands and [`WindowState`] stay
//! the same, so the frontend code calling them is forward-compatible.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// The persisted state for one window.
///
/// For the `"pet"` window only `x`/`y` are meaningful — the pet is a fixed
/// 120×120 declared in `tauri.conf.json`, so its size is not persisted (the
/// `w`/`h` fields stay `None`). For the `"panel"` window all four fields are
/// populated because the panel is user-resizable.
///
/// `w`/`h` are `Option` rather than a separate pet-state/panel-state pair so
/// the same struct + the same [`load_window_state`] command can serve both
/// windows; the frontend picks the fields it cares about.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct WindowState {
    /// Outer-window x in physical pixels (matches `outer_position().x`).
    pub x: i32,
    /// Outer-window y in physical pixels (matches `outer_position().y`).
    pub y: i32,
    /// Outer-window width in physical pixels (`None` for the pet).
    pub w: Option<u32>,
    /// Outer-window height in physical pixels (`None` for the pet).
    pub h: Option<u32>,
}

impl WindowState {
    /// Build a pet-only state (just position; size is left `None` because the
    /// pet's size is fixed by `tauri.conf.json`).
    pub fn pet(x: i32, y: i32) -> Self {
        Self {
            x,
            y,
            w: None,
            h: None,
        }
    }

    /// Build a panel state (position + size).
    pub fn panel(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self {
            x,
            y,
            w: Some(w),
            h: Some(h),
        }
    }
}

/// In-memory store. Single mutex-guarded HashMap keyed by window label
/// (`"pet"`, `"panel"`). `Option<HashMap>` rather than `HashMap` so the
/// static initializer can stay `Mutex::new(None)` (no const `HashMap::new`
/// needed across older toolchains). Task 15 swaps this for a TOML file.
static STATE: Mutex<Option<HashMap<String, WindowState>>> = Mutex::new(None);

/// Read a key out of the store. Returns a `Copy` so callers don't hold the
/// lock across an `await` point or an IPC boundary. `None` if the key has no
/// entry yet (first run, or the window was never moved/resized this session).
fn get(key: &str) -> Option<WindowState> {
    let guard = STATE.lock().ok()?;
    guard.as_ref()?.get(key).copied()
}

/// Insert (or overwrite) a key. Initializes the HashMap on first use so the
/// static can start as `None`.
fn put(key: &str, state: WindowState) {
    let Ok(mut guard) = STATE.lock() else {
        // Poisoned mutex means a panic while holding the lock; for an
        // in-memory best-effort store, dropping the write is preferable to
        // propagating the panic into the calling Tauri command.
        return;
    };
    if guard.is_none() {
        *guard = Some(HashMap::new());
    }
    if let Some(map) = guard.as_mut() {
        map.insert(key.to_string(), state);
    }
}

// ---------------------------------------------------------------------------
// Helper fns used from elsewhere in the crate (not Tauri commands).
//
// These exist so Rust-side callers (the click-through poller, the `lib.rs`
// startup-position logic) can read/write the store without going through the
// IPC boundary. The Tauri commands below are thin wrappers that call these.
// ---------------------------------------------------------------------------

/// Persist the pet's position from Rust. Used by the `lib.rs` startup-position
/// code path if it ever needs to write back; drag-time persistence is via the
/// [`save_pet_pos`] *command* from `Pet.svelte`.
pub fn save_pet_pos_state(x: i32, y: i32) {
    put("pet", WindowState::pet(x, y));
}

/// Persist the panel's rect from Rust. The frontend path is the
/// [`save_panel_state`] command; this helper is the symmetric write entry point
/// for Rust callers (none in Task 4, but the symmetry keeps Task 15's
/// file-backed version honest).
pub fn save_panel_state_state(x: i32, y: i32, w: u32, h: u32) {
    put("panel", WindowState::panel(x, y, w, h));
}

/// Load the pet's persisted position (or `None`). Used by `lib.rs::run` setup
/// to decide whether to restore a dragged spot or fall back to the real
/// bottom-right default for first run.
pub fn load_pet_pos_state() -> Option<(i32, i32)> {
    get("pet").map(|s| (s.x, s.y))
}

/// Load the panel's persisted rect (or `None`). Used by
/// [`crate::window::open_panel_above_pet`] to honor a user-resized panel when
/// re-opening (otherwise the OS default `380×560` from `tauri.conf.json` is
/// used, which is fine on first run).
pub fn load_panel_state_state() -> Option<(i32, i32, u32, u32)> {
    get("panel").and_then(|s| Some((s.x, s.y, s.w?, s.h?)))
}

// ---------------------------------------------------------------------------
// Tauri commands (called from the frontend via `invoke`).
//
// All five are sync (no `async`) because the store is in-memory and the
// operations are O(1) HashMap reads/writes — no I/O to await. Task 15 may
// make the load/save fns `async` when the backing store becomes a TOML file
// on disk.
// ---------------------------------------------------------------------------

/// Persist the pet's position. Called from `Pet.svelte` on `mouseup` after a
/// `data-tauri-drag-region` drag completes.
#[tauri::command]
pub fn save_pet_pos(x: i32, y: i32) -> Result<(), String> {
    save_pet_pos_state(x, y);
    Ok(())
}

/// Persist the panel's rect (position + size). Called from `panel/App.svelte`
/// at the end of a move/resize (debounced via `onMoved`/`onResized`).
#[tauri::command]
pub fn save_panel_state(x: i32, y: i32, w: u32, h: u32) -> Result<(), String> {
    save_panel_state_state(x, y, w, h);
    Ok(())
}

/// Generic load by window label. Returns the full [`WindowState`] (with `w`/`h`
/// as `Option` so the pet's `None` size is preserved) or `None` if no state is
/// stored. Useful for future tasks that want to query arbitrary windows.
#[tauri::command]
pub fn load_window_state(label: String) -> Result<Option<WindowState>, String> {
    Ok(get(&label))
}

/// Load just the pet's persisted position (or `None`) as a `(x, y)` tuple.
/// Convenience command for the frontend — `load_window_state("pet")` returns
/// the same data with the `x`/`y` fields and `None` size.
#[tauri::command]
pub fn load_pet_pos() -> Result<Option<(i32, i32)>, String> {
    Ok(load_pet_pos_state())
}

/// Load the panel's persisted rect (or `None`) as a `(x, y, w, h)` tuple so
/// the frontend can pass it straight to `setPosition`/`setSize`.
#[tauri::command]
pub fn load_panel_state() -> Result<Option<(i32, i32, u32, u32)>, String> {
    Ok(load_panel_state_state())
}
