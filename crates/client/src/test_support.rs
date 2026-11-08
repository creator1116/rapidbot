//! A harness for testing a [`Controller`] outside the game loop.
//!
//! Enabled with the `test-support` feature. It holds a small world, a player,
//! an inventory, an entity list and the server's tags, builds the same
//! [`TickContext`] the game loop builds, runs one tick of a controller, and
//! reports back what that controller asked for. Nothing of the game loop's
//! internals is exposed, so a downstream crate can write controller tests
//! against published API only.
//!
//! ```
//! use rapidbot_client::test_support::ControllerHarness;
//! use rapidbot_client::{Controller, Keys, TickContext};
//!
//! struct Forward;
//! impl Controller for Forward {
//!     fn tick(&mut self, ctx: &mut TickContext<'_>) {
//!         ctx.set_keys(Keys {
//!             forward: true,
//!             ..Keys::default()
//!         });
//!     }
//! }
//!
//! let mut harness = ControllerHarness::new();
//! assert!(harness.tick(&mut Forward).keys.forward);
//! ```

use std::collections::VecDeque;

use rapidbot_buf::{VarInt, to_bytes};
use rapidbot_physics::math::Vec3;
use rapidbot_physics::{Keys, Player};
use rapidbot_world::chunk::Chunk;
use rapidbot_world::item::ItemData;
use rapidbot_world::tags::Tags;
use rapidbot_world::{DimensionHeight, Registry, World};

use crate::controller::{Actions, Controller, TickContext};
use crate::entities::Entities;
use crate::inventory::Inventory;

/// The floor every harness starts with.
pub const FLOOR_Y: i32 = 63;

/// What a controller asked for during one tick.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Requested {
    /// The movement keys it wanted held.
    pub keys: Keys,
    /// The view it asked for, as `(yaw, pitch)`, or `None` for nothing.
    pub aim: Option<(f32, f32)>,
    /// The attack button: held, or let go.
    pub attack: Option<bool>,
    /// A click of the attack button this tick.
    pub click_attack: bool,
    /// A right-click on the block under the crosshair.
    pub use_block: bool,
    /// The use button: held, or let go.
    pub use_item: Option<bool>,
    /// A hotbar slot it selected.
    pub select_slot: Option<u8>,
    /// Scroll notches.
    pub scroll: i32,
    /// It asked to close the open container.
    pub close_container: bool,
    /// A container button it pressed.
    pub container_button: Option<i32>,
    /// Chat lines it typed, in order.
    pub chat: Vec<String>,
}

/// A world and player to drive a controller against.
pub struct ControllerHarness {
    player: Player,
    world: World,
    entities: Entities,
    inventory: Inventory,
    tags: Tags,
    players: Vec<crate::entities::NearbyPlayer>,
    tick_count: u64,
    chat_open: bool,
    attack_strength: f32,
    breaking: Option<((i32, i32, i32), f32)>,
    own_id: i32,
}

impl Default for ControllerHarness {
    fn default() -> Self {
        Self::new()
    }
}

impl ControllerHarness {
    /// A flat stone floor at y = 63 in the four chunks around the origin,
    /// with the player standing on it at `(0.5, 64, 0.5)`, facing +z, with a
    /// full attack charge, an empty inventory and no entities.
    pub fn new() -> Self {
        let mut world = World::new(DimensionHeight::OVERWORLD);
        for cx in -1..=0 {
            for cz in -1..=0 {
                world.insert_chunk(cx, cz, Chunk::empty(24));
            }
        }
        let stone = state("minecraft:stone");
        for x in -4..5 {
            for z in -4..5 {
                world.set_block_state(x, FLOOR_Y, z, stone);
            }
        }
        let mut player = Player::new();
        player.set_pos(Vec3::new(0.5, 64.0, 0.5));
        player.on_ground = true;
        Self {
            player,
            world,
            entities: Entities::default(),
            inventory: Inventory::default(),
            tags: Tags::default(),
            players: Vec::new(),
            tick_count: 0,
            chat_open: false,
            attack_strength: 1.0,
            breaking: None,
            own_id: 0,
        }
    }

