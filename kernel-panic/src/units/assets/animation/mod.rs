//! Per-unit animation, hand-written in Rust.
//!
//! This replaces the old bytecode COB virtual machine: every unit kind's
//! animation is a small Rust driver in [`units`](self::units), selected at
//! spawn by [`driver_for`]. The shared machinery below is deliberately
//! dumb — it interpolates piece turns/moves/spins toward targets and
//! applies them to the piece entities — so a driver only says *what*
//! moves, never *how to step it*.
//!
//! # Angle convention
//!
//! Drivers express rotations in **Spring degrees** (the `<n>` notation in
//! the original `.bos` scripts) and translations in **elmos** (the `[n]`
//! notation). Internally:
//!
//! * a stored piece angle `θ` is applied as Bevy euler `(X=+θ, Y=+θ, Z=−θ)`
//!   — the Z mirror comes from the s3o→Bevy handedness flip plus the
//!   180° yaw on the model root;
//! * an X translation is mirrored (Spring's engine negates piece X offsets
//!   when loading s3o; our parser keeps them verbatim).
//!
//! Both rules apply uniformly to turns, spins and aim — unlike the old VM
//! path, which negated spin-X but not turn-X (Bit/Pointer/Dos visibly
//! rolled the wrong way) and double-negated Z spins (Trojan's ring spun
//! backwards).

pub mod units;

use std::collections::HashMap;
use std::sync::Arc;

use bevy::prelude::*;

use spring_cob::{CobFile, parse_cob};

use super::meshes::load_asset_from_disk;
use crate::interaction::movement::{MovePath, MoveTarget};
use crate::units::components::Faction;
use crate::units::content::definitions::UnitKind;
use crate::units::weapon_fx::{ExplosionEvent, PendingExplosions};

pub use units::driver_for;

/// Angle conversion for the degree-based driver helpers. `<n>` in a
/// `.bos` is n degrees (Scriptor scales by 65536/360 into COB angle
/// units); `[n]` is n elmos.
pub const DEG2RAD: f32 = std::f32::consts::PI / 180.0;

/// Spring's fixed runtime scale for linear and angular values in a
/// compiled `.cob` (65536). Pinned by the regression tests below —
/// drivers work in degrees/elmos directly, so this only guards the
/// historical "byte animations 2.5× too small" bug against a comeback.
#[allow(dead_code)]
pub const COBSCALE: f32 = spring_cob::COBSCALE as f32;

// ---------------------------------------------------------------------------
// Piece-table components resolved at spawn
// ---------------------------------------------------------------------------

/// Marks a Bevy entity as an animated piece child of a unit.
#[derive(Component)]
pub struct PieceIndex;

/// Index (into [`AnimRig::piece_entities`]) of the unit's primary weapon
/// muzzle — where beams and projectiles originate.
///
/// Resolved at spawn from per-kind piece names ([`muzzle_piece_names`]);
/// drivers that cycle muzzles (Byte's bp0..bp3, Flow's gp0..gp3) rewrite
/// [`AnimRig::muzzle`] at fire time and [`sync_muzzle_pieces`] mirrors the
/// value into this component.
#[derive(Component, Clone, Copy, Debug)]
pub struct MuzzlePiece(pub usize);

/// Candidate piece names searched in order at spawn when a kind has no
/// dedicated muzzle mapping (`gunpoint` covers Bit / Pointer / DOS /
/// Exploit, `bp0` the Byte's first barrel, the rest are generic
/// fallbacks for third-party models).
pub const MUZZLE_CANDIDATE_NAMES: &[&str] = &["gunpoint", "bp0", "flare", "barrel", "muzzle"];

/// Per-kind muzzle piece names, cycled from the first entry, or `None`
/// to fall back to [`MUZZLE_CANDIDATE_NAMES`].
pub fn muzzle_piece_names(kind: UnitKind) -> Option<&'static [&'static str]> {
    use UnitKind::*;
    Some(match kind {
        Byte => &["bp0", "bp1", "bp2", "bp3"],
        Flow => &["gp0", "gp1", "gp2", "gp3"],
        Worm => &["head"],
        Packet => &["gp"],
        Connection => &["gp2"],
        Obelisk | Assembler => &["tip"],
        Bug => &["nose"],
        Bit | Pointer | Dos | Exploit => &["gunpoint"],
        _ => return None,
    })
}

