//! Pet brain — the tick loop that drives the state machine from live
//! OS signals (idle time, follow-activity throttle) and emits state
//! changes + activity nudges to the pet webview (Task 7).
//!
//! Spawned from `lib.rs::setup` as a dedicated `std::thread` (matching
//! the click-through poller's model in `window.rs`). Ticks every ~1s:
//! reads `activity::idle_since()`, applies `Signal::IdleFor` to the
//! [`StateMachine`], runs the [`HasSomething`] auto-clear timer, checks
//! the [`FollowActivityThrottle`], and emits `pet_state` (only on
//! change) + `pet_activity_nudge` (when the throttle permits) to the
//! `"pet"` window via `app.emit_to`.
//!
//! ## Sleep = static (Review Focus #1)
//!
//! When the pet is asleep (`PetState::Sleep`), this loop still ticks
//! (it must, to detect wake-up via a short `IdleFor`), but it emits
//! **no animation events**:
//!
//! - `pet_state` only fires when `current()` changes — zero events while
//!   the pet stays asleep.
//! - `pet_activity_nudge` is suppressed by the throttle itself:
//!   `FollowActivityThrottle::should_react` returns `None` when
//!   `last_input_idle > AWAY_THRESHOLD` (3min), and `SLEEP_AFTER` is
//!   5min, so by the time the pet is asleep the throttle already
//!   suppresses nudges. The frontend's sleep CSS has no keyframe either
//!   (see `src/pet/states.css`), so the webview paints nothing — idle
//!   CPU ≈ 0.
//!
//! ## What is NOT wired here (Tasks 13/16)
//!
//! - `Signal::AgentBusy` / `Signal::AgentDone` — the agent provider
//!   layer (Task 9+) will emit these; for now the agent is never busy.
//! - `Signal::ReminderDue` — the reminder scheduler (Task 16) will emit
//!   these; for now no reminders fire.
//! - `Signal::Error` / `Signal::NeedsInput` — same.
//! - `Signal::FocusMode(bool)` — the tray toggle / focus-window
//!   scheduler (Task 15/16) will emit this; for now focus is always off.
//! - `Signal::PanelOpened` — the panel window `show` path (Task 4)
//!   could emit this, but it's not wired here yet; the `HasSomething`
//!   auto-clear timer (5s) is the fallback clearance path.
//!
//! The dev-only `dev_trigger_state` Tauri command (in `lib.rs`) can
//! inject `ReminderDue` / `Error` / `AgentBusy` / etc. manually for
//! verification of the `!`/`?`/thinking states without waiting for the
//! real wiring.
//!
//! [`HasSomething`]: crate::state::machine::PetState::HasSomething

use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::activity;
use crate::state::machine::{PetState, Signal, StateMachine};
use crate::timers::FollowActivityThrottle;
use crate::window;

/// Tick interval. ~1Hz — fine-grained enough for the 5-min sleep
/// transition and the 5s `HasSomething` auto-clear, cheap enough that
/// idle CPU stays ~0 (a `GetLastInputInfo` + state-machine `apply` is
/// microseconds). Matches the brief's "周期（如 1s）".
const TICK_INTERVAL: Duration = Duration::from_secs(1);

/// `HasSomething` auto-clears back to `Standby` after this duration if
/// the user hasn't opened the panel. 5s — enough to register the
/// perk-up without lingering (the `!` state is transient by design).
/// See the brief's "auto-clear timer" for `HasSomethingExpired`.
const HAS_SOMETHING_TTL: Duration = Duration::from_secs(5);

/// Minimum spacing between follow-activity reactions (design doc:
/// ≥15min, configurable, with optional random up to 2×min). The
/// throttle's `max` is `None` → upper bound = 2×min = 30min.
const FOLLOW_ACTIVITY_MIN: Duration = Duration::from_secs(15 * 60);

