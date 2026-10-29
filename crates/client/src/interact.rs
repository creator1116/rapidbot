//! What the mouse buttons do: `Minecraft.startAttack`/`continueAttack`,
//! `MultiPlayerGameMode`'s block breaking with the block-change predictions
//! the server acknowledges (`BlockStatePredictionHandler`), and attacking
//! entities (`MultiPlayerGameMode.attack`, the client half of
//! `Player.attack`).

use std::collections::HashMap;

use rapidbot_physics::attributes;
use rapidbot_physics::math::Vec3;
use rapidbot_physics::player::Player;
use rapidbot_protocol::packets::play::LpVec3;
use rapidbot_protocol::packets::play::serverbound::{
    Attack, Interact, PlayerAction, PlayerActionKind, Punch, UseItem, UseItemOn,
};
use rapidbot_world::item::ItemStack;
use rapidbot_world::raycast::{self, BlockHit, Direction};
use rapidbot_world::tags::Tags;
use rapidbot_world::{AIR, Registry, StateId, World, pack_block_pos};

use crate::entities::{Entities, Entity, EntityHit};
use crate::inventory::Inventory;
use crate::net::PacketSender;

type BlockPos = (i32, i32, i32);

/// `Minecraft.hitResult`: what the crosshair is on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pick {
    /// The block under the crosshair. A miss when nothing is in reach, or
    /// when an entity is in front.
    pub block: BlockHit,
    /// The entity under the crosshair, within `entity_interaction_range`.
    pub entity: Option<EntityHit>,
}

fn distance_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    let (dx, dy, dz) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    dx * dx + dy * dy + dz * dz
}

/// `LocalPlayer.filterHitResult`: a hit beyond its range becomes a miss.
fn miss_at(location: [f64; 3], from: [f64; 3]) -> BlockHit {
    BlockHit {
        pos: (
            location[0].floor() as i32,
            location[1].floor() as i32,
            location[2].floor() as i32,
        ),
        location,
        direction: Direction::approximate_nearest(
            location[0] - from[0],
            location[1] - from[1],
            location[2] - from[2],
        ),
        inside: false,
        miss: true,
    }
}

/// `LocalPlayer.pick` with blocks only: what the crosshair would be on
/// with no entities around.
pub fn pick_block(player: &Player, world: &World) -> BlockHit {
    pick(player, world, &Entities::default(), 0, &Tags::default()).block
}

/// `LocalPlayer.raycastHitResult` for an item without an attack range of
/// its own: the nearest of the block and the entity along the view, each
/// within its interaction range of the eyes.
pub fn pick(player: &Player, world: &World, entities: &Entities, own_id: i32, tags: &Tags) -> Pick {
    let block_range = player.attributes.value(attributes::BLOCK_INTERACTION_RANGE);
    let entity_range = player
        .attributes
        .value(attributes::ENTITY_INTERACTION_RANGE);
    let mut max_distance = block_range.max(entity_range);
    let mut max_distance_sq = max_distance * max_distance;
    let eye = player.eye_position();
    let view = player.look_angle();
    let from = [eye.x, eye.y, eye.z];
    let far = [
        eye.x + view.x * max_distance,
        eye.y + view.y * max_distance,
        eye.z + view.z * max_distance,
    ];
    let block = raycast::clip(world, from, far);
    let block_distance_sq = distance_sq(block.location, from);
    if !block.miss {
        max_distance_sq = block_distance_sq;
        max_distance = max_distance_sq.sqrt();
    }
    let to = [
        eye.x + view.x * max_distance,
        eye.y + view.y * max_distance,
        eye.z + view.z * max_distance,
    ];
    let search = player
        .bounding_box()
        .expand_towards(
            view.x * max_distance,
            view.y * max_distance,
            view.z * max_distance,
        )
        .inflate(1.0, 1.0, 1.0);
    let search = (
        [search.min_x, search.min_y, search.min_z],
        [search.max_x, search.max_y, search.max_z],
    );
    let entity = entities.pick(own_id, from, to, search, max_distance_sq, tags);
    match entity {
        Some(hit) if distance_sq(hit.location, from) < block_distance_sq => {
            if distance_sq(hit.location, from) < entity_range * entity_range {
                Pick {
                    block: miss_at(hit.location, from),
                    entity: Some(hit),
                }
            } else {
                Pick {
                    block: miss_at(hit.location, from),
                    entity: None,
                }
            }
        }
        _ if block_distance_sq < block_range * block_range => Pick {
            block,
            entity: None,
        },
        _ => Pick {
            block: miss_at(block.location, from),
            entity: None,
        },
    }
}

/// A point to aim at for the crosshair to land on an entity: on its
/// vertical axis, `height` of the way up its box (0 feet, 1 top of the
/// head). `None` when that point is out of reach or something is in the
/// way.
pub fn entity_aim_point(
    player: &Player,
    world: &World,
    entities: &Entities,
    own_id: i32,
    tags: &Tags,
    id: i32,
    height: f64,
) -> Option<Vec3> {
    let entity = entities.get(id)?;
    let (min, max) = entity.bounding_box();
    let target = Vec3::new(
        entity.pos.x,
        min[1] + (max[1] - min[1]) * height.clamp(0.0, 1.0),
        entity.pos.z,
    );
    let eye = player.eye_position();
    let (dx, dy, dz) = (target.x - eye.x, target.y - eye.y, target.z - eye.z);
    let length = (dx * dx + dy * dy + dz * dz).sqrt();
    if length < 1.0e-6 {
        return None;
    }
    // Look along that line as the player would, and see what the crosshair
    // lands on.
    let mut probe = player.clone();
    let (yaw, pitch) = crate::controller::angles_to(eye, target);
    probe.y_rot = yaw;
    probe.x_rot = pitch;
    (pick(&probe, world, entities, own_id, tags)
        .entity
        .map(|hit| hit.id)
        == Some(id))
    .then_some(target)
}

/// `Player.getDestroySpeed`.
fn destroy_speed(player: &Player, held: Option<&ItemStack>, block: u32, tags: &Tags) -> f32 {
    let mut speed = held
        .and_then(ItemStack::tool)
        .map_or(1.0, |tool| tool.mining_speed(block, tags));
    if speed > 1.0 {
        speed += player.attributes.value(attributes::MINING_EFFICIENCY) as f32;
    }
    let effects = &player.effects;
    if effects.haste.is_some() || effects.conduit_power.is_some() {
        let amplifier = effects
            .haste
            .unwrap_or(0)
            .max(effects.conduit_power.unwrap_or(0));
        speed *= 1.0 + (amplifier + 1) as f32 * 0.2;
    }
    if let Some(amplifier) = effects.mining_fatigue {
        speed *= 0.3f64.powf((amplifier + 1) as f64) as f32;
    }
    speed *= player.attributes.value(attributes::BLOCK_BREAK_SPEED) as f32;
    if player.eye_in_water() {
        speed *= player.attributes.value(attributes::SUBMERGED_MINING_SPEED) as f32;
    }
    if !player.on_ground {
        speed /= 5.0;
    }
    speed
}

