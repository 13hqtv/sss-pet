//! Automation: scheduled + manually-triggered agent task execution.
//!
//! An automation binds a saved agent profile + a prompt + a trigger
//! (`daily` HH:MM local, or every-N-minutes). The reminders scheduler tick
//! calls [`run_automation_tick`] each 15s; due automations dispatch an async
//! [`automation_dispatch_async`] which reuses `chat::run_chat_turn` for one
//! agent turn and records the run via the emit accumulator. See
//! docs/superpowers/specs/2026-10-26-automation-design.md.

use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone};

/// Compute the next fire moment in local time.
///
/// - `daily`: parse `"HH:MM"` as a local wall-clock; the next local HH:MM
///   strictly after `from` (today's slot if still future, else tomorrow's).
///   An exact-equal slot rolls to tomorrow, so the next 15s tick can't
///   immediately re-fire the same slot.
/// - `interval`: `from + N minutes` (N parsed from `trigger_config`, > 0).
///
/// Unknown trigger types or unparseable configs return `None`.
pub fn compute_next_fire(
    trigger_type: &str,
    trigger_config: &str,
    from: DateTime<Local>,
) -> Option<DateTime<Local>> {
    match trigger_type {
        "daily" => {
            let (h, m) = parse_hhmm(trigger_config)?;
            let slot_naive = from.date_naive().and_hms_opt(h, m, 0)?;
            let slot = Local.from_local_datetime(&slot_naive).single()?;
            if slot > from {
                Some(slot)
            } else {
                let tomorrow = slot_naive + Duration::days(1);
                Local.from_local_datetime(&tomorrow).single()
            }
        }
        "interval" => {
            let n: i64 = trigger_config.parse().ok()?;
            if n <= 0 {
                return None;
            }
            Some(from + Duration::minutes(n))
        }
        _ => None,
    }
}

/// Parse `"HH:MM"` (24-hour) into `(hour, minute)`. Returns `None` on a
/// missing colon, non-numeric parts, or out-of-range values (h >= 24, m >= 60).
fn parse_hhmm(s: &str) -> Option<(u32, u32)> {
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    (h < 24 && m < 60).then_some((h, m))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_dt(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        let ndt = NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap();
        Local.from_local_datetime(&ndt).single().unwrap()
    }

    #[test]
    fn compute_next_fire_daily_future_today() {
        let n = compute_next_fire("daily", "09:00", local_dt(2026, 10, 26, 8, 0)).unwrap();
        assert_eq!(n, local_dt(2026, 10, 26, 9, 0));
    }

    #[test]
    fn compute_next_fire_daily_past_rolls_to_tomorrow() {
        let n = compute_next_fire("daily", "09:00", local_dt(2026, 10, 26, 9, 30)).unwrap();
        assert_eq!(n, local_dt(2026, 10, 27, 9, 0));
    }

    #[test]
    fn compute_next_fire_daily_exact_now_rolls_to_tomorrow() {
        // strictly > from: a daily fire at exactly HH:MM:00 rolls to tomorrow,
        // so the next tick (15s later) doesn't immediately re-fire the same slot.
        let n = compute_next_fire("daily", "09:00", local_dt(2026, 10, 26, 9, 0)).unwrap();
        assert_eq!(n, local_dt(2026, 10, 27, 9, 0));
    }

    #[test]
    fn compute_next_fire_interval_adds_minutes() {
        let n = compute_next_fire("interval", "60", local_dt(2026, 10, 26, 8, 0)).unwrap();
        assert_eq!(n, local_dt(2026, 10, 26, 9, 0));
    }

    #[test]
    fn compute_next_fire_rejects_invalid() {
        let f = local_dt(2026, 10, 26, 8, 0);
        assert_eq!(compute_next_fire("weekly", "x", f), None);
        assert_eq!(compute_next_fire("daily", "25:00", f), None);
        assert_eq!(compute_next_fire("daily", "abc", f), None);
        assert_eq!(compute_next_fire("interval", "0", f), None);
        assert_eq!(compute_next_fire("interval", "-5", f), None);
        assert_eq!(compute_next_fire("interval", "abc", f), None);
    }
}
