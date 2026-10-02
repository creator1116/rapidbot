//! Noticing when the server (or a staff member) is testing whether a person
//! is at the keyboard.
//!
//! Macro checks work by doing something to the player and watching the
//! reaction: turning the head, teleporting a few blocks, pushing with
//! velocity, swapping the held item, boxing the player in, opening a
//! screen, or simply talking to them. A macro either does not react or
//! reacts instantly and perfectly; a person notices after a moment.
//!
//! This module only *detects* and scores those events. Reacting like a
//! person is up to the mouse model (which gets disturbed) and the
//! controller (which receives the events).

use std::collections::VecDeque;

use rapidbot_physics::math::Vec3;
use tracing::warn;

#[derive(Debug, Clone, PartialEq)]
pub enum CheckKind {
    /// The server moved the player. `setback` means the destination is
    /// somewhere the player just was: an anticheat rubber-band, which says
    /// our movement was rejected.
    Teleport { distance: f64, yaw_change: f32, pitch_change: f32, setback: bool },
    /// The view was turned without moving the player (a rotation-only
    /// teleport or a `player_rotation` packet).
    ForcedRotation { yaw_change: f32, pitch_change: f32 },
    /// `player_look_at`: turned to face a point or entity.
    LookAt { yaw_change: f32, pitch_change: f32 },
    /// Velocity applied with no damage to explain it.
    Push { speed: f64 },
    /// Knockback that came with damage or an explosion (normal gameplay,
    /// but still something a person reacts to).
    Knockback { speed: f64 },
    /// The server changed the selected hotbar slot.
    HeldSlotChanged { slot: i32 },
    /// The server opened a container screen nobody asked for.
    ScreenOpened,
    /// Several blocks changed right around the player in a short time
    /// (boxed in, floor removed, obstacle placed).
    BlocksChangedNearby { count: usize },
    GameModeChanged,
    /// Flight was granted or revoked.
    FlightChanged { flying: bool },
    /// A chat, action-bar or title message that looks addressed to the
    /// player. `from` is the sender for player chat.
    Message { text: String, from: Option<String> },
    /// Another player came within range. An empty name means the entity is
    /// not in the tab list, as with anticheat "watchdog" fake players.
    PlayerNearby { name: String, distance: f64 },
    /// An inventory slot was changed by the server.
    InventoryChanged { slot: i16 },
    /// A mob effect was applied.
    EffectApplied { effect: String, amplifier: i32 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct CheckEvent {
    pub kind: CheckKind,
    /// Ticks since the client loaded.
    pub tick: u64,
    /// 0 (routine) to 1 (almost certainly a deliberate test).
    pub severity: f32,
}

/// Ticks after loading during which teleports are just the join sequence.
const SETTLE_TICKS: u64 = 40;
/// How far back to remember positions for setback detection.
const POSITION_HISTORY: usize = 60;
/// Words in a message that suggest someone is checking for a macro.
/// Players closer than this are reported when they arrive.
pub const NEARBY_RADIUS: f64 = 16.0;
const KEYWORDS: &[&str] = &["macro", "bot", "afk", "hello?", "you there", "are you", "respond", "answer"];

pub struct CheckMonitor {
    name: String,
    tick: u64,
    suspicion: f32,
    /// Events since the controller last looked.
    pending: Vec<CheckEvent>,
    positions: VecDeque<Vec3>,
    last_damage_tick: Option<u64>,
    nearby_block_changes: VecDeque<u64>,
    reported_block_burst: Option<u64>,
    /// Players currently within [`NEARBY_RADIUS`].
    nearby: std::collections::HashSet<uuid::Uuid>,
}

impl CheckMonitor {
    pub fn new(player_name: &str) -> Self {
        Self {
            name: player_name.to_lowercase(),
            tick: 0,
            suspicion: 0.0,
            pending: Vec::new(),
            positions: VecDeque::new(),
            last_damage_tick: None,
            nearby_block_changes: VecDeque::new(),
            reported_block_burst: None,
            nearby: Default::default(),
        }
    }

    /// Call once per loaded tick with the player's position.
    pub fn tick(&mut self, pos: Vec3) {
        self.tick += 1;
        self.positions.push_back(pos);
        if self.positions.len() > POSITION_HISTORY {
            self.positions.pop_front();
        }
        // Half-life of two minutes.
        self.suspicion *= 0.5f32.powf(1.0 / 2400.0);
    }

    /// Overall level of concern, 0 to 1: each event adds to it and it fades
    /// over a few minutes.
    pub fn suspicion(&self) -> f32 {
        self.suspicion
    }

    pub fn take_events(&mut self) -> Vec<CheckEvent> {
        std::mem::take(&mut self.pending)
    }

    fn settled(&self) -> bool {
        self.tick > SETTLE_TICKS
    }

    fn record(&mut self, kind: CheckKind, severity: f32) {
        self.suspicion = 1.0 - (1.0 - self.suspicion) * (1.0 - severity);
        warn!(?kind, severity, suspicion = self.suspicion, "possible check");
        self.pending.push(CheckEvent { kind, tick: self.tick, severity });
    }

    /// A `player_position` teleport, with the state before and after.
    /// Returns true if the view changed (the mouse hand gets disturbed).
    pub fn teleport(&mut self, from: Vec3, to: Vec3, yaw_change: f32, pitch_change: f32) -> bool {
        let rotated = yaw_change.abs() > 0.01 || pitch_change.abs() > 0.01;
        if !self.settled() {
            return rotated;
        }
        let d = Vec3::new(to.x - from.x, to.y - from.y, to.z - from.z);
        let distance = d.length_sqr().sqrt();
        if distance < 0.01 {
            if rotated {
                self.record(CheckKind::ForcedRotation { yaw_change, pitch_change }, 0.8);
            }
            return rotated;
        }
        // Rubber-band: back to where we were within the last few seconds
        // (not counting the last couple of ticks, which are "here").
        let setback = self.positions.iter().rev().skip(2).any(|p| {
            let (dx, dy, dz) = (p.x - to.x, p.y - to.y, p.z - to.z);
            dx * dx + dy * dy + dz * dz < 0.25
        });
        let severity = if setback {
            0.25
        } else if distance < 16.0 {
            0.7
        } else {
            // A long-range teleport is more likely a warp or lobby change.
            0.35
        };
        let severity = if rotated && !setback { (severity + 0.15f32).min(0.9) } else { severity };
        self.record(CheckKind::Teleport { distance, yaw_change, pitch_change, setback }, severity);
        rotated
    }

    pub fn forced_rotation(&mut self, yaw_change: f32, pitch_change: f32) {
        if self.settled() {
            self.record(CheckKind::ForcedRotation { yaw_change, pitch_change }, 0.8);
        }
    }

    pub fn look_at(&mut self, yaw_change: f32, pitch_change: f32) {
        if self.settled() {
            self.record(CheckKind::LookAt { yaw_change, pitch_change }, 0.8);
        }
    }

    /// The player took damage (hurt animation or damage event).
    pub fn damaged(&mut self) {
        self.last_damage_tick = Some(self.tick);
    }

    /// Velocity set or added by the server.
    pub fn velocity(&mut self, v: Vec3, explosion: bool) {
        let speed = v.length_sqr().sqrt();
        if !self.settled() || speed < 0.05 {
            return;
        }
        // The server re-sends plain falling velocity after fall damage and
        // similar; that is not a push.
        if !explosion && v.x.abs() < 0.03 && v.z.abs() < 0.03 && v.y <= 0.0 && v.y > -0.6 {
            return;
        }
        let explained = explosion || self.last_damage_tick.is_some_and(|t| self.tick.saturating_sub(t) <= 10);
        if explained {
            self.record(CheckKind::Knockback { speed }, 0.1);
        } else {
            self.record(CheckKind::Push { speed }, 0.65);
        }
    }

    pub fn held_slot_changed(&mut self, slot: i32) {
        if self.settled() {
            self.record(CheckKind::HeldSlotChanged { slot }, 0.6);
        }
    }

    pub fn screen_opened(&mut self) {
        if self.settled() {
            self.record(CheckKind::ScreenOpened, 0.5);
        }
    }

    pub fn game_mode_changed(&mut self) {
        if self.settled() {
            self.record(CheckKind::GameModeChanged, 0.5);
        }
    }

    pub fn flight_changed(&mut self, flying: bool) {
        if self.settled() {
            self.record(CheckKind::FlightChanged { flying }, 0.5);
        }
    }

    /// A block update arrived for `block` while the player's feet are at
    /// `player`. `changed` is false when the new state equals the old one
    /// (the server re-sends blocks under a player it thinks is floating).
    pub fn block_changed(&mut self, block: (i32, i32, i32), player: Vec3, changed: bool) {
        if !self.settled() || !changed {
            return;
        }
        let (dx, dy, dz) = (block.0 as f64 + 0.5 - player.x, block.1 as f64 + 0.5 - (player.y + 0.9), block.2 as f64 + 0.5 - player.z);
        if dx * dx + dy * dy + dz * dz > 3.0 * 3.0 {
            return;
        }
        self.nearby_block_changes.push_back(self.tick);
        while self.nearby_block_changes.front().is_some_and(|t| self.tick - t > 20) {
            self.nearby_block_changes.pop_front();
        }
        let count = self.nearby_block_changes.len();
        // One report per burst.
        let recently_reported = self.reported_block_burst.is_some_and(|t| self.tick - t <= 40);
        if count >= 3 && !recently_reported {
            self.reported_block_burst = Some(self.tick);
            self.record(CheckKind::BlocksChangedNearby { count }, 0.5);
        }
    }

    /// Call each tick with the other players within [`NEARBY_RADIUS`].
    pub fn players_nearby(&mut self, players: &[crate::entities::NearbyPlayer]) {
        let current: std::collections::HashSet<uuid::Uuid> = players.iter().map(|p| p.uuid).collect();
        if self.settled() {
            let arrived: Vec<_> = players.iter().filter(|p| !self.nearby.contains(&p.uuid)).collect();
            for p in arrived {
                let severity = if p.name.is_empty() {
                    0.6
                } else if p.distance < 5.0 {
                    0.45
                } else {
                    0.3
                };
                self.record(CheckKind::PlayerNearby { name: p.name.clone(), distance: p.distance }, severity);
            }
        }
        self.nearby = current;
    }

    pub fn inventory_changed(&mut self, slot: i16) {
        if self.settled() {
            self.record(CheckKind::InventoryChanged { slot }, 0.3);
        }
    }

    pub fn effect_applied(&mut self, effect: &str, amplifier: i32) {
        if !self.settled() {
            return;
        }
        // Effects that mess with movement or vision are classic checks.
        let disruptive = ["blindness", "darkness", "nausea", "slowness", "levitation", "jump_boost", "speed", "slow_falling"]
            .iter()
            .any(|e| effect.ends_with(e));
        self.record(CheckKind::EffectApplied { effect: effect.to_owned(), amplifier }, if disruptive { 0.45 } else { 0.2 });
    }

    /// A system chat, action-bar or title message, as plain text.
    pub fn message(&mut self, text: &str) {
        self.message_from(text, None);
    }

    /// A message with a known sender (player chat).
    pub fn message_from(&mut self, text: &str, from: Option<&str>) {
        if !self.settled() {
            return;
        }
        // Death messages name the player but are not addressed to them.
        if text.starts_with("death.") {
            return;
        }
        let lower = text.to_lowercase();
        let names_me = !self.name.is_empty() && lower.contains(&self.name);
        let keyword = KEYWORDS.iter().any(|k| lower.contains(k));
        let severity = match (names_me, keyword) {
            (true, true) => 0.7,
            (true, false) => 0.4,
            (false, true) => 0.2,
            (false, false) => return,
        };
        self.record(CheckKind::Message { text: text.to_owned(), from: from.map(str::to_owned) }, severity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settled() -> CheckMonitor {
        let mut m = CheckMonitor::new("Walker");
        for i in 0..100 {
            m.tick(Vec3::new(i as f64 * 0.2, 64.0, 0.0));
        }
        m
    }

    #[test]
    fn join_teleports_are_ignored() {
        let mut m = CheckMonitor::new("Walker");
        m.tick(Vec3::ZERO);
        assert!(m.teleport(Vec3::ZERO, Vec3::new(10.0, 64.0, 10.0), 90.0, 0.0));
        assert!(m.take_events().is_empty());
    }

    #[test]
    fn classifies_teleports() {
        let mut m = settled();
        let here = Vec3::new(19.8, 64.0, 0.0);

        // Head snap in place.
        m.teleport(here, here, 135.0, 0.0);
        // A few blocks sideways.
        m.teleport(here, Vec3::new(19.8, 64.0, 5.0), 0.0, 0.0);
        // Back to where we were two seconds ago.
        m.teleport(here, Vec3::new(12.0, 64.0, 0.0), 0.0, 0.0);

        let events = m.take_events();
        assert!(matches!(events[0].kind, CheckKind::ForcedRotation { .. }));
        assert!(matches!(events[1].kind, CheckKind::Teleport { setback: false, .. }));
        assert!(matches!(events[2].kind, CheckKind::Teleport { setback: true, .. }));
        assert!(events[0].severity > events[2].severity);
        assert!(m.suspicion() > 0.9);
    }

    #[test]
    fn velocity_after_damage_is_knockback() {
        let mut m = settled();
        m.velocity(Vec3::new(0.4, 0.4, 0.0), false);
        // Plain falling velocity is ignored.
        m.velocity(Vec3::new(0.0, -0.0784, 0.0), false);
        m.damaged();
        m.velocity(Vec3::new(0.4, 0.4, 0.0), false);
        let events = m.take_events();
        assert!(matches!(events[0].kind, CheckKind::Push { .. }));
        assert!(matches!(events[1].kind, CheckKind::Knockback { .. }));
    }

    #[test]
    fn nearby_block_bursts_and_messages() {
        let mut m = settled();
        let here = Vec3::new(19.8, 64.0, 0.0);
        m.block_changed((100, 64, 100), here, true);
        // Re-sent identical blocks are not changes.
        for x in 19..22 {
            m.block_changed((x, 63, 0), here, false);
        }
        for x in 19..22 {
            m.block_changed((x, 65, 1), here, true);
        }
        m.message("Walker are you a bot?");
        m.message("Server restarting in 5 minutes");
        let events = m.take_events();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0].kind, CheckKind::BlocksChangedNearby { count: 3 }));
        assert!(matches!(events[1].kind, CheckKind::Message { .. }));
    }

    #[test]
    fn suspicion_fades() {
        let mut m = settled();
        m.forced_rotation(90.0, 0.0);
        let before = m.suspicion();
        for _ in 0..2400 {
            m.tick(Vec3::ZERO);
        }
        assert!((m.suspicion() - before / 2.0).abs() < 0.01);
    }
}
