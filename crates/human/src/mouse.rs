//! A hand moving a mouse.
//!
//! The model outputs what a mouse reports: whole counts per frame. The
//! client feeds them through vanilla's `turnPlayer`, so rotation can only
//! change in the steps a real sensitivity setting allows.
//!
//! Aiming is a sequence of ballistic *submovements*, as in human motor
//! control:
//! - nothing happens for a reaction time after the goal changes;
//! - the first stroke has a bell-shaped speed profile, a duration from
//!   Fitts' law, and lands a little short or long and slightly off-axis;
//! - after a short pause, smaller corrective strokes close the gap;
//! - small slow errors are followed by smooth pursuit instead;
//! - at rest the hand mostly sends nothing, with occasional fidgets;
//! - noise grows with speed, and everything degrades with fatigue.

use crate::{Fatigue, Noise, reaction_time};

/// Where the player wants to look.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aim {
    /// Degrees, any winding (compared modulo 360).
    pub yaw: f32,
    /// Degrees, -90 (up) to 90 (down).
    pub pitch: f32,
    /// How close is good enough, in degrees. Walking somewhere needs maybe
    /// 2-4; clicking a block face under 1.
    pub tolerance: f32,
}

/// One player's motor habits. Randomised per bot so two bots do not move
/// alike.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct MouseProfile {
    /// Median reaction time, seconds.
    pub reaction_median: f64,
    pub reaction_sigma: f64,
    /// Fitts' law: `duration = a + b * log2(amplitude / width + 1)`.
    pub fitts_a: f64,
    pub fitts_b: f64,
    /// Effective target width in degrees for Fitts' law.
    pub target_width: f64,
    /// Fraction of the distance the first stroke covers, on average.
    /// Slightly under 1: people undershoot more than they overshoot.
    pub gain_mean: f64,
    pub gain_sd: f64,
    /// Direction error of a stroke, degrees.
    pub direction_sd: f64,
    /// Sideways bow of a stroke as a fraction of its length.
    pub curvature_sd: f64,
    /// Noise proportional to speed (fraction of each frame's step).
    pub noise_ratio: f64,
    /// Tremor while actively holding an aim, counts per frame at 60 fps.
    pub tremor: f64,
    /// Median pause between a stroke and its correction, seconds.
    pub settle_median: f64,
    /// Mean seconds between fidgets while resting.
    pub fidget_interval: f64,
    /// Errors below this many degrees are tracked smoothly.
    pub pursuit_limit: f64,
}

impl Default for MouseProfile {
    fn default() -> Self {
        Self {
            reaction_median: 0.23,
            reaction_sigma: 0.22,
            fitts_a: 0.08,
            fitts_b: 0.085,
            target_width: 1.5,
            gain_mean: 0.965,
            gain_sd: 0.06,
            direction_sd: 2.0,
            curvature_sd: 0.04,
            noise_ratio: 0.035,
            tremor: 0.22,
            settle_median: 0.09,
            fidget_interval: 5.0,
            pursuit_limit: 4.0,
        }
    }
}

impl MouseProfile {
    /// A plausible individual: each trait varied around the default.
    pub fn random(noise: &mut Noise) -> Self {
        let d = Self::default();
        let gain_mean = (d.gain_mean + 0.02 * noise.normal()).clamp(0.9, 1.02);
        let mut vary = |v: f64, spread: f64| v * noise.lognormal(1.0, spread);
        Self {
            reaction_median: vary(d.reaction_median, 0.12),
            reaction_sigma: vary(d.reaction_sigma, 0.15),
            fitts_a: vary(d.fitts_a, 0.2),
            fitts_b: vary(d.fitts_b, 0.2),
            target_width: vary(d.target_width, 0.15),
            gain_mean,
            gain_sd: vary(d.gain_sd, 0.2),
            direction_sd: vary(d.direction_sd, 0.25),
            curvature_sd: vary(d.curvature_sd, 0.3),
            noise_ratio: vary(d.noise_ratio, 0.25),
            tremor: vary(d.tremor, 0.3),
            settle_median: vary(d.settle_median, 0.2),
            fidget_interval: vary(d.fidget_interval, 0.4),
            pursuit_limit: vary(d.pursuit_limit, 0.2),
        }
    }
}

