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
use crate::map_events::hex_farm::{HexFarmState, HexFarmView};
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
    /// A Hex Farm tower rising out of the void.
    Rising,
    Roam,
}

impl Kind {
    /// Seconds a shot of this kind holds before the next pick.
    fn dwell(self) -> f32 {
        match self {
            Kind::Fight => 11.0,
            Kind::Launch => 9.0,
            Kind::Frontline => 9.0,
            Kind::Rising => 8.0,
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
/// A rising tower: as good as a mid-sized fight, and it cuts in.
const RISING_SCORE: f32 = 14.0;
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

/// What the map offers besides its units: datavents to look at, towers
/// rising (Hex Farm), and which ground is worth a roaming shot (not the
/// void, on maps with one).
pub(super) struct Scene<'a> {
    pub vents: &'a [Vec3],
    pub rising: &'a [Vec3],
    pub open: &'a dyn Fn(Vec2) -> bool,
}

impl Scene<'_> {
    /// No vents, nothing rising, all ground open.
    pub const EMPTY: Scene<'static> = Scene {
        vents: &[],
        rising: &[],
        open: &|_| true,
    };
}

/// Open-ground roam points get this many tries to land off the void.
const ROAM_TRIES: usize = 6;

/// Score everything worth a shot. `current` is where the camera looks
/// now; `rolls` supplies `0..1` randoms for the open-ground points.
pub(super) fn interests(
    seen: &[Sighting],
    scene: &Scene,
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

    for &pos in scene.rising {
        out.push(Interest {
            pos,
            kind: Kind::Rising,
            score: RISING_SCORE,
            follow: None,
        });
    }

    // The map itself: free datavents and open ground well inside it.
    let (min, max) = bounds;
    let inset = (max - min) * 0.15;
    let vents = scene.vents;
    for &v in vents {
        out.push(Interest {
            pos: v,
            kind: Kind::Roam,
            score: ROAM_SCORE / (1.0 + vents.len() as f32 / 8.0),
            follow: None,
        });
    }
    for _ in 0..ROAM_POINTS {
        let mut point = || min + inset + (max - min - 2.0 * inset) * Vec2::new(rolls(), rolls());
        let Some(p) = (0..ROAM_TRIES).map(|_| point()).find(|&p| (scene.open)(p)) else {
            continue;
        };
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
    /// The eased terrain floor under the focus.
    floor: Option<f32>,
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
/// The floor looks this far round the focus (elmos) for the highest
/// ground, and eases toward it at these rates (per second): up fast,
/// so a ridge is cleared in time, down slowly.
const FLOOR_RADIUS: f32 = 192.0;
const FLOOR_RISE: f32 = 3.0;
const FLOOR_FALL: f32 = 0.8;

/// The highest ground at `p` and at eight points `FLOOR_RADIUS` round it.
fn ground_floor(hm: &Heightmap, p: Vec2) -> f32 {
    (0..8)
        .map(|k| {
            let a = k as f32 * std::f32::consts::FRAC_PI_4;
            p + FLOOR_RADIUS * Vec2::new(ops::cos(a), ops::sin(a))
        })
        .fold(hm.sample(p.x, p.y), |m, q| m.det_max(hm.sample(q.x, q.y)))
}

/// The steepest the orbit tilts (rad above the ground plane) to look
/// over a wall between the eye and the shot, the step it tilts by, and
/// how fast it rises to that (per second; it settles back at [`EASE`]).
const PITCH_MAX: f32 = 1.3;
const PITCH_STEP: f32 = 0.05;
const PITCH_RISE: f32 = 2.5;
/// Points along the line from the shot to the eye that must clear the
/// terrain.
const SIGHT_SAMPLES: usize = 24;

/// The shallowest pitch from `pitch` up to [`PITCH_MAX`] whose line of
/// sight, from an eye `distance` away at `yaw` down to `focus`, clears
/// the terrain. The generated maps' lanes run between walls taller than
/// the eye stands over most shots (Circular Buffer's ring, Deadlock's
/// corridors): at the shot's own pitch the camera showed the side of a
/// wall instead of the fight behind it.
fn clear_pitch(hm: &Heightmap, focus: Vec3, yaw: f32, distance: f32, pitch: f32) -> f32 {
    let blocked = |p: f32| {
        let horizontal = distance * ops::cos(p);
        let eye = Vec3::new(
            horizontal * ops::sin(yaw),
            distance * ops::sin(p),
            horizontal * ops::cos(yaw),
        );
        (1..=SIGHT_SAMPLES).any(|k| {
            let q = focus + eye * (k as f32 / SIGHT_SAMPLES as f32);
            hm.sample(q.x, q.z) + GROUND_CLEARANCE > q.y
        })
    };
    let mut p = pitch;
    while p < PITCH_MAX && blocked(p) {
        p = (p + PITCH_STEP).det_min(PITCH_MAX);
    }
    p
}

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
    (hex_farm, hex_view): (Option<Res<HexFarmState>>, Option<Res<HexFarmView>>),
    mut cam: Query<&mut RtsCameraState, With<RtsCamera>>,
    dev: Res<DevOptions>,
) {
    let Ok(mut state) = cam.single_mut() else {
        return;
    };
    let dt = time.delta_secs();
    // Plain borrow: the fly-by and the floor are separate fields.
    let director = &mut *director;
    director.clock += dt;
    let zoom = dev.attract_distance.map_or(1.0, |d| d / 1250.0);
    // Terrain floor for every focus write below. Fights sit at their
    // ground height, and a straight glide or fly-by between fights at
    // different altitudes would tunnel through an intervening ridge
    // (Data_Cache_L1's mesas are ~1000 elmos tall) — minutes of solid
    // rock on screen. Riding `terrain + clearance` sweeps the camera
    // up and over instead; the camera smoothing eases the vertical.
    // The floor is the highest ground round the focus, eased: a point
    // sample jumps by a tower's height each time the focus crosses a
    // Hex Farm tower's edge (or a tower rises under it), and the camera
    // bobbed up and down with it.
    let ride_terrain = |focus: &mut Vec3, floor: &mut Option<f32>| {
        let Some(hm) = heightmap.as_deref() else {
            return;
        };
        let wanted = ground_floor(hm, focus.xz()) + GROUND_CLEARANCE;
        let eased = match *floor {
            Some(f) => {
                let rate = if wanted > f { FLOOR_RISE } else { FLOOR_FALL };
                f + (wanted - f) * (rate * dt).det_min(1.0)
            }
            None => wanted,
        };
        *floor = Some(eased);
        if focus.y < eased {
            focus.y = eased;
        }
    };
    // A freshly loaded map invalidates the old shot.
    if bounds.is_changed() {
        director.shot = None;
        director.pass = None;
        director.retarget_in = 0.0;
        director.floor = None;
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
    // Hex Farm: towers rising out of the void are worth a shot, and the
    // void itself (black) is not.
    let rising_points = || -> Vec<Vec3> {
        let (Some(state), Some(view)) = (hex_farm.as_deref(), hex_view.as_deref()) else {
            return Vec::new();
        };
        view.rising_towers()
            .map(|k| {
                let h = &state.farm.hexes[k];
                Vec3::new(h.x as f32, h.y as f32, h.z as f32)
            })
            .collect()
    };
    let open_ground = |p: Vec2| {
        hex_farm
            .as_deref()
            .is_none_or(|s| !s.farm.is_void(p.x as f64, p.y as f64))
    };

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
        let rising = rising_points();
        let scene = Scene {
            rising: &rising,
            ..Scene::EMPTY
        };
        cut = interests(&seen, &scene, (min, max), None, roll)
            .iter()
            .any(|i| {
                matches!(i.kind, Kind::Fight | Kind::Launch | Kind::Rising) && i.score >= CUT_SCORE
            });
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
        let (vents, rising) = (vent_points(), rising_points());
        let scene = Scene {
            vents: &vents,
            rising: &rising,
            open: &open_ground,
        };
        let candidates = interests(&seen, &scene, (min, max), current, roll);
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
        ride_terrain(&mut state.focus, &mut director.floor);
        // The eye trails the focus by the pass offset (same arithmetic
        // as `compute_transform_from_state`); lift the focus until the
        // eye clears the terrain it hangs over, too — a ridge behind
        // the track would otherwise swallow the camera mid-pass.
        if let Some(hm) = heightmap.as_deref() {
            let horizontal = PASS_DISTANCE * zoom * ops::cos(PASS_PITCH);
            let eye_y = state.focus.y + PASS_DISTANCE * zoom * ops::sin(PASS_PITCH);
            let eye_x = state.focus.x + horizontal * ops::sin(pass.yaw);
            let eye_z = state.focus.z + horizontal * ops::cos(pass.yaw);
            let lift = ground_floor(hm, Vec2::new(eye_x, eye_z)) + GROUND_CLEARANCE - eye_y;
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
        ride_terrain(&mut state.focus, &mut director.floor);
    }
    let ease = (EASE * dt).det_min(1.0);
    director.yaw_rate_now += (director.yaw_rate - director.yaw_rate_now) * ease;
    state.yaw += director.yaw_rate_now * dt;
    // A slight sway on top of the shot's pitch, so a long hold breathes.
    let mut pitch = director.pitch + 0.03 * ops::sin(director.clock * 0.4);
    let mut pitch_ease = ease;
    if let Some(hm) = heightmap.as_deref() {
        // Over a wall in the way, tilting up quickly.
        let clear = clear_pitch(hm, state.focus, state.yaw, state.distance, pitch);
        if clear > pitch {
            pitch = clear;
            if clear > state.pitch {
                pitch_ease = (PITCH_RISE * dt).det_min(1.0);
            }
        }
    }
    state.pitch += (pitch - state.pitch) * pitch_ease;
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

    #[test]
    fn the_orbit_tilts_to_look_over_a_wall() {
        // 1024 elmos square, flat at 0 but for a 400-tall wall along
        // z = 640..704, between a shot at z = 300 and an eye looking
        // from +z (yaw 0).
        let n = 129;
        let heights = (0..n * n)
            .map(|i| {
                if (80..=88).contains(&(i / n)) {
                    400.0
                } else {
                    0.0
                }
            })
            .collect();
        let hm = Heightmap::from_raw(heights, n, n);
        let focus = Vec3::new(512.0, GROUND_CLEARANCE, 300.0);
        let tilted = clear_pitch(&hm, focus, 0.0, 1000.0, 0.45);
        assert!(tilted > 0.45 && tilted < PITCH_MAX, "{tilted}");
        // From the other side nothing is in the way.
        let pi = std::f32::consts::PI;
        assert_eq!(clear_pitch(&hm, focus, pi, 1000.0, 0.45), 0.45);
    }

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
            &Scene {
                vents: &[Vec3::new(1500.0, 0.0, 1500.0)],
                ..Scene::EMPTY
            },
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
        let c = interests(&seen, &Scene::EMPTY, BOUNDS, None, || 0.5);
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
        let c = interests(&seen, &Scene::EMPTY, BOUNDS, None, || 0.5);
        let score = |x: f32| {
            c.iter()
                .find(|i| i.kind == Kind::NewBuilding && i.pos.x == x)
                .unwrap()
                .score
        };
        assert!(score(520.0) < score(2500.0) * 0.5);
        let c = interests(
            &seen,
            &Scene::EMPTY,
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
        let c = interests(&seen, &Scene::EMPTY, BOUNDS, None, || {
            r = (r + 0.37) % 1.0;
            r
        });
        assert!(c.iter().all(|i| i.kind == Kind::Roam));
        for i in &c {
            assert!(i.pos.x >= 600.0 && i.pos.x <= 3500.0 && i.pos.z >= 600.0 && i.pos.z <= 3500.0);
        }
    }

    #[test]
    fn rising_towers_are_shots_and_the_void_is_not() {
        let rising = [Vec3::new(2000.0, 300.0, 2000.0)];
        // Only the west half is ground; the east half is void.
        let open = |p: Vec2| p.x < 2000.0;
        let scene = Scene {
            rising: &rising,
            open: &open,
            ..Scene::EMPTY
        };
        let mut r = 0.0f32;
        let c = interests(&[], &scene, BOUNDS, None, || {
            r = (r + 0.37) % 1.0;
            r
        });
        let top = best(&c);
        assert_eq!(top.kind, Kind::Rising);
        assert!(top.score >= CUT_SCORE, "a rising tower cuts in");
        let roam: Vec<_> = c.iter().filter(|i| i.kind == Kind::Roam).collect();
        assert!(!roam.is_empty());
        assert!(
            roam.iter().all(|i| i.pos.x < 2000.0),
            "no roaming over the void"
        );
    }

    #[test]
    fn frontlines_sit_between_the_armies() {
        let seen = vec![
            at(1000.0, 2000.0, 0, Role::Mobile),
            at(800.0, 2000.0, 0, Role::Mobile),
            at(2000.0, 2000.0, 1, Role::Mobile),
            at(2400.0, 2000.0, 1, Role::Mobile),
        ];
        let c = interests(&seen, &Scene::EMPTY, BOUNDS, None, || 0.5);
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