/// The bundled pet brain: [`StateMachine`] + [`FollowActivityThrottle`]
/// + the `HasSomething` auto-clear timer + last-emitted state, behind a
/// single [`Mutex`] so the tick task grabs one lock per tick (no
/// per-field lock contention).
///
/// Construct with [`PetBrain::new`] (starts in `Standby`, throttle
/// armed so the first reaction is ≥ `FOLLOW_ACTIVITY_MIN` after `now`).
/// Managed by Tauri as `Mutex<PetBrain>` (see [`BrainState`]).
pub struct PetBrain {
    /// The state machine. Driven by `Signal::IdleFor` each tick (and
    /// by the `HasSomething` auto-clear arm when the TTL expires).
    machine: StateMachine,
    /// Follow-activity reaction throttle. `should_react` →
    /// `on_reacted` → `emit pet_activity_nudge`.
    throttle: FollowActivityThrottle,
    /// `Instant` at which the current `HasSomething` state began, for
    /// the auto-clear timer. `None` whenever `current()` is not
    /// `HasSomething` (set on entry, cleared on exit, checked each
    /// tick against `HAS_SOMETHING_TTL`).
    has_since: Option<Instant>,
    /// Last state emitted to the frontend. Diffed against `current()`
    /// each tick so `pet_state` only fires on change — no event flood
    /// while the state is stable (and zero events while asleep).
    last_emitted: PetState,
}

impl PetBrain {
    /// Create a fresh brain at `now`. Starts in `Standby`, focus off,
    /// idle zero, throttle armed (`next_allowed = now + min`).
    pub fn new(now: Instant) -> Self {
        Self {
            machine: StateMachine::new(),
            throttle: FollowActivityThrottle::new(
                FOLLOW_ACTIVITY_MIN,
                None, // max = 2 * min = 30min
                now,
                // Pseudo-random in [0, 1) from the current Unix
                // nanosecond timestamp. NOT cryptographic — adequate
                // for spacing follow-activity reactions (the brief
                // allows any float in [0,1]). Avoids adding `rand`
                // as a dependency for this one closure.
                Box::new(|| {
                    use std::time::SystemTime;
                    let nanos = SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0);
                    (nanos % 1000) as f64 / 1000.0
                }),
            ),
            has_since: None,
            last_emitted: PetState::Standby,
        }
    }

    /// The current pose. Read-only — callers lock the brain and read
    /// this for diagnostics; the tick loop emits it on change. Currently
    /// unused (the tick loop reads `current()` via the local `guard`),
    /// but kept `pub` for future diagnostics / Tauri commands that may
    /// query the brain's state.
    #[allow(dead_code)]
    pub fn current(&self) -> PetState {
        self.machine.current()
    }

    /// Apply a signal to the state machine and mark the resulting state
    /// as "already emitted" (so the tick loop doesn't re-emit it on the
    /// next tick). Used by the `dev_trigger_state` Tauri command in
    /// `lib.rs` for manual state triggering (verification of the
    /// `!`/`?`/thinking poses). Returns the new current state.
    ///
    /// This does NOT run the `HasSomething` auto-clear timer — the tick
    /// loop handles that on its next tick (if the dev signal entered
    /// `HasSomething`, the next tick starts the 5s TTL).
    pub(crate) fn apply_and_stage_emit(&mut self, signal: Signal) -> PetState {
        let new_state = self.machine.apply(signal);
        self.last_emitted = new_state;
        new_state
    }
}

/// Managed Tauri state: a `Mutex`-wrapped [`PetBrain`]. Held via
/// `app.manage(Mutex::new(PetBrain::new(now)))` in `lib.rs::setup`,
/// retrieved in the tick thread via `app.state::<BrainState>()` and
/// in the `dev_trigger_state` command via the same path.
///
/// `Mutex<PetBrain>` is `Send + Sync + 'static` because `PetBrain` is
/// `Send` (its `FollowActivityThrottle` holds a `Box<dyn FnMut + Send>`,
/// which is `Send` but not `Sync` — exactly what `Mutex` requires).
pub type BrainState = Mutex<PetBrain>;