/// A ballistic stroke, in mouse counts.
#[derive(Debug, Clone, Copy)]
struct Stroke {
    plan_x: f64,
    plan_y: f64,
    duration: f64,
    elapsed: f64,
    /// Counts already produced along the plan (before noise).
    done_x: f64,
    done_y: f64,
    /// Sideways bow, counts at the midpoint.
    bow: f64,
    /// Skews the speed peak earlier (<1) or later (>1).
    skew: f64,
    corrections: u8,
}

impl Stroke {
    /// Minimum-jerk position profile, 0 → 1.
    fn progress(&self, t: f64) -> f64 {
        let tau = (t / self.duration).clamp(0.0, 1.0).powf(self.skew);
        tau * tau * tau * (10.0 - 15.0 * tau + 6.0 * tau * tau)
    }

    /// Planned displacement at `t`, including the sideways bow.
    fn position(&self, t: f64) -> (f64, f64) {
        let s = self.progress(t);
        let len = self.plan_x.hypot(self.plan_y).max(1e-9);
        let (nx, ny) = (-self.plan_y / len, self.plan_x / len);
        let lateral = self.bow * (std::f64::consts::PI * s).sin();
        (
            self.plan_x * s + nx * lateral,
            self.plan_y * s + ny * lateral,
        )
    }
}

#[derive(Debug, Clone, Copy)]
enum Phase {
    /// Hand resting or holding an aim that is good enough.
    Rest,
    /// Noticed something to do; nothing moves yet.
    Reacting {
        remaining: f64,
    },
    Moving(Stroke),
    /// Pause after a stroke, before deciding whether to correct.
    Settling {
        remaining: f64,
        corrections: u8,
    },
    /// Smoothly following a small or slowly moving error.
    Pursuit,
    /// A small aimless movement while resting.
    Fidget(Stroke),
}

pub struct MouseModel {
    noise: Noise,
    pub profile: MouseProfile,
    pub fatigue: Fatigue,
    target: Option<Aim>,
    phase: Phase,
    /// Fractional counts not yet reported (mice report integers).
    carry_x: f64,
    carry_y: f64,
    next_fidget: f64,
    /// Pending re-plan after the goal moved mid-stroke.
    replan_in: Option<f64>,
    /// Aim the current stroke was planned for.
    planned_for: Option<Aim>,
}

fn wrap_degrees(mut a: f64) -> f64 {
    a %= 360.0;
    if a >= 180.0 {
        a -= 360.0;
    }
    if a < -180.0 {
        a += 360.0;
    }
    a
}

impl MouseModel {
    pub fn new(seed: u64) -> Self {
        let mut noise = Noise::new(seed);
        let profile = MouseProfile::random(&mut noise);
        Self::with_profile(seed ^ 0x9e37_79b9_7f4a_7c15, profile)
    }

    pub fn with_profile(seed: u64, profile: MouseProfile) -> Self {
        let mut noise = Noise::new(seed);
        let next_fidget = noise.exponential(profile.fidget_interval);
        Self {
            noise,
            profile,
            fatigue: Fatigue::default(),
            target: None,
            phase: Phase::Rest,
            carry_x: 0.0,
            carry_y: 0.0,
            next_fidget,
            replan_in: None,
            planned_for: None,
        }
    }

