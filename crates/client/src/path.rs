//! Finding a way across the block grid: A* over the places a player can
//! stand, with the moves a person on foot makes (walk, step up, hop up a
//! block, sprint-jump short gaps, drop a few blocks). Not vanilla: this is
//! bot logic, and it only decides *where* to go. [`crate::nav::Walker`]
//! turns the result into mouse and key input.
//!
//! Not handled yet: breaking or placing blocks. Doors are opened on the way
//! through, but not broken.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::OnceLock;

use rapidbot_physics::math::Vec3;
use rapidbot_world::collision::block_collisions;
use rapidbot_world::fluid::{self, FluidKind};
use rapidbot_world::{Aabb, Axis, Registry, World};

use crate::door::{self, Door};

/// Player box: 0.6 wide, 1.8 tall.
const HALF_WIDTH: f64 = 0.3;
const HEIGHT: f64 = 1.8;
/// `maxUpStep` is 0.6; a jump clears 1.25, so one block and a bit.
const STEP: f64 = 0.6;
const JUMP: f64 = 1.2;
/// Falls of more than three blocks hurt.
const MAX_DROP: f64 = 3.2;
/// Landing up to three blocks from the takeoff: two empty blocks. Three
/// empty blocks are possible in vanilla with perfect timing, but a bot
/// should not need frame-perfect movement to reach a goal.
const MAX_GAP: i32 = 3;
/// Landing a block higher is fine; higher than a normal jump is not.
const GAP_UP: f64 = 1.0;
/// The player's head is this high at the top of a jump. A ceiling below it
/// turns a would-be gap jump into a bumped head.
const JUMP_HEADROOM: f64 = HEIGHT + 1.3;

const DANGER: u8 = 1;
const WATER: u8 = 2;
const CACTUS: u8 = 4;
const CLIMB: u8 = 8;

/// Per block state: whether touching it hurts or traps, whether it holds
/// water, and whether it can be climbed.
fn traits() -> &'static [u8] {
    static TRAITS: OnceLock<Vec<u8>> = OnceLock::new();
    TRAITS.get_or_init(|| {
        const CLIMBABLE: [&str; 9] = [
            "minecraft:ladder",
            "minecraft:vine",
            "minecraft:scaffolding",
            "minecraft:twisting_vines",
            "minecraft:twisting_vines_plant",
            "minecraft:weeping_vines",
            "minecraft:weeping_vines_plant",
            "minecraft:cave_vines",
            "minecraft:cave_vines_plant",
        ];
        const HARMFUL: [&str; 14] = [
            "minecraft:fire",
            "minecraft:soul_fire",
            "minecraft:magma_block",
            "minecraft:campfire",
            "minecraft:soul_campfire",
            "minecraft:cactus",
            "minecraft:sweet_berry_bush",
            "minecraft:wither_rose",
            "minecraft:powder_snow",
            "minecraft:cobweb",
            "minecraft:pointed_dripstone",
            "minecraft:nether_portal",
            "minecraft:end_portal",
            "minecraft:end_gateway",
        ];
        let registry = Registry::get();
        registry
            .states
            .iter()
            .map(|state| {
                let name = registry.blocks[state.block as usize].name.as_str();
                let mut t = 0;
                if HARMFUL.contains(&name) || state.fluid.ends_with("lava") {
                    t |= DANGER;
                }
                if state.fluid.ends_with("water") {
                    t |= WATER;
                }
                if name == "minecraft:cactus" {
                    t |= CACTUS;
                }
                if CLIMBABLE.contains(&name) {
                    t |= CLIMB;
                }
                t
            })
            .collect()
    })
}

fn trait_at(world: &World, x: i32, y: i32, z: i32) -> u8 {
    world
        .block_state(x, y, z)
        .map_or(0, |s| traits()[s as usize])
}

pub(crate) fn climbable_at(world: &World, x: i32, y: i32, z: i32) -> bool {
    trait_at(world, x, y, z) & CLIMB != 0
}

/// Whether a player-sized column centred in block column (x, z) is free of
/// collision between `y0` and `y1`. False where the chunk is not loaded.
fn column_free(world: &World, x: i32, z: i32, y0: f64, y1: f64) -> bool {
    let registry = Registry::get();
    let bbox = Aabb::new(
        x as f64 + 0.5 - HALF_WIDTH,
        y0,
        z as f64 + 0.5 - HALF_WIDTH,
        x as f64 + 0.5 + HALF_WIDTH,
        y1,
        z as f64 + 0.5 + HALF_WIDTH,
    );
    // One cell lower too: fences and walls are 1.5 tall.
    for y in (y0.floor() as i32 - 1)..=((y1 - 1.0e-7).floor() as i32) {
        let Some(state) = world.block_state(x, y, z) else {
            return false;
        };
        let Some(shape) = registry.collision_shape(state) else {
            continue;
        };
        if shape.is_block {
            if (y + 1) as f64 > y0 + 1.0e-7 && (y as f64) < y1 - 1.0e-7 {
                return false;
            }
        } else if shape.at(x, y, z).intersects_box(&bbox) {
            return false;
        }
    }
    true
}

