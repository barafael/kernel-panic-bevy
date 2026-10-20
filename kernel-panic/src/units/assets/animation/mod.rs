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

use crate::sim::{DetFloat, sq};
use bevy::prelude::*;

use crate::interaction::movement::{MovePath, MoveTarget};
use crate::units::components::Faction;
use crate::units::content::definitions::UnitKind;
use crate::units::team_colors::{TeamColor, UnitColor};
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

/// How close (radians) an aim turn must be for [`AnimRig::turn_settled`]
/// to report the script's `wait-for-turn` done: ~5°, the same slack the
/// host-side aim gates allow (`combat::aim::AIM_HEADING_TOLERANCE` /
/// `AIM_PITCH_TOLERANCE`), so driver and host agree on "on target". It
/// is under one tick's turn for the fast turrets (Packet 24°, Flow 16°,
/// Byte 9°) and well inside what the shots correct (homing missiles,
/// splash).
pub const WAIT_FOR_TURN_TOLERANCE: f32 = 0.09;

/// Spring's fixed runtime scale for linear and angular values in a
/// compiled `.cob` (65536). Pinned by the regression tests below —
/// drivers work in degrees/elmos directly, so this only guards the
/// historical "byte animations 2.5× too small" bug against a comeback.
#[cfg(test)]
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

/// A piece's weapon emit point and direction in its own local space —
/// upstream `S3DModelPiece::GetEmitPos` / `GetEmitDir`
/// (`Rendering/Models/3DModelPiece.cpp:60-77`), which
/// `CWeapon::UpdateWeaponVectors` reads through `GetEmitDirPos` for the
/// `QueryWeapon` piece: an empty piece (the usual `gp` / `gunpoint`
/// marker) emits from its origin along local +Z; a one-vertex piece
/// from its origin toward that vertex; otherwise from vertex 0 toward
/// vertex 1. Attached to every model piece entity at spawn.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct PieceEmit {
    pub pos: Vec3,
    pub dir: Vec3,
}

impl Default for PieceEmit {
    fn default() -> Self {
        Self {
            pos: Vec3::ZERO,
            dir: Vec3::Z,
        }
    }
}

impl PieceEmit {
    pub fn from_vertices(vertices: &[[f32; 3]]) -> Self {
        match vertices {
            [] => Self::default(),
            [v0] => Self {
                pos: Vec3::ZERO,
                dir: Vec3::from_array(*v0),
            },
            [v0, v1, ..] => Self {
                pos: Vec3::from_array(*v0),
                dir: Vec3::from_array(*v1) - Vec3::from_array(*v0),
            },
        }
    }
}

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
        // virus.bos `QueryWeapon1`: gun1 until the first `Shot1` flips it.
        Virus => &["gun1", "gun0"],
        _ => return None,
    })
}

/// The FBI `[SFXTypes]` index a unit's `FireWeapon1` plays at its
/// muzzle (`emit-sfx 1024+i from <muzzle>`), or `None` when the script
/// emits nothing there. Upstream has no automatic muzzle flash: the
/// only muzzle effect is what the script emits.
///
/// - bit.bos `emit-sfx 1025 from gunpoint` → 1 (arrowflare).
/// - byte.bos `emit-sfx 1024 from bp0..bp3` → 0.
/// - pointer.bos `emit-sfx 1024 from gunpoint` → 0.
/// - flow.bos, packet.bos, connection.bos, dos.bos, … declare no
///   `FireWeapon1` emit at all (Flow's `[SFXTypes]` are never used by
///   its weapon).
pub fn fire_weapon_sfx(kind: UnitKind) -> Option<usize> {
    use UnitKind::*;
    match kind {
        Bit => Some(1),
        Byte | Pointer => Some(0),
        _ => None,
    }
}