/// Cached COB piece index for the deploy/aim gun pivot (`gunbase`). Set at
/// spawn when the unit's script declares the piece; read every frame by
/// `aim_weapons_system` so it doesn't re-scan the piece-name table.
#[derive(Component, Clone, Copy, Debug)]
pub struct GunbasePiece(pub usize);

/// Cached COB piece index for the turret yaw pivot (`aimer`). Only the
/// Byte declares one: its whole firing assembly (rotor → blades →
/// barrels) swings on `AimWeapon1` while the body stays put.
#[derive(Component, Clone, Copy, Debug)]
pub struct AimerPiece(pub usize);

/// Cached COB piece index for the Connection's hatch (`body`). Read every
/// frame by `animate_connection_hatch` so it doesn't re-scan piece names.
#[derive(Component, Clone, Copy, Debug)]
pub struct HatchPiece(pub usize);

/// Cached parsed COB files, keyed by script filename. Kept only for the
/// piece-name tables (which mirror each s3o's piece tree) — no bytecode
/// is executed any more.
#[derive(Resource, Default)]
pub struct CobFileCache {
    files: HashMap<String, Option<Arc<CobFile>>>,
}

/// Load a COB file from disk, cached.
pub fn load_cob_cached(script: &str, cache: &mut CobFileCache) -> Option<Arc<CobFile>> {
    cache
        .files
        .entry(script.to_string())
        .or_insert_with(|| load_asset_from_disk(script, parse_cob).map(Arc::new))
        .clone()
}

/// Does the unit's COB script declare an `AimWeapon1`? Spawn-time
/// metadata (gates the [`AimScript`](crate::units::combat::AimScript)
/// component) read straight off the parsed function table.
pub fn declares_aim_weapon(cob: &CobFile) -> bool {
    cob.function_id("AimWeapon1").is_some()
}

// ---------------------------------------------------------------------------
// Rig: pieces + interpolation state + fx outbox
// ---------------------------------------------------------------------------

/// Piece axis, matching the `.bos` scripts' `x-axis` / `y-axis` / `z-axis`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    #[inline]
    fn index(self) -> usize {
        match self {
            Axis::X => 0,
            Axis::Y => 1,
            Axis::Z => 2,
        }
    }
}

/// One-shot visual effect requested by a driver, drained each frame by
/// [`animation_system`].
#[derive(Debug)]
pub enum FxEvent {
    Emit { piece: usize, sfx: i32 },
    /// Piece detonation. `severity` mirrors the upstream
    /// `explode ... type FALL/SHATTER/...` class (3 = FALL, 4 = SHATTER
    /// in the constant encoding we inherit); currently all classes
    /// render the same burst, so the value is carried for parity only.
    Explode {
        piece: usize,
        #[allow(dead_code)]
        severity: i32,
    },
    Show { piece: usize },
    Hide { piece: usize },
}

/// The per-unit animation hardware: piece table, interpolation arrays and
/// the effect outbox. Split from [`UnitAnimator`] so a driver call can
/// take `&mut AnimRig` and `&mut dyn UnitAnim` from one component
/// without fighting the borrow checker.
pub struct AnimRig {
    /// Piece names in COB declaration order (from the parsed script).
    pub piece_names: Vec<String>,
    /// Maps COB piece index → Bevy child entity.
    pub piece_entities: Vec<Entity>,
    /// Static base offsets from the s3o model (never modified by animation).
    pub piece_base_offsets: Vec<[f32; 3]>,
    /// Current animated rotation per piece (radians, [x,y,z]).
    pub piece_rotations: Vec<[f32; 3]>,
    /// Current animated translation offset per piece (elmos, [x,y,z]).
    pub piece_translations: Vec<[f32; 3]>,
    /// Target rotation per piece (for interpolated turns).
    pub target_rotations: Vec<[f32; 3]>,
    /// Turn speed per piece per axis (radians/sec, 0 = idle).
    pub turn_speeds: Vec<[f32; 3]>,
    /// Target translation per piece (for interpolated moves).
    pub target_translations: Vec<[f32; 3]>,
    /// Move speed per piece per axis (elmos/sec, 0 = idle).
    pub move_speeds: Vec<[f32; 3]>,
    /// Spin velocity per piece per axis (radians/sec).
    pub spin_speeds: Vec<[f32; 3]>,
    /// Weapon muzzle piece index (drivers may cycle it between shots).
    pub muzzle: usize,
    /// Effects queued by drivers, drained every frame.
    pub outbox: Vec<FxEvent>,
}

