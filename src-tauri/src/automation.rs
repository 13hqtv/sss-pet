//! Automation: scheduled + manually-triggered agent task execution.
//!
//! An automation binds a saved agent profile + a prompt + a trigger
//! (`daily` HH:MM local, or every-N-minutes). The reminders scheduler tick
//! calls [`run_automation_tick`] each 15s; due automations dispatch an async
//! [`automation_dispatch_async`] which reuses `chat::run_chat_turn` for one
//! agent turn and records the run via the emit accumulator. See
//! docs/superpowers/specs/2026-10-26-automation-design.md.

use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone, Utc};
use rusqlite::Connection;

use crate::agent::conversation::StoredMessage;
use crate::db;

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

/// Everything `automation_dispatch_async` needs to run one agent turn without
/// re-entering the db lock: the bound agent's provider/model/cwd/prompt/
/// system_prompt/mcp_config + its enabled skills (with script files, for
/// materialization). Built under the scheduler's lock by `prepare_dispatch`.
pub struct AutomationDispatch {
    pub automation_id: String,
    pub automation_name: String,
    pub run_id: String,
    pub conv_id: String,
    pub provider: String,
    pub model: Option<String>,
    pub cwd: String,
    pub prompt: String,
    pub system_prompt: Option<String>,
    pub mcp_config: Option<String>,
    pub enabled_skills: Vec<(db::Skill, Vec<db::SkillFile>)>,
}

/// Create the run row + a 🤖-tagged first-class conversation + load the agent
/// profile and its enabled skills, returning everything
/// `automation_dispatch_async` needs to run one agent turn. Does NOT check
/// overlap or advance `next_fire_at`/`last_run_at` — callers decide those
/// (`run_automation_tick` for schedule fires, `automation_dispatch_now_prep`
/// for manual runs).
fn prepare_dispatch(
    conn: &mut Connection,
    automation: &db::Automation,
    source: &str,
    now: DateTime<Utc>,
) -> Result<(db::AutomationRun, AutomationDispatch), String> {
    let agent = db::agent_get(conn, &automation.agent_id)
        .map_err(|e| format!("agent_get: {e}"))?
        .ok_or_else(|| format!("agent {} not found", automation.agent_id))?;
    let cwd = agent.cwd.clone().unwrap_or_else(|| ".".to_string());

    // new_conversation hardcodes title="新对话"; overwrite + save again so the
    // row is distinguishable in list_conversations (🤖 prefix) + bound to the
    // agent profile for chat_send-style live reload.
    let mut conv = db::new_conversation(conn, &agent.provider, agent.model.clone(), &cwd,
        Some(automation.agent_id.clone()))
        .map_err(|e| format!("new_conversation: {e}"))?;
    conv.title = format!(
        "🤖 {} — {}",
        automation.name,
        now.with_timezone(&Local).format("%Y-%m-%d %H:%M")
    );
    db::save_conversation(conn, &conv).map_err(|e| format!("save_conversation: {e}"))?;

    let run = db::automation_run_create(conn, &automation.id, Some(&conv.id), source, "running",
        &now.to_rfc3339())
        .map_err(|e| format!("automation_run_create: {e}"))?;

    // Load enabled skills + their script files (mirrors chat_send's assembly)
    // so dispatch_async can materialize them outside the db lock.
    let skills = db::agent_enabled_skills(conn, &automation.agent_id)
        .map_err(|e| format!("agent_enabled_skills: {e}"))?;
    let mut enabled_skills = Vec::with_capacity(skills.len());
    for skill in &skills {
        let files = db::skill_file_list(conn, &skill.id)
            .map_err(|e| format!("skill_file_list: {e}"))?;
        enabled_skills.push((skill.clone(), files));
    }

    let dispatch = AutomationDispatch {
        automation_id: automation.id.clone(),
        automation_name: automation.name.clone(),
        run_id: run.id.clone(),
        conv_id: conv.id,
        provider: agent.provider.clone(),
        model: agent.model.clone(),
        cwd,
        prompt: automation.prompt.clone(),
        system_prompt: agent.system_prompt.clone(),
        mcp_config: agent.mcp_config.clone(),
        enabled_skills,
    };
    Ok((run, dispatch))
}

