//! Fatigue and moment-to-moment arousal.

use crate::Noise;

/// How tired the player is, from 0 (fresh) to 1. Builds over about an hour
/// of activity and recovers over about ten minutes of rest. A slow random
/// "arousal" wander is layered on top so performance is never constant.
#[derive(Debug, Clone)]
pub struct Fatigue {
    level: f64,
    arousal: f64,
    /// Seconds of activity to approach full fatigue.
    pub build_time: f64,
    /// Seconds of rest to recover.
    pub recovery_time: f64,
}

impl Default for Fatigue {
    fn default() -> Self {
        Self { level: 0.0, arousal: 0.0, build_time: 3600.0, recovery_time: 600.0 }
    }
}

impl Fatigue {
    pub fn update(&mut self, dt: f64, active: bool, noise: &mut Noise) {
        if active {
            self.level += dt / self.build_time * (1.0 - self.level);
        } else {
            self.level -= dt / self.recovery_time * self.level;
        }
        self.level = self.level.clamp(0.0, 1.0);
        // Ornstein-Uhlenbeck wander: drifts over a couple of minutes.
        self.arousal += -self.arousal * dt / 120.0 + 0.02 * dt.sqrt() * noise.normal();
        self.arousal = self.arousal.clamp(-0.25, 0.25);
    }

    pub fn level(&self) -> f64 {
        self.level
    }

    /// Multiplier on movement speed: tired and low-arousal means slower.
    pub fn speed_factor(&self) -> f64 {
        (1.0 + self.arousal) / (1.0 + 0.35 * self.level)
    }

    /// Multiplier on motor noise.
    pub fn noise_factor(&self) -> f64 {
        1.0 + self.level
    }

    #[cfg(test)]
    pub(crate) fn set_level(&mut self, level: f64) {
        self.level = level;
    }
}
