//! `LocalPlayer`: the bot's own player and what it reports to the server.
//! Movement itself lives in [`rapidbot_physics::Player`].

use rapidbot_physics::math::Vec3;
use rapidbot_physics::{Keys, Player};
use rapidbot_protocol::packets::play::serverbound::{
    MovePlayerPos, MovePlayerPosRot, MovePlayerRot, MovePlayerStatusOnly, MoveVehicle,
    PlayerAbilities, PlayerCommand, PlayerCommandAction, PlayerInput,
};
use rapidbot_protocol::packets::play::{
    Input, PositionAndRotation, PositionMoveRotation, move_flags, relative,
};

use crate::entities::{Entities, Entity};
use crate::math;
use crate::net::PacketSender;

/// The server's vehicle state for the local player's seat. Vehicle motion is
/// server-driven; this mirrors the last position and the attachment needed
/// for `MovePlayerRot`.
#[derive(Debug, Clone, Copy)]
struct Vehicle {
    id: i32,
    pos: Vec3,
    y_rot: f32,
    x_rot: f32,
    on_ground: bool,
    seat: usize,
    passengers: usize,
    authoritative: bool,
}

#[derive(Debug, Clone)]
pub struct LocalPlayer {
    pub id: i32,
    pub physics: Player,

    /// The direct vehicle the local player rides, if any.
    riding: Option<i32>,
    vehicle: Option<Vehicle>,

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
            riding: None,
            vehicle: None,
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

    /// `LocalPlayer.onSyncedDataUpdated` for the living flags: once the
    /// server says we are using an item, read its `UseEffects` from the
    /// hand that started the use.
    pub fn sync_use_effects(&mut self, inventory: &crate::inventory::Inventory) {
        if !self.physics.using_item {
            return;
        }
        let stack = if self.physics.using_item_offhand {
            inventory.offhand()
        } else {
            inventory.held()
        };
        let effects = stack.map(|s| s.use_effects()).unwrap_or_default();
        self.physics
            .set_use_effects(effects.can_sprint, effects.speed_multiplier);
    }

    /// `ClientboundSetPassengersPacket` for the local player: start riding
    /// the vehicle, or dismount when this packet no longer lists us.
    pub fn set_passengers(
        &mut self,
        vehicle: &Entity,
        passengers: &[i32],
        authoritative: bool,
    ) -> bool {
        let Some(seat) = passengers.iter().position(|id| *id == self.id) else {
            return false;
        };
        if self.riding != Some(vehicle.id) {
            self.riding = Some(vehicle.id);
            // A boat turns a newly mounted rider to its own yaw.
            if vehicle.info().is("AbstractBoat") {
                self.physics.y_rot = vehicle.y_rot;
            }
        }
        self.vehicle = Some(Vehicle {
            id: vehicle.id,
            pos: vehicle.pos,
            y_rot: vehicle.y_rot,
            x_rot: vehicle.x_rot,
            on_ground: vehicle.on_ground,
            seat,
            passengers: passengers.len(),
            authoritative,
        });
        true
    }

    pub fn dismount(&mut self) {
        self.riding = None;
        self.vehicle = None;
    }

    pub fn is_riding(&self) -> bool {
        self.riding.is_some()
    }

    pub fn riding_vehicle(&self) -> Option<i32> {
        self.riding
    }

    /// `Entity.positionRider` for the local player. The vehicle itself is
    /// ticked by the client entity tracker; this only keeps the rider at its
    /// seat and copies the state `sendChanges` reports.
    pub fn sync_vehicle(&mut self, entities: &Entities) {
        let Some(id) = self.riding else {
            self.vehicle = None;
            return;
        };
        let Some(entity) = entities.get(id) else {
            // The server removed the vehicle without a passengers update.
            self.dismount();
            return;
        };
        let (_, height) = entity.dimensions();
        let (seat, passengers) = self.vehicle.map_or((0, 1), |v| (v.seat, v.passengers));
        let offset = if entity.info().is("AbstractBoat") {
            let y = if entity.info().is("Boat") || entity.info().is("ChestBoat") {
                height as f64 / 3.0
            } else {
                height as f64 * 0.888_888_9
            };
            let z = if passengers > 1 {
                if seat == 0 { 0.2 } else { -0.6 }
            } else if entity.info().is("AbstractChestBoat") {
                0.15
            } else {
                0.0
            };
            Vec3::new(0.0, y, z).y_rot(-entity.y_rot.to_radians())
        } else {
            // EntityAttachments.PASSENGER falls back to AT_HEIGHT.
            Vec3::new(0.0, height as f64, 0.0)
        };
        self.vehicle = Some(Vehicle {
            id,
            pos: entity.pos,
            y_rot: entity.y_rot,
            x_rot: entity.x_rot,
            on_ground: entity.on_ground,
            seat,
            passengers,
            authoritative: self.vehicle.is_some_and(|v| v.authoritative),
        });
        self.physics.set_pos(entity.pos.plus(offset));
        // Player.tick: `if (isSpectator() || isPassenger()) setOnGround(false)`.
        // Collision flags are not touched: vanilla does not move the passenger
        // while riding, so the last values remain until dismount.
        self.physics.on_ground = false;
    }

