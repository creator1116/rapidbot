//! `LocalPlayer`: the bot's own player and what it reports to the server.
//! Movement itself lives in [`rapidbot_physics::Player`].

use rapidbot_physics::math::Vec3;
use rapidbot_physics::{Keys, Player};
use rapidbot_protocol::packets::play::serverbound::{
    MovePlayerPos, MovePlayerPosRot, MovePlayerRot, MovePlayerStatusOnly, PlayerCommand, PlayerCommandAction,
    PlayerInput,
};
use rapidbot_protocol::packets::play::{Input, PositionMoveRotation, move_flags, relative};

use crate::math;
use crate::net::PacketSender;

#[derive(Debug, Clone)]
pub struct LocalPlayer {
    pub id: i32,
    pub physics: Player,

    // What the server last heard, for `sendPosition`.
    x_last: f64,
    y_last: f64,
    z_last: f64,
    y_rot_last: f32,
    x_rot_last: f32,
    last_on_ground: bool,
    last_horizontal_collision: bool,
    was_sprinting: bool,
    position_reminder: i32,
    last_sent_input: Keys,
}

fn to_input(k: Keys) -> Input {
    Input {
        forward: k.forward,
        backward: k.backward,
        left: k.left,
        right: k.right,
        jump: k.jump,
        shift: k.shift,
        sprint: k.sprint,
    }
}

impl LocalPlayer {
    /// `MultiPlayerGameMode.createPlayer` followed by what `handleLogin`
    /// does. The "last sent" fields start at zero, so the first
    /// `sendPosition` after loading always sends the position.
    pub fn new(id: i32) -> Self {
        Self {
            id,
            physics: Player::new(),
            x_last: 0.0,
            y_last: 0.0,
            z_last: 0.0,
            y_rot_last: 0.0,
            x_rot_last: 0.0,
            last_on_ground: false,
            last_horizontal_collision: false,
            was_sprinting: false,
            position_reminder: 0,
            last_sent_input: Keys::default(),
        }
    }

    /// `handleRespawn`: the replacement player. `keep` is `dataToKeep`
    /// (1 = attribute modifiers, 2 = entity data).
    pub fn respawned(old: &LocalPlayer, keep: u8) -> Self {
        let mut new = Self::new(old.id);
        let (o, p) = (&old.physics, &mut new.physics);
        p.abilities = o.abilities;
        p.spectator = o.spectator;
        p.food_level = o.food_level;
        p.sprint_window = o.sprint_window;
        if keep & 2 != 0 {
            // createPlayer(.., lastSentInput, wasSprinting = isSprinting()),
            // entity data (which carries the sprinting flag), motion, view.
            new.last_sent_input = old.last_sent_input;
            new.was_sprinting = o.sprinting;
            p.sprinting = o.sprinting;
            p.delta_movement = o.delta_movement;
            p.y_rot = o.y_rot;
            p.x_rot = o.x_rot;
        }
        // assignAllValues keeps modifiers; assignBaseValues only the bases.
        p.attributes = o.attributes.clone();
        if keep & 1 == 0 {
            p.attributes.clear_modifiers();
            p.sprinting = false;
        }
        new
    }

    /// `ClientPacketListener.setValuesFromPositionPacket` (not interpolated)
    /// using `PositionMoveRotation.calculateAbsolute`.
    pub fn apply_teleport(&mut self, change: &PositionMoveRotation, relatives: i32) {
        let p = &mut self.physics;
        let has = |flag: i32| relatives & flag != 0;
        let offset_x = if has(relative::X) { p.pos.x } else { 0.0 };
        let offset_y = if has(relative::Y) { p.pos.y } else { 0.0 };
        let offset_z = if has(relative::Z) { p.pos.z } else { 0.0 };
        let offset_y_rot = if has(relative::Y_ROT) { p.y_rot } else { 0.0 };
        let offset_x_rot = if has(relative::X_ROT) { p.x_rot } else { 0.0 };

        let position = Vec3::new(
            offset_x + change.position.x,
            offset_y + change.position.y,
            offset_z + change.position.z,
        );
        let y_rot = offset_y_rot + change.y_rot;
        let x_rot = math::clamp_f32(offset_x_rot + change.x_rot, -90.0, 90.0);

        let mut movement = p.delta_movement;
        if has(relative::ROTATE_DELTA) {
            let diff_y_rot = p.y_rot - y_rot;
            let diff_x_rot = p.x_rot - x_rot;
            movement = movement.x_rot((diff_x_rot as f64).to_radians() as f32);
            movement = movement.y_rot((diff_y_rot as f64).to_radians() as f32);
        }
        let delta = |current: f64, change: f64, flag: i32| if has(flag) { current + change } else { change };
        p.delta_movement = Vec3::new(
            delta(movement.x, change.delta_movement.x, relative::DELTA_X),
            delta(movement.y, change.delta_movement.y, relative::DELTA_Y),
            delta(movement.z, change.delta_movement.z, relative::DELTA_Z),
        );
        p.set_pos(position);
        p.y_rot = y_rot;
        p.x_rot = x_rot;
    }

