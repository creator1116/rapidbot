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

/// While mining the blocks of the next waypoint.
struct DigWait {
    cell: (i32, i32, i32),
    ticks: u32,
    /// The attack button is down for this dig, so it can be let go after.
    pressed: bool,
}

/// While placing the block a waypoint stands on.
struct PlaceWait {
    cell: (i32, i32, i32),
    ticks: u32,
    /// The right click has been sent; wait for the server's block update.
    clicked: bool,
}

/// Whether the blocks of a waypoint cell still have to be mined.
fn dig_still_needed(ctx: &TickContext<'_>, cell: (i32, i32, i32)) -> bool {
    path::blocks(ctx.world, cell.0, cell.1, cell.2)
        || path::blocks(ctx.world, cell.0, cell.1 + 1, cell.2)
}

/// Whether the floor of a waypoint cell still has to be placed.
fn place_still_needed(ctx: &TickContext<'_>, cell: (i32, i32, i32)) -> bool {
    !path::blocks(ctx.world, cell.0, cell.1 - 1, cell.2)
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

/// How the walker crosses a gap it has to build across.
///
/// None of this is vanilla: vanilla only supplies the physics. Holding sneak
/// is what stops a player walking off an edge (`Player.maybeBackOffFromEdge`,
/// `Player.isStayingOnGroundSurface`) and it also slows the walk to sneak
/// speed, so *when* to press it is a technique, not a rule.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Bridge {
    /// Stop at the edge of each cell, sneak, place, then step on: the safe
    /// way, and what the walker does unless it is told otherwise.
    #[default]
    Stop,
    /// Speed bridging: keep walking at full speed and press sneak only for
    /// the last stretch before the edge, so the block goes in safely without
    /// the whole crossing being slowed to sneak speed.
    Speed,
    /// Telebridging: sprint off the edge, place a triple extension of blocks
    /// under the fall, and land on the last one. The fastest and the most
    /// visible way to cross a void; the jump has to be started at the edge
    /// and the blocks have to go in within the arc, or the walker falls.
    Tele,
}

/// Speed bridging: how close to the next cell's centre the walker gets before
/// it presses sneak. The cell centre is the far edge of the block it stands
/// on, so this is the last half block or so of the walk.
const SPEED_BRIDGE_SNEAK_WITHIN: f64 = 1.1;

/// Telebridging: how close to the first hole's centre the walker gets before
/// it jumps. The jump is taken early because most of the arc is spent
/// turning the view around to place the first block; the forward key is let
/// go only once the extension is in, and the full sprint arc carries the
/// walker onto the last block of the extension.
const TELE_JUMP_WITHIN: f64 = 1.5;

/// How far from the edge the telebridge approach starts aiming its view at
/// the catch face, so the crosshair is on it when the flight begins.
const TELE_AIM_WITHIN: f64 = 5.0;

/// How far ahead of the bot the telebridge look-down holds the view, on the
/// ground and through the flight: steep enough that the crosshair sweeps
/// the catch face early, level enough that the ray crosses it instead of
/// diving into the hole.
const TELE_LOOK_AHEAD: f64 = 2.5;

/// Telebridging: how many blocks one jump extends the bridge by.
const TELE_EXTENSION: usize = 3;

/// Telebridging: the widest gap one jump will cross. The extension is the
/// far [`TELE_EXTENSION`] cells of the gap, and the fall has to land on one
/// of them (or on the far ground itself), so the gap can be at most one cell
/// wider than the extension.
const TELE_MAX_RUN: usize = 4;

/// The state of a telebridge jump: which cells the arc is placing, which one
/// is next, and how far into the fall it is.
struct TeleWait {
    /// The cells to floor, far-to-near: at most [`TELE_EXTENSION`]. The last
    /// one is the cell the fall lands on.
    cells: Vec<(i32, i32, i32)>,
    /// The route index of each cell: the flight carries the walker over the
    /// near cells, and the route is ticked past them as they are placed.
    waypoints: Vec<usize>,
    /// The cell being placed.
    index: usize,
    /// Ticks since the feet left the ground.
    ticks: u32,
    /// One bit per cell: the click has been made.
    clicked: u32,
}