    /// Apply `ClientboundMoveVehiclePacket` to the authoritative vehicle.
    pub fn snap_vehicle(
        &mut self,
        entities: &mut Entities,
        moving_to: PositionAndRotation,
    ) -> bool {
        let Some(vehicle) = self.vehicle else {
            return false;
        };
        if !vehicle.authoritative {
            return false;
        }
        let pos = Vec3::new(
            moving_to.position.x,
            moving_to.position.y,
            moving_to.position.z,
        );
        entities.snap_vehicle(
            vehicle.id,
            pos,
            moving_to.y_rot,
            moving_to.x_rot,
            vehicle.on_ground,
        );
        self.sync_vehicle(entities);
        true
    }

    /// The packet `handleMoveVehicle` echoes back to the server.
    pub fn serverbound_vehicle(&self) -> Option<MoveVehicle> {
        let vehicle = self.vehicle?;
        Some(MoveVehicle {
            moving_to: PositionAndRotation {
                position: rapidbot_protocol::packets::play::Vec3 {
                    x: vehicle.pos.x,
                    y: vehicle.pos.y,
                    z: vehicle.pos.z,
                },
                y_rot: vehicle.y_rot,
                x_rot: vehicle.x_rot,
            },
        })
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
        let delta = |current: f64, change: f64, flag: i32| {
            if has(flag) { current + change } else { change }
        };
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
        // `onUpdateAbilities`: creative flight was toggled.
        if self.physics.abilities_changed {
            self.physics.abilities_changed = false;
            let flags = if self.physics.abilities.flying { 2 } else { 0 };
            net.send(&PlayerAbilities { flags });
        }
        if self.physics.started_fall_flying {
            self.physics.started_fall_flying = false;
            net.send(&PlayerCommand {
                entity_id: self.id,
                action: PlayerCommandAction::StartFallFlying,
                data: 0,
            });
        }
        let keys = self.physics.key_presses;
        if self.last_sent_input != keys {
            net.send(&PlayerInput {
                input: to_input(keys),
            });
            self.last_sent_input = keys;
        }
        if self.is_riding() {
            let p = &self.physics;
            net.send(&MovePlayerRot {
                y_rot: p.y_rot,
                x_rot: p.x_rot,
                flags: move_flags(p.on_ground, p.horizontal_collision),
            });
            if self.vehicle.is_some_and(|v| v.authoritative)
                && let Some(packet) = self.serverbound_vehicle()
            {
                net.send(&packet);
                self.send_is_sprinting_if_needed(net);
            }
        } else {
            self.send_position(net);
        }
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
        let pos = rapidbot_protocol::packets::play::Vec3 {
            x: p.pos.x,
            y: p.pos.y,
            z: p.pos.z,
        };

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
            net.send(&MovePlayerPosRot {
                pos,
                y_rot: p.y_rot,
                x_rot: p.x_rot,
                flags,
            });
        } else if moved {
            net.send(&MovePlayerPos { pos, flags });
        } else if rotated {
            net.send(&MovePlayerRot {
                y_rot: p.y_rot,
                x_rot: p.x_rot,
                flags,
            });
        } else if self.last_on_ground != p.on_ground
            || self.last_horizontal_collision != p.horizontal_collision
        {
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
            let action = if sprinting {
                PlayerCommandAction::StartSprinting
            } else {
                PlayerCommandAction::StopSprinting
            };
            net.send(&PlayerCommand {
                entity_id: self.id,
                action,
                data: 0,
            });
            self.was_sprinting = sprinting;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapidbot_buf::Decode;
    use rapidbot_protocol::Packet;
    use rapidbot_protocol::packets::play::Vec3 as WireVec3;
    use rapidbot_protocol::packets::play::serverbound as sb;
    use tokio::sync::mpsc::UnboundedReceiver;

    fn change(x: f64, y: f64, z: f64, y_rot: f32, x_rot: f32) -> PositionMoveRotation {
        PositionMoveRotation {
            position: WireVec3 { x, y, z },
            delta_movement: WireVec3::default(),
            y_rot,
            x_rot,
        }
    }

    fn packets(rx: &mut UnboundedReceiver<Vec<u8>>) -> Vec<(i32, Vec<u8>)> {
        let mut out = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            let mut body = frame.as_slice();
            let id = rapidbot_buf::VarInt::decode(&mut body).unwrap().0;
            out.push((id, body.to_vec()));
        }
        out
    }

