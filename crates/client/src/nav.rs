//! Route finding and movement using modeled input.
//!
//! [`Walker`] takes a controller somewhere on foot. It plans a route with
//! [`crate::path`] (a moment of standing still, spread over a few ticks),
//! then follows it using the mouse and keyboard input models, cutting across
//! open ground instead of following the grid and hopping up blocks. Where no
//! route can be planned (in water, in mid-air) it heads straight for the goal.

use rapidbot_human::Noise;
use rapidbot_physics::Keys;
use rapidbot_physics::math::{Vec3, wrap_degrees};

use crate::controller::{TickContext, angles_to};
use crate::path::{self, Goal, Path, Search};

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Idle,
    Walking,
}

/// How the walker is getting to its goal.
enum Plan {
    /// Nothing decided yet.
    None,
    /// Working out a route; standing still meanwhile.
    Searching(Search),
    Following {
        path: Path,
        /// The next waypoint to reach.
        next: usize,
        /// The waypoint being steered at (`next` or further on, when the
        /// way there is open), and ticks until that is looked at again.
        target: usize,
        retarget_in: u32,
    },
    /// No route: straight at the goal.
    Straight,
}

/// Places a search may examine before settling for the nearest it found.
const SEARCH_LIMIT: usize = 24_000;
/// Places examined per tick: about a millisecond's work.
const SEARCH_PER_TICK: usize = 400;

pub struct Walker {
    noise: Noise,
    goal: Option<Vec3>,
    state: State,
    /// Gaze height offset so the view is not locked dead level.
    gaze_pitch: f32,
    /// Delay before a jump key press when blocked, in ticks.
    jump_in: Option<u32>,
    /// Ticks left to keep the jump key down (a tap lasts a few ticks).
    jump_hold: u32,
    /// Whether this player sprint-jumps on long stretches (many do).
    pub sprint_jumps: bool,
    /// Closest the player has been to the goal, and for how many ticks
    /// that has not improved.
    best_distance: f64,
    stalled_ticks: u32,
    gave_up: bool,
    plan: Plan,
    /// Searches started for this goal without getting anywhere new.
    replans: u32,
    /// Whether the goal's height matters.
    exact: bool,
}

impl Walker {
    pub fn new(seed: u64) -> Self {
        let mut noise = Noise::new(seed);
        let sprint_jumps = noise.chance(0.6);
        Self {
            noise,
            sprint_jumps,
            best_distance: f64::MAX,
            stalled_ticks: 0,
            gave_up: false,
            plan: Plan::None,
            replans: 0,
            exact: false,
            goal: None,
            state: State::Idle,
            gaze_pitch: 8.0,
            jump_in: None,
            jump_hold: 0,
        }
    }

    /// Heads for a point, standing wherever the ground is there: of several
    /// floors (a cave under a hill), the one nearest the height given.
    pub fn go_to(&mut self, goal: Vec3) {
        self.start(goal, false);
    }

    /// Heads for a point at that height (within a block): for goals on a
    /// particular floor of a building or level of a cave.
    pub fn go_to_exact(&mut self, goal: Vec3) {
        self.start(goal, true);
    }

    fn start(&mut self, goal: Vec3, exact: bool) {
        self.goal = Some(goal);
        self.exact = exact;
        self.plan = Plan::None;
        self.replans = 0;
        self.best_distance = f64::MAX;
        self.stalled_ticks = 0;
        self.gave_up = false;
        if self.state == State::Idle {
            self.state = State::Walking;
            self.gaze_pitch = self.noise.gaussian(8.0, 5.0).clamp(-10.0, 30.0) as f32;
        }
    }

    pub fn stop(&mut self) {
        self.goal = None;
        self.plan = Plan::None;
        self.state = State::Idle;
    }

    /// True once the goal is reached (or there is none).
    pub fn arrived(&self) -> bool {
        self.goal.is_none()
    }

    /// True if the last goal was abandoned: no way there was found, or the
    /// way turned out to be blocked.
    pub fn gave_up(&self) -> bool {
        self.gave_up
    }

    pub fn tick(&mut self, ctx: &mut TickContext<'_>) {
        match self.state {
            State::Idle => {
                ctx.set_keys(Keys::default());
            }
            State::Walking => self.walk(ctx),
        }
    }

    fn give_up(&mut self, ctx: &mut TickContext<'_>) {
        self.goal = None;
        self.plan = Plan::None;
        self.gave_up = true;
        self.state = State::Idle;
        ctx.set_keys(Keys::default());
    }

    /// Counts ticks without getting closer to `distance` = 0. True once a
    /// person would stop pushing.
    fn stalled(&mut self, distance: f64) -> bool {
        if distance < self.best_distance - 0.3 {
            self.best_distance = distance;
            self.stalled_ticks = 0;
            return false;
        }
        self.stalled_ticks += 1;
        self.stalled_ticks > 50 + self.noise.uniform(0.0, 40.0) as u32
    }

