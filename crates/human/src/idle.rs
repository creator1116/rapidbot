//! What a person does when left standing for a long time.
//!
//! Not vanilla, and not fitted to a recording either: the intervals below are
//! a guess at how often someone shifts their weight, looks around, or
//! straightens up while they wait. What they model is the *shape* of the
//! behaviour: nothing at all for the first few seconds, gestures that get
//! further apart the longer the wait goes on, and each one returning to where
//! it started, so a bot that is idle for an hour is not still facing the
//! corner it wandered into ten minutes in.
//!
//! [`Idler::advance`] is the whole interface: call it with the time since the
//! last call and it returns the next thing to do, if anything. The caller
//! owns the mouse and the keys, so nothing here talks to a server.
//!
//! ```
//! use rapidbot_human::idle::{IdleAction, Idler};
//!
//! let mut idler = Idler::new(1);
//! let mut gestures = 0;
//! for _ in 0..(60 * 20) {
//!     match idler.advance(1.0 / 20.0) {
//!         // A caller with a `TickContext` turns the view or holds sneak here.
//!         Some(IdleAction::Glance { yaw, pitch }) => {
//!             let _ = (yaw, pitch);
//!             gestures += 1;
//!         }
//!         Some(IdleAction::Sneak(true)) => gestures += 1,
//!         Some(IdleAction::Return) | Some(IdleAction::Sneak(false)) | None => {}
//!     }
//! }
//! assert!(gestures > 0, "a minute of standing is not perfectly still");
//! ```

use crate::Noise;

/// Something a person does while standing around.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IdleAction {
    /// Look somewhere else: an offset in degrees from the view at the moment
    /// the glance starts. The idler sends [`IdleAction::Return`] when it is
    /// time to look back.
    Glance { yaw: f32, pitch: f32 },
    /// Look back where they were.
    Return,
    /// Shift their weight: hold the sneak key, or let it go.
    Sneak(bool),
}

/// A gesture with a scheduled end.
#[derive(Debug, Clone, Copy)]
enum Pending {
    Glance { left: f64 },
    Sneak { left: f64 },
}

/// Decides when a standing bot does something, and what.
pub struct Idler {
    noise: Noise,
    /// Seconds since the bot last did anything of its own.
    idle_for: f64,
    /// Seconds until the next gesture.
    next_in: f64,
    /// A gesture still running, and how long it has left.
    pending: Option<Pending>,
    /// How many gestures have been started, for tests and diagnostics.
    gestures: u64,
}

impl Idler {
    /// The first gesture waits this long, whatever the seed: someone who has
    /// just stopped does not immediately start fidgeting.
    pub const FIRST_GESTURE_MIN: f64 = 6.0;
    pub const FIRST_GESTURE_MAX: f64 = 20.0;

    pub fn new(seed: u64) -> Self {
        let mut noise = Noise::new(seed ^ 0x1d1e_5eed_1d1e_5eed);
        let next_in = noise.uniform(Self::FIRST_GESTURE_MIN, Self::FIRST_GESTURE_MAX);
        Self {
            noise,
            idle_for: 0.0,
            next_in,
            pending: None,
            gestures: 0,
        }
    }

    /// Seconds this bot has been standing around.
    pub fn idle_for(&self) -> f64 {
        self.idle_for
    }

    /// How many gestures have started so far.
    pub fn gestures(&self) -> u64 {
        self.gestures
    }

    /// Whether a gesture is still running (a glance held, or a weight shift).
    pub fn is_returning(&self) -> bool {
        self.pending.is_some()
    }

    /// The bot did something itself, or its owner did: the idle clock starts
    /// again and any gesture in progress is dropped, so the caller's view is
    /// never pulled away by a look-back that no longer applies.
    pub fn activity(&mut self) {
        self.idle_for = 0.0;
        self.next_in = self
            .noise
            .uniform(Self::FIRST_GESTURE_MIN, Self::FIRST_GESTURE_MAX);
        self.pending = None;
    }

