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

use std::sync::{Arc, Mutex};
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
    /// Also keeps the `HasSomething` auto-clear timer (`has_since`) in
    /// sync with the result (fix round 1): when the machine *enters*
    /// `HasSomething` here, the TTL clock starts immediately rather than
    /// on the next 1s tick — otherwise the observed auto-clear window
    /// was 5–6s (up to 1s lost waiting for the tick to notice the entry).
    /// When the result leaves `HasSomething`, the timer is cleared so a
    /// future entry starts the TTL fresh. The tick loop's own `has_since`
    /// handling is unchanged and remains the steady-state path.
    pub(crate) fn apply_and_stage_emit(&mut self, signal: Signal) -> PetState {
        let new_state = self.machine.apply(signal);
        if new_state == PetState::HasSomething {
            // Start the TTL clock immediately on entry so the ~5s
            // auto-clear is measured from the actual transition, not
            // from the next tick's `Instant::now()`. Only set when
            // unset (matches the tick loop's "start if None" rule — a
            // re-trigger while already in HasSomething does not restart
            // the clock).
            if self.has_since.is_none() {
                self.has_since = Some(Instant::now());
            }
        } else {
            // State left HasSomething (or was never in it) — clear the
            // timer so the next entry starts the TTL fresh.
            self.has_since = None;
        }
        self.last_emitted = new_state;
        new_state
    }

    /// Apply a signal to the state machine WITHOUT staging an emit.
    /// Used by Task 16 paths that want the tick loop to observe + emit
    /// the new state on its next 1s tick (rather than emitting it
    /// immediately from outside the tick loop):
    ///
    /// - The reminder scheduler applies `Signal::ReminderDue` here so
    ///   the brain enters `HasSomething`; the tick loop emits
    ///   `pet_state` on its next tick (1s latency is fine for a `!`
    ///   alert — the toast already gave the immediate feedback).
    /// - The focus / 勿扰 toggle from `config.focus.enabled` applies
    ///   `Signal::FocusMode(bool)` here on startup + on settings
    ///   change; the tick loop emits the resulting `Sleep`/`Standby`.
    ///
    /// The tick loop's `last_emitted` diff logic handles the emit: when
    /// `current()` differs from `last_emitted` on the next tick, it
    /// emits + advances. So this helper just runs the transition.
    ///
    /// Also keeps `has_since` in sync (same rule as
    /// [`Self::apply_and_stage_emit`]): start the TTL clock on
    /// `HasSomething` entry, clear it on exit.
    pub fn apply_signal(&mut self, signal: Signal) -> PetState {
        let new_state = self.machine.apply(signal);
        if new_state == PetState::HasSomething {
            if self.has_since.is_none() {
                self.has_since = Some(Instant::now());
            }
        } else {
            self.has_since = None;
        }
        // NOTE: do NOT advance `last_emitted` here — let the tick loop
        // observe the diff and emit. If we advanced, a transition
        // applied between ticks would never be emitted (the diff would
        // be zero on the next tick).
        new_state
    }
}

/// Managed Tauri state: an `Arc<Mutex>`-wrapped [`PetBrain`]. Held via
/// `app.manage(Arc::new(Mutex::new(PetBrain::new(now))))` in
/// `lib.rs::setup`, retrieved in the tick thread via
/// `app.state::<BrainState>()` and in the `dev_trigger_state` /
/// `dismiss_pet_alert` / `apply_focus_to_brain` commands via the same
/// path.
///
/// `Arc<Mutex<PetBrain>>` (Task 16 wrap): the reminder scheduler's
/// spawned tokio task needs to OWN a brain handle (it outlives any
/// single Tauri command call — the task runs for the process's
/// lifetime). The previous `Mutex<PetBrain>` wasn't `Clone`, so the
/// spawn couldn't get its own handle without re-managing. Wrapping in
/// `Arc` lets `lib.rs::setup` clone the Arc into the spawn AND keep
/// the same single `Mutex<PetBrain>` (one brain, two holders). The
/// `State<'_, Arc<Mutex<PetBrain>>>` from `app.state` derefs through
/// `Arc` to `&Mutex<PetBrain>` transparently, so the existing
/// `brain.lock()` call sites in `tick` / `dev_trigger_state` /
/// `dismiss_pet_alert` / `apply_focus_to_brain` work unchanged.
///
/// `Arc<Mutex<PetBrain>>` is `Send + Sync + 'static` because
/// `PetBrain` is `Send` (its `FollowActivityThrottle` holds a
/// `Box<dyn FnMut + Send>`, which is `Send` but not `Sync` — exactly
/// what `Mutex` requires).
pub type BrainState = Arc<Mutex<PetBrain>>;

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
        // NOTE (fix round 1): `last_emitted` is NOT advanced here. It is
        // only updated after `emit_to` succeeds (below). If we set it
        // under the lock before emitting, a failed `emit_to` (logged,
        // swallowed) would leave `last_emitted == current`, so the next
        // tick would see no diff and never re-attempt — the frontend
        // would be stuck at the pre-change state with no retry path.
        (current, state_changed, fire_nudge)
    };

    // Emit outside the lock (best-effort; errors → stderr, not panic).
    if fire_nudge {
        if let Err(e) = app.emit_to("pet", "pet_activity_nudge", ()) {
            eprintln!("[pet] emit pet_activity_nudge failed: {e}");
        }
    }
    if state_changed {
        match app.emit_to("pet", "pet_state", current) {
            Ok(_) => {
                // Only record the emission as complete after success —
                // a failed emit leaves `last_emitted` at the prior value
                // so the next tick re-attempts (diff still != 0). On
                // success we re-lock and advance `last_emitted`. If the
                // lock is poisoned (e.g. a panic in another thread
                // mid-tick) we skip the update rather than crashing the
                // tick loop — the worst case is one redundant re-emit on
                // the next tick, which is harmless.
                if let Ok(mut guard) = brain.lock() {
                    guard.last_emitted = current;
                }
            }
            Err(e) => {
                eprintln!("[pet] emit pet_state failed: {e}");
            }
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