    fn walk(&mut self, ctx: &mut TickContext<'_>) {
        let Some(goal) = self.goal else {
            self.state = State::Idle;
            return;
        };
        let pos = ctx.player.pos;
        let (dx, dz) = (goal.x - pos.x, goal.z - pos.z);
        let distance = (dx * dx + dz * dz).sqrt();
        if distance < 0.45 && (!self.exact || (goal.y - pos.y).abs() < 1.5) {
            self.goal = None;
            self.plan = Plan::None;
            self.state = State::Idle;
            ctx.set_keys(Keys::default());
            return;
        }

        match &mut self.plan {
            Plan::None => {
                // Routes start from the ground: wait out a fall.
                if !ctx.player.on_ground && !ctx.player.in_water {
                    ctx.set_keys(Keys::default());
                    return;
                }
                let (x, z) = (goal.x.floor() as i32, goal.z.floor() as i32);
                let y = goal.y.floor() as i32;
                let target = if self.exact {
                    Goal::Block { x, y, z }
                } else {
                    // The ground there nearest the height asked for; any
                    // height if none is in sight yet.
                    match path::nearest_surface(ctx.world, x, z, y) {
                        Some((cell_y, _)) => Goal::Block { x, y: cell_y, z },
                        None => Goal::Column { x, z },
                    }
                };
                self.best_distance = f64::MAX;
                self.stalled_ticks = 0;
                self.plan = match Search::new(ctx.world, pos, target, SEARCH_LIMIT) {
                    Some(search) => Plan::Searching(search),
                    None => Plan::Straight,
                };
            }
            Plan::Searching(search) => {
                // Thinking: stand, and turn towards where we want to go.
                ctx.set_keys(Keys::default());
                let eye = ctx.player.eye_position();
                let (yaw, _) = angles_to(eye, Vec3::new(goal.x, eye.y, goal.z));
                ctx.aim(yaw, self.gaze_pitch, 8.0);
                let Some(path) = search.step(ctx.world, SEARCH_PER_TICK) else {
                    return;
                };
                if path.points.len() < 2 {
                    if path.complete {
                        // Already in the goal's block: finish on foot.
                        self.plan = Plan::Straight;
                    } else {
                        self.give_up(ctx);
                    }
                    return;
                }
                self.best_distance = f64::MAX;
                self.stalled_ticks = 0;
                self.plan = Plan::Following {
                    path,
                    next: 1,
                    target: 1,
                    retarget_in: 0,
                };
            }
            Plan::Following { .. } => self.follow(ctx),
            Plan::Straight => {
                if self.stalled(distance) {
                    self.give_up(ctx);
                    return;
                }
                self.steer(ctx, goal, distance, true);
            }
        }
    }

    fn follow(&mut self, ctx: &mut TickContext<'_>) {
        let Plan::Following {
            path,
            next,
            target,
            retarget_in,
        } = &mut self.plan
        else {
            return;
        };
        let pos = ctx.player.pos;
        let flat = |a: Vec3| ((a.x - pos.x).powi(2) + (a.z - pos.z).powi(2)).sqrt();

        // Tick off waypoints: the one aimed for, or any on the way to it.
        let reach = (*target + 1).min(path.points.len());
        if let Some(i) = (*next..reach).rev().find(|&i| {
            let w = path.points[i].pos();
            flat(w) < 0.4 && pos.y - w.y > -0.05 && pos.y - w.y < 1.3
        }) {
            *next = i + 1;
            *retarget_in = 0;
            self.best_distance = f64::MAX;
            self.stalled_ticks = 0;
            self.replans = 0;
        }
        if *next >= path.points.len() {
            // At the end: the last few steps to the exact spot, or, if the
            // path stopped short, another look from here.
            self.plan = if path.complete {
                Plan::Straight
            } else {
                Plan::None
            };
            self.best_distance = f64::MAX;
            self.stalled_ticks = 0;
            return;
        }

        // Knocked, fallen or moved off the route, or the route changed
        // under us: think again.
        let ahead = path.points[*next];
        let off_route = flat(ahead.pos()) > 6.0 || (pos.y - ahead.surface).abs() > 4.5;
        let broken = ctx.tick % 20 == 0
            && path.points[*next..(*next + 8).min(path.points.len())]
                .iter()
                .any(|w| {
                    path::surface(ctx.world, w.cell.0, w.cell.1, w.cell.2)
                        .is_none_or(|s| (s - w.surface).abs() > 0.01)
                });
        if off_route || broken {
            self.plan = Plan::None;
            return;
        }

        // Where to steer: as far along the route as can be walked to in a
        // straight line on this level. Looked at afresh a few times a second.
        if *retarget_in == 0 || *target < *next {
            *target = *next;
            if ctx.player.on_ground {
                for j in *next..(*next + 8).min(path.points.len()) {
                    let w = path.points[j];
                    if (w.surface - pos.y).abs() > 0.01 {
                        break;
                    }
                    if path::line_walkable(ctx.world, pos, w.pos()) {
                        *target = j;
                    }
                }
            }
            *retarget_in = 4;
        }
        *retarget_in -= 1;
        let aim_at = path.points[*target].pos();
        // Room to run: the open stretch ahead, and more route beyond it.
        let run = flat(aim_at) + (path.points.len() - 1 - *target) as f64;
        let open = flat(aim_at);

        let to_next = flat(path.points[*next].pos());
        if self.stalled(to_next) {
            self.replans += 1;
            if self.replans > 2 {
                self.give_up(ctx);
            } else {
                self.plan = Plan::None;
            }
            return;
        }
        self.steer(ctx, aim_at, run.min(open + 3.0), false);
    }