impl AnimRig {
    /// COB piece index for `name`, or `None` when the model has no such
    /// piece (animations targeting it become no-ops).
    pub fn piece(&self, name: &str) -> Option<usize> {
        self.piece_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(name))
    }

    /// Rotate `piece` toward `target_deg` at `speed_deg_per_sec` (`.bos`
    /// `turn <piece> to <axis> <t> speed <s>`). A speed of 0 snaps
    /// instantly (`turn ... now`).
    pub fn turn_deg(&mut self, piece: &str, axis: Axis, target_deg: f32, speed_deg_per_sec: f32) {
        self.turn_rad(
            piece,
            axis,
            target_deg * DEG2RAD,
            speed_deg_per_sec * DEG2RAD,
        );
    }

    /// Rotate `piece` toward `target_rad` at `speed_rad_per_sec`. A speed
    /// of 0 snaps instantly. Used for host-computed aim headings.
    pub fn turn_rad(&mut self, piece: &str, axis: Axis, target_rad: f32, speed_rad_per_sec: f32) {
        let Some(p) = self.piece(piece) else {
            return;
        };
        let a = axis.index();
        self.target_rotations[p][a] = target_rad;
        if speed_rad_per_sec <= 0.0 {
            self.piece_rotations[p][a] = target_rad;
            self.turn_speeds[p][a] = 0.0;
        } else {
            self.turn_speeds[p][a] = speed_rad_per_sec;
        }
    }

    /// Slide `piece` to `elmos` along `axis` at `speed` elmos/sec (`.bos`
    /// `move <piece> to <axis> [d] speed [s]`). A speed of 0 snaps
    /// instantly. X is mirrored — see the module docs.
    pub fn move_to(&mut self, piece: &str, axis: Axis, elmos: f32, speed: f32) {
        let Some(p) = self.piece(piece) else {
            return;
        };
        let a = axis.index();
        let target = if axis == Axis::X { -elmos } else { elmos };
        self.target_translations[p][a] = target;
        if speed <= 0.0 {
            self.piece_translations[p][a] = target;
            self.move_speeds[p][a] = 0.0;
        } else {
            self.move_speeds[p][a] = speed;
        }
    }

    /// Continuous spin in degrees/sec (`.bos` `spin <piece> around <axis>
    /// speed <n>`). Direction follows the same convention as turns.
    pub fn spin_dps(&mut self, piece: &str, axis: Axis, deg_per_sec: f32) {
        let Some(p) = self.piece(piece) else {
            return;
        };
        self.spin_speeds[p][axis.index()] = deg_per_sec * DEG2RAD;
    }

    /// Stop a spin (`.bos` `stop-spin <piece> around <axis>`).
    pub fn stop_spin(&mut self, piece: &str, axis: Axis) {
        let Some(p) = self.piece(piece) else {
            return;
        };
        self.spin_speeds[p][axis.index()] = 0.0;
    }

    pub fn emit(&mut self, piece: &str, sfx: i32) {
        if let Some(p) = self.piece(piece) {
            self.outbox.push(FxEvent::Emit { piece: p, sfx });
        }
    }

    pub fn explode(&mut self, piece: &str, severity: i32) {
        if let Some(p) = self.piece(piece) {
            self.outbox.push(FxEvent::Explode {
                piece: p,
                severity,
            });
        }
    }

    pub fn show(&mut self, piece: &str) {
        if let Some(p) = self.piece(piece) {
            self.outbox.push(FxEvent::Show { piece: p });
        }
    }

    pub fn hide(&mut self, piece: &str) {
        if let Some(p) = self.piece(piece) {
            self.outbox.push(FxEvent::Hide { piece: p });
        }
    }
}

// ---------------------------------------------------------------------------
// Driver trait + component
// ---------------------------------------------------------------------------

/// What a driver needs to know about the host world this frame.
#[derive(Clone, Copy, Debug)]
pub struct AnimCtx {
    pub dt: f32,
    /// BUILD_PERCENT_LEFT: 100 just spawned → 0 finished.
    pub build_percent: i32,
    /// The unit has a move order right now.
    pub moving: bool,
    /// The unit (as a factory) is currently producing something.
    pub producing: bool,
    /// Deploy cycle state for units with a `Deployable` component.
    pub deploy: Option<crate::units::combat::DeployState>,
    /// The unit currently has a live aim request (`AimTarget`).
    pub aim_active: bool,
}

