//! Block placement prediction: the client half of `BlockItem.useOn`.
//!
//! The server is authoritative, but `MultiPlayerGameMode` predicts block
//! changes as soon as the click is sent. The prediction has to match the
//! state the server will place, otherwise the acknowledged state replaces a
//! visible wrong block for a tick. This follows `BlockPlaceContext`,
//! `BlockItem`, and the common `Block.getStateForPlacement` implementations.

use rapidbot_buf::{Decode, read_var_int};
use rapidbot_physics::math;
use rapidbot_physics::player::Player;
use rapidbot_world::item::ItemStack;
use rapidbot_world::raycast::{BlockHit, Direction};
use rapidbot_world::registry::BlockInfo;
use rapidbot_world::tags::Tags;
use rapidbot_world::{Registry, StateId, World};

type BlockPos = (i32, i32, i32);

/// A predicted block change. The first one is the clicked position; the
/// rest are the upper/head half of doors, beds, and double plants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlockChange {
    pub pos: BlockPos,
    pub state: StateId,
}

const DEG_TO_RAD: f32 = (std::f64::consts::PI / 180.0) as f32;
const FACE_STURDY_NORTH: u16 = 1;
const FACE_STURDY_EAST: u16 = 2;
const FACE_STURDY_SOUTH: u16 = 4;
const FACE_STURDY_WEST: u16 = 8;
const FACE_FULL_BLOCK: u16 = 32;
const FACE_STURDY_UP: u16 = 128;
const FACE_STURDY_DOWN: u16 = 256;
const FACE_SOLID: u16 = 512;
const FACE_CENTER_NORTH: u16 = 1 << 10;
const FACE_CENTER_EAST: u16 = 1 << 11;
const FACE_CENTER_SOUTH: u16 = 1 << 12;
const FACE_CENTER_WEST: u16 = 1 << 13;
const FACE_CENTER_UP: u16 = 1 << 14;
const FACE_CENTER_DOWN: u16 = 1 << 15;

fn offset(pos: BlockPos, direction: Direction) -> BlockPos {
    let (dx, dy, dz) = direction.normal();
    (pos.0 + dx, pos.1 + dy, pos.2 + dz)
}

fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Down => "down",
        Direction::Up => "up",
        Direction::North => "north",
        Direction::South => "south",
        Direction::West => "west",
        Direction::East => "east",
    }
}

fn direction_from_name(name: &str) -> Option<Direction> {
    Some(match name {
        "down" => Direction::Down,
        "up" => Direction::Up,
        "north" => Direction::North,
        "south" => Direction::South,
        "west" => Direction::West,
        "east" => Direction::East,
        _ => return None,
    })
}

fn block_name(registry: &Registry, state: StateId) -> &str {
    registry
        .block_of(state)
        .name
        .strip_prefix("minecraft:")
        .unwrap_or("")
}

fn short_name(block: &BlockInfo) -> &str {
    block.name.strip_prefix("minecraft:").unwrap_or(&block.name)
}

fn with(state: StateId, properties: &[(&str, &str)]) -> StateId {
    Registry::get().state_with_properties(state, properties)
}

fn property<'a>(registry: &'a Registry, state: StateId, key: &str) -> Option<&'a str> {
    registry.property(state, key)
}

fn int_property(registry: &Registry, state: StateId, key: &str) -> Option<i32> {
    property(registry, state, key)?.parse().ok()
}

fn face_flag(registry: &Registry, state: StateId, flag: u16) -> bool {
    registry.state(state).face_flags & flag != 0
}

fn sturdy(registry: &Registry, state: StateId, direction: Direction) -> bool {
    let flag = match direction {
        Direction::North => FACE_STURDY_NORTH,
        Direction::East => FACE_STURDY_EAST,
        Direction::South => FACE_STURDY_SOUTH,
        Direction::West => FACE_STURDY_WEST,
        Direction::Up => FACE_STURDY_UP,
        Direction::Down => FACE_STURDY_DOWN,
    };
    face_flag(registry, state, flag)
}

fn center_sturdy(registry: &Registry, state: StateId, direction: Direction) -> bool {
    let flag = match direction {
        Direction::North => FACE_CENTER_NORTH,
        Direction::East => FACE_CENTER_EAST,
        Direction::South => FACE_CENTER_SOUTH,
        Direction::West => FACE_CENTER_WEST,
        Direction::Up => FACE_CENTER_UP,
        Direction::Down => FACE_CENTER_DOWN,
    };
    face_flag(registry, state, flag)
}

fn full_block(registry: &Registry, state: StateId) -> bool {
    face_flag(registry, state, FACE_FULL_BLOCK)
}

fn full_face(registry: &Registry, state: StateId, direction: Direction) -> bool {
    let Some(shape) = registry.collision_shape(state) else {
        return false;
    };
    shape.is_face_full(
        direction.axis(),
        matches!(
            direction,
            Direction::Up | Direction::East | Direction::South
        ),
    )
}

fn solid(registry: &Registry, state: StateId) -> bool {
    face_flag(registry, state, FACE_SOLID)
}

/// `Direction.orderedByNearest`, including its float arithmetic and tie
/// breaking, because placement rules use the first direction.
fn ordered_by_nearest(player: &Player) -> [Direction; 6] {
    let pitch = player.x_rot * DEG_TO_RAD;
    let yaw = -player.y_rot * DEG_TO_RAD;
    let pitch_sin = math::sin(pitch as f64);
    let pitch_cos = math::cos(pitch as f64);
    let yaw_sin = math::sin(yaw as f64);
    let yaw_cos = math::cos(yaw as f64);
    let x_pos = yaw_sin > 0.0;
    let y_pos = pitch_sin < 0.0;
    let z_pos = yaw_cos > 0.0;
    let x_yaw = if x_pos { yaw_sin } else { -yaw_sin };
    let y_mag = if y_pos { -pitch_sin } else { pitch_sin };
    let z_yaw = if z_pos { yaw_cos } else { -yaw_cos };
    let x_mag = x_yaw * pitch_cos;
    let z_mag = z_yaw * pitch_cos;
    let axis_x = if x_pos {
        Direction::East
    } else {
        Direction::West
    };
    let axis_y = if y_pos {
        Direction::Up
    } else {
        Direction::Down
    };
    let axis_z = if z_pos {
        Direction::South
    } else {
        Direction::North
    };
    let make = |a: Direction, b: Direction, c: Direction| {
        [a, b, c, c.opposite(), b.opposite(), a.opposite()]
    };
    if x_yaw > z_yaw {
        if y_mag > x_mag {
            make(axis_y, axis_x, axis_z)
        } else if z_mag > y_mag {
            make(axis_x, axis_z, axis_y)
        } else {
            make(axis_x, axis_y, axis_z)
        }
    } else if y_mag > z_mag {
        make(axis_y, axis_z, axis_x)
    } else if x_mag > y_mag {
        make(axis_z, axis_x, axis_y)
    } else {
        make(axis_z, axis_y, axis_x)
    }
}

/// `Direction.fromYRot`.
fn horizontal_direction(y_rot: f32) -> Direction {
    let index = ((y_rot / 90.0 + 0.5).floor() as i32) & 3;
    match index {
        0 => Direction::South,
        1 => Direction::West,
        2 => Direction::North,
        _ => Direction::East,
    }
}

/// `RotationSegment.convertToSegment(float)`.
fn rotation_segment(degrees: f32) -> i32 {
    ((degrees * (16.0 / 360.0)) + 0.5).floor() as i32 & 15
}

/// `RotationSegment.convertToSegment(Direction)` for horizontal
/// directions: `get2DDataValue() << 2`.
fn segment_for_direction(direction: Direction) -> i32 {
    match direction {
        Direction::South => 0,
        Direction::West => 4,
        Direction::North => 8,
        Direction::East => 12,
        _ => 0,
    }
}

fn segment_to_direction(segment: i32) -> Option<Direction> {
    Some(match segment {
        0 => Direction::North,
        4 => Direction::East,
        8 => Direction::South,
        12 => Direction::West,
        _ => return None,
    })
}

struct PlaceContext<'a> {
    world: &'a World,
    player: &'a Player,
    item: &'a ItemStack,
    hit: &'a BlockHit,
    tags: &'a Tags,
    secondary: bool,
    replace_clicked: bool,
    pos: BlockPos,
    nearest: [Direction; 6],
}