    /// `LocalPlayer.sendChanges` for a player not riding anything.
    pub fn send_changes(&mut self, net: &PacketSender) {
        // Sent from aiStep, so ahead of everything sendChanges sends.
        if self.physics.started_fall_flying {
            self.physics.started_fall_flying = false;
            net.send(&PlayerCommand { entity_id: self.id, action: PlayerCommandAction::StartFallFlying, data: 0 });
        }
        let keys = self.physics.key_presses;
        if self.last_sent_input != keys {
            net.send(&PlayerInput { input: to_input(keys) });
            self.last_sent_input = keys;
        }
        self.send_position(net);
    }

    /// `LocalPlayer.sendPosition`.
    fn send_position(&mut self, net: &PacketSender) {
        self.send_is_sprinting_if_needed(net);

        let p = &self.physics;
        let dx = p.pos.x - self.x_last;
        let dy = p.pos.y - self.y_last;
        let dz = p.pos.z - self.z_last;
        // Float subtraction, then widened: exactly as `double deltaYRot = a - b`.
        let d_y_rot = (p.y_rot - self.y_rot_last) as f64;
        let d_x_rot = (p.x_rot - self.x_rot_last) as f64;
        self.position_reminder += 1;
        let moved = dx * dx + dy * dy + dz * dz > 2.0e-4 * 2.0e-4 || self.position_reminder >= 20;
        let rotated = d_y_rot != 0.0 || d_x_rot != 0.0;
        let flags = move_flags(p.on_ground, p.horizontal_collision);
        let pos = rapidbot_protocol::packets::play::Vec3 { x: p.pos.x, y: p.pos.y, z: p.pos.z };

        tracing::trace!(
            target: "rapidbot_client::moves",
            x = p.pos.x,
            y = p.pos.y,
            z = p.pos.z,
            vy = p.delta_movement.y,
            on_ground = p.on_ground,
            moved,
            "tick"
        );
        if moved && rotated {
            net.send(&MovePlayerPosRot { pos, y_rot: p.y_rot, x_rot: p.x_rot, flags });
        } else if moved {
            net.send(&MovePlayerPos { pos, flags });
        } else if rotated {
            net.send(&MovePlayerRot { y_rot: p.y_rot, x_rot: p.x_rot, flags });
        } else if self.last_on_ground != p.on_ground || self.last_horizontal_collision != p.horizontal_collision {
            net.send(&MovePlayerStatusOnly { flags });
        }

        if moved {
            self.x_last = p.pos.x;
            self.y_last = p.pos.y;
            self.z_last = p.pos.z;
            self.position_reminder = 0;
        }
        if rotated {
            self.y_rot_last = p.y_rot;
            self.x_rot_last = p.x_rot;
        }
        self.last_on_ground = p.on_ground;
        self.last_horizontal_collision = p.horizontal_collision;
    }

    /// `LocalPlayer.sendIsSprintingIfNeeded`.
    fn send_is_sprinting_if_needed(&mut self, net: &PacketSender) {
        let sprinting = self.physics.sprinting;
        if sprinting != self.was_sprinting {
            let action = if sprinting { PlayerCommandAction::StartSprinting } else { PlayerCommandAction::StopSprinting };
            net.send(&PlayerCommand { entity_id: self.id, action, data: 0 });
            self.was_sprinting = sprinting;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapidbot_protocol::packets::play::Vec3 as WireVec3;

    fn change(x: f64, y: f64, z: f64, y_rot: f32, x_rot: f32) -> PositionMoveRotation {
        PositionMoveRotation {
            position: WireVec3 { x, y, z },
            delta_movement: WireVec3::default(),
            y_rot,
            x_rot,
        }
    }

    #[test]
    fn absolute_and_relative_teleports() {
        let mut p = LocalPlayer::new(1);
        p.apply_teleport(&change(10.5, 64.0, -3.5, 90.0, 100.0), 0);
        assert_eq!(p.physics.pos, Vec3::new(10.5, 64.0, -3.5));
        assert_eq!(p.physics.y_rot, 90.0);
        assert_eq!(p.physics.x_rot, 90.0, "pitch is clamped");

        p.apply_teleport(&change(1.0, 0.0, 0.0, 10.0, -5.0), relative::X | relative::Y_ROT | relative::X_ROT);
        assert_eq!(p.physics.pos, Vec3::new(11.5, 0.0, 0.0));
        assert_eq!(p.physics.y_rot, 100.0);
        assert_eq!(p.physics.x_rot, 85.0);
        // The bounding box follows the position.
        assert_eq!(p.physics.bounding_box().min_y, 0.0);
    }
}
