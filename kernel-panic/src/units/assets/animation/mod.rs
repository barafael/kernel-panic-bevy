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

/// Kani model-checking harnesses proving equivalence with the retired
/// bytecode-VM pipeline. Compiled only under `cargo kani` (`cfg(kani)`)
/// — invisible to normal builds, tests and wasm.
#[cfg(kani)]
mod proofs;

use bevy::prelude::*;

use crate::interaction::movement::{MovePath, MoveTarget};
use crate::units::components::Faction;
use crate::units::content::definitions::UnitKind;
use crate::units::weapon_fx::{ExplosionEvent, PendingExplosions};

pub use units::{driver_for, has_aim_weapon, piece_names};

/// Angle conversion for the degree-based driver helpers. `<n>` in a
/// `.bos` is n degrees (Scriptor scales by 65536/360 into COB angle
/// units); `[n]` is n elmos.
pub const DEG2RAD: f32 = std::f32::consts::PI / 180.0;

/// Degree→radian in one call — replaces the per-driver `rad2deg`-style
/// helpers and the `n * super::super::DEG2RAD` spelling.
pub const fn deg2rad(deg: f32) -> f32 {
    deg * DEG2RAD
}

/// Sentinel piece index for pieces a unit's model doesn't have. Drivers
/// bind their piece indices once (in [`UnitAnim::bind`]); animating a
/// missing piece must be a harmless no-op, exactly like the old
/// name-based `piece() == None` path — so every index-taking primitive
/// bounds-checks and bails on this value.
pub const PIECE_MISSING: usize = usize::MAX;

/// Spring's fixed runtime scale for linear and angular values in a
/// compiled `.cob` (65536). Pinned by the regression tests below —
/// drivers work in degrees/elmos directly, so this only guards the
/// historical "byte animations 2.5× too small" bug against a comeback.
#[allow(dead_code)]
pub const COBSCALE: f32 = 65536.0;

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
    Emit {
        piece: usize,
        kind: SfxKind,
    },
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

/// What a driver wants an `emit-sfx` to look like, replacing the raw COB
/// integer opcodes (`2048`, `4097`, ...) drivers used to push. Upstream
/// scripts OR a weapon index into the constant (`emit-sfx
/// SFX_DETONATE_WEAPON + 1`); the current renderer buckets by range only,
/// so the index is dropped here rather than carried dead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SfxKind {
    /// `0..SFX_FIRE_WEAPON_BASE` — generic SFX (wake, smoke, ground
    /// spark, dust). Tiny puff so movement/attack scripts don't strobe.
    Puff,
    /// `SFX_FIRE_WEAPON_BASE..SFX_DETONATE_WEAPON_BASE` — weapon-fire
    /// flash at the piece (builder beams, idle turret flares). Small pop.
    FireFlash,
    /// `SFX_DETONATE_WEAPON_BASE..SFX_CEG_BASE` — explicit weapon
    /// detonation (the worm's bite). Full explosion radius. No driver
    /// emits it yet — upstream's `emit-sfx 4097 from head` (exploit.bos)
    /// is the first candidate when per-weapon detonation is wired.
    #[allow(dead_code)]
    Detonate,
    /// `SFX_CEG_BASE..` — named-CEG spawn, treated as a medium puff so
    /// ambient dust still reads without faking the particle system. No
    /// driver emits it yet (named CEGs render through `weapon_fx`
    /// instead); kept so the opcode space stays fully encoded.
    #[allow(dead_code)]
    Ceg,
}

/// The per-unit animation hardware: piece table, interpolation arrays and
/// the effect outbox. Split from [`UnitAnimator`] so a driver call can
/// take `&mut AnimRig` and `&mut dyn UnitAnim` from one component
/// without fighting the borrow checker.
pub struct AnimRig {
    /// Piece names in COB declaration order (from the static per-kind
    /// table). `&'static` — the old `Vec<String>` forced a heap string
    /// per piece per unit and case-insensitive compares on every lookup;
    /// drivers now bind indices once and this table is only consulted
    /// by [`AnimRig::piece`] (binding + tests).
    pub piece_names: &'static [&'static str],
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
    /// Movement gate multiplier in [0, 1], written by the driver every
    /// frame. A driver holds this at 0 while a fold/unfold choreography
    /// must complete before the unit may drive (Byte folds to move);
    /// the movement system multiplies its step by it. Defaults to 1.
    pub move_gate: f32,
    /// Effects queued by drivers, drained every frame.
    pub outbox: Vec<FxEvent>,
    /// Whether the rig's piece transforms changed since
    /// [`animation_system`] last applied them. Set by every primitive
    /// write and by [`tick_rig`]; lets `apply_and_drain` skip the
    /// per-piece `Quat::from_euler` + transform write for idle rigs —
    /// an army standing still costs nothing to animate.
    pub dirty: bool,
}

