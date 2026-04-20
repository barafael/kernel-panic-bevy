//! Per-frame tick of every live weapon visual: fade beams, animate
//! projectile arcs, drift + billboard build-sparkles, despawn at end of life.

use bevy::prelude::*;

use super::shared::{
    BeamVisual, BuildSparkle, GroundFlash, ImpactBurst, LaserBolt, ProjectileVisual,
    TRAIL_SAMPLE_COUNT,
};
use crate::rendering::camera::RtsCamera;

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(super) fn tick_weapon_fx(
    time: Res<Time>,
    mut beams: Query<(Entity, &mut BeamVisual, &mut Transform), Without<ProjectileVisual>>,
    mut projectiles: Query<(Entity, &mut ProjectileVisual, &mut Transform)>,
    mut bolts: Query<
        (Entity, &mut LaserBolt, &mut Transform),
        (Without<BeamVisual>, Without<ProjectileVisual>),
    >,
    mut sparkles: Query<
        (Entity, &mut BuildSparkle, &mut Transform),
        (
            Without<BeamVisual>,
            Without<ProjectileVisual>,
            Without<LaserBolt>,
        ),
    >,
    mut impacts: Query<
        (Entity, &mut ImpactBurst, &mut Transform),
        (
            Without<BeamVisual>,
            Without<ProjectileVisual>,
            Without<LaserBolt>,
            Without<BuildSparkle>,
        ),
    >,
    mut flashes: Query<
        (Entity, &mut GroundFlash, &mut Transform),
        (
            Without<BeamVisual>,
            Without<ProjectileVisual>,
            Without<LaserBolt>,
            Without<BuildSparkle>,
            Without<ImpactBurst>,
        ),
    >,
    camera_q: Query<&GlobalTransform, With<RtsCamera>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    // Projectile trails + build-sparkle billboards both need the
    // camera's world position; resolve it once per frame.
    let cam_pos = camera_q
        .single()
        .map(|gt| gt.translation())
        .unwrap_or(Vec3::Y * 1000.0);

    // Hit-scan beams (BeamLaser / BuildLaser). Rewrite the 4 corners
    // each frame so the ribbon always faces the camera — same xdir
    // math as the bolt path above.
    for (entity, mut beam, _transform) in &mut beams {
        beam.lifetime -= dt;
        if beam.lifetime <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        let beam_dir = (beam.end - beam.start).try_normalize().unwrap_or(Vec3::Z);
        let mid = (beam.start + beam.end) * 0.5;
        let to_cam = (cam_pos - mid).normalize_or(Vec3::Y);
        let dir1 = to_cam
            .cross(beam_dir)
            .try_normalize()
            .unwrap_or_else(|| Vec3::Y.cross(beam_dir).try_normalize().unwrap_or(Vec3::X));
        // Fade by shrinking thickness over the lifetime.
        let fade = (beam.lifetime / beam.max_lifetime).sqrt();
        let offset = dir1 * beam.thickness * fade;
        // Match the bolt convention: U=0 at `end` (target-side, where
        // any texture's "arrow tip" / "hit end" should read), U=1 at
        // `start` (shooter-side). Keeps a textured BeamLaser like the
        // future DOS_Beam showing its `dosray` stream flowing from
        // builder to target rather than backwards.
        if let Some(mesh) = meshes.get_mut(&beam.mesh) {
            let bl = beam.end - offset;
            let br = beam.start - offset;
            let tr = beam.start + offset;
            let tl = beam.end + offset;
            mesh.insert_attribute(
                Mesh::ATTRIBUTE_POSITION,
                vec![bl.to_array(), br.to_array(), tr.to_array(), tl.to_array()],
            );
        }
    }

    // Traveling laser bolts — Spring's `CLaserProjectile::Draw`. Lead
    // advances from `origin` at `speed` until reaching the target; the
    // tail trails by up to `max_length`, then catches up once the lead
    // stops. For each live bolt we rewrite the quad's 4 vertices with
    // camera-facing corners — `dir1 = ((midpoint - cam) × beam_dir).normalize()`
    // is the width axis; the quad spans `±dir1 * thickness` at lead
    // and tail. Despawns once both ends pass the target.
    for (entity, mut bolt, _transform) in &mut bolts {
        bolt.elapsed += dt;
        let lead_raw = bolt.speed * bolt.elapsed;
        let lead_dist = lead_raw.min(bolt.total_distance);
        let tail_raw = (lead_raw - bolt.max_length).max(0.0);
        if tail_raw >= bolt.total_distance {
            commands.entity(entity).despawn();
            continue;
        }
        let tail_dist = tail_raw.min(bolt.total_distance);
        let lead_pos = bolt.origin + bolt.direction * lead_dist;
        let tail_pos = bolt.origin + bolt.direction * tail_dist;
        let mid = (lead_pos + tail_pos) * 0.5;
        let to_cam = (cam_pos - mid).normalize_or(Vec3::Y);
        // dir1 is the quad's width axis: perpendicular to both the
        // beam direction and the camera-to-bolt ray. If the camera is
        // looking straight down the bolt, fall back to the camera's
        // own right vector so the bolt stays visible head-on.
        let dir1 = to_cam
            .cross(bolt.direction)
            .try_normalize()
            .unwrap_or_else(|| {
                // Bolt viewed end-on: pick any perpendicular that lies in
                // the camera plane.
                Vec3::Y
                    .cross(bolt.direction)
                    .try_normalize()
                    .unwrap_or(Vec3::X)
            });
        let offset = dir1 * bolt.thickness;

        // Mesh UVs are fixed: bl=(0,0), br=(1,0), tr=(1,1), tl=(0,1).
        // Upstream assigns `tex1->xstart` (U=0) to the LEAD and
        // `tex1->xend` (U=1) to the TAIL (see `LaserProjectile.cpp::Draw`,
        // where `drawPos` — the lead — gets `tex1->xstart` and `pos2` —
        // the tail — gets `tex1->xend`). The `arrow.tga` atlas has its
        // chevron tips at low U, so that mapping makes the arrows read
        // as `>>>>` pointing at the target. Inverting it (tail-at-U=0)
        // flipped them to face the shooter — the regression the user
        // caught. So: LEAD corners go to bl/tl (U=0), TAIL corners go
        // to br/tr (U=1).
        if let Some(mesh) = meshes.get_mut(&bolt.mesh) {
            let bl = lead_pos - offset;
            let br = tail_pos - offset;
            let tr = tail_pos + offset;
            let tl = lead_pos + offset;
            mesh.insert_attribute(
                Mesh::ATTRIBUTE_POSITION,
                vec![bl.to_array(), br.to_array(), tr.to_array(), tl.to_array()],
            );
        }
    }

    for (entity, mut proj, mut transform) in &mut projectiles {
        let total_dist = proj.origin.distance(proj.target);
        if total_dist < 0.1 {
            despawn_projectile(entity, &mut proj, &mut commands);
            continue;
        }
        proj.progress += (proj.speed * dt) / total_dist;
        if proj.progress >= 1.0 {
            despawn_projectile(entity, &mut proj, &mut commands);
            continue;
        }

        let t = proj.progress;
        let mut pos = proj.origin.lerp(proj.target, t);
        if proj.arc_height > 0.0 {
            let arc = 4.0 * t * (1.0 - t);
            pos.y += proj.arc_height * total_dist * arc;
        }
        transform.translation = pos;

        // Advance the trail ring-buffer and rewrite the ribbon mesh.
        if let Some(trail) = &mut proj.trail {
            update_trail_samples(&mut trail.samples, pos);
            rewrite_trail_mesh(
                &mut meshes,
                &trail.mesh,
                &trail.samples,
                trail.half_width,
                cam_pos,
            );
        }
    }

    // Build-sparkle particles: drift, decay velocity (airdrag=1 in CEG kills it
    // fast), fade by shrinking the quad, billboard toward camera, despawn at
    // end of life. Material is shared, so per-particle alpha must come from
    // scale rather than mutating colour. `cam_pos` was resolved at the top of
    // the system — shared with the projectile-trail path.
    for (entity, mut sparkle, mut transform) in &mut sparkles {
        sparkle.lifetime -= dt;
        if sparkle.lifetime <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        // Drift; airdrag=1 → exponential velocity decay (~half-life 0.1s).
        transform.translation += sparkle.velocity * dt;
        sparkle.velocity *= (1.0 - dt * 7.0).max(0.0);

        // Colour map is white, white, transparent — i.e. opaque for first half,
        // then ramps to zero. Approximate by holding full size for the first
        // half of life and shrinking smoothly to zero across the second half.
        let life_frac = sparkle.lifetime / sparkle.max_lifetime;
        let fade = if life_frac > 0.5 {
            1.0
        } else {
            life_frac * 2.0
        };
        let s = sparkle.base_size * fade;

        // Billboard: face the camera while keeping world-up.
        let to_cam = (cam_pos - transform.translation).normalize_or(Vec3::Z);
        let right = Vec3::Y.cross(to_cam).normalize_or(Vec3::X);
        let up = to_cam.cross(right).normalize_or(Vec3::Y);
        transform.rotation = Quat::from_mat3(&Mat3::from_cols(right, up, to_cam));
        transform.scale = Vec3::splat(s);
    }

    // Impact bursts: scale up while fading, then despawn. Material is
    // shared (cached by color), so opacity comes from the scale curve.
    for (entity, mut impact, mut transform) in &mut impacts {
        impact.lifetime -= dt;
        if impact.lifetime <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        let life_frac = 1.0 - impact.lifetime / impact.max_lifetime;
        let scale = impact.base_size * (1.0 + life_frac * 1.5);
        transform.scale = Vec3::splat(scale);
    }

    // Ground flash ring: expand outward from 0.25× to 1.5× radius over
    // the lifetime, then collapse to zero in the final quarter to fade
    // out cleanly. Flat (XZ) scale keeps the disc hugging the ground;
    // rotation is set at spawn and never touched.
    for (entity, mut flash, mut transform) in &mut flashes {
        flash.lifetime -= dt;
        if flash.lifetime <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        let life_frac = 1.0 - flash.lifetime / flash.max_lifetime;
        let grow = 0.25 + life_frac * 1.25;
        let fade = if life_frac > 0.75 {
            (1.0 - life_frac) * 4.0
        } else {
            1.0
        };
        let r = flash.base_radius * grow * fade;
        transform.scale = Vec3::splat(r);
    }
}

