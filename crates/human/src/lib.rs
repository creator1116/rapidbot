//! Models of a person at the keyboard and mouse.
//!
//! Vanilla-exactness (the other crates) makes a bot look like the vanilla
//! *client*. This crate is about looking like a *person* using it: nothing
//! here is ported from Minecraft, it is motor-control modelling.
//!
//! Everything is driven by a seeded RNG so a bot has a stable "personality"
//! and tests are reproducible.

pub mod fatigue;
pub mod keyboard;
pub mod mouse;
pub mod typing;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

pub use fatigue::Fatigue;
pub use keyboard::Keyboard;
pub use mouse::{Aim, MouseModel, MouseProfile};
pub use typing::Typist;

/// Random source with the distributions the models need.
pub struct Noise {
    rng: StdRng,
}

impl Noise {
    pub fn new(seed: u64) -> Self {
        Self { rng: StdRng::seed_from_u64(seed) }
    }

    pub fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        self.rng.gen_range(lo..hi)
    }

    pub fn chance(&mut self, p: f64) -> bool {
        self.rng.gen_bool(p.clamp(0.0, 1.0))
    }

    /// Standard normal (Box-Muller).
    pub fn normal(&mut self) -> f64 {
        let u1: f64 = self.rng.gen_range(f64::MIN_POSITIVE..1.0);
        let u2: f64 = self.rng.gen_range(0.0..1.0);
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    pub fn gaussian(&mut self, mean: f64, sd: f64) -> f64 {
        mean + sd * self.normal()
    }

    /// Log-normal with the given median: reaction times and durations are
    /// right-skewed, never negative.
    pub fn lognormal(&mut self, median: f64, sigma: f64) -> f64 {
        median * (sigma * self.normal()).exp()
    }

    /// Time until the next event of a Poisson process with this mean gap.
    pub fn exponential(&mut self, mean: f64) -> f64 {
        -mean * self.rng.gen_range(f64::MIN_POSITIVE..1.0f64).ln()
    }
}

/// A human reaction time in seconds: log-normal around `median`, slower when
/// tired, never below what nerves and muscles allow.
pub fn reaction_time(noise: &mut Noise, median: f64, sigma: f64, fatigue: f64) -> f64 {
    (noise.lognormal(median, sigma) * (1.0 + 0.4 * fatigue)).max(0.12)
}
