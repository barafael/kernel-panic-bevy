use std::collections::HashMap;
use std::sync::Arc;

use bevy::prelude::*;

use spring_cob::{AnimCommand, CobFile, CobVm, parse_cob};

use super::meshes::load_asset_from_disk;
use crate::units::components::Faction;
use crate::units::weapon_fx::{ExplosionEvent, PendingExplosions};

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

/// Component holding per-unit animation state.
#[derive(Component)]
pub struct CobAnimator {
    pub vm: CobVm,
    pub cob: Arc<CobFile>,
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
    /// Turn speed per piece per axis (radians/sec, 0 = instant).
    pub turn_speeds: Vec<[f32; 3]>,
    /// Target translation per piece (for interpolated moves).
    pub target_translations: Vec<[f32; 3]>,
    /// Move speed per piece per axis (elmos/sec, 0 = instant).
    pub move_speeds: Vec<[f32; 3]>,
    /// Spin velocity per piece per axis (radians/sec).
    pub spin_speeds: Vec<[f32; 3]>,
}

/// Marks a Bevy entity as an animated piece child.
#[derive(Component)]
pub struct PieceIndex;

/// Index (into `CobAnimator::piece_entities`) of the unit's primary
/// weapon muzzle — where beams and projectiles originate.
///
/// Upstream scripts express this through the `QueryWeapon1(piecenum)`
/// callout which sets `piecenum` to a piece like `gunpoint`, `flare`,
/// or `bp0`. The [`refresh_muzzle_pieces`] system runs every frame
/// and asks the script directly, so Byte's `gp`-driven cycle through
/// bp0 → bp1 → bp2 → bp3 is honoured shot-for-shot instead of
/// resolving to a single barrel once at spawn. `MuzzlePiece::resolve`
/// remains as a spawn-time fallback for units whose scripts don't
/// declare a recognised muzzle name.
#[derive(Component, Clone, Copy, Debug)]
pub struct MuzzlePiece(pub usize);

impl MuzzlePiece {
    /// Candidate piece names searched in order at spawn, as a
    /// fallback when `QueryWeapon1` is absent (factories, turretless
    /// units). `gunpoint` covers Bit / Pointer / DOS / Exploit*,
    /// `bp0` the Byte's first barrel, `flare` / `barrel` / `muzzle`
    /// are generic fallbacks for any third-party unit that doesn't
    /// follow KP's specific naming.
    pub const CANDIDATE_NAMES: &'static [&'static str] =
        &["gunpoint", "bp0", "flare", "barrel", "muzzle"];

    /// Resolve the muzzle against a parsed script's piece table. Returns
    /// `None` for Worms / factories / turretless units whose scripts
    /// declare none of the recognised names — combat falls back to the
    /// unit transform origin in that case.
    pub fn resolve(cob: &CobFile) -> Option<Self> {
        Self::CANDIDATE_NAMES.iter().find_map(|name| {
            cob.piece_names
                .iter()
                .position(|n| n.eq_ignore_ascii_case(name))
                .map(Self)
        })
    }
}

/// Refresh [`MuzzlePiece`] from the script's `QueryWeapon1` answer
/// for every unit currently engaging a target. Why: byte's `gp`
/// static var only advances inside `FireWeapon1`, so units without
/// an [`crate::units::combat::AimTarget`] never see it change —
/// re-querying them every tick would burn VM ops at scale for no
/// behaviour change.
pub fn refresh_muzzle_pieces(
    mut query: Query<
        (Entity, &mut CobAnimator, Option<&MuzzlePiece>),
        With<crate::units::combat::AimTarget>,
    >,
    mut commands: Commands,
) {
    for (entity, mut animator, cached) in &mut query {
        let cob = animator.cob.clone();
        let Some(piece_i32) = animator.vm.call_script_out_param(&cob, "QueryWeapon1") else {
            continue;
        };
        let Ok(piece_usize) = usize::try_from(piece_i32) else {
            continue;
        };
        if piece_usize >= animator.piece_entities.len() {
            continue;
        }
        if cached.is_none_or(|mp| mp.0 != piece_usize) {
            commands.entity(entity).insert(MuzzlePiece(piece_usize));
        }
    }
}