impl<'a> PlaceContext<'a> {
    fn new(
        world: &'a World,
        player: &'a Player,
        item: &'a ItemStack,
        hit: &'a BlockHit,
        tags: &'a Tags,
        secondary: bool,
    ) -> Option<Self> {
        let replace_clicked = can_be_replaced(world, item, tags, secondary, hit, hit.pos, true);
        let pos = if replace_clicked {
            hit.pos
        } else {
            offset(hit.pos, hit.direction)
        };
        if pos.1 < world.height.min_y || pos.1 > world.height.max_y() {
            return None;
        }
        if !replace_clicked && !can_be_replaced(world, item, tags, secondary, hit, pos, false) {
            return None;
        }
        let mut nearest = ordered_by_nearest(player);
        if !replace_clicked {
            let wanted = hit.direction.opposite();
            if let Some(index) = nearest.iter().position(|d| *d == wanted) {
                nearest.copy_within(0..index, 1);
                nearest[0] = wanted;
            }
        }
        Some(Self {
            world,
            player,
            item,
            hit,
            tags,
            secondary,
            replace_clicked,
            pos,
            nearest,
        })
    }

    fn registry(&self) -> &'static Registry {
        Registry::get()
    }

    fn state_at(&self, pos: BlockPos) -> StateId {
        self.world.block_state_or_air(pos.0, pos.1, pos.2)
    }

    fn water_at(&self, pos: BlockPos) -> bool {
        self.registry().state(self.state_at(pos)).fluid == "minecraft:water"
    }

    fn water_source_at(&self, pos: BlockPos) -> bool {
        let state = self.registry().state(self.state_at(pos));
        state.fluid == "minecraft:water" && state.fluid_amount >= 8
    }

    fn horizontal(&self) -> Direction {
        horizontal_direction(self.player.y_rot)
    }

    fn nearest(&self) -> Direction {
        self.nearest[0]
    }

    fn click_y(&self) -> f64 {
        self.hit.location[1] - self.pos.1 as f64
    }

    fn click_x(&self) -> f64 {
        self.hit.location[0] - self.pos.0 as f64
    }

    fn click_z(&self) -> f64 {
        self.hit.location[2] - self.pos.2 as f64
    }

    fn can_replace(&self, pos: BlockPos, replacing_clicked: bool) -> bool {
        can_be_replaced(
            self.world,
            self.item,
            self.tags,
            self.secondary,
            self.hit,
            pos,
            replacing_clicked,
        )
    }

    fn place_state(&self, state: StateId) -> Option<Vec<BlockChange>> {
        let registry = self.registry();
        if let Some(shape) = registry.collision_shape(state)
            && shape
                .at(self.pos.0, self.pos.1, self.pos.2)
                .intersects_box(&self.player.bounding_box())
        {
            return None;
        }
        let mut changes = vec![BlockChange {
            pos: self.pos,
            state,
        }];
        self.add_secondary_blocks(state, &mut changes);
        Some(changes)
    }

    fn add_secondary_blocks(&self, state: StateId, changes: &mut Vec<BlockChange>) {
        let registry = self.registry();
        let name = block_name(registry, state);
        let lower = self.pos.1 < self.world.height.max_y();
        if name.ends_with("_door") && property(registry, state, "half") == Some("lower") && lower {
            let upper_pos = (self.pos.0, self.pos.1 + 1, self.pos.2);
            if self.can_replace(upper_pos, self.replace_clicked) {
                let upper = with(state, &[("half", "upper")]);
                changes.push(BlockChange {
                    pos: upper_pos,
                    state: upper,
                });
            }
        } else if name.ends_with("_bed") && property(registry, state, "part") == Some("foot") {
            let facing = property(registry, state, "facing")
                .and_then(direction_from_name)
                .unwrap_or(Direction::North);
            let head_pos = offset(self.pos, facing);
            if head_pos.1 >= self.world.height.min_y
                && head_pos.1 <= self.world.height.max_y()
                && self.can_replace(head_pos, self.replace_clicked)
            {
                let head = with(state, &[("part", "head")]);
                changes.push(BlockChange {
                    pos: head_pos,
                    state: head,
                });
            }
        } else if is_double_plant(name)
            && property(registry, state, "half") == Some("lower")
            && lower
        {
            let upper_pos = (self.pos.0, self.pos.1 + 1, self.pos.2);
            if self.can_replace(upper_pos, self.replace_clicked) {
                let block = registry.block_of(state);
                let mut upper = with(block.default_state, &[("half", "upper")]);
                if property(registry, upper, "waterlogged").is_some() {
                    upper = with(
                        upper,
                        &[(
                            "waterlogged",
                            if self.water_at(upper_pos) {
                                "true"
                            } else {
                                "false"
                            },
                        )],
                    );
                }
                changes.push(BlockChange {
                    pos: upper_pos,
                    state: upper,
                });
            }
        } else if name == "small_dripleaf"
            && property(registry, state, "half") == Some("lower")
            && lower
        {
            let upper_pos = (self.pos.0, self.pos.1 + 1, self.pos.2);
            if self.can_replace(upper_pos, self.replace_clicked) {
                let facing = property(registry, state, "facing").unwrap_or("north");
                let block = registry.block_of(state);
                let upper = with(
                    block.default_state,
                    &[
                        ("half", "upper"),
                        ("facing", facing),
                        (
                            "waterlogged",
                            if self.water_at(upper_pos) {
                                "true"
                            } else {
                                "false"
                            },
                        ),
                    ],
                );
                changes.push(BlockChange {
                    pos: upper_pos,
                    state: upper,
                });
            }
        }
    }
}

/// Predict the block changes a right-click with `item` will make, or `None`
/// if vanilla would not place a block.
pub(crate) fn predict(
    world: &World,
    player: &Player,
    item: &ItemStack,
    hit: &BlockHit,
    tags: &Tags,
    secondary: bool,
) -> Option<Vec<BlockChange>> {
    if hit.miss {
        return None;
    }
    let context = PlaceContext::new(world, player, item, hit, tags, secondary)?;
    let registry = Registry::get();
    let block = registry.block_for_item(item.item)?;
    let state = placement_state_for_item(&context, block)?;
    let state = apply_component(registry, state, item);
    context.place_state(state)
}

/// `StandingAndWallBlockItem.getPlacementState`, or `BlockItem` with the
/// block's own `getStateForPlacement`.
fn placement_state_for_item(context: &PlaceContext<'_>, block: &BlockInfo) -> Option<StateId> {
    let registry = Registry::get();
    if let (Some(wall), Some(attachment)) = (
        registry.wall_block_for_item(context.item.item),
        registry.wall_attachment_direction(context.item.item),
    ) {
        for direction in context.nearest {
            if direction == attachment.opposite() {
                continue;
            }
            let candidate = if direction == attachment { block } else { wall };
            if let Some(state) = placement_state(context, candidate)
                && attachment_can_survive(context, state)
            {
                return Some(state);
            }
        }
        return None;
    }
    placement_state(context, block)
}

/// `StandingAndWallBlockItem.canPlace` for the standing variants. Wall
/// variants were checked by `wall_attachment` already.
fn attachment_can_survive(context: &PlaceContext<'_>, state: StateId) -> bool {
    let registry = Registry::get();
    let name = block_name(registry, state);
    if is_wall_attachment(name) {
        return true;
    }
    if name.ends_with("_hanging_sign") {
        let above = (context.pos.0, context.pos.1 + 1, context.pos.2);
        return center_sturdy(registry, context.state_at(above), Direction::Down);
    }
    if name.ends_with("_sign") || name.ends_with("_banner") {
        let below = (context.pos.0, context.pos.1 - 1, context.pos.2);
        return solid(registry, context.state_at(below));
    }
    if matches!(
        name,
        "torch" | "soul_torch" | "redstone_torch" | "copper_torch"
    ) {
        let below = (context.pos.0, context.pos.1 - 1, context.pos.2);
        return center_sturdy(registry, context.state_at(below), Direction::Up);
    }
    if name.ends_with("_coral_fan") {
        let below = (context.pos.0, context.pos.1 - 1, context.pos.2);
        return sturdy(registry, context.state_at(below), Direction::Up);
    }
    true
}

