//! Desktop-pet state machine (Task 5).
//!
//! A pure-Rust, dependency-free finite state machine that derives the pet's
//! high-level pose ([`PetState`]) from a stream of [`Signal`]s produced
//! elsewhere in the app. Task 5 builds and unit-tests this in isolation; it
//! is **not** wired into the Tauri app yet — Task 7 subscribes pet rendering
//! to [`StateMachine::current`] (emitted as a `pet_state` event) and Task 16
//! emits [`Signal::ReminderDue`] from the reminder scheduler. The names of
//! every `pub` item in this file are the stable contract those tasks
//! consume; do not rename or re-shape them without coordinating.
//!
//! ## State graph (informal)
//!
//! ```text
//!                ┌──────────────────────────────────────────────┐
//!                │                                              │
//!   new() ──▶ Standby ──IdleFor(≥SLEEP_AFTER)──▶ Sleep        │
//!              │  ▲▲                                   │        │
//!              │  ││                                   │ short  │
//!              │  │└─ UserActivity (only if Sleep) ────┘ idle   │
//!              │  └── IdleFor(short) (only if Sleep) ───┘        │
//!              │                                                │
//!              ├ AgentBusy ──▶ Thinking (sticky vs idle/activity)│
//!              ├ AgentDone ──▶ HasSomething                     │
//!              ├ ReminderDue ▶ HasSomething                     │
//!              ├ Error ──────▶ NeedsYou (sticky vs idle/panel)  │
//!              └ NeedsInput ▶ NeedsYou                         │
//!                                                              │
//!   HasSomething ── PanelOpened ──▶ Standby                    │
//!   NeedsYou     ── Dismissed   ──▶ Standby (or Sleep if focus)│
//!                                                              │
//!   FocusMode(true)  ──▶ Sleep (suppress ambient), any state   │
//!   FocusMode(false) ──▶ resume (Sleep if last idle long, else │
//!                        Standby)                              │
//! └──────────────────────────────────────────────────────────────┘
//! ```
//!
//! The rules below resolve the brief's open ambiguity (e.g. whether
//! `Thinking`/`NeedsYou` are cleared by idle / user-activity) toward the
//! least-surprising reading of the design doc's "`!`/`?` 清除规则":
//! sticky states stay until the explicit clearing signal arrives.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Idle threshold that transitions the pet from `Standby` to `Sleep`.
///
/// 5 minutes — matches the design doc ("睡眠——长时间无任务"). Tunable:
/// Task 7 / 16 may expose this through a settings command later without
/// touching the [`StateMachine`] logic.
pub const SLEEP_AFTER: Duration = Duration::from_secs(5 * 60);

/// The pet's high-level pose. Each variant maps 1:1 to a Task 7 animation
/// set; the pet window subscribes to [`StateMachine::current`] and switches
/// its CSS state class on change.
///
/// Variants are ordered loosely by "escalation" for readability, not for
/// `PartialOrd` semantics (the derive is `Eq`, not `Ord`).
///
/// `Serialize` is derived (Task 7) so the tick loop can emit the current
/// state to the pet webview via `app.emit_to("pet", "pet_state", state)`;
/// serde serializes each unit variant as its bare name string (`"Sleep"`,
/// `"Standby"`, etc.), which the frontend maps to a CSS state class.
/// `Deserialize` is derived for symmetry (future tasks may round-trip the
/// state through a command boundary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PetState {
    /// Asleep: dimmed, head down, `Zzz`. Static frame — no animation loop
    /// (idle CPU ≈ 0). Reached after [`SLEEP_AFTER`] of inactivity or when
    /// [`Signal::FocusMode`]`(true)` suppresses the pet.
    Sleep,
    /// Awake but idle. Occasional blink / glance. The default state out of
    /// [`StateMachine::new`].
    Standby,
    /// Agent is running: gear spins or `...` bounces. Sticky against idle
    /// and user-activity — only [`Signal::AgentDone`] / [`Signal::Error`]
    /// / [`Signal::FocusMode`] clear it.
    Thinking,
    /// Something happened (agent done, reminder due). Pet perks up, shows
    /// `!`. Auto-cleared by [`Signal::PanelOpened`] (the user opened the
    /// panel and saw it) — and conceptually by a Task 7 timer, though that
    /// timer is not part of this state machine's surface.
    HasSomething,
    /// The pet needs the user's attention (error / waiting for input).
    /// Shows `?`. Sticky against everything except [`Signal::Dismissed`]
    /// — it never auto-clears, by design ("它的意思就是"需要你关注"").
    NeedsYou,
}

