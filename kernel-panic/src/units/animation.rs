use std::collections::HashMap;
use std::f32::consts::PI;
use std::sync::Arc;

use bevy::prelude::*;

use spring_cob::{AnimCommand, CobFile, CobVm, parse_cob};

use super::components::Faction;
use super::meshes::load_asset_from_disk;

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
    /// COB linear-constant scale for this script. The .bos compiler
    /// multiplies every `[N]` literal by this constant; we divide by it
    /// here to recover N elmos. Spring's *engine* default is 65536, but
    /// the Kernel Panic project's *Scriptor* default was 163840 (only
    /// pointer/hole opted out — they have a header comment overriding
    /// the constant back to 65536). So pick the right per-unit value at
    /// spawn time and stash it here.
    pub linear_constant: f32,
}

/// Marks a Bevy entity as an animated piece child.
#[derive(Component)]
pub struct PieceIndex;

/// Index (into `CobAnimator::piece_entities`) of the unit's primary weapon
/// muzzle — where beams and projectiles should originate. Upstream scripts
/// express this through the `QueryWeapon1(piecenum)` callout which sets
/// `piecenum` to a piece like `gunpoint`, `flare`, or `bp0`. Our VM doesn't
/// yet execute `call_script` to consume a returned out-param, so we resolve
/// the muzzle heuristically at spawn from a small list of well-known names.
/// Covers every KP unit that declares one (Bit / Byte / Pointer / DOS /
/// Exploit*); units without any match fall back to the unit transform
/// origin in `combat_system`.
#[derive(Component, Clone, Copy, Debug)]
pub struct MuzzlePiece(pub usize);

/// Cached COB piece index for the deploy/aim gun pivot (`gunbase`). Set at
/// spawn when the unit's script declares the piece; read every frame by
/// `aim_weapons_system` so it doesn't re-scan the piece-name table.
#[derive(Component, Clone, Copy, Debug)]
pub struct GunbasePiece(pub usize);

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

/// Spring uses "angular units" where 65536 = 360°. Convert to radians.
fn spring_angle_to_radians(angle: i32) -> f32 {
    (angle as f32) / 65536.0 * 2.0 * PI
}

/// Spring linear units: the .bos `[N]` literal is compiled to
/// `N * linear_constant`, so dividing by the same constant recovers N
/// elmos. The constant is per-script (Kernel Panic's project default
/// is 163840; pointer.bos and hole.bos override it back to 65536).
fn spring_linear_to_elmos(val: i32, linear_constant: f32) -> f32 {
    val as f32 / linear_constant
}

/// Turn/TurnNow destinations map 1:1 between Spring and Bevy on X/Y; only
/// the Z axis needs negation, since Spring's left-handed Z rotation inverts
/// relative to Bevy's right-handed world.
fn cobwtf_turn_axis(axis: i32, value: i32) -> i32 {
    if axis == 2 { -value } else { value }
}

/// Spin on the X axis additionally needs its sign flipped: Spring's
/// "spin body around x-axis" rolls a unit forward over its nose as it
/// moves, but the same raw angular velocity rolls a Bevy (right-handed)
/// mesh backward. Flipping X here restores the expected forward roll
/// (visible on the Pointer cube while moving). Z still needs the Turn
/// negation.
fn cobwtf_spin_axis(axis: i32, value: i32) -> i32 {
    match axis {
        0 => -value,
        2 => -value,
        _ => value,
    }
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
    mut emerging_q: Query<(&mut CobAnimator, &mut super::spawning::Emerging)>,
    mut finished_q: Query<&mut CobAnimator, Without<super::spawning::Emerging>>,
    mut removed: RemovedComponents<super::spawning::Emerging>,
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
                        let pos = spring_linear_to_elmos(
                            cobwtf_move_axis(*axis, *destination),
                            animator.linear_constant,
                        );
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
                        animator.target_translations[p][a] = spring_linear_to_elmos(
                            cobwtf_move_axis(*axis, *destination),
                            animator.linear_constant,
                        );
                        animator.move_speeds[p][a] =
                            spring_linear_to_elmos(speed.abs(), animator.linear_constant);
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
                // EmitSfx/SetValue — not yet implemented.
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