fn placement_state(context: &PlaceContext<'_>, block: &BlockInfo) -> Option<StateId> {
    let registry = Registry::get();
    let base = block.default_state;
    let name = short_name(block);
    let has = |key: &str| property(registry, base, key).is_some();
    let waterlogged = |state| {
        if property(registry, state, "waterlogged").is_some() {
            with(
                state,
                &[(
                    "waterlogged",
                    if context.water_at(context.pos) {
                        "true"
                    } else {
                        "false"
                    },
                )],
            )
        } else {
            state
        }
    };
    let source_waterlogged = |state| {
        if property(registry, state, "waterlogged").is_some() {
            with(
                state,
                &[(
                    "waterlogged",
                    if context.water_source_at(context.pos) {
                        "true"
                    } else {
                        "false"
                    },
                )],
            )
        } else {
            state
        }
    };

    // Doors and beds place a second block and need room above/forward.
    if has("hinge") {
        if context.pos.1 >= context.world.height.max_y()
            || !context.can_replace(
                (context.pos.0, context.pos.1 + 1, context.pos.2),
                context.replace_clicked,
            )
        {
            return None;
        }
        let facing = context.horizontal();
        let hinge = door_hinge(context, facing);
        return Some(waterlogged(with(
            base,
            &[
                ("facing", direction_name(facing)),
                ("hinge", hinge),
                ("open", "false"),
                ("powered", "false"),
                ("half", "lower"),
            ],
        )));
    }
    if name.ends_with("_bed") && has("part") {
        let facing = context.horizontal();
        let head = offset(context.pos, facing);
        if head.1 < context.world.height.min_y
            || head.1 > context.world.height.max_y()
            || !context.can_replace(head, context.replace_clicked)
        {
            return None;
        }
        return Some(with(
            base,
            &[
                ("facing", direction_name(facing)),
                ("occupied", "false"),
                ("part", "foot"),
            ],
        ));
    }
    if is_double_plant(name) && has("half") {
        let above = (context.pos.0, context.pos.1 + 1, context.pos.2);
        if context.pos.1 >= context.world.height.max_y()
            || !context.can_replace(above, context.replace_clicked)
        {
            return None;
        }
        let below = (context.pos.0, context.pos.1 - 1, context.pos.2);
        let below_state = context.state_at(below);
        let below_block = registry.state(below_state).block;
        let supported = if name == "tall_seagrass" {
            let below_ok = sturdy(registry, below_state, Direction::Up)
                && !context
                    .tags
                    .block_is("minecraft:cannot_support_seagrass", below_block);
            let water_below = {
                let state = registry.state(context.state_at(context.pos));
                state.fluid == "minecraft:water" && state.fluid_amount >= 8
            };
            let water_above = {
                let state = registry.state(context.state_at(above));
                state.fluid == "minecraft:water" && state.fluid_amount >= 8
            };
            below_ok && water_below && water_above
        } else {
            context
                .tags
                .block_is("minecraft:supports_vegetation", below_block)
        };
        if !supported {
            return None;
        }
        return Some(waterlogged(with(base, &[("half", "lower")])));
    }
    if name == "small_dripleaf" && has("half") {
        let above = (context.pos.0, context.pos.1 + 1, context.pos.2);
        if context.pos.1 >= context.world.height.max_y()
            || !context.can_replace(above, context.replace_clicked)
        {
            return None;
        }
        let below = (context.pos.0, context.pos.1 - 1, context.pos.2);
        let below_state = context.state_at(below);
        let below_block = registry.state(below_state).block;
        let water_above = {
            let state = registry.state(context.state_at(above));
            state.fluid == "minecraft:water" && state.fluid_amount >= 8
        };
        if !context
            .tags
            .block_is("minecraft:supports_small_dripleaf", below_block)
            && !(water_above
                && context
                    .tags
                    .block_is("minecraft:supports_vegetation", below_block))
        {
            return None;
        }
        return Some(waterlogged(with(
            base,
            &[
                ("half", "lower"),
                ("facing", direction_name(context.horizontal().opposite())),
            ],
        )));
    }

    if matches!(name, "campfire" | "soul_campfire") && has("facing") {
        let water = context.water_at(context.pos);
        let below = (context.pos.0, context.pos.1 - 1, context.pos.2);
        let signal = block_name(registry, context.state_at(below)) == "hay_block";
        return Some(with(
            base,
            &[
                ("facing", direction_name(context.horizontal())),
                ("lit", if water { "false" } else { "true" }),
                ("signal_fire", if signal { "true" } else { "false" }),
                ("waterlogged", if water { "true" } else { "false" }),
            ],
        ));
    }

    if name == "big_dripleaf" && has("facing") {
        let below = (context.pos.0, context.pos.1 - 1, context.pos.2);
        let below_state = context.state_at(below);
        let below_name = block_name(registry, below_state);
        let facing = if matches!(below_name, "big_dripleaf" | "big_dripleaf_stem") {
            property(registry, below_state, "facing")
                .and_then(direction_from_name)
                .unwrap_or_else(|| context.horizontal().opposite())
        } else {
            context.horizontal().opposite()
        };
        let supported = matches!(below_name, "big_dripleaf" | "big_dripleaf_stem")
            || context.tags.block_is(
                "minecraft:supports_big_dripleaf",
                registry.state(below_state).block,
            );
        if !supported {
            return None;
        }
        return Some(source_waterlogged(with(
            base,
            &[("facing", direction_name(facing))],
        )));
    }

    if name == "end_rod" && has("facing") {
        let clicked = context.hit.direction;
        let support = context.state_at(offset(context.pos, clicked.opposite()));
        let facing = if block_name(registry, support) == "end_rod"
            && property(registry, support, "facing") == Some(direction_name(clicked))
        {
            clicked.opposite()
        } else {
            clicked
        };
        return Some(with(base, &[("facing", direction_name(facing))]));
    }

    if name == "tripwire_hook" && has("facing") {
        for direction in context.nearest {
            if !direction.is_horizontal() {
                continue;
            }
            let support = context.state_at(offset(context.pos, direction));
            if sturdy(registry, support, direction.opposite()) {
                return Some(with(
                    base,
                    &[
                        ("facing", direction_name(direction.opposite())),
                        ("attached", "false"),
                        ("powered", "false"),
                    ],
                ));
            }
        }
        return None;
    }

    if name == "cocoa" && has("facing") {
        for direction in context.nearest {
            if !direction.is_horizontal() {
                continue;
            }
            let support = context.state_at(offset(context.pos, direction));
            if context
                .tags
                .block_is("minecraft:supports_cocoa", registry.state(support).block)
            {
                return Some(with(base, &[("facing", direction_name(direction))]));
            }
        }
        return None;
    }

    if name.ends_with("_stairs") && has("shape") {
        let facing = context.horizontal();
        let half = if context.hit.direction != Direction::Down
            && (context.hit.direction == Direction::Up || context.click_y() <= 0.5)
        {
            "bottom"
        } else {
            "top"
        };
        let mut state = waterlogged(with(
            base,
            &[("facing", direction_name(facing)), ("half", half)],
        ));
        let shape = stairs_shape(context, state, facing);
        state = with(state, &[("shape", shape)]);
        return Some(state);
    }
    if name.ends_with("_slab") && has("type") {
        let current = context.state_at(context.pos);
        if registry.state(current).block == block.id
            && property(registry, current, "type") != Some("double")
        {
            return Some(with(
                current,
                &[("type", "double"), ("waterlogged", "false")],
            ));
        }
        let half = if context.hit.direction != Direction::Down
            && (context.hit.direction == Direction::Up || context.click_y() <= 0.5)
        {
            "bottom"
        } else {
            "top"
        };
        return Some(waterlogged(with(base, &[("type", half)])));
    }
    if name.ends_with("_trapdoor") && has("half") {
        let (facing, half) = if !context.replace_clicked && context.hit.direction.is_horizontal() {
            (
                context.hit.direction,
                if context.click_y() > 0.5 {
                    "top"
                } else {
                    "bottom"
                },
            )
        } else {
            (
                context.horizontal().opposite(),
                if context.hit.direction == Direction::Up {
                    "bottom"
                } else {
                    "top"
                },
            )
        };
        return Some(waterlogged(with(
            base,
            &[
                ("facing", direction_name(facing)),
                ("half", half),
                ("open", "false"),
                ("powered", "false"),
            ],
        )));
    }
    if name.ends_with("_fence_gate") && has("in_wall") {
        let facing = context.horizontal();
        let axis = facing.axis();
        let wall = |pos: BlockPos| {
            let state = context.state_at(pos);
            context
                .tags
                .block_is("minecraft:walls", registry.state(state).block)
        };
        let in_wall = match axis {
            rapidbot_world::Axis::Z => {
                wall(offset(context.pos, Direction::West))
                    || wall(offset(context.pos, Direction::East))
            }
            rapidbot_world::Axis::X => {
                wall(offset(context.pos, Direction::North))
                    || wall(offset(context.pos, Direction::South))
            }
            rapidbot_world::Axis::Y => false,
        };
        return Some(with(
            base,
            &[
                ("facing", direction_name(facing)),
                ("open", "false"),
                ("powered", "false"),
                ("in_wall", if in_wall { "true" } else { "false" }),
            ],
        ));
    }

    if name == "ladder" && !context.replace_clicked {
        // `LadderBlock.getStateForPlacement` returns null before trying any
        // direction when the clicked support is a ladder facing this face.
        let clicked = context.state_at(context.hit.pos);
        if block_name(registry, clicked) == "ladder"
            && property(registry, clicked, "facing") == Some(direction_name(context.hit.direction))
        {
            return None;
        }
    }
    if is_wall_attachment(name) && has("facing") {
        return wall_attachment(context, base, name);
    }

    // Standing signs/banners use rotation + 180; skulls use rotation; hanging
    // signs either attach to a ceiling or to a wall.
    if has("rotation") && has("attached") {
        return hanging_sign(context, base);
    }
    if has("rotation") {
        let segment = if name.ends_with("_sign") || name.ends_with("_banner") {
            rotation_segment(context.player.y_rot + 180.0)
        } else {
            rotation_segment(context.player.y_rot)
        };
        let mut state = with(base, &[("rotation", &segment.to_string())]);
        if has("powered") {
            state = with(state, &[("powered", "false")]);
        }
        return Some(waterlogged(state));
    }

    if has("face") && has("facing") {
        return face_attached(context, base);
    }
    if has("axis") {
        return Some(waterlogged(with(
            base,
            &[("axis", axis_name(context.hit.direction.axis()))],
        )));
    }

    if name == "lantern" || name.ends_with("_lantern") {
        return lantern(context, base);
    }
    if (name == "lightning_rod" || name.ends_with("_lightning_rod")) && has("facing") {
        return Some(waterlogged(with(
            base,
            &[
                ("facing", direction_name(context.hit.direction)),
                ("powered", "false"),
            ],
        )));
    }
    if (name.ends_with("_amethyst_bud") || name.ends_with("_amethyst_cluster")) && has("facing") {
        let support = context.state_at(offset(context.pos, context.hit.direction.opposite()));
        if !sturdy(registry, support, context.hit.direction) {
            return None;
        }
        return Some(waterlogged(with(
            base,
            &[("facing", direction_name(context.hit.direction))],
        )));
    }
    if name.ends_with("_shulker_box") && has("facing") {
        return Some(with(
            base,
            &[("facing", direction_name(context.hit.direction))],
        ));
    }

    if (name == "chest" || name.ends_with("_chest")) && has("type") {
        return Some(chest(context, base));
    }
    if name == "ender_chest" {
        return Some(waterlogged(with(
            base,
            &[("facing", direction_name(context.horizontal().opposite()))],
        )));
    }

    if name == "snow" && has("layers") {
        if !snow_can_survive(context) {
            return None;
        }
        let current = context.state_at(context.pos);
        if registry.state(current).block == block.id {
            let layers = int_property(registry, current, "layers").unwrap_or(1);
            return Some(with(
                current,
                &[("layers", &(layers + 1).min(8).to_string())],
            ));
        }
        return Some(with(base, &[("layers", "1")]));
    }
    if (name == "candle" || name.ends_with("_candle")) && has("candles") {
        let below = (context.pos.0, context.pos.1 - 1, context.pos.2);
        if !center_sturdy(registry, context.state_at(below), Direction::Up) {
            return None;
        }
        let current = context.state_at(context.pos);
        if registry.state(current).block == block.id {
            let candles = int_property(registry, current, "candles").unwrap_or(1);
            return Some(with(
                current,
                &[("candles", &(candles + 1).min(4).to_string())],
            ));
        }
        return Some(waterlogged(with(base, &[("candles", "1")])));
    }
    if name == "sea_pickle" && has("pickles") {
        let current = context.state_at(context.pos);
        if registry.state(current).block == block.id {
            let pickles = int_property(registry, current, "pickles").unwrap_or(1);
            return Some(with(
                current,
                &[("pickles", &(pickles + 1).min(4).to_string())],
            ));
        }
        return Some(waterlogged(with(base, &[("pickles", "1")])));
    }
    if name == "turtle_egg" && has("eggs") {
        let current = context.state_at(context.pos);
        if registry.state(current).block == block.id {
            let eggs = int_property(registry, current, "eggs").unwrap_or(1);
            return Some(with(current, &[("eggs", &(eggs + 1).min(4).to_string())]));
        }
        return Some(with(base, &[("eggs", "1")]));
    }
    if (has("segment_amount") || has("flower_amount")) && has("facing") {
        let key = if has("segment_amount") {
            "segment_amount"
        } else {
            "flower_amount"
        };
        let current = context.state_at(context.pos);
        if registry.state(current).block == block.id {
            let amount = int_property(registry, current, key).unwrap_or(1);
            return Some(with(current, &[(key, &(amount + 1).min(4).to_string())]));
        }
        return Some(with(
            base,
            &[
                ("facing", direction_name(context.horizontal().opposite())),
                (key, "1"),
            ],
        ));
    }

    if name.ends_with("_sign") || name.ends_with("_banner") {
        // Handled above. This is only reachable for an unexpected property
        // set, where the default state is the safe prediction.
        return Some(base);
    }

    if name == "crafter" && has("orientation") {
        let front = context.nearest().opposite();
        let top = match front {
            Direction::Down => context.horizontal().opposite(),
            Direction::Up => context.horizontal(),
            _ => Direction::Up,
        };
        // `FrontAndTop.fromFrontAndTop`.
        let orientation = if front.axis() == rapidbot_world::Axis::Y {
            format!("{}_{}", direction_name(front), direction_name(top))
        } else {
            format!("{}_up", direction_name(front))
        };
        return Some(with(
            base,
            &[("orientation", &orientation), ("triggered", "false")],
        ));
    }

    let facing = match facing_kind(name) {
        Some(FacingKind::Opposite) => Some(context.horizontal().opposite()),
        Some(FacingKind::Same) => Some(context.horizontal()),
        Some(FacingKind::Clockwise) => Some(context.horizontal().clockwise()),
        Some(FacingKind::Nearest) => Some(context.nearest()),
        Some(FacingKind::NearestOpposite) => Some(context.nearest().opposite()),
        Some(FacingKind::Clicked) => Some(context.hit.direction),
        Some(FacingKind::ClickedOppositeClampedDown) => {
            let direction = context.hit.direction.opposite();
            Some(if direction.is_horizontal() {
                direction
            } else {
                Direction::Down
            })
        }
        None => None,
    };
    if let Some(facing) = facing
        && has("facing")
    {
        let mut state = waterlogged(with(base, &[("facing", direction_name(facing))]));
        for (key, value) in default_booleans(name) {
            if property(registry, state, key).is_some() {
                state = with(state, &[(key, value)]);
            }
        }
        return Some(state);
    }

    // Conservative fallback: the exact vanilla default state. This is right
    // for most blocks (stone, plants, rails, ...); unusual state-dependent
    // blocks are enumerated above.
    Some(waterlogged(base))
}