/// The height a player stands at with their feet in block cell (x, y, z):
/// on a low shape in the cell itself (slab, carpet) or on the block below.
/// In deep water, the top water cell is a place to float at the surface.
/// A climbable block is a place to hold on at its own height.
/// `None` if there is nothing to stand on, no room, or it is harmful.
pub fn surface(world: &World, x: i32, y: i32, z: i32) -> Option<f64> {
    let registry = Registry::get();
    let here = world.block_state(x, y, z)?;
    let here_shape = registry.collision_shape(here);
    // An open door's own shape is a thin panel at the side, so it is not a
    // surface to stand on. The floor below the door is.
    let open_door = if let Some(door) = door::at(world, x, y, z)
        && door.open
        && door.is_lower((x, y, z))
    {
        let below = world.block_state(x, y - 1, z)?;
        Some((door.standing_height(world)?, below))
    } else {
        None
    };
    // Only the top water cell, and only when the water is deep enough to
    // swim: one-deep water is handled as wading on the block below.
    let water_surface = if here_shape.is_none()
        && let Some(fluid) = fluid::fluid_at(world, x, y, z)
        && fluid.kind == FluidKind::Water
        && fluid::fluid_at(world, x, y - 1, z).is_some_and(|f| f.kind == FluidKind::Water)
        && !fluid::fluid_at(world, x, y + 1, z).is_some_and(|f| f.kind == FluidKind::Water)
    {
        Some(y as f64 + fluid::height(world, fluid, x, y, z) as f64)
    } else {
        None
    };
    let (s, floor) = if let Some((s, floor)) = open_door {
        (s, floor)
    } else if let Some(s) = water_surface {
        (s, here)
    } else if climbable_at(world, x, y, z) {
        (y as f64, here)
    } else {
        match here_shape {
            Some(shape) => {
                let top = shape.at(0, 0, 0).max(Axis::Y);
                if top >= 1.0 {
                    return None;
                }
                (y as f64 + top, here)
            }
            None => {
                let below = world.block_state(x, y - 1, z)?;
                let top = registry.collision_shape(below)?.at(0, 0, 0).max(Axis::Y);
                if top < 1.0 {
                    return None;
                }
                (y as f64 + (top - 1.0), below)
            }
        }
    };
    let t = traits();
    let head = (s + 1.62).floor() as i32;
    if (t[floor as usize] | t[here as usize] | trait_at(world, x, head, z)) & DANGER != 0 {
        return None;
    }
    // Head under water: that is swimming, which is not planned for.
    if trait_at(world, x, head, z) & WATER != 0 {
        return None;
    }
    // Brushing a cactus hurts.
    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        if trait_at(world, x + dx, y, z + dz) & CACTUS != 0 {
            return None;
        }
    }
    column_free(world, x, z, s, s + HEIGHT).then_some(s)
}

/// The standing place in column (x, z) nearest in height to `y`, looking
/// up to 16 blocks either way: its cell height and surface. Keeps a goal
/// "over there" on the ground in sight rather than in a cave beneath it.
pub fn nearest_surface(world: &World, x: i32, z: i32, y: i32) -> Option<(i32, f64)> {
    (0..=16)
        .flat_map(|d| [y + d, y - d])
        .find_map(|cy| surface(world, x, cy, z).map(|s| (cy, s)))
}

/// Where a search is headed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Goal {
    /// Any standing place in this block column.
    Column { x: i32, z: i32 },
    /// Standing in (or within a block above or below) this block.
    Block { x: i32, y: i32, z: i32 },
}

impl Goal {
    fn xz(&self) -> (i32, i32) {
        match *self {
            Goal::Column { x, z } | Goal::Block { x, z, .. } => (x, z),
        }
    }

    /// Octile distance over the ground: never more than the real cost.
    fn estimate(&self, cell: (i32, i32, i32)) -> f64 {
        let (x, z) = self.xz();
        let (dx, dz) = ((cell.0 - x).abs() as f64, (cell.2 - z).abs() as f64);
        dx.max(dz) + (std::f64::consts::SQRT_2 - 1.0) * dx.min(dz)
    }

    fn reached(&self, cell: (i32, i32, i32), surface: f64) -> bool {
        match *self {
            Goal::Column { x, z } => cell.0 == x && cell.2 == z,
            Goal::Block { x, y, z } => {
                cell.0 == x && cell.2 == z && (surface - y as f64).abs() < 1.0
            }
        }
    }
}

/// A place to stand on the way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Waypoint {
    pub cell: (i32, i32, i32),
    /// Feet height when standing there.
    pub surface: f64,
    /// Reached by a sprint jump over a gap, from the previous waypoint.
    pub jump: bool,
    /// The door to open here, if this is the lower half of one.
    pub door: Option<Door>,
}