/// Input signal that drives the state machine. Each variant corresponds to
/// a discrete event raised elsewhere in the app:
///
/// | Variant | Raised by |
/// |---|---|
/// | [`IdleFor`](Self::IdleFor) | Task 7 idle poller (`GetLastInputInfo`) |
/// | [`AgentBusy`](Self::AgentBusy) / [`AgentDone`](Self::AgentDone) | agent provider layer (Task 9+) |
/// | [`ReminderDue`](Self::ReminderDue) | reminder scheduler (Task 16) |
/// | [`Error`](Self::Error) / [`NeedsInput`](Self::NeedsInput) | agent / reminder layer |
/// | [`UserActivity`](Self::UserActivity) | Task 7 activity hook (keyboard / mouse) |
/// | [`FocusMode`](Self::FocusMode) | tray toggle / focus-window scheduler |
/// | [`PanelOpened`](Self::PanelOpened) | panel window `show` (Task 4 wiring) |
/// | [`Dismissed`](Self::Dismissed) | frontend "dismiss" button on a `?` badge |
///
/// `Copy` is sound: the largest variant is `IdleFor(Duration)` = 16 bytes,
/// and every `apply` consumes a `Signal` by value anyway.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Signal {
    /// OS-level idle time. `d >= `[`SLEEP_AFTER`] transitions `Standby` →
    /// `Sleep`; a short `d` while `Sleep` wakes the pet to `Standby`. A
    /// short `d` while in a sticky state (`Thinking` / `NeedsYou`) leaves
    /// the state unchanged.
    IdleFor(Duration),
    /// The agent started working (stream opened, first token pending).
    AgentBusy,
    /// The agent finished (stream closed, final result emitted).
    AgentDone,
    /// A reminder fired. Surfaces as `HasSomething` so the pet perks up
    /// and shows `!`; cleared when the panel is opened.
    ReminderDue,
    /// An agent / reminder / config error occurred. Escalates to
    /// `NeedsYou` even in focus mode — "需要你" is critical by design.
    Error,
    /// The agent is waiting for the user to confirm / pick an option.
    /// Escalates to `NeedsYou` even in focus mode.
    NeedsInput,
    /// A keyboard / mouse event was observed. Wakes a sleeping pet
    /// (`Sleep` → `Standby`); otherwise a no-op at the `PetState` level
    /// (the transient follow-activity glance is a Task 7 animation
    /// concern, not a state change).
    UserActivity,
    /// Focus / do-not-disturb toggle. `true` forces `Sleep` and suppresses
    /// ambient signals (idle / activity / agent-lifecycle / reminder)
    /// while held; `Error` / `NeedsInput` still surface. `false` resumes:
    /// recompute from the last seen idle (`Sleep` if long, else `Standby`).
    FocusMode(bool),
    /// The panel window was just shown. Clears `HasSomething` (the user
    /// saw it) but **never** clears `NeedsYou` (that needs an explicit
    /// `Dismissed`).
    PanelOpened,
    /// The user dismissed the current `?`. Clears `NeedsYou` → `Standby`
    /// (or `Sleep` if focus is on or the last idle was long).
    Dismissed,
    /// The Task 7 auto-clear timer fired (5s after entering `HasSomething`
    /// without a [`Signal::PanelOpened`]). Clears `HasSomething` →
    /// `Standby`; no-op for every other state. The timer only runs while
    /// the machine is in `HasSomething`, but the arm is defensive: if it
    /// fires late (after a transition already moved the state), it
    /// doesn't clobber the new state.
    HasSomethingExpired,
}

/// The desktop pet state machine. Holds the current [`PetState`], the
/// focus flag, and the last observed idle duration (so `FocusMode(false)`
/// can resume correctly).
///
/// Construct with [`StateMachine::new`] (starts in `Standby`, focus off,
/// idle zero). Drive it by feeding [`Signal`]s to [`StateMachine::apply`];
/// each call returns the new `current()` so a Tauri event emitter can
/// diff-and-emit only on change (Task 7's concern, not enforced here).
#[derive(Debug, Clone)]
pub struct StateMachine {
    /// Latest pose. Read via [`Self::current`].
    current: PetState,
    /// True while focus / do-not-disturb is on. Suppresses ambient signals
    /// and forces `Sleep` when first turned on.
    focus: bool,
    /// Most recent idle duration seen via [`Signal::IdleFor`]. Used only by
    /// [`Signal::FocusMode`]`(false)` to decide whether to resume asleep
    /// or awake. Defaults to zero (i.e. "no long idle seen yet").
    last_idle: Duration,
}

