use std::collections::HashMap;
use std::f32::consts::PI;
use std::sync::Arc;

use bevy::prelude::*;

use spring_cob::{AnimCommand, CobFile, CobVm, parse_cob};

use super::meshes::load_asset_from_disk;

/// Component holding per-unit animation state.
#[derive(Component)]
pub struct CobAnimator {
    pub vm: CobVm,
    pub cob: Arc<CobFile>,
    /// Maps COB piece index → Bevy child entity.
    pub piece_entities: Vec<Entity>,
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
pub struct PieceIndex(pub usize);

/// Cached parsed COB files, keyed by script filename.
#[derive(Resource, Default)]
pub struct CobFileCache {
    files: HashMap<&'static str, Option<Arc<CobFile>>>,
}

/// Load a COB file from disk, cached.
pub fn load_cob_cached(script: &'static str, cache: &mut CobFileCache) -> Option<Arc<CobFile>> {
    cache
        .files
        .entry(script)
        .or_insert_with(|| load_asset_from_disk(script, |data| parse_cob(data)).map(Arc::new))
        .clone()
}

/// Spring uses "angular units" where 65536 = 360°. Convert to radians.
fn spring_angle_to_radians(angle: i32) -> f32 {
    (angle as f32) / 65536.0 * 2.0 * PI
}

/// Spring linear units: 1 unit = 1/65536 of an elmo for speeds encoded
/// in the bytecode. Positions are in raw elmos.
fn spring_linear_to_elmos(val: i32) -> f32 {
    val as f32 / 65536.0
}

/// System: tick all CobAnimator VMs and apply piece transforms.
pub fn animation_system(
    time: Res<Time>,
    mut animators: Query<(&mut CobAnimator, &Children)>,
    mut transforms: Query<&mut Transform, With<PieceIndex>>,
) {
    let dt = time.delta_secs();
    let dt_ms = (dt * 1000.0) as i32;

    for (mut animator, _children) in &mut animators {
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
                        let angle = spring_angle_to_radians(*destination);
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
                        animator.target_rotations[p][a] = spring_angle_to_radians(*destination);
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
                        let pos = spring_linear_to_elmos(*destination);
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
                        animator.target_translations[p][a] = spring_linear_to_elmos(*destination);
                        animator.move_speeds[p][a] = spring_linear_to_elmos(speed.abs());
                    }
                }
                AnimCommand::Spin {
                    piece, axis, speed, ..
                } => {
                    let p = *piece as usize;
                    let a = *axis as usize;
                    if p < animator.spin_speeds.len() && a < 3 {
                        animator.spin_speeds[p][a] = spring_angle_to_radians(*speed);
                    }
                }
                AnimCommand::StopSpin { piece, axis, .. } => {
                    let p = *piece as usize;
                    let a = *axis as usize;
                    if p < animator.spin_speeds.len() && a < 3 {
                        animator.spin_speeds[p][a] = 0.0;
                    }
                }
                // Show/Hide/EmitSfx/Explode/SetValue — handled elsewhere or ignored for now.
                _ => {}
            }
        }

        // Interpolate piece transforms and collect anim-finished events.
        let num_pieces = animator.piece_rotations.len();
        let mut turn_finished: Vec<(i32, i32)> = Vec::new();
        let mut move_finished: Vec<(i32, i32)> = Vec::new();

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

            // Apply to Bevy transform.
            if p < animator.piece_entities.len() {
                let entity = animator.piece_entities[p];
                if let Ok(mut tf) = transforms.get_mut(entity) {
                    let r = animator.piece_rotations[p];
                    let t = animator.piece_translations[p];
                    tf.rotation = Quat::from_euler(EulerRot::XYZ, r[0], r[1], r[2]);
                    tf.translation = Vec3::new(t[0], t[1], t[2]);
                }
            }
        }

        // Notify VM of completed animations.
        for (piece, axis) in turn_finished {
            animator
                .vm
                .anim_finished(spring_cob::AnimType::Turn, piece, axis);
        }
        for (piece, axis) in move_finished {
            animator
                .vm
                .anim_finished(spring_cob::AnimType::Move, piece, axis);
        }
    }
}