impl AnimCtx {
    /// A context for event-driven driver calls (`start_moving`,
    /// `fire`, ...) that don't need world state beyond the rig.
    pub fn minimal() -> Self {
        Self {
            dt: 0.0,
            build_percent: 0,
            moving: false,
            producing: false,
            deploy: None,
            aim_active: false,
        }
    }
}

/// Per-unit animation logic. One impl per unit kind lives in
/// [`units`](self::units); every method defaults to a no-op so a driver
/// only implements what its unit actually does.
pub trait UnitAnim: Send + Sync + 'static {
    /// Run once when the unit spawns: initial poses, resting spins, the
    /// build-emerge pose. Corresponds to the `.bos` `Create()`.
    fn create(&mut self, _rig: &mut AnimRig, _ctx: AnimCtx) {}

    /// Per-frame tick: looping spins, choreography timers, idle effects.
    fn update(&mut self, _rig: &mut AnimRig, _ctx: AnimCtx) {}

    /// `.bos` `StartMoving()`.
    fn start_moving(&mut self, _rig: &mut AnimRig, _ctx: AnimCtx) {}

    /// `.bos` `StopMoving()`.
    fn stop_moving(&mut self, _rig: &mut AnimRig, _ctx: AnimCtx) {}

    /// `.bos` `AimWeapon1(h, p)` — steer the weapon toward the target.
    /// Returns `true` when the shot may commit (the upstream contract:
    /// return 1 ⇒ allowed to fire).
    fn aim(&mut self, _rig: &mut AnimRig, _heading: f32, _pitch: f32, _ctx: AnimCtx) -> bool {
        true
    }

    /// `.bos` `FireWeapon1()` — muzzle flash / recoil / barrel cycling.
    fn fire(&mut self, _rig: &mut AnimRig, _ctx: AnimCtx) {}

    /// `.bos` `Activate()` — factory opens for production.
    fn activate(&mut self, _rig: &mut AnimRig, _ctx: AnimCtx) {}

    /// `.bos` `Deactivate()` — factory closes.
    fn deactivate(&mut self, _rig: &mut AnimRig, _ctx: AnimCtx) {}

    /// `.bos` `Killed(severity, corpsetype)`.
    fn killed(&mut self, _rig: &mut AnimRig, _ctx: AnimCtx) {}

    /// True while a death or one-shot animation is still playing; the
    /// dying unit is despawned once this goes `false`.
    fn busy(&self) -> bool {
        false
    }
}

/// Component holding a unit's animation rig and its per-kind driver.
#[derive(Component)]
pub struct UnitAnimator {
    pub rig: AnimRig,
    /// `create()` has run (drivers are lazy-created on the first tick so
    /// spawn code doesn't need an `AnimCtx`).
    pub created: bool,
    pub driver: Box<dyn UnitAnim>,
}

// ---------------------------------------------------------------------------
// System
// ---------------------------------------------------------------------------

/// Tick every driver, interpolate the rigs, and apply piece transforms.
///
/// Also feeds `BUILD_PERCENT_LEFT` from the `Emerging` component into the
/// driver context (upstream value: 100 just spawned → 0 finished), and
/// fills in movement/production/deploy state from the host components.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn animation_system(
    time: Res<Time>,
    mut animators: Query<
        (
            Entity,
            &mut UnitAnimator,
            &Faction,
            &GlobalTransform,
            Option<&MoveTarget>,
            Option<&MovePath>,
            Option<&crate::units::lifecycle::production::Producer>,
            Option<&crate::units::combat::Deployable>,
            Option<&crate::units::lifecycle::spawning::Emerging>,
            Option<&crate::units::combat::AimTarget>,
        ),
        // Dying units are deliberately included: their `killed()`
        // drivers tick here (explode/hide choreography) until despawn.
    >,
    mut transforms: Query<(&mut Transform, &mut Visibility), With<PieceIndex>>,
    mut spawn_commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut death_assets: ResMut<DeathParticleAssets>,
    mut explosions: ResMut<PendingExplosions>,
) {
    let dt = time.delta_secs();

    for (
        _entity,
        mut animator,
        faction,
        unit_gtf,
        move_target,
        move_path,
        producer,
        deployable,
        emerging,
        aim_target,
    ) in &mut animators
    {
        let build_percent = emerging
            .map(|e| {
                if e.total > 0.0 {
                    ((e.remaining / e.total) * 100.0).round() as i32
                } else {
                    0
                }
            })
            .unwrap_or(0);

        let ctx = AnimCtx {
            dt,
            build_percent,
            moving: move_target.is_some() || move_path.is_some(),
            producing: producer.is_some_and(|p| p.current_production().is_some()),
            deploy: deployable.map(|d| d.state),
            aim_active: aim_target.is_some(),
        };

        let UnitAnimator {
            rig,
            created,
            driver,
            ..
        } = &mut *animator;
        if !*created {
            driver.create(rig, ctx);
            *created = true;
        }
        driver.update(rig, ctx);

        tick_rig(rig, dt);

        apply_and_drain(
            rig,
            *faction,
            unit_gtf,
            &mut transforms,
            &mut spawn_commands,
            &mut meshes,
            &mut materials,
            &mut death_assets,
            &mut explosions,
        );
    }
}