impl AnimRig {
    /// COB piece index for `name`, or `None` when the model has no such
    /// piece. Called from [`UnitAnim::bind`] at spawn (once per driver)
    /// and by tests — never on the per-frame path.
    pub fn piece(&self, name: &str) -> Option<usize> {
        self.piece_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(name))
    }

    /// Resolve a piece name to a bind-time index, collapsing "missing
    /// piece" into [`PIECE_MISSING`] so drivers can store plain `usize`s.
    pub fn bind_piece(&self, name: &str) -> usize {
        self.piece(name).unwrap_or(PIECE_MISSING)
    }

    /// Bounds-check for the bound-index primitives: `PIECE_MISSING` and
    /// out-of-range indices become no-ops.
    #[inline]
    fn live(&self, piece: usize) -> bool {
        piece < self.piece_rotations.len()
    }

    /// Rotate `piece` toward `target_deg` at `speed_deg_per_sec` (`.bos`
    /// `turn <piece> to <axis> <t> speed <s>`). A speed of 0 snaps
    /// instantly (`turn ... now`).
    pub fn turn_deg(&mut self, piece: usize, axis: Axis, target_deg: f32, speed_deg_per_sec: f32) {
        self.turn_rad(
            piece,
            axis,
            target_deg * DEG2RAD,
            speed_deg_per_sec * DEG2RAD,
        );
    }

    /// Rotate `piece` toward `target_rad` at `speed_rad_per_sec`. A speed
    /// of 0 snaps instantly. Used for host-computed aim headings.
    pub fn turn_rad(&mut self, piece: usize, axis: Axis, target_rad: f32, speed_rad_per_sec: f32) {
        if !self.live(piece) {
            return;
        }
        let a = axis.index();
        self.target_rotations[piece][a] = target_rad;
        if speed_rad_per_sec <= 0.0 {
            self.piece_rotations[piece][a] = target_rad;
            self.turn_speeds[piece][a] = 0.0;
        } else {
            self.turn_speeds[piece][a] = speed_rad_per_sec;
        }
        self.dirty = true;
    }

    /// Slide `piece` to `elmos` along `axis` at `speed` elmos/sec (`.bos`
    /// `move <piece> to <axis> [d] speed [s]`). A speed of 0 snaps
    /// instantly. X is mirrored — see the module docs.
    pub fn move_to(&mut self, piece: usize, axis: Axis, elmos: f32, speed: f32) {
        if !self.live(piece) {
            return;
        }
        let a = axis.index();
        let target = if axis == Axis::X { -elmos } else { elmos };
        self.target_translations[piece][a] = target;
        if speed <= 0.0 {
            self.piece_translations[piece][a] = target;
            self.move_speeds[piece][a] = 0.0;
        } else {
            self.move_speeds[piece][a] = speed;
        }
        self.dirty = true;
    }

    /// True when `piece` has reached its target along `axis` (the
    /// `.bos` `wait-for-turn`/`wait-for-move` conditions). Rotation and
    /// translation are both checked against the same axis index — the
    /// non-moving component sits at its (unchanged) target, so it never
    /// falsifies the check. Missing pieces are "at target" (nothing to
    /// wait for).
    pub fn at_target(&self, piece: usize, axis: Axis) -> bool {
        if !self.live(piece) {
            return true;
        }
        let a = axis.index();
        const EPS: f32 = 1e-4;
        (self.piece_rotations[piece][a] - self.target_rotations[piece][a]).abs() < EPS
            && (self.piece_translations[piece][a] - self.target_translations[piece][a]).abs() < EPS
    }

    /// Continuous spin in degrees/sec (`.bos` `spin <piece> around <axis>
    /// speed <n>`). Direction follows the same convention as turns.
    pub fn spin_dps(&mut self, piece: usize, axis: Axis, deg_per_sec: f32) {
        if !self.live(piece) {
            return;
        }
        self.spin_speeds[piece][axis.index()] = deg_per_sec * DEG2RAD;
        self.dirty = true;
    }

    /// Stop a spin (`.bos` `stop-spin <piece> around <axis>`).
    pub fn stop_spin(&mut self, piece: usize, axis: Axis) {
        if !self.live(piece) {
            return;
        }
        self.spin_speeds[piece][axis.index()] = 0.0;
    }

    pub fn emit(&mut self, piece: usize, kind: SfxKind) {
        if self.live(piece) {
            self.outbox.push(FxEvent::Emit { piece, kind });
        }
    }

    pub fn explode(&mut self, piece: usize, severity: i32) {
        if self.live(piece) {
            self.outbox.push(FxEvent::Explode { piece, severity });
        }
    }

    pub fn show(&mut self, piece: usize) {
        if self.live(piece) {
            self.outbox.push(FxEvent::Show { piece });
        }
    }

    pub fn hide(&mut self, piece: usize) {
        if self.live(piece) {
            self.outbox.push(FxEvent::Hide { piece });
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
    /// The unit is under an explicit attack order (attack-target /
    /// attack-ground). Foldable units must treat this as a want-open
    /// signal even when no enemy unit is auto-acquired to aim at (a
    /// ground order may target empty terrain) — otherwise a click gate
    /// on the fold state would deadlock against the missing AimTarget.
    pub attack_ordering: bool,
    /// The unit is still emerging (has an `Emerging` component). Drivers
    /// must run their build-emerge pose only while this is set — past
    /// emergence, the snap-to-rest would fight later choreography on
    /// the same piece (e.g. the byte's base lift on unfold).
    pub emerging: bool,
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
            attack_ordering: false,
            emerging: false,
        }
    }
}