    /// Turns towards `target` and holds the keys to get there. `distance`
    /// is how much room there is to build up speed.
    fn steer(&mut self, ctx: &mut TickContext<'_>, target: Vec3, distance: f64, slow_near: bool) {
        let pos = ctx.player.pos;
        let near = ((target.x - pos.x).powi(2) + (target.z - pos.z).powi(2)).sqrt();
        let eye = ctx.player.eye_position();
        let (yaw, _) = angles_to(eye, Vec3::new(target.x, eye.y, target.z));
        // Loose tolerance far away, tighter when close so we do not orbit.
        let tolerance = if near > 6.0 { 4.0 } else { 2.0 };
        ctx.aim(yaw, self.gaze_pitch, tolerance);

        let yaw_error = wrap_degrees(yaw - ctx.player.y_rot).abs();
        // People start walking while still turning, but not backwards, and
        // not off to the side of something close.
        let forward = yaw_error < if near < 1.5 && !slow_near { 30.0 } else { 50.0 };
        let sprint = forward && yaw_error < 15.0 && distance > 5.0;

        // Blocked by something low: hop, a moment after bumping into it.
        if ctx.player.horizontal_collision && ctx.player.on_ground && forward {
            match self.jump_in {
                None => self.jump_in = Some(self.noise.uniform(1.0, 4.0) as u32),
                Some(0) => {
                    self.jump_hold = self.noise.uniform(3.0, 6.0) as u32;
                    self.jump_in = None;
                }
                Some(n) => self.jump_in = Some(n - 1),
            }
        } else {
            self.jump_in = None;
        }
        // Holding space while sprinting a long way: a hop every landing.
        // In water, space keeps the head up.
        let jump = self.jump_hold > 0
            || (self.sprint_jumps && sprint && distance > 8.0)
            || ctx.player.in_water;
        self.jump_hold = self.jump_hold.saturating_sub(1);

        ctx.set_keys(Keys {
            forward,
            sprint,
            jump,
            ..Keys::default()
        });
    }
}

#[cfg(test)]
mod tests {
    use rapidbot_human::MouseModel;
    use rapidbot_physics::Player;
    use rapidbot_world::chunk::Chunk;
    use rapidbot_world::tags::Tags;
    use rapidbot_world::{DimensionHeight, World};

    use super::*;
    use crate::MouseSettings;

    /// Runs walker + mouse + physics the way the game loop does: per tick
    /// the controller then physics, then three 60 fps mouse frames.
    struct Sim {
        world: World,
        tags: Tags,
        player: Player,
        mouse: MouseModel,
        aim: Option<rapidbot_human::Aim>,
        walker: Walker,
        keys: Keys,
        tick: u64,
        chat: std::collections::VecDeque<String>,
        inventory: crate::inventory::Inventory,
        actions: crate::controller::Actions,
    }

    impl Sim {
        fn new() -> Self {
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
            let mut player = Player::new();
            player.y_rot = 0.0;
            player.set_pos(Vec3::new(0.5, 64.0, 0.5));
            Self {
                world,
                tags: Tags::default(),
                player,
                mouse: MouseModel::new(9),
                aim: None,
                walker: Walker::new(4),
                keys: Keys::default(),
                tick: 0,
                chat: Default::default(),
                inventory: Default::default(),
                actions: Default::default(),
            }
        }

