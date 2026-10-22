//! Seismic pings — `CUnit::DoSeismicPing` and `CSeismicGroundFlash`.
//!
//! A ground unit with an active move order pings on each of its slow
//! updates (every 15 frames, `Unit.cpp:1105`): a red cross on the
//! ground near it, growing by its FBI `SeismicSignature` per frame for
//! a second, offset by up to half the radar error (±48 elmos) per axis
//! so it tells roughly where, not exactly. The local player sees the
//! ping only for a unit outside their line of sight that is inside a
//! friendly seismic detector's range (`Unit.cpp:1488-1493`): the Worm's
//! 1024 ([`SeismicDetector`]). Terrain does not block the range, and a
//! detector still being built, dead or stunned has none
//! (`LosHandler.cpp:167-190`).
//!
//! Information only, and only for this machine's perspective: a ping
//! sets no sight or radar bits, nothing targets by it, and the sim never
//! reads one (upstream's Lua `UnitSeismicPing` callin has no listener in
//! KP, nor does its AI). So the whole thing lives on the rendering side:
//! the spawner runs in Animate after the visibility systems, and the
//! ping entity stays out of the state census (`net::state`'s "other"
//! bucket), since how many exist depends on who is watching.

use bevy::prelude::*;

use super::batch::FxQuadBatches;
use super::ceg::{CegRenderAssets, ground_axes, ground_flash_normal, ground_square};
use crate::interaction::ground_move::{GroundMover, Progress};
use crate::net::NetId;
use crate::rng;
use crate::sim::{DetFloat, GAME_SPEED, SimTick};
use crate::terrain::heightmap::Heightmap;
use crate::units::combat::{Dying, Stunned};
use crate::units::components::{TeamId, UnitType};
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::lifecycle::bookkeeping::SeismicDetector;
use crate::units::lifecycle::spawning::Emerging;
use crate::units::mechanics::cloak::FogEnabled;
use crate::units::player::LocalTeam;
use crate::units::teams::Alliances;

/// `CSeismicGroundFlash(.., ttl = 30, fade = 15, ..)`: a second on the
/// ground, opaque for the first half, fading over the second.
pub const PING_TTL_FRAMES: f32 = 30.0;
pub const PING_FADE_FRAMES: f32 = 15.0;
/// The cross's colour, `(0.8, 0, 0)`.
pub const PING_COLOR: [f32; 3] = [0.8, 0.0, 0.0];
/// `defBaseRadarErrorSize`: the ping lands within a box this wide
/// around the unit (`pos + (rx, 0, rz) · radarErrorSize`, `rx`, `rz` in
/// `[-0.5, 0.5)`).
pub const RADAR_ERROR_SIZE: f32 = 96.0;

/// One seismic ping on the ground, a cross that grows `size_growth`
/// per frame from nothing for [`PING_TTL_FRAMES`]. Local to this
/// machine's perspective; never sim state. Match content (the plugin
/// requires `MatchEntity` of it), cleared with the match.
#[derive(Component, Clone, Debug)]
pub struct SeismicPing {
    pub pos: Vec3,
    /// The quad's axes in the ground plane (`ceg::ground_axes`).
    pub axes: (Vec3, Vec3),
    /// `sizeGrowth`: the unit's seismic signature.
    pub size_growth: f32,
    /// Sim frames since spawn.
    pub age_frames: f32,
}

impl SeismicPing {
    /// `size += sizeGrowth` per update, from 0.
    pub fn half_size(&self) -> f32 {
        self.size_growth * self.age_frames
    }

    /// Opaque until `fade` frames are left, then linear to nothing
    /// (`GroundFlash.cpp:418-421`).
    pub fn alpha(&self) -> f32 {
        let left = PING_TTL_FRAMES - self.age_frames;
        (left / PING_FADE_FRAMES).det_clamp(0.0, 1.0)
    }
}