/// One scheduler tick: find due automations, apply the missed/overlap/
/// in-window policy, and return the dispatches for the caller to
/// `tokio::spawn`. All db work happens under the caller's lock; no LLM
/// calls, no async IO.
///
/// - **missed** (`next_fire_at < now - 5min`): advance `next_fire_at` only,
///   don't run — no catch-up for time the app was closed.
/// - **overlap** (a `running` run exists): skip silently.
/// - **in-window**: `prepare_dispatch`, then stamp `last_run_at` +
///   advance `next_fire_at`.
pub fn run_automation_tick(conn: &mut Connection, now: DateTime<Utc>) -> Vec<AutomationDispatch> {
    let now_rfc = now.to_rfc3339();
    let now_local = now.with_timezone(&Local);
    let due = match db::automation_get_due(conn, &now_rfc) {
        Ok(d) => d,
        Err(e) => {
            log::error!("[automation] get_due failed: {e}");
            return Vec::new();
        }
    };
    let mut dispatches = Vec::new();
    for auto in due {
        let nf = match auto
            .next_fire_at
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        {
            Some(t) => t.with_timezone(&Utc),
            None => continue, // get_due filters NOT NULL, so this shouldn't happen
        };
        if nf < now - Duration::minutes(5) {
            let next = compute_next_fire(&auto.trigger_type, &auto.trigger_config, now_local)
                .map(|dt| dt.with_timezone(&Utc).to_rfc3339());
            if let Err(e) = db::automation_set_next_fire(conn, &auto.id, next) {
                log::error!("[automation] set_next_fire(missed) {}: {e}", auto.id);
            }
            continue;
        }
        match db::automation_run_has_running(conn, &auto.id) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(e) => {
                log::error!("[automation] has_running {}: {e}", auto.id);
                continue;
            }
        }
        match prepare_dispatch(conn, &auto, "schedule", now) {
            Ok((_run, dispatch)) => {
                let next = compute_next_fire(&auto.trigger_type, &auto.trigger_config, now_local)
                    .map(|dt| dt.with_timezone(&Utc).to_rfc3339());
                if let Err(e) = db::automation_set_last_run(conn, &auto.id, Some(now_rfc.clone())) {
                    log::error!("[automation] set_last_run {}: {e}", auto.id);
                }
                if let Err(e) = db::automation_set_next_fire(conn, &auto.id, next) {
                    log::error!("[automation] set_next_fire {}: {e}", auto.id);
                }
                dispatches.push(dispatch);
            }
            Err(e) => log::error!("[automation] prepare_dispatch {}: {e}", auto.id),
        }
    }
    dispatches
}

/// Close out a run on `chat_result`: status=completed, finished_at=now, and
/// result_summary = the last assistant message's content (first 200 chars).
/// No assistant message → result_summary stays NULL (COALESCE preserves it).
/// Called by the emit accumulator inside `automation_dispatch_async`.
pub fn finalize_run_on_result(
    conn: &mut Connection,
    run_id: &str,
    accumulated: &[StoredMessage],
) -> rusqlite::Result<()> {
    let summary: Option<String> = accumulated
        .iter()
        .rev()
        .find(|m| m.role == "assistant")
        .map(|m| m.content.chars().take(200).collect());
    let now = chrono::Utc::now().to_rfc3339();
    db::automation_run_update(conn, run_id, "completed", Some(now), None, summary)?;
    Ok(())
}

