//! What drives the bot: the stand-in for a person at the keyboard.

use rapidbot_human::Aim;
use rapidbot_physics::math::{self, Vec3};
use rapidbot_physics::{Keys, Player};
use rapidbot_world::World;

use crate::checks::CheckEvent;

/// Called once per client tick at the point where vanilla handles key
/// bindings: after the connection tick, before the player moves. Only
/// called once the client has loaded into the world (until then vanilla
/// shows the loading screen, which swallows input).
pub trait Controller: Send + 'static {
    fn tick(&mut self, ctx: &mut TickContext<'_>);
}

/// The bot's view of the game for one tick.
pub struct TickContext<'a> {
    pub player: &'a mut Player,
    pub world: &'a World,
    /// Ticks since the client loaded.
    pub tick: u64,
    /// Things that happened since the last tick that look like a server or
    /// staff member testing for a macro. See [`crate::checks`].
    pub events: &'a [CheckEvent],
    /// Overall concern from recent events, 0 to 1, fading over minutes.
    pub suspicion: f32,
    /// Other players within 32 blocks, nearest first.
    pub players: &'a [crate::entities::NearbyPlayer],
    /// True while the chat box is open (a line is being typed): keys and
    /// mouse do nothing until it is sent.
    pub chat_open: bool,
    /// What the player carries and wears, and any container screen the
    /// server has open. While one is open, keys and mouse do nothing.
    pub inventory: &'a crate::inventory::Inventory,
    /// Every entity the server has shown us, where the client has them.
    pub entities: &'a crate::entities::Entities,
    /// What the crosshair is on this tick (`Minecraft.hitResult`): a block
    /// within reach, or a miss (also when an entity is in front).
    pub crosshair: rapidbot_world::raycast::BlockHit,
    /// The entity the crosshair is on, within reach: what a click attacks.
    pub target: Option<crate::entities::EntityHit>,
    /// The attack charge a click now would hit with, 0 to 1
    /// (`getAttackStrengthScale(0.5)`): above 0.9 is a full-strength hit.
    pub attack_strength: f32,
    /// The block being broken and its progress, 0 to 1.
    pub breaking: Option<((i32, i32, i32), f32)>,
    pub(crate) own_id: i32,
    pub(crate) tags: &'a rapidbot_world::tags::Tags,
    pub(crate) actions: &'a mut Actions,
    pub(crate) aim: &'a mut Option<Aim>,
    pub(crate) keys: &'a mut Keys,
    pub(crate) chat: &'a mut std::collections::VecDeque<String>,
}

/// Things the controller asked for this tick that the hands carry out.
#[derive(Debug, Default)]
pub(crate) struct Actions {
    pub select_slot: Option<u8>,
    pub attack: Option<bool>,
    pub click: bool,
    pub close_container: bool,
}

impl TickContext<'_> {
    /// Selects a hotbar slot (0 to 8) by its number key. The finger gets
    /// there after a moment, and the server hears of it on the tick after
    /// that, as with the vanilla client.
    pub fn select_slot(&mut self, slot: u8) {
        if slot < 9 {
            self.actions.select_slot = Some(slot);
        }
    }

    /// Presses or lets go of the attack button (left mouse). Held on a
    /// block it digs, as in the game: whatever is under the crosshair, so
    /// aim first. The finger follows with human latency.
    pub fn hold_attack(&mut self, down: bool) {
        self.actions.attack = Some(down);
    }

    /// One click of the attack button: attacks the entity under the
    /// crosshair ([`TickContext::target`]), or swings at nothing. The finger
    /// presses within a frame or two and lets go a moment later; a click
    /// asked for while one is under way is ignored.
    pub fn click_attack(&mut self) {
        self.actions.click = true;
    }

    /// A point to aim at for the crosshair to land on an entity, `height`
    /// of the way up its box (0 feet, 1 top). `None` when it cannot be hit
    /// from here: too far, or something in the way.
    pub fn entity_aim_point(&self, id: i32, height: f64) -> Option<Vec3> {
        crate::interact::entity_aim_point(self.player, self.world, self.entities, self.own_id, self.tags, id, height)
    }

    /// Closes the container screen the server opened (Escape), after the
    /// moment it takes a person to see it and react.
    pub fn close_container(&mut self) {
        self.actions.close_container = true;
    }

    /// Types a line into chat: a message, or a command if it starts with
    /// `/`. Typing takes a person's time, and the player stands still
    /// while the chat box is open. Lines queue up.
    pub fn chat(&mut self, line: impl Into<String>) {
        self.chat.push_back(line.into());
    }

    /// Keys the player wants held from now on. The fingers follow with
    /// human latency (a few tens of milliseconds, each key on its own), so
    /// hold an intent for a few ticks if it must register: a one-tick tap
    /// may be dropped, as a person's would.
    pub fn set_keys(&mut self, keys: Keys) {
        *self.keys = keys;
    }

    /// The keys currently wanted (not necessarily down yet).
    pub fn wanted_keys(&self) -> Keys {
        *self.keys
    }

    /// Where to look. The human mouse model turns the view there over the
    /// following frames (reaction time, a stroke, corrections). `tolerance`
    /// is how many degrees off is good enough.
    pub fn aim(&mut self, yaw: f32, pitch: f32, tolerance: f32) {
        *self.aim = Some(Aim { yaw, pitch, tolerance });
    }

    /// Aims at a point in the world, from the player's eyes.
    pub fn aim_at(&mut self, target: Vec3, tolerance: f32) {
        let (yaw, pitch) = angles_to(self.player.eye_position(), target);
        self.aim(yaw, pitch, tolerance);
    }

    /// Lets go of the aim; the hand rests (and fidgets).
    pub fn stop_aiming(&mut self) {
        *self.aim = None;
    }

    /// Sets the view direction instantly. No person turns like this: it is
    /// for tests and debugging, not for servers that watch rotation.
    pub fn snap_look(&mut self, y_rot: f32, x_rot: f32) {
        self.player.y_rot = y_rot;
        self.player.x_rot = x_rot.clamp(-90.0, 90.0);
        *self.aim = None;
    }
}

/// Yaw and pitch (Minecraft convention: yaw 0 faces +z, pitch positive down)
/// from `from` towards `to`.
pub fn angles_to(from: Vec3, to: Vec3) -> (f32, f32) {
    let (dx, dy, dz) = (to.x - from.x, to.y - from.y, to.z - from.z);
    let horizontal = (dx * dx + dz * dz).sqrt();
    let yaw = (f64::atan2(dz, dx).to_degrees() - 90.0) as f32;
    let pitch = -(f64::atan2(dy, horizontal).to_degrees()) as f32;
    (math::wrap_degrees(yaw), pitch)
}

/// Does nothing: an idle player.
pub struct Idle;

impl Controller for Idle {
    fn tick(&mut self, _: &mut TickContext<'_>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles() {
        let o = Vec3::ZERO;
        let (yaw, pitch) = angles_to(o, Vec3::new(0.0, 0.0, 5.0));
        assert!(yaw.abs() < 1e-4 && pitch.abs() < 1e-4);
        let (yaw, _) = angles_to(o, Vec3::new(-5.0, 0.0, 0.0));
        assert!((yaw - 90.0).abs() < 1e-4);
        let (_, pitch) = angles_to(o, Vec3::new(0.0, -5.0, 5.0));
        assert!((pitch - 45.0).abs() < 1e-4);
    }
}