/// `BlockBehaviour.getDestroyProgress`: the share of a block broken per
/// tick of holding the button.
pub fn destroy_progress(
    player: &Player,
    held: Option<&ItemStack>,
    state: StateId,
    tags: &Tags,
) -> f32 {
    let registry = Registry::get();
    let info = registry.state(state);
    if info.hardness == -1.0 {
        return 0.0;
    }
    let correct = !registry.requires_tool(state)
        || held
            .and_then(ItemStack::tool)
            .is_some_and(|tool| tool.is_correct_for_drops(info.block, tags));
    let modifier = if correct { 30 } else { 100 };
    destroy_speed(player, held, info.block, tags) / info.hardness / modifier as f32
}

/// `FluidState.createLegacyBlock`: what is left where a block was.
fn fluid_block(state: StateId) -> StateId {
    let registry = Registry::get();
    let info = registry.state(state);
    if info.fluid.is_empty() {
        return AIR;
    }
    // Source water is `water[level=0]`; flowing is 8 - amount, +8 falling.
    let name = if info.fluid.ends_with("lava") {
        "minecraft:lava"
    } else {
        "minecraft:water"
    };
    let source = info.fluid == "minecraft:water" || info.fluid == "minecraft:lava";
    let level = if source {
        0
    } else {
        8 - info.fluid_amount.min(8) as u32 + if info.fluid_falling { 8 } else { 0 }
    };
    let Some(block) = registry.block_by_name(name) else {
        return AIR;
    };
    let wanted = format!("level={level}");
    (0..registry.state_count() as StateId)
        .find(|s| registry.state(*s).block == block.id && registry.state(*s).properties == wanted)
        .unwrap_or(AIR)
}

/// A point to aim at for the crosshair to land on a block: the first of
/// the block's centre and face centres that the eye can see. `None` when
/// the block is hidden or out of reach.
pub fn aim_point(player: &Player, world: &World, pos: BlockPos) -> Option<Vec3> {
    let range = player.attributes.value(attributes::BLOCK_INTERACTION_RANGE);
    let eye = player.eye_position();
    let (x, y, z) = (pos.0 as f64, pos.1 as f64, pos.2 as f64);
    let candidates = [
        [0.5, 0.5, 0.5],
        [0.5, 0.9, 0.5],
        [0.1, 0.5, 0.5],
        [0.9, 0.5, 0.5],
        [0.5, 0.5, 0.1],
        [0.5, 0.5, 0.9],
        [0.5, 0.1, 0.5],
    ];
    candidates
        .into_iter()
        .map(|c| Vec3::new(x + c[0], y + c[1], z + c[2]))
        .find(|target| {
            let (dx, dy, dz) = (target.x - eye.x, target.y - eye.y, target.z - eye.z);
            let length = (dx * dx + dy * dy + dz * dz).sqrt();
            if length < 1.0e-6 {
                return false;
            }
            let scale = range / length;
            let hit = raycast::clip(
                world,
                [eye.x, eye.y, eye.z],
                [eye.x + dx * scale, eye.y + dy * scale, eye.z + dz * scale],
            );
            !hit.miss
                && hit.pos == pos
                && distance_sq(hit.location, [eye.x, eye.y, eye.z]) < range * range
        })
}

/// What the client needs to act on blocks.
pub(crate) struct Hands<'a> {
    pub player: &'a mut Player,
    pub world: &'a mut World,
    pub inventory: &'a Inventory,
    pub entities: &'a Entities,
    pub tags: &'a Tags,
    pub net: &'a PacketSender,
    /// `blockActionRestricted`: spectator, or adventure mode (items that
    /// may break blocks in adventure mode are not considered).
    pub restricted: bool,
    /// `MultiPlayerGameMode.hasMissTime`: not creative.
    pub survival: bool,
}

/// The first block state of the block an item would place. Approximate:
/// this uses the registry's first state, which is the default state for
/// block items with no properties.
fn placed_state(item: &ItemStack) -> Option<StateId> {
    let registry = Registry::get();
    let block = registry.block_by_name(item.name())?;
    (0..registry.state_count() as StateId).find(|state| registry.state(*state).block == block.id)
}

/// Whether a block can be replaced by a placement (`BlockPlaceContext`).
fn replaceable(state: StateId) -> bool {
    let registry = Registry::get();
    let info = registry.state(state);
    info.is_air
        || !info.fluid.is_empty()
        || registry.collision_shape(state).is_none()
        || matches!(
            registry.block_of(state).name.as_str(),
            "minecraft:snow"
                | "minecraft:moss_carpet"
                | "minecraft:short_grass"
                | "minecraft:tall_grass"
        )
}

/// Where `BlockItem.useOn` would put a block: the clicked block itself when
/// it can be replaced, otherwise the adjacent block on the clicked face.
fn placement_pos(h: &Hands<'_>, hit: &BlockHit) -> Option<BlockPos> {
    if hit.miss {
        return None;
    }
    let clicked = h.world.block_state_or_air(hit.pos.0, hit.pos.1, hit.pos.2);
    let pos = if hit.inside || replaceable(clicked) {
        hit.pos
    } else {
        let (dx, dy, dz) = hit.direction.normal();
        (hit.pos.0 + dx, hit.pos.1 + dy, hit.pos.2 + dz)
    };
    let state = h.world.block_state_or_air(pos.0, pos.1, pos.2);
    replaceable(state).then_some(pos)
}

struct ServerVerified {
    sequence: i32,
    state: StateId,
    player_pos: Vec3,
}

/// `MultiPlayerGameMode` plus the attack half of `Minecraft`.
#[derive(Default)]
pub(crate) struct GameMode {
    destroying: bool,
    destroy_pos: BlockPos,
    destroy_direction: Option<Direction>,
    destroying_item: Option<ItemStack>,
    destroy_progress: f32,
    destroy_delay: i32,
    miss_time: i32,
    /// `Minecraft.rightClickDelay`: ticks before holding the button uses
    /// the item again.
    right_click_delay: i32,
    /// `Player.attackStrengthTicker`: ticks since the last swing at an
    /// entity or at nothing.
    attack_strength_ticker: i32,
    /// `Player.lastItemInMainHand`.
    last_item_in_main_hand: Option<ItemStack>,
    /// `BlockStatePredictionHandler`.
    sequence: i32,
    last_teleport_sequence: i32,
    verified: HashMap<BlockPos, ServerVerified>,
}

impl GameMode {
    pub fn new() -> Self {
        Self {
            last_teleport_sequence: -1,
            ..Default::default()
        }
    }

    /// The right-click delay half of `Minecraft.tick`.
    pub fn tick(&mut self) {
        if self.right_click_delay > 0 {
            self.right_click_delay -= 1;
        }
    }

    /// `MultiPlayerGameMode.useItemOn` for the main hand. Every call takes
    /// the next prediction sequence; a block item also predicts the block
    /// it would place, so the server's update for that position is held
    /// until the acknowledgement.
    pub(crate) fn use_item_on(&mut self, h: &mut Hands<'_>, hit: &BlockHit) {
        if hit.miss {
            return;
        }
        let sequence = self.next_sequence();
        if let Some(item) = h.inventory.held()
            && let Some(state) = placed_state(item)
            && let Some(pos) = placement_pos(h, hit)
        {
            let obstructed = Registry::get().collision_shape(state).is_some_and(|shape| {
                shape
                    .at(pos.0, pos.1, pos.2)
                    .intersects_box(&h.player.bounding_box())
            });
            if !obstructed {
                self.predict_block(h, pos, state);
            }
        }
        let (x, y, z) = hit.pos;
        h.net.send(&UseItemOn {
            hand: 0,
            pos: pack_block_pos(x, y, z),
            direction: hit.direction as i32,
            cursor_x: (hit.location[0] - x as f64) as f32,
            cursor_y: (hit.location[1] - y as f64) as f32,
            cursor_z: (hit.location[2] - z as f64) as f32,
            inside: hit.inside,
            world_border: false,
            sequence,
        });
    }

