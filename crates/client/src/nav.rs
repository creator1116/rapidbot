//! Route finding and movement using modeled input.
//!
//! [`Walker`] takes a controller somewhere on foot. It plans a route with
//! [`crate::path`] (a moment of standing still, spread over a few ticks),
//! then follows it using the mouse and keyboard input models, cutting across
//! open ground instead of following the grid and hopping up blocks. Routes
//! sprint-jump short gaps, cross deep water by swimming at the surface,
//! climb ladders and vines, and open closed wooden doors on the way through.
//! Where no route can be planned (in mid-air, or from under water) it heads
//! straight for the goal.

use rapidbot_human::Noise;
use rapidbot_physics::Keys;
use rapidbot_physics::math::{Vec3, wrap_degrees};

use crate::controller::{TickContext, angles_to};
use crate::door::{self, Door};
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

/// While stopped at a closed door, waiting for the server to open it.
struct DoorWait {
    door: Door,
    ticks: u32,
    /// A right click has been sent; wait for the server's block update
    /// rather than toggling the door closed again if it is slow.
    clicked: bool,
}

/// Places a search may examine before settling for the nearest it found.
const SEARCH_LIMIT: usize = 24_000;
/// Places examined per tick: about a millisecond's work.
const SEARCH_PER_TICK: usize = 400;