    /// Sets a block by name, where the harness has a chunk loaded.
    pub fn set_block(&mut self, x: i32, y: i32, z: i32, name: &str) -> &mut Self {
        self.world.set_block_state(x, y, z, state(name));
        self
    }

    /// Puts `count` of an item in an inventory slot. Hotbar 0 is slot 36.
    pub fn set_item(&mut self, index: usize, name: &str, count: i32) -> &mut Self {
        let data = ItemData::get();
        let id = data.item_id(name).expect("unknown item");
        let mut body = Vec::new();
        body.extend(to_bytes(&VarInt(0))); // container 0: the player's own
        body.extend(to_bytes(&VarInt(0))); // menu state
        body.extend((index as i16).to_be_bytes());
        body.extend(to_bytes(&VarInt(count)));
        body.extend(to_bytes(&VarInt(id as i32)));
        body.extend([0, 0]); // no components added or removed
        self.inventory
            .set_slot(&body)
            .expect("the harness builds a valid slot");
        self
    }

    /// Gives the harness's tags an entry, so a controller can be tested
    /// against one (`minecraft:block`, `minecraft:mineable/pickaxe`, ...).
    pub fn tag(&mut self, registry: &str, tag: &str, id: u32) -> &mut Self {
        self.tags.insert(registry, tag, id);
        self
    }

