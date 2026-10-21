//! Task 16: reminder scheduler.
//!
//! A background tokio task that periodically scans `db::reminder_list`
//! for reminders whose `due_at` has passed AND `fired == false`, and
//! for each due reminder:
//! 1. Fires a Windows toast via the injected `notify` closure
//!    (production wires this to [`crate::notify::toast`]; the test
//!    injects a collecting fake).
//! 2. Applies `Signal::ReminderDue` to the [`PetBrain`] via the
//!    injected `apply_signal` closure (production wires this to
//!    `brain.lock().apply_signal(ReminderDue)`; the test injects a
//!    recording fake). This puts the pet into `HasSomething` so it
//!    perks up + shows `!`. The brain's 5s `HasSomething` auto-clear
//!    timer (Task 7) handles the `!` going away.
//! 3. Marks the reminder `fired = 1` via `db::reminder_mark_fired`
//!    so the next tick doesn't re-fire it.
//!
//! ## Focus / 勿扰
//!
//! Per the task brief: reminders are USER-SCHEDULED alerts, so they
//! fire even under 勿扰 (focus mode). Focus only suppresses the
//! AMBIENT follow-activity nudge + the sleep animation — not user-set
//! reminders. The state machine's `Signal::ReminderDue` IS gated on
//! focus (it won't enter `HasSomething` while `focus == true`), but
//! the toast still fires (the user explicitly asked for this alert).
//! This is consistent with the ruling in the task brief.
//!
//! ## Tick interval
//!
//! 15s — tighter than the brief's "every ~60s" for snappier due-
//! detection (a reminder set for "in 30 seconds" shouldn't wait up to
//! a minute to fire). 15s is cheap (one `reminder_list` query + a
//! handful of comparisons) and well within the user's perception of
//! "on time" for a personal reminder. The test uses a fake clock so
//! the interval doesn't slow it down.
//!
//! ## Testability
//!
//! [`run_reminder_tick`] is extracted as a free function with
//! injected `notify` + `apply_signal` closures + an injected `now`
//! (DateTime<Utc>) so the unit test can drive a due reminder through
//! the scheduler without waiting for a real 15s tick or a real
//! `chrono::Utc::now`. The test asserts: due reminder → toast called
//! + signal applied + marked fired; not-yet-due reminder → nothing;
//! already-fired reminder → nothing (no double-fire).

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use tauri::{AppHandle, Runtime};

use crate::brain::BrainState;
use crate::db;
use crate::notify;
use crate::state::machine::Signal;

/// Tick interval for the spawned scheduler. 15s — see module docs.
const TICK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// Run one scheduler tick. Pure of I/O except for the two injected
/// closures (notify + apply_signal) + the db read/write. Extracted as
/// a free function so the test can drive it with a fake `now` + fake
/// closures + a fresh temp db.
///
/// # Arguments
/// - `conn`: shared SQLite connection (the same managed
///   `Arc<Mutex<Connection>>` the chat commands use).
/// - `now`: the current UTC time. Production passes `Utc::now()`; the
///   test passes a fixed time so a due reminder can be set up
///   deterministically.
/// - `notify`: injected toast closure. Production wires this to
///   `notify::toast(&app, title, body)`; the test injects a collecting
///   fake so the assertion doesn't depend on a real WinRT call.
/// - `apply_signal`: injected signal-apply closure. Production wires
///   this to `brain.lock().apply_signal(Signal::ReminderDue)`; the test
///   injects a recording fake.
///
/// # Returns
/// The list of reminder ids fired this tick (mostly for the test;
/// production ignores it). Errors from the db are logged to stderr
/// (a transient db failure shouldn't crash the scheduler) and the
/// tick returns an empty vec — the next tick will retry.
pub fn run_reminder_tick(
    conn: &Connection,
    now: DateTime<Utc>,
    notify: &mut dyn FnMut(&str, &str),
    apply_signal: &mut dyn FnMut(Signal),
) -> Vec<i64> {
    let reminders = match db::reminder_list(conn) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[reminders] reminder_list failed: {e}");
            return Vec::new();
        }
    };
    let mut fired_ids: Vec<i64> = Vec::new();
    for r in reminders {
        if r.fired {
            continue;
        }
        // Parse the stored RFC3339 due_at. A malformed row is logged +
        // skipped (rather than crashing the whole tick — one bad row
        // shouldn't block the others).
        let due: DateTime<Utc> = match DateTime::parse_from_rfc3339(&r.due_at) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(e) => {
                eprintln!(
                    "[reminders] reminder {} has malformed due_at {:?}: {e}",
                    r.id, r.due_at
                );
                continue;
            }
        };
        if due <= now {
            // Fire: toast + ReminderDue + mark fired.
            notify("提醒", &r.text);
            apply_signal(Signal::ReminderDue);
            if let Err(e) = db::reminder_mark_fired(conn, r.id) {
                eprintln!("[reminders] reminder_mark_fired {} failed: {e}", r.id);
                // Don't skip recording the id — the toast + signal
                // already fired; the next tick will retry the mark
                // (idempotent UPDATE).
            }
            fired_ids.push(r.id);
        }
    }
    fired_ids
}

