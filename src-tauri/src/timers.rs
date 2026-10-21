//! Follow-activity throttle (Task 6).
//!
//! Pure-Rust throttle that ensures the desktop pet's follow-activity
//! reactions (a quick glance toward the cursor when the user is active)
//! are spaced ≥ `min` apart, with an optional random extension up to
//! `max` (or `2*min` if `max` is `None`). Reactions are also suppressed
//! while the user has been idle longer than [`AWAY_THRESHOLD`] (the pet
//! shouldn't react to an idle/away user).
//!
//! Task 6 builds and unit-tests this in isolation; it is **not** wired
//! into the Tauri app yet — Task 7 instantiates a
//! [`FollowActivityThrottle`] and calls
//! [`FollowActivityThrottle::should_react`] on its activity-poll tick
//! to decide whether to fire the follow-activity animation, then
//! [`FollowActivityThrottle::on_reacted`] once the animation has been
//! triggered.
//!
//! ## Threshold formula
//!
//! On each [`FollowActivityThrottle::on_reacted`], the next-allowed
//! instant is `now + threshold` where `threshold` is sampled from
//! `[min, max.unwrap_or(2*min)]` via the `randomness` closure (which
//! returns a float in `[0.0, 1.0]`):
//!
//! ```text
//! span      = max.unwrap_or(min * 2)      // upper bound of the threshold
//! threshold = min + (span - min) * frac   // linear interp in [min, span]
//! ```
//!
//! So `frac=0.0` → `threshold = min` (deterministic minimum spacing),
//! `frac=1.0` → `threshold = span` (upper bound, either `max` or
//! `2*min`). The brief's literal one-liner (`min + span * frac`) would
//! have ranged `[min, min + span]` (i.e. `[min, 3*min]` for
//! `max=None`), contradicting both the parenthetical `next_allowed ∈
//! [now+min, now+max_or_2x]` and the test-7 assertion
//! `next_allowed <= t0 + 2*min` for `frac≈1.0`. This implementation
//! follows the stated semantics (range `[min, max_or_2x]`), which
//! matches both the parenthetical and test 7.

use std::time::{Duration, Instant};

/// Idle duration past which the user counts as "away". Follow-activity
/// reactions are suppressed while the user has been idle longer than
/// this — the pet should not react to a user who has stepped away from
/// the machine. 3 minutes (matches the design doc's "离开" boundary;
/// also strictly less than the 5-minute [`SLEEP_AFTER`] so the pet
/// settles to `Sleep` before follow-activity reactions stop).
///
/// [`SLEEP_AFTER`]: crate::state::machine::SLEEP_AFTER
pub const AWAY_THRESHOLD: Duration = Duration::from_secs(3 * 60);

/// Throttle for the pet's follow-activity reactions. Construct with
/// [`FollowActivityThrottle::new`]; drive with
/// [`FollowActivityThrottle::should_react`] (query) and
/// [`FollowActivityThrottle::on_reacted`] (record a reaction so the
/// next one is throttled).
///
/// `min` is the minimum spacing between reactions. `max`, if set, is
/// the upper bound of a random spacing chosen on each
/// [`Self::on_reacted`]; if `None`, the upper bound defaults to
/// `2*min`. The random spacing is sampled from
/// `[min, max.unwrap_or(2*min)]` using the `randomness` closure (which
/// must return a float in `[0.0, 1.0]`). Reactions are also suppressed
/// while the user is "away" (idle > [`AWAY_THRESHOLD`]).
///
/// `randomness` is `Box<dyn FnMut + Send>` so Task 7 can plug in a
/// `rand::rngs::ThreadRng` (or a deterministic closure in tests).
pub struct FollowActivityThrottle {
    /// Minimum spacing between reactions.
    min: Duration,
    /// Upper bound of the (random) spacing; `None` means `2*min`.
    max: Option<Duration>,
    /// Earliest `Instant` at which the next reaction is permitted.
    /// Initialized to `start + min` so the FIRST reaction is ≥ `min`
    /// after the throttle starts (no burst at startup); updated by
    /// `on_reacted` to `now + threshold`.
    next_allowed: Instant,
    /// `Instant` of the most recent reaction, or `None` if none has
    /// fired yet. Diagnostics / Task 7 UI may read this via
    /// [`Self::last_reaction`].
    last_reaction: Option<Instant>,
    /// Random source for the spacing extension. Returns a float in
    /// `[0.0, 1.0]`; sampled once per `on_reacted` call. `FnMut` so a
    /// real RNG can mutate its internal state.
    randomness: Box<dyn FnMut() -> f64 + Send>,
}

impl FollowActivityThrottle {
    /// Create a fresh throttle. `next_allowed = start + min` so the
    /// FIRST reaction is ≥ `min` after the throttle starts (no burst
    /// at startup). `randomness` must return a float in `[0.0, 1.0]`.
    pub fn new(
        min: Duration,
        max: Option<Duration>,
        start: Instant,
        randomness: Box<dyn FnMut() -> f64 + Send>,
    ) -> Self {
        Self {
            min,
            max,
            next_allowed: start + min,
            last_reaction: None,
            randomness,
        }
    }