/// Whether the player's feet are in a climbable block. Holding jump then
/// climbs, and letting go slides back down.
fn on_climbable(ctx: &TickContext<'_>) -> bool {
    let pos = ctx.player.pos;
    path::climbable_at(
        ctx.world,
        pos.x.floor() as i32,
        pos.y.floor() as i32,
        pos.z.floor() as i32,
    )
}

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
    /// The door being opened, if the next waypoint is one.
    door_wait: Option<DoorWait>,
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
            door_wait: None,
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
        self.door_wait = None;
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
        self.door_wait = None;
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
        self.door_wait = None;
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
                self.door_wait = None;
                // Routes start from the ground: wait out a fall. Someone
                // holding a ladder can be planned from where they are.
                if !ctx.player.on_ground && !ctx.player.in_water && !on_climbable(ctx) {
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
                // Hold a ladder so planning does not turn into sliding down.
                ctx.set_keys(Keys {
                    jump: on_climbable(ctx),
                    ..Keys::default()
                });
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
                self.steer(ctx, goal, distance, true, false);
            }
        }
    }

    fn follow(&mut self, ctx: &mut TickContext<'_>) {
        // The next waypoint may be a closed door. Stop and open it before
        // steering, so the walker does not keep pushing into the panel.
        let next_door = match &self.plan {
            Plan::Following { path, next, .. } if *next < path.points.len() => {
                path.points[*next].door
            }
            _ => None,
        };
        // A closed door ahead caps the waypoints that may be ticked off, so
        // walking up against the panel's near side cannot "reach" the door
        // cell and skip the click.
        let first_closed = match &self.plan {
            Plan::Following { path, next, .. } => path.points[*next..]
                .iter()
                .position(|w| {
                    w.door.is_some_and(|door| {
                        !door::at(ctx.world, door.lower.0, door.lower.1, door.lower.2)
                            .is_some_and(|current| current.open)
                    })
                })
                .map(|i| *next + i),
            _ => None,
        };
        if self
            .door_wait
            .as_ref()
            .is_some_and(|wait| Some(wait.door.lower) != next_door.map(|door| door.lower))
        {
            self.door_wait = None;
        }
        if let Some(door) = next_door
            && self.open_door(ctx, door)
        {
            return;
        }

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
        // Stop before a closed door: the door itself is not reached until it
        // has been opened.
        let limit = first_closed
            .unwrap_or(path.points.len())
            .min(path.points.len());
        let reach = (*target + 1).min(path.points.len()).min(limit).max(*next);
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
            self.door_wait = None;
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
        let broken = ctx.tick.is_multiple_of(20)
            && path.points[*next..(*next + 8).min(path.points.len())]
                .iter()
                .any(|w| {
                    // A door is opened on arrival, so its closed shape is
                    // expected. A door that has gone is caught at the door.
                    w.door.is_none()
                        && path::surface(ctx.world, w.cell.0, w.cell.1, w.cell.2)
                            .is_none_or(|s| (s - w.surface).abs() > 0.01)
                });
        if off_route || broken {
            self.door_wait = None;
            self.plan = Plan::None;
            return;
        }

        // Where to steer: as far along the route as can be walked to in a
        // straight line on this level. Looked at afresh a few times a second.
        if *retarget_in == 0 || *target < *next {
            *target = *next;
            if ctx.player.on_ground {
                for j in *next..(*next + 8).min(path.points.len()).min(limit) {
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

        // A sprint jump needs speed before the edge. Start sprinting while
        // the jump is still a waypoint or two ahead, then jump just before
        // the feet leave the takeoff block.
        let jump_ahead = path.points[*next..(*next + 3).min(path.points.len())]
            .iter()
            .any(|w| w.jump);
        if path.points[*next].jump && *next > 0 && ctx.player.on_ground {
            let takeoff = path.points[*next - 1].pos();
            let landing = path.points[*next].pos();
            let (dx, dz) = (landing.x - takeoff.x, landing.z - takeoff.z);
            let length = (dx * dx + dz * dz).sqrt();
            if length > 0.0 {
                let (ux, uz) = (dx / length, dz / length);
                let along = (pos.x - takeoff.x) * ux + (pos.z - takeoff.z) * uz;
                let across = ((pos.x - takeoff.x) * uz - (pos.z - takeoff.z) * ux).abs();
                if along > 0.15 && across < 0.45 && flat(landing) > 0.5 {
                    self.jump_hold = self.jump_hold.max(2);
                }
            }
        }

        let to_next = flat(path.points[*next].pos());
        if self.stalled(to_next) {
            self.replans += 1;
            self.door_wait = None;
            if self.replans > 2 {
                self.give_up(ctx);
            } else {
                self.plan = Plan::None;
            }
            return;
        }
        self.steer(ctx, aim_at, run.min(open + 3.0), false, jump_ahead);
    }

    /// Deals with the closed door at the next waypoint. Returns true while
    /// the walker should stand still (opening or waiting for the update),
    /// false when the door is already open and walking can continue.
    fn open_door(&mut self, ctx: &mut TickContext<'_>, planned: Door) -> bool {
        let Some(current) = door::at(ctx.world, planned.lower.0, planned.lower.1, planned.lower.2)
        else {
            self.plan = Plan::None;
            self.door_wait = None;
            return true;
        };
        if current.open {
            self.door_wait = None;
            return false;
        }
        if !current.can_open() {
            // Iron, or held closed by redstone: this route is not usable.
            self.plan = Plan::None;
            self.door_wait = None;
            return true;
        }

        // Stand still, and do not carry a jump over from the last stretch.
        self.jump_hold = 0;
        self.jump_in = None;
        ctx.set_keys(Keys::default());

        let wait = match &mut self.door_wait {
            Some(wait) if wait.door.lower == current.lower => wait,
            _ => {
                self.door_wait = Some(DoorWait {
                    door: current,
                    ticks: 0,
                    clicked: false,
                });
                self.door_wait.as_mut().expect("just set")
            }
        };
        wait.ticks = wait.ticks.saturating_add(1);
        if let Some(aim) = current.aim_point(ctx.world) {
            ctx.aim_at(aim, 2.0);
        }

        // The crosshair is the pick from before this tick's mouse movement.
        // Click only when it is actually on the door; wait for the human
        // mouse model to settle if it is not.
        let hit = ctx.crosshair;
        let on_door = !hit.miss && (hit.pos == current.lower || hit.pos == current.upper);
        if on_door && !wait.clicked {
            ctx.click_use();
            wait.clicked = true;
        }

        // No update after six seconds: think again, and eventually give up.
        if wait.ticks > 120 {
            self.door_wait = None;
            self.replans += 1;
            if self.replans > 2 {
                self.give_up(ctx);
            } else {
                self.plan = Plan::None;
            }
            return true;
        }
        true
    }

    /// Turns towards `target` and holds the keys to get there. `distance`
    /// is how much room there is to build up speed. `force_sprint` is for a
    /// gap jump, where speed matters even over a short run-up.
    fn steer(
        &mut self,
        ctx: &mut TickContext<'_>,
        target: Vec3,
        distance: f64,
        slow_near: bool,
        force_sprint: bool,
    ) {
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
        let mut forward = yaw_error < if near < 1.5 && !slow_near { 30.0 } else { 50.0 };

        // On a ladder or vine, jump climbs and no key slides down. Aim
        // straight up or down without walking off the rungs.
        let climbing = on_climbable(ctx);
        let climb_up = climbing && target.y > pos.y + 0.05;
        let climb_down = climbing && target.y < pos.y - 0.05;
        if climbing && (climb_up || climb_down) && near < 0.45 {
            forward = false;
        }
        let sprint =
            !climb_down && (force_sprint || (forward && yaw_error < 15.0 && distance > 5.0));

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
        // In water, space keeps the head up. Up a ladder it climbs.
        let jump = self.jump_hold > 0
            || (self.sprint_jumps && sprint && distance > 8.0)
            || ctx.player.in_water
            || climb_up;
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
    use rapidbot_world::{DimensionHeight, Registry, World};

    use super::*;
    use crate::MouseSettings;

    fn state(name: &str, properties: &str) -> u32 {
        let registry = Registry::get();
        let block = registry.block_by_name(name).unwrap();
        registry
            .states
            .iter()
            .position(|s| s.block == block.id && s.properties.contains(properties))
            .unwrap() as u32
    }

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
        use_clicks: u32,
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
                use_clicks: 0,
            }
        }

        fn set_door(&mut self, x: i32, y: i32, z: i32, name: &str, open: bool) {
            let properties = |half: &str| {
                format!("facing=north,half={half},hinge=left,open={open},powered=false")
            };
            self.world
                .set_block_state(x, y, z, state(name, &properties("lower")));
            self.world
                .set_block_state(x, y + 1, z, state(name, &properties("upper")));
        }

        fn step(&mut self) {
            self.actions = Default::default();
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

            // Stand in for the server's use handler: if the walker clicked
            // while the crosshair was on a door, open both halves. The real
            // server replies with block updates on a later tick.
            if self.actions.use_block {
                self.use_clicks += 1;
                if !crosshair.miss
                    && let Some(door) = door::at(
                        &self.world,
                        crosshair.pos.0,
                        crosshair.pos.1,
                        crosshair.pos.2,
                    )
                    && (crosshair.pos == door.lower || crosshair.pos == door.upper)
                {
                    let block = Registry::get()
                        .state(
                            self.world
                                .block_state(door.lower.0, door.lower.1, door.lower.2)
                                .unwrap(),
                        )
                        .block;
                    let name = Registry::get().blocks[block as usize].name.clone();
                    self.set_door(door.lower.0, door.lower.1, door.lower.2, &name, true);
                }
            }

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
    fn opens_a_wooden_door_and_walks_through() {
        let mut sim = Sim::new();
        for x in -32..48 {
            for y in 64..=65 {
                sim.world.set_block_state(x, y, 8, 1);
            }
        }
        sim.set_door(0, 64, 8, "minecraft:oak_door", false);
        sim.walker.go_to(Vec3::new(0.5, 64.0, 14.5));
        let mut ticks = 0;
        while !sim.walker.arrived() {
            sim.step();
            ticks += 1;
            assert!(ticks < 900, "never got through, at {:?}", sim.player.pos);
        }
        assert!(!sim.walker.gave_up());
        assert!(sim.use_clicks > 0, "never clicked the door");
        let door = door::at(&sim.world, 0, 64, 8).expect("door is still there");
        assert!(door.open, "door did not stay open");
        assert!(sim.player.pos.z > 8.5, "at {:?}", sim.player.pos);
    }

    #[test]
    fn an_open_door_is_walked_through_without_a_click() {
        let mut sim = Sim::new();
        for x in -32..48 {
            for y in 64..=65 {
                sim.world.set_block_state(x, y, 8, 1);
            }
        }
        sim.set_door(0, 64, 8, "minecraft:oak_door", true);
        sim.walker.go_to(Vec3::new(0.5, 64.0, 14.5));
        let mut ticks = 0;
        while !sim.walker.arrived() {
            sim.step();
            ticks += 1;
            assert!(ticks < 900, "never got through, at {:?}", sim.player.pos);
        }
        assert!(!sim.walker.gave_up());
        assert_eq!(sim.use_clicks, 0, "clicked an already open door");
        assert!(sim.player.pos.z > 8.5, "at {:?}", sim.player.pos);
    }

    #[test]
    fn gives_up_at_a_closed_iron_door() {
        let mut sim = Sim::new();
        for x in -32..48 {
            for y in 64..=65 {
                sim.world.set_block_state(x, y, 8, 1);
            }
        }
        sim.set_door(0, 64, 8, "minecraft:iron_door", false);
        sim.walker.go_to(Vec3::new(0.5, 64.0, 14.5));
        for _ in 0..900 {
            sim.step();
        }
        assert!(sim.walker.gave_up());
        assert_eq!(sim.use_clicks, 0, "tried to open an iron door");
        // The panel can be entered from the open middle of the block, but
        // it cannot be crossed.
        assert!(sim.player.pos.z < 8.8, "at {:?}", sim.player.pos);
    }

    #[test]
    fn swims_across_a_deep_channel() {
        let mut sim = Sim::new();
        let water = state("minecraft:water", "level=0");
        // A three-wide channel deeper than the player across the whole
        // route: too wide to jump, so it has to swim.
        for x in -32..48 {
            for z in 4..=6 {
                for y in 58..=63 {
                    sim.world.set_block_state(x, y, z, water);
                }
                sim.world.set_block_state(x, 57, z, 1);
            }
        }
        sim.walker.go_to(Vec3::new(0.5, 64.0, 10.5));
        let mut ticks = 0;
        let mut wet = false;
        while !sim.walker.arrived() {
            sim.step();
            wet |= sim.player.in_water;
            ticks += 1;
            assert!(ticks < 600, "never crossed, at {:?}", sim.player.pos);
        }
        assert!(!sim.walker.gave_up());
        assert!(wet, "the route went through the water");
        let p = sim.player.pos;
        assert!((p.x - 0.5).hypot(p.z - 10.5) < 0.6, "ended at {p:?}");
    }

    #[test]
    fn swims_out_after_dropping_into_water() {
        let mut sim = Sim::new();
        let water = state("minecraft:water", "level=0");
        for x in -2..=2 {
            for z in -2..=2 {
                sim.world.set_block_state(x, 63, z, water);
                sim.world.set_block_state(x, 62, z, water);
                sim.world.set_block_state(x, 61, z, 1);
            }
        }
        sim.player.set_pos(Vec3::new(0.5, 62.2, 0.5));
        sim.walker.go_to(Vec3::new(0.5, 64.0, 8.5));
        let mut ticks = 0;
        while !sim.walker.arrived() {
            sim.step();
            ticks += 1;
            assert!(ticks < 600, "never got out, at {:?}", sim.player.pos);
        }
        assert!(!sim.walker.gave_up());
        let p = sim.player.pos;
        assert!((p.x - 0.5).hypot(p.z - 8.5) < 0.6, "ended at {p:?}");
    }

    #[test]
    fn climbs_a_ladder_to_a_platform_and_back_down() {
        let mut sim = Sim::new();
        let ladder = state("minecraft:ladder", "waterlogged=false");
        let ladder_block = Registry::get()
            .block_by_name("minecraft:ladder")
            .unwrap()
            .id;
        sim.tags
            .insert("minecraft:block", "minecraft:climbable", ladder_block);
        // A ladder on the south side of an eight-block wall, with a walkable
        // top. The player has to climb it; jumping up is too high.
        for y in 64..=71 {
            sim.world.set_block_state(0, y, 1, ladder);
            for x in -1..=1 {
                sim.world.set_block_state(x, y, 2, 1);
            }
        }
        sim.walker.go_to_exact(Vec3::new(0.5, 72.0, 2.5));
        let mut ticks = 0;
        let mut climbed = false;
        while !sim.walker.arrived() {
            sim.step();
            ticks += 1;
            climbed |= sim.player.pos.y > 65.0;
            assert!(ticks < 600, "never climbed, at {:?}", sim.player.pos);
        }
        assert!(!sim.walker.gave_up());
        assert!(climbed, "never left the ground");
        assert_eq!(sim.player.pos.y, 72.0);
        assert!(sim.player.pos.z > 1.5, "at {:?}", sim.player.pos);

        sim.walker.go_to_exact(Vec3::new(0.5, 64.0, 0.5));
        let mut ticks = 0;
        while !sim.walker.arrived() {
            sim.step();
            ticks += 1;
            assert!(ticks < 600, "never got down, at {:?}", sim.player.pos);
        }
        // Arrival is horizontal; let the last drop settle.
        for _ in 0..40 {
            sim.step();
        }
        assert!(!sim.walker.gave_up());
        assert_eq!(sim.player.pos.y, 64.0);
        assert!(sim.player.pos.z < 1.0, "at {:?}", sim.player.pos);
    }

    #[test]
    fn jumps_over_a_short_gap() {
        let mut sim = Sim::new();
        // A two-block-wide trench right across the world.
        for x in -32..48 {
            for z in 4..=5 {
                sim.world.set_block_state(x, 63, z, 0);
            }
        }
        sim.walker.go_to(Vec3::new(0.5, 64.0, 10.5));
        let mut ticks = 0;
        let mut jumped = false;
        while !sim.walker.arrived() {
            sim.step();
            jumped |= !sim.player.on_ground && sim.player.pos.y > 64.5;
            ticks += 1;
            assert!(ticks < 600, "never crossed, at {:?}", sim.player.pos);
        }
        assert!(!sim.walker.gave_up());
        assert!(jumped, "walked into the gap instead of jumping");
        let p = sim.player.pos;
        assert!((p.x - 0.5).hypot(p.z - 10.5) < 0.6, "ended at {p:?}");
        assert!(p.z > 6.0, "did not get past the gap");
    }

    #[test]
    fn jumps_a_short_gap_from_near_the_edge() {
        let mut sim = Sim::new();
        for x in -32..48 {
            for z in 4..=5 {
                sim.world.set_block_state(x, 63, z, 0);
            }
        }
        // Starting almost at the edge leaves no room to build up speed.
        sim.player.set_pos(Vec3::new(0.5, 64.0, 3.9));
        sim.walker.go_to(Vec3::new(0.5, 64.0, 10.5));
        let mut ticks = 0;
        while !sim.walker.arrived() {
            sim.step();
            ticks += 1;
            assert!(ticks < 600, "never crossed, at {:?}", sim.player.pos);
        }
        assert!(!sim.walker.gave_up());
        assert!(sim.player.pos.z > 6.0, "at {:?}", sim.player.pos);
    }

    #[test]
    fn jumps_a_gap_onto_a_one_block_higher_ledge() {
        let mut sim = Sim::new();
        for x in -32..48 {
            for z in 4..=5 {
                sim.world.set_block_state(x, 63, z, 0);
            }
            for z in 6..=12 {
                sim.world.set_block_state(x, 64, z, 1);
            }
        }
        sim.walker.go_to_exact(Vec3::new(0.5, 65.0, 10.5));
        let mut ticks = 0;
        while !sim.walker.arrived() {
            sim.step();
            ticks += 1;
            assert!(ticks < 600, "never got up, at {:?}", sim.player.pos);
        }
        assert!(!sim.walker.gave_up());
        assert_eq!(sim.player.pos.y, 65.0);
        assert!(sim.player.pos.z > 6.0);
    }

    #[test]
    fn gives_up_at_a_gap_too_wide_to_jump() {
        let mut sim = Sim::new();
        for x in -32..48 {
            for z in 4..=7 {
                sim.world.set_block_state(x, 63, z, 0);
            }
        }
        sim.walker.go_to(Vec3::new(0.5, 64.0, 12.5));
        for _ in 0..600 {
            sim.step();
        }
        assert!(sim.walker.gave_up());
        assert!(sim.player.pos.z < 4.5, "at {:?}", sim.player.pos);
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