/// Cached COB piece index for the deploy/aim gun pivot (`gunbase`). Set at
/// spawn when the unit's script declares the piece; read every frame by
/// `aim_weapons_system` so it doesn't re-scan the piece-name table.
#[derive(Component, Clone, Copy, Debug)]
pub struct GunbasePiece(pub usize);

/// Cached COB piece index for the turret yaw pivot (`aimer`). Set at
/// spawn when the unit's script declares the piece. Upstream units
/// with an `aimer` (byte, wormOLD) use it as the rotation target for
/// `AimWeapon1(h,p)` — heading `h` applied to the aimer's Y axis
/// swings every descendant (rotor → blades → bp0..3) around so the
/// firing piece ends up on the target bearing. Without this, the
/// firing corner stays at the model's authored heading=0 and the
/// bolt visibly emerges perpendicular to the target line. Read by
/// `aim_weapons_system` to drive the rotation.
#[derive(Component, Clone, Copy, Debug)]
pub struct AimerPiece(pub usize);

/// Cached COB piece index for the Connection's hatch (`body`). Set at
/// spawn when the unit's script declares the piece; read every frame by
/// `animate_connection_hatch` so it doesn't re-scan the piece-name table.
#[derive(Component, Clone, Copy, Debug)]
pub struct HatchPiece(pub usize);

/// Cached parsed COB files, keyed by script filename.
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

/// Spring's fixed runtime scale for both linear and angular values in
/// a compiled `.cob`. **Always 65536**, never per-unit — see
/// [`spring_cob`]'s crate-level docs for the full rationale and
/// upstream citations (`rts/Sim/Units/Scripts/CobInstance.h:20`).
///
/// The per-`UnitKind` divisor table that used to live in
/// `content::definitions` was a misread of the Scriptor comment in
/// `byte.bos`/`bit.bos`/etc. (`// To be compiled with a linear
/// constant of 163840`). That's a compiler directive, not a runtime
/// divisor. Dividing by 163840 produced 2.5×-too-small animations
/// (blade spacing, base lift, launcher arm) on every unit whose
/// `.bos` was compiled at 163840.
const COBSCALE: f32 = spring_cob::COBSCALE as f32;

/// TA-angle units (1 revolution = `COBSCALE` = 65536) → radians.
/// Matches upstream's `TAANG2RAD = π / COBSCALE_HALF`
/// (`CobInstance.h:25`).
fn spring_angle_to_radians(angle: i32) -> f32 {
    (angle as f32) * std::f32::consts::TAU / COBSCALE
}

/// COB linear value (Scriptor bakes source `[N]` → `N * scriptor_constant`)
/// → elmos, via `value * COBSCALE_INV` exactly like
/// `CobInstance.h:131`'s `Move`/`MoveNow` dispatch. Do not add a
/// per-unit divisor here — see [`COBSCALE`].
fn spring_linear_to_elmos(val: i32) -> f32 {
    val as f32 / COBSCALE
}

/// Turn/TurnNow destinations map 1:1 between Spring and Bevy on X/Y; only
/// the Z axis needs negation, since Spring's left-handed Z rotation inverts
/// relative to Bevy's right-handed world.
fn cobwtf_turn_axis(axis: i32, value: i32) -> i32 {
    if axis == 2 { -value } else { value }
}

/// Spin axis mapping. With the spawn-time `model_root` 180° Y rotation
/// we now apply, an X-axis spin on a piece — spinning around its local
/// `+X` — is rendered around world `-X` (model_root flips the X axis),
/// which already inverts Spring's left-handed handedness for X spins.
/// So an unmodified Spring `spin body x +180` produces the expected
/// forward roll (top of the Pointer cube toward the direction of
/// motion) without any extra negation here.
///
/// Z still needs negation: the same model_root-flip-vs-handedness
/// argument applied to Z would also cancel out, but Spring's `RotateZ`
/// is left-handed independently of the front-axis convention, and we
/// haven't found a Z-axis spin in the KP roster to verify against —
/// keep the historical sign and revisit if a unit reports it wrong.
fn cobwtf_spin_axis(axis: i32, value: i32) -> i32 {
    if axis == 2 { -value } else { value }
}

