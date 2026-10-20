//! The attract-mode camera behind the main menu: a spectator director
//! that roams the demo battle.
//!
//! Every few seconds it scores what is going on ([`pick_interest`]) and
//! glides to one of the candidates, chosen at random by score:
//!
//! - fights (units aiming at something), clustered, most of all where
//!   several teams meet;
//! - SIGTERM launches (the bomber is followed while it flies);
//! - artillery banks (Exploits, Obelisks, Pointers), new buildings,
//!   frontlines (where two teams' armies are closest);
//! - else the map itself: free datavents and open ground.
//!
//! Anything by a homebase scores low, and so does the spot it already
//! shows, so the camera keeps moving through the map rather than
//! resting on bases. Each shot gets its own orbit speed and direction,
//! pitch and distance, eased into; every other fight gets a low fly-by.
//! Render-side only: it reads the sim, never writes it.

use bevy::math::ops;
use bevy::prelude::*;

use crate::game_setup::DevOptions;
use crate::rendering::camera::{MapBounds, RtsCamera, RtsCameraState};
use crate::rng::clock_f64;
use crate::sim::DetFloat;
use crate::terrain::geovent::GeoventSmoker;
use crate::terrain::heightmap::Heightmap;
use crate::units::combat::AimTarget;
use crate::units::components::{Homebase, TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::lifecycle::spawning::Emerging;

/// What a candidate shot shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Kind {
    Fight,
    Launch,
    Artillery,
    NewBuilding,
    Frontline,
    Roam,
}

impl Kind {
    /// Seconds a shot of this kind holds before the next pick.
    fn dwell(self) -> f32 {
        match self {
            Kind::Fight => 11.0,
            Kind::Launch => 9.0,
            Kind::Frontline => 9.0,
            Kind::Artillery => 8.0,
            Kind::Roam => 7.0,
            Kind::NewBuilding => 6.0,
        }
    }
}

/// One unit as the director sees it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Sighting {
    pub pos: Vec3,
    pub team: u8,
    pub role: Role,
    pub entity: Entity,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Role {
    /// Aiming at something right now.
    Fighting,
    /// A SIGTERM bomber in flight.
    Launch,
    Artillery,
    NewBuilding,
    Homebase,
    /// Any other mobile unit (frontlines are made of these and fighters).
    Mobile,
    /// Any other structure.
    Structure,
}

/// A candidate shot.
#[derive(Clone, Copy, Debug)]
pub(super) struct Interest {
    pub pos: Vec3,
    pub kind: Kind,
    pub score: f32,
    /// A unit to follow (a launch), instead of a fixed point.
    pub follow: Option<Entity>,
}

/// Weights: what the director cares about, per unit or per shot.
const FIGHT_WEIGHT: f32 = 3.0;
const LAUNCH_SCORE: f32 = 40.0;
const ARTILLERY_WEIGHT: f32 = 1.5;
const NEW_BUILDING_SCORE: f32 = 4.0;
const FRONTLINE_SCORE: f32 = 8.0;
const ROAM_SCORE: f32 = 1.5;
/// Fights are clustered on a grid this coarse (elmos).
const CLUSTER: f32 = 384.0;
/// Within this of a homebase a shot scores less: ×0.3, a fight ×0.6.
const HOME_RADIUS: f32 = 700.0;
/// Within this of the current shot a candidate scores ×0.35.
const STALE_RADIUS: f32 = 500.0;
/// Two armies closer than this make a frontline.
const FRONTLINE_RANGE: f32 = 2500.0;
/// Units per team the frontline search samples.
const FRONTLINE_SAMPLE: usize = 48;
/// Open-ground roam points tried per pick.
const ROAM_POINTS: usize = 6;