    /// Advances by `dt` seconds and returns the next thing to do, if any.
    pub fn advance(&mut self, dt: f64) -> Option<IdleAction> {
        self.idle_for += dt;
        if let Some(pending) = &mut self.pending {
            let left = match pending {
                Pending::Glance { left } | Pending::Sneak { left } => left,
            };
            *left -= dt;
            if *left > 0.0 {
                return None;
            }
            let finished = self.pending.take().expect("just matched");
            return Some(match finished {
                Pending::Glance { .. } => IdleAction::Return,
                Pending::Sneak { .. } => IdleAction::Sneak(false),
            });
        }

        self.next_in -= dt;
        if self.next_in > 0.0 {
            return None;
        }
        // Whoever has been waiting longer moves less often, on top of the
        // short-term spread.
        let settled = (self.idle_for / 60.0).min(20.0);
        self.next_in = self.noise.uniform(4.0, 30.0) + settled * 1.5;
        self.gestures += 1;

        let roll = self.noise.uniform(0.0, 1.0);
        if roll < 0.55 {
            // Look around: mostly sideways, sometimes up or down at nothing.
            let yaw = self.noise.uniform(-50.0, 50.0) as f32;
            let pitch = self.noise.uniform(-12.0, 12.0) as f32;
            let hold = self.noise.uniform(0.4, 2.5);
            self.pending = Some(Pending::Glance { left: hold });
            Some(IdleAction::Glance { yaw, pitch })
        } else if roll < 0.85 {
            let hold = self.noise.uniform(0.2, 1.2);
            self.pending = Some(Pending::Sneak { left: hold });
            Some(IdleAction::Sneak(true))
        } else {
            // A quick glance at something close, no weight shift: a nod or a
            // look at their own feet.
            let pitch = self.noise.uniform(-25.0, 20.0) as f32;
            let hold = self.noise.uniform(0.2, 0.8);
            self.pending = Some(Pending::Glance { left: hold });
            Some(IdleAction::Glance { yaw: 0.0, pitch })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f64 = 1.0 / 20.0;

    /// Seconds of simulated time to a list of actions.
    fn run(seed: u64, seconds: f64) -> Vec<IdleAction> {
        run_timed(seed, seconds)
            .into_iter()
            .map(|(_, action)| action)
            .collect()
    }

    /// The same, with the time each action happened at.
    fn run_timed(seed: u64, seconds: f64) -> Vec<(f64, IdleAction)> {
        let mut idler = Idler::new(seed);
        let mut out = Vec::new();
        let mut elapsed = 0.0;
        while elapsed < seconds {
            if let Some(action) = idler.advance(DT) {
                out.push((elapsed, action));
            }
            elapsed += DT;
        }
        out
    }

    #[test]
    fn nothing_happens_for_the_first_few_seconds() {
        // Whatever the seed, the first gesture waits at least the minimum.
        for seed in 0..64 {
            let mut idler = Idler::new(seed);
            let mut elapsed = 0.0;
            while elapsed < Idler::FIRST_GESTURE_MIN {
                assert_eq!(
                    idler.advance(DT),
                    None,
                    "seed {seed} moved after {elapsed} seconds"
                );
                elapsed += DT;
            }
        }
    }

    #[test]
    fn gestures_are_spread_out_and_always_end() {
        let actions = run(3, 600.0);
        assert!(actions.len() >= 4, "ten minutes: {:?}", actions.len());
        // Every gesture that starts is undone later, and the actions alternate
        // between starting and returning.
        let mut open = 0i32;
        for action in &actions {
            match action {
                IdleAction::Glance { .. } | IdleAction::Sneak(true) => open += 1,
                IdleAction::Return | IdleAction::Sneak(false) => open -= 1,
            }
            assert!(open >= 0, "an action ended before it started");
        }
        assert_eq!(open, 0, "everything came back");
    }

    #[test]
    fn a_longer_wait_means_fewer_gestures() {
        let first_minute = run(5, 60.0).len();
        let last_minute = run(5, 3600.0).len() - run(5, 3540.0).len();
        assert!(
            last_minute <= first_minute,
            "an hour in: {last_minute} gestures vs {first_minute} in the first minute"
        );
    }

    #[test]
    fn a_glance_is_held_before_it_returns() {
        let timed = run_timed(9, 120.0);
        let (start, _) = timed
            .iter()
            .find(|(_, action)| matches!(action, IdleAction::Glance { .. }))
            .expect("a glance in two minutes");
        let (back, _) = timed
            .iter()
            .find(|(at, action)| at > start && matches!(action, IdleAction::Return))
            .expect("a return after it");
        let held = back - start;
        assert!((0.4..=3.0).contains(&held), "held for {held} seconds");
    }

    #[test]
    fn activity_resets_the_clock_and_drops_a_gesture() {
        let mut idler = Idler::new(4);
        let mut started = false;
        let mut elapsed = 0.0;
        while elapsed < 60.0 && !started {
            if let Some(action) = idler.advance(DT) {
                assert!(
                    matches!(action, IdleAction::Glance { .. } | IdleAction::Sneak(true)),
                    "a gesture starts before it ends: {action:?}"
                );
                started = true;
            }
            elapsed += DT;
        }
        assert!(started, "something happened in a minute");
        assert!(idler.is_returning(), "and it was still running");

        idler.activity();
        assert_eq!(idler.idle_for(), 0.0);
        assert!(!idler.is_returning(), "the look-back was dropped");
        // Nothing happens again immediately.
        let mut elapsed = 0.0;
        while elapsed < Idler::FIRST_GESTURE_MIN {
            assert_eq!(idler.advance(DT), None);
            elapsed += DT;
        }
    }

    #[test]
    fn deterministic_per_seed() {
        assert_eq!(run(7, 300.0), run(7, 300.0));
        assert_ne!(run(7, 300.0), run(8, 300.0));
    }
}
