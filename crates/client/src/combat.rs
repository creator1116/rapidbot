//! Not vanilla: how a person fights. Keeps the crosshair on a target,
//! closes the distance, and clicks when the attack charge is back, with a
//! person's timing rather than a machine's.
//!
//! Everything goes through the same hands as any other controller: the
//! mouse model turns the view, the finger model clicks, and what the click
//! hits is whatever the crosshair is on that tick.

use rapidbot_human::Noise;
use rapidbot_physics::Keys;
use rapidbot_physics::math::Vec3;

use crate::controller::TickContext;

/// What [`Fighter::tick`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FightStatus {
    /// No target, or it is gone.
    Idle,
    /// Too far to hit: walking up to it.
    Approaching,
    /// In reach, lining up or waiting for the charge.
    Engaged,
    /// Asked for a click this tick.
    Swung,
}

pub struct Fighter {
    noise: Noise,
    target: Option<i32>,
    /// Where on the target's body the aim rests, 0 feet to 1 head; people
    /// drift around the chest, they do not track one exact point.
    aim_height: f64,
    reroll_in: u32,
    /// Ticks left before the click, once the crosshair is on the target
    /// and the charge is enough.
    swing_in: Option<u32>,
    /// The charge this swing waits for: most wait for the full bar, some
    /// swings come a little early.
    wanted_charge: f32,
    /// How close (horizontally, eye to the target's axis) to walk before
    /// letting go of forward.
    pub keep_distance: f64,
    /// Walk up to the target when it is out of reach.
    pub approach: bool,
    /// Sprint while approaching.
    pub sprint: bool,
    /// Circle the target while in reach, rather than standing still.
    pub strafe: bool,
    /// Back away when the target is closer than `keep_distance`.
    pub retreat: bool,
    /// Which way to circle: -1 left, 1 right.
    strafe_direction: f32,
    ticks_until_strafe_change: u32,
}

impl Fighter {
    pub fn new(seed: u64) -> Self {
        Self {
            noise: Noise::new(seed ^ 0x5eed_f167),
            target: None,
            aim_height: 0.7,
            reroll_in: 0,
            swing_in: None,
            wanted_charge: 1.0,
            keep_distance: 1.9,
            approach: true,
            sprint: true,
            strafe: true,
            retreat: true,
            strafe_direction: 1.0,
            ticks_until_strafe_change: 0,
        }
    }

    /// The entity to fight, or `None` to stop.
    pub fn set_target(&mut self, id: Option<i32>) {
        if self.target != id {
            self.target = id;
            self.swing_in = None;
            self.ticks_until_strafe_change = 0;
        }
    }

    pub fn target(&self) -> Option<i32> {
        self.target
    }

    fn next_swing(&mut self) {
        self.swing_in = None;
        self.wanted_charge = if self.noise.chance(0.85) {
            1.0
        } else {
            self.noise.uniform(0.8, 1.0) as f32
        };
    }

    /// The movement keys for a target `horizontal` blocks away (eye to
    /// body axis): approach when far, circle while in reach, and back off
    /// when the target gets too close.
    fn movement_keys(&self, horizontal: f64) -> Keys {
        let forward = self.approach && horizontal > self.keep_distance;
        let backward = self.retreat && horizontal < self.keep_distance * 0.65;
        let circle = self.strafe && horizontal <= self.keep_distance && !backward;
        Keys {
            forward,
            backward,
            left: circle && self.strafe_direction < 0.0,
            right: circle && self.strafe_direction > 0.0,
            sprint: forward && self.sprint,
            ..Keys::default()
        }
    }

    /// One tick of fighting. Sets the aim and the movement keys, and asks
    /// for a click when the time is right.
    pub fn tick(&mut self, ctx: &mut TickContext<'_>) -> FightStatus {
        let Some(entity) = self.target.and_then(|id| ctx.entities.get(id)) else {
            if self.target.take().is_some() {
                ctx.set_keys(Keys::default());
                ctx.stop_aiming();
            }
            return FightStatus::Idle;
        };
        let id = entity.id;
        if self.reroll_in == 0 {
            self.aim_height = self.noise.gaussian(0.68, 0.1).clamp(0.4, 0.92);
            self.reroll_in = (self.noise.lognormal(1.6, 0.5) * 20.0) as u32;
        } else {
            self.reroll_in -= 1;
        }
        let (min, max) = entity.bounding_box();
        let body = Vec3::new(
            entity.pos.x,
            min[1] + (max[1] - min[1]) * self.aim_height,
            entity.pos.z,
        );
        let eye = ctx.player.eye_position();
        let horizontal = ((body.x - eye.x).powi(2) + (body.z - eye.z).powi(2)).sqrt();
        // A point the crosshair can actually land on, if there is one.
        let reachable = ctx.entity_aim_point(id, self.aim_height);
        ctx.aim_at(reachable.unwrap_or(body), 1.5);

        if self.strafe {
            if self.ticks_until_strafe_change == 0 {
                self.strafe_direction = if self.noise.chance(0.5) { -1.0 } else { 1.0 };
                self.ticks_until_strafe_change = (self.noise.uniform(0.8, 2.2) * 20.0) as u32;
            } else {
                self.ticks_until_strafe_change -= 1;
            }
        }

        if self.approach || self.strafe || self.retreat {
            ctx.set_keys(self.movement_keys(horizontal));
        }

        let on_target = ctx.target.is_some_and(|hit| hit.id == id);
        if !on_target {
            self.swing_in = None;
            return if reachable.is_some() {
                FightStatus::Engaged
            } else {
                FightStatus::Approaching
            };
        }
        if ctx.attack_strength < self.wanted_charge {
            return FightStatus::Engaged;
        }
        match self.swing_in {
            None => {
                // Noticing the bar is back and the aim is good.
                let seconds = self.noise.lognormal(0.09, 0.45).clamp(0.0, 0.6);
                self.swing_in = Some((seconds * 20.0).round() as u32);
                FightStatus::Engaged
            }
            Some(0) => {
                ctx.click_attack();
                self.next_swing();
                FightStatus::Swung
            }
            Some(ticks) => {
                self.swing_in = Some(ticks - 1);
                FightStatus::Engaged
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fights_at_a_distance_with_strafing_and_retreat() {
        let mut fighter = Fighter::new(1);
        fighter.strafe = true;
        fighter.retreat = true;
        fighter.strafe_direction = 1.0;
        fighter.keep_distance = 2.0;

        let far = fighter.movement_keys(4.0);
        assert!(far.forward && far.sprint);
        assert!(!far.left && !far.right && !far.backward);

        let in_reach = fighter.movement_keys(2.0);
        assert!(!in_reach.forward && !in_reach.backward);
        assert!(in_reach.right && !in_reach.left);

        fighter.strafe_direction = -1.0;
        let other_way = fighter.movement_keys(2.0);
        assert!(other_way.left && !other_way.right);

        let close = fighter.movement_keys(1.0);
        assert!(close.backward && !close.forward);
        assert!(!close.left && !close.right);
    }
}