/// Run one brain tick. Pure of I/O except for the two `emit_to` calls
/// (best-effort — errors are logged to stderr, not propagated, so a
/// transient emit failure doesn't crash the tick loop).
///
/// Steps (in order):
/// 1. Read `idle = activity::idle_since()`.
/// 2. `machine.apply(Signal::IdleFor(idle))` — re-derives from the
///    latest signal each tick (Task 5 carry: focus/idle transitions
///    self-correct).
/// 3. `HasSomething` auto-clear: if `current() == HasSomething`,
///    start the timer (if not started); if the timer has elapsed
///    `HAS_SOMETHING_TTL`, apply `Signal::HasSomethingExpired`. If
///    the state left `HasSomething` (via `PanelOpened` etc.), clear
///    the timer.
/// 4. `FollowActivityThrottle`: if `should_react(now, idle)` →
///    `on_reacted(now)` + emit `pet_activity_nudge`.
/// 5. Diff `current()` against `last_emitted`; if changed, emit
///    `pet_state` and update `last_emitted`.
///
/// Returns the post-tick `current()` for testability / diagnostics.
pub fn tick<R: Runtime>(app: &AppHandle<R>, brain: &BrainState, now: Instant) -> PetState {
    let idle = activity::idle_since();

    // Decide everything under the lock, then emit outside so we don't
    // hold the mutex across the IPC boundary (emit_to may re-enter
    // Tauri state and could deadlock on a contended lock).
    let (current, state_changed, fire_nudge) = {
        let mut guard = brain.lock().expect("PetBrain mutex poisoned");

        // 1. IdleFor each tick — re-derives from the latest signal.
        guard.machine.apply(Signal::IdleFor(idle));

        // 2. HasSomething auto-clear.
        if guard.machine.current() == PetState::HasSomething {
            if guard.has_since.is_none() {
                guard.has_since = Some(now);
            } else if let Some(since) = guard.has_since {
                if now.duration_since(since) >= HAS_SOMETHING_TTL {
                    guard.machine.apply(Signal::HasSomethingExpired);
                    guard.has_since = None;
                }
            }
        } else {
            // State left HasSomething (via PanelOpened, idle→Sleep,
            // etc.) — reset the timer so a future HasSomething
            // starts the TTL fresh.
            guard.has_since = None;
        }

        // 3. Follow-activity nudge. The throttle suppresses while
        //    idle > AWAY_THRESHOLD (3min), so by the time the pet is
        //    asleep (5min) nudges are already off — sleep = quiet.
        let fire_nudge = guard.throttle.should_react(now, idle).is_some();
        if fire_nudge {
            guard.throttle.on_reacted(now);
        }

        let current = guard.machine.current();
        let state_changed = current != guard.last_emitted;
        if state_changed {
            guard.last_emitted = current;
        }
        (current, state_changed, fire_nudge)
    };

    // Emit outside the lock (best-effort; errors → stderr, not panic).
    if fire_nudge {
        if let Err(e) = app.emit_to("pet", "pet_activity_nudge", ()) {
            eprintln!("[pet] emit pet_activity_nudge failed: {e}");
        }
    }
    if state_changed {
        if let Err(e) = app.emit_to("pet", "pet_state", current) {
            eprintln!("[pet] emit pet_state failed: {e}");
        }
    }

    current
}

/// Spawn the brain tick loop as a named `std::thread`. Runs for the
/// lifetime of the process. Gated on pet visibility: while the pet is
/// hidden (`pet_is_visible() == false`), the loop skips its work
/// (no `idle_since` read, no state machine apply, no emit) and just
/// sleeps at [`TICK_INTERVAL`] cadence until the pet reappears — same
/// pattern as the click-through poller in `window.rs`.
///
/// Named `pet-brain-tick` so it is identifiable in a debugger / process
/// listing.
pub fn spawn_brain_tick<R: Runtime + 'static>(app: AppHandle<R>) {
    if let Err(e) = thread::Builder::new()
        .name("pet-brain-tick".to_string())
        .spawn(move || {
            loop {
                thread::sleep(TICK_INTERVAL);
                // Gate on pet visibility — no work when hidden.
                if !window::pet_is_visible() {
                    continue;
                }
                // `app.state::<BrainState>()` returns a `State<'_,
                // Mutex<PetBrain>>` which derefs to `&Mutex<PetBrain>`;
                // `&*state` recovers the `&Mutex<PetBrain>` the tick
                // function expects. The state table entry lives as long
                // as the app (behind an Arc), so the reference is valid
                // for the entire thread.
                let brain = &*app.state::<BrainState>();
                let _ = tick(&app, brain, Instant::now());
            }
        })
    {
        eprintln!("[pet] failed to spawn pet-brain-tick thread: {e}");
    }
}
