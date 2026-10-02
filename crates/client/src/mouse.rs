//! `MouseHandler.turnPlayer`: how mouse counts become rotation.

use rapidbot_physics::Player;

/// `options.sensitivity` (0 to 1, default 0.5: "100%").
#[derive(Debug, Clone, Copy)]
pub struct MouseSettings {
    pub sensitivity: f64,
}

impl Default for MouseSettings {
    fn default() -> Self {
        Self { sensitivity: 0.5 }
    }
}

impl MouseSettings {
    /// `ss = sensitivity * 0.6F + 0.2F; sens = ss³ * 8`.
    fn scale(&self) -> f64 {
        let ss = self.sensitivity * 0.6f32 as f64 + 0.2f32 as f64;
        ss * ss * ss * 8.0
    }

    /// Degrees one mouse count turns the view by (`Entity.turn` multiplies
    /// by 0.15F). 0.15 at the default sensitivity.
    pub fn degrees_per_count(&self) -> f64 {
        self.scale() * 0.15f32 as f64
    }

    /// Applies a frame's accumulated mouse movement to the player.
    pub fn turn_player(&self, player: &mut Player, dx: i32, dy: i32) {
        let sens = self.scale();
        player.turn(dx as f64 * sens, dy as f64 * sens);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_sensitivity_steps() {
        let s = MouseSettings::default();
        assert!((s.degrees_per_count() - 0.15).abs() < 1e-6);
        let mut p = Player::new();
        p.y_rot = 0.0;
        s.turn_player(&mut p, 10, -4);
        // (float)(10 * sens) * 0.15F, with sens just above 1.
        assert_eq!(p.y_rot, (10.0 * s.scale()) as f32 * 0.15);
        assert_eq!(p.x_rot, (-4.0 * s.scale()) as f32 * 0.15);
        // Pitch clamps, yaw does not wrap.
        s.turn_player(&mut p, 4000, 4000);
        assert_eq!(p.x_rot, 90.0);
        assert!(p.y_rot > 360.0);
    }
}
