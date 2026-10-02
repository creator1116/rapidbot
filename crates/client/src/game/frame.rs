//! Frame and tick timing: `DeltaTracker.Timer` and `FramerateLimitTracker`.

use std::time::{Duration, Instant};

use rand::Rng;

/// `DeltaTracker.Timer`. Kept in f32 like vanilla, so tick boundaries drift
/// the same way.
pub struct DeltaTimer {
    delta_tick_residual: f32,
    last_ms: i64,
    ms_per_tick: f32,
}

impl DeltaTimer {
    pub fn new(ticks_per_second: f32, now_ms: i64) -> Self {
        Self { delta_tick_residual: 0.0, last_ms: now_ms, ms_per_tick: 1000.0 / ticks_per_second }
    }

    /// `advanceGameTime`: ticks due since the last call.
    pub fn advance_game_time(&mut self, now_ms: i64) -> i32 {
        let delta_ticks = (now_ms - self.last_ms) as f32 / self.ms_per_tick;
        self.last_ms = now_ms;
        self.delta_tick_residual += delta_ticks;
        let ticks = self.delta_tick_residual as i32;
        self.delta_tick_residual -= ticks as f32;
        ticks
    }
}

/// The player's video settings, which set the frame rate.
#[derive(Debug, Clone, Copy)]
pub struct DisplaySettings {
    /// `options.framerateLimit`, default 120 (260 means unlimited).
    pub framerate_limit: u32,
    /// `options.enableVsync`, default on.
    pub vsync: bool,
    /// The monitor's refresh rate, which caps frames when vsync is on.
    pub refresh_rate: u32,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self { framerate_limit: 120, vsync: true, refresh_rate: 60 }
    }
}

pub struct FrameLimiter {
    settings: DisplaySettings,
    /// `latestInputTime`. Mouse and keyboard input reset it; the bot's
    /// controller will once it acts.
    latest_input_ms: i64,
}

impl FrameLimiter {
    pub fn new(settings: DisplaySettings, now_ms: i64) -> Self {
        // The player just clicked "Join Server".
        Self { settings, latest_input_ms: now_ms }
    }

    pub fn on_input_received(&mut self, now_ms: i64) {
        self.latest_input_ms = now_ms;
    }

    /// `FramerateLimitTracker.getFramerateLimit` with the default
    /// `InactivityFpsLimit.AFK`, capped by vsync.
    pub fn framerate(&self, in_menu: bool) -> u32 {
        let base = if self.settings.vsync {
            self.settings.framerate_limit.min(self.settings.refresh_rate)
        } else {
            self.settings.framerate_limit
        };
        let afk_ms = crate::clock::millis() - self.latest_input_ms;
        if afk_ms > 600_000 {
            10
        } else if afk_ms > 60_000 {
            base.min(30)
        } else if in_menu {
            60
        } else {
            base
        }
    }

    pub fn wait_for_next_frame(&self, frame_start: Instant, in_menu: bool) {
        let fps = self.framerate(in_menu).max(1);
        // Frame times are never perfectly even; a few percent of jitter.
        let jitter = rand::thread_rng().gen_range(0.97..1.03);
        let budget = Duration::from_secs_f64(jitter / fps as f64);
        if let Some(rest) = budget.checked_sub(frame_start.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_accumulates_like_vanilla() {
        let mut t = DeltaTimer::new(20.0, 0);
        assert_eq!(t.advance_game_time(49), 0);
        assert_eq!(t.advance_game_time(51), 1);
        assert_eq!(t.advance_game_time(1051), 20);
    }
}