fn axis_name(axis: rapidbot_world::Axis) -> &'static str {
    match axis {
        rapidbot_world::Axis::X => "x",
        rapidbot_world::Axis::Y => "y",
        rapidbot_world::Axis::Z => "z",
    }
}

enum FacingKind {
    Opposite,
    Same,
    Clockwise,
    Nearest,
    NearestOpposite,
    Clicked,
    ClickedOppositeClampedDown,
}

fn facing_kind(name: &str) -> Option<FacingKind> {
    Some(match name {
        "carved_pumpkin"
        | "jack_o_lantern"
        | "loom"
        | "stonecutter"
        | "vault"
        | "beehive"
        | "bee_nest"
        | "chiseled_bookshelf"
        | "ender_chest"
        | "end_portal_frame"
        | "furnace"
        | "smoker"
        | "blast_furnace"
        | "lectern"
        | "copper_golem_statue"
        | "dried_ghast"
        | "big_dripleaf"
        | "small_dripleaf" => FacingKind::Opposite,
        "decorated_pot" | "campfire" | "soul_campfire" | "calibrated_sculk_sensor" => {
            FacingKind::Same
        }
        "anvil" | "chipped_anvil" | "damaged_anvil" => FacingKind::Clockwise,
        "observer" => FacingKind::Nearest,
        "barrel" | "dispenser" | "dropper" | "piston" | "sticky_piston" => {
            FacingKind::NearestOpposite
        }
        "command_block" | "chain_command_block" | "repeating_command_block" => {
            FacingKind::NearestOpposite
        }
        "hopper" => FacingKind::ClickedOppositeClampedDown,
        "tripwire_hook" => FacingKind::NearestOpposite,
        "repeater" | "comparator" => FacingKind::Opposite,
        "end_rod" | "shulker_box" => FacingKind::Clicked,
        "shelf_mushroom" => FacingKind::NearestOpposite,
        "cocoa" => FacingKind::Nearest,
        _ if name.ends_with("_glazed_terracotta") => FacingKind::Opposite,
        _ if name.ends_with("_shulker_box") => FacingKind::Clicked,
        _ if name.ends_with("_shelf") => FacingKind::Opposite,
        _ if name.ends_with("_furnace") => FacingKind::Opposite,
        _ if name.ends_with("_chest") => FacingKind::Opposite,
        _ if name.ends_with("_anvil") => FacingKind::Clockwise,
        _ => return None,
    })
}