/// Per-unit animation logic. One impl per unit kind lives in
/// [`units`](self::units); every method defaults to a no-op so a driver
/// only implements what its unit actually does.
pub trait UnitAnim: Send + Sync + 'static {
    /// Resolve this driver's piece indices from the rig, once at spawn.
    /// Called before any other method, so even a unit killed on its
    /// spawn frame can address its pieces. Store the results (using
    /// [`PIECE_MISSING`] for pieces the model lacks) and use the
    /// index-taking rig primitives from then on — the old
    /// name-per-call API forced a `format!` + case-insensitive linear
    /// scan on every primitive call.
    fn bind(&mut self, _rig: &AnimRig) {}

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
    /// dying unit is despawned once this goes `false` (or the
    /// [`DEATH_ANIM_TIMEOUT`](crate::units::combat::lifecycle) backstop
    /// fires). Drivers with death choreography override this alongside
    /// [`UnitAnim::killed`] so the corpse lingers for the burst instead
    /// of popping out of existence on the death frame.
    fn busy(&self) -> bool {
        false
    }

    /// For units with a fold cycle: `Some(true)` while fully unfolded,
    /// `Some(false)` otherwise. `None` for units without one. Read by
    /// host systems that mirror fold state (the byte's open-armor
    /// damage discount).
    fn is_open(&self) -> Option<bool> {
        None
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

/// Everything [`animation_system`] needs to push rig state into the
/// render world: piece transforms, spawn commands for death particles,
/// the shared death-particle assets, and the explosion outbox. Grouped
/// into one `SystemParam` so the system signature stays readable — the
/// old signature was a 12-tuple query plus six loose params, suppressed
/// with `clippy::too_many_arguments`.
#[derive(bevy::ecs::system::SystemParam)]
pub struct AnimFxOut<'w, 's> {
    pub transforms:
        Query<'w, 's, (&'static mut Transform, &'static mut Visibility), With<PieceIndex>>,
    pub commands: Commands<'w, 's>,
    pub meshes: ResMut<'w, Assets<Mesh>>,
    pub materials: ResMut<'w, Assets<StandardMaterial>>,
    pub death_assets: ResMut<'w, DeathParticleAssets>,
    pub explosions: ResMut<'w, PendingExplosions>,
}

/// The driver-tick query plus its per-frame world inputs. Dying units are
/// deliberately included: their `killed()` drivers tick here (explode/hide
/// choreography) until despawn.
#[derive(bevy::ecs::system::SystemParam)]
pub struct AnimDrivers<'w, 's> {
    pub animators: Query<
        'w,
        's,
        (
            Entity,
            &'static mut UnitAnimator,
            &'static Faction,
            &'static GlobalTransform,
            Option<&'static MoveTarget>,
            Option<&'static MovePath>,
            Option<&'static crate::units::lifecycle::production::Producer>,
            Option<&'static crate::units::combat::Deployable>,
            Option<&'static crate::units::lifecycle::spawning::Emerging>,
            Option<&'static crate::units::combat::AimTarget>,
            Option<&'static crate::units::combat::AttackGroundOrder>,
            Option<&'static crate::units::combat::AttackTargetOrder>,
        ),
    >,
}

/// Tick every driver, interpolate the rigs, and apply piece transforms.
///
/// Also feeds `BUILD_PERCENT_LEFT` from the `Emerging` component into the
/// driver context (upstream value: 100 just spawned → 0 finished), and
/// fills in movement/production/deploy state from the host components.
pub fn animation_system(time: Res<Time>, mut drivers: AnimDrivers, mut fx: AnimFxOut) {
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
        attack_ground,
        attack_target,
    ) in &mut drivers.animators
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
            attack_ordering: attack_ground.is_some() || attack_target.is_some(),
            emerging: emerging.is_some(),
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

        apply_and_drain(rig, *faction, unit_gtf, &mut fx);
    }
}