/// Advance a rig's interpolation: spins integrate continuously; turns and
/// moves step toward their targets and stop when they arrive.
pub fn tick_rig(rig: &mut AnimRig, dt: f32) {
    for p in 0..rig.piece_rotations.len() {
        for a in 0..3 {
            let spin = rig.spin_speeds[p][a];
            if spin != 0.0 {
                rig.piece_rotations[p][a] += spin * dt;
            }

            let speed = rig.turn_speeds[p][a];
            if speed > 0.0 {
                let target = rig.target_rotations[p][a];
                let current = rig.piece_rotations[p][a];
                let step = speed * dt;
                let diff = target - current;
                if diff.abs() <= step {
                    rig.piece_rotations[p][a] = target;
                    rig.turn_speeds[p][a] = 0.0;
                } else {
                    rig.piece_rotations[p][a] += step * diff.signum();
                }
            }

            let mspeed = rig.move_speeds[p][a];
            if mspeed > 0.0 {
                let target = rig.target_translations[p][a];
                let current = rig.piece_translations[p][a];
                let step = mspeed * dt;
                let diff = target - current;
                if diff.abs() <= step {
                    rig.piece_translations[p][a] = target;
                    rig.move_speeds[p][a] = 0.0;
                } else {
                    rig.piece_translations[p][a] += step * diff.signum();
                }
            }
        }
    }
}

/// Apply a rig's piece transforms to Bevy, then drain its fx outbox.
#[allow(clippy::too_many_arguments)]
fn apply_and_drain(
    rig: &mut AnimRig,
    faction: Faction,
    unit_gtf: &GlobalTransform,
    transforms: &mut Query<(&mut Transform, &mut Visibility), With<PieceIndex>>,
    spawn_commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    death_assets: &mut DeathParticleAssets,
    explosions: &mut PendingExplosions,
) {
    let piece_count = rig.piece_rotations.len().min(rig.piece_entities.len());
    for p in 0..piece_count {
        let Ok((mut tf, _)) = transforms.get_mut(rig.piece_entities[p]) else {
            continue;
        };
        let r = rig.piece_rotations[p];
        let t = rig.piece_translations[p];
        let base = rig.piece_base_offsets[p];
        // Stored Spring angles apply as Bevy euler (X=+θ, Y=+θ, Z=−θ) —
        // see the module docs for the handedness derivation.
        tf.rotation = Quat::from_euler(EulerRot::YXZ, r[1], r[0], -r[2]);
        tf.translation = Vec3::new(base[0] + t[0], base[1] + t[1], base[2] + t[2]);
    }

    for fx in rig.outbox.drain(..) {
        let in_range = |piece: usize| piece < rig.piece_entities.len();
        match fx {
            FxEvent::Show { piece } if in_range(piece) => {
                if let Ok((_, mut vis)) = transforms.get_mut(rig.piece_entities[piece]) {
                    *vis = Visibility::Inherited;
                }
            }
            FxEvent::Hide { piece } if in_range(piece) => {
                if let Ok((_, mut vis)) = transforms.get_mut(rig.piece_entities[piece]) {
                    *vis = Visibility::Hidden;
                }
            }
            FxEvent::Explode { piece, .. } if in_range(piece) => {
                if let Ok((_, mut vis)) = transforms.get_mut(rig.piece_entities[piece]) {
                    *vis = Visibility::Hidden;
                }
                let piece_world_pos = transforms
                    .get(rig.piece_entities[piece])
                    .map(|(tf, _)| unit_gtf.translation() + tf.translation)
                    .unwrap_or_else(|_| unit_gtf.translation());
                spawn_death_particle(
                    piece_world_pos,
                    faction,
                    death_assets,
                    spawn_commands,
                    meshes,
                    materials,
                );
            }
            FxEvent::Emit { piece, sfx } if in_range(piece) => {
                let piece_world_pos = transforms
                    .get(rig.piece_entities[piece])
                    .map(|(tf, _)| unit_gtf.translation() + tf.translation)
                    .unwrap_or_else(|_| unit_gtf.translation());
                dispatch_emit_sfx(sfx, piece_world_pos, faction, explosions);
            }
            _ => {}
        }
    }
}

