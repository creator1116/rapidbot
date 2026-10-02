//! Fingers on keys.
//!
//! A decision to press or release a key does not take effect instantly or
//! on a clean boundary: each finger has its own small latency, two keys
//! "pressed together" land a few tens of milliseconds apart, and a tap has
//! a minimum duration.

use crate::Noise;

/// Number of keys modelled: the movement keys of `Input`, then the two
/// mouse buttons (attack, use).
pub const KEYS: usize = 9;

pub struct Keyboard {
    noise: Noise,
    intent: [bool; KEYS],
    actual: [bool; KEYS],
    /// Seconds until a pending change on each key lands.
    pending: [Option<f64>; KEYS],
    /// Seconds each key has been in its current state.
    held_for: [f64; KEYS],
    /// Median finger latency, seconds.
    pub latency_median: f64,
    /// Shortest time a key stays down once pressed, seconds.
    pub min_tap: f64,
}

impl Keyboard {
    pub fn new(seed: u64) -> Self {
        let mut noise = Noise::new(seed);
        Self {
            latency_median: 0.04 * noise.lognormal(1.0, 0.2),
            min_tap: 0.06 * noise.lognormal(1.0, 0.2),
            noise,
            intent: [false; KEYS],
            actual: [false; KEYS],
            pending: [None; KEYS],
            held_for: [0.0; KEYS],
        }
    }

    /// What the player wants held from now on.
    pub fn set_intent(&mut self, keys: [bool; KEYS]) {
        self.intent = keys;
    }

    pub fn intent(&self) -> [bool; KEYS] {
        self.intent
    }

    /// Advances one frame; returns the keys physically down.
    pub fn frame(&mut self, dt: f64, fatigue: f64) -> [bool; KEYS] {
        for k in 0..KEYS {
            self.held_for[k] += dt;
            if self.intent[k] == self.actual[k] {
                // Changed their mind before the finger moved.
                self.pending[k] = None;
                continue;
            }
            let remaining = match self.pending[k] {
                Some(t) => t - dt,
                None => {
                    let mut latency = self.noise.lognormal(self.latency_median, 0.45) * (1.0 + 0.4 * fatigue);
                    if self.actual[k] {
                        // A release cannot come before the tap is over.
                        latency = latency.max(self.min_tap - self.held_for[k]);
                    }
                    latency
                }
            };
            if remaining <= 0.0 {
                self.actual[k] = self.intent[k];
                self.held_for[k] = 0.0;
                self.pending[k] = None;
            } else {
                self.pending[k] = Some(remaining);
            }
        }
        self.actual
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f64 = 1.0 / 60.0;

    #[test]
    fn presses_land_late_and_not_together() {
        let mut same_frame = 0;
        for seed in 0..200 {
            let mut kb = Keyboard::new(seed);
            let mut intent = [false; KEYS];
            intent[0] = true;
            intent[6] = true;
            kb.set_intent(intent);
            let mut landed = [None; KEYS];
            for frame in 0..60 {
                let keys = kb.frame(DT, 0.0);
                for k in [0, 6] {
                    if keys[k] && landed[k].is_none() {
                        landed[k] = Some(frame);
                    }
                }
            }
            let (a, b) = (landed[0].expect("pressed"), landed[6].expect("pressed"));
            assert!(a <= 20 && b <= 20, "seed {seed}: {a} {b}");
            if a == b {
                same_frame += 1;
            }
        }
        // Two fingers rarely land in the same 16 ms frame every time.
        assert!(same_frame < 150, "{same_frame}/200 simultaneous");
        assert!(same_frame > 5, "sometimes they do: {same_frame}");
    }

    #[test]
    fn taps_have_a_minimum_length() {
        for seed in 0..50 {
            let mut kb = Keyboard::new(seed);
            let mut intent = [false; KEYS];
            intent[4] = true;
            kb.set_intent(intent);
            let mut down_frames = 0;
            for frame in 0..120 {
                // The controller asks for a one-frame tap.
                if frame == 1 {
                    kb.set_intent([false; KEYS]);
                }
                if kb.frame(DT, 0.0)[4] {
                    down_frames += 1;
                }
            }
            // Either the finger never got there, or it stayed down a while.
            assert!(down_frames == 0 || down_frames >= 2, "seed {seed}: {down_frames}");
        }
    }
}