/// Score everything worth a shot. `current` is where the camera looks
/// now; `rolls` supplies `0..1` randoms for the open-ground points.
pub(super) fn interests(
    seen: &[Sighting],
    vents: &[Vec3],
    bounds: (Vec2, Vec2),
    current: Option<Vec3>,
    mut rolls: impl FnMut() -> f32,
) -> Vec<Interest> {
    let mut out = Vec::new();
    let homes: Vec<Vec3> = seen
        .iter()
        .filter(|s| s.role == Role::Homebase)
        .map(|s| s.pos)
        .collect();

    // Fights: per grid cell, the fighters in it and round it.
    let cell = |p: Vec3| {
        (
            (p.x / CLUSTER).floor() as i32,
            (p.z / CLUSTER).floor() as i32,
        )
    };
    let fighters: Vec<&Sighting> = seen.iter().filter(|s| s.role == Role::Fighting).collect();
    let mut cells: Vec<(i32, i32)> = fighters.iter().map(|s| cell(s.pos)).collect();
    cells.sort_unstable();
    cells.dedup();
    for &(cx, cz) in &cells {
        let near: Vec<&&Sighting> = fighters
            .iter()
            .filter(|s| {
                let (x, z) = cell(s.pos);
                (x - cx).abs() <= 1 && (z - cz).abs() <= 1
            })
            .collect();
        let mut teams: Vec<u8> = near.iter().map(|s| s.team).collect();
        teams.sort_unstable();
        teams.dedup();
        let centre = near.iter().map(|s| s.pos).sum::<Vec3>() / near.len() as f32;
        out.push(Interest {
            pos: centre,
            kind: Kind::Fight,
            score: FIGHT_WEIGHT * near.len() as f32 * (1.0 + 0.5 * (teams.len() as f32 - 1.0)),
            follow: None,
        });
    }

    for s in seen {
        match s.role {
            Role::Launch => out.push(Interest {
                pos: s.pos,
                kind: Kind::Launch,
                score: LAUNCH_SCORE,
                follow: Some(s.entity),
            }),
            Role::Artillery => {
                let bank = seen
                    .iter()
                    .filter(|o| o.role == Role::Artillery && o.pos.distance(s.pos) < CLUSTER)
                    .count();
                out.push(Interest {
                    pos: s.pos,
                    kind: Kind::Artillery,
                    score: ARTILLERY_WEIGHT * bank as f32,
                    follow: None,
                });
            }
            Role::NewBuilding => out.push(Interest {
                pos: s.pos,
                kind: Kind::NewBuilding,
                score: NEW_BUILDING_SCORE,
                follow: None,
            }),
            _ => {}
        }
    }

    // Frontlines: for each pair of teams, the closest pair of their
    // (sampled) soldiers.
    let mut teams: Vec<u8> = seen.iter().map(|s| s.team).collect();
    teams.sort_unstable();
    teams.dedup();
    let army = |team: u8| -> Vec<Vec3> {
        let all: Vec<Vec3> = seen
            .iter()
            .filter(|s| s.team == team && matches!(s.role, Role::Mobile | Role::Fighting))
            .map(|s| s.pos)
            .collect();
        let stride = all.len().div_ceil(FRONTLINE_SAMPLE).max(1);
        all.into_iter().step_by(stride).collect()
    };
    for (i, &a) in teams.iter().enumerate() {
        let left = army(a);
        for &b in &teams[i + 1..] {
            let right = army(b);
            let closest = left
                .iter()
                .flat_map(|&p| right.iter().map(move |&q| (p, q)))
                .min_by(|x, y| x.0.distance(x.1).total_cmp(&y.0.distance(y.1)));
            if let Some((p, q)) = closest
                && p.distance(q) < FRONTLINE_RANGE
            {
                out.push(Interest {
                    pos: (p + q) * 0.5,
                    kind: Kind::Frontline,
                    score: FRONTLINE_SCORE,
                    follow: None,
                });
            }
        }
    }

    // The map itself: free datavents and open ground well inside it.
    let (min, max) = bounds;
    let inset = (max - min) * 0.15;
    for &v in vents {
        out.push(Interest {
            pos: v,
            kind: Kind::Roam,
            score: ROAM_SCORE / (1.0 + vents.len() as f32 / 8.0),
            follow: None,
        });
    }
    for _ in 0..ROAM_POINTS {
        let p = min + inset + (max - min - 2.0 * inset) * Vec2::new(rolls(), rolls());
        out.push(Interest {
            pos: Vec3::new(p.x, 0.0, p.y),
            kind: Kind::Roam,
            score: ROAM_SCORE,
            follow: None,
        });
    }

    for i in &mut out {
        let flat = |a: Vec3, b: Vec3| a.xz().distance(b.xz());
        if homes.iter().any(|&h| flat(h, i.pos) < HOME_RADIUS) {
            i.score *= if i.kind == Kind::Fight { 0.6 } else { 0.3 };
        }
        if current.is_some_and(|c| flat(c, i.pos) < STALE_RADIUS) {
            i.score *= 0.35;
        }
    }
    out
}

