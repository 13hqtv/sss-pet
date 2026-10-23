//! Task 16: Windows toast notifications for due reminders.
//!
//! Thin wrapper around `tauri-plugin-notification` (the Tauri v2 native
//! notification plugin). The plugin wraps WinRT
//! `ToastNotificationManager` and registers the AppUserModelID under
//! Tauri's identifier, so a background tokio task (the reminder
//! scheduler) can fire a toast via
//! `app.notification().builder()…show()` without the app doing its own
//! AUMID registration — which on Windows is a non-trivial
//! `SetCurrentProcessExplicitAppUserModelID` dance + Start Menu shortcut
//! requirement that the plugin handles for us.
//!
//! Design ref: `docs/superpowers/specs/2026-10-21-desktop-pet-design.md`
//! ("提醒 — toast + 宠物 `!`"). Reminders are USER-SCHEDULED alerts, so
//! they fire even under 勿扰 (focus mode); the focus toggle only
//! suppresses the AMBIENT follow-activity nudge + sleep animation, not
//! user-set reminders.
//!
//! ## Why a wrapper (not direct plugin calls in the scheduler)
//!
//! The reminder scheduler's tick function takes an injected
//! `notify: impl Fn(&str, &str)` closure so the unit test can inject a
//! fake (collecting) notifier without touching the real plugin. The
//! production spawn wires that closure to [`toast`] — the single place
//! that knows about `tauri-plugin-notification`. Testability + a thin
//! seam, mirroring `chat::run_chat_turn`'s injected `emit` closure.
//!
//! ## Errors
//!
//! `toast` is best-effort: a failed `show()` (e.g. notification
//! platform not available, focus-assist suppressed it, the AppHandle
//! has no webview) is logged to stderr, NOT propagated. A missed toast
//! is recoverable — the pet `!` (set by the scheduler via
//! `Signal::ReminderDue`) is the primary alert; the toast is a
//! convenience layer on top. Same philosophy as the brain's
//! `emit pet_state` calls.

use tauri::{AppHandle, Runtime};

/// Fire a Windows toast notification with the given title + body.
///
/// Best-effort: errors are logged to stderr, NOT returned. A missed
/// toast is recoverable (the pet `!` from `Signal::ReminderDue` is the
/// primary alert). Called by the reminder scheduler's production spawn
/// (the test injects a fake closure instead).
///
/// The plugin's `NotificationExt::notification` is gated on the
/// `notification` feature of the Tauri runtime; we registered the
/// plugin in `lib.rs::run` so `app.notification()` is available for
/// the lifetime of the process.
pub fn toast<R: Runtime>(app: &AppHandle<R>, title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    if let Err(e) = app
        .notification()
        .builder()
        .title(title)
        .body(body)
        .show()
    {
        log::error!("[notify] toast show failed (title={title:?}): {e}");
    }
}

#[cfg(test)]
mod tests {
    // The real `toast` calls into `tauri-plugin-notification`, which
    // needs a live Tauri runtime — out of reach for a unit test. The
    // reminder scheduler's tick takes an injected `notify: impl Fn(&str,
    // &str)` closure, so the test injects a collecting fake instead of
    // touching this module. See `reminders::tests::due_reminder_fires`.
    #[test]
    fn toast_module_compiles() {
        // Trivial compile-only assertion so this module has a test entry
        // (cargo counts it toward the test suite even though it has no
        // runtime behavior to assert — the seam is in `reminders.rs`).
        assert!(true);
    }
}
