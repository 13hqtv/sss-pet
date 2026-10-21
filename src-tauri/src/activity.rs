//! OS-level idle-time query (Task 6).
//!
//! [`idle_since`] returns the [`Duration`] since the user's last input
//! (keyboard / mouse) on Windows, backed by Win32 `GetLastInputInfo`.
//! The desktop pet's activity poller (Task 7) calls this on every tick
//! and feeds the result to the state machine as `Signal::IdleFor`
//! (long idle → `Sleep`) and to
//! [`crate::timers::FollowActivityThrottle::should_react`] (idle > 3min
//! → away, suppress follow-activity reactions).
//!
//! On non-Windows targets, [`idle_since`] returns [`Duration::ZERO`];
//! this project is Windows-only and the placeholder just keeps the
//! crate compiling elsewhere (it is NOT unit-tested — `idle_since` is
//! a thin OS call that's verified via `cargo build` only).
//!
//! [`idle_since`]: idle_since

use std::time::Duration;

#[cfg(windows)]
mod windows_impl {
    use std::time::Duration;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetLastInputInfo, LASTINPUTINFO,
    };

    // `GetTickCount64` from `kernel32.dll`, declared manually to avoid
    // pulling in additional `windows-sys` features (it lives under
    // `Win32::System::SystemInformation` but the FFI symbol is the
    // same). Returns the system tick count in milliseconds since boot,
    // as a 64-bit value (no 49-day wraparound, unlike the 32-bit
    // `GetTickCount`).
    //
    // `unsafe extern` is required by Rust 2024 edition (this crate's
    // edition) — extern blocks became unsafe so the caller opts into
    // the FFI safety obligation explicitly.
    unsafe extern "system" {
        fn GetTickCount64() -> u64;
    }

    /// Read the duration since the user's last keyboard / mouse input.
    ///
    /// Calls Win32 `GetLastInputInfo` (which fills `LASTINPUTINFO.dwTime`
    /// with the tick count at which the last input was received) and
    /// subtracts from `GetTickCount64()`. Returns `Duration::ZERO` if
    /// `GetLastInputInfo` fails or if `dwTime` is somehow ahead of the
    /// current tick count (the latter only happens at the 49-day
    /// `GetTickCount` 32-bit wraparound boundary; we treat it as
    /// "not idle" rather than producing a huge bogus duration).
    pub fn idle_since() -> Duration {
        unsafe {
            let mut info = LASTINPUTINFO {
                cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
                dwTime: 0,
            };
            // GetLastInputInfo returns BOOL (i32): nonzero = success.
            if GetLastInputInfo(&mut info) == 0 {
                // Failure — rare (only on terminal sessions being
                // disconnected, etc.); return zero as a safe fallback.
                return Duration::ZERO;
            }
            let last_input_ms = info.dwTime as u64;
            let now_ms = GetTickCount64();
            if now_ms >= last_input_ms {
                Duration::from_millis(now_ms - last_input_ms)
            } else {
                // `dwTime` is a 32-bit tick count and so is `now_ms`
                // truncated to 32 bits at the 49-day boundary. If the
                // 64-bit `now_ms` is somehow behind `last_input_ms`,
                // the input tick must have wrapped; treat as not idle.
                Duration::ZERO
            }
        }
    }
}

#[cfg(windows)]
pub fn idle_since() -> Duration {
    windows_impl::idle_since()
}

#[cfg(not(windows))]
pub fn idle_since() -> Duration {
    // Non-Windows placeholder: this project is Windows-only, the
    // placeholder just keeps the crate compiling for non-Windows
    // targets. NOT unit-tested (see module docs).
    Duration::ZERO
}