/// Pick one of `candidates` at random, weighted by score (`roll` in `0..1`).
pub(super) fn pick_interest(candidates: &[Interest], roll: f32) -> Option<Interest> {
    let total: f32 = candidates.iter().map(|i| i.score).sum();
    if total <= 0.0 {
        return None;
    }
    let mut d = roll * total;
    for i in candidates {
        if d < i.score {
            return Some(*i);
        }
        d -= i.score;
    }
    candidates.last().copied()
}

/// The director's state.
#[derive(Resource, Default)]
pub(super) struct AttractCamera {
    /// Seconds until the next pick.
    retarget_in: f32,
    /// Seconds until the next check for a fight worth cutting to.
    check_in: f32,
    shot: Option<Interest>,
    clock: f32,
    /// A low fly-by in progress instead of the orbit.
    pass: Option<LowPass>,
    /// Fights shown since the last fly-by: every other one gets a pass.
    fights_since_pass: u32,
    /// This shot's orbit speed (rad/s, signed), pitch and distance, and
    /// the orbit speed eased toward it.
    yaw_rate: f32,
    yaw_rate_now: f32,
    pitch: f32,
    distance: f32,
}

/// A low, fast sweep across a battle: the focus travels `start → end`
/// through the fight while the camera sits low behind it, looking along
/// the direction of travel — a helicopter shot between the wide orbits.
struct LowPass {
    start: Vec3,
    end: Vec3,
    /// Camera yaw that looks along `start → end`.
    yaw: f32,
    elapsed: f32,
}

/// Fastest and slowest glide of the focus (elmos/s), and how much of the
/// remaining way it covers per second in between.
const GLIDE_MAX: f32 = 520.0;
const GLIDE_MIN: f32 = 60.0;
const GLIDE_RATE: f32 = 0.9;
/// A shot's orbit speed: between these (rad/s), either way round.
const ORBIT_SLOW: f32 = 0.03;
const ORBIT_FAST: f32 = 0.11;
/// A shot's pitch (rad above the ground plane) and distance (elmos),
/// fights a little closer.
const PITCH_RANGE: (f32, f32) = (0.45, 0.85);
const DISTANCE_RANGE: (f32, f32) = (950.0, 1500.0);
const FIGHT_DISTANCE_RANGE: (f32, f32) = (800.0, 1150.0);
/// How fast pitch and distance ease to the shot's (per second).
const EASE: f32 = 0.6;
/// Check this often for a fight to cut to, and cut when one scores this.
const CHECK_SECONDS: f32 = 1.5;
const CUT_SCORE: f32 = 12.0;
/// Half-length of a fly-by's ground track (elmos) and its duration.
const PASS_HALF_LENGTH: f32 = 480.0;
const PASS_SECONDS: f32 = 9.0;
/// Camera pitch and distance during a fly-by: ~20° above the ground
/// plane, ~210 elmos up — low enough to skim the fight, high enough to
/// clear Kernel Panic's terrain and towers.
const PASS_PITCH: f32 = 0.34;
const PASS_DISTANCE: f32 = 620.0;
/// Minimum height of the attract camera's focus above the terrain it
/// crosses. A glide or fly-by between fights at different altitudes
/// must clear intervening ridges instead of tunnelling through them.
const GROUND_CLEARANCE: f32 = 12.0;
/// The focus stays this fraction of the map in from its edges.
const EDGE_INSET: f32 = 0.06;

fn roll() -> f32 {
    clock_f64() as f32
}

fn between((lo, hi): (f32, f32)) -> f32 {
    lo + (hi - lo) * roll()
}

/// What the director reads of each unit: where, what, whose, and
/// whether it is aiming, emerging or a homebase.
type UnitView = (
    Entity,
    &'static GlobalTransform,
    &'static UnitType,
    &'static TeamId,
    Has<AimTarget>,
    Has<Emerging>,
    Has<Homebase>,
);