    /// `MultiPlayerGameMode.interact`: send `Interact` for the main hand,
    /// then the off hand when the main hand looks like it would not consume
    /// the click. The server is authoritative about success.
    fn use_entity(&mut self, h: &mut Hands<'_>, hit: &EntityHit) {
        let Some(target) = h.entities.get(hit.id) else {
            return;
        };
        let location = LpVec3 {
            x: hit.location[0] - target.pos.x,
            y: hit.location[1] - target.pos.y,
            z: hit.location[2] - target.pos.z,
        };
        let secondary = h.player.key_presses.shift;
        for hand in 0..2 {
            let stack = if hand == 0 {
                h.inventory.held()
            } else {
                h.inventory.offhand()
            };
            // Vanilla sends the main hand even with an empty hand; the off
            // hand follows only if the first interaction is not consumed.
            h.net.send(&Interact {
                entity_id: target.id,
                hand,
                location,
                using_secondary_action: secondary,
            });
            if Self::likely_entity_interact(stack, target) {
                // The local interaction is predicted to succeed and swing,
                // except for `interaction` entities, which consume without
                // an animation.
                if !target.info().is("Interaction") {
                    h.net.send(&Punch);
                }
                break;
            }
        }
    }

    /// Whether a held stack looks like it would consume an entity click.
    /// This is a prediction only; the server decides and may resync.
    fn likely_entity_interact(stack: Option<&ItemStack>, target: &Entity) -> bool {
        let info = target.info();
        if info.is("ItemFrame") {
            return stack.is_some() || target.framed_item().is_some();
        }
        if info.is("Interaction") {
            return true;
        }
        stack.is_some_and(|stack| {
            stack.has("minecraft:food")
                || stack.has("minecraft:equippable")
                || stack.name().ends_with("_spawn_egg")
                || matches!(
                    stack.name(),
                    "minecraft:lead"
                        | "minecraft:name_tag"
                        | "minecraft:shears"
                        | "minecraft:bucket"
                        | "minecraft:water_bucket"
                        | "minecraft:lava_bucket"
                        | "minecraft:milk_bucket"
                )
        })
    }

    /// `Minecraft.startUseItem` and the use half of `handleKeybinds`.
    ///
    /// `down` is the use button; `clicked` is a fresh press this tick.
    /// With nothing under the crosshair this sends `UseItem` for the main
    /// hand, then the off hand. On a block it sends `UseItemOn` instead, and
    /// on an entity it sends `Interact`.
    pub fn use_button(&mut self, h: &mut Hands<'_>, pick: &Pick, down: bool, clicked: bool) {
        if h.player.spectator {
            return;
        }
        // handleKeybinds: letting go while using releases the item.
        if h.player.using_item && !down {
            Self::send_action(
                h,
                PlayerActionKind::ReleaseUseItem,
                (0, 0, 0),
                Direction::Down,
                0,
            );
            h.player.stop_using_item();
        }
        // startUseItem does nothing while a block is being destroyed.
        if self.destroying || h.player.using_item {
            return;
        }
        // A click starts at once; holding waits for the delay.
        if !down || (!clicked && self.right_click_delay > 0) {
            return;
        }
        self.right_click_delay = 4;
        if let Some(entity) = pick.entity {
            self.use_entity(h, &entity);
            return;
        }
        if !pick.block.miss {
            self.use_item_on(h, &pick.block);
            return;
        }
        let y_rot = h.player.y_rot;
        let x_rot = h.player.x_rot;
        for hand in 0..2 {
            let stack = if hand == 0 {
                h.inventory.held()
            } else {
                h.inventory.offhand()
            };
            let Some(stack) = stack else { continue };
            // Vanilla stops at the first hand whose use succeeds. We do
            // not know the result locally, so send the off hand too when
            // the main hand does not look usable (a sword in front of a
            // shield, for example).
            h.net.send(&UseItem {
                hand,
                sequence: self.next_sequence(),
                y_rot,
                x_rot,
            });
            if Self::likely_uses_item(stack) {
                break;
            }
        }
    }

    /// Whether an item looks like it starts a use in progress. The server
    /// is authoritative: it sends `DATA_LIVING_ENTITY_FLAGS` either way.
    fn likely_uses_item(stack: &ItemStack) -> bool {
        stack.has("minecraft:consumable")
            || stack.has("minecraft:blocks_attacks")
            || matches!(
                stack.name(),
                "minecraft:bow"
                    | "minecraft:crossbow"
                    | "minecraft:trident"
                    | "minecraft:goat_horn"
                    | "minecraft:spyglass"
                    | "minecraft:brush"
                    | "minecraft:fishing_rod"
            )
    }

    /// The block being broken and how far along it is, 0 to 1.
    pub fn breaking(&self) -> Option<(BlockPos, f32)> {
        self.destroying
            .then_some((self.destroy_pos, self.destroy_progress))
    }

    /// The end of `Player.tick`: the attack charge builds, and starts over
    /// when a different item is held.
    pub fn player_tick(&mut self, held: Option<&ItemStack>) {
        self.attack_strength_ticker = self.attack_strength_ticker.saturating_add(1);
        if self.last_item_in_main_hand.as_ref() != held {
            // ItemStack.isSameItem.
            if self.last_item_in_main_hand.as_ref().map(|s| s.item) != held.map(|s| s.item) {
                self.attack_strength_ticker = 0;
            }
            self.last_item_in_main_hand = held.cloned();
        }
    }

    /// `Player.getCurrentItemAttackStrengthDelay`: ticks for a full charge.
    fn attack_strength_delay(player: &Player) -> f32 {
        (1.0 / player.attributes.value(attributes::ATTACK_SPEED) * 20.0) as f32
    }

    /// `Player.getAttackStrengthScale`: the attack charge, 0 to 1.
    pub fn attack_strength(&self, player: &Player, a: f32) -> f32 {
        ((self.attack_strength_ticker as f32 + a) / Self::attack_strength_delay(player))
            .clamp(0.0, 1.0)
    }

    /// `Player.cannotAttackWithItem`: the item needs more charge to swing.
    fn cannot_attack_with(&self, player: &Player, held: Option<&ItemStack>) -> bool {
        let required = held
            .and_then(|s| s.component("minecraft:minimum_attack_charge"))
            .and_then(|bytes| {
                bytes
                    .get(..4)
                    .map(|b| f32::from_be_bytes([b[0], b[1], b[2], b[3]]))
            })
            .unwrap_or(0.0);
        required > 0.0
            && self.attack_strength_ticker as f32 / Self::attack_strength_delay(player) < required
    }