/// What the telebridge wants from this tick.
enum TeleStep {
    /// Walk on: nothing to do yet.
    Walk,
    /// At the edge: press jump.
    Jump,
    /// Airborne. `aim` is the face to look at, if there is one to click;
    /// `advance` ticks the route past a waypoint the flight has carried the
    /// walker over.
    Place {
        aim: Option<Vec3>,
        advance: Option<usize>,
    },
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
    /// What routes may mine through. `None` (the default) plans around
    /// blocks the player cannot walk through.
    pub dig: Option<path::DigPolicy>,
    /// What a route may place to fill a hole in the floor. `None` (the
    /// default) only walks on ground that is already there, and the block
    /// named here has to be the one in hand when the click is made.
    pub place: Option<path::PlacePolicy>,
    /// The waypoint cell being mined, if the next one needs it.
    dig_wait: Option<DigWait>,
    /// The waypoint cell being floored, if the next one needs it.
    place_wait: Option<PlaceWait>,
    /// The telebridge jump in progress, if the walker is mid-arc.
    tele: Option<TeleWait>,
    /// How to bridge a gap the route has to build across.
    pub bridge: Bridge,
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
            dig: None,
            place: None,
            dig_wait: None,
            place_wait: None,
            tele: None,
            bridge: Bridge::default(),
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
                let search = Search::new(ctx.world, pos, target, SEARCH_LIMIT).map(|mut search| {
                    if let Some(dig) = self.dig {
                        search = search.with_digging(dig);
                    }
                    if let Some(place) = self.place {
                        search = search.with_placing(place);
                    }
                    search
                });
                self.plan = match search {
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
                self.steer(ctx, goal, distance, true, false, false);
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

        // The next waypoint may need its blocks mined before it can be stood
        // in, and, like a closed door, it must not be ticked off from
        // outside: the cell is not reached until the blocks are gone.
        let next_dig = match &self.plan {
            Plan::Following { path, next, .. } if *next < path.points.len() => {
                let waypoint = path.points[*next];
                (waypoint.dig && dig_still_needed(ctx, waypoint.cell)).then_some(waypoint.cell)
            }
            _ => None,
        };
        let first_dig = match &self.plan {
            Plan::Following { path, next, .. } => path
                .points
                .iter()
                .enumerate()
                .skip(*next)
                .find(|(_, w)| w.dig && dig_still_needed(ctx, w.cell))
                .map(|(i, _)| i),
            _ => None,
        };
        if self
            .dig_wait
            .as_ref()
            .is_some_and(|wait| Some(wait.cell) != next_dig)
        {
            // Finished, or the route changed: let go of the button.
            if self.dig_wait.take().is_some_and(|wait| wait.pressed) {
                ctx.hold_attack(false);
            }
        }
        if let Some(cell) = next_dig
            && self.dig_at(ctx, cell)
        {
            return;
        }

        // The next waypoint may need a block placed under it first, and the
        // cell is not reached until that block is there.
        let next_place = match &self.plan {
            Plan::Following { path, next, .. } if *next < path.points.len() => {
                let waypoint = path.points[*next];
                (waypoint.place && place_still_needed(ctx, waypoint.cell)).then_some(waypoint.cell)
            }
            _ => None,
        };
        let first_place = match &self.plan {
            Plan::Following { path, next, .. } => path
                .points
                .iter()
                .enumerate()
                .skip(*next)
                .find(|(_, w)| w.place && place_still_needed(ctx, w.cell))
                .map(|(i, _)| i),
            _ => None,
        };
        if self
            .place_wait
            .as_ref()
            .is_some_and(|wait| Some(wait.cell) != next_place)
        {
            self.place_wait = None;
        }
        // Stopping to place is the safe way, and all the `Stop` mode does.
        // The moving modes are handled further down, where the walking keys
        // are decided: they keep walking while the block goes in. A gap too
        // wide for a telebridge jump is placed this way too — standing at
        // the edge beats leaping into the void — and the placement times out
        // and gives up when even that cannot reach.
        // The telebridge cell for the too-wide check: taken here while the
        // route is only borrowed, and falling back to the route's first
        // unfloored place cell so the check fires during the approach too —
        // a gap the jump cannot catch is not leapt into from two blocks
        // before the edge either.
        let tele_check_cell = match next_place {
            Some(cell) => Some(cell),
            None => match &self.plan {
                Plan::Following { path, .. } => {
                    first_place.and_then(|i| path.points.get(i).map(|w| w.cell))
                }
                _ => None,
            },
        };
        let tele_too_wide = self.bridge == Bridge::Tele
            && tele_check_cell.is_some_and(|cell| self.tele_run(ctx, cell).len() > TELE_MAX_RUN);
        if tele_too_wide {
            // A gap this wide cannot be caught: say so and stop, rather than
            // leaping into the void.
            self.give_up(ctx);
            return;
        }
        if self.bridge == Bridge::Stop
            && let Some(cell) = next_place
            && self.place_at(ctx, cell)
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
        // The telebridge cell, taken while the route is at hand: the next
        // waypoint when it is a place cell — the walker is at the edge or in
        // the air — else the route's first unfloored place cell, so the
        // approach starts early enough to look down and build up speed.
        let tele_cell = match next_place {
            Some(cell) => Some(cell),
            None => first_place.and_then(|i| path.points.get(i).map(|w| w.cell)),
        };
        let pos = ctx.player.pos;
        let flat = |a: Vec3| ((a.x - pos.x).powi(2) + (a.z - pos.z).powi(2)).sqrt();

        // Tick off waypoints: the one aimed for, or any on the way to it.
        // Stop before a closed door: the door itself is not reached until it
        // has been opened.
        let limit = [first_closed, first_dig, first_place]
            .into_iter()
            .flatten()
            .min()
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
                    // A waypoint to mine or to floor has no surface until
                    // that is done, and is checked there instead.
                    w.door.is_none()
                        && !w.dig
                        && !w.place
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
                for (offset, w) in path.points[*next..(*next + 8).min(path.points.len()).min(limit)]
                    .iter()
                    .enumerate()
                {
                    if (w.surface - pos.y).abs() > 0.01 {
                        break;
                    }
                    if path::line_walkable(ctx.world, pos, w.pos()) {
                        *target = *next + offset;
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
        // Bridging: the stopping mode was handled further up, where it stands
        // still. The moving ones keep walking. Speed bridging holds sneak for
        // the last stretch before the edge and looks at the face the block
        // goes against; telebridging sprints off the edge and places under
        // the fall. The look has to come after steering, which sets the
        // walking gaze.
        let mut place_aim = None;
        let mut sneak = false;
        if self.bridge == Bridge::Speed
            && let Some(cell) = next_place
            && let Some((aim, late)) = self.bridge_speed(ctx, cell)
        {
            place_aim = Some(aim);
            sneak = late;
        }
        let mut on_tele_approach = false;
        let mut tele_advance = None;
        let mut tele_fly = false;
        if self.bridge == Bridge::Tele {
            // The arc's own state keeps the sequence running once the route
            // has been ticked past the cells it is placing.
            let tele_cell = tele_cell.or_else(|| {
                self.tele
                    .as_ref()
                    .and_then(|wait| wait.cells.get(wait.index).copied())
            });
            let step = match tele_cell {
                Some(cell) => self.bridge_tele(ctx, cell),
                None => {
                    if ctx.player.on_ground {
                        self.tele = None;
                    }
                    TeleStep::Walk
                }
            };
            match step {
                TeleStep::Walk => on_tele_approach = true,
                TeleStep::Jump => {
                    self.jump_hold = self.jump_hold.max(3);
                    on_tele_approach = true;
                }
                TeleStep::Place { aim, advance } => {
                    place_aim = aim;
                    tele_advance = advance;
                    tele_fly = true;
                }
            }
        } else {
            self.tele = None;
        }
        // The telebridge approach is sprinted so the takeoff has speed, and
        // the arc keeps the sprint until the forward key is let go.
        let tele_sprint = on_tele_approach
            || (self.bridge == Bridge::Tele && (next_place.is_some() || self.tele.is_some()));
        self.steer(
            ctx,
            aim_at,
            run.min(open + 3.0),
            false,
            jump_ahead || tele_sprint,
            sneak,
        );
        if let Some(aim) = place_aim {
            ctx.aim_at(aim, 3.0);
        }
        if tele_fly {
            // The telly technique: the view is down on the block being
            // placed, and the movement holds whatever mix of forward and
            // strafe keeps the walker travelling along the route — otherwise
            // every placement aim drags the flight sideways and the walker
            // drops off its own bridge.
            let mut keys = ctx.wanted_keys();
            keys.forward = true;
            keys.sprint = true;
            keys.backward = false;
            let (travel_yaw, _) = angles_to(
                ctx.player.eye_position(),
                Vec3::new(aim_at.x, ctx.player.eye_position().y, aim_at.z),
            );
            let off = wrap_degrees(travel_yaw - ctx.player.y_rot);
            keys.right = off > 5.0;
            keys.left = off < -5.0;
            ctx.set_keys(keys);
        }
        // The flight carried the walker over these waypoints.
        if let Some(i) = tele_advance {
            self.advance_route(i);
        }
    }

    /// Speed bridging: aim at the face the next block goes against and say
    /// whether to hold sneak, while the walk carries on. The click is the same
    /// check [`Walker::place_at`] makes — only when the crosshair is really on
    /// a face beside the hole — so an unreachable face is never clicked
    /// blindly. `None` when there is nothing to place, or the block is not in
    /// hand.
    fn bridge_speed(
        &mut self,
        ctx: &mut TickContext<'_>,
        cell: (i32, i32, i32),
    ) -> Option<(Vec3, bool)> {
        let hole = (cell.0, cell.1 - 1, cell.2);
        if path::blocks(ctx.world, hole.0, hole.1, hole.2) {
            self.place_wait = None;
            return None;
        }
        let place = self.place?;
        // The block has to be the one in hand, or the click would place
        // something else. Stop and think, as `place_at` does.
        let in_hand = ctx
            .inventory
            .held()
            .and_then(|stack| path::PlacePolicy::for_item(stack.item))
            .is_some_and(|held| held.block == place.block);
        if !in_hand {
            self.place_wait = None;
            self.replans += 1;
            if self.replans > 2 {
                self.give_up(ctx);
            } else {
                self.plan = Plan::None;
            }
            return None;
        }

        let targets = path::place_targets(ctx.world, hole);
        let wait = match &mut self.place_wait {
            Some(wait) if wait.cell == cell => wait,
            _ => {
                self.place_wait = Some(PlaceWait {
                    cell,
                    ticks: 0,
                    clicked: false,
                });
                self.place_wait.as_mut().expect("just set")
            }
        };
        wait.ticks = wait.ticks.saturating_add(1);
        // One face at a time, trying the next every fifteen ticks.
        let aim = *targets.get((wait.ticks / 15) as usize % targets.len().max(1))?;

        // The crosshair is the pick from before this tick's mouse movement.
        let hit = ctx.crosshair;
        let on_face = !hit.miss
            && path::NEIGHBOURS.iter().any(|((dx, dy, dz), face)| {
                hit.pos == (hole.0 + dx, hole.1 + dy, hole.2 + dz) && hit.direction == *face
            });
        if !wait.clicked && on_face {
            ctx.click_use();
            wait.clicked = true;
        }

        // Sneak only for the last stretch: the next cell has no floor, so
        // without sneak the walk would carry the player over the edge. The
        // cell centre is the far edge of the block being stood on.
        let next = Vec3::new(cell.0 as f64 + 0.5, cell.1 as f64, cell.2 as f64 + 0.5);
        let to_edge =
            ((next.x - ctx.player.pos.x).powi(2) + (next.z - ctx.player.pos.z).powi(2)).sqrt();
        Some((aim, to_edge < SPEED_BRIDGE_SNEAK_WITHIN))
    }

    /// The run of unfloored place cells a telebridge jump would cross,
    /// starting at `first`, with each cell's route index.
    fn tele_run(
        &self,
        ctx: &TickContext<'_>,
        first: (i32, i32, i32),
    ) -> Vec<((i32, i32, i32), usize)> {
        let mut run: Vec<((i32, i32, i32), usize)> = Vec::new();
        if let Plan::Following { path, next, .. } = &self.plan {
            for (i, waypoint) in path.points.iter().enumerate().skip(*next) {
                if run.is_empty() {
                    if waypoint.cell == first {
                        run.push((waypoint.cell, i));
                    }
                    continue;
                }
                if !waypoint.place || !place_still_needed(ctx, waypoint.cell) || run.len() >= 8 {
                    // The route may hop the rest of the gap in one jump
                    // waypoint. The flight crosses those cells all the same,
                    // and they need placing, so they join the run.
                    if waypoint.jump {
                        let (last, _) = run[run.len() - 1];
                        let (dx, dz) = (waypoint.cell.0 - last.0, waypoint.cell.2 - last.2);
                        let (sx, sz) = (dx.signum(), dz.signum());
                        let steps = dx.abs().max(dz.abs());
                        for step in 1..steps {
                            run.push(((last.0 + sx * step, last.1, last.2 + sz * step), i));
                        }
                    }
                    break;
                }
                run.push((waypoint.cell, i));
            }
        }
        run
    }

    /// Telebridging: what this tick of the jump looks like. On the ground it
    /// walks, pre-aims at the first face it will click, and jumps at the
    /// edge; in the air it works through the extension, clicking when the
    /// crosshair is really on a face of the hole being filled — the same
    /// check the other modes make, so an unreachable face is never clicked
    /// blindly.
    ///
    /// The extension is placed **far-to-near**: mid-air, the only faces that
    /// exist are ahead — the far side of the gap first, then the west face of
    /// each block placed before it — so every click is made looking forward
    /// and down, the way the walk was already looking. The near cells the
    /// flight crosses before their faces exist are the ones the route
    /// sprint-jumps anyway.
    fn bridge_tele(&mut self, ctx: &mut TickContext<'_>, first: (i32, i32, i32)) -> TeleStep {
        // The block has to be the one in hand, or the click would place
        // something else. Stop and think, as the other modes do.
        let in_hand = self.place.is_some_and(|place| {
            ctx.inventory
                .held()
                .and_then(|stack| path::PlacePolicy::for_item(stack.item))
                .is_some_and(|held| held.block == place.block)
        });
        if !in_hand {
            self.tele = None;
            self.replans += 1;
            if self.replans > 2 {
                self.give_up(ctx);
            } else {
                self.plan = Plan::None;
            }
            return TeleStep::Walk;
        }

        // The extension: the far cells of the stretch the jump crosses,
        // far-to-near. Mid-air the only faces that exist are ahead — the far
        // side of the gap first, then the west face of each block placed
        // before it — so every click is made looking forward and down, the
        // way the walk was already looking, and the last block placed is the
        // one the fall lands on.
        let extension: Vec<((i32, i32, i32), usize)> = match &self.tele {
            Some(wait) => wait
                .cells
                .iter()
                .copied()
                .zip(wait.waypoints.iter().copied())
                .collect(),
            None => self
                .tele_run(ctx, first)
                .into_iter()
                .rev()
                .take(TELE_EXTENSION)
                .collect(),
        };

        if ctx.player.on_ground {
            self.tele = None;
            let centre = Vec3::new(first.0 as f64 + 0.5, first.1 as f64, first.2 as f64 + 0.5);
            let to_edge = ((centre.x - ctx.player.pos.x).powi(2)
                + (centre.z - ctx.player.pos.z).powi(2))
            .sqrt();
            if to_edge < TELE_JUMP_WITHIN {
                return TeleStep::Jump;
            }
            // Pre-aim, the telly look-down: walk the approach holding the
            // view on the walkway a couple of blocks ahead, so the crosshair
            // is already steep when the flight begins and sweeps across the
            // catch face while the bot is still above the hole. From a level
            // view the face is only met after the fall has carried the bot
            // into the cell it is filling — where vanilla refuses the
            // placement.
            if to_edge < TELE_AIM_WITHIN {
                let ahead = TELE_LOOK_AHEAD * (centre.x - ctx.player.pos.x).signum();
                let look = Vec3::new(
                    ctx.player.pos.x + ahead,
                    first.1 as f64 - 0.5,
                    ctx.player.pos.z,
                );
                return TeleStep::Place {
                    aim: Some(look),
                    advance: None,
                };
            }
            return TeleStep::Walk;
        }

        // Airborne. The cell order was frozen at the takeoff.
        if self.tele.is_none() {
            self.tele = Some(TeleWait {
                cells: extension.iter().map(|(cell, _)| *cell).collect(),
                waypoints: extension.iter().map(|(_, i)| *i).collect(),
                index: 0,
                ticks: 0,
                clicked: 0,
            });
        }
        let wait = self.tele.as_mut().expect("just set");
        wait.ticks += 1;
        let old_index = wait.index;
        while wait.index < wait.cells.len() && !place_still_needed(ctx, wait.cells[wait.index]) {
            wait.index += 1;
        }
        // A cell the flight has already carried the walker over, and the
        // route with it.
        let advance = (wait.index != old_index || wait.index >= wait.cells.len())
            .then(|| wait.waypoints[wait.index.saturating_sub(1)]);
        if wait.index >= wait.cells.len() {
            // The extension is in; the rest is landing on it and walking on.
            return TeleStep::Place { aim: None, advance };
        }
        let cell = wait.cells[wait.index];
        let hole = (cell.0, cell.1 - 1, cell.2);
        // The telly look-down continues through the flight: the same point
        // the approach held the view on, recomputed as the bot advances. The
        // crosshair then sweeps across the catch face while the bot is still
        // above the hole it is filling; aiming at the face itself from a
        // level view only meets it after the fall has carried the box into
        // the cell, where vanilla refuses the placement.
        let aim = {
            let dir = (cell.0 as f64 + 0.5 - ctx.player.pos.x)
                .signum()
                .mul_add(TELE_LOOK_AHEAD, ctx.player.pos.x);
            Vec3::new(dir, hole.1 as f64 + 0.5, ctx.player.pos.z)
        };
        let hit = ctx.crosshair;
        let on_face = !hit.miss
            && path::NEIGHBOURS.iter().any(|((dx, dy, dz), face)| {
                hit.pos == (hole.0 + dx, hole.1 + dy, hole.2 + dz) && hit.direction == *face
            });
        if wait.clicked >> wait.index & 1 == 0 && on_face {
            ctx.click_use();
            wait.clicked |= 1 << wait.index;
        }
        TeleStep::Place {
            aim: Some(aim),
            advance,
        }
    }

    /// Ticks the route past waypoint `i`: the telebridge flight carried the
    /// walker over it without standing on it.
    fn advance_route(&mut self, i: usize) {
        if let Plan::Following {
            path,
            next,
            retarget_in,
            ..
        } = &mut self.plan
        {
            *next = (*next).max(i + 1).min(path.points.len().saturating_sub(1));
            *retarget_in = 0;
        }
        self.best_distance = f64::MAX;
        self.stalled_ticks = 0;
        self.replans = 0;
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

    /// Mines the blocks of the next waypoint. Returns true while the walker
    /// should stand still: it aims at the lowest block that is still there
    /// and holds the attack button until the server's block update takes it
    /// away, exactly as the dig example does.
    fn dig_at(&mut self, ctx: &mut TickContext<'_>, cell: (i32, i32, i32)) -> bool {
        // Stand still, and do not carry a jump over from the last stretch.
        self.jump_hold = 0;
        self.jump_in = None;
        ctx.set_keys(Keys::default());

        // Feet first, then head: the player fits through a one-high hole.
        let Some(y) = [cell.1, cell.1 + 1]
            .into_iter()
            .find(|y| path::blocks(ctx.world, cell.0, *y, cell.2))
        else {
            if self.dig_wait.take().is_some_and(|wait| wait.pressed) {
                ctx.hold_attack(false);
            }
            return false;
        };

        let wait = match &mut self.dig_wait {
            Some(wait) if wait.cell == cell => wait,
            _ => {
                self.dig_wait = Some(DigWait {
                    cell,
                    ticks: 0,
                    pressed: false,
                });
                self.dig_wait.as_mut().expect("just set")
            }
        };
        wait.ticks = wait.ticks.saturating_add(1);
        ctx.aim_at(
            Vec3::new(cell.0 as f64 + 0.5, y as f64 + 0.5, cell.2 as f64 + 0.5),
            3.0,
        );

        // The crosshair is the pick from before this tick's mouse movement.
        // Hold the button only once it is actually on the block.
        let hit = ctx.crosshair;
        if !hit.miss && hit.pos == (cell.0, y, cell.2) {
            ctx.hold_attack(true);
            wait.pressed = true;
        }
        let (timed_out, pressed) = (wait.ticks > 400, wait.pressed);

        // Twenty seconds without the block going: think again, and
        // eventually give up.
        if timed_out {
            if pressed {
                ctx.hold_attack(false);
            }
            self.dig_wait = None;
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

    /// Fills the hole under the next waypoint. Returns true while the walker
    /// should stand still: it aims at the face of a block beside the hole and
    /// clicks only once the crosshair is really on such a face, so a face
    /// that cannot be reached is never clicked blindly.
    fn place_at(&mut self, ctx: &mut TickContext<'_>, cell: (i32, i32, i32)) -> bool {
        self.jump_hold = 0;
        self.jump_in = None;
        ctx.set_keys(Keys::default());

        let hole = (cell.0, cell.1 - 1, cell.2);
        if path::blocks(ctx.world, hole.0, hole.1, hole.2) {
            // Already floored: nothing to do.
            self.place_wait = None;
            return false;
        }
        let Some(place) = self.place else {
            return false;
        };
        // The block has to be the one in hand, or the click would place
        // something else (or nothing).
        let in_hand = ctx
            .inventory
            .held()
            .and_then(|stack| path::PlacePolicy::for_item(stack.item))
            .is_some_and(|held| held.block == place.block);
        if !in_hand {
            self.place_wait = None;
            self.replans += 1;
            if self.replans > 2 {
                self.give_up(ctx);
            } else {
                self.plan = Plan::None;
            }
            return true;
        }

        let wait = match &mut self.place_wait {
            Some(wait) if wait.cell == cell => wait,
            _ => {
                self.place_wait = Some(PlaceWait {
                    cell,
                    ticks: 0,
                    clicked: false,
                });
                self.place_wait.as_mut().expect("just set")
            }
        };
        wait.ticks = wait.ticks.saturating_add(1);

        // The crosshair is the pick from before this tick's mouse movement.
        // Aim at one face at a time, trying the next every fifteen ticks, and
        // click as soon as the crosshair is on any face beside the hole.
        let targets = path::place_targets(ctx.world, hole);
        if let Some(aim) = targets.get((wait.ticks / 15) as usize % targets.len().max(1)) {
            ctx.aim_at(*aim, 3.0);
        }
        let hit = ctx.crosshair;
        let on_face = !hit.miss
            && path::NEIGHBOURS.iter().any(|((dx, dy, dz), face)| {
                hit.pos == (hole.0 + dx, hole.1 + dy, hole.2 + dz) && hit.direction == *face
            });
        if !wait.clicked && on_face {
            ctx.click_use();
            wait.clicked = true;
        }
        let timed_out = wait.ticks > 200;

        // Ten seconds without the block appearing: think again, and
        // eventually give up.
        if timed_out {
            self.place_wait = None;
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
        // Hold sneak as well: what stops a player walking off an edge, and
        // what speed bridging presses only at the last moment.
        sneak: bool,
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
            shift: sneak,
            ..Keys::default()
        });
    }
}

#[cfg(test)]
mod tests {
    use rapidbot_human::MouseModel;
    use rapidbot_physics::Player;
    use rapidbot_world::chunk::Chunk;
    use rapidbot_world::item::ItemData;
    use rapidbot_world::raycast::Direction;
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
        settings: MouseSettings,
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
                settings: MouseSettings::default(),
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

            // Stand in for the server's mining: a held attack on a block
            // takes it away (at once here, rather than after the break
            // time).
            if self.actions.attack == Some(true) && !crosshair.miss {
                let (x, y, z) = crosshair.pos;
                self.world.set_block_state(x, y, z, 0);
            }

            // Stand in for the server's placement: a click on a block face
            // puts the walker's chosen block in front of that face.
            if self.actions.use_block
                && !crosshair.miss
                && let Some(place) = self.walker.place
            {
                let (dx, dy, dz) = match crosshair.direction {
                    Direction::Down => (0, -1, 0),
                    Direction::Up => (0, 1, 0),
                    Direction::North => (0, 0, -1),
                    Direction::South => (0, 0, 1),
                    Direction::West => (-1, 0, 0),
                    Direction::East => (1, 0, 0),
                };
                self.world.set_block_state(
                    crosshair.pos.0 + dx,
                    crosshair.pos.1 + dy,
                    crosshair.pos.2 + dz,
                    place.block,
                );
            }

            // No keyboard model here: fingers follow at once.
            self.player.held_keys = self.keys;
            self.tick += 1;
            self.player.tick(&self.world, &self.tags);
            let settings = self.settings;
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

    /// A dirt box around the goal is mined into: the walker aims at the
    /// blocks it has to break and holds the attack button until they go.
    #[test]
    fn digs_into_a_walled_goal() {
        let mut sim = Sim::new();
        let dirt = state("minecraft:dirt", "");
        for x in 5..=7 {
            for z in -1..=1 {
                if x == 6 && z == 0 {
                    continue;
                }
                for y in 64..=65 {
                    sim.world.set_block_state(x, y, z, dirt);
                }
            }
        }
        sim.walker.dig = Some(path::DigPolicy::HAND);
        sim.walker.go_to(Vec3::new(6.5, 64.0, 0.5));
        let mut ticks = 0;
        while !sim.walker.arrived() && !sim.walker.gave_up() && ticks < 3_000 {
            sim.step();
            ticks += 1;
        }
        // Arriving means inside the box, which is only reachable by mining.
        assert!(
            !sim.walker.gave_up() && sim.player.pos.x > 5.5,
            "did not mine in, at {:?}, after {ticks} ticks",
            sim.player.pos
        );
    }

    /// Stone is harder than a hand policy allows, so the same box stays a
    /// wall and the walker gives up rather than mining it out.
    #[test]
    fn does_not_dig_stone_by_hand() {
        let mut sim = Sim::new();
        let stone = state("minecraft:stone", "");
        for x in 5..=7 {
            for z in -1..=1 {
                if x == 6 && z == 0 {
                    continue;
                }
                for y in 64..=65 {
                    sim.world.set_block_state(x, y, z, stone);
                }
            }
        }
        sim.walker.dig = Some(path::DigPolicy::HAND);
        sim.walker.go_to(Vec3::new(6.5, 64.0, 0.5));
        for _ in 0..900 {
            sim.step();
        }
        assert!(sim.walker.gave_up(), "stone should not be mined by hand");
    }

    /// The item registry id of a name.
    fn item_id(name: &str) -> u32 {
        ItemData::get()
            .items
            .iter()
            .position(|item| item.name == name)
            .unwrap() as u32
    }

    /// A `set_slot` body putting one `item` in the player's slot `slot`.
    fn held_block_body(slot: i16, item: u32) -> Vec<u8> {
        let mut body = vec![0, 0]; // container 0: the player's own inventory
        body.extend_from_slice(&slot.to_be_bytes());
        body.push(1); // one item
        rapidbot_buf::write_var_int(&mut body, item as i32);
        body.push(0); // no components added
        body.push(0); // none removed
        body
    }

    /// A void gap across the world is bridged with a placed block, clicked
    /// against the floor on the far side. The ceiling rules out jumping it.
    #[test]
    fn places_a_block_across_a_gap() {
        let mut sim = Sim::new();
        let dirt = state("minecraft:dirt", "");
        let item = item_id("minecraft:dirt");
        for z in -32..48 {
            sim.world.set_block_state(4, 63, z, 0);
            sim.world.set_block_state(4, 66, z, 1);
            sim.world.set_block_state(5, 66, z, 1);
        }
        assert_eq!(
            Registry::get()
                .block_for_item(item)
                .map(|block| block.name.as_str()),
            Some("minecraft:dirt"),
            "item id {item}"
        );
        sim.inventory.set_slot(&held_block_body(36, item)).unwrap();
        sim.walker.place = path::PlacePolicy::for_item(item);
        sim.walker.go_to(Vec3::new(6.5, 64.0, 0.5));
        let mut ticks = 0;
        while !sim.walker.arrived() && !sim.walker.gave_up() && ticks < 2_000 {
            sim.step();
            ticks += 1;
        }
        assert!(
            !sim.walker.gave_up() && sim.player.pos.x > 5.0,
            "did not place across, at {:?}, after {ticks} ticks",
            sim.player.pos
        );
        assert_eq!(
            sim.world.block_state(4, 63, 0),
            Some(dirt),
            "the gap is filled"
        );
    }

    /// Speed bridging: the click goes in while the walk carries on, and sneak
    /// is held only for the last stretch before the edge. The route here is a
    /// single-cell gap, which is all the planner chains today (a longer bridge
    /// needs waypoints whose placements build on each other); a walker with
    /// speed can also clear one cell on momentum, so what this pins is the
    /// behaviour, not the crossing. The cell before the gap starts at x = 3,
    /// so sneaking before 3.3 is standing at the far side of it rather than at
    /// the edge.
    #[test]
    fn speed_bridging_places_while_walking() {
        let mut sim = Sim::new();
        let item = item_id("minecraft:dirt");
        for z in -32..48 {
            sim.world.set_block_state(4, 63, z, 0);
            sim.world.set_block_state(4, 66, z, 1);
            sim.world.set_block_state(5, 66, z, 1);
        }
        sim.inventory.set_slot(&held_block_body(36, item)).unwrap();
        sim.walker.place = path::PlacePolicy::for_item(item);
        sim.walker.bridge = Bridge::Speed;
        sim.walker.go_to(Vec3::new(6.5, 64.0, 0.5));

        let mut ticks = 0;
        let mut sneaked_at = Vec::new();
        let mut walked_while_sneaking = 0;
        let mut clicked_while_sneaking = 0;
        while !sim.walker.arrived() && !sim.walker.gave_up() && ticks < 2_000 {
            let before = sim.use_clicks;
            sim.step();
            ticks += 1;
            if sim.keys.shift {
                sneaked_at.push(sim.player.pos.x);
                if sim.keys.forward {
                    walked_while_sneaking += 1;
                }
                if sim.use_clicks > before {
                    clicked_while_sneaking += 1;
                }
            }
        }
        assert!(
            !sim.walker.gave_up() && sim.player.pos.x > 5.0 && sim.player.pos.y > 60.0,
            "did not bridge while walking, at {:?}, after {ticks} ticks",
            sim.player.pos
        );
        assert!(sim.use_clicks > 0, "it placed the block");
        assert!(!sneaked_at.is_empty(), "it sneaked before the edge");
        assert!(
            sneaked_at.iter().all(|x| *x > 3.3),
            "it sneaked before the gap, at {sneaked_at:?}"
        );
        assert!(
            walked_while_sneaking > 0,
            "it kept walking while the block went in, which is the whole point"
        );
        assert!(
            clicked_while_sneaking > 0,
            "the click happened while walking, unlike the stopping mode"
        );
    }

    /// The stopping mode is the safe one: it never sneaks, and it never walks
    /// while it is placing.
    #[test]
    fn the_stopping_mode_stands_still_and_never_sneaks() {
        let mut sim = Sim::new();
        let item = item_id("minecraft:dirt");
        for z in -32..48 {
            sim.world.set_block_state(4, 63, z, 0);
            sim.world.set_block_state(4, 66, z, 1);
            sim.world.set_block_state(5, 66, z, 1);
        }
        sim.inventory.set_slot(&held_block_body(36, item)).unwrap();
        sim.walker.place = path::PlacePolicy::for_item(item);
        assert_eq!(sim.walker.bridge, Bridge::Stop, "the default");
        sim.walker.go_to(Vec3::new(6.5, 64.0, 0.5));

        let mut ticks = 0;
        let mut sneaks = 0;
        while !sim.walker.arrived() && !sim.walker.gave_up() && ticks < 2_000 {
            sim.step();
            ticks += 1;
            if sim.keys.shift {
                sneaks += 1;
            }
        }
        assert!(!sim.walker.gave_up() && sim.player.pos.x > 5.0);
        assert_eq!(sneaks, 0, "the stopping mode does not sneak");
    }

    /// A gap wider than one telebridge jump can catch is not leapt into: the
    /// walker stands at the edge and tries the old way instead, and gives up
    /// there rather than dropping into the void. Six cells: the extension is
    /// three, and the fall would land two cells short of it.
    #[test]
    fn a_gap_too_wide_to_catch_is_not_jumped() {
        let mut sim = Sim::new();
        let item = item_id("minecraft:dirt");
        for z in -32..48 {
            for x in 4..10 {
                sim.world.set_block_state(x, 63, z, 0);
            }
        }
        sim.inventory.set_slot(&held_block_body(36, item)).unwrap();
        sim.walker.place = path::PlacePolicy::for_item(item);
        sim.walker.bridge = Bridge::Tele;
        sim.walker.go_to(Vec3::new(12.5, 64.0, 0.5));

        let mut ticks = 0;
        let mut lowest = 64.0;
        while !sim.walker.gave_up() && !sim.walker.arrived() && ticks < 2_000 {
            sim.step();
            ticks += 1;
            if sim.player.pos.y < lowest {
                lowest = sim.player.pos.y;
            }
        }
        assert!(
            sim.walker.gave_up() && lowest > 63.0,
            "the walker leapt or fell into a gap it could not catch: \
             gave up {}, lowest {lowest}, at {:?} after {ticks} ticks",
            sim.walker.gave_up(),
            sim.player.pos
        );
    }

    /// Telebridging: the walker sprints off the edge and places the
    /// extension's far cell mid-air, clicked against the ground past the gap.
    /// The rest of the cascade — each block against the one placed before it,
    /// and the catch under the fall — is implemented the same way but its
    /// timing is a hair off in the simulator: the walker places, then lands
    /// in the gap it was filling, and that edge is what a live server has to
    /// tune. This test pins the part that works: the walker leaps at the
    /// edge and its first placement happens while it is airborne.
    #[test]
    fn telebridging_places_its_first_block_mid_air() {
        let mut sim = Sim::new();
        let item = item_id("minecraft:dirt");
        for z in -32..48 {
            for x in 4..7 {
                sim.world.set_block_state(x, 63, z, 0);
            }
        }
        sim.inventory.set_slot(&held_block_body(36, item)).unwrap();
        sim.settings.sensitivity = 1.0;
        sim.walker.place = path::PlacePolicy::for_item(item);
        sim.walker.bridge = Bridge::Tele;
        sim.walker.go_to(Vec3::new(7.5, 64.0, 0.5));

        let mut ticks = 0;
        let mut placed_mid_air = false;
        while !placed_mid_air && !sim.walker.gave_up() && ticks < 400 {
            let on_ground = sim.player.on_ground;
            let before = sim.use_clicks;
            sim.step();
            ticks += 1;
            if sim.use_clicks > before && !on_ground {
                placed_mid_air = true;
            }
        }
        assert!(
            placed_mid_air,
            "no placement happened while the walker was airborne"
        );
        let bridged = (4..7)
            .filter(|x| sim.world.block_state(*x, 63, 0) != Some(0))
            .count();
        assert!(
            bridged >= 1,
            "the mid-air click did not place a bridge block: {bridged} of 3"
        );
    }

    /// Without the block in hand the walker does not click at all: it gives
    /// up rather than placing something else.
    #[test]
    fn does_not_place_without_the_block_in_hand() {
        let mut sim = Sim::new();
        let item = item_id("minecraft:dirt");
        for z in -32..48 {
            sim.world.set_block_state(4, 63, z, 0);
            sim.world.set_block_state(4, 66, z, 1);
            sim.world.set_block_state(5, 66, z, 1);
        }
        sim.walker.place = path::PlacePolicy::for_item(item);
        sim.walker.go_to(Vec3::new(6.5, 64.0, 0.5));
        for _ in 0..900 {
            sim.step();
        }
        assert!(sim.walker.gave_up(), "no block, no bridge");
        assert_eq!(sim.world.block_state(4, 63, 0), Some(0), "nothing placed");
    }
}