#[allow(clippy::too_many_arguments)]
pub(super) fn attract_camera(
    time: Res<Time>,
    mut director: ResMut<AttractCamera>,
    bounds: Res<MapBounds>,
    heightmap: Option<Res<Heightmap>>,
    units: Query<UnitView>,
    vents: Query<&GeoventSmoker>,
    registry: Option<Res<UnitRegistry>>,
    mut cam: Query<&mut RtsCameraState, With<RtsCamera>>,
    dev: Res<DevOptions>,
) {
    let Ok(mut state) = cam.single_mut() else {
        return;
    };
    let dt = time.delta_secs();
    director.clock += dt;
    let zoom = dev.attract_distance.map_or(1.0, |d| d / 1250.0);
    // Terrain floor for every focus write below. Fights sit at their
    // ground height, and a straight glide or fly-by between fights at
    // different altitudes would tunnel through an intervening ridge
    // (Data_Cache_L1's mesas are ~1000 elmos tall) — minutes of solid
    // rock on screen. Riding `terrain + clearance` sweeps the camera
    // up and over instead; the camera smoothing eases the vertical.
    let ride_terrain = |focus: &mut Vec3| {
        if let Some(hm) = heightmap.as_deref() {
            let floor = hm.sample(focus.x, focus.z) + GROUND_CLEARANCE;
            if focus.y < floor {
                focus.y = floor;
            }
        }
    };
    // A freshly loaded map invalidates the old shot.
    if bounds.is_changed() {
        director.shot = None;
        director.pass = None;
        director.retarget_in = 0.0;
    }
    let (min, max) = (bounds.min.xz(), bounds.max.xz());
    let snapshot = || -> Vec<Sighting> {
        units
            .iter()
            .map(|(entity, tf, kind, team, aiming, emerging, home)| {
                let building = registry.as_deref().is_some_and(|r| r.is_building(kind.0));
                let role = if home {
                    Role::Homebase
                } else if kind.0 == UnitKind::Signal {
                    Role::Launch
                } else if aiming {
                    Role::Fighting
                } else if matches!(
                    kind.0,
                    UnitKind::Exploit | UnitKind::Obelisk | UnitKind::Pointer
                ) {
                    Role::Artillery
                } else if emerging && building {
                    Role::NewBuilding
                } else if building {
                    Role::Structure
                } else {
                    Role::Mobile
                };
                Sighting {
                    pos: tf.translation(),
                    team: team.0,
                    role,
                    entity,
                }
            })
            .collect()
    };
    let vent_points = || -> Vec<Vec3> { vents.iter().map(|v| v.pos).collect() };

    director.retarget_in -= dt;
    director.check_in -= dt;
    // A big fight or a launch cuts in on a quieter shot.
    let quiet = director
        .shot
        .is_none_or(|s| !matches!(s.kind, Kind::Fight | Kind::Launch));
    let mut cut = false;
    if quiet && director.check_in <= 0.0 && director.pass.is_none() {
        director.check_in = CHECK_SECONDS;
        let seen = snapshot();
        cut = interests(&seen, &[], (min, max), None, roll)
            .iter()
            .any(|i| matches!(i.kind, Kind::Fight | Kind::Launch) && i.score >= CUT_SCORE);
    }
    // A followed bomber gone ends its shot.
    let lost = director
        .shot
        .and_then(|s| s.follow)
        .is_some_and(|e| units.get(e).is_err());
    if director.retarget_in <= 0.0 || director.shot.is_none() || cut || lost {
        director.pass = None;
        let seen = snapshot();
        let current = Some(state.focus);
        let candidates = interests(&seen, &vent_points(), (min, max), current, roll);
        let shot = pick_interest(&candidates, roll()).unwrap_or(Interest {
            pos: (bounds.min + bounds.max) * 0.5,
            kind: Kind::Roam,
            score: 0.0,
            follow: None,
        });
        director.retarget_in = shot.kind.dwell();
        // Each shot its own way round, pitch and distance.
        let speed = between((ORBIT_SLOW, ORBIT_FAST));
        director.yaw_rate = if roll() < 0.5 { speed } else { -speed };
        director.pitch = between(PITCH_RANGE);
        director.distance = between(if shot.kind == Kind::Fight {
            FIGHT_DISTANCE_RANGE
        } else {
            DISTANCE_RANGE
        });
        if shot.kind == Kind::Fight {
            director.fights_since_pass += 1;
            if director.fights_since_pass >= 2 {
                director.fights_since_pass = 0;
                director.pass = Some(LowPass::across(shot.pos, &bounds));
                director.retarget_in = PASS_SECONDS;
            }
        }
        director.shot = Some(shot);
    }

    if let Some(pass) = director.pass.as_mut() {
        // The camera smoothing eases the swoop in and out; the focus
        // itself runs the track with a smoothstep so the pass slows as
        // it enters and leaves the fight.
        pass.elapsed += dt;
        let t = (pass.elapsed / PASS_SECONDS).det_clamp(0.0, 1.0);
        let eased = t * t * (3.0 - 2.0 * t);
        state.focus = pass.start.lerp(pass.end, eased);
        ride_terrain(&mut state.focus);
        // The eye trails the focus by the pass offset (same arithmetic
        // as `compute_transform_from_state`); lift the focus until the
        // eye clears the terrain it hangs over, too — a ridge behind
        // the track would otherwise swallow the camera mid-pass.
        if let Some(hm) = heightmap.as_deref() {
            let horizontal = PASS_DISTANCE * zoom * ops::cos(PASS_PITCH);
            let eye_y = state.focus.y + PASS_DISTANCE * zoom * ops::sin(PASS_PITCH);
            let eye_x = state.focus.x + horizontal * ops::sin(pass.yaw);
            let eye_z = state.focus.z + horizontal * ops::cos(pass.yaw);
            let lift = hm.sample(eye_x, eye_z) + GROUND_CLEARANCE - eye_y;
            if lift > 0.0 {
                state.focus.y += lift;
            }
        }
        state.yaw = pass.yaw;
        state.pitch = PASS_PITCH;
        state.distance = PASS_DISTANCE * zoom;
        return;
    }

    if let Some(shot) = director.shot {
        let target = shot
            .follow
            .and_then(|e| units.get(e).ok())
            .map_or(shot.pos, |(_, tf, ..)| tf.translation());
        let to = target - state.focus;
        let speed = (to.length() * GLIDE_RATE).det_clamp(GLIDE_MIN, GLIDE_MAX);
        let step = speed * dt;
        state.focus = if to.length() <= step {
            target
        } else {
            state.focus + to.normalize() * step
        };
        // Within the map, a little in from its edges.
        let inset = (max - min) * EDGE_INSET;
        let xz = state.focus.xz().clamp(min + inset, max - inset);
        state.focus.x = xz.x;
        state.focus.z = xz.y;
        ride_terrain(&mut state.focus);
    }
    let ease = (EASE * dt).det_min(1.0);
    director.yaw_rate_now += (director.yaw_rate - director.yaw_rate_now) * ease;
    state.yaw += director.yaw_rate_now * dt;
    // A slight sway on top of the shot's pitch, so a long hold breathes.
    let pitch = director.pitch + 0.03 * ops::sin(director.clock * 0.4);
    state.pitch += (pitch - state.pitch) * ease;
    state.distance += (director.distance * zoom - state.distance) * ease;
}