impl Waypoint {
    /// Centre of the cell at standing height.
    pub fn pos(&self) -> Vec3 {
        Vec3::new(
            self.cell.0 as f64 + 0.5,
            self.surface,
            self.cell.2 as f64 + 0.5,
        )
    }
}

#[derive(Debug, Clone)]
pub struct Path {
    /// From the start (inclusive) to the end.
    pub points: Vec<Waypoint>,
    /// False if the goal was not reached: the path then ends at the
    /// reachable place nearest to it.
    pub complete: bool,
}

struct Node {
    cell: (i32, i32, i32),
    surface: f64,
    cost: f64,
    parent: u32,
    closed: bool,
}

/// An A* search that can be run a bit at a time, so a long search does not
/// hold up the tick it starts in (a late tick is visible to the server).
pub struct Search {
    goal: Goal,
    nodes: Vec<Node>,
    index: HashMap<(i32, i32, i32), u32>,
    open: BinaryHeap<Reverse<(u64, u32)>>,
    surfaces: HashMap<(i32, i32, i32), Option<f64>>,
    best: u32,
    expanded: usize,
    limit: usize,
}

const NO_PARENT: u32 = u32::MAX;
const CARDINALS: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
const DIAGONALS: [(i32, i32); 4] = [(1, 1), (1, -1), (-1, 1), (-1, -1)];

impl Search {
    /// Starts from where the player stands (or the ground just below).
    /// `None` if that is nowhere a path can start: in the air, deep under
    /// water, in an unloaded chunk. `limit` caps the places examined.
    pub fn new(world: &World, from: Vec3, goal: Goal, limit: usize) -> Option<Self> {
        let (x, z) = (from.x.floor() as i32, from.z.floor() as i32);
        let top = (from.y + 1.0e-3).floor() as i32;
        let (cell, surface) = (0..4).find_map(|down| {
            let cell = (x, top - down, z);
            surface(world, cell.0, cell.1, cell.2).map(|s| (cell, s))
        })?;
        let mut search = Self {
            goal,
            nodes: vec![Node {
                cell,
                surface,
                cost: 0.0,
                parent: NO_PARENT,
                closed: false,
            }],
            index: HashMap::from([(cell, 0)]),
            open: BinaryHeap::new(),
            surfaces: HashMap::new(),
            best: 0,
            expanded: 0,
            limit,
        };
        search.open.push(Reverse((0, 0)));
        Some(search)
    }

    fn surface(&mut self, world: &World, cell: (i32, i32, i32)) -> Option<f64> {
        *self
            .surfaces
            .entry(cell)
            .or_insert_with(|| surface(world, cell.0, cell.1, cell.2))
    }

    /// Examines up to `budget` more places. Returns the path once the
    /// search is over, `None` while it is still going.
    pub fn step(&mut self, world: &World, budget: usize) -> Option<Path> {
        for _ in 0..budget {
            let Some(Reverse((_, id))) = self.open.pop() else {
                return Some(self.path(world));
            };
            let node = &mut self.nodes[id as usize];
            if node.closed {
                continue;
            }
            node.closed = true;
            let (cell, surface, cost) = (node.cell, node.surface, node.cost);
            if self.goal.reached(cell, surface) {
                self.best = id;
                return Some(self.path(world));
            }
            let best = &self.nodes[self.best as usize];
            let (h, best_h) = (self.goal.estimate(cell), self.goal.estimate(best.cell));
            if h < best_h || (h == best_h && cost < best.cost) {
                self.best = id;
            }
            self.expanded += 1;
            if self.expanded >= self.limit {
                return Some(self.path(world));
            }
            self.expand(world, id, cell, surface, cost);
        }
        None
    }

    fn expand(&mut self, world: &World, id: u32, cell: (i32, i32, i32), s: f64, cost: f64) {
        let (x, y, z) = cell;
        for (dx, dz) in CARDINALS {
            // The highest place in the next column that can be got to.
            let mut stepped = false;
            for ny in (y - 4..=y + 1).rev() {
                let Some(s2) = self.surface(world, (x + dx, ny, z + dz)) else {
                    continue;
                };
                let d = s2 - s;
                if d > JUMP {
                    continue;
                }
                let step_cost = if d > STEP {
                    // Hop up: needs headroom over where we stand.
                    if !column_free(world, x, z, s2, s2 + HEIGHT) {
                        break;
                    }
                    2.0
                } else if d >= -1.0e-6 {
                    1.0 + d
                } else {
                    // Step or drop down: the way out over the edge and the
                    // fall below it must be clear.
                    if d < -MAX_DROP || !column_free(world, x + dx, z + dz, s2, s + HEIGHT) {
                        break;
                    }
                    1.0 - d * 0.4
                };
                self.relax(world, id, (x + dx, ny, z + dz), s2, cost + step_cost);
                stepped = true;
                break;
            }
            if !stepped {
                // A closed door has no ordinary surface. Treat it as a
                // place to stand once it has been opened.
                for ny in [y + 1, y, y - 1] {
                    if self.door_step(world, id, (x + dx, ny, z + dz), s, cost) {
                        break;
                    }
                }
            }
        }
        for (dx, dz) in DIAGONALS {
            // Only on the level or a step, and never cutting a corner.
            for ny in [y, y + 1, y - 1] {
                let Some(s2) = self.surface(world, (x + dx, ny, z + dz)) else {
                    continue;
                };
                let d = s2 - s;
                if d.abs() > STEP {
                    continue;
                }
                let high = s.max(s2);
                let side_ok = |cx: i32, cz: i32| {
                    column_free(world, cx, cz, high, high + HEIGHT)
                        && door::at(world, cx, high.floor() as i32, cz).is_none()
                        && (trait_at(world, cx, high.floor() as i32, cz)
                            | trait_at(world, cx, high.floor() as i32 - 1, cz))
                            & DANGER
                            == 0
                };
                if side_ok(x + dx, z) && side_ok(x, z + dz) {
                    let step_cost = std::f64::consts::SQRT_2 + d.abs();
                    self.relax(world, id, (x + dx, ny, z + dz), s2, cost + step_cost);
                }
                break;
            }
        }
        self.gap_jumps(world, id, cell, s, cost);
        self.climb(world, id, cell, cost);
    }