        fn step(&mut self) {
            let crosshair = crate::interact::pick_block(&self.player, &self.world);
            let entities = crate::entities::Entities::default();
            let mut ctx = TickContext {
                player: &mut self.player,
                world: &self.world,
                tick: self.tick,
                players: &[],
                chat_open: false,
                inventory: &self.inventory,
                entities: &entities,
                crosshair,
                target: None,
                attack_strength: 1.0,
                own_id: 0,
                tags: &self.tags,
                breaking: None,
                actions: &mut self.actions,
                aim: &mut self.aim,
                keys: &mut self.keys,
                chat: &mut self.chat,
            };
            self.walker.tick(&mut ctx);
            // No keyboard model here: fingers follow at once.
            self.player.held_keys = self.keys;
            self.tick += 1;
            self.player.tick(&self.world, &self.tags);
            let settings = MouseSettings::default();
            for _ in 0..3 {
                self.mouse.set_target(self.aim);
                let (dx, dy) = self.mouse.frame(
                    1.0 / 60.0,
                    self.player.y_rot,
                    self.player.x_rot,
                    settings.degrees_per_count(),
                );
                settings.turn_player(&mut self.player, dx, dy);
            }
        }
    }

    #[test]
    fn walks_to_a_goal_behind_it() {
        let mut sim = Sim::new();
        // Behind and to the side: needs a big turn first.
        sim.walker.go_to(Vec3::new(-9.0, 64.0, -12.0));
        let mut ticks = 0;
        while !sim.walker.arrived() {
            sim.step();
            ticks += 1;
            assert!(ticks < 400, "never arrived, at {:?}", sim.player.pos);
        }
        let p = sim.player.pos;
        assert!((p.x + 9.0).hypot(p.z + 12.0) < 0.6);
        // 15 blocks takes a person about 4-8 seconds including the turn.
        assert!((60..200).contains(&ticks), "took {ticks} ticks");
        assert!(sim.player.on_ground);
    }

    #[test]
    fn walks_around_a_wall() {
        let mut sim = Sim::new();
        // A two-high wall across the way.
        for x in -5..=5 {
            sim.world.set_block_state(x, 64, 4, 1);
            sim.world.set_block_state(x, 65, 4, 1);
        }
        sim.walker.go_to(Vec3::new(0.5, 64.0, 20.0));
        let mut ticks = 0;
        while !sim.walker.arrived() {
            sim.step();
            ticks += 1;
            assert!(ticks < 600, "never arrived, at {:?}", sim.player.pos);
        }
        assert!(!sim.walker.gave_up());
        let p = sim.player.pos;
        assert!((p.x - 0.5).hypot(p.z - 20.0) < 0.6);
        // About 27 blocks of walking; a detour, not a crawl along the grid.
        assert!(ticks < 260, "took {ticks} ticks");
    }

    #[test]
    fn gives_up_when_the_goal_is_walled_in() {
        let mut sim = Sim::new();
        // A closed two-high ring around the goal.
        for i in -3..=3 {
            for y in 64..=65 {
                sim.world.set_block_state(i, y, 17, 1);
                sim.world.set_block_state(i, y, 23, 1);
                sim.world.set_block_state(-3, y, 20 + i, 1);
                sim.world.set_block_state(3, y, 20 + i, 1);
            }
        }
        sim.walker.go_to(Vec3::new(0.5, 64.0, 20.0));
        for _ in 0..600 {
            sim.step();
        }
        assert!(sim.walker.gave_up());
        assert!(sim.walker.arrived(), "no goal left");
        assert!(!sim.keys.forward, "stopped pushing");
        // It went as near as it could get first.
        assert!(
            sim.player.pos.z > 14.0 && sim.player.pos.z < 17.0,
            "at {:?}",
            sim.player.pos
        );
    }

    #[test]
    fn climbs_steps_and_drops_down() {
        let mut sim = Sim::new();
        // Three steps up to a platform, across it, and a 3-block drop.
        for x in -2..=2 {
            for (z, h) in [(4, 1), (5, 2), (6, 3), (7, 3), (8, 3), (9, 3)] {
                for y in 64..64 + h {
                    sim.world.set_block_state(x, y, z, 1);
                }
            }
        }
        sim.walker.go_to_exact(Vec3::new(0.5, 67.0, 8.5));
        let mut ticks = 0;
        while !sim.walker.arrived() {
            sim.step();
            ticks += 1;
            assert!(ticks < 400, "never got up, at {:?}", sim.player.pos);
        }
        assert_eq!(sim.player.pos.y, 67.0);

        sim.walker.go_to(Vec3::new(0.5, 64.0, 14.5));
        while !sim.walker.arrived() {
            sim.step();
            ticks += 1;
            assert!(ticks < 800, "never got down, at {:?}", sim.player.pos);
        }
        assert!(!sim.walker.gave_up());
        assert_eq!(sim.player.pos.y, 64.0);
        assert!(sim.player.pos.z > 14.0);
    }
}