    /// Sets (or clears) what the player wants to look at. Cheap to call
    /// every tick with a slowly changing aim.
    pub fn set_target(&mut self, target: Option<Aim>) {
        if let (Some(new), Some(planned), Phase::Moving(_)) = (target, self.planned_for, self.phase)
        {
            let moved = wrap_degrees((new.yaw - planned.yaw) as f64)
                .hypot((new.pitch - planned.pitch) as f64);
            // The goal jumped mid-stroke: the hand keeps going until the
            // change has been noticed, then redirects.
            if moved > 3.0 && self.replan_in.is_none() {
                let rt = self.reaction() * 0.7;
                self.replan_in = Some(rt);
            }
        }
        self.target = target;
    }

    pub fn target(&self) -> Option<Aim> {
        self.target
    }

    /// True while a stroke or pursuit is under way.
    pub fn is_moving(&self) -> bool {
        matches!(self.phase, Phase::Moving(_) | Phase::Pursuit)
    }

    fn reaction(&mut self) -> f64 {
        reaction_time(
            &mut self.noise,
            self.profile.reaction_median,
            self.profile.reaction_sigma,
            self.fatigue.level(),
        )
    }

    fn plan(&mut self, err_x: f64, err_y: f64, deg_per_count: f64, corrections: u8) -> Stroke {
        let fatigue = self.fatigue.level();
        let noise_factor = self.fatigue.noise_factor();
        let amplitude_deg = err_x.hypot(err_y) * deg_per_count;

        let gain = if corrections == 0 {
            self.noise.gaussian(
                self.profile.gain_mean - 0.05 * fatigue,
                self.profile.gain_sd * noise_factor,
            )
        } else {
            // Corrections are small and better calibrated.
            self.noise
                .gaussian(0.985, self.profile.gain_sd * 0.7 * noise_factor)
        }
        .clamp(0.7, 1.2);
        let angle = self
            .noise
            .gaussian(0.0, self.profile.direction_sd * noise_factor)
            .to_radians();
        let (sin, cos) = angle.sin_cos();
        let plan_x = (err_x * cos - err_y * sin) * gain;
        let plan_y = (err_y * cos + err_x * sin) * gain;

        let fitts = self.profile.fitts_a
            + self.profile.fitts_b * (amplitude_deg / self.profile.target_width + 1.0).log2();
        let duration =
            (fitts / self.fatigue.speed_factor() * self.noise.lognormal(1.0, 0.12)).max(0.06);
        let length = plan_x.hypot(plan_y);
        Stroke {
            plan_x,
            plan_y,
            duration,
            elapsed: 0.0,
            done_x: 0.0,
            done_y: 0.0,
            bow: self.noise.gaussian(0.0, self.profile.curvature_sd) * length,
            skew: self.noise.gaussian(1.0, 0.07).clamp(0.8, 1.25),
            corrections,
        }
    }

    fn plan_fidget(&mut self) -> Stroke {
        let length = self.noise.uniform(1.0, 9.0);
        let direction = self.noise.uniform(0.0, std::f64::consts::TAU);
        Stroke {
            plan_x: length * direction.cos(),
            // Wrists move sideways more freely than up and down.
            plan_y: length * direction.sin() * 0.6,
            duration: self.noise.uniform(0.08, 0.3),
            elapsed: 0.0,
            done_x: 0.0,
            done_y: 0.0,
            bow: 0.0,
            skew: 1.0,
            corrections: 0,
        }
    }

    fn advance(&mut self, stroke: &mut Stroke, dt: f64) -> (f64, f64) {
        stroke.elapsed += dt;
        let (px, py) = stroke.position(stroke.elapsed);
        let (step_x, step_y) = (px - stroke.done_x, py - stroke.done_y);
        stroke.done_x = px;
        stroke.done_y = py;
        // Signal-dependent noise: faster movement, more scatter.
        let sd = self.profile.noise_ratio * self.fatigue.noise_factor() * step_x.hypot(step_y);
        (
            step_x + self.noise.gaussian(0.0, sd),
            step_y + self.noise.gaussian(0.0, sd),
        )
    }