fn default_booleans(name: &str) -> &'static [(&'static str, &'static str)] {
    match name {
        "campfire" | "soul_campfire" => &[("lit", "true"), ("signal_fire", "false")],
        "furnace" | "smoker" | "blast_furnace" => &[("lit", "false")],
        "dispenser" | "dropper" => &[("triggered", "false")],
        "piston" | "sticky_piston" => &[("extended", "false")],
        "observer" => &[("powered", "false")],
        "repeater" => &[("powered", "false"), ("locked", "false")],
        "comparator" => &[("powered", "false"), ("mode", "compare")],
        "lectern" => &[("has_book", "false"), ("powered", "false")],
        "tripwire_hook" => &[("attached", "false"), ("powered", "false")],
        "end_portal_frame" => &[("eye", "false")],
        "shelf_mushroom" => &[("age", "0")],
        _ if name.ends_with("_shelf") => &[("powered", "false")],
        _ if name.ends_with("_command_block") => &[("conditional", "false")],
        _ => &[],
    }
}

fn is_wall_attachment(name: &str) -> bool {
    name == "wall_torch"
        || name.ends_with("_wall_torch")
        || name.ends_with("_wall_sign")
        || name.ends_with("_wall_hanging_sign")
        || name.ends_with("_wall_banner")
        || name.ends_with("_wall_skull")
        || name.ends_with("_wall_head")
        || name == "ladder"
        || name.ends_with("_wall_fan")
}

fn wall_attachment(context: &PlaceContext<'_>, base: StateId, name: &str) -> Option<StateId> {
    let registry = Registry::get();
    let hanging_sign = name.ends_with("_wall_hanging_sign");
    let skull = name.ends_with("_wall_skull") || name.ends_with("_wall_head");
    let needs_solid = name.ends_with("_wall_sign") || name.ends_with("_wall_banner");
    for direction in context.nearest {
        if !direction.is_horizontal() {
            continue;
        }
        if hanging_sign && direction.axis() == context.hit.direction.axis() {
            continue;
        }
        let support_pos = offset(context.pos, direction);
        let support = context.state_at(support_pos);
        let survives = if hanging_sign {
            // `WallHangingSignBlock.canSurvive` is the default `true`; its
            // side attachment is checked below.
            true
        } else if skull {
            // `WallSkullBlock` only needs a neighbour that cannot be replaced.
            !context.can_replace(support_pos, context.replace_clicked)
        } else if needs_solid {
            solid(registry, support)
        } else {
            sturdy(registry, support, direction.opposite())
        };
        if !survives {
            continue;
        }
        let facing = direction.opposite();
        let mut state = with(base, &[("facing", direction_name(facing))]);
        if property(registry, state, "waterlogged").is_some() {
            state = with(
                state,
                &[(
                    "waterlogged",
                    if context.water_at(context.pos) {
                        "true"
                    } else {
                        "false"
                    },
                )],
            );
        }
        if hanging_sign && !wall_hanging_sign_can_place(context, facing) {
            continue;
        }
        return Some(state);
    }
    None
}

/// `WallHangingSignBlock.canPlace`: one of the two side neighbours must be
/// a full face, or another wall hanging sign with a parallel facing.
fn wall_hanging_sign_can_place(context: &PlaceContext<'_>, facing: Direction) -> bool {
    let registry = Registry::get();
    for (side, attach_face) in [
        (facing.clockwise(), facing.counter_clockwise()),
        (facing.counter_clockwise(), facing.clockwise()),
    ] {
        let side_state = context.state_at(offset(context.pos, side));
        let is_hanging_sign = context.tags.block_is(
            "minecraft:wall_hanging_signs",
            registry.state(side_state).block,
        );
        if is_hanging_sign {
            if property(registry, side_state, "facing")
                .and_then(direction_from_name)
                .is_some_and(|neighbour| neighbour.axis() == facing.axis())
            {
                return true;
            }
        } else if sturdy(registry, side_state, attach_face) {
            return true;
        }
    }
    false
}

fn hanging_sign(context: &PlaceContext<'_>, base: StateId) -> Option<StateId> {
    let registry = Registry::get();
    let above_pos = (context.pos.0, context.pos.1 + 1, context.pos.2);
    let above = context.state_at(above_pos);
    let is_hanging_sign = context
        .tags
        .block_is("minecraft:all_hanging_signs", registry.state(above).block);
    let direction = horizontal_direction(context.player.y_rot);
    let mut attached = !full_face(registry, above, Direction::Down) || context.secondary;
    if is_hanging_sign && !context.secondary {
        if let Some(above_facing) =
            property(registry, above, "facing").and_then(direction_from_name)
        {
            if above_facing.axis() == direction.axis() {
                attached = false;
            }
        } else if let Some(rotation) = int_property(registry, above, "rotation")
            && let Some(above_direction) = segment_to_direction(rotation)
            && above_direction.axis() == direction.axis()
        {
            attached = false;
        }
    }
    let segment = if attached {
        rotation_segment(context.player.y_rot + 180.0)
    } else {
        segment_for_direction(direction.opposite())
    };
    let state = with(
        base,
        &[
            ("attached", if attached { "true" } else { "false" }),
            ("rotation", &segment.to_string()),
        ],
    );
    Some(with(
        state,
        &[(
            "waterlogged",
            if context.water_at(context.pos) {
                "true"
            } else {
                "false"
            },
        )],
    ))
}

/// `SnowLayerBlock.canSurvive`.
fn snow_can_survive(context: &PlaceContext<'_>) -> bool {
    let registry = Registry::get();
    let below = (context.pos.0, context.pos.1 - 1, context.pos.2);
    let below_state = context.state_at(below);
    let below_block = registry.state(below_state).block;
    if context
        .tags
        .block_is("minecraft:cannot_support_snow_layer", below_block)
    {
        return false;
    }
    if context
        .tags
        .block_is("minecraft:support_override_snow_layer", below_block)
    {
        return true;
    }
    full_face(registry, below_state, Direction::Up)
        || (block_name(registry, below_state) == "snow"
            && int_property(registry, below_state, "layers") == Some(8))
}