/// The offset of a ping from its unit on `frame`, in `[-48, 48)` per
/// axis: upstream draws two synced random numbers; here a hash of the
/// unit and the frame, so the pattern is as random to the eye and needs
/// no stream — the draw already depends on who is watching, which no
/// sim random stream may.
pub fn ping_offset(id: NetId, frame: u64) -> Vec2 {
    let seed = rng::splitmix64(u64::from(id.0) ^ frame.rotate_left(32));
    let mut state = (seed as u32) | 1;
    let rx = rng::next_f32(&mut state) - 0.5;
    let rz = rng::next_f32(&mut state) - 0.5;
    Vec2::new(rx, rz) * RADAR_ERROR_SIZE
}

/// `CUnit::SlowUpdate` → `DoSeismicPing` for the local player: every
/// hostile ground unit on its slow-update frame with an active move
/// order, a positive signature, hidden from the player and inside a
/// friendly, finished, live, unstunned [`SeismicDetector`]'s range
/// gets a [`SeismicPing`]. Runs after the visibility systems so
/// `Visibility` is this tick's line-of-sight verdict.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn spawn_seismic_pings(
    fog: Res<FogEnabled>,
    local: Res<LocalTeam>,
    alliances: Res<Alliances>,
    tick: Res<SimTick>,
    registry: Res<UnitRegistry>,
    heightmap: Option<Res<Heightmap>>,
    detectors: Query<
        (&TeamId, &SeismicDetector, &Transform),
        (Without<Dying>, Without<Emerging>, Without<Stunned>),
    >,
    movers: Query<
        (
            &NetId,
            &UnitType,
            &TeamId,
            &Transform,
            &GroundMover,
            &Visibility,
        ),
        (Without<Dying>, Without<Emerging>),
    >,
    mut coverage: Local<Vec<(Vec3, f32)>>,
    mut commands: Commands,
) {
    if !fog.0 {
        return;
    }
    coverage.clear();
    coverage.extend(
        detectors
            .iter()
            .filter(|(team, _, _)| alliances.friendly(team.0, local.0))
            .map(|(_, det, tf)| (tf.translation, det.radius * det.radius)),
    );
    if coverage.is_empty() {
        return;
    }
    for (id, kind, team, tf, mover, vis) in &movers {
        if !mover.is_slow_update()
            || mover.progress != Progress::Active
            || *vis != Visibility::Hidden
            || alliances.friendly(team.0, local.0)
        {
            continue;
        }
        let signature = registry.seismic_signature(kind.0);
        if signature <= 0.0 {
            continue;
        }
        let pos = tf.translation;
        let covered = coverage
            .iter()
            .any(|(det, r2)| crate::units::spatial::flat_dist_sq(pos, *det) <= *r2);
        if !covered {
            continue;
        }
        let off = ping_offset(*id, tick.0);
        let (x, z) = (pos.x + off.x, pos.z + off.y);
        let (pos, axes) = match heightmap.as_deref() {
            Some(hm) => {
                let at = hm.place(x, z) + Vec3::Y;
                let normal = ground_flash_normal(hm, at, signature * PING_TTL_FRAMES);
                (at, ground_axes(normal))
            }
            None => (Vec3::new(x, pos.y + 1.0, z), ground_axes(Vec3::Y)),
        };
        commands.spawn(SeismicPing {
            pos,
            axes,
            size_growth: signature,
            age_frames: 0.0,
        });
    }
}