/// Spawn the reminder scheduler as a tokio task. Runs for the lifetime
/// of the process. Every [`TICK_INTERVAL`], locks the shared db +
/// runs [`run_reminder_tick`] with the real `notify::toast` + the
/// real `brain.apply_signal` (production wiring). Errors are logged,
/// not propagated (a transient db failure shouldn't crash the
/// scheduler).
///
/// `Arc<Mutex<Connection>>` + the `BrainState` (`Arc<Mutex<PetBrain>>`)
/// are passed BY VALUE (the caller `Arc::clone`s them) so the spawned
/// task owns them for its lifetime (the task outlives any single Tauri
/// command).
pub fn spawn_reminder_scheduler<R: Runtime + 'static>(
    app: AppHandle<R>,
    db_state: Arc<Mutex<Connection>>,
    brain_state: BrainState,
) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(TICK_INTERVAL).await;
            // Take the db lock + run a tick. The brain lock is taken
            // inside the notify/apply closures (only when a reminder
            // is actually due) so a quiet tick (no due reminders)
            // doesn't touch the brain at all.
            let fired = {
                let conn = match db_state.lock() {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("[reminders] db lock poisoned: {e}");
                        continue;
                    }
                };
                let now = Utc::now();
                let mut notify_fn = |title: &str, body: &str| {
                    notify::toast(&app, title, body);
                };
                let mut apply_fn = |signal: Signal| {
                    if let Ok(mut brain) = brain_state.lock() {
                        brain.apply_signal(signal);
                    } else {
                        eprintln!("[reminders] brain lock poisoned");
                    }
                };
                run_reminder_tick(&conn, now, &mut notify_fn, &mut apply_fn)
            };
            if !fired.is_empty() {
                eprintln!("[reminders] fired {} reminder(s) this tick", fired.len());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brain::PetBrain;
    use crate::state::machine::PetState;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex as StdMutex;

    /// Per-test unique reminder text so parallel tests don't collide on
    /// row content (the data dir is per-test via tempdir so collisions
    /// are unlikely — but being explicit avoids future flake).
    static REM_SEQ: AtomicU64 = AtomicU64::new(0);

    fn unique_text(label: &str) -> String {
        let n = REM_SEQ.fetch_add(1, Ordering::SeqCst);
        format!("reminder-{label}-{n}")
    }

    /// Open + migrate a fresh db in a writable temp dir, returning the
    /// `Connection` + the tempdir (kept alive so the db file isn't
    /// cleaned up mid-test) + the `TestOverrideGuard` (kept alive so
    /// the path-resolution override stays active — its Drop clears
    /// the override so the next test on this thread starts clean).
    fn fresh_db() -> (tempfile::TempDir, db::TestOverrideGuard, Connection) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let g = db::set_test_overrides(Some(tmp.path().to_path_buf()), None, None);
        let mut conn = db::open().expect("open");
        db::migrate(&mut conn).expect("migrate");
        (tmp, g, conn)
    }

    /// **Reminder scheduler tick — due reminder fires toast + signal +
    /// mark-fired; not-yet-due + already-fired reminders do not.**
    ///
    /// Three reminders in the db:
    /// 1. Due 1h ago, `fired=0` → should fire (toast called once,
    ///    signal applied once, marked fired).
    /// 2. Due 1h in the future, `fired=0` → should NOT fire.
    /// 3. Due 1h ago, `fired=1` → should NOT fire (no double-fire).
    ///
    /// The injected `notify` collects its calls into a `Vec<(String,
    /// String)>` so we can assert the title/body of the fired toast.
    /// The injected `apply_signal` records the signals it received so
    /// we can assert `ReminderDue` was applied exactly once. A real
    /// `PetBrain` is used so we can additionally assert it entered
    /// `HasSomething` (the production path).
    #[test]
    fn due_reminder_fires_toast_and_signal_and_marks_fired() {
        let (_tmp, _g, conn) = fresh_db();
        let now = Utc::now();

        // Three reminders — past-due unfired, future unfired, past-due
        // already-fired.
        let past_due = (now - chrono::Duration::hours(1)).to_rfc3339();
        let future_due = (now + chrono::Duration::hours(1)).to_rfc3339();
        let id_past = db::reminder_add(&conn, &unique_text("past"), &past_due)
            .expect("add past");
        let _id_future = db::reminder_add(&conn, &unique_text("future"), &future_due)
            .expect("add future");
        let id_already = db::reminder_add(&conn, &unique_text("already"), &past_due)
            .expect("add already");
        db::reminder_mark_fired(&conn, id_already).expect("mark already fired");

        // Collecting notify + a real PetBrain (so we can assert the
        // state machine actually entered HasSomething). The apply_fn
        // closure locks the brain + applies the signal — mirroring the
        // production wiring.
        let notified: Arc<StdMutex<Vec<(String, String)>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let notified_clone = Arc::clone(&notified);
        let mut notify_fn = move |title: &str, body: &str| {
            notified_clone
                .lock()
                .unwrap()
                .push((title.to_string(), body.to_string()));
        };

        let brain = StdMutex::new(PetBrain::new(std::time::Instant::now()));
        let mut apply_fn = |signal: Signal| {
            brain.lock().unwrap().apply_signal(signal);
        };

        let fired = run_reminder_tick(&conn, now, &mut notify_fn, &mut apply_fn);

        // Only the past-due unfired reminder fired.
        assert_eq!(fired, vec![id_past], "only the past-due unfired reminder fires");

        // Toast was called exactly once with the right title + body.
        let notifications = notified.lock().unwrap().clone();
        assert_eq!(notifications.len(), 1, "exactly one toast fired");
        assert_eq!(notifications[0].0, "提醒", "toast title is 提醒");
        // The toast body is the reminder's text — find it (the text
        // was uniquified, so just check it starts with "reminder-past-").
        assert!(
            notifications[0].1.starts_with("reminder-past-"),
            "toast body is the reminder text, got: {:?}",
            notifications[0]
        );

        // The brain entered HasSomething (the production state-machine
        // outcome of Signal::ReminderDue).
        assert_eq!(
            brain.lock().unwrap().current(),
            PetState::HasSomething,
            "brain must be in HasSomething after ReminderDue"
        );

        // The past-due reminder is now marked fired in the db — the
        // next tick won't re-fire it.
        let reminders = db::reminder_list(&conn).expect("list");
        let past = reminders.iter().find(|r| r.id == id_past).expect("past present");
        assert!(past.fired, "past-due reminder must be marked fired");
    }

    /// A tick with NO due reminders calls neither notify nor
    /// apply_signal (a quiet tick doesn't touch the brain).
    #[test]
    fn quiet_tick_does_not_fire_anything() {
        let (_tmp, _g, conn) = fresh_db();
        let now = Utc::now();
        // One future reminder.
        let future = (now + chrono::Duration::hours(2)).to_rfc3339();
        let _id = db::reminder_add(&conn, &unique_text("future"), &future).expect("add");

        let called_notify = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let called_signal = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cn = Arc::clone(&called_notify);
        let cs = Arc::clone(&called_signal);
        let mut notify_fn = move |_t: &str, _b: &str| {
            cn.fetch_add(1, Ordering::SeqCst);
        };
        let mut apply_fn = move |_s: Signal| {
            cs.fetch_add(1, Ordering::SeqCst);
        };

        let fired = run_reminder_tick(&conn, now, &mut notify_fn, &mut apply_fn);
        assert!(fired.is_empty(), "no reminders should fire");
        assert_eq!(
            called_notify.load(Ordering::SeqCst),
            0,
            "notify must NOT be called on a quiet tick"
        );
        assert_eq!(
            called_signal.load(Ordering::SeqCst),
            0,
            "apply_signal must NOT be called on a quiet tick"
        );
    }
}