    /// `MultiPlayerGameMode.attack`: the packet, then what `Player.attack`
    /// does on the client. The caller has sent the carried item.
    fn attack(&mut self, h: &mut Hands<'_>, target: &Entity) {
        h.net.send(&Attack {
            entity_id: target.id,
        });
        tracing::debug!(
            id = target.id,
            kind = target.kind,
            charge = self.attack_strength(h.player, 0.5),
            sprinting = h.player.sprinting,
            "attack"
        );
        // Player.attack. `cannotAttack`: armour stands and the like in a
        // protected area (`mayInteract`) are not considered.
        if target.is_attackable(h.tags) {
            let strength = self.attack_strength(h.player, 0.5);
            // onAttack()
            self.attack_strength_ticker = 0;
            // deflectProjectile: a redirectable projectile ends it here.
            let deflected = target.info().is("Projectile")
                && h.tags.contains(
                    "minecraft:entity_type",
                    "minecraft:redirectable_projectile",
                    target.type_id(),
                );
            // The client's attack damage is the attribute's base, always
            // above zero, so the rest runs.
            if !deflected {
                let full_strength = strength > 0.9;
                let knockback_attack = h.player.sprinting && full_strength;
                if target.hurt_client() {
                    // causeExtraKnockback: enchantments are the server's
                    // business, so client-side only the sprint hit counts.
                    let knockback = h.player.attributes.value(attributes::ATTACK_KNOCKBACK) as f32
                        / 2.0
                        + if knockback_attack { 0.5 } else { 0.0 };
                    if knockback > 0.0 {
                        h.player.delta_movement = h.player.delta_movement.multiply(0.6, 1.0, 0.6);
                        h.player.set_sprinting(false);
                    }
                }
            }
        }
        // player.resetAttackStrengthTicker()
        self.attack_strength_ticker = 0;
        if h.player.abilities.instabuild {
            self.destroy_delay = 5;
        }
    }

    /// `startPredicting`: the sequence number of a new predicted action.
    fn next_sequence(&mut self) -> i32 {
        self.sequence += 1;
        self.sequence
    }

    /// `ClientLevel.setBlock` while predicting: remember what the server
    /// last said was there.
    fn predict_block(&mut self, h: &mut Hands<'_>, pos: BlockPos, state: StateId) {
        let old = h.world.block_state_or_air(pos.0, pos.1, pos.2);
        let sequence = self.sequence;
        self.verified
            .entry(pos)
            .and_modify(|v| v.sequence = sequence)
            .or_insert(ServerVerified {
                sequence,
                state: old,
                player_pos: h.player.pos,
            });
        h.world.set_block_state(pos.0, pos.1, pos.2, state);
    }

    /// `setServerVerifiedBlockState`: true if the position is predicted,
    /// in which case the world keeps the prediction until the ack.
    pub fn server_block(&mut self, pos: BlockPos, state: StateId) -> bool {
        match self.verified.get_mut(&pos) {
            Some(verified) => {
                verified.state = state;
                true
            }
            None => false,
        }
    }

    /// `handleBlockChangedAck` → `endPredictionsUpTo` → `syncBlockState`.
    pub fn block_changed_ack(&mut self, sequence: i32, world: &mut World, player: &mut Player) {
        let done: Vec<BlockPos> = self
            .verified
            .iter()
            .filter(|(_, v)| v.sequence <= sequence)
            .map(|(p, _)| *p)
            .collect();
        for pos in done {
            let Some(verified) = self.verified.remove(&pos) else {
                continue;
            };
            if world.block_state_or_air(pos.0, pos.1, pos.2) == verified.state {
                continue;
            }
            world.set_block_state(pos.0, pos.1, pos.2, verified.state);
            // The block is back and the player is in it: return to where
            // the prediction was made.
            if self.last_teleport_sequence < sequence {
                let colliding =
                    Registry::get()
                        .collision_shape(verified.state)
                        .is_some_and(|shape| {
                            shape
                                .at(pos.0, pos.1, pos.2)
                                .intersects_box(&player.bounding_box())
                        });
                if colliding {
                    player.set_pos(verified.player_pos);
                }
            }
        }
    }

    /// `handleMovePlayer`: `BlockStatePredictionHandler.onTeleport`, then
    /// `stopDestroyBlock`.
    pub fn on_teleport(&mut self, net: &PacketSender) {
        self.last_teleport_sequence = self.sequence;
        if self.destroying {
            let pos = self.destroy_pos;
            net.send(&PlayerAction {
                action: PlayerActionKind::AbortDestroyBlock,
                pos: pack_block_pos(pos.0, pos.1, pos.2),
                direction: Direction::Down as u8,
                sequence: 0,
            });
            self.destroying = false;
            self.destroy_progress = 0.0;
        }
    }

    fn send_action(
        h: &Hands<'_>,
        action: PlayerActionKind,
        pos: BlockPos,
        direction: Direction,
        sequence: i32,
    ) {
        h.net.send(&PlayerAction {
            action,
            pos: pack_block_pos(pos.0, pos.1, pos.2),
            direction: direction as u8,
            sequence,
        });
    }

    fn is_air(h: &Hands<'_>, pos: BlockPos) -> bool {
        Registry::get()
            .state(h.world.block_state_or_air(pos.0, pos.1, pos.2))
            .is_air
    }

    /// `MultiPlayerGameMode.destroyBlock`: the client-side removal.
    fn destroy_block(&mut self, h: &mut Hands<'_>, pos: BlockPos) -> bool {
        if h.restricted {
            return false;
        }
        let held = h.inventory.held();
        // canDestroyBlock: swords and the like do not break blocks in creative.
        if h.player.abilities.instabuild
            && held
                .and_then(ItemStack::tool)
                .is_some_and(|t| !t.can_destroy_blocks_in_creative)
        {
            return false;
        }
        let old = h.world.block_state_or_air(pos.0, pos.1, pos.2);
        if Registry::get().state(old).is_air {
            return false;
        }
        let left = fluid_block(old);
        self.predict_block(h, pos, left);
        left != old
    }

    /// `startDestroyBlock`.
    fn start_destroy_block(
        &mut self,
        h: &mut Hands<'_>,
        pos: BlockPos,
        direction: Direction,
    ) -> bool {
        if h.restricted {
            return false;
        }
        if h.player.abilities.instabuild {
            let sequence = self.next_sequence();
            self.destroy_block(h, pos);
            Self::send_action(
                h,
                PlayerActionKind::StartDestroyBlock,
                pos,
                direction,
                sequence,
            );
            self.destroy_delay = 5;
        } else if !self.destroying || !self.same_destroy_target(h, pos) {
            if self.destroying {
                Self::send_action(
                    h,
                    PlayerActionKind::AbortDestroyBlock,
                    self.destroy_pos,
                    direction,
                    0,
                );
            }
            let state = h.world.block_state_or_air(pos.0, pos.1, pos.2);
            let sequence = self.next_sequence();
            let not_air = !Registry::get().state(state).is_air;
            if not_air && destroy_progress(h.player, h.inventory.held(), state, h.tags) >= 1.0 {
                self.destroy_block(h, pos);
            } else {
                self.destroying = true;
                self.destroy_pos = pos;
                self.destroy_direction = Some(direction);
                self.destroying_item = h.inventory.held().cloned();
                self.destroy_progress = 0.0;
            }
            Self::send_action(
                h,
                PlayerActionKind::StartDestroyBlock,
                pos,
                direction,
                sequence,
            );
        }
        true
    }

    /// `stopDestroyBlock`.
    fn stop_destroy_block(&mut self, h: &mut Hands<'_>) {
        if self.destroying {
            Self::send_action(
                h,
                PlayerActionKind::AbortDestroyBlock,
                self.destroy_pos,
                Direction::Down,
                0,
            );
            self.destroying = false;
            self.destroy_progress = 0.0;
        }
    }