    fn tremor(&mut self, dt: f64) -> (f64, f64) {
        let sd = self.profile.tremor * self.fatigue.noise_factor() * (dt * 60.0).sqrt();
        (
            self.noise.gaussian(0.0, sd),
            self.noise.gaussian(0.0, sd * 0.7),
        )
    }

    /// Advances one frame and returns the mouse counts `(dx, dy)` it
    /// produced. `yaw`/`pitch` are the player's current rotation and
    /// `deg_per_count` what one count turns the view by at the current
    /// sensitivity.
    pub fn frame(&mut self, dt: f64, yaw: f32, pitch: f32, deg_per_count: f64) -> (i32, i32) {
        let error = self.target.map(|t| {
            (
                wrap_degrees((t.yaw - yaw) as f64) / deg_per_count,
                (t.pitch.clamp(-90.0, 90.0) - pitch) as f64 / deg_per_count,
                t.tolerance.max(0.05) as f64 / deg_per_count,
            )
        });
        let off_target =
            |tol_scale: f64| error.is_some_and(|(ex, ey, tol)| ex.hypot(ey) > tol * tol_scale);
        let pursuit_limit = self.profile.pursuit_limit / deg_per_count;

        if let Some(remaining) = self.replan_in {
            let remaining = remaining - dt;
            self.replan_in = Some(remaining);
            if remaining <= 0.0 {
                self.replan_in = None;
                if let (Phase::Moving(_), Some((ex, ey, _))) = (self.phase, error) {
                    let stroke = self.plan(ex, ey, deg_per_count, 0);
                    self.planned_for = self.target;
                    self.phase = Phase::Moving(stroke);
                }
            }
        }

        let (out_x, out_y) = match self.phase {
            Phase::Rest => {
                if off_target(1.0) {
                    let (ex, ey, _) = error.unwrap();
                    self.phase = if ex.hypot(ey) < pursuit_limit {
                        Phase::Pursuit
                    } else {
                        Phase::Reacting {
                            remaining: self.reaction(),
                        }
                    };
                    (0.0, 0.0)
                } else {
                    self.next_fidget -= dt;
                    if self.next_fidget <= 0.0 {
                        self.next_fidget = self.noise.exponential(self.profile.fidget_interval);
                        self.phase = Phase::Fidget(self.plan_fidget());
                    }
                    (0.0, 0.0)
                }
            }
            Phase::Reacting { remaining } => {
                let remaining = remaining - dt;
                if remaining > 0.0 {
                    self.phase = Phase::Reacting { remaining };
                } else if let Some((ex, ey, _)) = error.filter(|_| off_target(1.0)) {
                    let stroke = self.plan(ex, ey, deg_per_count, 0);
                    self.planned_for = self.target;
                    self.phase = Phase::Moving(stroke);
                } else {
                    self.phase = Phase::Rest;
                }
                (0.0, 0.0)
            }
            Phase::Moving(mut stroke) => {
                let step = self.advance(&mut stroke, dt);
                self.phase = if stroke.elapsed >= stroke.duration {
                    let pause = self.noise.lognormal(self.profile.settle_median, 0.3)
                        * (1.0 + 0.3 * self.fatigue.level());
                    Phase::Settling {
                        remaining: pause,
                        corrections: stroke.corrections,
                    }
                } else {
                    Phase::Moving(stroke)
                };
                step
            }
            Phase::Settling {
                remaining,
                corrections,
            } => {
                let remaining = remaining - dt;
                if remaining > 0.0 {
                    self.phase = Phase::Settling {
                        remaining,
                        corrections,
                    };
                } else if off_target(1.0) && corrections < 3 {
                    let (ex, ey, _) = error.unwrap();
                    let stroke = self.plan(ex, ey, deg_per_count, corrections + 1);
                    self.planned_for = self.target;
                    self.phase = Phase::Moving(stroke);
                } else {
                    self.phase = Phase::Rest;
                }
                self.tremor(dt)
            }
            Phase::Pursuit => match error {
                Some((ex, ey, tol)) => {
                    let distance = ex.hypot(ey);
                    if distance <= tol * 0.5 {
                        self.phase = Phase::Rest;
                        (0.0, 0.0)
                    } else if distance > pursuit_limit * 1.5 {
                        // Too far to follow smoothly: needs a proper stroke.
                        self.phase = Phase::Reacting {
                            remaining: self.reaction() * 0.6,
                        };
                        (0.0, 0.0)
                    } else {
                        // First-order lag of about 130 ms.
                        let k = 1.0 - (-dt * self.fatigue.speed_factor() / 0.13).exp();
                        let (tx, ty) = self.tremor(dt);
                        let sd =
                            self.profile.noise_ratio * self.fatigue.noise_factor() * distance * k;
                        (
                            ex * k + self.noise.gaussian(0.0, sd) + tx * 0.5,
                            ey * k + self.noise.gaussian(0.0, sd) + ty * 0.5,
                        )
                    }
                }
                None => {
                    self.phase = Phase::Rest;
                    (0.0, 0.0)
                }
            },
            Phase::Fidget(mut stroke) => {
                let step = self.advance(&mut stroke, dt);
                self.phase = if stroke.elapsed >= stroke.duration {
                    Phase::Rest
                } else {
                    Phase::Fidget(stroke)
                };
                step
            }
        };

        let active = !matches!(self.phase, Phase::Rest);
        self.fatigue.update(dt, active, &mut self.noise);

        self.carry_x += out_x;
        self.carry_y += out_y;
        let dx = self.carry_x.round();
        let dy = self.carry_y.round();
        self.carry_x -= dx;
        self.carry_y -= dy;
        (dx as i32, dy as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DPC: f64 = 0.15;
    const DT: f64 = 1.0 / 60.0;

    /// Applies counts the way vanilla does at default sensitivity.
    fn apply(yaw: &mut f32, pitch: &mut f32, dx: i32, dy: i32) {
        *yaw += (dx as f64 * 1.0) as f32 * 0.15;
        *pitch = (*pitch + (dy as f64 * 1.0) as f32 * 0.15).clamp(-90.0, 90.0);
    }

    struct Run {
        yaw: Vec<f32>,
        first_motion: Option<f64>,
        max_speed: f64,
    }

    fn turn(seed: u64, target_yaw: f32, seconds: f64) -> Run {
        let mut m = MouseModel::new(seed);
        // No fidgets, so the first motion is the reaction to the target.
        m.next_fidget = f64::MAX;
        m.set_target(Some(Aim {
            yaw: target_yaw,
            pitch: 0.0,
            tolerance: 1.0,
        }));
        let (mut yaw, mut pitch) = (0.0f32, 0.0f32);
        let mut run = Run {
            yaw: vec![],
            first_motion: None,
            max_speed: 0.0,
        };
        for i in 0..(seconds / DT) as usize {
            let (dx, dy) = m.frame(DT, yaw, pitch, DPC);
            if (dx, dy) != (0, 0) && run.first_motion.is_none() {
                run.first_motion = Some(i as f64 * DT);
            }
            run.max_speed = run.max_speed.max((dx as f64).hypot(dy as f64) * DPC / DT);
            apply(&mut yaw, &mut pitch, dx, dy);
            run.yaw.push(yaw);
        }
        run
    }

    #[test]
    fn reaches_the_target_after_a_reaction_time() {
        for seed in 0..40 {
            let run = turn(seed, 90.0, 3.0);
            let rt = run.first_motion.expect("moved");
            assert!((0.12..1.2).contains(&rt), "seed {seed}: reaction {rt}");
            let end = *run.yaw.last().unwrap();
            assert!((end - 90.0).abs() < 2.5, "seed {seed}: ended at {end}");
            // A person flicks at a few hundred degrees per second, not thousands.
            assert!(
                run.max_speed < 900.0,
                "seed {seed}: {} deg/s",
                run.max_speed
            );
            assert!(
                run.max_speed > 100.0,
                "seed {seed}: {} deg/s",
                run.max_speed
            );
        }
    }

    #[test]
    fn speed_is_bell_shaped_not_constant() {
        let run = turn(3, 120.0, 3.0);
        let speeds: Vec<f32> = run.yaw.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
        let peak = speeds.iter().cloned().fold(0.0, f32::max);
        let moving: Vec<f32> = speeds.iter().cloned().filter(|s| *s > 0.0).collect();
        let slow = moving.iter().filter(|s| **s < peak * 0.3).count();
        // Plenty of slow frames at the start and end of the stroke.
        assert!(slow >= 4, "only {slow} slow frames of {}", moving.len());
    }

    #[test]
    fn strokes_overshoot_and_undershoot_across_people() {
        let (mut over, mut under) = (0, 0);
        for seed in 0..200 {
            let run = turn(seed, 60.0, 3.0);
            let peak = run.yaw.iter().cloned().fold(f32::MIN, f32::max);
            if peak > 61.0 {
                over += 1;
            }
            // Where the first stroke stopped: first frame pair with no motion after moving.
            let moved = run.yaw.iter().position(|y| *y > 5.0).unwrap();
            let stop = run.yaw[moved..]
                .windows(3)
                .position(|w| w[0] == w[1] && w[1] == w[2])
                .map(|i| run.yaw[moved + i]);
            if stop.is_some_and(|y| y < 59.0) {
                under += 1;
            }
        }
        assert!(over > 10, "overshoots: {over}");
        assert!(under > 10, "undershoots: {under}");
        assert!(
            under > over / 2,
            "undershoot should be common: {under} vs {over}"
        );
    }

    #[test]
    fn deterministic_per_seed_and_different_between_seeds() {
        assert_eq!(turn(7, 90.0, 2.0).yaw, turn(7, 90.0, 2.0).yaw);
        assert_ne!(turn(7, 90.0, 2.0).yaw, turn(8, 90.0, 2.0).yaw);
    }

    #[test]
    fn rest_is_mostly_silent_with_occasional_fidgets() {
        let mut m = MouseModel::new(11);
        let (mut silent, mut total) = (0, 0);
        for _ in 0..(120.0 / DT) as usize {
            let (dx, dy) = m.frame(DT, 0.0, 0.0, DPC);
            total += 1;
            if (dx, dy) == (0, 0) {
                silent += 1;
            }
        }
        assert!(silent < total, "a resting hand still fidgets sometimes");
        assert!(
            silent as f64 > total as f64 * 0.9,
            "{silent}/{total} silent"
        );
    }

    #[test]
    fn fatigue_slows_and_loosens() {
        let mean_duration = |fatigue: f64| {
            let mut m = MouseModel::with_profile(5, MouseProfile::default());
            m.fatigue.set_level(fatigue);
            (0..200)
                .map(|_| m.plan(400.0, 0.0, DPC, 0).duration)
                .sum::<f64>()
                / 200.0
        };
        assert!(mean_duration(0.9) > mean_duration(0.0) * 1.15);
    }

    #[test]
    fn small_errors_are_pursued_smoothly() {
        let mut m = MouseModel::new(2);
        m.next_fidget = f64::MAX;
        let (mut yaw, mut pitch) = (0.0f32, 0.0f32);
        let mut max_step = 0;
        // The target drifts at 10 degrees per second.
        for i in 0..240 {
            m.set_target(Some(Aim {
                yaw: i as f32 * DT as f32 * 10.0,
                pitch: 0.0,
                tolerance: 0.5,
            }));
            let (dx, dy) = m.frame(DT, yaw, pitch, DPC);
            max_step = max_step.max(dx.abs());
            apply(&mut yaw, &mut pitch, dx, dy);
        }
        assert!((yaw - 40.0).abs() < 4.0, "lagging at {yaw}");
        assert!(
            max_step < 12,
            "pursuit should have no flicks, saw {max_step} counts"
        );
    }
}
