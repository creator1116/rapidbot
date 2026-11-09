//! A controller that stands around the way a person does.
//!
//! [`rapidbot_human::Idler`] decides *when* to glance somewhere, shift weight
//! or nod; this applies those decisions through a [`TickContext`]. It is the
//! long-session counterpart of [`crate::Idle`], which never does anything at
//! all and is the right choice for tests and for a bot whose owner drives
//! every action.

use rapidbot_human::idle::{IdleAction, Idler};
use rapidbot_physics::Keys;

use crate::controller::{Controller, TickContext};

/// A person standing still: nothing for the first few seconds, then an
/// occasional glance, weight shift or nod, each returning to where it
/// started, and further apart the longer they wait.
pub struct HumanIdle {
    idler: Idler,
    /// The view a glance started from, to go back to.
    home: Option<(f32, f32)>,
    sneaking: bool,
    /// The owner did something: let go of the aim on the next tick.
    drop_aim: bool,
}

impl HumanIdle {
    /// A person with this seed's habits.
    pub fn new(seed: u64) -> Self {
        Self {
            idler: Idler::new(seed),
            home: None,
            sneaking: false,
            drop_aim: false,
        }
    }

    /// Seconds this bot has been standing around.
    pub fn idle_for(&self) -> f64 {
        self.idler.idle_for()
    }

    /// How many gestures it has made since it last did anything.
    pub fn gestures(&self) -> u64 {
        self.idler.gestures()
    }

    /// The bot or its owner did something of their own: restart the idle
    /// clock, drop any glance in progress, and let go of the aim, so the view
    /// is never pulled back somewhere that no longer applies.
    pub fn activity(&mut self) {
        self.idler.activity();
        self.home = None;
        self.sneaking = false;
        self.drop_aim = true;
    }
}

impl Controller for HumanIdle {
    fn tick(&mut self, ctx: &mut TickContext<'_>) {
        // The idler works in seconds; a tick is a twentieth of one.
        match self.idler.advance(1.0 / 20.0) {
            Some(IdleAction::Glance { yaw, pitch }) => {
                let home = *self
                    .home
                    .get_or_insert((ctx.player.y_rot, ctx.player.x_rot));
                ctx.aim(home.0 + yaw, (home.1 + pitch).clamp(-90.0, 90.0), 1.5);
            }
            Some(IdleAction::Return) => {
                if let Some((yaw, pitch)) = self.home.take() {
                    ctx.aim(yaw, pitch, 1.5);
                }
            }
            Some(IdleAction::Sneak(down)) => self.sneaking = down,
            None => {}
        }
        if std::mem::take(&mut self.drop_aim) {
            ctx.stop_aiming();
        }
        // The keys are per tick, so the weight shift has to be re-stated.
        ctx.set_keys(Keys {
            shift: self.sneaking,
            ..Keys::default()
        });
    }
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::*;
    use crate::test_support::ControllerHarness;

    /// Five minutes of standing: it looks around and shifts its weight
    /// sometimes, and mostly does nothing at all.
    #[test]
    fn a_standing_bot_moves_a_little() {
        let mut harness = ControllerHarness::new();
        let mut idle = HumanIdle::new(7);
        let (mut aims, mut sneaks, mut still) = (0, 0, 0);
        for _ in 0..(20 * 60 * 5) {
            let asked = harness.tick(&mut idle);
            if asked.aim.is_some() {
                aims += 1;
            }
            if asked.keys.shift {
                sneaks += 1;
            }
            if asked.aim.is_none() && !asked.keys.shift {
                still += 1;
            }
        }
        assert!(aims > 0, "it looked around");
        assert!(sneaks > 0, "it shifted its weight");
        assert!(still > aims + sneaks, "and mostly stood still");
        assert!(idle.gestures() > 0);
        assert!(idle.idle_for() > 60.0, "it kept the session clock");
    }

    /// Someone who has just stopped does not immediately start fidgeting,
    /// and doing something starts that wait again.
    #[test]
    fn nothing_happens_at_first() {
        let mut harness = ControllerHarness::new();
        let mut idle = HumanIdle::new(3);
        for _ in 0..(20 * 4) {
            let asked = harness.tick(&mut idle);
            assert!(
                asked.aim.is_none() && !asked.keys.shift,
                "moved after four seconds: {asked:?}"
            );
        }
        idle.activity();
        assert_eq!(idle.idle_for(), 0.0);
    }

    /// A glance goes back to where it started.
    #[test]
    fn a_glance_comes_home() {
        let mut harness = ControllerHarness::new();
        harness.player().y_rot = 30.0;
        harness.player().x_rot = -10.0;
        let mut idle = HumanIdle::new(11);
        let mut aiming_away = false;
        for _ in 0..(20 * 60 * 5) {
            let asked = harness.tick(&mut idle);
            let Some((yaw, pitch)) = asked.aim else {
                continue;
            };
            if (yaw - 30.0).abs() > 1.0 || (pitch + 10.0).abs() > 1.0 {
                aiming_away = true;
            } else if aiming_away {
                // Back at the view it started from.
                return;
            }
        }
        panic!("the glance never came home");
    }
}