/// Close out a run on `chat_error`: status=failed, finished_at=now, error=msg.
/// result_summary is left untouched (COALESCE preserves the prior value).
pub fn finalize_run_on_error(conn: &mut Connection, run_id: &str, error: &str) -> rusqlite::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    db::automation_run_update(conn, run_id, "failed", Some(now), Some(error.to_string()), None)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use chrono::Utc;
    use rusqlite::Connection;

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

    // -- run_automation_tick (Task 4) --

    fn fresh_db() -> (tempfile::TempDir, db::TestOverrideGuard, Connection) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let g = db::set_test_overrides(Some(tmp.path().to_path_buf()), None, None);
        let conn = db::open().expect("open");
        db::migrate(&conn).expect("migrate");
        (tmp, g, conn)
    }

    fn seed_agent(conn: &Connection) -> String {
        db::agent_create(conn, "A", None, "claude", None, None, None, None).unwrap()
    }

    #[test]
    fn run_automation_tick_in_window_creates_run_and_advances() {
        let (_t, _g, mut conn) = fresh_db();
        let aid = seed_agent(&conn);
        let a = db::automation_create(&mut conn, "n", &aid, "do it", "interval", "60", true, None).unwrap();
        let now = Utc::now();
        // next_fire 1 minute ago — inside the 5-min catch-up window
        db::automation_set_next_fire(&conn, &a.id, Some((now - Duration::minutes(1)).to_rfc3339())).unwrap();
        let d = run_automation_tick(&mut conn, now);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].automation_id, a.id);
        assert!(db::automation_run_has_running(&conn, &a.id).unwrap());
        // next_fire advanced into the future
        let a2 = db::automation_get(&conn, &a.id).unwrap().unwrap();
        let nf = DateTime::parse_from_rfc3339(a2.next_fire_at.as_deref().unwrap())
            .unwrap().with_timezone(&Utc);
        assert!(nf > now);
        assert!(a2.last_run_at.is_some());
        // conversation created + title prefixed 🤖
        let conv = db::load_conversation(&conn, &d[0].conv_id).unwrap().unwrap();
        assert!(conv.title.starts_with("🤖"));
    }

    #[test]
    fn run_automation_tick_overlap_skips() {
        let (_t, _g, mut conn) = fresh_db();
        let aid = seed_agent(&conn);
        let a = db::automation_create(&mut conn, "n", &aid, "p", "interval", "60", true, None).unwrap();
        // a schedule run already in flight
        db::automation_run_create(&mut conn, &a.id, None, "schedule", "running",
            &Utc::now().to_rfc3339()).unwrap();
        db::automation_set_next_fire(&conn, &a.id,
            Some((Utc::now() - Duration::minutes(1)).to_rfc3339())).unwrap();
        let d = run_automation_tick(&mut conn, Utc::now());
        assert!(d.is_empty());
        // still only the pre-existing run row — no new one created
        assert_eq!(db::automation_run_list(&conn, &a.id).unwrap().len(), 1);
    }

    #[test]
    fn run_automation_tick_missed_only_advances() {
        let (_t, _g, mut conn) = fresh_db();
        let aid = seed_agent(&conn);
        let a = db::automation_create(&mut conn, "n", &aid, "p", "interval", "60", true, None).unwrap();
        // next_fire 1 hour ago — outside the 5-min window → advance only, don't run
        db::automation_set_next_fire(&conn, &a.id,
            Some((Utc::now() - Duration::hours(1)).to_rfc3339())).unwrap();
        let d = run_automation_tick(&mut conn, Utc::now());
        assert!(d.is_empty());
        assert!(!db::automation_run_has_running(&conn, &a.id).unwrap());
        let a2 = db::automation_get(&conn, &a.id).unwrap().unwrap();
        let nf = DateTime::parse_from_rfc3339(a2.next_fire_at.as_deref().unwrap())
            .unwrap().with_timezone(&Utc);
        assert!(nf > Utc::now());
    }

    #[test]
    fn run_automation_tick_disabled_not_due() {
        let (_t, _g, mut conn) = fresh_db();
        let aid = seed_agent(&conn);
        let a = db::automation_create(&mut conn, "n", &aid, "p", "interval", "60", false, None).unwrap();
        db::automation_set_next_fire(&conn, &a.id,
            Some((Utc::now() - Duration::minutes(1)).to_rfc3339())).unwrap();
        let d = run_automation_tick(&mut conn, Utc::now());
        assert!(d.is_empty());
        let _ = a;
    }

    // -- finalize helpers (Task 5) --
    use crate::agent::conversation::StoredMessage;

    fn mk_msg(role: &str, content: &str) -> StoredMessage {
        StoredMessage {
            role: role.into(),
            content: content.into(),
            kind: None,
            tool: None,
            created_at: None,
        }
    }

    fn seed_running_run(conn: &mut Connection) -> String {
        let aid = seed_agent(conn);
        let a = db::automation_create(conn, "n", &aid, "p", "interval", "60", true, None).unwrap();
        db::automation_run_create(conn, &a.id, None, "schedule", "running", &Utc::now().to_rfc3339())
            .unwrap()
            .id
    }

    #[test]
    fn finalize_run_on_result_summary_from_last_assistant() {
        let (_t, _g, mut conn) = fresh_db();
        let rid = seed_running_run(&mut conn);
        let msgs = vec![mk_msg("user", "hi"), mk_msg("assistant", "这是回复，比较长的内容…")];
        finalize_run_on_result(&mut conn, &rid, &msgs).unwrap();
        let got = db::automation_run_get(&conn, &rid).unwrap().unwrap();
        assert_eq!(got.status, "completed");
        assert!(got.finished_at.is_some());
        assert!(got.result_summary.as_deref().unwrap().contains("这是回复"));
    }

    #[test]
    fn finalize_run_on_result_no_assistant_keeps_none() {
        let (_t, _g, mut conn) = fresh_db();
        let rid = seed_running_run(&mut conn);
        finalize_run_on_result(&mut conn, &rid, &[mk_msg("user", "x")]).unwrap();
        let got = db::automation_run_get(&conn, &rid).unwrap().unwrap();
        assert_eq!(got.status, "completed");
        assert!(got.result_summary.is_none());
    }

    #[test]
    fn finalize_run_on_error_sets_failed() {
        let (_t, _g, mut conn) = fresh_db();
        let rid = seed_running_run(&mut conn);
        finalize_run_on_error(&mut conn, &rid, "agent::new failed").unwrap();
        let got = db::automation_run_get(&conn, &rid).unwrap().unwrap();
        assert_eq!(got.status, "failed");
        assert_eq!(got.error.as_deref(), Some("agent::new failed"));
    }
}