    fn same_destroy_target(&self, h: &Hands<'_>, pos: BlockPos) -> bool {
        pos == self.destroy_pos
            && ItemStack::same_item_same_components(
                h.inventory.held(),
                self.destroying_item.as_ref(),
            )
    }

    /// `continueDestroyBlock`. The caller has sent the carried item
    /// (`ensureHasSentCarriedItem`).
    fn continue_destroy_block(
        &mut self,
        h: &mut Hands<'_>,
        pos: BlockPos,
        direction: Direction,
    ) -> bool {
        if self.destroy_delay > 0 {
            self.destroy_delay -= 1;
            return true;
        }
        if h.player.abilities.instabuild {
            self.destroy_delay = 5;
            let sequence = self.next_sequence();
            self.destroy_block(h, pos);
            Self::send_action(
                h,
                PlayerActionKind::StartDestroyBlock,
                pos,
                direction,
                sequence,
            );
            return true;
        }
        if !self.same_destroy_target(h, pos) {
            return self.start_destroy_block(h, pos, direction);
        }
        let state = h.world.block_state_or_air(pos.0, pos.1, pos.2);
        if Registry::get().state(state).is_air {
            self.destroying = false;
            return false;
        }
        self.destroy_progress += destroy_progress(h.player, h.inventory.held(), state, h.tags);
        if self.destroy_progress >= 1.0 {
            self.destroying = false;
            let sequence = self.next_sequence();
            self.destroy_block(h, pos);
            Self::send_action(
                h,
                PlayerActionKind::StopDestroyBlock,
                pos,
                direction,
                sequence,
            );
            self.destroy_direction = None;
            self.destroy_progress = 0.0;
            self.destroy_delay = 5;
        } else if self.destroy_direction != Some(direction) {
            self.destroy_direction = Some(direction);
            Self::send_action(
                h,
                PlayerActionKind::ChangeDestroyDirection,
                pos,
                direction,
                0,
            );
        }
        true
    }

    /// `Minecraft.startAttack` for a click of the attack button. Returns
    /// true if the click finished its work (a block broke at once).
    fn start_attack(
        &mut self,
        h: &mut Hands<'_>,
        pick: &Pick,
        carried: &mut impl FnMut(&PacketSender),
    ) -> bool {
        if self.miss_time > 0 {
            return false;
        }
        let held = h.inventory.held();
        if self.cannot_attack_with(h.player, held) {
            return false;
        }
        // Spears stab instead of swinging; not supported.
        if held.is_some_and(|s| s.has("minecraft:piercing_weapon")) {
            return false;
        }
        let hit = &pick.block;
        let mut end = false;
        if let Some(target) = pick.entity.and_then(|e| h.entities.get(e.id)) {
            // An item with its own attack range checks it here; only
            // spears have one, and they returned above.
            carried(h.net);
            self.attack(h, target);
        } else if !hit.miss && !Self::is_air(h, hit.pos) {
            self.start_destroy_block(h, hit.pos, hit.direction);
            end = Self::is_air(h, hit.pos);
        } else {
            if h.survival {
                self.miss_time = 10;
            }
            // player.resetAttackStrengthTicker()
            self.attack_strength_ticker = 0;
        }
        // player.swing(..) then the punch.
        h.net.send(&Punch);
        end
    }

    /// `Minecraft.continueAttack`.
    fn continue_attack(
        &mut self,
        h: &mut Hands<'_>,
        down: bool,
        hit: &BlockHit,
        carried: &mut impl FnMut(&PacketSender),
    ) {
        if !down {
            self.miss_time = 0;
        }
        if self.miss_time > 0
            || h.inventory
                .held()
                .is_some_and(|s| s.has("minecraft:piercing_weapon"))
        {
            return;
        }
        if down && !hit.miss {
            if !Self::is_air(h, hit.pos) {
                carried(h.net);
                if self.continue_destroy_block(h, hit.pos, hit.direction) {
                    h.net.send(&Punch);
                }
            }
        } else {
            self.stop_destroy_block(h);
        }
    }

    /// The attack button's part of `handleKeybinds`, and the `missTime`
    /// countdown after it. `clicks` is how many times the button went down
    /// since the last tick, `down` whether it is held now. `carried` is
    /// `ensureHasSentCarriedItem`.
    pub fn attack_button(
        &mut self,
        h: &mut Hands<'_>,
        clicks: u32,
        down: bool,
        pick: &Pick,
        mut carried: impl FnMut(&PacketSender),
    ) {
        let mut instant = false;
        for _ in 0..clicks {
            instant |= self.start_attack(h, pick, &mut carried);
        }
        self.continue_attack(h, !instant && down, &pick.block, &mut carried);
        if self.miss_time > 0 {
            self.miss_time -= 1;
        }
    }