    /// Decide whether to fire a follow-activity reaction at `now`,
    /// given the user's current idle duration (`last_input_idle`).
    ///
    /// Returns `Some(now)` if a reaction is permitted now, `None`
    /// otherwise. The caller is responsible for calling
    /// [`Self::on_reacted`] after actually firing the reaction so the
    /// next one is throttled.
    ///
    /// Decision rules (in order):
    /// 1. If `last_input_idle > AWAY_THRESHOLD` → `None` (user is
    ///    away; don't react to an idle user).
    /// 2. Else if `now >= self.next_allowed` → `Some(now)` (react
    ///    now).
    /// 3. Else → `None`.
    pub fn should_react(&self, now: Instant, last_input_idle: Duration) -> Option<Instant> {
        if last_input_idle > AWAY_THRESHOLD {
            // User has been away for more than AWAY_THRESHOLD — even if
            // the throttle window has expired, don't fire a follow-
            // activity reaction toward an idle user.
            None
        } else if now >= self.next_allowed {
            Some(now)
        } else {
            None
        }
    }

    /// Record that a follow-activity reaction just fired at `now`.
    /// Resets `next_allowed` to `now + threshold`, where `threshold`
    /// is sampled from `[min, max.unwrap_or(2*min)]` via `randomness`.
    /// Also stashes `now` in `last_reaction`.
    ///
    /// See the module docs for the exact threshold formula.
    pub fn on_reacted(&mut self, now: Instant) {
        // span = upper bound of the threshold = max.unwrap_or(2*min).
        let span = self.max.unwrap_or(self.min * 2);
        // Linear interpolation: threshold = min + (span - min) * frac,
        // in [min, span]. `saturating_sub` so a degenerate `max < min`
        // yields `threshold = min` (deterministic minimum) rather than
        // panicking on Duration underflow.
        let frac = (self.randomness)();
        let delta = span.saturating_sub(self.min);
        let scaled = Duration::from_secs_f64(delta.as_secs_f64() * frac);
        let threshold = self.min + scaled;
        self.next_allowed = now + threshold;
        self.last_reaction = Some(now);
    }

    /// The earliest `Instant` at which the next reaction is allowed.
    /// Mostly for diagnostics / tests; Task 7 may use it to schedule
    /// the next activity poll.
    pub fn next_allowed(&self) -> Instant {
        self.next_allowed
    }

    /// The `Instant` of the most recent reaction, or `None` if none
    /// has fired yet. Task 7 may use this for diagnostics / UI.
    pub fn last_reaction(&self) -> Option<Instant> {
        self.last_reaction
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn follow_activity_throttle_behaviors() {
        let t0 = Instant::now();
        let min = Duration::from_secs(15 * 60);
        let max = None; // random in [min, 2*min]
        // NOTE: the brief specified closure `|| 0.5` here. With the
        // semantics-correct threshold formula `min + (span - min) * frac`
        // (matching the brief's stated "[min, max.unwrap_or(min*2)]"
        // range), frac=0.5 yields threshold = 1.5*min = 22m30s, which
        // would FAIL the second assertion of behavior 5 (it expects
        // `Some` at +15m01s after the reaction, but threshold=22m30s
        // pushes `next_allowed` well past that). Trivially adjusted to
        // `|| 0.0` so behavior 5 deterministically exercises the
        // throttle minimum (15min); behavior 7 separately exercises
        // the random bounds with closures 0.0 and 0.999.
        let mut th = FollowActivityThrottle::new(min, max, t0, Box::new(|| 0.0));

        // 1. fresh, active, before min → None
        assert_eq!(th.should_react(t0, Duration::ZERO), None);
        // 2. 10min after start, active → None
        assert_eq!(
            th.should_react(t0 + Duration::from_secs(600), Duration::ZERO),
            None
        );
        // 3. 14m59s → None
        assert_eq!(
            th.should_react(t0 + Duration::from_secs(15 * 60 - 1), Duration::ZERO),
            None
        );
        // 4. 15m01s → Some(now)
        let r = th.should_react(t0 + Duration::from_secs(15 * 60 + 1), Duration::ZERO);
        assert!(r.is_some());

        // 5. react, then throttle again
        th.on_reacted(t0 + Duration::from_secs(15 * 60 + 1));
        assert_eq!(
            th.should_react(
                t0 + Duration::from_secs(15 * 60 + 1) + Duration::from_secs(600),
                Duration::ZERO
            ),
            None
        ); // +10min → None
        assert!(
            th.should_react(
                t0 + Duration::from_secs(15 * 60 + 1) + Duration::from_secs(15 * 60 + 1),
                Duration::ZERO
            )
            .is_some()
        ); // +15m01s → Some

        // 6. away → None regardless
        assert_eq!(
            th.should_react(
                t0 + Duration::from_secs(10 * 60 * 60),
                Duration::from_secs(4 * 60)
            ),
            None
        ); // idle 4min = away

        // 7. randomness bounds: with randomness=0.0 → threshold=min;
        //    =1.0(ish) → near 2*min
        let mut th0 = FollowActivityThrottle::new(min, None, t0, Box::new(|| 0.0));
        th0.on_reacted(t0);
        assert_eq!(th0.next_allowed(), t0 + min); // exactly min
        let mut th1 = FollowActivityThrottle::new(min, None, t0, Box::new(|| 0.999));
        th1.on_reacted(t0);
        // NOTE: brief wrote `t0 + 2*min`; Rust has no `u32 * Duration`
        // impl (only `Duration * u32`), so trivially reordered to
        // `min * 2` for type inference.
        assert!(th1.next_allowed() >= t0 + min && th1.next_allowed() <= t0 + min * 2);
    }
}