/// Move destinations on the X axis are mirrored between Spring and Bevy.
/// Spring's engine flips the sign of a piece's X offset when loading s3o
/// (so `move left to x-axis [10]` sends the piece to world x=-10), but
/// our parser keeps authored offsets verbatim. The COB `left` piece on
/// the Pointer sits at geometric x∈[-16..0]; Open() sends it to [10],
/// which should slide it OUTWARD (more negative X), not toward center.
/// Flipping X here reproduces Spring's behavior so Open parts the halves
/// and Close brings them back together. Y and Z map 1:1.
fn cobwtf_move_axis(axis: i32, value: i32) -> i32 {
    if axis == 0 { -value } else { value }
}

/// Push host-tracked unit values (BUILD_PERCENT_LEFT today,
/// more later) into each unit's COB VM so its `Create()` script can
/// drive its own emerge animation. Runs before `animation_system` so
/// the values are visible to scripts on this tick.
///
/// Spring's BUILD_PERCENT_LEFT goes 100 (just spawned) → 0 (finished).
/// We map from `Emerging.remaining / total` so the unit's `Create()`
/// loop ticks down naturally over the rise window. The CobVm defaults
/// unset keys to 0, so units that never had `Emerging` (buildings that
/// skipped the rise, units spawned via cheat paths) read 0 implicitly
/// — we only need to touch animators that are mid-emerge or just
/// finished emerging (to post the final 0 that closes out their
/// `while(get BUILD_PERCENT_LEFT)` Create() loop).
pub fn publish_unit_values(
    mut emerging_q: Query<(
        &mut CobAnimator,
        &mut crate::units::lifecycle::spawning::Emerging,
    )>,
    mut finished_q: Query<&mut CobAnimator, Without<crate::units::lifecycle::spawning::Emerging>>,
    mut removed: RemovedComponents<crate::units::lifecycle::spawning::Emerging>,
) {
    for (mut animator, mut emerging) in &mut emerging_q {
        let percent = if emerging.total > 0.0 {
            ((emerging.remaining / emerging.total) * 100.0).round() as i32
        } else {
            0
        };
        if percent == emerging.last_build_percent {
            continue;
        }
        emerging.last_build_percent = percent;
        animator
            .vm
            .set_unit_value(spring_cob::unit_values::BUILD_PERCENT_LEFT, percent);
    }
    for entity in removed.read() {
        if let Ok(mut animator) = finished_q.get_mut(entity) {
            animator
                .vm
                .set_unit_value(spring_cob::unit_values::BUILD_PERCENT_LEFT, 0);
        }
    }
}