/// Mirror driver-cycled muzzle indices ([`AnimRig::muzzle`]) into the
/// [`MuzzlePiece`] component that combat reads. Runs right after
/// [`animation_system`].
pub fn sync_muzzle_pieces(
    animators: Query<(Entity, &UnitAnimator), Changed<UnitAnimator>>,
    muzzle: Query<&MuzzlePiece>,
    mut commands: Commands,
) {
    for (entity, animator) in &animators {
        let current = muzzle.get(entity).ok().map(|m| m.0);
        if current != Some(animator.rig.muzzle) {
            commands
                .entity(entity)
                .insert(MuzzlePiece(animator.rig.muzzle));
        }
    }
}

// ---------------------------------------------------------------------------
// COB EmitSfx dispatch
// ---------------------------------------------------------------------------

/// Upstream sfx-type constants. COB scripts pass these as integer
/// literals, optionally ORed with a weapon index — e.g. `emit-sfx
/// SFX_DETONATE_WEAPON + 1 from head` detonates weapon 1 at the `head`
/// piece. We bucket the raw value into fire / detonate / generic and
/// spawn a correspondingly-sized explosion. Weapon-index / CEG-ID
/// demultiplexing is intentionally coarse: wiring every weapon-specific
/// CEG would require loading the full upstream particle-system table,
/// and the current goal is faithful-enough visible feedback.
const SFX_FIRE_WEAPON_BASE: i32 = 2048;
const SFX_DETONATE_WEAPON_BASE: i32 = 4096;
const SFX_CEG_BASE: i32 = 8192;
const SFX_GLOBAL_CEG_BASE: i32 = 16384;

/// Turn a raw SFX opcode arg into a faction-coloured explosion event.
///
/// * `0..SFX_FIRE_WEAPON_BASE`: generic SFX (wake, smoke, fire spark,
///   dust cloud). Tiny puff so movement/attack scripts don't strobe the
///   screen every step.
/// * `SFX_FIRE_WEAPON_BASE..SFX_DETONATE_WEAPON_BASE`: weapon-fire flash
///   at the piece (builder's build-beam emit, idle turret idle-flare).
///   Small pop.
/// * `SFX_DETONATE_WEAPON_BASE..SFX_CEG_BASE`: explicit weapon
///   detonation — the worm's `emit-sfx 4097 from head` in exploit.bos
///   uses this for its bite. Full explosion radius.
/// * `SFX_CEG_BASE..`: named-CEG spawn. Treated as a medium puff so
///   scripts that use it for ambient dust (assembler's construction
///   beam glows) still read, without faking the full particle system.
fn dispatch_emit_sfx(sfx_type: i32, pos: Vec3, faction: Faction, explosions: &mut PendingExplosions) {
    let (radius, intensity) = if sfx_type >= SFX_GLOBAL_CEG_BASE {
        // Global CEG (attached to world, not piece) — ignore; wiring it up
        // needs a registry we don't have.
        return;
    } else if sfx_type >= SFX_CEG_BASE {
        (6.0, 0.9)
    } else if sfx_type >= SFX_DETONATE_WEAPON_BASE {
        (32.0, 1.0)
    } else if sfx_type >= SFX_FIRE_WEAPON_BASE {
        (4.0, 0.8)
    } else {
        (2.5, 0.6)
    };

    let base = faction.rgb_f32();
    let rgb = [
        base[0] * intensity,
        base[1] * intensity,
        base[2] * intensity,
    ];

    explosions.events.push(ExplosionEvent {
        pos,
        rgb,
        radius,
        ceg_name: String::new(),
    });
}