fn face_attached(context: &PlaceContext<'_>, base: StateId) -> Option<StateId> {
    let registry = Registry::get();
    // `GrindstoneBlock.canSurvive` is always true, unlike buttons and levers.
    let grindstone = block_name(registry, base) == "grindstone";
    for direction in context.nearest {
        let (face, facing) = if direction.axis() == rapidbot_world::Axis::Y {
            (
                if direction == Direction::Up {
                    "ceiling"
                } else {
                    "floor"
                },
                context.horizontal(),
            )
        } else {
            ("wall", direction.opposite())
        };
        let support = context.state_at(offset(context.pos, direction));
        if !grindstone && !sturdy(registry, support, direction.opposite()) {
            continue;
        }
        let mut state = with(base, &[("face", face), ("facing", direction_name(facing))]);
        if property(registry, state, "powered").is_some() {
            state = with(state, &[("powered", "false")]);
        }
        return Some(state);
    }
    None
}

fn lantern(context: &PlaceContext<'_>, base: StateId) -> Option<StateId> {
    let registry = Registry::get();
    for direction in context.nearest {
        if direction.axis() != rapidbot_world::Axis::Y {
            continue;
        }
        let hanging = direction == Direction::Up;
        let support_pos = if hanging {
            (context.pos.0, context.pos.1 + 1, context.pos.2)
        } else {
            (context.pos.0, context.pos.1 - 1, context.pos.2)
        };
        let support = context.state_at(support_pos);
        if !center_sturdy(
            registry,
            support,
            if hanging {
                Direction::Down
            } else {
                Direction::Up
            },
        ) {
            continue;
        }
        let state = with(
            base,
            &[
                ("hanging", if hanging { "true" } else { "false" }),
                (
                    "waterlogged",
                    if context.water_at(context.pos) {
                        "true"
                    } else {
                        "false"
                    },
                ),
            ],
        );
        return Some(state);
    }
    None
}