/// Tick all CobAnimator VMs and apply piece transforms.
///
/// `turn_finished` / `move_finished` are `Local` scratch buffers reused across
/// every animator on every frame. They were originally allocated fresh per
/// animator, which meant ~N units × several allocations per frame just to hold
/// a handful of completion events; `Local` keeps the capacity across runs so
/// the steady state hits zero allocations.
#[allow(clippy::too_many_arguments)]
pub fn animation_system(
    time: Res<Time>,
    mut animators: Query<(&mut CobAnimator, &Faction, &GlobalTransform)>,
    mut transforms: Query<(&mut Transform, &mut Visibility), With<PieceIndex>>,
    mut spawn_commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut death_assets: ResMut<DeathParticleAssets>,
    mut explosions: ResMut<PendingExplosions>,
    mut turn_finished: Local<Vec<(i32, i32)>>,
    mut move_finished: Local<Vec<(i32, i32)>>,
) {
    let dt = time.delta_secs();
    let dt_ms = (dt * 1000.0) as i32;

    for (mut animator, faction, unit_gtf) in &mut animators {
        // Tick the COB VM.
        let cob = animator.cob.clone();
        let commands = animator.vm.tick(&cob, dt_ms);

        // Process animation commands.
        for cmd in &commands {
            match cmd {
                AnimCommand::TurnNow {
                    piece,
                    axis,
                    destination,
                } => {
                    let p = *piece as usize;
                    let a = *axis as usize;
                    if p < animator.piece_rotations.len() && a < 3 {
                        let angle = spring_angle_to_radians(cobwtf_turn_axis(*axis, *destination));
                        animator.piece_rotations[p][a] = angle;
                        animator.target_rotations[p][a] = angle;
                        animator.turn_speeds[p][a] = 0.0;
                    }
                }
                AnimCommand::Turn {
                    piece,
                    axis,
                    destination,
                    speed,
                } => {
                    let p = *piece as usize;
                    let a = *axis as usize;
                    if p < animator.piece_rotations.len() && a < 3 {
                        animator.target_rotations[p][a] =
                            spring_angle_to_radians(cobwtf_turn_axis(*axis, *destination));
                        animator.turn_speeds[p][a] = spring_angle_to_radians(speed.abs());
                    }
                }
                AnimCommand::MoveNow {
                    piece,
                    axis,
                    destination,
                } => {
                    let p = *piece as usize;
                    let a = *axis as usize;
                    if p < animator.piece_translations.len() && a < 3 {
                        let pos = spring_linear_to_elmos(cobwtf_move_axis(*axis, *destination));
                        animator.piece_translations[p][a] = pos;
                        animator.target_translations[p][a] = pos;
                        animator.move_speeds[p][a] = 0.0;
                    }
                }
                AnimCommand::Move {
                    piece,
                    axis,
                    destination,
                    speed,
                } => {
                    let p = *piece as usize;
                    let a = *axis as usize;
                    if p < animator.piece_translations.len() && a < 3 {
                        animator.target_translations[p][a] =
                            spring_linear_to_elmos(cobwtf_move_axis(*axis, *destination));
                        animator.move_speeds[p][a] = spring_linear_to_elmos(speed.abs());
                    }
                }
                AnimCommand::Spin {
                    piece, axis, speed, ..
                } => {
                    let p = *piece as usize;
                    let a = *axis as usize;
                    if p < animator.spin_speeds.len() && a < 3 {
                        animator.spin_speeds[p][a] =
                            spring_angle_to_radians(cobwtf_spin_axis(*axis, *speed));
                    }
                }
                AnimCommand::StopSpin { piece, axis, .. } => {
                    let p = *piece as usize;
                    let a = *axis as usize;
                    if p < animator.spin_speeds.len() && a < 3 {
                        animator.spin_speeds[p][a] = 0.0;
                    }
                }
                AnimCommand::Show { piece } => {
                    let p = *piece as usize;
                    if p < animator.piece_entities.len()
                        && let Ok((_, mut vis)) = transforms.get_mut(animator.piece_entities[p])
                    {
                        *vis = Visibility::Inherited;
                    }
                }
                AnimCommand::Hide { piece } => {
                    let p = *piece as usize;
                    if p < animator.piece_entities.len()
                        && let Ok((_, mut vis)) = transforms.get_mut(animator.piece_entities[p])
                    {
                        *vis = Visibility::Hidden;
                    }
                }
                AnimCommand::Explode { piece, .. } => {
                    let p = *piece as usize;
                    if p < animator.piece_entities.len() {
                        // Hide the original piece.
                        if let Ok((_, mut vis)) = transforms.get_mut(animator.piece_entities[p]) {
                            *vis = Visibility::Hidden;
                        }
                        // Spawn a brief explosion burst at the piece's world position.
                        let piece_world_pos =
                            if let Ok((tf, _)) = transforms.get(animator.piece_entities[p]) {
                                unit_gtf.translation() + tf.translation
                            } else {
                                unit_gtf.translation()
                            };
                        spawn_death_particle(
                            piece_world_pos,
                            *faction,
                            &mut death_assets,
                            &mut spawn_commands,
                            &mut meshes,
                            &mut materials,
                        );
                    }
                }
                AnimCommand::EmitSfx { sfx_type, piece } => {
                    let p = *piece as usize;
                    if p < animator.piece_entities.len() {
                        let piece_world_pos =
                            if let Ok((tf, _)) = transforms.get(animator.piece_entities[p]) {
                                unit_gtf.translation() + tf.translation
                            } else {
                                unit_gtf.translation()
                            };
                        dispatch_emit_sfx(*sfx_type, piece_world_pos, *faction, &mut explosions);
                    }
                }
                // SetValue — not yet implemented.
                _ => {}
            }
        }

        // Interpolate piece transforms and collect anim-finished events.
        let num_pieces = animator.piece_rotations.len();
        turn_finished.clear();
        move_finished.clear();

        for p in 0..num_pieces {
            for a in 0..3 {
                // Spin: continuous rotation.
                if animator.spin_speeds[p][a] != 0.0 {
                    animator.piece_rotations[p][a] += animator.spin_speeds[p][a] * dt;
                }

                // Interpolate turn toward target.
                let speed = animator.turn_speeds[p][a];
                if speed > 0.0 {
                    let target = animator.target_rotations[p][a];
                    let current = animator.piece_rotations[p][a];
                    let diff = target - current;
                    let step = speed * dt;
                    if diff.abs() <= step {
                        animator.piece_rotations[p][a] = target;
                        animator.turn_speeds[p][a] = 0.0;
                        turn_finished.push((p as i32, a as i32));
                    } else {
                        animator.piece_rotations[p][a] += step * diff.signum();
                    }
                }

                // Interpolate move toward target.
                let mspeed = animator.move_speeds[p][a];
                if mspeed > 0.0 {
                    let target = animator.target_translations[p][a];
                    let current = animator.piece_translations[p][a];
                    let diff = target - current;
                    let step = mspeed * dt;
                    if diff.abs() <= step {
                        animator.piece_translations[p][a] = target;
                        animator.move_speeds[p][a] = 0.0;
                        move_finished.push((p as i32, a as i32));
                    } else {
                        animator.piece_translations[p][a] += step * diff.signum();
                    }
                }
            }

            // Apply to Bevy transform: base offset + animated translation.
            if p < animator.piece_entities.len() {
                let entity = animator.piece_entities[p];
                if let Ok((mut tf, _)) = transforms.get_mut(entity) {
                    let r = animator.piece_rotations[p];
                    let t = animator.piece_translations[p];
                    let base = animator.piece_base_offsets[p];
                    // Spring uses left-handed (Z=forward) coords with rotation
                    // composition R = Ry(-ry) * Rx(-rx) * Rz(-rz). Bevy is
                    // right-handed (Z=back); flipping Z reverses rotations
                    // around X and Y but not around Z. So Bevy's equivalent
                    // is Ry(+ry) * Rx(+rx) * Rz(-rz).
                    tf.rotation = Quat::from_euler(EulerRot::YXZ, r[1], r[0], -r[2]);
                    tf.translation = Vec3::new(base[0] + t[0], base[1] + t[1], base[2] + t[2]);
                }
            }
        }

        // Notify VM of completed animations.
        for (piece, axis) in turn_finished.drain(..) {
            animator
                .vm
                .anim_finished(spring_cob::AnimType::Turn, piece, axis);
        }
        for (piece, axis) in move_finished.drain(..) {
            animator
                .vm
                .anim_finished(spring_cob::AnimType::Move, piece, axis);
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
fn dispatch_emit_sfx(
    sfx_type: i32,
    pos: Vec3,
    faction: Faction,
    explosions: &mut PendingExplosions,
) {
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
    //! bug. If anyone adds a per-unit divisor back, these fail.
    //! The companion tests live in `spring-cob::cobscale_tests` —
    //! they pin the constant itself; these pin the helpers.

    use super::{COBSCALE, spring_angle_to_radians, spring_linear_to_elmos};

    #[test]
    fn linear_helper_uses_cobscale_65536() {
        assert!((COBSCALE - 65536.0).abs() < 1e-6);
    }

    #[test]
    fn byte_bos_move_blade0_bracket_4_recovers_10_elmos() {
        // byte.bos: `move blade0 to z-axis [4] speed [16];`
        // Compiled with Scriptor linear=163840 per the header
        // comment, so `[4]` is 4 * 163840 = 655360 in bytecode.
        // Upstream runtime: 655360 / 65536 = 10.0 elmos effective.
        assert_eq!(spring_linear_to_elmos(4 * 163840), 10.0);
    }

    #[test]
    fn standard_scriptor_bracket_1_recovers_1_elmo() {
        // Most units (pointer, hole, …) use Scriptor's default 65536.
        // Source `[1]` → 65536 in bytecode → 1.0 elmo at runtime.
        assert_eq!(spring_linear_to_elmos(65536), 1.0);
    }

    #[test]
    fn angle_helper_half_circle_is_pi() {
        // Spring's TA-angle unit: 65536 = 2π. Half circle = 32768.
        let half_circle = spring_angle_to_radians(32768);
        assert!((half_circle - std::f32::consts::PI).abs() < 1e-3);
    }
}