impl Default for StateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl StateMachine {
    /// Create a fresh machine in `Standby`, focus off, idle zero.
    pub fn new() -> Self {
        Self {
            current: PetState::Standby,
            focus: false,
            last_idle: Duration::ZERO,
        }
    }

    /// The current pose. Read-only — Task 7 emits this on every change.
    pub fn current(&self) -> PetState {
        self.current
    }

    /// Apply `signal`, update internal state, and return the new
    /// [`PetState`] (always equal to the post-call [`Self::current`]).
    ///
    /// See the module docs and the [`Signal`] variants for the exact
    /// transition rules. The short version: `FocusMode(true)` always wins;
    /// `Thinking` / `NeedsYou` are sticky against idle / activity;
    /// `PanelOpened` clears `HasSomething` only; `Dismissed` clears
    /// `NeedsYou` only.
    pub fn apply(&mut self, signal: Signal) -> PetState {
        match signal {
            // Focus precedence: FocusMode(true) forces Sleep regardless of
            // current state. FocusMode(false) resumes by recomputing from
            // the last seen idle.
            Signal::FocusMode(true) => {
                self.focus = true;
                self.current = PetState::Sleep;
            }
            Signal::FocusMode(false) => {
                self.focus = false;
                self.current = if self.last_idle >= SLEEP_AFTER {
                    PetState::Sleep
                } else {
                    PetState::Standby
                };
            }

            // Critical escalations — Error and NeedsInput surface even in
            // focus mode. "需要你" means the user must pay attention, so
            // do-not-disturb does not swallow it.
            Signal::Error | Signal::NeedsInput => {
                self.current = PetState::NeedsYou;
            }

            // Ambient / agent-lifecycle signals are suppressed while focus
            // is on — the pet stays asleep.
            Signal::AgentBusy => {
                if !self.focus {
                    self.current = PetState::Thinking;
                }
            }
            Signal::AgentDone | Signal::ReminderDue => {
                if !self.focus {
                    self.current = PetState::HasSomething;
                }
            }

            // PanelOpened clears HasSomething → Standby. It does NOT clear
            // Thinking or NeedsYou (those take priority). Under focus the
            // current state is never HasSomething (focus suppresses the
            // transitions that would set it), so the `!self.focus` guard
            // is mostly defensive.
            Signal::PanelOpened => {
                if !self.focus && self.current == PetState::HasSomething {
                    self.current = PetState::Standby;
                }
            }

            // Dismissed clears NeedsYou → Standby, or Sleep if focus is on
            // or the last idle was long. No-op for non-NeedsYou states.
            Signal::Dismissed => {
                if self.current == PetState::NeedsYou {
                    self.current = if self.focus || self.last_idle >= SLEEP_AFTER {
                        PetState::Sleep
                    } else {
                        PetState::Standby
                    };
                }
            }

            // HasSomethingExpired: the Task 7 auto-clear timer (5s TTL in
            // brain.rs) fired. Clears HasSomething → Standby only; no-op
            // for every other state (the timer only runs while in
            // HasSomething, but defensive against a late fire after a
            // transition). Under focus the current state is never
            // HasSomething, so the `!self.focus` guard is defensive.
            Signal::HasSomethingExpired => {
                if !self.focus && self.current == PetState::HasSomething {
                    self.current = PetState::Standby;
                }
            }

            // IdleFor: always remember the latest duration (used on
            // FocusMode(false) resume), then transition — but only if not
            // in a sticky state and not under focus.
            Signal::IdleFor(d) => {
                self.last_idle = d;
                if !self.focus
                    && self.current != PetState::Thinking
                    && self.current != PetState::NeedsYou
                {
                    if d >= SLEEP_AFTER {
                        self.current = PetState::Sleep;
                    } else if self.current == PetState::Sleep {
                        self.current = PetState::Standby;
                    }
                    // else (Standby / HasSomething, short idle): unchanged.
                }
            }

            // UserActivity: wake from Sleep only. No-op for any other state
            // (the transient follow-activity animation is a Task 7 concern,
            // not a PetState). Sticky states stay sticky.
            Signal::UserActivity => {
                if !self.focus && self.current == PetState::Sleep {
                    self.current = PetState::Standby;
                }
            }
        }
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn focus_forces_sleep() {
        let mut m = StateMachine::new();
        m.apply(Signal::AgentBusy);
        assert_eq!(m.apply(Signal::FocusMode(true)), PetState::Sleep);
    }

    #[test]
    fn needs_you_persists_until_dismissed() {
        let mut m = StateMachine::new();
        m.apply(Signal::NeedsInput);
        assert_eq!(m.current(), PetState::NeedsYou);
        m.apply(Signal::PanelOpened);
        assert_eq!(m.current(), PetState::NeedsYou);
        assert_eq!(m.apply(Signal::Dismissed), PetState::Standby);
    }

    #[test]
    fn has_something_clears_on_panel_open() {
        let mut m = StateMachine::new();
        m.apply(Signal::ReminderDue);
        assert_eq!(m.current(), PetState::HasSomething);
        assert_eq!(m.apply(Signal::PanelOpened), PetState::Standby);
    }

    // Additional coverage beyond the brief's three.

    #[test]
    fn agent_busy_then_done() {
        let mut m = StateMachine::new();
        assert_eq!(m.apply(Signal::AgentBusy), PetState::Thinking);
        assert_eq!(m.apply(Signal::AgentDone), PetState::HasSomething);
    }

    #[test]
    fn idle_long_sleeps_short_wakes() {
        let mut m = StateMachine::new();
        assert_eq!(
            m.apply(Signal::IdleFor(Duration::from_secs(6 * 60))),
            PetState::Sleep
        );
        assert_eq!(
            m.apply(Signal::IdleFor(Duration::from_secs(10))),
            PetState::Standby
        );
    }

    #[test]
    fn dismissed_clears_needs_you() {
        let mut m = StateMachine::new();
        m.apply(Signal::Error);
        assert_eq!(m.current(), PetState::NeedsYou);
        assert_eq!(m.apply(Signal::Dismissed), PetState::Standby);
    }

    // Task 7: HasSomethingExpired auto-clears HasSomething → Standby,
    // and is a no-op for every other state (including under focus).
    #[test]
    fn has_something_expired_clears_to_standby() {
        let mut m = StateMachine::new();
        // Enter HasSomething via ReminderDue.
        m.apply(Signal::ReminderDue);
        assert_eq!(m.current(), PetState::HasSomething);
        // Auto-clear fires → Standby.
        assert_eq!(m.apply(Signal::HasSomethingExpired), PetState::Standby);
        // No-op when not in HasSomething.
        assert_eq!(m.apply(Signal::HasSomethingExpired), PetState::Standby);
        // No-op for other non-HasSomething states: Thinking.
        m.apply(Signal::AgentBusy);
        assert_eq!(m.current(), PetState::Thinking);
        assert_eq!(m.apply(Signal::HasSomethingExpired), PetState::Thinking);
        // No-op under focus (focus forces Sleep; HasSomethingExpired
        // doesn't override it).
        m.apply(Signal::FocusMode(true));
        assert_eq!(m.current(), PetState::Sleep);
        assert_eq!(m.apply(Signal::HasSomethingExpired), PetState::Sleep);
    }

    // Task 7 (Review Focus #1): 20 consecutive IdleFor(SLEEP_AFTER+1)
    // ticks drive the machine to Sleep and it stays there. The "no
    // animation" half of sleep=static is enforced structurally — the
    // frontend sleep CSS has no keyframe, and the tick loop emits
    // `pet_state` only on change (so zero events while asleep). This
    // test asserts the state-machine half: after 20 long-idle ticks
    // the machine is in Sleep and did not wander.
    #[test]
    fn sleep_is_static_after_idle_ticks() {
        let mut m = StateMachine::new();
        let long_idle = SLEEP_AFTER + Duration::from_secs(1);
        for _ in 0..20 {
            m.apply(Signal::IdleFor(long_idle));
        }
        assert_eq!(m.current(), PetState::Sleep);
    }
}