fn door_hinge(context: &PlaceContext<'_>, facing: Direction) -> &'static str {
    let registry = Registry::get();
    let left = facing.counter_clockwise();
    let right = facing.clockwise();
    let above = (context.pos.0, context.pos.1 + 1, context.pos.2);
    let left_pos = offset(context.pos, left);
    let right_pos = offset(context.pos, right);
    let left_above = offset(above, left);
    let right_above = offset(above, right);
    let mut balance = 0;
    if full_block(registry, context.state_at(left_pos)) {
        balance -= 1;
    }
    if full_block(registry, context.state_at(left_above)) {
        balance -= 1;
    }
    if full_block(registry, context.state_at(right_pos)) {
        balance += 1;
    }
    if full_block(registry, context.state_at(right_above)) {
        balance += 1;
    }
    let door_left = is_lower_door(registry, context.state_at(left_pos));
    let door_right = is_lower_door(registry, context.state_at(right_pos));
    if (!door_left || door_right) && balance <= 0 {
        if (!door_right || door_left) && balance >= 0 {
            let (step_x, _, step_z) = facing.normal();
            let click_x = context.click_x();
            let click_z = context.click_z();
            return if (step_x >= 0 || click_z >= 0.5)
                && (step_x <= 0 || click_z <= 0.5)
                && (step_z >= 0 || click_x <= 0.5)
                && (step_z <= 0 || click_x >= 0.5)
            {
                "left"
            } else {
                "right"
            };
        }
        "left"
    } else {
        "right"
    }
}

fn is_lower_door(registry: &Registry, state: StateId) -> bool {
    block_name(registry, state).ends_with("_door")
        && property(registry, state, "half") == Some("lower")
}

fn is_double_plant(name: &str) -> bool {
    matches!(
        name,
        "tall_grass"
            | "large_fern"
            | "sunflower"
            | "lilac"
            | "rose_bush"
            | "peony"
            | "pitcher_plant"
            | "tall_seagrass"
    )
}

fn stairs_shape(context: &PlaceContext<'_>, state: StateId, facing: Direction) -> &'static str {
    let registry = Registry::get();
    let half = property(registry, state, "half").unwrap_or("bottom");
    let behind = context.state_at(offset(context.pos, facing));
    if let Some(behind_facing) = stair_facing(registry, behind)
        && property(registry, behind, "half") == Some(half)
        && behind_facing.axis() != facing.axis()
        && stair_shape_free(context, state, behind_facing.opposite())
    {
        return if behind_facing == facing.counter_clockwise() {
            "outer_left"
        } else {
            "outer_right"
        };
    }
    let front = context.state_at(offset(context.pos, facing.opposite()));
    if let Some(front_facing) = stair_facing(registry, front)
        && property(registry, front, "half") == Some(half)
        && front_facing.axis() != facing.axis()
        && stair_shape_free(context, state, front_facing)
    {
        return if front_facing == facing.counter_clockwise() {
            "inner_left"
        } else {
            "inner_right"
        };
    }
    "straight"
}

fn stair_facing(registry: &Registry, state: StateId) -> Option<Direction> {
    let name = block_name(registry, state);
    if !name.ends_with("_stairs") || property(registry, state, "shape").is_none() {
        return None;
    }
    property(registry, state, "facing").and_then(direction_from_name)
}

fn stair_shape_free(context: &PlaceContext<'_>, state: StateId, neighbour: Direction) -> bool {
    let registry = Registry::get();
    let other = context.state_at(offset(context.pos, neighbour));
    let Some(other_facing) = stair_facing(registry, other) else {
        return true;
    };
    other_facing
        != property(registry, state, "facing")
            .and_then(direction_from_name)
            .unwrap_or(Direction::North)
        || property(registry, other, "half") != property(registry, state, "half")
}

fn chest(context: &PlaceContext<'_>, base: StateId) -> StateId {
    let mut facing = context.horizontal().opposite();
    let mut kind = "single";
    if context.hit.direction.is_horizontal()
        && context.secondary
        && let Some(neighbour_facing) =
            candidate_partner_facing(context, context.pos, context.hit.direction.opposite())
        && neighbour_facing.axis() != context.hit.direction.axis()
    {
        facing = neighbour_facing;
        kind = if facing.counter_clockwise() == context.hit.direction.opposite() {
            "right"
        } else {
            "left"
        };
    }
    if kind == "single" && !context.secondary {
        if candidate_partner_facing(context, context.pos, facing.clockwise()) == Some(facing) {
            kind = "left";
        } else if candidate_partner_facing(context, context.pos, facing.counter_clockwise())
            == Some(facing)
        {
            kind = "right";
        }
    }
    let state = with(base, &[("facing", direction_name(facing)), ("type", kind)]);
    with(
        state,
        &[(
            "waterlogged",
            if context.water_at(context.pos) {
                "true"
            } else {
                "false"
            },
        )],
    )
}

fn candidate_partner_facing(
    context: &PlaceContext<'_>,
    pos: BlockPos,
    direction: Direction,
) -> Option<Direction> {
    let registry = Registry::get();
    let neighbour = context.state_at(offset(pos, direction));
    let block = registry.state(neighbour).block;
    let same = block == registry.state(context.state_at(context.pos)).block
        || context
            .registry()
            .block_for_item(context.item.item)
            .is_some_and(|b| b.id == block);
    if same && property(registry, neighbour, "type") == Some("single") {
        property(registry, neighbour, "facing").and_then(direction_from_name)
    } else {
        None
    }
}

/// `BlockItemStateProperties.apply`: tolerant, later entries win.
fn apply_component(registry: &Registry, state: StateId, item: &ItemStack) -> StateId {
    let Some(bytes) = item.component("minecraft:block_state") else {
        return state;
    };
    let mut buf = bytes;
    let Ok(count) = read_var_int(&mut buf) else {
        return state;
    };
    let mut properties = Vec::new();
    for _ in 0..count.max(0) {
        let Ok(key) = String::decode(&mut buf) else {
            break;
        };
        let Ok(value) = String::decode(&mut buf) else {
            break;
        };
        properties.push((key, value));
    }
    let refs: Vec<(&str, &str)> = properties
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    registry.state_with_properties(state, &refs)
}

fn can_be_replaced(
    world: &World,
    item: &ItemStack,
    _tags: &Tags,
    secondary: bool,
    hit: &BlockHit,
    pos: BlockPos,
    replacing_clicked: bool,
) -> bool {
    let registry = Registry::get();
    let state = world.block_state_or_air(pos.0, pos.1, pos.2);
    let name = block_name(registry, state);
    let same_item = registry
        .block_for_item(item.item)
        .is_some_and(|block| block.id == registry.state(state).block);

    if name == "snow" {
        let layers = int_property(registry, state, "layers").unwrap_or(1);
        return if !same_item || layers >= 8 {
            layers == 1
        } else if replacing_clicked {
            hit.direction == Direction::Up
        } else {
            true
        };
    }
    if name.ends_with("_slab") && registry.property(state, "type").is_some() {
        let kind = registry.property(state, "type").unwrap_or("bottom");
        if kind == "double" || !same_item {
            return false;
        }
        if !replacing_clicked {
            return true;
        }
        let above = hit.location[1] - pos.1 as f64 > 0.5;
        return match kind {
            "bottom" => hit.direction == Direction::Up || (above && hit.direction.is_horizontal()),
            "top" => hit.direction == Direction::Down || (!above && hit.direction.is_horizontal()),
            _ => false,
        };
    }
    if ((name == "candle" || name.ends_with("_candle"))
        && registry.property(state, "candles").is_some())
        || (name == "sea_pickle" && registry.property(state, "pickles").is_some())
        || (name == "turtle_egg" && registry.property(state, "eggs").is_some())
    {
        let key = if registry.property(state, "candles").is_some() {
            "candles"
        } else if registry.property(state, "pickles").is_some() {
            "pickles"
        } else {
            "eggs"
        };
        return !secondary && same_item && int_property(registry, state, key).unwrap_or(4) < 4;
    }
    if registry.property(state, "segment_amount").is_some()
        || registry.property(state, "flower_amount").is_some()
    {
        let key = if registry.property(state, "segment_amount").is_some() {
            "segment_amount"
        } else {
            "flower_amount"
        };
        if !secondary && same_item && int_property(registry, state, key).unwrap_or(4) < 4 {
            return true;
        }
    }
    registry.state(state).replaceable && !same_item
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapidbot_buf::{Encode, VarInt, write_var_int};
    use rapidbot_world::chunk::Chunk;
    use rapidbot_world::item::ItemData;
    use rapidbot_world::{AIR, DimensionHeight};

    fn empty_world() -> World {
        let mut world = World::new(DimensionHeight::OVERWORLD);
        for x in -1..=1 {
            for z in -1..=1 {
                world.insert_chunk(x, z, Chunk::empty(24));
            }
        }
        world
    }

    fn default_state(name: &str) -> StateId {
        Registry::get()
            .block_by_name(name)
            .unwrap_or_else(|| panic!("{name} is not a block"))
            .default_state
    }

    fn state(name: &str, properties: &[(&str, &str)]) -> StateId {
        Registry::get().state_with_properties(default_state(name), properties)
    }

    fn item(name: &str) -> ItemStack {
        ItemStack::new(ItemData::get().item_id(name).unwrap(), 1)
    }

    fn item_with_block_state(name: &str, properties: &[(&str, &str)]) -> ItemStack {
        let data = ItemData::get();
        let item = data.item_id(name).unwrap();
        let component = data.component_id("minecraft:block_state").unwrap();
        let mut payload = Vec::new();
        write_var_int(&mut payload, properties.len() as i32);
        for (key, value) in properties {
            key.to_string().encode(&mut payload);
            value.to_string().encode(&mut payload);
        }
        let mut bytes = Vec::new();
        VarInt(1).encode(&mut bytes);
        VarInt(item as i32).encode(&mut bytes);
        VarInt(1).encode(&mut bytes);
        VarInt(0).encode(&mut bytes);
        VarInt(component as i32).encode(&mut bytes);
        bytes.extend(payload);
        ItemStack::decode(&mut bytes.as_slice()).unwrap().unwrap()
    }

    fn hit(pos: BlockPos, direction: Direction, location: [f64; 3]) -> BlockHit {
        BlockHit {
            pos,
            location,
            direction,
            inside: false,
            miss: false,
        }
    }

    fn player() -> Player {
        let mut player = Player::new();
        player.y_rot = 0.0;
        player
    }

    fn predict_changes(
        world: &World,
        item: &ItemStack,
        hit: &BlockHit,
        tags: &Tags,
        secondary: bool,
    ) -> Option<Vec<BlockChange>> {
        predict(world, &player(), item, hit, tags, secondary)
    }

    fn predicted_state(
        world: &World,
        item: &ItemStack,
        hit: &BlockHit,
        tags: &Tags,
    ) -> Option<StateId> {
        let changes = predict_changes(world, item, hit, tags, false)?;
        assert_eq!(changes.len(), 1);
        Some(changes[0].state)
    }

    #[test]
    fn predicts_stairs_facing_half_and_shape() {
        let mut world = empty_world();
        world.set_block_state(0, 63, 0, default_state("minecraft:stone"));
        let hit = hit((0, 63, 0), Direction::Up, [0.5, 64.0, 0.5]);
        assert_eq!(
            predicted_state(
                &world,
                &item("minecraft:oak_stairs"),
                &hit,
                &Tags::default()
            ),
            Some(state(
                "minecraft:oak_stairs",
                &[
                    ("facing", "south"),
                    ("half", "bottom"),
                    ("shape", "straight"),
                    ("waterlogged", "false"),
                ],
            ))
        );
    }

    #[test]
    fn predicts_slab_bottom_and_double() {
        let mut world = empty_world();
        world.set_block_state(0, 63, 0, default_state("minecraft:stone"));
        let stairs_hit = hit((0, 63, 0), Direction::Up, [0.5, 64.0, 0.5]);
        assert_eq!(
            predicted_state(
                &world,
                &item("minecraft:oak_slab"),
                &stairs_hit,
                &Tags::default()
            ),
            Some(state(
                "minecraft:oak_slab",
                &[("type", "bottom"), ("waterlogged", "false")],
            ))
        );

        world.set_block_state(
            0,
            64,
            0,
            state(
                "minecraft:oak_slab",
                &[("type", "bottom"), ("waterlogged", "false")],
            ),
        );
        let slab_hit = hit((0, 64, 0), Direction::Up, [0.5, 65.0, 0.5]);
        assert_eq!(
            predicted_state(
                &world,
                &item("minecraft:oak_slab"),
                &slab_hit,
                &Tags::default()
            ),
            Some(state(
                "minecraft:oak_slab",
                &[("type", "double"), ("waterlogged", "false")],
            ))
        );
    }

    #[test]
    fn predicts_both_door_halves() {
        let mut world = empty_world();
        world.set_block_state(0, 63, 0, default_state("minecraft:stone"));
        let hit = hit((0, 63, 0), Direction::Up, [0.5, 64.0, 0.5]);
        let changes = predict_changes(
            &world,
            &item("minecraft:oak_door"),
            &hit,
            &Tags::default(),
            false,
        )
        .unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].pos, (0, 64, 0));
        assert_eq!(
            changes[0].state,
            state(
                "minecraft:oak_door",
                &[
                    ("facing", "south"),
                    ("half", "lower"),
                    ("hinge", "left"),
                    ("open", "false"),
                    ("powered", "false"),
                ],
            )
        );
        assert_eq!(changes[1].pos, (0, 65, 0));
        assert_eq!(
            changes[1].state,
            state(
                "minecraft:oak_door",
                &[
                    ("facing", "south"),
                    ("half", "upper"),
                    ("hinge", "left"),
                    ("open", "false"),
                    ("powered", "false"),
                ],
            )
        );
    }

    #[test]
    fn predicts_both_bed_halves() {
        let mut world = empty_world();
        world.set_block_state(0, 63, 0, default_state("minecraft:stone"));
        let hit = hit((0, 63, 0), Direction::Up, [0.5, 64.0, 0.5]);
        let changes = predict_changes(
            &world,
            &item("minecraft:red_bed"),
            &hit,
            &Tags::default(),
            false,
        )
        .unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].pos, (0, 64, 0));
        assert_eq!(
            changes[0].state,
            state(
                "minecraft:red_bed",
                &[("facing", "south"), ("occupied", "false"), ("part", "foot")],
            )
        );
        assert_eq!(changes[1].pos, (0, 64, 1));
        assert_eq!(
            changes[1].state,
            state(
                "minecraft:red_bed",
                &[("facing", "south"), ("occupied", "false"), ("part", "head")],
            )
        );
    }

    #[test]
    fn predicts_both_double_plant_halves() {
        let mut world = empty_world();
        let dirt = default_state("minecraft:dirt");
        world.set_block_state(0, 63, 0, dirt);
        let mut tags = Tags::default();
        tags.insert(
            "minecraft:block",
            "minecraft:supports_vegetation",
            Registry::get().state(dirt).block,
        );
        let hit = hit((0, 63, 0), Direction::Up, [0.5, 64.0, 0.5]);
        let changes =
            predict_changes(&world, &item("minecraft:tall_grass"), &hit, &tags, false).unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(
            changes[0].state,
            state("minecraft:tall_grass", &[("half", "lower")])
        );
        assert_eq!(
            changes[1].state,
            state("minecraft:tall_grass", &[("half", "upper")])
        );
    }

    #[test]
    fn predicts_wall_torch_and_ladder() {
        let mut world = empty_world();
        world.set_block_state(0, 64, 0, default_state("minecraft:stone"));
        let hit = hit((0, 64, 0), Direction::East, [1.0, 64.5, 0.5]);
        assert_eq!(
            predicted_state(&world, &item("minecraft:torch"), &hit, &Tags::default()),
            Some(state(
                "minecraft:wall_torch",
                &[("facing", "east"), ("waterlogged", "false")],
            ))
        );
        assert_eq!(
            predicted_state(&world, &item("minecraft:ladder"), &hit, &Tags::default()),
            Some(state(
                "minecraft:ladder",
                &[("facing", "east"), ("waterlogged", "false")],
            ))
        );
    }

    #[test]
    fn ladder_rejects_another_ladder_on_the_same_face() {
        let mut world = empty_world();
        world.set_block_state(
            0,
            64,
            0,
            state(
                "minecraft:ladder",
                &[("facing", "east"), ("waterlogged", "false")],
            ),
        );
        let hit = hit((0, 64, 0), Direction::East, [1.0, 64.5, 0.5]);
        assert_eq!(
            predict_changes(
                &world,
                &item("minecraft:ladder"),
                &hit,
                &Tags::default(),
                false
            ),
            None
        );
    }

    #[test]
    fn predicts_wall_hanging_sign_with_side_support_only() {
        let mut world = empty_world();
        world.set_block_state(0, 63, 0, default_state("minecraft:stone"));
        // The sign faces north. Its west/east side support is what
        // `WallHangingSignBlock.canPlace` requires; the block behind it stays.
        world.set_block_state(1, 64, 0, default_state("minecraft:stone"));
        let hit = hit((0, 63, 0), Direction::Up, [0.5, 64.0, 0.5]);
        assert_eq!(
            predicted_state(
                &world,
                &item("minecraft:oak_hanging_sign"),
                &hit,
                &Tags::default()
            ),
            Some(state(
                "minecraft:oak_wall_hanging_sign",
                &[("facing", "north"), ("waterlogged", "false")],
            ))
        );
    }

    #[test]
    fn predicts_chest_facing_and_single_type() {
        let mut world = empty_world();
        world.set_block_state(0, 63, 0, default_state("minecraft:stone"));
        let hit = hit((0, 63, 0), Direction::Up, [0.5, 64.0, 0.5]);
        assert_eq!(
            predicted_state(&world, &item("minecraft:chest"), &hit, &Tags::default()),
            Some(state(
                "minecraft:chest",
                &[
                    ("facing", "north"),
                    ("type", "single"),
                    ("waterlogged", "false"),
                ],
            ))
        );
    }

    #[test]
    fn predicts_snow_layers_and_support() {
        let mut world = empty_world();
        world.set_block_state(0, 63, 0, default_state("minecraft:stone"));
        let new_hit = hit((0, 63, 0), Direction::Up, [0.5, 64.0, 0.5]);
        assert_eq!(
            predicted_state(&world, &item("minecraft:snow"), &new_hit, &Tags::default()),
            Some(state("minecraft:snow", &[("layers", "1")]))
        );

        world.set_block_state(0, 64, 0, state("minecraft:snow", &[("layers", "1")]));
        let stack_hit = hit((0, 64, 0), Direction::Up, [0.5, 65.0, 0.5]);
        assert_eq!(
            predicted_state(
                &world,
                &item("minecraft:snow"),
                &stack_hit,
                &Tags::default()
            ),
            Some(state("minecraft:snow", &[("layers", "2")]))
        );

        let mut unsupported = empty_world();
        unsupported.set_block_state(
            0,
            63,
            0,
            state(
                "minecraft:oak_slab",
                &[("type", "bottom"), ("waterlogged", "false")],
            ),
        );
        assert_eq!(
            predicted_state(
                &unsupported,
                &item("minecraft:snow"),
                &new_hit,
                &Tags::default()
            ),
            None
        );
    }

    #[test]
    fn predicts_candles_only_with_center_support() {
        let mut world = empty_world();
        world.set_block_state(0, 63, 0, default_state("minecraft:stone"));
        let hit = hit((0, 63, 0), Direction::Up, [0.5, 64.0, 0.5]);
        assert_eq!(
            predicted_state(&world, &item("minecraft:candle"), &hit, &Tags::default()),
            Some(state(
                "minecraft:candle",
                &[("candles", "1"), ("waterlogged", "false")],
            ))
        );

        let mut unsupported = empty_world();
        unsupported.set_block_state(0, 63, 0, AIR);
        assert_eq!(
            predicted_state(
                &unsupported,
                &item("minecraft:candle"),
                &hit,
                &Tags::default()
            ),
            None
        );
    }

    #[test]
    fn predicts_lantern_hanging_from_center_support() {
        let mut world = empty_world();
        world.set_block_state(0, 65, 0, default_state("minecraft:stone"));
        let hit = hit((0, 65, 0), Direction::Down, [0.5, 65.0, 0.5]);
        assert_eq!(
            predicted_state(&world, &item("minecraft:lantern"), &hit, &Tags::default()),
            Some(state(
                "minecraft:lantern",
                &[("hanging", "true"), ("waterlogged", "false")],
            ))
        );
    }

    #[test]
    fn predicts_candle_stack() {
        let mut world = empty_world();
        world.set_block_state(0, 63, 0, default_state("minecraft:stone"));
        world.set_block_state(
            0,
            64,
            0,
            state(
                "minecraft:candle",
                &[("candles", "1"), ("waterlogged", "false")],
            ),
        );
        let hit = hit((0, 64, 0), Direction::Up, [0.5, 65.0, 0.5]);
        assert_eq!(
            predicted_state(&world, &item("minecraft:candle"), &hit, &Tags::default()),
            Some(state(
                "minecraft:candle",
                &[("candles", "2"), ("waterlogged", "false")],
            ))
        );
    }

    #[test]
    fn predicts_crafter_orientation() {
        let mut world = empty_world();
        world.set_block_state(0, 63, 0, default_state("minecraft:stone"));
        let hit = hit((0, 63, 0), Direction::Up, [0.5, 64.0, 0.5]);
        assert_eq!(
            predicted_state(&world, &item("minecraft:crafter"), &hit, &Tags::default()),
            Some(state(
                "minecraft:crafter",
                &[("orientation", "up_south"), ("triggered", "false")],
            ))
        );
    }

    #[test]
    fn applies_block_state_component_after_placement_state() {
        let mut world = empty_world();
        world.set_block_state(0, 63, 0, default_state("minecraft:stone"));
        let hit = hit((0, 63, 0), Direction::Up, [0.5, 64.0, 0.5]);
        let item = item_with_block_state("minecraft:oak_stairs", &[("half", "top")]);
        assert_eq!(
            predicted_state(&world, &item, &hit, &Tags::default()),
            Some(state(
                "minecraft:oak_stairs",
                &[
                    ("facing", "south"),
                    ("half", "top"),
                    ("shape", "straight"),
                    ("waterlogged", "false"),
                ],
            ))
        );
    }
}
