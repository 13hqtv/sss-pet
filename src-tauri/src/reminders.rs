//! Reminder scheduler (v2: recurring, snooze, popup events).
//!
//! A background tokio task that periodically queries `db::reminder_get_due`
//! for reminders whose `next_fire_at` has passed (and are not snoozed or
//! done), and for each due reminder:
//!
//! 1. Fires a Windows toast via the injected `notify` closure.
//! 2. Applies `Signal::ReminderDue` to the [`PetBrain`] — puts the pet
//!    into `HasSomething` (`!`) as a visual cue.
//! 3. Emits a `reminder_fired` event to the pet window (payload: id + text)
//!    so the Pet.svelte frontend can show an interactive popup with
//!    snooze/dismiss buttons.
//! 4. For `once` reminders: sets status to `active` (awaits user action).
//!    For `recurring` reminders: computes the next fire time from
//!    interval_value + interval_unit and updates `next_fire_at`.
//!
//! ## Snooze
//!
//! When the user snoozes a reminder (from the popup or the list), the
//! `snoozed_until` field is set to a future time. The `reminder_get_due`
//! query excludes snoozed reminders until their snooze expires. When the
//! snooze expires, the reminder fires again.
//!
//! ## Focus / 勿扰
//!
//! Reminders fire even under focus mode (the toast always fires). Focus
//! only suppresses the `Signal::ReminderDue` at the brain level (pet
//! won't enter `HasSomething`), but the toast + popup event still fire.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use rusqlite::Connection;
use serde_json::json;
use tauri::{AppHandle, Emitter, Runtime};

use crate::brain::BrainState;
use crate::db;
use crate::notify;
use crate::state::machine::Signal;

/// Tick interval for the spawned scheduler.
const TICK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// Compute the next fire time for a recurring reminder based on its
/// interval_value + interval_unit, starting from `now`.
pub fn compute_next_fire(now: DateTime<Utc>, value: i64, unit: &str) -> DateTime<Utc> {
    match unit {
        "minutes" => now + Duration::minutes(value),
        "hours" => now + Duration::hours(value),
        "days" => now + Duration::days(value),
        "weeks" => now + Duration::weeks(value),
        _ => now + Duration::minutes(value), // fallback: treat as minutes
    }
}

/// Run one scheduler tick. Queries `reminder_get_due` for due reminders,
/// fires toast + signal + popup event for each, and updates their state
/// (active for once, next-fire for recurring).
///
/// Extracted as a free function with injected closures + injected `now`
/// for testability.
pub fn run_reminder_tick(
    conn: &Connection,
    now: DateTime<Utc>,
    notify: &mut dyn FnMut(&str, &str),
    apply_signal: &mut dyn FnMut(Signal),
    emit_reminder: &mut dyn FnMut(i64, &str),
) -> Vec<i64> {
    let now_str = now.to_rfc3339();
    let due = match db::reminder_get_due(conn, &now_str) {
        Ok(r) => r,
        Err(e) => {
            log::error!("[reminders] reminder_get_due failed: {e}");
            return Vec::new();
        }
    };
    let mut fired_ids: Vec<i64> = Vec::new();
    for r in due {
        // Fire toast.
        notify("提醒", &r.text);
        // Apply brain signal (HasSomething `!`).
        apply_signal(Signal::ReminderDue);
        // Emit popup event to the pet window.
        emit_reminder(r.id, &r.text);

        // Update the reminder's state.
        if r.reminder_type == "recurring" && r.interval_value > 0 {
            // Recurring: compute next fire time + update.
            let next = compute_next_fire(now, r.interval_value, &r.interval_unit);
            if let Err(e) = db::reminder_set_next_fire(conn, r.id, Some(&next.to_rfc3339())) {
                log::error!("[reminders] set_next_fire {} failed: {e}", r.id);
            }
        } else {
            // Once: set status to active (awaiting user action).
            if let Err(e) = db::reminder_set_status(conn, r.id, "active") {
                log::error!("[reminders] set_status active {} failed: {e}", r.id);
            }
        }
        fired_ids.push(r.id);
    }

    // Check todos with approaching deadlines (within 24h of end_at).
    // Uses a negative id range (-(todo_id)) in fired_ids to distinguish
    // from reminder ids — the caller (scheduler) doesn't differentiate.
    let horizon = now + Duration::hours(24);
    let approaching = match db::todo_get_deadline_approaching(conn, &now_str, &horizon.to_rfc3339()) {
        Ok(t) => t,
        Err(e) => {
            log::error!("[reminders] todo_get_deadline_approaching failed: {e}");
            Vec::new()
        }
    };
    for todo in approaching {
        let msg = format!("⏰ 代办「{}」即将到期", todo.text);
        notify("代办到期提醒", &msg);
        apply_signal(Signal::ReminderDue);
        // Use negative id to distinguish todo deadline from reminder.
        emit_reminder(-todo.id, &msg);
        if let Err(e) = db::todo_set_deadline_notified(conn, todo.id, true) {
            log::error!("[reminders] set_deadline_notified {} failed: {e}", todo.id);
        }
        fired_ids.push(-todo.id);
    }

    fired_ids
}

