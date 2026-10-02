//! Finding a way across the block grid: A* over the places a player can
//! stand, with the moves a person on foot makes (walk, step up, hop up a
//! block, drop a few blocks). Not vanilla: this is bot logic, and it only
//! decides *where* to go. [`crate::nav::Walker`] turns the result into
//! mouse and key input.
//!
//! Not handled yet: swimming across deep water, ladders, gap jumps, doors,
//! breaking or placing blocks.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::OnceLock;

use rapidbot_physics::math::Vec3;
use rapidbot_world::collision::block_collisions;
use rapidbot_world::{Aabb, Axis, Registry, World};

/// Player box: 0.6 wide, 1.8 tall.
const HALF_WIDTH: f64 = 0.3;
const HEIGHT: f64 = 1.8;
/// `maxUpStep` is 0.6; a jump clears 1.25, so one block and a bit.
const STEP: f64 = 0.6;
const JUMP: f64 = 1.2;
/// Falls of more than three blocks hurt.
const MAX_DROP: f64 = 3.2;

const DANGER: u8 = 1;
const WATER: u8 = 2;
const CACTUS: u8 = 4;

/// Per block state: whether touching it hurts or traps, and whether it
/// holds water.
fn traits() -> &'static [u8] {
    static TRAITS: OnceLock<Vec<u8>> = OnceLock::new();
    TRAITS.get_or_init(|| {
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
                t
            })
            .collect()
    })
}

fn trait_at(world: &World, x: i32, y: i32, z: i32) -> u8 {
    world.block_state(x, y, z).map_or(0, |s| traits()[s as usize])
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
        let Some(state) = world.block_state(x, y, z) else { return false };
        let Some(shape) = registry.collision_shape(state) else { continue };
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
/// `None` if there is nothing to stand on, no room, or it is harmful.
pub fn surface(world: &World, x: i32, y: i32, z: i32) -> Option<f64> {
    let registry = Registry::get();
    let here = world.block_state(x, y, z)?;
    let (s, floor) = match registry.collision_shape(here) {
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
    (0..=16).flat_map(|d| [y + d, y - d]).find_map(|cy| surface(world, x, cy, z).map(|s| (cy, s)))
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
            Goal::Block { x, y, z } => cell.0 == x && cell.2 == z && (surface - y as f64).abs() < 1.0,
        }
    }
}

/// A place to stand on the way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Waypoint {
    pub cell: (i32, i32, i32),
    /// Feet height when standing there.
    pub surface: f64,
}

impl Waypoint {
    /// Centre of the cell at standing height.
    pub fn pos(&self) -> Vec3 {
        Vec3::new(self.cell.0 as f64 + 0.5, self.surface, self.cell.2 as f64 + 0.5)
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
    /// `None` if that is nowhere a path can start: in the air, in deep
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
            nodes: vec![Node { cell, surface, cost: 0.0, parent: NO_PARENT, closed: false }],
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
        *self.surfaces.entry(cell).or_insert_with(|| surface(world, cell.0, cell.1, cell.2))
    }

    /// Examines up to `budget` more places. Returns the path once the
    /// search is over, `None` while it is still going.
    pub fn step(&mut self, world: &World, budget: usize) -> Option<Path> {
        for _ in 0..budget {
            let Some(Reverse((_, id))) = self.open.pop() else { return Some(self.path()) };
            let node = &mut self.nodes[id as usize];
            if node.closed {
                continue;
            }
            node.closed = true;
            let (cell, surface, cost) = (node.cell, node.surface, node.cost);
            if self.goal.reached(cell, surface) {
                self.best = id;
                return Some(self.path());
            }
            let best = &self.nodes[self.best as usize];
            let (h, best_h) = (self.goal.estimate(cell), self.goal.estimate(best.cell));
            if h < best_h || (h == best_h && cost < best.cost) {
                self.best = id;
            }
            self.expanded += 1;
            if self.expanded >= self.limit {
                return Some(self.path());
            }
            self.expand(world, id, cell, surface, cost);
        }
        None
    }