    /// The player, to set a position, a view or a state before a tick.
    pub fn player(&mut self) -> &mut Player {
        &mut self.player
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    pub fn inventory(&self) -> &Inventory {
        &self.inventory
    }

    pub fn entities(&mut self) -> &mut Entities {
        &mut self.entities
    }

    pub fn tags(&self) -> &Tags {
        &self.tags
    }

    /// The entities the context reports as nearby players.
    pub fn set_players(&mut self, players: Vec<crate::entities::NearbyPlayer>) -> &mut Self {
        self.players = players;
        self
    }

    /// Whether the chat box is open: with it open, keys and mouse do nothing.
    pub fn set_chat_open(&mut self, open: bool) -> &mut Self {
        self.chat_open = open;
        self
    }

    /// The attack charge a click would use, 0 to 1.
    pub fn set_attack_strength(&mut self, charge: f32) -> &mut Self {
        self.attack_strength = charge;
        self
    }

    /// The block being broken and its progress, as the context reports it.
    pub fn set_breaking(&mut self, breaking: Option<((i32, i32, i32), f32)>) -> &mut Self {
        self.breaking = breaking;
        self
    }

    /// The local player's entity id, which the context reports.
    pub fn set_own_id(&mut self, id: i32) -> &mut Self {
        self.own_id = id;
        self
    }

    /// Runs one tick of `controller` and reports what it asked for.
    pub fn tick(&mut self, controller: &mut dyn Controller) -> Requested {
        self.tick_count += 1;
        let pick = crate::interact::pick(
            &self.player,
            &self.world,
            &self.entities,
            self.own_id,
            &self.tags,
        );
        let mut actions = Actions::default();
        let mut aim = None;
        let mut keys = Keys::default();
        let mut chat = VecDeque::new();
        {
            let mut ctx = TickContext {
                player: &mut self.player,
                world: &self.world,
                tick: self.tick_count,
                players: &self.players,
                chat_open: self.chat_open,
                inventory: &self.inventory,
                entities: &self.entities,
                crosshair: pick.block,
                target: pick.entity,
                attack_strength: self.attack_strength,
                breaking: self.breaking,
                own_id: self.own_id,
                tags: &self.tags,
                actions: &mut actions,
                aim: &mut aim,
                keys: &mut keys,
                chat: &mut chat,
            };
            controller.tick(&mut ctx);
        }
        Requested {
            keys,
            aim: aim.map(|aim| (aim.yaw, aim.pitch)),
            attack: actions.attack,
            click_attack: actions.click,
            use_block: actions.use_block,
            use_item: actions.use_item,
            select_slot: actions.select_slot,
            scroll: actions.scroll,
            close_container: actions.close_container,
            container_button: actions.container_button,
            chat: chat.into_iter().collect(),
        }
    }

    /// Runs `ticks` ticks, returning what each one asked for.
    pub fn tick_n(&mut self, controller: &mut dyn Controller, ticks: u32) -> Vec<Requested> {
        (0..ticks).map(|_| self.tick(controller)).collect()
    }
}

fn state(name: &str) -> rapidbot_world::StateId {
    Registry::get()
        .block_by_name(name)
        .unwrap_or_else(|| panic!("unknown block {name}"))
        .default_state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_controller_drives_the_harness() {
        struct Ask;
        impl Controller for Ask {
            fn tick(&mut self, ctx: &mut TickContext<'_>) {
                ctx.set_keys(Keys {
                    forward: true,
                    jump: true,
                    ..Keys::default()
                });
                // Straight ahead of the player, so yaw and pitch are zero.
                ctx.aim_at(Vec3::new(0.5, ctx.player.eye_position().y, 10.5), 2.0);
                ctx.click_attack();
                ctx.select_slot(3);
                ctx.scroll_hotbar(1);
                ctx.chat("hello");
            }
        }

        let mut harness = ControllerHarness::new();
        let asked = harness.tick(&mut Ask);
        assert!(asked.keys.forward && asked.keys.jump);
        assert!(asked.click_attack);
        assert_eq!(asked.select_slot, Some(3));
        assert_eq!(asked.scroll, 1);
        assert_eq!(asked.chat, ["hello".to_string()]);
        let (yaw, pitch) = asked.aim.expect("an aim was asked for");
        assert!(yaw.abs() < 1.0e-3, "yaw {yaw}");
        assert!(pitch.abs() < 1.0e-3, "pitch {pitch}");
    }

    /// The world and the crosshair are real: looking straight down puts the
    /// crosshair on the floor block under the player.
    #[test]
    fn the_crosshair_comes_from_the_harness_world() {
        struct Look;
        impl Controller for Look {
            fn tick(&mut self, ctx: &mut TickContext<'_>) {
                ctx.snap_look(0.0, 90.0);
            }
        }
        struct AssertFloor;
        impl Controller for AssertFloor {
            fn tick(&mut self, ctx: &mut TickContext<'_>) {
                assert_eq!(ctx.crosshair.pos, (0, FLOOR_Y, 0));
                assert_eq!(
                    ctx.world.block_state(0, FLOOR_Y, 0),
                    Some(state("minecraft:stone"))
                );
                // Outside the loaded chunks the world does not answer.
                assert_eq!(ctx.world.block_state(1000, FLOOR_Y, 1000), None);
            }
        }

        let mut harness = ControllerHarness::new();
        harness.tick(&mut Look);
        harness.tick(&mut AssertFloor);
    }

    /// The requested aim and the tags are readable from inside a tick, which
    /// is what a controller that composes others needs.
    #[test]
    fn a_controller_can_read_the_aim_and_the_tags() {
        struct Peek;
        impl Controller for Peek {
            fn tick(&mut self, ctx: &mut TickContext<'_>) {
                assert_eq!(ctx.requested_aim(), None, "nothing asked for yet");
                ctx.aim(45.0, 10.0, 1.0);
                assert_eq!(ctx.requested_aim(), Some((45.0, 10.0)));
                assert!(ctx.tags().block_is("minecraft:test_tag", 7));
            }
        }

        let mut harness = ControllerHarness::new();
        harness.tag("minecraft:block", "minecraft:test_tag", 7);
        harness.tick(&mut Peek);
    }

    /// An inventory slot and a nearby-player list can be set up before a
    /// tick, and the context reports them.
    #[test]
    fn items_and_players_are_visible_to_a_controller() {
        struct Peek;
        impl Controller for Peek {
            fn tick(&mut self, ctx: &mut TickContext<'_>) {
                assert_eq!(
                    ctx.inventory.held().map(|s| s.name()),
                    Some("minecraft:stone")
                );
                assert_eq!(ctx.inventory.held().map(|s| s.count), Some(5));
                assert!(ctx.players.is_empty());
            }
        }

        let mut harness = ControllerHarness::new();
        harness.set_item(36, "minecraft:stone", 5);
        harness.tick(&mut Peek);
    }
}