/// Spawn the reminder scheduler as a tokio task. Runs for the lifetime
/// of the process. Every [`TICK_INTERVAL`], locks the shared db + runs
/// [`run_reminder_tick`] with the real production wiring (notify::toast
/// + brain.apply_signal + app.emit_to("pet", "reminder_fired")).
pub fn spawn_reminder_scheduler<R: Runtime + 'static>(
    app: AppHandle<R>,
    db_state: Arc<Mutex<Connection>>,
    brain_state: BrainState,
) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(TICK_INTERVAL).await;
            let (fired, dispatches) = {
                let mut conn = match db_state.lock() {
                    Ok(c) => c,
                    Err(e) => {
                        log::error!("[reminders] db lock poisoned: {e}");
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
                        log::error!("[reminders] brain lock poisoned");
                    }
                };
                let app_handle = app.clone();
                let mut emit_fn = move |id: i64, text: &str| {
                    let _ = app_handle.emit_to(
                        "pet",
                        "reminder_fired",
                        json!({ "id": id, "text": text }),
                    );
                };
                let fired =
                    run_reminder_tick(&conn, now, &mut notify_fn, &mut apply_fn, &mut emit_fn);
                // Automation due check (sibling, same locked block): sync DB
                // prep only — run_chat_turn + LLM happen in spawned tasks
                // after the guard releases, so the tick stays non-blocking.
                let dispatches = crate::automation::run_automation_tick(&mut conn, now);
                (fired, dispatches)
            };
            if !fired.is_empty() {
                log::error!("[reminders] fired {} reminder(s) this tick", fired.len());
            }
            for d in dispatches {
                let app = app.clone();
                let db_state = db_state.clone();
                tauri::async_runtime::spawn(
                    crate::automation::automation_dispatch_async(app, db_state, d),
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    fn fresh_db() -> (tempfile::TempDir, db::TestOverrideGuard, Connection) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let g = db::set_test_overrides(Some(tmp.path().to_path_buf()), None, None);
        let mut conn = db::open().expect("open");
        db::migrate(&mut conn).expect("migrate");
        (tmp, g, conn)
    }

    #[test]
    fn due_reminder_fires_and_sets_active() {
        let (_tmp, _g, conn) = fresh_db();
        let now = Utc::now();
        let past = (now - chrono::Duration::hours(1)).to_rfc3339();

        let id = db::reminder_add(&conn, "test task", "once", 0, "minutes", Some(&past))
            .expect("add");

        let notified = Arc::new(StdMutex::new(Vec::new()));
        let notified_clone = Arc::clone(&notified);
        let mut notify_fn = move |title: &str, body: &str| {
            notified_clone.lock().unwrap().push((title.to_string(), body.to_string()));
        };
        let mut apply_fn = |_s: Signal| {};
        let emitted = Arc::new(StdMutex::new(Vec::new()));
        let emitted_clone = Arc::clone(&emitted);
        let mut emit_fn = move |id: i64, text: &str| {
            emitted_clone.lock().unwrap().push((id, text.to_string()));
        };

        let fired = run_reminder_tick(&conn, now, &mut notify_fn, &mut apply_fn, &mut emit_fn);
        assert_eq!(fired, vec![id]);

        let notifications = notified.lock().unwrap().clone();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].0, "提醒");

        let emits = emitted.lock().unwrap().clone();
        assert_eq!(emits.len(), 1);
        assert_eq!(emits[0].0, id);
        assert_eq!(emits[0].1, "test task");

        // Once-type reminder → status = active.
        let list = db::reminder_list(&conn).expect("list");
        let r = list.iter().find(|r| r.id == id).expect("found");
        assert_eq!(r.status, "active");
    }

    #[test]
    fn recurring_reminder_computes_next_fire() {
        let (_tmp, _g, conn) = fresh_db();
        let now = Utc::now();
        let past = (now - chrono::Duration::minutes(1)).to_rfc3339();

        let id = db::reminder_add(&conn, "recurring task", "recurring", 30, "minutes", Some(&past))
            .expect("add");

        let mut notify_fn = |_t: &str, _b: &str| {};
        let mut apply_fn = |_s: Signal| {};
        let mut emit_fn = |_id: i64, _text: &str| {};

        let fired = run_reminder_tick(&conn, now, &mut notify_fn, &mut apply_fn, &mut emit_fn);
        assert_eq!(fired, vec![id]);

        // Next fire should be ~30 min from now.
        let list = db::reminder_list(&conn).expect("list");
        let r = list.iter().find(|r| r.id == id).expect("found");
        assert!(r.next_fire_at.is_some());
        let next = DateTime::parse_from_rfc3339(r.next_fire_at.as_deref().unwrap()).unwrap();
        let expected_min = now + Duration::minutes(29);
        let expected_max = now + Duration::minutes(31);
        assert!(next.with_timezone(&Utc) > expected_min);
        assert!(next.with_timezone(&Utc) < expected_max);
    }

    #[test]
    fn snoozed_reminder_does_not_fire() {
        let (_tmp, _g, conn) = fresh_db();
        let now = Utc::now();
        let past = (now - chrono::Duration::hours(1)).to_rfc3339();
        let snooze_future = (now + chrono::Duration::minutes(30)).to_rfc3339();

        let id = db::reminder_add(&conn, "snoozed task", "once", 0, "minutes", Some(&past))
            .expect("add");
        db::reminder_snooze(&conn, id, &snooze_future).expect("snooze");

        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = Arc::clone(&count);
        let mut notify_fn = move |_t: &str, _b: &str| { count_clone.fetch_add(1, Ordering::SeqCst); };
        let mut apply_fn = |_s: Signal| {};
        let mut emit_fn = |_id: i64, _text: &str| {};

        let fired = run_reminder_tick(&conn, now, &mut notify_fn, &mut apply_fn, &mut emit_fn);
        assert!(fired.is_empty(), "snoozed reminder should not fire");
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn quiet_tick_does_not_fire_anything() {
        let (_tmp, _g, conn) = fresh_db();
        let now = Utc::now();
        let future = (now + chrono::Duration::hours(2)).to_rfc3339();
        let _id = db::reminder_add(&conn, "future task", "once", 0, "minutes", Some(&future))
            .expect("add");

        let cn = Arc::new(AtomicUsize::new(0));
        let cn_clone = Arc::clone(&cn);
        let mut notify_fn = move |_t: &str, _b: &str| { cn_clone.fetch_add(1, Ordering::SeqCst); };
        let cs = Arc::new(AtomicUsize::new(0));
        let cs_clone = Arc::clone(&cs);
        let mut apply_fn = move |_s: Signal| { cs_clone.fetch_add(1, Ordering::SeqCst); };
        let ce = Arc::new(AtomicUsize::new(0));
        let ce_clone = Arc::clone(&ce);
        let mut emit_fn = move |_id: i64, _text: &str| { ce_clone.fetch_add(1, Ordering::SeqCst); };

        let fired = run_reminder_tick(&conn, now, &mut notify_fn, &mut apply_fn, &mut emit_fn);
        assert!(fired.is_empty());
        assert_eq!(cn.load(Ordering::SeqCst), 0);
        assert_eq!(cs.load(Ordering::SeqCst), 0);
        assert_eq!(ce.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn compute_next_fire_works() {
        let now = Utc::now();
        let next = compute_next_fire(now, 30, "minutes");
        assert!(next > now + Duration::minutes(29));
        assert!(next < now + Duration::minutes(31));

        let next = compute_next_fire(now, 2, "hours");
        assert!(next > now + Duration::minutes(119));
        assert!(next < now + Duration::minutes(121));

        let next = compute_next_fire(now, 1, "days");
        assert!(next > now + Duration::hours(23));
        assert!(next < now + Duration::hours(25));

        let next = compute_next_fire(now, 1, "weeks");
        assert!(next > now + Duration::days(6));
        assert!(next < now + Duration::days(8));
    }
}