    /// A closed door is a passable place after a right click. Add it as a
    /// neighbour only when it is the lower half and can be opened by hand.
    fn door_step(
        &mut self,
        world: &World,
        parent: u32,
        target: (i32, i32, i32),
        from_s: f64,
        cost: f64,
    ) -> bool {
        let Some(door) = door::at(world, target.0, target.1, target.2) else {
            return false;
        };
        if !door.is_lower(target) || !door.can_open() {
            return false;
        }
        let Some(s2) = door.standing_height(world) else {
            return false;
        };
        let d = s2 - from_s;
        if d > JUMP || d < -MAX_DROP {
            return false;
        }
        if d > STEP {
            let source = self.nodes[parent as usize].cell;
            if !column_free(world, source.0, source.2, s2, s2 + HEIGHT) {
                return false;
            }
        } else if d < -1.0e-6 && !column_free(world, target.0, target.2, s2, from_s + HEIGHT) {
            return false;
        }
        self.relax(world, parent, target, s2, cost + 1.5);
        true
    }

    /// Sprint jumps across a short gap. The takeoff and landing are block
    /// centres up to [`MAX_GAP`] apart; everything between has to be clear at
    /// body and jump height. The walker builds up sprint speed and jumps at
    /// the edge.
    fn gap_jumps(&mut self, world: &World, id: u32, cell: (i32, i32, i32), s: f64, cost: f64) {
        let (x, y, z) = cell;
        // Jumping out of deep water or off a ladder is a different move.
        if trait_at(world, x, y, z) & (WATER | CLIMB) != 0 {
            return;
        }
        // There has to be room to rise off the takeoff block too.
        if !column_free(world, x, z, s, s + JUMP_HEADROOM) {
            return;
        }
        for (dx, dz) in CARDINALS {
            for distance in 2..=MAX_GAP {
                let (cx, cz) = (x + dx * (distance - 1), z + dz * (distance - 1));
                // A walkable cell is not a gap; walking there is cheaper.
                if self
                    .surface(world, (cx, y, cz))
                    .is_some_and(|s1| (s1 - s).abs() < 0.01)
                {
                    break;
                }
                if !column_free(world, cx, cz, s, s + JUMP_HEADROOM) {
                    break;
                }
                // A closed door blocks the flight path even though its thin
                // panel leaves the column's centre clear.
                if door::at(world, cx, y, cz).is_some_and(|d| !d.open) {
                    break;
                }
                let (lx, lz) = (x + dx * distance, z + dz * distance);
                // The highest surface in the landing column that can be
                // reached. A surface above the takeoff that is too high is
                // skipped in favour of a lower one; below the drop limit
                // there is nothing worth looking at.
                for ny in (y - 4..=y + 1).rev() {
                    let Some(s2) = self.surface(world, (lx, ny, lz)) else {
                        continue;
                    };
                    let d = s2 - s;
                    if d > GAP_UP {
                        continue;
                    }
                    if d < -MAX_DROP {
                        break;
                    }
                    // Jump onto dry ground. Dropping into deep water is a
                    // different move, with its own way out.
                    if trait_at(world, lx, ny, lz) & WATER != 0 {
                        continue;
                    }
                    let step_cost = 1.0 + distance as f64 + d.abs() * 0.2;
                    self.relax(world, id, (lx, ny, lz), s2, cost + step_cost);
                    break;
                }
            }
        }
    }