/// Advance a rig's interpolation: spins integrate continuously; turns and
/// moves step toward their targets and stop when they arrive. Flags the
/// rig dirty whenever anything actually moved.
pub fn tick_rig(rig: &mut AnimRig, dt: f32) {
    let mut changed = false;
    for p in 0..rig.piece_rotations.len() {
        for a in 0..3 {
            let spin = rig.spin_speeds[p][a];
            if spin != 0.0 {
                rig.piece_rotations[p][a] += spin * dt;
                changed = true;
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
                changed = true;
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
                changed = true;
            }
        }
    }
    if changed {
        rig.dirty = true;
    }
}

/// Apply a rig's piece transforms to Bevy, then drain its fx outbox.
/// The transform write is gated on the rig's dirty flag — an idle rig
/// (no in-flight interpolation, spinning pieces, or fresh commands)
/// skips the per-piece euler compose entirely.
fn apply_and_drain(
    rig: &mut AnimRig,
    faction: Faction,
    unit_gtf: &GlobalTransform,
    fx: &mut AnimFxOut,
) {
    if rig.dirty {
        let piece_count = rig.piece_rotations.len().min(rig.piece_entities.len());
        for p in 0..piece_count {
            let Ok((mut tf, _)) = fx.transforms.get_mut(rig.piece_entities[p]) else {
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
        rig.dirty = false;
    }

    for fx_event in rig.outbox.drain(..) {
        let in_range = |piece: usize| piece < rig.piece_entities.len();
        match fx_event {
            FxEvent::Show { piece } if in_range(piece) => {
                if let Ok((_, mut vis)) = fx.transforms.get_mut(rig.piece_entities[piece]) {
                    *vis = Visibility::Inherited;
                }
            }
            FxEvent::Hide { piece } if in_range(piece) => {
                if let Ok((_, mut vis)) = fx.transforms.get_mut(rig.piece_entities[piece]) {
                    *vis = Visibility::Hidden;
                }
            }
            FxEvent::Explode { piece, .. } if in_range(piece) => {
                if let Ok((_, mut vis)) = fx.transforms.get_mut(rig.piece_entities[piece]) {
                    *vis = Visibility::Hidden;
                }
                let piece_world_pos = fx
                    .transforms
                    .get(rig.piece_entities[piece])
                    .map(|(tf, _)| unit_gtf.translation() + tf.translation)
                    .unwrap_or_else(|_| unit_gtf.translation());
                spawn_death_particle(
                    piece_world_pos,
                    faction,
                    &mut fx.death_assets,
                    &mut fx.commands,
                    &mut fx.meshes,
                    &mut fx.materials,
                );
            }
            FxEvent::Emit { piece, kind } if in_range(piece) => {
                let piece_world_pos = fx
                    .transforms
                    .get(rig.piece_entities[piece])
                    .map(|(tf, _)| unit_gtf.translation() + tf.translation)
                    .unwrap_or_else(|_| unit_gtf.translation());
                dispatch_emit_sfx(kind, piece_world_pos, faction, &mut fx.explosions);
            }
            _ => {}
        }
    }
}

/// Mirror driver-cycled muzzle indices ([`AnimRig::muzzle`]) into the
/// [`MuzzlePiece`] component that combat reads. Runs right after
/// [`animation_system`].
///
/// Why no `Changed<UnitAnimator>` filter: `animation_system` takes the
/// animator by `DerefMut` for every unit every frame (the rig ticks even
/// when idle), so the change-detection flag fires unconditionally and the
/// filter never filters. The work here is one component get + compare per
/// unit — cheap enough to just run.
pub fn sync_muzzle_pieces(
    animators: Query<(Entity, &UnitAnimator)>,
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
// EmitSfx dispatch
// ---------------------------------------------------------------------------

/// Turn a typed [`SfxKind`] into a faction-coloured explosion event.
///
/// The raw COB opcode → kind decoding lives in [`SfxKind`]; this is the
/// rendering half. Sizes follow the old range-bucketing so the visual
/// language is unchanged (see the [`SfxKind`] docs for the upstream
/// constant ranges each kind stands in for).
fn dispatch_emit_sfx(kind: SfxKind, pos: Vec3, faction: Faction, explosions: &mut PendingExplosions) {
    let (radius, intensity) = match kind {
        SfxKind::Ceg => (6.0, 0.9),
        SfxKind::Detonate => (32.0, 1.0),
        SfxKind::FireFlash => (4.0, 0.8),
        SfxKind::Puff => (2.5, 0.6),
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