/// The piece a unit's gadget-drawn weapon arc starts from, when that is
/// not its muzzle. `network_arceffect.lua` `gadget:Explosion` calls
/// `BigArc(owner, nil, nil, 3, x,y,z)`: Lua piece 3 of the Connection
/// is `correcter` (on the hull, where `gp` pivots) — its beam leaves
/// `gp2`, slid out above the target.
pub fn arc_origin_piece(kind: UnitKind) -> Option<&'static str> {
    (kind == UnitKind::Connection).then_some("correcter")
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
    /// Piece detonation (`explode piece type ...`). Every explode class
    /// renders the same burst, so the class isn't carried.
    Explode {
        piece: usize,
    },
    Show {
        piece: usize,
    },
    Hide {
        piece: usize,
    },
    /// `call-script lua_BuildArc(piece)` (gateway.bos): the
    /// `network_arceffect.lua` gadget's white strand from `piece` to the
    /// unit this builder is building (`Spring.GetUnitIsBuilding`); no
    /// strand while it builds nothing.
    BuildArc {
        piece: usize,
    },
}

/// What a driver wants an `emit-sfx` to look like, replacing the raw COB
/// integer opcodes (`2048`, `4097`, ...) drivers used to push. Upstream
/// scripts OR a weapon index into the constant (`emit-sfx
/// SFX_DETONATE_WEAPON + 1`); the current renderer buckets by range only,
/// so the index is dropped here rather than carried dead. Only the two
/// ranges a driver emits are encoded: explicit weapon detonations
/// (`SFX_DETONATE_WEAPON_BASE..`) go through the combat path (worm
/// bite / Wormsplash) and named CEGs (`SFX_CEG_BASE..`) through
/// `weapon_fx`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SfxKind {
    /// `0..SFX_FIRE_WEAPON_BASE` — generic SFX (wake, smoke, ground
    /// spark, dust). Tiny puff so movement/attack scripts don't strobe.
    Puff,
    /// `SFX_FIRE_WEAPON_BASE..SFX_DETONATE_WEAPON_BASE` — weapon-fire
    /// flash at the piece (builder beams, idle turret flares). Small pop.
    FireFlash,
    /// `emit-sfx 2048` on a factory: fire the unit's `BuildLaser` beam
    /// from the piece along its emit direction (`CUnitScript::EmitSfx`
    /// → `CWeapon::Fire` toward `absPos + absDir`), a 256-elmo white
    /// beam that stops where it meets the ground.
    BuildBeam,
    /// `SFX_CEG_BASE + n` — the unit's n-th `explosiongenerator`, by its
    /// authored name (`custom:corruption_const`).
    Ceg(&'static str),
}