    /// Ladder and vine columns are climbed one block at a time. A player
    /// can also jump into a climbable cell directly above the ground.
    fn climb(&mut self, world: &World, id: u32, cell: (i32, i32, i32), cost: f64) {
        let (x, y, z) = cell;
        if !climbable_at(world, x, y, z) {
            // Jump into a ladder that starts just above the ground, or
            // step down onto one that starts just below the feet.
            for (ny, step_cost) in [(y + 1, 2.0), (y - 1, 1.0)] {
                if !climbable_at(world, x, ny, z) {
                    continue;
                }
                let s2 = ny as f64;
                if column_free(world, x, z, s2, s2 + HEIGHT) {
                    self.relax(world, id, (x, ny, z), s2, cost + step_cost);
                }
            }
            return;
        }
        for ny in [y + 1, y - 1] {
            if !climbable_at(world, x, ny, z) {
                continue;
            }
            let Some(s2) = self.surface(world, (x, ny, z)) else {
                continue;
            };
            if (s2 - ny as f64).abs() > 1.0e-6 || !column_free(world, x, z, s2, s2 + HEIGHT) {
                continue;
            }
            let step_cost = if ny > y { 1.0 } else { 1.2 };
            self.relax(world, id, (x, ny, z), s2, cost + step_cost);
        }
    }

    fn relax(
        &mut self,
        world: &World,
        parent: u32,
        cell: (i32, i32, i32),
        surface: f64,
        mut cost: f64,
    ) {
        // Wading is slow.
        if trait_at(world, cell.0, cell.1, cell.2) & WATER != 0 {
            cost += 1.5;
        }
        let id = match self.index.get(&cell) {
            Some(&id) => {
                let node = &mut self.nodes[id as usize];
                if node.closed || node.cost <= cost {
                    return;
                }
                node.cost = cost;
                node.parent = parent;
                id
            }
            None => {
                let id = self.nodes.len() as u32;
                self.nodes.push(Node {
                    cell,
                    surface,
                    cost,
                    parent,
                    closed: false,
                });
                self.index.insert(cell, id);
                id
            }
        };
        // Slightly greedy: much faster, paths barely longer.
        let f = cost + 1.2 * self.goal.estimate(cell);
        self.open.push(Reverse(((f * 1024.0) as u64, id)));
    }

    fn path(&self, world: &World) -> Path {
        let mut points = Vec::new();
        let mut id = self.best;
        while id != NO_PARENT {
            let node = &self.nodes[id as usize];
            let door = door::at(world, node.cell.0, node.cell.1, node.cell.2)
                .filter(|door| door.is_lower(node.cell));
            points.push(Waypoint {
                cell: node.cell,
                surface: node.surface,
                jump: false,
                door,
            });
            id = node.parent;
        }
        points.reverse();
        for i in 1..points.len() {
            let (a, b) = (points[i - 1].cell, points[i].cell);
            points[i].jump = (a.0 - b.0).abs().max((a.2 - b.2).abs()) > 1;
        }
        let end = &self.nodes[self.best as usize];
        Path {
            points,
            complete: self.goal.reached(end.cell, end.surface),
        }
    }
}

/// Runs a search to the end in one go.
pub fn find(world: &World, from: Vec3, goal: Goal, limit: usize) -> Option<Path> {
    let mut search = Search::new(world, from, goal, limit)?;
    loop {
        if let Some(path) = search.step(world, 4096) {
            return Some(path);
        }
    }
}