// ---------------------------------------------------------------------------
// Death particle effects
// ---------------------------------------------------------------------------

/// Brief particle burst spawned when a piece explodes.
#[derive(Component)]
pub struct DeathParticle {
    pub lifetime: f32,
    pub max_lifetime: f32,
}

/// Lazily-populated shared mesh and per-faction emissive materials for
/// death particles. `spawn_death_particle` seeds the entry the first time
/// it needs a given faction color; every subsequent particle re-uses the
/// same Handle. Mirrors the pattern used by `BuildSparkleAssets` /
/// `ImpactBurstAssets`.
#[derive(Resource, Default)]
pub struct DeathParticleAssets {
    pub mesh: Option<Handle<Mesh>>,
    pub system: Option<Handle<StandardMaterial>>,
    pub hacker: Option<Handle<StandardMaterial>>,
    pub network: Option<Handle<StandardMaterial>>,
}

impl DeathParticleAssets {
    fn material_for(
        &mut self,
        faction: Faction,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        let slot = match faction {
            Faction::System => &mut self.system,
            Faction::Hacker => &mut self.hacker,
            Faction::Network => &mut self.network,
        };
        slot.get_or_insert_with(|| {
            let color = LinearRgba::from(faction.color());
            materials.add(StandardMaterial {
                base_color: Color::from(color),
                emissive: color * 6.0,
                unlit: true,
                alpha_mode: AlphaMode::Add,
                ..default()
            })
        })
        .clone()
    }
}

/// Spawn a brief expanding, fading burst at `pos` in the unit's faction color.
///
/// Mesh and per-faction material are owned by [`DeathParticleAssets`] so
/// every burst reuses the same Handle — an arena full of dying units
/// doesn't each mint a fresh sphere and material asset.
fn spawn_death_particle(
    pos: Vec3,
    faction: Faction,
    assets: &mut DeathParticleAssets,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) {
    let mesh = assets
        .mesh
        .get_or_insert_with(|| meshes.add(Sphere::new(1.0).mesh().ico(2).unwrap()))
        .clone();
    let material = assets.material_for(faction, materials);

    commands.spawn((
        DeathParticle {
            lifetime: 0.0,
            max_lifetime: 0.5,
        },
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(pos).with_scale(Vec3::splat(2.0)),
    ));
}

/// Expand and fade death particles, then despawn them. The shared
/// material means we can't mutate alpha per-particle, so "fade" is done
/// purely through the scale curve — the sphere grows, peaks, then
/// shrinks back to zero in the last ~20% of its life, disappearing
/// cleanly. The material itself stays at full emissive.
pub fn decay_death_particles(
    time: Res<Time>,
    mut query: Query<(Entity, &mut DeathParticle, &mut Transform)>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (entity, mut particle, mut transform) in &mut query {
        particle.lifetime += dt;
        let t = (particle.lifetime / particle.max_lifetime).clamp(0.0, 1.0);

        if t >= 1.0 {
            commands.entity(entity).despawn();
            continue;
        }

        // Grow then shrink: peaks at t=0.8, collapses to zero by t=1.0.
        let peak_scale = 2.0 + 20.0 * t.min(0.8) / 0.8;
        let tail = if t > 0.8 { (1.0 - t) / 0.2 } else { 1.0 };
        transform.scale = Vec3::splat(peak_scale * tail);
    }
}

#[cfg(test)]
mod cobscale_regression {
    //! Regression guards for the "byte animations are 2.5× too small"
    //! bug. The `.bos` bracket conventions these pin: `<n>` is n degrees,
    //! `[n]` is n elmos compiled at Scriptor's linear constant.

    use super::COBSCALE;

    #[test]
    fn cobscale_is_65536() {
        assert!((COBSCALE - 65536.0).abs() < 1e-6);
    }

    #[test]
    fn half_circle_is_32768_cob_units() {
        // Spring's TA-angle unit: 65536 = 2π. Half circle = 32768.
        let half_circle = 32768.0 * std::f32::consts::TAU / COBSCALE;
        assert!((half_circle - std::f32::consts::PI).abs() < 1e-3);
    }
}