    fn expand(&mut self, world: &World, id: u32, cell: (i32, i32, i32), s: f64, cost: f64) {
        let (x, y, z) = cell;
        for (dx, dz) in CARDINALS {
            // The highest place in the next column that can be got to.
            for ny in (y - 4..=y + 1).rev() {
                let Some(s2) = self.surface(world, (x + dx, ny, z + dz)) else { continue };
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
                break;
            }
        }
        for (dx, dz) in DIAGONALS {
            // Only on the level or a step, and never cutting a corner.
            for ny in [y, y + 1, y - 1] {
                let Some(s2) = self.surface(world, (x + dx, ny, z + dz)) else { continue };
                let d = s2 - s;
                if d.abs() > STEP {
                    continue;
                }
                let high = s.max(s2);
                let side_ok = |cx: i32, cz: i32| {
                    column_free(world, cx, cz, high, high + HEIGHT)
                        && (trait_at(world, cx, high.floor() as i32, cz) | trait_at(world, cx, high.floor() as i32 - 1, cz))
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
    }

    fn relax(&mut self, world: &World, parent: u32, cell: (i32, i32, i32), surface: f64, mut cost: f64) {
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
                self.nodes.push(Node { cell, surface, cost, parent, closed: false });
                self.index.insert(cell, id);
                id
            }
        };
        // Slightly greedy: much faster, paths barely longer.
        let f = cost + 1.2 * self.goal.estimate(cell);
        self.open.push(Reverse(((f * 1024.0) as u64, id)));
    }

    fn path(&self) -> Path {
        let mut points = Vec::new();
        let mut id = self.best;
        while id != NO_PARENT {
            let node = &self.nodes[id as usize];
            points.push(Waypoint { cell: node.cell, surface: node.surface });
            id = node.parent;
        }
        points.reverse();
        let end = &self.nodes[self.best as usize];
        Path { points, complete: self.goal.reached(end.cell, end.surface) }
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
        surface(world, x.floor() as i32, cell_y, z.floor() as i32).is_some_and(|v| (v - s).abs() < 0.01)
    };
    let (dx, dz) = (to.x - from.x, to.z - from.z);
    let steps = ((dx * dx + dz * dz).sqrt() / 0.25).ceil().max(1.0) as usize;
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        let (x, z) = (from.x + dx * t, from.z + dz * t);
        if !level(x, z) {
            return false;
        }
        let bbox = Aabb::new(x - HALF_WIDTH, s, z - HALF_WIDTH, x + HALF_WIDTH, s + HEIGHT, z + HALF_WIDTH);
        if !block_collisions(world, registry, &bbox.deflate(1.0e-7, 1.0e-7, 1.0e-7)).is_empty() {
            return false;
        }
        // Under each corner: level ground or a wall, not an edge.
        for (cx, cz) in [(-1.0, -1.0), (-1.0, 1.0), (1.0, -1.0), (1.0, 1.0)] {
            let (px, pz) = (x + cx * HALF_WIDTH, z + cz * HALF_WIDTH);
            if !level(px, pz) && column_free(world, px.floor() as i32, pz.floor() as i32, s, s + HEIGHT) {
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
        r.states.iter().position(|s| s.block == block.id && s.properties.contains(properties)).unwrap() as u32
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
        assert!(path.points.iter().any(|p| p.cell.0.abs() >= 6), "went round the end of the wall");
        assert!(path.points.iter().all(|p| p.cell.2 != 4 || p.cell.0.abs() > 5));
        // Corners are not cut: next to the wall's end the path moves squarely.
        for pair in path.points.windows(2) {
            let (a, b) = (pair[0].cell, pair[1].cell);
            if a.0 != b.0 && a.2 != b.2 {
                assert!(surface(&world, a.0, 64, b.2).is_some() && surface(&world, b.0, 64, a.2).is_some());
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
        assert_eq!(path.points.last().unwrap().cell.2, 5, "ends as near as it can get");
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

        let fence = state("minecraft:oak_fence", "east=false,north=false,south=false,waterlogged=false,west=false");
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
            if x != -6 {
                world.set_block_state(x, 63, 8, water);
                world.set_block_state(x, 62, 8, 1);
            }
        }
        let path = find(&world, start(), Goal::Column { x: 0, z: 12 }, 40_000).unwrap();
        assert!(path.complete);
        let crossing = |z: i32| path.points.iter().find(|p| p.cell.2 == z).unwrap().cell;
        assert_eq!(crossing(4).0, 6, "crossed the lava at the gap");
        // Wading one block costs less than the walk to the bridge and back.
        assert_ne!(crossing(8).0, -6);
        assert_eq!(surface(&world, 0, 64, 4), None);
    }

    #[test]
    fn line_of_walk() {
        let mut world = flat();
        let a = Vec3::new(0.5, 64.0, 0.5);
        assert!(line_walkable(&world, a, Vec3::new(6.5, 64.0, 3.5)));
        world.set_block_state(3, 64, 2, 1);
        assert!(!line_walkable(&world, a, Vec3::new(6.5, 64.0, 3.5)), "a block in the way");
        world.set_block_state(3, 64, 2, 0);
        world.set_block_state(3, 63, 2, 0);
        assert!(!line_walkable(&world, a, Vec3::new(6.5, 64.0, 3.5)), "a hole in the way");
        // Along a wall is fine; along a drop is not.
        world.set_block_state(3, 63, 2, 1);
        for z in 0..8 {
            world.set_block_state(1, 64, z, 1);
            world.set_block_state(-1, 63, z, 0);
        }
        let b = Vec3::new(0.5, 64.0, 6.5);
        assert!(!line_walkable(&world, Vec3::new(0.25, 64.0, 0.5), Vec3::new(0.25, 64.0, 6.5)));
        assert!(line_walkable(&world, Vec3::new(0.69, 64.0, 0.5), Vec3::new(0.69, 64.0, 6.5)));
        assert!(line_walkable(&world, a, b));
    }
}