/// `BuildLaser` range (`retroweapons.tdf [BuildLaser] range=256`).
const BUILD_BEAM_RANGE: f32 = 256.0;

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
    /// A bare rig over `names` (no piece entities) for driver unit tests.
    #[cfg(test)]
    pub fn for_test(names: &'static [&'static str]) -> Self {
        let n = names.len();
        Self {
            piece_names: names,
            piece_entities: Vec::new(),
            piece_base_offsets: vec![[0.0; 3]; n],
            piece_rotations: vec![[0.0; 3]; n],
            piece_translations: vec![[0.0; 3]; n],
            target_rotations: vec![[0.0; 3]; n],
            turn_speeds: vec![[0.0; 3]; n],
            target_translations: vec![[0.0; 3]; n],
            move_speeds: vec![[0.0; 3]; n],
            spin_speeds: vec![[0.0; 3]; n],
            muzzle: 0,
            move_gate: 1.0,
            outbox: Vec::new(),
            dirty: true,
        }
    }

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

    /// `wait-for-turn` as `AimWeapon` polls it: true once `piece` sits
    /// within [`WAIT_FOR_TURN_TOLERANCE`] of the last turn issued on
    /// `axis` (measured the short way round). Missing pieces are always
    /// settled.
    ///
    /// Why a tolerance instead of [`Self::at_target`]: the host re-runs
    /// a driver's `aim()` every tick with the target's current bearing,
    /// *before* the rig ticks, so a turret tracking a moving target is
    /// always one tick of drift behind the freshly issued angle — an
    /// exact compare would hold its fire for as long as the target
    /// moves. Upstream re-aims only every 15 frames
    /// (`CWeapon::CanCallAimingScript`), so its wait lands on a stale
    /// bearing and the shot commits anyway.
    pub fn turn_settled(&self, piece: usize, axis: Axis) -> bool {
        if !self.live(piece) {
            return true;
        }
        let a = axis.index();
        short_arc(
            self.target_rotations[piece][a],
            self.piece_rotations[piece][a],
        )
        .abs()
            <= WAIT_FOR_TURN_TOLERANCE
    }

    /// `wait-for-move` as `AimWeapon` polls it: true once `piece` is
    /// within `tolerance` elmos of the last move issued along `axis` —
    /// the translation twin of [`Self::turn_settled`], for slides whose
    /// target follows a moving unit. Missing pieces are always settled.
    pub fn move_settled(&self, piece: usize, axis: Axis, tolerance: f32) -> bool {
        if !self.live(piece) {
            return true;
        }
        let a = axis.index();
        (self.target_translations[piece][a] - self.piece_translations[piece][a]).abs() <= tolerance
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

    /// `explode piece type <severity>`. `_severity` is the upstream
    /// `FALL/SHATTER/...` class, taken so drivers transcribe the `.bos`
    /// 1:1; every class currently renders the same burst.
    pub fn explode(&mut self, piece: usize, _severity: i32) {
        if self.live(piece) {
            self.outbox.push(FxEvent::Explode { piece });
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

    /// `call-script lua_BuildArc(piece)`, see [`FxEvent::BuildArc`].
    pub fn build_arc(&mut self, piece: usize) {
        if self.live(piece) {
            self.outbox.push(FxEvent::BuildArc { piece });
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
    /// The unit currently has a live aim request (`AimTarget`).
    pub aim_active: bool,
    /// The unit is under an explicit attack order (attack-target /
    /// attack-ground), or a Byte is launching mines. Foldable units must
    /// treat this as a want-open signal even when no enemy unit is
    /// auto-acquired to aim at (a ground order may target empty terrain)
    /// — otherwise a click gate on the fold state would deadlock against
    /// the missing AimTarget.
    pub attack_ordering: bool,
    /// The unit is still emerging (has an `Emerging` component). Drivers
    /// must run their build-emerge pose only while this is set — past
    /// emergence, the snap-to-rest would fight later choreography on
    /// the same piece (e.g. the byte's base lift on unfold).
    pub emerging: bool,
    /// The unit (as a factory) advanced a buildee this frame: the window
    /// between the engine's `StartBuilding` and `StopBuilding` calls,
    /// which excludes the unfold, a blocked pad and the gap between
    /// units.
    pub building: bool,
    /// Horizontal distance from the unit to its aim target, for scripts
    /// that size a piece to it (connection.bos `move gp2 to z-axis (get
    /// XZ_HYPOT(get UNIT_XZ(get TARGET_ID(1)) - get PIECE_XZ(gp)))`).
    /// Set only on `aim()` calls (`combat::drive_aim_script`).
    pub target_xz_dist: Option<f32>,
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
            aim_active: false,
            attack_ordering: false,
            emerging: false,
            building: false,
            target_xz_dist: None,
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

    /// `.bos` `Shot1()` followed by `QueryWeapon1()`: upstream
    /// `CWeapon::UpdateSalvo` (`Weapon.cpp:590-597`) calls both once per
    /// *projectile*, right before it resolves the muzzle and spawns the
    /// projectile. Drivers that cycle barrels point [`AnimRig::muzzle`]
    /// at the piece `QueryWeapon1` would return. Runs synchronously on
    /// the combat side, so the muzzle is current for the very shot.
    fn shot(&mut self, _rig: &mut AnimRig) {}

    /// `.bos` `EndBurst1()` — after the salvo's last shot.
    fn end_burst(&mut self, _rig: &mut AnimRig) {}

    /// `.bos` `StartBuilding(heading, pitch)` on a mobile builder: aim
    /// the nano emitter at the build site (`heading`/`pitch` body-relative,
    /// radians) and start spraying. `CBuilder::UpdateBuild` adds build
    /// power only once the script has set `INBUILDSTANCE`
    /// ([`Self::in_build_stance`]).
    fn start_building(&mut self, _rig: &mut AnimRig, _heading: f32, _pitch: f32) {}

    /// `.bos` `StopBuilding()` on a mobile builder: stop spraying and
    /// stow the emitter.
    fn stop_building(&mut self, _rig: &mut AnimRig) {}

    /// A mobile builder's `INBUILDSTANCE`: `Some(false)` while its
    /// emitter is still turning onto the site. `None` for units without
    /// a build script.
    fn in_build_stance(&self) -> Option<bool> {
        None
    }

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

    /// For units with a deploy cycle (the Pointer): where the script's
    /// `Open()`/`Close()` threads are right now. The host mirrors it into
    /// [`crate::units::combat::Deployable`] for the fire gates.
    fn deploy_state(&self) -> Option<crate::units::combat::DeployState> {
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
    /// Last frame's world pose of each piece, for beams fired along a
    /// piece's emit direction.
    pub globals:
        Query<'w, 's, (&'static GlobalTransform, Option<&'static PieceEmit>), With<PieceIndex>>,
    pub attacks: ResMut<'w, crate::units::weapon_fx::PendingAttacks>,
    pub heightmap: Option<Res<'w, crate::terrain::heightmap::Heightmap>>,
    /// Units under construction: a build beam stops on the one it hits
    /// (`TraceRay` with `collidefriendly=1`; finished units are out of
    /// a factory's way and the builder's own pieces are skipped).
    pub buildees: Query<
        'w,
        's,
        (
            &'static GlobalTransform,
            &'static crate::units::combat::CollisionVolume,
        ),
        With<crate::units::lifecycle::spawning::Emerging>,
    >,
    /// A builder's current nanoframe (`Constructing::building`, upstream
    /// `curBuild`), where its [`FxEvent::BuildArc`] strand ends.
    pub constructing: Query<'w, 's, &'static crate::units::lifecycle::construction::Constructing>,
    /// A builder clearing a feature off its site: the strand ends on the
    /// feature (`network_arceffect.lua` draws it for that order too).
    pub clearing: Query<'w, 's, &'static crate::units::lifecycle::construction::ClearingFeature>,
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
    #[allow(clippy::type_complexity)] // the animator bundle; split params would scatter one loop.
    pub animators: Query<
        'w,
        's,
        (
            Entity,
            &'static mut UnitAnimator,
            (&'static Faction, Option<&'static UnitColor>),
            &'static GlobalTransform,
            Option<&'static MoveTarget>,
            Option<&'static MovePath>,
            Option<&'static crate::units::lifecycle::production::Producer>,
            Option<&'static crate::units::lifecycle::spawning::Emerging>,
            Option<&'static crate::units::combat::AimTarget>,
            Option<&'static crate::units::combat::AttackGroundOrder>,
            Option<&'static crate::units::combat::AttackTargetOrder>,
            Has<crate::interaction::movement::AttackMoveActive>,
            Option<&'static crate::units::combat::TargetCache>,
            Has<crate::units::mechanics::command_fire::MineLaunch>,
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
        entity,
        mut animator,
        faction,
        unit_gtf,
        move_target,
        move_path,
        producer,
        emerging,
        aim_target,
        attack_ground,
        attack_target,
        attack_move,
        target_cache,
        launching_mines,
    ) in &mut drivers.animators
    {
        // `BUILD_PERCENT_LEFT` is `int((1 - buildProgress) * 100)`:
        // truncated, so it reads 0 once the build passes 99 %.
        let build_percent = emerging
            .map(|e| {
                if e.total > 0.0 {
                    ((e.remaining / e.total) * 100.0) as i32
                } else {
                    0
                }
            })
            .unwrap_or(0);

        let ctx = AnimCtx {
            dt,
            build_percent,
            moving: crate::interaction::movement::moving_for_script(
                move_target.is_some() || move_path.is_some(),
                attack_move,
                crate::interaction::movement::fight_engaged(aim_target.is_some(), target_cache),
            ),
            producing: producer.is_some_and(|p| p.current_production().is_some()),
            building: producer.is_some_and(|p| p.building),
            aim_active: aim_target.is_some(),
            attack_ordering: attack_ground.is_some()
                || attack_target.is_some()
                // byte.bos `LaunchMines`: `start-script Open()` while folded.
                || launching_mines,
            emerging: emerging.is_some(),
            target_xz_dist: None,
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

        let (faction, color) = faction;
        let color = color.map_or(TeamColor::of_faction(*faction), |c| c.0);
        apply_and_drain(entity, rig, color, unit_gtf, &mut fx);
    }
}

/// Signed angle (radians) a turn from `current` to `target` covers on
/// the short way round, in `[-π, π)` — upstream
/// `CUnitScript::TurnToward` (`Sim/Units/Scripts/UnitScript.cpp`):
/// `delta = fmod(dest - cur + 3π, 2π) - π`. A turn is an orientation
/// change, never a wind-up: a turret at +170° aiming at −170° swings
/// 20°, not 340°. An exact half turn goes the negative way, as the
/// engine's formula does.
///
/// A difference already inside `[-π, π)` is returned as is (no
/// round trip through the modulo), so a turn that doesn't cross the
/// wrap steps bit-for-bit like a plain linear interpolation — what the
/// Kani harnesses in `proofs.rs` pin against the retired VM.
pub fn short_arc(target: f32, current: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let d = target - current;
    if (-PI..PI).contains(&d) {
        d
    } else {
        (d + PI).rem_euclid(TAU) - PI
    }
}

/// Advance a rig's interpolation: spins integrate continuously; turns and
/// moves step toward their targets and stop when they arrive. Turns take
/// the [`short_arc`] like the engine's; stored angles aren't wrapped
/// (drivers and tests read them back as issued), so an arriving turn
/// lands on its target value exactly — the same orientation the short
/// arc reached. Flags the rig dirty whenever anything actually moved.
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
                let diff = short_arc(target, current);
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
    entity: Entity,
    rig: &mut AnimRig,
    color: TeamColor,
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
                let piece_world_pos = piece_world_pos(fx, rig.piece_entities[piece], unit_gtf);
                spawn_death_particle(
                    piece_world_pos,
                    color,
                    &mut fx.death_assets,
                    &mut fx.commands,
                    &mut fx.meshes,
                    &mut fx.materials,
                );
            }
            FxEvent::Emit {
                piece,
                kind: SfxKind::BuildBeam,
            } if in_range(piece) => {
                let Ok((gtf, emit)) = fx.globals.get(rig.piece_entities[piece]) else {
                    continue;
                };
                // A unit spawned this frame has no propagated transforms
                // yet; a beam from the identity pose would start at the
                // map origin.
                if unit_gtf.translation() == Vec3::ZERO {
                    continue;
                }
                let start = gtf.translation();
                let dir = gtf
                    .rotation()
                    .mul_vec3(emit.map_or(Vec3::Z, |e| e.dir))
                    .normalize_or(Vec3::NEG_Y);
                let far = start + dir * BUILD_BEAM_RANGE;
                let mut end = fx
                    .heightmap
                    .as_deref()
                    .and_then(|hm| hm.ground_hit(start, far))
                    .unwrap_or(far);
                let mut nearest = 1.0f32;
                for (tf, volume) in &fx.buildees {
                    let center = volume.center(tf);
                    if center.distance_squared(start) > sq(BUILD_BEAM_RANGE + volume.radius) {
                        continue;
                    }
                    if let Some(t) = volume.ray_segment_hit(center, start, end)
                        && t < nearest
                    {
                        nearest = t;
                    }
                }
                if nearest < 1.0 {
                    end = start.lerp(end, nearest);
                }
                fx.attacks
                    .events
                    .push(crate::units::weapon_fx::AttackEvent {
                        attacker_pos: start,
                        target_pos: end,
                        weapon_id: crate::units::content::weapons::WeaponId::BUILD_LASER,
                        muzzle_ceg: None,
                        delayed_hit: None,
                        arc_from: None,
                    });
            }
            FxEvent::Emit { piece, kind } if in_range(piece) => {
                let piece_world_pos = piece_world_pos(fx, rig.piece_entities[piece], unit_gtf);
                let up = unit_gtf.up().as_vec3();
                dispatch_emit_sfx(kind, piece_world_pos, up, color, &mut fx.explosions);
            }
            FxEvent::BuildArc { piece } if in_range(piece) => {
                // `BuildArc(u, ud, team, piece)`: from the piece to the
                // buildee's base position (`GetUnitPosition(b)`).
                let Some(to) = fx
                    .constructing
                    .get(entity)
                    .ok()
                    .and_then(|c| c.building)
                    .and_then(|b| fx.buildees.get(b).ok())
                    .map(|(gtf, _)| gtf.translation())
                    .or_else(|| fx.clearing.get(entity).ok().map(|c| c.at))
                else {
                    continue;
                };
                let from = piece_world_pos(fx, rig.piece_entities[piece], unit_gtf);
                fx.attacks
                    .build_arcs
                    .push(crate::units::weapon_fx::BuildArcEvent { from, to });
            }
            _ => {}
        }
    }
}

/// Where an `explode` / `emit-sfx` piece is in the world: its
/// propagated pose (last frame's — a tick stale at most, invisible for a
/// burst). The piece's local translation added to the unit origin, which
/// this used to be, ignored the unit's facing, the model root's 180° yaw
/// and every parent piece, so a Kernel's death bursts all went off near
/// its root instead of at its tips. A unit spawned this frame has no
/// propagated pose yet (identity): fall back to the unit origin.
fn piece_world_pos(fx: &AnimFxOut, piece: Entity, unit_gtf: &GlobalTransform) -> Vec3 {
    fx.globals
        .get(piece)
        .map(|(gtf, _)| gtf.translation())
        .ok()
        .filter(|pos| *pos != Vec3::ZERO)
        .unwrap_or_else(|| unit_gtf.translation())
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
                .try_insert(MuzzlePiece(animator.rig.muzzle));
        }
    }
}