/// Whether a player can walk in a straight line from `from` to `to`
/// staying on the level of `to`: nothing in the way, ground under the
/// whole way, and no drop or hazard beside it.
pub fn line_walkable(world: &World, from: Vec3, to: Vec3) -> bool {
    let registry = Registry::get();
    let s = to.y;
    let cell_y = (s + 1.0e-4).floor() as i32;
    let level = |x: f64, z: f64| {
        surface(world, x.floor() as i32, cell_y, z.floor() as i32)
            .is_some_and(|v| (v - s).abs() < 0.01)
    };
    let (dx, dz) = (to.x - from.x, to.z - from.z);
    let steps = ((dx * dx + dz * dz).sqrt() / 0.25).ceil().max(1.0) as usize;
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        let (x, z) = (from.x + dx * t, from.z + dz * t);
        let (cx, cz) = (x.floor() as i32, z.floor() as i32);
        // Do not cut through a closed door: the thin panel leaves the column
        // centre clear, so collision alone would let a line do it.
        if [cell_y, cell_y + 1]
            .iter()
            .any(|&y| door::at(world, cx, y, cz).is_some_and(|door| !door.open))
        {
            return false;
        }
        if !level(x, z) {
            return false;
        }
        let bbox = Aabb::new(
            x - HALF_WIDTH,
            s,
            z - HALF_WIDTH,
            x + HALF_WIDTH,
            s + HEIGHT,
            z + HALF_WIDTH,
        );
        if !block_collisions(world, registry, &bbox.deflate(1.0e-7, 1.0e-7, 1.0e-7)).is_empty() {
            return false;
        }
        // Under each corner: level ground or a wall, not an edge.
        for (cx, cz) in [(-1.0, -1.0), (-1.0, 1.0), (1.0, -1.0), (1.0, 1.0)] {
            let (px, pz) = (x + cx * HALF_WIDTH, z + cz * HALF_WIDTH);
            if !level(px, pz)
                && column_free(world, px.floor() as i32, pz.floor() as i32, s, s + HEIGHT)
            {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use rapidbot_world::DimensionHeight;
    use rapidbot_world::chunk::Chunk;

    use super::*;

    fn state(name: &str, properties: &str) -> u32 {
        let r = Registry::get();
        let block = r.block_by_name(name).unwrap();
        r.states
            .iter()
            .position(|s| s.block == block.id && s.properties.contains(properties))
            .unwrap() as u32
    }

    /// Replace both halves of the door at (x, y, z) with a fresh one.
    fn set_door(world: &mut World, x: i32, y: i32, z: i32, name: &str, open: bool) {
        let properties =
            |half: &str| format!("facing=north,half={half},hinge=left,open={open},powered=false");
        world.set_block_state(x, y, z, state(name, &properties("lower")));
        world.set_block_state(x, y + 1, z, state(name, &properties("upper")));
    }

    /// Stone floor with its top at y = 64 over x, z in -32..48.
    fn flat() -> World {
        let mut world = World::new(DimensionHeight::OVERWORLD);
        for cx in -2..=2 {
            for cz in -2..=2 {
                world.insert_chunk(cx, cz, Chunk::empty(24));
                for x in 0..16 {
                    for z in 0..16 {
                        world.set_block_state(cx * 16 + x, 63, cz * 16 + z, 1);
                    }
                }
            }
        }
        world
    }

    fn start() -> Vec3 {
        Vec3::new(0.5, 64.0, 0.5)
    }

    #[test]
    fn straight_across_open_ground() {
        let world = flat();
        let path = find(&world, start(), Goal::Column { x: 10, z: 10 }, 10_000).unwrap();
        assert!(path.complete);
        // Ten diagonal moves.
        assert_eq!(path.points.len(), 11);
        assert_eq!(path.points[10].pos(), Vec3::new(10.5, 64.0, 10.5));
        assert!(
            path.points.iter().all(|p| !p.jump),
            "diagonals are not gap jumps"
        );
    }

    #[test]
    fn walks_through_a_closed_wooden_door() {
        let mut world = flat();
        // A solid two-high wall right across the loaded world, with one
        // wooden door in it.
        for x in -32..=47 {
            for y in 64..=65 {
                world.set_block_state(x, y, 4, 1);
            }
        }
        set_door(&mut world, 0, 64, 4, "minecraft:oak_door", false);
        assert_eq!(surface(&world, 0, 64, 4), None);

        let path = find(&world, start(), Goal::Column { x: 0, z: 10 }, 20_000).unwrap();
        assert!(path.complete, "did not plan through the door");
        let door = path
            .points
            .iter()
            .find(|p| p.door.is_some())
            .expect("no door waypoint");
        assert_eq!(door.cell, (0, 64, 4));
        assert!(!door.door.unwrap().open);
        assert!(
            path.points
                .iter()
                .filter(|p| p.cell.2 == 4)
                .all(|p| p.cell.0 == 0),
            "crossed the wall outside the door: {:?}",
            path.points
        );

        // Once open, the doorway is an ordinary surface.
        set_door(&mut world, 0, 64, 4, "minecraft:oak_door", true);
        assert_eq!(surface(&world, 0, 64, 4), Some(64.0));
        let path = find(&world, start(), Goal::Column { x: 0, z: 10 }, 20_000).unwrap();
        assert!(path.complete);
        assert!(
            path.points
                .iter()
                .any(|p| p.cell == (0, 64, 4) && p.door.is_some_and(|d| d.open))
        );
    }

    #[test]
    fn an_iron_door_is_not_a_route() {
        let mut world = flat();
        for x in -32..=47 {
            for y in 64..=65 {
                world.set_block_state(x, y, 4, 1);
            }
        }
        set_door(&mut world, 0, 64, 4, "minecraft:iron_door", false);
        let path = find(&world, start(), Goal::Column { x: 0, z: 10 }, 20_000).unwrap();
        assert!(!path.complete, "planned through a closed iron door");
        assert!(
            path.points.iter().all(|p| p.cell.2 != 4),
            "reached the wall: {:?}",
            path.points
        );
    }

    #[test]
    fn goes_around_a_wall() {
        let mut world = flat();
        for x in -5..=5 {
            for y in 64..=65 {
                world.set_block_state(x, y, 4, 1);
            }
        }
        let path = find(&world, start(), Goal::Column { x: 0, z: 10 }, 10_000).unwrap();
        assert!(path.complete);
        assert!(
            path.points.iter().any(|p| p.cell.0.abs() >= 6),
            "went round the end of the wall"
        );
        assert!(
            path.points
                .iter()
                .all(|p| p.cell.2 != 4 || p.cell.0.abs() > 5)
        );
        // Corners are not cut: next to the wall's end the path moves squarely.
        for pair in path.points.windows(2) {
            let (a, b) = (pair[0].cell, pair[1].cell);
            if a.0 != b.0 && a.2 != b.2 {
                assert!(
                    surface(&world, a.0, 64, b.2).is_some()
                        && surface(&world, b.0, 64, a.2).is_some()
                );
            }
        }
    }

    #[test]
    fn hops_up_one_block_but_not_two() {
        let mut world = flat();
        // A one-high ledge across z = 3..6, and a two-high one behind it.
        for x in -32..48 {
            for z in 3..6 {
                world.set_block_state(x, 64, z, 1);
            }
            for y in 64..=66 {
                world.set_block_state(x, y, 6, 1);
            }
        }
        let path = find(&world, start(), Goal::Column { x: 0, z: 4 }, 10_000).unwrap();
        assert!(path.complete);
        assert_eq!(path.points.last().unwrap().surface, 65.0);
        // The top of the higher wall is two up from the ledge: out of reach.
        let path = find(&world, start(), Goal::Column { x: 0, z: 6 }, 10_000).unwrap();
        assert!(!path.complete);
        assert_eq!(
            path.points.last().unwrap().cell.2,
            5,
            "ends as near as it can get"
        );
    }

    #[test]
    fn drops_three_but_not_five() {
        let mut world = flat();
        // Raise the start onto a platform 3 above the floor.
        for x in -2..=2 {
            for z in -2..=2 {
                for y in 64..=66 {
                    world.set_block_state(x, y, z, 1);
                }
            }
        }
        let high = Vec3::new(0.5, 67.0, 0.5);
        let path = find(&world, high, Goal::Column { x: 8, z: 0 }, 10_000).unwrap();
        assert!(path.complete);
        assert_eq!(path.points.last().unwrap().surface, 64.0);

        for x in -2..=2 {
            for z in -2..=2 {
                for y in 67..=68 {
                    world.set_block_state(x, y, z, 1);
                }
            }
        }
        let higher = Vec3::new(0.5, 69.0, 0.5);
        let path = find(&world, higher, Goal::Column { x: 8, z: 0 }, 10_000).unwrap();
        assert!(!path.complete, "a five-block drop is not taken");
    }

    #[test]
    fn slabs_are_steps_and_fences_are_walls() {
        let mut world = flat();
        let slab = state("minecraft:stone_slab", "type=bottom,waterlogged=false");
        // Slab then a full block: two half steps, no jump needed.
        world.set_block_state(0, 64, 2, slab);
        world.set_block_state(0, 64, 3, 1);
        assert_eq!(surface(&world, 0, 64, 2), Some(64.5));
        assert_eq!(surface(&world, 0, 65, 3), Some(65.0));
        assert_eq!(surface(&world, 0, 64, 3), None);

        let fence = state(
            "minecraft:oak_fence",
            "east=false,north=false,south=false,waterlogged=false,west=false",
        );
        for x in -32..48 {
            world.set_block_state(x, 64, 8, fence);
        }
        // 1.5 high: standing on it is possible, getting up there is not.
        assert_eq!(surface(&world, 0, 65, 8), Some(65.5));
        let path = find(&world, start(), Goal::Column { x: 0, z: 12 }, 20_000).unwrap();
        assert!(!path.complete);
    }

    #[test]
    fn keeps_out_of_lava_and_wades_only_if_it_must() {
        let mut world = flat();
        let lava = state("minecraft:lava", "level=0");
        let water = state("minecraft:water", "level=0");
        // A lava trench across the way with one gap at x = 6, and a water
        // channel (one deep) further on with a bridge at x = -6.
        for x in -32..48 {
            if x != 6 {
                world.set_block_state(x, 63, 4, lava);
            }
            for z in 8..=10 {
                if x != -6 {
                    world.set_block_state(x, 63, z, water);
                    world.set_block_state(x, 62, z, 1);
                }
            }
        }
        let path = find(&world, start(), Goal::Column { x: 0, z: 12 }, 40_000).unwrap();
        assert!(path.complete);
        assert!(
            path.points
                .iter()
                .all(|p| trait_at(&world, p.cell.0, p.cell.1, p.cell.2) & DANGER == 0),
            "stepped into lava"
        );
        // The trench is one block wide, so the route may jump it anywhere.
        // If it chooses the stone gap instead there is a waypoint at z = 4.
        if let Some(p) = path.points.iter().find(|p| p.cell.2 == 4) {
            assert_eq!(p.cell.0, 6, "crossed the lava at the gap");
        }
        // Wading three blocks costs less than the walk to the bridge and back.
        let crossing = path.points.iter().find(|p| p.cell.2 == 8).unwrap().cell;
        assert_ne!(crossing.0, -6);
        assert_eq!(surface(&world, 0, 64, 4), None);
    }

    #[test]
    fn swims_across_deep_water() {
        let mut world = flat();
        let water = state("minecraft:water", "level=0");
        // A three-wide, two-deep channel: too wide to jump and too deep to
        // wade, with stone underneath.
        for x in -32..48 {
            for z in 4..=6 {
                world.set_block_state(x, 63, z, water);
                world.set_block_state(x, 62, z, water);
                world.set_block_state(x, 61, z, 1);
            }
        }
        let path = find(&world, start(), Goal::Column { x: 0, z: 12 }, 40_000).unwrap();
        assert!(path.complete);
        let crossing = path.points.iter().find(|p| p.cell.2 == 4).unwrap();
        assert_eq!(crossing.cell.1, 63, "the surface water cell");
        let expected = 63.0 + 8.0 / 9.0;
        assert!(
            (crossing.surface - expected).abs() < 1.0e-6,
            "float at the water top, got {}",
            crossing.surface
        );
        assert!(
            path.points
                .windows(2)
                .all(|p| (p[0].surface - p[1].surface).abs() < 1.5),
            "no impossible jumps between waypoints"
        );
    }

    #[test]
    fn climbs_a_ladder_to_a_platform() {
        let mut world = flat();
        let ladder = state("minecraft:ladder", "waterlogged=false");
        // A ladder on the south side of a four-block platform.
        for y in 64..=67 {
            world.set_block_state(0, y, 1, ladder);
            for x in -1..=1 {
                world.set_block_state(x, y, 2, 1);
            }
        }
        assert_eq!(surface(&world, 0, 64, 1), Some(64.0));
        assert_eq!(surface(&world, 0, 67, 1), Some(67.0));
        let path = find(&world, start(), Goal::Block { x: 0, y: 68, z: 2 }, 20_000).unwrap();
        assert!(path.complete, "did not reach the platform");
        assert!(
            path.points
                .iter()
                .any(|p| p.cell == (0, 66, 1) && p.surface == 66.0),
            "path did not climb the ladder: {:?}",
            path.points
        );
        assert_eq!(path.points.last().unwrap().pos(), Vec3::new(0.5, 68.0, 2.5));
    }

    #[test]
    fn jumps_a_short_gap_but_not_a_wide_one() {
        let mut world = flat();
        // A two-block-wide trench right across the world. There is no way
        // around, so crossing it means a jump.
        for x in -32..48 {
            for z in 4..=5 {
                world.set_block_state(x, 63, z, 0);
            }
        }
        let path = find(&world, start(), Goal::Column { x: 0, z: 8 }, 10_000).unwrap();
        assert!(path.complete, "did not cross the gap");
        let jump = path
            .points
            .iter()
            .find(|p| p.jump)
            .expect("no jump in the path");
        assert_eq!(jump.cell.2, 6, "landed just past the gap");
        assert_eq!(jump.surface, 64.0);
        assert_eq!(
            path.points.iter().filter(|p| p.jump).count(),
            1,
            "one jump is enough"
        );

        // Four empty blocks are beyond a reliable sprint jump.
        for x in -32..48 {
            for z in 4..=7 {
                world.set_block_state(x, 63, z, 0);
            }
        }
        let path = find(&world, start(), Goal::Column { x: 0, z: 9 }, 10_000).unwrap();
        assert!(!path.complete, "crossed a gap that is too wide");
    }

    #[test]
    fn does_not_jump_under_a_low_ceiling() {
        let mut world = flat();
        for x in -32..48 {
            for z in 4..=5 {
                world.set_block_state(x, 63, z, 0);
                // A low ceiling over the gap only: standing on either side
                // is fine, but there is no room to jump across.
                world.set_block_state(x, 66, z, 1);
            }
        }
        let path = find(&world, start(), Goal::Column { x: 0, z: 8 }, 10_000).unwrap();
        assert!(!path.complete, "jumped into the ceiling");
    }

    #[test]
    fn line_of_walk() {
        let mut world = flat();
        let a = Vec3::new(0.5, 64.0, 0.5);
        assert!(line_walkable(&world, a, Vec3::new(6.5, 64.0, 3.5)));
        world.set_block_state(3, 64, 2, 1);
        assert!(
            !line_walkable(&world, a, Vec3::new(6.5, 64.0, 3.5)),
            "a block in the way"
        );
        world.set_block_state(3, 64, 2, 0);
        world.set_block_state(3, 63, 2, 0);
        assert!(
            !line_walkable(&world, a, Vec3::new(6.5, 64.0, 3.5)),
            "a hole in the way"
        );
        // Along a wall is fine; along a drop is not.
        world.set_block_state(3, 63, 2, 1);
        for z in 0..8 {
            world.set_block_state(1, 64, z, 1);
            world.set_block_state(-1, 63, z, 0);
        }
        let b = Vec3::new(0.5, 64.0, 6.5);
        assert!(!line_walkable(
            &world,
            Vec3::new(0.25, 64.0, 0.5),
            Vec3::new(0.25, 64.0, 6.5)
        ));
        assert!(line_walkable(
            &world,
            Vec3::new(0.69, 64.0, 0.5),
            Vec3::new(0.69, 64.0, 6.5)
        ));
        assert!(line_walkable(&world, a, b));
    }
}