    /// A screen is open: `missTime = 10000`, and keybinds do not run.
    pub fn screen_open(&mut self) {
        self.miss_time = 10000;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapidbot_buf::{Decode, Encode, VarInt};
    use rapidbot_protocol::Packet;
    use rapidbot_world::DimensionHeight;
    use rapidbot_world::chunk::Chunk;
    use rapidbot_world::item::ItemData;

    struct Scene {
        player: Player,
        world: World,
        inventory: Inventory,
        entities: Entities,
        tags: Tags,
        net: PacketSender,
        sent: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
        mode: GameMode,
        dirt: StateId,
    }

    fn state(name: &str) -> StateId {
        let registry = Registry::get();
        let block = registry.block_by_name(name).unwrap().id;
        (0..registry.state_count() as StateId)
            .find(|s| registry.state(*s).block == block)
            .unwrap()
    }

    impl Scene {
        /// A stone floor at y = 63 with a dirt block at (1, 64, 0); the
        /// player stands at the origin looking down at the dirt.
        fn new() -> Self {
            let height = DimensionHeight {
                min_y: -64,
                height: 384,
            };
            let mut world = World::new(height);
            for cx in -1..=0 {
                for cz in -1..=0 {
                    world.insert_chunk(cx, cz, Chunk::empty(24));
                }
            }
            for x in -4..5 {
                for z in -4..5 {
                    world.set_block_state(x, 63, z, state("minecraft:stone"));
                }
            }
            let dirt = state("minecraft:dirt");
            world.set_block_state(1, 64, 0, dirt);
            let mut player = Player::new();
            player.set_pos(Vec3::new(0.5, 64.0, 0.5));
            player.on_ground = true;
            player.y_rot = -90.0;
            player.x_rot = 40.0;
            let (net, sent) = PacketSender::for_test();
            Self {
                player,
                world,
                inventory: Inventory::default(),
                entities: Entities::default(),
                tags: Tags::default(),
                net,
                sent,
                mode: GameMode::new(),
                dirt,
            }
        }

        /// One tick of the attack button; returns the packets it sent as
        /// (id, body).
        fn tick(&mut self, clicks: u32, down: bool) -> Vec<(i32, Vec<u8>)> {
            let hit = pick(&self.player, &self.world, &self.entities, 1, &self.tags);
            let mut hands = Hands {
                player: &mut self.player,
                world: &mut self.world,
                inventory: &self.inventory,
                entities: &self.entities,
                tags: &self.tags,
                net: &self.net,
                restricted: false,
                survival: true,
            };
            self.mode
                .attack_button(&mut hands, clicks, down, &hit, |_| {});
            self.mode.player_tick(self.inventory.held());
            self.take_packets()
        }

        /// One tick of the use button.
        fn tick_use(&mut self, down: bool, clicked: bool) -> Vec<(i32, Vec<u8>)> {
            let hit = pick(&self.player, &self.world, &self.entities, 1, &self.tags);
            let mut hands = Hands {
                player: &mut self.player,
                world: &mut self.world,
                inventory: &self.inventory,
                entities: &self.entities,
                tags: &self.tags,
                net: &self.net,
                restricted: false,
                survival: true,
            };
            self.mode.use_button(&mut hands, &hit, down, clicked);
            self.take_packets()
        }

        /// Puts a stack in an inventory-menu slot (36 is hotbar 0).
        fn set_item(&mut self, index: usize, name: &str) {
            let id = ItemData::get().item_id(name).unwrap();
            let mut body = Vec::new();
            body.extend(rapidbot_buf::to_bytes(&VarInt(0)));
            body.extend(rapidbot_buf::to_bytes(&VarInt(0)));
            body.extend((index as i16).to_be_bytes());
            body.extend(rapidbot_buf::to_bytes(&VarInt(1)));
            body.extend(rapidbot_buf::to_bytes(&VarInt(id as i32)));
            body.extend([0, 0]);
            self.inventory.set_slot(&body).unwrap();
        }

        fn take_packets(&mut self) -> Vec<(i32, Vec<u8>)> {
            let mut packets = Vec::new();
            while let Ok(frame) = self.sent.try_recv() {
                let mut buf = frame.as_slice();
                let id = VarInt::decode(&mut buf).unwrap().0;
                packets.push((id, buf.to_vec()));
            }
            packets
        }
    }

    fn action(body: &[u8]) -> PlayerAction {
        PlayerAction::decode(&mut &body[..]).unwrap()
    }

    #[test]
    fn crosshair_and_aim_point() {
        let scene = Scene::new();
        let hit = pick_block(&scene.player, &scene.world);
        assert_eq!((hit.pos, hit.miss), ((1, 64, 0), false));
        assert_eq!(hit.direction, Direction::Up);
        let aim = aim_point(&scene.player, &scene.world, (1, 64, 0)).unwrap();
        assert_eq!((aim.x, aim.y, aim.z), (1.5, 64.5, 0.5));
        // Out of reach.
        assert!(aim_point(&scene.player, &scene.world, (30, 64, 0)).is_none());
    }

    /// `getDestroyProgress` for the usual cases.
    #[test]
    fn progress_per_tick() {
        let scene = Scene::new();
        let stone = state("minecraft:stone");
        // Bare hand on dirt: 1 / 0.5 / 30. On stone, the wrong tool: 1 / 1.5 / 100.
        assert_eq!(
            destroy_progress(&scene.player, None, scene.dirt, &scene.tags),
            1.0f32 / 0.5 / 30.0
        );
        assert_eq!(
            destroy_progress(&scene.player, None, stone, &scene.tags),
            1.0f32 / 1.5 / 100.0
        );
        // A diamond pickaxe on stone, once the server's tags say it mines it.
        let pick = ItemStack::new(
            ItemData::get()
                .item_id("minecraft:diamond_pickaxe")
                .unwrap(),
            1,
        );
        let mut tags = Tags::default();
        tags.insert(
            "minecraft:block",
            "minecraft:mineable/pickaxe",
            Registry::get().state(stone).block,
        );
        assert_eq!(
            destroy_progress(&scene.player, Some(&pick), stone, &tags),
            8.0f32 / 1.5 / 30.0
        );
        // In the air: a fifth of the speed.
        let mut airborne = Scene::new();
        airborne.player.on_ground = false;
        assert_eq!(
            destroy_progress(&airborne.player, None, airborne.dirt, &airborne.tags),
            1.0f32 / 5.0 / 0.5 / 30.0
        );
        // Bedrock never breaks.
        assert_eq!(
            destroy_progress(&scene.player, None, state("minecraft:bedrock"), &scene.tags),
            0.0
        );
    }

    /// Holding the button on dirt: start, a punch per tick, stop with the
    /// next sequence number, and the block gone before the server says so.
    #[test]
    fn digs_a_block() {
        let mut scene = Scene::new();
        let start = scene.tick(1, true);
        // startAttack swings, and continueAttack runs on the same tick:
        // the first tick already counts and punches twice.
        assert_eq!(
            start.iter().map(|p| p.0).collect::<Vec<_>>(),
            [PlayerAction::ID, Punch::ID, Punch::ID]
        );
        assert_eq!(start[0].0, PlayerAction::ID);
        let begin = action(&start[0].1);
        assert_eq!(
            (begin.action, begin.sequence, begin.direction),
            (PlayerActionKind::StartDestroyBlock, 1, Direction::Up as u8)
        );
        assert_eq!(rapidbot_world::unpack_block_pos(begin.pos), (1, 64, 0));
        assert_eq!(
            scene.mode.breaking(),
            Some(((1, 64, 0), 1.0f32 / 0.5 / 30.0))
        );

        let mut ticks = 0;
        let stop = loop {
            let packets = scene.tick(0, true);
            ticks += 1;
            assert!(ticks < 40, "never finished");
            if packets.len() == 1 {
                assert_eq!(packets[0].0, Punch::ID);
                continue;
            }
            break packets;
        };
        // Fifteen ticks in all (0.75 s for dirt by hand), counting the first.
        assert_eq!(ticks, 14);
        let end = action(&stop[0].1);
        assert_eq!(
            (end.action, end.sequence),
            (PlayerActionKind::StopDestroyBlock, 2)
        );
        assert_eq!(stop[1].0, Punch::ID);
        assert_eq!(scene.world.block_state(1, 64, 0), Some(AIR));
        assert_eq!(scene.mode.breaking(), None);

        // The server's own update for the block is held until the ack.
        assert!(scene.mode.server_block((1, 64, 0), AIR));
        scene
            .mode
            .block_changed_ack(2, &mut scene.world, &mut scene.player);
        assert!(!scene.mode.server_block((1, 64, 0), AIR));
        assert_eq!(scene.world.block_state(1, 64, 0), Some(AIR));

        // destroyDelay: five ticks of swinging at the floor behind it
        // before the next block is started.
        for _ in 0..5 {
            let packets = scene.tick(0, true);
            assert_eq!(packets.iter().map(|p| p.0).collect::<Vec<_>>(), [Punch::ID]);
        }
        let next = scene.tick(0, true);
        assert_eq!(
            action(&next[0].1).action,
            PlayerActionKind::StartDestroyBlock
        );
        assert_eq!(action(&next[0].1).sequence, 3);
    }

    /// Letting go early aborts; the server refusing a break puts the block
    /// back when it acknowledges.
    #[test]
    fn abort_and_rejection() {
        let mut scene = Scene::new();
        scene.tick(1, true);
        scene.tick(0, true);
        let released = scene.tick(0, false);
        assert_eq!(released.len(), 1);
        let abort = action(&released[0].1);
        assert_eq!(
            (abort.action, abort.sequence, abort.direction),
            (
                PlayerActionKind::AbortDestroyBlock,
                0,
                Direction::Down as u8
            )
        );
        assert!(scene.tick(0, false).is_empty());

        // Break it for real, but the server says the dirt is still there.
        scene.tick(1, true);
        while scene.world.block_state(1, 64, 0) != Some(AIR) {
            scene.tick(0, true);
        }
        let dirt = scene.dirt;
        assert!(scene.mode.server_block((1, 64, 0), dirt));
        assert_eq!(scene.world.block_state(1, 64, 0), Some(AIR));
        scene
            .mode
            .block_changed_ack(3, &mut scene.world, &mut scene.player);
        assert_eq!(scene.world.block_state(1, 64, 0), Some(dirt));
    }

    /// A click on nothing: one punch, then ten ticks in which clicks do
    /// nothing (`missTime`).
    #[test]
    fn miss_time() {
        let mut scene = Scene::new();
        scene.player.x_rot = -60.0;
        assert_eq!(
            scene.tick(1, false).iter().map(|p| p.0).collect::<Vec<_>>(),
            [Punch::ID]
        );
        // Not held, so continueAttack clears missTime and clicks work again.
        assert_eq!(scene.tick(1, false).len(), 1);
        // Held: the miss time runs, and further clicks are swallowed.
        assert_eq!(scene.tick(1, true).len(), 1);
        for _ in 0..9 {
            assert!(scene.tick(1, true).is_empty());
        }
        assert_eq!(scene.tick(1, true).len(), 1);
    }

    /// Puts an entity of `kind` at `pos` with entity ID 7.
    fn spawn(scene: &mut Scene, kind: &str, pos: [f64; 3]) {
        use rapidbot_buf::Encode;
        let mut add = Vec::new();
        VarInt(7).encode(&mut add);
        uuid::Uuid::from_u128(7).encode(&mut add);
        VarInt(
            Registry::get()
                .entity_types
                .iter()
                .position(|n| n == kind)
                .unwrap() as i32,
        )
        .encode(&mut add);
        for v in pos {
            v.encode(&mut add);
        }
        add.extend_from_slice(&[0, 0, 0, 0, 0]);
        scene.entities.add_entity(&add).unwrap();
    }

    /// The crosshair takes the entity in front of a block, only within
    /// three blocks, and a block in front of the entity hides it.
    #[test]
    fn crosshair_prefers_the_nearer_of_block_and_entity() {
        let mut scene = Scene::new();
        scene.player.x_rot = 0.0;
        spawn(&mut scene, "minecraft:zombie", [2.5, 64.0, 0.5]);
        let hit = pick(&scene.player, &scene.world, &scene.entities, 1, &scene.tags);
        assert_eq!(hit.entity.map(|e| e.id), Some(7));
        assert!(hit.block.miss);
        assert_eq!(hit.entity.unwrap().location[0], 2.5 - 0.3f32 as f64);
        assert!(
            entity_aim_point(
                &scene.player,
                &scene.world,
                &scene.entities,
                1,
                &scene.tags,
                7,
                0.8
            )
            .is_some()
        );

        // 3.2 blocks to its box: seen by the ray, out of reach, so a miss
        // (even with a block further along within block reach).
        let mut far = Scene::new();
        far.player.x_rot = 0.0;
        spawn(&mut far, "minecraft:zombie", [4.0, 64.0, 0.5]);
        far.world.set_block_state(4, 65, 0, far.dirt);
        let hit = pick(&far.player, &far.world, &far.entities, 1, &far.tags);
        assert_eq!((hit.entity, hit.block.miss), (None, true));
        assert!(
            entity_aim_point(&far.player, &far.world, &far.entities, 1, &far.tags, 7, 0.8)
                .is_none()
        );

        // A block between them.
        let dirt = scene.dirt;
        scene.world.set_block_state(1, 65, 0, dirt);
        let hit = pick(&scene.player, &scene.world, &scene.entities, 1, &scene.tags);
        assert_eq!(
            (hit.entity, hit.block.pos, hit.block.miss),
            (None, (1, 65, 0), false)
        );
    }

    /// A click on a mob: carried item, attack, punch. No slowdown, since
    /// only the server knows whether a mob was hurt.
    #[test]
    fn attacks_a_mob() {
        let mut scene = Scene::new();
        scene.player.x_rot = 0.0;
        spawn(&mut scene, "minecraft:zombie", [2.5, 64.0, 0.5]);
        for _ in 0..20 {
            scene.tick(0, false);
        }
        assert_eq!(scene.mode.attack_strength(&scene.player, 0.0), 1.0);
        scene.player.set_sprinting(true);
        scene.player.delta_movement = Vec3::new(0.2, 0.0, 0.1);
        let packets = scene.tick(1, false);
        assert_eq!(
            packets.iter().map(|p| p.0).collect::<Vec<_>>(),
            [Attack::ID, Punch::ID]
        );
        assert_eq!(packets[0].1, [7]);
        assert!(scene.player.sprinting);
        assert_eq!(scene.player.delta_movement, Vec3::new(0.2, 0.0, 0.1));
        // The charge starts over: 1 tick of the 5 a bare hand needs
        // (attack speed 4), counted from the end of this tick.
        assert_eq!(scene.mode.attack_strength(&scene.player, 0.0), 1.0f32 / 5.0);
        // Holding the button on an entity does nothing more.
        assert!(scene.tick(0, true).is_empty());
    }

    /// A fully charged sprinting hit on another player: the client slows
    /// itself to 0.6 and stops sprinting (`causeExtraKnockback`). A weak or
    /// walking hit does neither.
    #[test]
    fn sprint_hit_on_a_player_slows_and_resets_sprint() {
        let mut scene = Scene::new();
        scene.player.x_rot = 0.0;
        spawn(&mut scene, "minecraft:player", [2.5, 64.0, 0.5]);
        for _ in 0..20 {
            scene.tick(0, false);
        }
        scene.player.set_sprinting(true);
        scene.player.delta_movement = Vec3::new(0.2, -0.0784, 0.1);
        let packets = scene.tick(1, false);
        assert_eq!(
            packets.iter().map(|p| p.0).collect::<Vec<_>>(),
            [Attack::ID, Punch::ID]
        );
        assert!(!scene.player.sprinting);
        assert_eq!(
            scene.player.delta_movement,
            Vec3::new(0.2 * 0.6, -0.0784, 0.1 * 0.6)
        );

        // Straight after: the charge is nearly empty, so no knockback hit.
        scene.player.set_sprinting(true);
        scene.player.delta_movement = Vec3::new(0.2, 0.0, 0.1);
        scene.tick(1, false);
        assert!(scene.player.sprinting);
        assert_eq!(scene.player.delta_movement, Vec3::new(0.2, 0.0, 0.1));

        // Charged again but walking: no slowdown either.
        for _ in 0..20 {
            scene.tick(0, false);
        }
        scene.player.set_sprinting(false);
        scene.tick(1, false);
        assert_eq!(scene.player.delta_movement, Vec3::new(0.2, 0.0, 0.1));
    }

    /// Swinging at nothing empties the charge too; changing the held item
    /// does as well.
    #[test]
    fn attack_charge() {
        let mut scene = Scene::new();
        scene.player.x_rot = -60.0;
        for _ in 0..10 {
            scene.tick(0, false);
        }
        assert_eq!(scene.mode.attack_strength(&scene.player, 0.0), 1.0);
        scene.tick(1, false);
        assert_eq!(scene.mode.attack_strength(&scene.player, 0.0), 0.2);
        for _ in 0..10 {
            scene.tick(0, false);
        }
        // Hotbar slot 0: one diamond sword, no component changes.
        let mut body = vec![0u8, 1];
        rapidbot_buf::write_var_int(
            &mut body,
            ItemData::get().item_id("minecraft:diamond_sword").unwrap() as i32,
        );
        body.extend_from_slice(&[0, 0]);
        scene.inventory.set_player_inventory(&body).unwrap();
        scene.tick(0, false);
        assert_eq!(scene.mode.attack_strength(&scene.player, 0.0), 0.0);
    }

    /// A right click on an entity sends `Interact` with the hit point
    /// relative to the entity, and falls through to the off hand.
    #[test]
    fn right_clicks_entities() {
        let mut scene = Scene::new();
        scene.player.x_rot = 0.0;
        spawn(&mut scene, "minecraft:zombie", [2.5, 64.0, 0.5]);
        let packets = scene.tick_use(true, true);
        assert_eq!(
            packets.iter().map(|p| p.0).collect::<Vec<_>>(),
            [Interact::ID, Interact::ID]
        );
        let main = Interact::decode(&mut &packets[0].1[..]).unwrap();
        assert_eq!((main.entity_id, main.hand), (7, 0));
        assert!(!main.using_secondary_action);
        assert!((main.location.x + 0.3).abs() < 0.01);
        assert!((main.location.y - 1.62).abs() < 0.02);
        assert!(main.location.z.abs() < 0.01);
        assert_eq!(Interact::decode(&mut &packets[1].1[..]).unwrap().hand, 1);
    }

    /// Item frames and interaction entities are picked with their real
    /// boxes, rather than being left out of entity picking.
    #[test]
    fn picks_item_frames_and_interactions() {
        let mut scene = Scene::new();
        scene.player.x_rot = 0.0;
        spawn(&mut scene, "minecraft:item_frame", [2.0, 65.5, 0.5]);
        let hit = pick(&scene.player, &scene.world, &scene.entities, 1, &scene.tags);
        assert_eq!(hit.entity.map(|e| e.id), Some(7));

        let mut scene = Scene::new();
        scene.player.x_rot = 0.0;
        spawn(&mut scene, "minecraft:interaction", [2.0, 64.0, 0.5]);
        let mut data = vec![8u8, 3];
        2.0f32.encode(&mut data);
        data.extend_from_slice(&[9, 3]);
        3.0f32.encode(&mut data);
        data.push(0xff);
        scene.entities.set_entity_data(7, &data).unwrap();
        let hit = pick(&scene.player, &scene.world, &scene.entities, 1, &scene.tags);
        assert_eq!(hit.entity.map(|e| e.id), Some(7));
    }

    /// A block item predicts the block it places and sends that prediction's
    /// sequence; the server's answer is held until its acknowledgement.
    #[test]
    fn use_on_a_block_predicts_placement() {
        let mut scene = Scene::new();
        scene.set_item(36, "minecraft:stone");
        let packets = scene.tick_use(true, true);
        assert_eq!(
            packets.iter().map(|p| p.0).collect::<Vec<_>>(),
            [UseItemOn::ID]
        );
        let use_on = UseItemOn::decode(&mut &packets[0].1[..]).unwrap();
        assert_eq!(use_on.sequence, 1);
        let stone = state("minecraft:stone");
        assert_eq!(scene.world.block_state(1, 65, 0), Some(stone));

        // The server disagrees. Its update waits for the ack, then wins.
        assert!(scene.mode.server_block((1, 65, 0), AIR));
        assert_eq!(scene.world.block_state(1, 65, 0), Some(stone));
        scene
            .mode
            .block_changed_ack(1, &mut scene.world, &mut scene.player);
        assert_eq!(scene.world.block_state(1, 65, 0), Some(AIR));
    }

    /// A held use button repeats only after the four-tick right-click
    /// delay, as `Minecraft.startUseItem` sets it.
    #[test]
    fn use_button_waits_between_held_clicks() {
        let mut scene = Scene::new();
        let first = scene.tick_use(true, true);
        assert_eq!(
            first.iter().map(|p| p.0).collect::<Vec<_>>(),
            [UseItemOn::ID]
        );
        assert!(scene.tick_use(true, false).is_empty());
        for _ in 0..4 {
            scene.mode.tick();
        }
        let repeat = scene.tick_use(true, false);
        assert_eq!(
            repeat.iter().map(|p| p.0).collect::<Vec<_>>(),
            [UseItemOn::ID]
        );
    }

    /// With a block item in the main hand, the off hand still gets its
    /// `UseItem`; with a bow in the main hand it does not.
    #[test]
    fn use_button_tries_both_hands() {
        let mut scene = Scene::new();
        scene.player.x_rot = -90.0;
        scene.player.y_rot = 30.0;
        scene.set_item(36, "minecraft:stone");
        scene.set_item(45, "minecraft:shield");
        let packets = scene.tick_use(true, true);
        assert_eq!(
            packets.iter().map(|p| p.0).collect::<Vec<_>>(),
            [UseItem::ID, UseItem::ID]
        );
        let main = UseItem::decode(&mut &packets[0].1[..]).unwrap();
        let off = UseItem::decode(&mut &packets[1].1[..]).unwrap();
        assert_eq!(
            (main.hand, main.sequence, main.y_rot, main.x_rot),
            (0, 1, 30.0, -90.0)
        );
        assert_eq!((off.hand, off.sequence), (1, 2));

        let mut scene = Scene::new();
        scene.player.x_rot = -90.0;
        scene.set_item(36, "minecraft:bow");
        scene.set_item(45, "minecraft:shield");
        let packets = scene.tick_use(true, true);
        assert_eq!(
            packets.iter().map(|p| p.0).collect::<Vec<_>>(),
            [UseItem::ID]
        );
        assert_eq!(UseItem::decode(&mut &packets[0].1[..]).unwrap().hand, 0);
    }

    /// Letting go while using sends the release action, then stops locally.
    #[test]
    fn releasing_use_item_sends_player_action() {
        let mut scene = Scene::new();
        scene.player.start_using_item(false);
        let packets = scene.tick_use(false, false);
        assert_eq!(
            packets.iter().map(|p| p.0).collect::<Vec<_>>(),
            [PlayerAction::ID]
        );
        let release = action(&packets[0].1);
        assert_eq!(
            (
                release.action,
                release.pos,
                release.direction,
                release.sequence
            ),
            (
                PlayerActionKind::ReleaseUseItem,
                0,
                Direction::Down as u8,
                0
            )
        );
        assert!(!scene.player.using_item);
    }

    #[test]
    fn waterlogged_blocks_leave_water() {
        let registry = Registry::get();
        let slab = registry.block_by_name("minecraft:oak_slab").unwrap().id;
        let wet = (0..registry.state_count() as StateId)
            .find(|s| {
                registry.state(*s).block == slab
                    && registry.state(*s).properties.contains("waterlogged=true")
            })
            .unwrap();
        let left = fluid_block(wet);
        assert_eq!(registry.block_of(left).name, "minecraft:water");
        assert_eq!(registry.state(left).properties, "level=0");
        assert_eq!(fluid_block(state("minecraft:stone")), AIR);
    }
}