// ---------------------------------------------------------------------------
// EmitSfx dispatch
// ---------------------------------------------------------------------------

/// Turn a typed [`SfxKind`] into a owner-coloured explosion event.
///
/// The raw COB opcode → kind decoding lives in [`SfxKind`]; this is the
/// rendering half. Sizes follow the old range-bucketing so the visual
/// language is unchanged (see the [`SfxKind`] docs for the upstream
/// constant ranges each kind stands in for).
fn dispatch_emit_sfx(
    kind: SfxKind,
    pos: Vec3,
    up: Vec3,
    color: TeamColor,
    explosions: &mut PendingExplosions,
) {
    let (radius, intensity) = match kind {
        SfxKind::FireFlash => (4.0, 0.8),
        SfxKind::Puff => (2.5, 0.6),
        SfxKind::Ceg(name) => {
            explosions.events.push(ExplosionEvent {
                pos,
                up,
                rgb: color.rgb_f32(),
                radius: 8.0,
                ceg_name: name.to_string(),
            });
            return;
        }
        // Handled by the caller: it needs the piece's direction.
        SfxKind::BuildBeam => return,
    };

    let base = color.rgb_f32();
    let rgb = [
        base[0] * intensity,
        base[1] * intensity,
        base[2] * intensity,
    ];

    explosions.events.push(ExplosionEvent {
        pos,
        up,
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

/// Lazily-populated shared mesh and per-owner-colour emissive materials for
/// death particles. `spawn_death_particle` seeds the entry the first time
/// it needs a given owner color; every subsequent particle re-uses the
/// same Handle. Mirrors the pattern used by `BuildSparkleAssets` /
/// `ImpactBurstAssets`.
#[derive(Resource, Default)]
pub struct DeathParticleAssets {
    pub mesh: Option<Handle<Mesh>>,
    pub by_color: std::collections::HashMap<TeamColor, Handle<StandardMaterial>>,
}

impl DeathParticleAssets {
    fn material_for(
        &mut self,
        color: TeamColor,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        self.by_color
            .entry(color)
            .or_insert_with(|| {
                let color = LinearRgba::from(color.color());
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

/// Spawn a brief expanding, fading burst at `pos` in the unit's owner color.
///
/// Mesh and per-colour material are owned by [`DeathParticleAssets`] so
/// every burst reuses the same Handle — an arena full of dying units
/// doesn't each mint a fresh sphere and material asset.
fn spawn_death_particle(
    pos: Vec3,
    color: TeamColor,
    assets: &mut DeathParticleAssets,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) {
    let mesh = assets
        .mesh
        .get_or_insert_with(|| meshes.add(Sphere::new(1.0).mesh().ico(2).unwrap()))
        .clone();
    let material = assets.material_for(color, materials);

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
        let t = (particle.lifetime / particle.max_lifetime).det_clamp(0.0, 1.0);

        if t >= 1.0 {
            commands.entity(entity).despawn();
            continue;
        }

        // Grow then shrink: peaks at t=0.8, collapses to zero by t=1.0.
        let peak_scale = 2.0 + 20.0 * t.det_min(0.8) / 0.8;
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

#[cfg(test)]
mod turn_semantics {
    //! Engine turn semantics (`CUnitScript::TurnToward`): every turn
    //! takes the short way round.

    use super::*;
    use std::f32::consts::{PI, TAU};

    #[test]
    fn short_arc_wraps_into_minus_pi_to_pi() {
        let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
        assert!(close(
            short_arc(deg2rad(-170.0), deg2rad(170.0)),
            deg2rad(20.0)
        ));
        assert!(close(
            short_arc(deg2rad(170.0), deg2rad(-170.0)),
            deg2rad(-20.0)
        ));
        assert!(close(
            short_arc(deg2rad(30.0), deg2rad(10.0)),
            deg2rad(20.0)
        ));
        // A piece that spun many turns still takes the short way.
        assert!(close(short_arc(0.0, 10.0 * TAU + 0.25), -0.25));
        // An exact half turn goes negative, as the engine's formula does.
        assert!(close(short_arc(PI, 0.0), -PI));
    }

    /// Regression: a Byte aimer at +170° tracking a target that crosses
    /// behind it to −170° swung 340° the long way (1.26 s at <270>/s)
    /// and held its fire the whole time. The short arc is 20°: 3 ticks.
    #[test]
    fn a_turn_across_the_back_takes_the_short_arc() {
        let mut rig = AnimRig::for_test(&["aimer"]);
        rig.turn_deg(0, Axis::Y, 170.0, 0.0);
        rig.turn_deg(0, Axis::Y, -170.0, 270.0);
        let mut ticks = 0;
        while !rig.at_target(0, Axis::Y) {
            tick_rig(&mut rig, 1.0 / 30.0);
            ticks += 1;
            assert!(ticks < 10, "turned the long way round");
            // Never strays from the 20° arc between the two headings.
            let r = rig.piece_rotations[0][1];
            assert!(short_arc(PI, r).abs() <= deg2rad(10.0) + 1e-4);
        }
        assert_eq!(ticks, 3);
        assert_eq!(rig.piece_rotations[0][1], deg2rad(-170.0));
    }

    /// The system's queries (drivers, piece transforms, buildees, the
    /// builders' `Constructing`) must not alias — Bevy panics at
    /// initialisation if they do, which no other test would catch.
    #[test]
    fn animation_system_params_are_disjoint() {
        let mut world = World::new();
        let mut system = IntoSystem::into_system(animation_system);
        system.initialize(&mut world);
    }

    #[test]
    fn turn_settled_measures_the_short_way() {
        let mut rig = AnimRig::for_test(&["turret"]);
        rig.turn_deg(0, Axis::Y, 179.0, 0.0);
        rig.turn_deg(0, Axis::Y, -179.0, 90.0);
        // 2° apart across ±180°: inside the wait tolerance.
        assert!(rig.turn_settled(0, Axis::Y));
        rig.turn_deg(0, Axis::Y, 0.0, 90.0);
        assert!(!rig.turn_settled(0, Axis::Y));
        // Missing pieces never block a wait.
        assert!(rig.turn_settled(PIECE_MISSING, Axis::Y));
    }

    #[test]
    fn turn_time_counts_the_short_arc() {
        let mut rig = AnimRig::for_test(&["rotor"]);
        rig.turn_deg(0, Axis::Y, 170.0, 0.0);
        let t = units::turn_time(&rig, 0, Axis::Y, -170.0, 20.0);
        assert!((t - 1.0).abs() < 1e-3, "{t}");
    }
}