    #[test]
    fn absolute_and_relative_teleports() {
        let mut p = LocalPlayer::new(1);
        p.apply_teleport(&change(10.5, 64.0, -3.5, 90.0, 100.0), 0);
        assert_eq!(p.physics.pos, Vec3::new(10.5, 64.0, -3.5));
        assert_eq!(p.physics.y_rot, 90.0);
        assert_eq!(p.physics.x_rot, 90.0, "pitch is clamped");

        p.apply_teleport(
            &change(1.0, 0.0, 0.0, 10.0, -5.0),
            relative::X | relative::Y_ROT | relative::X_ROT,
        );
        assert_eq!(p.physics.pos, Vec3::new(11.5, 0.0, 0.0));
        assert_eq!(p.physics.y_rot, 100.0);
        assert_eq!(p.physics.x_rot, 85.0);
        // The bounding box follows the position.
        assert_eq!(p.physics.bounding_box().min_y, 0.0);
    }

    #[test]
    fn riding_sends_rot_and_vehicle() {
        let mut p = LocalPlayer::new(7);
        let mut boat = Entity::test_entity(42, "minecraft:oak_boat", Vec3::new(10.0, 64.0, 10.0));
        boat.y_rot = 30.0;
        boat.on_ground = true;
        let (_, height) = boat.dimensions();
        let mut entities = Entities::default();
        entities.insert_for_test(boat.clone());

        p.physics.y_rot = 100.0;
        assert!(p.set_passengers(&boat, &[7], true));
        assert_eq!(p.physics.y_rot, 30.0, "a new boat rider takes its yaw");
        p.sync_vehicle(&entities);
        assert!((p.physics.pos.y - (64.0 + height as f64 / 3.0)).abs() < 1e-9);
        assert!(
            !p.physics.on_ground,
            "Player.tick clears onGround when riding"
        );

        let (net, mut rx) = PacketSender::for_test();
        p.send_changes(&net);
        let sent = packets(&mut rx);
        assert_eq!(
            sent.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            [sb::MovePlayerRot::ID, sb::MoveVehicle::ID]
        );
        let rot = sb::MovePlayerRot::from_body(&sent[0].1).unwrap();
        assert_eq!((rot.y_rot, rot.x_rot), (30.0, 0.0));
        let vehicle = sb::MoveVehicle::from_body(&sent[1].1).unwrap();
        assert_eq!(
            (
                vehicle.moving_to.position.x,
                vehicle.moving_to.position.y,
                vehicle.moving_to.position.z,
                vehicle.moving_to.y_rot,
            ),
            (10.0, 64.0, 10.0, 30.0)
        );

        // A passenger who is not the controlling passenger gets no
        // MoveVehicle, but still reports rotation every tick.
        assert!(p.set_passengers(&boat, &[9, 7], false));
        p.sync_vehicle(&entities);
        let (net, mut rx) = PacketSender::for_test();
        p.send_changes(&net);
        assert_eq!(
            packets(&mut rx)
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            [sb::MovePlayerRot::ID]
        );
    }

    #[test]
    fn move_vehicle_snaps_the_authoritative_vehicle() {
        let mut p = LocalPlayer::new(7);
        let boat = Entity::test_entity(42, "minecraft:oak_boat", Vec3::new(10.0, 64.0, 10.0));
        let mut entities = Entities::default();
        entities.insert_for_test(boat.clone());
        p.set_passengers(&boat, &[7], true);

        let target = PositionAndRotation {
            position: WireVec3 {
                x: 20.0,
                y: 65.0,
                z: 20.0,
            },
            y_rot: 45.0,
            x_rot: 10.0,
        };
        assert!(p.snap_vehicle(&mut entities, target));
        let vehicle = entities.get(42).unwrap();
        assert_eq!(
            (vehicle.pos, vehicle.y_rot, vehicle.x_rot),
            (Vec3::new(20.0, 65.0, 20.0), 45.0, 10.0)
        );
        let echo = p.serverbound_vehicle().unwrap();
        assert_eq!(echo.moving_to, target);

        // Without an authoritative seat the packet is ignored.
        p.set_passengers(&boat, &[9, 7], false);
        assert!(!p.snap_vehicle(&mut entities, target));
    }

    #[test]
    fn missing_vehicle_dismounts() {
        let mut p = LocalPlayer::new(7);
        let boat = Entity::test_entity(42, "minecraft:oak_boat", Vec3::new(10.0, 64.0, 10.0));
        p.set_passengers(&boat, &[7], true);
        p.sync_vehicle(&Entities::default());
        assert!(!p.is_riding());
        assert!(p.serverbound_vehicle().is_none());
    }
}