/// Age every [`SeismicPing`] and push its cross for this frame; gone
/// once [`PING_TTL_FRAMES`] old (ages and dies without a material too,
/// so a headless world never piles them up).
pub(super) fn tick_seismic_pings(
    time: Res<Time>,
    mut pings: Query<(Entity, &mut SeismicPing)>,
    ceg_assets: Res<CegRenderAssets>,
    mut batches: ResMut<FxQuadBatches>,
    mut commands: Commands,
) {
    if pings.is_empty() {
        return;
    }
    let dt_frames = time.delta_secs() * GAME_SPEED;
    for (entity, mut ping) in &mut pings {
        if ping.age_frames >= PING_TTL_FRAMES {
            commands.entity(entity).despawn();
            continue;
        }
        let Some(material) = ceg_assets.seismic_material() else {
            ping.age_frames += dt_frames;
            continue;
        };
        let alpha = ping.alpha();
        let half = ping.half_size();
        if alpha > 0.0 && half > 0.0 {
            let [r, g, b] = PING_COLOR;
            batches.push_flat_quad(
                material,
                ground_square(ping.pos, ping.axes, half),
                [r, g, b, alpha],
            );
        }
        ping.age_frames += dt_frames;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_setup::SPECTATOR_TEAM;
    use crate::units::content::definitions::UnitKind;
    use bevy::ecs::system::RunSystemOnce;

    const BIT_AT: Vec3 = Vec3::new(500.0, 0.0, 0.0);

    fn flat_world() -> World {
        let mut world = World::new();
        world.insert_resource(FogEnabled(true));
        world.insert_resource(LocalTeam(0));
        world.insert_resource(Alliances::from_seats([(0, 0), (1, 1)]));
        world.insert_resource(SimTick(15));
        world.insert_resource(UnitRegistry::load());
        let w = 65;
        world.insert_resource(Heightmap::from_raw(vec![0.0; w * w], w, w));
        world
    }

    fn worm(world: &mut World, team: u8) -> Entity {
        world
            .spawn((
                UnitType(UnitKind::Worm),
                TeamId(team),
                SeismicDetector { radius: 1024.0 },
                Transform::IDENTITY,
            ))
            .id()
    }

    /// An enemy Bit at `at`, hidden, mid-order, on its slow-update
    /// frame.
    fn moving_bit(world: &mut World, at: Vec3) -> Entity {
        let reg = world.resource::<UnitRegistry>();
        let tf = Transform::from_translation(at);
        let mover = crate::interaction::movement::ground_mover_components(UnitKind::Bit, reg, &tf);
        let e = world
            .spawn((
                UnitType(UnitKind::Bit),
                TeamId(1),
                NetId(7),
                tf,
                Visibility::Hidden,
                mover,
            ))
            .id();
        let mut m = world.get_mut::<GroundMover>(e).unwrap();
        m.progress = Progress::Active;
        m.frame = 15;
        e
    }

    /// A named way of silencing the scene.
    type Tweak = (&'static str, fn(&mut World, Entity));

    fn pings(world: &mut World) -> Vec<SeismicPing> {
        world.flush();
        world.query::<&SeismicPing>().iter(world).cloned().collect()
    }

    fn spawn(world: &mut World) -> Vec<SeismicPing> {
        world.run_system_once(spawn_seismic_pings).unwrap();
        pings(world)
    }

    /// A hidden enemy walking inside a friendly Worm's 1024 pings once
    /// per slow update, near where it is, growing by its signature.
    #[test]
    fn a_hidden_mover_in_range_pings() {
        let mut world = flat_world();
        worm(&mut world, 0);
        moving_bit(&mut world, BIT_AT);
        let got = spawn(&mut world);
        assert_eq!(got.len(), 1);
        let p = &got[0];
        assert!((p.pos.x - BIT_AT.x).abs() < RADAR_ERROR_SIZE / 2.0, "{p:?}");
        assert!((p.pos.z - BIT_AT.z).abs() < RADAR_ERROR_SIZE / 2.0, "{p:?}");
        assert_eq!(p.pos.y, 1.0, "a hair above the ground");
        assert_eq!(p.size_growth, 1.0);
        assert_eq!(p.age_frames, 0.0);
        // Same unit, same frame: the same spot; the next slow update,
        // another.
        assert_eq!(ping_offset(NetId(7), 15), ping_offset(NetId(7), 15));
        assert_ne!(ping_offset(NetId(7), 15), ping_offset(NetId(7), 30));
    }

    /// Everything that keeps a unit quiet: it is in sight, it stands
    /// still, it is between slow updates, it is out of range, fog is
    /// off, it is a friend, it has no signature (a Worm).
    #[test]
    fn no_ping_without_every_condition() {
        let cases: &[Tweak] = &[
            ("visible", |w, bit| {
                *w.get_mut::<Visibility>(bit).unwrap() = Visibility::Visible;
            }),
            ("standing", |w, bit| {
                w.get_mut::<GroundMover>(bit).unwrap().progress = Progress::Done;
            }),
            ("between slow updates", |w, bit| {
                w.get_mut::<GroundMover>(bit).unwrap().frame = 7;
            }),
            ("out of range", |w, bit| {
                w.get_mut::<Transform>(bit).unwrap().translation = Vec3::new(1500.0, 0.0, 0.0);
            }),
            ("fog off", |w, _| {
                w.insert_resource(FogEnabled(false));
            }),
            ("a friend", |w, bit| {
                *w.get_mut::<TeamId>(bit).unwrap() = TeamId(0);
            }),
            ("a Worm, signature 0", |w, bit| {
                *w.get_mut::<UnitType>(bit).unwrap() = UnitType(UnitKind::Worm);
            }),
            ("a spectator", |w, _| {
                w.insert_resource(LocalTeam(SPECTATOR_TEAM));
            }),
        ];
        for (name, tweak) in cases {
            let mut world = flat_world();
            worm(&mut world, 0);
            let bit = moving_bit(&mut world, BIT_AT);
            tweak(&mut world, bit);
            assert!(spawn(&mut world).is_empty(), "{name}: pinged");
        }
    }

    /// A detector that is not yet built, is stunned, is dying or belongs
    /// to the enemy covers nothing.
    #[test]
    fn only_a_live_friendly_detector_listens() {
        let cases: &[Tweak] = &[
            ("emerging", |w, worm| {
                w.entity_mut(worm).insert(Emerging {
                    target_y: 0.0,
                    remaining: 1.0,
                    total: 1.0,
                    rally_point: None,
                    rally_then: None,
                    style: crate::units::lifecycle::spawning::EmergeStyle::Rise,
                });
            }),
            ("stunned", |w, worm| {
                w.entity_mut(worm).insert(Stunned { remaining: 1.0 });
            }),
            ("dying", |w, worm| {
                w.entity_mut(worm).insert(Dying { timer: 1.0 });
            }),
            ("the enemy's", |w, worm| {
                *w.get_mut::<TeamId>(worm).unwrap() = TeamId(1);
            }),
        ];
        for (name, tweak) in cases {
            let mut world = flat_world();
            let worm = worm(&mut world, 0);
            moving_bit(&mut world, BIT_AT);
            tweak(&mut world, worm);
            assert!(spawn(&mut world).is_empty(), "{name}: pinged");
        }
    }

    /// Opaque for the first half second, fading over the second, gone
    /// after 30 frames — material or not.
    #[test]
    fn a_ping_fades_and_dies() {
        let ping = |age| SeismicPing {
            pos: Vec3::ZERO,
            axes: ground_axes(Vec3::Y),
            size_growth: 2.0,
            age_frames: age,
        };
        assert_eq!(ping(10.0).alpha(), 1.0);
        assert_eq!(ping(22.5).alpha(), 0.5);
        assert_eq!(ping(30.0).alpha(), 0.0);
        assert_eq!(ping(10.0).half_size(), 20.0);

        let mut world = World::new();
        world.init_resource::<Time>();
        world.init_resource::<CegRenderAssets>();
        world.init_resource::<FxQuadBatches>();
        let young = world.spawn(ping(0.0)).id();
        let old = world.spawn(ping(30.0)).id();
        world
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f64(1.0 / 30.0));
        world.run_system_once(tick_seismic_pings).unwrap();
        world.flush();
        assert!(world.get::<SeismicPing>(old).is_none(), "30 frames: gone");
        let young = world.get::<SeismicPing>(young).expect("still here");
        assert!(
            (young.age_frames - 1.0).abs() < 1e-4,
            "aged without a material"
        );
    }
}