impl LowPass {
    /// A pass through `target` along a random heading, kept inside the
    /// map. The camera looks along the track from behind: with the
    /// eye at `focus + (sin yaw, ·, cos yaw)`, the ground forward is
    /// `(-sin yaw, -cos yaw)`, so `yaw = atan2(-d.x, -d.z)` for track
    /// direction `d`.
    fn across(target: Vec3, bounds: &MapBounds) -> Self {
        let angle = roll() * std::f32::consts::TAU;
        let d = Vec3::new(ops::cos(angle), 0.0, ops::sin(angle));
        let clamp = |p: Vec3| {
            let xz = p.xz().clamp(bounds.min.xz(), bounds.max.xz());
            Vec3::new(xz.x, p.y, xz.y)
        };
        Self {
            start: clamp(target - d * PASS_HALF_LENGTH),
            end: clamp(target + d * PASS_HALF_LENGTH),
            yaw: ops::atan2(-d.x, -d.z),
            elapsed: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, z: f32, team: u8, role: Role) -> Sighting {
        Sighting {
            pos: Vec3::new(x, 0.0, z),
            team,
            role,
            entity: Entity::PLACEHOLDER,
        }
    }

    const BOUNDS: (Vec2, Vec2) = (Vec2::ZERO, Vec2::splat(4096.0));

    fn best(candidates: &[Interest]) -> Interest {
        *candidates
            .iter()
            .max_by(|a, b| a.score.total_cmp(&b.score))
            .unwrap()
    }

    #[test]
    fn a_big_fight_outscores_everything_quiet() {
        let mut seen = vec![
            at(200.0, 200.0, 0, Role::Homebase),
            at(3900.0, 3900.0, 1, Role::Homebase),
            at(1000.0, 3000.0, 0, Role::Artillery),
            at(3000.0, 1000.0, 1, Role::NewBuilding),
        ];
        for i in 0..6 {
            seen.push(at(
                2000.0 + i as f32 * 20.0,
                2000.0,
                (i % 2) as u8,
                Role::Fighting,
            ));
        }
        let c = interests(
            &seen,
            &[Vec3::new(1500.0, 0.0, 1500.0)],
            BOUNDS,
            None,
            || 0.5,
        );
        let top = best(&c);
        assert_eq!(top.kind, Kind::Fight);
        assert!(top.pos.distance(Vec3::new(2050.0, 0.0, 2000.0)) < 1.0);
    }

    #[test]
    fn a_launch_is_followed() {
        let mut seen = vec![at(2000.0, 2000.0, 0, Role::Launch)];
        seen[0].entity = Entity::from_raw_u32(7).unwrap();
        let c = interests(&seen, &[], BOUNDS, None, || 0.5);
        let top = best(&c);
        assert_eq!(top.kind, Kind::Launch);
        assert_eq!(top.follow, seen[0].entity.into());
    }

    #[test]
    fn homebases_and_the_current_shot_score_low() {
        let seen = vec![
            at(500.0, 500.0, 0, Role::Homebase),
            at(520.0, 500.0, 0, Role::NewBuilding),
            at(2500.0, 2500.0, 1, Role::NewBuilding),
        ];
        let c = interests(&seen, &[], BOUNDS, None, || 0.5);
        let score = |x: f32| {
            c.iter()
                .find(|i| i.kind == Kind::NewBuilding && i.pos.x == x)
                .unwrap()
                .score
        };
        assert!(score(520.0) < score(2500.0) * 0.5);
        let c = interests(
            &seen,
            &[],
            BOUNDS,
            Some(Vec3::new(2500.0, 0.0, 2500.0)),
            || 0.5,
        );
        let open = c
            .iter()
            .find(|i| i.kind == Kind::NewBuilding && i.pos.x == 2500.0)
            .unwrap();
        assert!(open.score < NEW_BUILDING_SCORE * 0.5);
    }

    #[test]
    fn quiet_maps_roam_inside_the_map() {
        let seen = vec![
            at(200.0, 200.0, 0, Role::Homebase),
            at(3900.0, 3900.0, 1, Role::Homebase),
        ];
        let mut r = 0.0f32;
        let c = interests(&seen, &[], BOUNDS, None, || {
            r = (r + 0.37) % 1.0;
            r
        });
        assert!(c.iter().all(|i| i.kind == Kind::Roam));
        for i in &c {
            assert!(i.pos.x >= 600.0 && i.pos.x <= 3500.0 && i.pos.z >= 600.0 && i.pos.z <= 3500.0);
        }
    }

    #[test]
    fn frontlines_sit_between_the_armies() {
        let seen = vec![
            at(1000.0, 2000.0, 0, Role::Mobile),
            at(800.0, 2000.0, 0, Role::Mobile),
            at(2000.0, 2000.0, 1, Role::Mobile),
            at(2400.0, 2000.0, 1, Role::Mobile),
        ];
        let c = interests(&seen, &[], BOUNDS, None, || 0.5);
        let front = c.iter().find(|i| i.kind == Kind::Frontline).unwrap();
        assert!(front.pos.distance(Vec3::new(1500.0, 0.0, 2000.0)) < 1.0);
    }

    #[test]
    fn picks_follow_the_scores() {
        let mk = |score| Interest {
            pos: Vec3::ZERO,
            kind: Kind::Roam,
            score,
            follow: None,
        };
        let c = [mk(1.0), mk(3.0)];
        assert_eq!(pick_interest(&c, 0.2).unwrap().score, 1.0);
        assert_eq!(pick_interest(&c, 0.3).unwrap().score, 3.0);
        assert!(pick_interest(&[], 0.5).is_none());
    }
}