/// Despawn the projectile and its companion ribbon entity (if any).
/// Called from the projectile loop at both the "arrived at target" and
/// the "origin == target" early-exit paths so the ribbon never
/// dangles without its projectile.
fn despawn_projectile(entity: Entity, proj: &mut ProjectileVisual, commands: &mut Commands) {
    if let Some(trail) = proj.trail.take() {
        commands.entity(trail.ribbon_entity).despawn();
    }
    commands.entity(entity).despawn();
}

/// Advance the ring-buffer by one step: drop the oldest sample and
/// prepend the projectile's current position at the head of the
/// buffer. `samples` is oldest-first so `last()` is always the head
/// (current projectile pos) and `first()` is the tail end.
fn update_trail_samples(samples: &mut Vec<Vec3>, head_pos: Vec3) {
    if samples.is_empty() {
        return;
    }
    samples.rotate_left(1);
    if let Some(last) = samples.last_mut() {
        *last = head_pos;
    }
}

/// Rebuild the trail's triangle-strip mesh from the sample buffer.
///
/// The ribbon is a series of 1-quad-wide segments stitched together
/// as a single triangle strip (`N` samples → `2N` vertices). Each
/// sample contributes two vertices offset by `±half_width` along a
/// camera-facing right vector (so the ribbon always reads as a flat
/// strip regardless of viewing angle). UV.u encodes "progress along
/// the trail" from 0 (oldest) to 1 (head), which lets textures with
/// baked-in gradients (firetrail.tga's yellow→transparent fade) feel
/// natural without any material mutation.
fn rewrite_trail_mesh(
    meshes: &mut Assets<Mesh>,
    handle: &Handle<Mesh>,
    samples: &[Vec3],
    half_width: f32,
    cam_pos: Vec3,
) {
    let Some(mesh) = meshes.get_mut(handle) else {
        return;
    };
    let expected = TRAIL_SAMPLE_COUNT * 2;

    let mut positions = Vec::with_capacity(expected);
    let mut uvs = Vec::with_capacity(expected);

    for (i, sample) in samples.iter().enumerate() {
        let to_cam = (cam_pos - *sample).normalize_or(Vec3::Y);
        // Tangent: the direction the ribbon runs at this sample. Use
        // the next sample when possible so the normal is authoritative
        // for the segment; fall back to the previous sample at the head.
        let tangent = if i + 1 < samples.len() {
            (samples[i + 1] - *sample).normalize_or(Vec3::Z)
        } else if i > 0 {
            (*sample - samples[i - 1]).normalize_or(Vec3::Z)
        } else {
            Vec3::Z
        };
        let right = tangent.cross(to_cam).normalize_or(Vec3::X) * half_width;
        let left = *sample - right;
        let right_pos = *sample + right;
        positions.push(left.to_array());
        positions.push(right_pos.to_array());
        let u = i as f32 / (samples.len().saturating_sub(1).max(1)) as f32;
        uvs.push([u, 0.0]);
        uvs.push([u, 1.0]);
    }

    // Strip vertex count is fixed at spawn time; if we're somehow
    // short (e.g. empty samples) pad with degenerate vertices at the
    // head so the GPU sees a consistent buffer size.
    while positions.len() < expected {
        positions.push(positions.last().copied().unwrap_or([0.0, 0.0, 0.0]));
        uvs.push([1.0, 0.0]);
    }

    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
}
