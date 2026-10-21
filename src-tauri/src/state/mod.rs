//! Pet state machine (Task 5).
//!
//! Pure-Rust, dependency-free state machine that derives the desktop pet's
//! current high-level pose (`PetState`) from a stream of `Signal`s produced
//! elsewhere in the app — agent lifecycle (`AgentBusy` / `AgentDone`),
//! reminders (`ReminderDue`), errors / input-wanted (`Error` /
//! `NeedsInput`), idle / activity (`IdleFor` / `UserActivity`), and focus /
//! dismissal (`FocusMode(bool)` / `PanelOpened` / `Dismissed`).
//!
//! Task 5 builds and unit-tests the machine in isolation. It is **not** wired
//! into the Tauri app yet — Task 7 subscribes pet rendering to
//! [`machine::StateMachine::current`] (emitted as a `pet_state` Tauri event)
//! and Task 16 emits [`machine::Signal::ReminderDue`] from the reminder
//! scheduler. The interface names in [`machine`] are therefore the stable
//! contract those tasks consume.
//!
//! [`machine::StateMachine::current`]: machine::StateMachine::current
//! [`machine::Signal::ReminderDue`]: machine::Signal::ReminderDue

pub mod machine;
