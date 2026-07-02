//! Per-frame tick of every live weapon visual: fade beams, animate
//! projectile arcs, drift + billboard build-sparkles, despawn at end of life.

use bevy::mesh::VertexAttributeValues;
use bevy::prelude::*;

use super::shared::{
    BeamVisual, BuildSparkle, DelayedHit, ExplosionEvent, GroundFlash, ImpactBurst, LaserBolt,
    PendingExplosions, ProjectileVisual, TRAIL_SAMPLE_COUNT,
};
use crate::rendering::camera::RtsCamera;
use bevy::ecs::system::SystemParam;

use crate::units::combat::{CollisionVolume, DamageQueue, PendingDamage};
use crate::units::components::{Faction, TeamId, UnitType, is_friendly};
use crate::units::content::weapons::WeaponRegistry;
use crate::units::spatial::SpatialIndex;

/// Grouped read-only inputs for the volumetric mid-flight collision
/// pass: target volumes, attacker team/faction (for the friendly
/// filter), and the broad-phase spatial index. Bundled as a
/// `SystemParam` so `tick_weapon_fx` stays under Bevy's 16-arg limit.
#[derive(SystemParam)]
pub(super) struct VolumeHitCtx<'w, 's> {
    target_q: Query<'w, 's, (&'static GlobalTransform, &'static CollisionVolume), With<UnitType>>,
    attacker_q: Query<'w, 's, (&'static TeamId, &'static Faction)>,
    spatial: Res<'w, SpatialIndex>,
}

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
    delayed_hits: Query<&DelayedHit>,
    mut damage_queue: ResMut<DamageQueue>,
    mut pending_explosions: ResMut<PendingExplosions>,
    weapon_registry: Res<WeaponRegistry>,
    camera_q: Query<&GlobalTransform, With<RtsCamera>>,
    volume_ctx: VolumeHitCtx,
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
        // Decay-aware fade. With `beamdecay` < 1.0 the beam is supposed to
        // dim its color each sim frame (Spring's `BeamLaserProjectile::Update`
        // multiplies `coreCol*` / `edgeCol*` by `decay`). We honour that
        // via vertex colors below; thickness stays full. For weapons
        // without authored decay (default 1.0) we keep the legacy
        // sqrt-thickness shrink so a single-frame `beamtime` weapon
        // still reads as a smooth flash rather than a hard pop.
        let elapsed_frames = (beam.max_lifetime - beam.lifetime).max(0.0) * 30.0;
        let intensity = if beam.decay < 1.0 {
            beam.decay.powf(elapsed_frames)
        } else {
            1.0
        };
        let thickness_fade = if beam.decay < 1.0 {
            1.0
        } else {
            (beam.lifetime / beam.max_lifetime).sqrt()
        };
        let offset = dir1 * beam.thickness * thickness_fade;
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
            rewrite_quad_positions(mesh, bl, br, tr, tl);
            rewrite_quad_color(mesh, [intensity, intensity, intensity, 1.0]);
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
        let prev_lead_raw = (bolt.speed * bolt.elapsed).min(bolt.total_distance);
        bolt.elapsed += dt;
        let lead_raw = bolt.speed * bolt.elapsed;
        let lead_dist = lead_raw.min(bolt.total_distance);

        // §1.8: sweep this frame's lead segment against (1) the
        // intended target, then (2) the broader spatial-index neighbours
        // for "anyone in path". Triggering removes `DelayedHit`, so
        // the timeout fallback below is a no-op for the same bolt and
        // bolts can't double-trigger.
        let prev_lead_pos = bolt.origin + bolt.direction * prev_lead_raw;
        let curr_lead_pos = bolt.origin + bolt.direction * lead_dist;
        let hit_meta = delayed_hits.get(entity).ok();
        let target_entity = hit_meta.and_then(|h| h.target);
        let attacker_entity = hit_meta.map(|h| h.attacker);
        if let Some(impact_pos) = target_volume_hit(
            target_entity,
            prev_lead_pos,
            curr_lead_pos,
            &volume_ctx.target_q,
        ) {
            trigger_delayed_hit(
                entity,
                None,
                impact_pos,
                &delayed_hits,
                &weapon_registry,
                &mut damage_queue,
                &mut pending_explosions,
                &mut commands,
            );
        } else if let Some(attacker) = attacker_entity
            && let Some((hit_entity, impact_pos)) = broad_phase_volume_hit(
                attacker,
                target_entity,
                prev_lead_pos,
                curr_lead_pos,
                &volume_ctx.spatial,
                &volume_ctx.target_q,
                &volume_ctx.attacker_q,
            )
        {
            trigger_delayed_hit(
                entity,
                Some(hit_entity),
                impact_pos,
                &delayed_hits,
                &weapon_registry,
                &mut damage_queue,
                &mut pending_explosions,
                &mut commands,
            );
        } else if lead_raw >= bolt.total_distance {
            let impact_pos = bolt.origin + bolt.direction * bolt.total_distance;
            trigger_delayed_hit(
                entity,
                None,
                impact_pos,
                &delayed_hits,
                &weapon_registry,
                &mut damage_queue,
                &mut pending_explosions,
                &mut commands,
            );
        }
        let tail_raw = (lead_raw - bolt.max_length).max(0.0);
        if tail_raw >= bolt.total_distance {
            if let Some(caps) = bolt.caps.as_ref() {
                commands.entity(caps.lead_entity).despawn();
                commands.entity(caps.tail_entity).despawn();
            }
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
            rewrite_quad_positions(mesh, bl, br, tr, tl);
        }

        // Rewrite endcap quads (texture2). Upstream's `dir2` is the
        // camera-aligned forward axis: perpendicular to dir1 and to
        // the camera ray, pointing roughly along the bolt. Each cap
        // is a `2*thickness × thickness` quad anchored at the bolt's
        // tip, extending one thickness *outward* (forward at the
        // lead, backward at the tail).
        if let Some(caps) = bolt.caps.as_ref() {
            let dir2 = to_cam.cross(dir1).try_normalize().unwrap_or(bolt.direction);
            let cap_depth = dir2 * bolt.thickness;
            // Lead cap: extends *past* the lead in the forward direction
            // so it reads as a rounded tip at the leading edge.
            if let Some(mesh) = meshes.get_mut(&caps.lead_mesh) {
                let bl = lead_pos - offset + cap_depth;
                let br = lead_pos - offset;
                let tr = lead_pos + offset;
                let tl = lead_pos + offset + cap_depth;
                rewrite_quad_positions(mesh, bl, br, tr, tl);
            }
            // Tail cap: extends *past* the tail in the backward direction
            // for the trailing tip.
            if let Some(mesh) = meshes.get_mut(&caps.tail_mesh) {
                let bl = tail_pos - offset;
                let br = tail_pos - offset - cap_depth;
                let tr = tail_pos + offset - cap_depth;
                let tl = tail_pos + offset;
                rewrite_quad_positions(mesh, bl, br, tr, tl);
            }
        }
    }

    for (entity, mut proj, mut transform) in &mut projectiles {
        let total_dist = proj.origin.distance(proj.target);
        if total_dist < 0.1 {
            trigger_delayed_hit(
                entity,
                None,
                proj.target,
                &delayed_hits,
                &weapon_registry,
                &mut damage_queue,
                &mut pending_explosions,
                &mut commands,
            );
            despawn_projectile(entity, &mut proj, &mut commands);
            continue;
        }
        let prev_progress = proj.progress;
        proj.progress += (proj.speed * dt) / total_dist;

        // §1.8: sweep this frame's straight-line segment against (1)
        // the intended target's volume, then (2) the broader spatial
        // index for any other unit in path. The arc-height curve is
        // non-linear but the per-frame slice is short enough that a
        // straight segment is a good approximation — visible miss rate
        // vs the upstream parabolic ray test is below frame-time
        // discretisation noise. Triggering here removes `DelayedHit`,
        // so the `progress >= 1.0` fallback below is a no-op for the
        // same projectile.
        let seg_start = proj.origin.lerp(proj.target, prev_progress);
        let seg_end = proj.origin.lerp(proj.target, proj.progress.min(1.0));
        let hit_meta = delayed_hits.get(entity).ok();
        let target_entity = hit_meta.and_then(|h| h.target);
        let attacker_entity = hit_meta.map(|h| h.attacker);

        let mut intercepted = false;
        if let Some(impact_pos) =
            target_volume_hit(target_entity, seg_start, seg_end, &volume_ctx.target_q)
        {
            trigger_delayed_hit(
                entity,
                None,
                impact_pos,
                &delayed_hits,
                &weapon_registry,
                &mut damage_queue,
                &mut pending_explosions,
                &mut commands,
            );
            intercepted = true;
        } else if let Some(attacker) = attacker_entity
            && let Some((hit_entity, impact_pos)) = broad_phase_volume_hit(
                attacker,
                target_entity,
                seg_start,
                seg_end,
                &volume_ctx.spatial,
                &volume_ctx.target_q,
                &volume_ctx.attacker_q,
            )
        {
            trigger_delayed_hit(
                entity,
                Some(hit_entity),
                impact_pos,
                &delayed_hits,
                &weapon_registry,
                &mut damage_queue,
                &mut pending_explosions,
                &mut commands,
            );
            intercepted = true;
        }
        if intercepted {
            despawn_projectile(entity, &mut proj, &mut commands);
            continue;
        }

        if proj.progress >= 1.0 {
            trigger_delayed_hit(
                entity,
                None,
                proj.target,
                &delayed_hits,
                &weapon_registry,
                &mut damage_queue,
                &mut pending_explosions,
                &mut commands,
            );
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

/// Sweep a per-frame travel segment against the intended target's
/// `CollisionVolume`. Returns the world-space impact position when the
/// segment crosses the volume, `None` otherwise.
///
/// This is the §1.8 volumetric-hit work for in-flight projectiles: the
/// previous logic waited for the bolt's lead to reach the *predicted*
/// total distance and then relied on `apply_damage`'s spray-angle
/// check, which left targets that moved during flight unhit. With this
/// helper the bolt couples to the target's *current* volume each frame
/// — a moving target gets caught the moment the bolt's segment crosses
/// it. Ground-targeted shots (`target == None`) and shots whose target
/// has despawned fall through to `None` and the existing
/// total-distance fallback fires.
fn target_volume_hit(
    target: Option<Entity>,
    seg_start: Vec3,
    seg_end: Vec3,
    target_q: &Query<(&GlobalTransform, &CollisionVolume), With<UnitType>>,
) -> Option<Vec3> {
    let target = target?;
    let (tf, volume) = target_q.get(target).ok()?;
    let t = volume.ray_segment_hit(tf.translation(), seg_start, seg_end)?;
    Some(seg_start.lerp(seg_end, t))
}

/// Broad-phase second pass: find any non-friendly, non-attacker unit
/// whose `CollisionVolume` the segment crosses, returning the closest
/// (smallest `t`) candidate. Catches the "friendly walks into the
/// bolt" / "an enemy steps into the path" cases the intended-target
/// check misses.
///
/// `skip_target` is the intended target (already covered by
/// [`target_volume_hit`]) — passing it here keeps the broad-phase from
/// double-firing on the same entity. `attacker` is the unit that fired
/// the bolt; we never let a unit shoot itself, and friendlies are
/// skipped so the broad pass doesn't unintentionally introduce
/// friendly fire (matches upstream's default `collidefriendly=0`).
///
/// In the test environment the spatial index is typically empty (no
/// `rebuild_spatial_index` has run), so this returns `None` and the
/// existing intended-target / timeout paths drive behaviour. In a
/// running game the index is rebuilt at the head of every Simulate
/// frame, so this catches anyone in path.
fn broad_phase_volume_hit(
    attacker: Entity,
    skip_target: Option<Entity>,
    seg_start: Vec3,
    seg_end: Vec3,
    spatial: &SpatialIndex,
    target_q: &Query<(&GlobalTransform, &CollisionVolume), With<UnitType>>,
    attacker_q: &Query<(&TeamId, &Faction)>,
) -> Option<(Entity, Vec3)> {
    // Conservative bound on any unit's collision-volume reach; chosen
    // to comfortably exceed every shipped S3O bounding sphere. Used
    // only as a broad-phase culling pad, not as a damage radius.
    const MAX_UNIT_VOLUME_REACH: f32 = 96.0;

    let attacker_info = attacker_q.get(attacker).ok();
    let mid = seg_start.lerp(seg_end, 0.5);
    let half_len = (seg_end - seg_start).length() * 0.5;
    let radius = half_len + MAX_UNIT_VOLUME_REACH;

    let mut best: Option<(Entity, f32)> = None;
    spatial.query_radius(mid, radius, |entry| {
        if entry.entity == attacker {
            return;
        }
        if Some(entry.entity) == skip_target {
            return;
        }
        if let Some((atk_team, atk_faction)) = attacker_info
            && is_friendly(entry.team, entry.faction, atk_team.0, *atk_faction)
        {
            return;
        }
        let Ok((tf, volume)) = target_q.get(entry.entity) else {
            return;
        };
        if let Some(t) = volume.ray_segment_hit(tf.translation(), seg_start, seg_end) {
            if best.is_none_or(|(_, prev_t)| t < prev_t) {
                best = Some((entry.entity, t));
            }
        }
    });

    best.map(|(entity, t)| (entity, seg_start.lerp(seg_end, t)))
}

/// Fire the one-shot impact payload riding on a traveling visual: push
/// the deferred [`PendingDamage`] onto [`DamageQueue`] and enqueue the
/// weapon's impact CEG as an [`ExplosionEvent`], then remove the
/// component so the still-visible bolt tail can't re-trigger. No-op
/// for entities without a [`DelayedHit`] (hitscan beams / build-lasers
/// whose damage settled at spawn time).
#[allow(clippy::too_many_arguments)]
fn trigger_delayed_hit(
    entity: Entity,
    target_override: Option<Entity>,
    impact_pos: Vec3,
    delayed_hits: &Query<&DelayedHit>,
    weapon_registry: &WeaponRegistry,
    damage_queue: &mut DamageQueue,
    pending_explosions: &mut PendingExplosions,
    commands: &mut Commands,
) {
    let Ok(hit) = delayed_hits.get(entity) else {
        return;
    };
    // `target_override` wins when the broad-phase intercepted a unit
    // other than the originally-aimed-at one. Otherwise stick with
    // `hit.target` (which may itself be `None` for ground-targeted
    // shots).
    let final_target = target_override.or(hit.target);
    damage_queue.push(PendingDamage {
        target: final_target,
        attacker: hit.attacker,
        weapon: hit.weapon.to_string(),
        impact_pos,
        attacker_distance: hit.attacker_distance,
    });
    let (rgb, radius, ceg_name) = weapon_registry
        .get(&hit.weapon)
        .map_or(([0.7; 3], 4.0, String::new()), |w| {
            (w.rgb_color, w.area_of_effect, w.explosion_generator.clone())
        });
    pending_explosions.events.push(ExplosionEvent {
        pos: impact_pos,
        rgb,
        radius,
        ceg_name,
    });
    commands.entity(entity).remove::<DelayedHit>();
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
fn update_trail_samples(samples: &mut [Vec3], head_pos: Vec3) {
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
    let denom = (samples.len().saturating_sub(1).max(1)) as f32;

    // Pre-allocated at spawn (`build_projectile_trail`); mutate in place so
    // we don't allocate `expected × 12` bytes per live projectile per frame.
    let Some(VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
    else {
        return;
    };
    let mut write = 0usize;
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
        positions[write] = (*sample - right).to_array();
        positions[write + 1] = (*sample + right).to_array();
        write += 2;
    }
    // Pad any unused tail slots with the last valid vertex so the GPU sees
    // a stable buffer size (trail shorter than the ring buffer capacity).
    let pad = positions
        .get(write.saturating_sub(1))
        .copied()
        .unwrap_or([0.0; 3]);
    for slot in positions[write..expected].iter_mut() {
        *slot = pad;
    }

    // UVs also need rewriting each frame because `samples.len()` determines
    // the `u` gradient. Same in-place pattern.
    if let Some(VertexAttributeValues::Float32x2(uvs)) = mesh.attribute_mut(Mesh::ATTRIBUTE_UV_0) {
        let mut write = 0usize;
        for i in 0..samples.len() {
            let u = i as f32 / denom;
            uvs[write] = [u, 0.0];
            uvs[write + 1] = [u, 1.0];
            write += 2;
        }
        for slot in uvs[write..expected].iter_mut() {
            *slot = [1.0, 0.0];
        }
    }
}

/// Rewrite a 4-vertex quad's positions in place (shared by beam + bolt
/// paths). Uses `Mesh::attribute_mut` to mutate the existing
/// `Float32x3` buffer instead of allocating a fresh `Vec` every frame.
/// The vertex order matches [`super::shared::build_billboard_quad_mesh`].
fn rewrite_quad_positions(mesh: &mut Mesh, bl: Vec3, br: Vec3, tr: Vec3, tl: Vec3) {
    if let Some(VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
        && positions.len() >= 4
    {
        positions[0] = bl.to_array();
        positions[1] = br.to_array();
        positions[2] = tr.to_array();
        positions[3] = tl.to_array();
    }
}

/// Set all 4 quad vertex colors to `rgba`. Used by the beam path to
/// apply per-frame `beamdecay` intensity without per-beam material
/// clones: the cached material's `base_color` carries the weapon's
/// authored RGB, vertex color carries the time-varying multiplier,
/// and Bevy's StandardMaterial multiplies them on the GPU.
fn rewrite_quad_color(mesh: &mut Mesh, rgba: [f32; 4]) {
    if let Some(VertexAttributeValues::Float32x4(colors)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_COLOR)
        && colors.len() >= 4
    {
        for slot in colors.iter_mut().take(4) {
            *slot = rgba;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    /// Traveling weapons MUST defer damage + impact CEG until the bolt's
    /// lead reaches the target. Bolt flies for 1 s: first tick
    /// (mid-flight) leaves queues empty; second tick (impact) pushes
    /// one damage + one explosion and removes the component; third
    /// tick (tail still trailing) must not re-fire.
    #[test]
    fn laser_bolt_defers_damage_until_lead_reaches_target() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingExplosions>()
            .init_resource::<WeaponRegistry>()
            .init_resource::<SpatialIndex>()
            .init_resource::<Assets<Mesh>>();

        let attacker = app.world_mut().spawn_empty().id();
        let target = app.world_mut().spawn_empty().id();

        // Bolt geometry: 100 elmos at 100 elmos/s → impact at t=1.0.
        // max_length=50 so tail takes another 0.5 s to clear.
        let mesh_handle = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(super::super::shared::build_billboard_quad_mesh());
        let bolt_entity = app
            .world_mut()
            .spawn((
                LaserBolt {
                    origin: Vec3::ZERO,
                    direction: Vec3::Z,
                    total_distance: 100.0,
                    speed: 100.0,
                    max_length: 50.0,
                    thickness: 1.0,
                    elapsed: 0.0,
                    mesh: mesh_handle,
                    caps: None,
                },
                Transform::IDENTITY,
                DelayedHit {
                    target: Some(target),
                    attacker,
                    weapon: std::borrow::Cow::Borrowed("TestLaser"),
                    attacker_distance: 100.0,
                },
            ))
            .id();

        // Tick 1: advance 0.5 s — lead at 50 elmos, not yet at target.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(500));
        app.world_mut().run_system_once(tick_weapon_fx).unwrap();
        assert!(app.world().resource::<DamageQueue>().is_empty());
        assert!(
            app.world()
                .resource::<PendingExplosions>()
                .events
                .is_empty()
        );
        assert!(app.world().get::<DelayedHit>(bolt_entity).is_some());

        // Tick 2: advance another 0.5 s → lead now at 100 elmos = impact.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(500));
        app.world_mut().run_system_once(tick_weapon_fx).unwrap();
        assert_eq!(app.world().resource::<DamageQueue>().len(), 1);
        assert_eq!(app.world().resource::<PendingExplosions>().events.len(), 1);
        assert!(
            app.world().get::<DelayedHit>(bolt_entity).is_none(),
            "component removed after firing so later ticks can't re-trigger",
        );

        // Tick 3: tail still trailing — must not re-fire.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(200));
        app.world_mut().run_system_once(tick_weapon_fx).unwrap();
        assert_eq!(app.world().resource::<DamageQueue>().len(), 1);
        assert_eq!(app.world().resource::<PendingExplosions>().events.len(), 1);
    }

    /// Bolts without a `DelayedHit` (hitscan beams / build-lasers come
    /// through here too for the width-axis rewrite) must NOT touch
    /// `DamageQueue` or `PendingExplosions` — their damage path settles
    /// at fire time via combat's `PendingDamage` push.
    #[test]
    fn laser_bolt_without_delayed_hit_leaves_queues_empty() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingExplosions>()
            .init_resource::<WeaponRegistry>()
            .init_resource::<SpatialIndex>()
            .init_resource::<Assets<Mesh>>();

        let mesh_handle = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(super::super::shared::build_billboard_quad_mesh());
        app.world_mut().spawn((
            LaserBolt {
                origin: Vec3::ZERO,
                direction: Vec3::Z,
                total_distance: 10.0,
                speed: 100.0,
                max_length: 20.0,
                thickness: 1.0,
                elapsed: 0.0,
                mesh: mesh_handle,
                caps: None,
            },
            Transform::IDENTITY,
        ));

        // Advance well past impact.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(1));
        app.world_mut().run_system_once(tick_weapon_fx).unwrap();

        assert!(app.world().resource::<DamageQueue>().is_empty());
        assert!(
            app.world()
                .resource::<PendingExplosions>()
                .events
                .is_empty()
        );
    }

    /// §1.8 mid-flight volumetric hit. Target sits at z=70 with a
    /// 5-elmo collision sphere; the predicted impact at z=100 is
    /// behind it. The first tick advances the bolt past the target
    /// volume — `target_volume_hit` should fire damage at the actual
    /// crossing point (z ≈ 65), NOT at the predicted z=100.
    #[test]
    fn laser_bolt_intercepts_target_volume_before_predicted_impact() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingExplosions>()
            .init_resource::<WeaponRegistry>()
            .init_resource::<SpatialIndex>()
            .init_resource::<Assets<Mesh>>();

        let attacker = app.world_mut().spawn_empty().id();
        let target = app
            .world_mut()
            .spawn((
                Transform::from_translation(Vec3::new(0.0, 0.0, 70.0)),
                GlobalTransform::from(Transform::from_translation(Vec3::new(0.0, 0.0, 70.0))),
                CollisionVolume::sphere(5.0),
                UnitType(crate::units::content::definitions::UnitKind::Bit),
            ))
            .id();

        let mesh_handle = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(super::super::shared::build_billboard_quad_mesh());

        app.world_mut().spawn((
            LaserBolt {
                origin: Vec3::ZERO,
                direction: Vec3::Z,
                total_distance: 100.0,
                speed: 100.0,
                max_length: 50.0,
                thickness: 1.0,
                elapsed: 0.0,
                mesh: mesh_handle,
                caps: None,
            },
            Transform::IDENTITY,
            DelayedHit {
                target: Some(target),
                attacker,
                weapon: std::borrow::Cow::Borrowed("TestLaser"),
                attacker_distance: 100.0,
            },
        ));

        // Tick 1: 0.7 s → lead advances 0..70. Target volume sits at
        // z=70 ± 5, so the segment 0→70 crosses the front of the
        // sphere at z = 65.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(700));
        app.world_mut().run_system_once(tick_weapon_fx).unwrap();

        let queue = app.world().resource::<DamageQueue>();
        assert_eq!(queue.len(), 1);
        let impact_z = queue
            .iter_snapshot_for_test()
            .next()
            .expect("damage entry")
            .impact_pos
            .z;
        assert!(
            (impact_z - 65.0).abs() < 0.5,
            "impact should land on the sphere front (~65), got {impact_z}",
        );
    }

    /// Broad-phase: a hostile unit standing in the bolt's path —
    /// not the intended target — should intercept the shot, with the
    /// resolved `PendingDamage::target` pointing at the interloper.
    /// Friendly units sharing the attacker's team are skipped entirely
    /// (matches upstream's default `collidefriendly=0`).
    #[test]
    fn laser_bolt_broad_phase_intercepts_enemy_in_path_skips_friendly() {
        use crate::units::components::Faction;
        use crate::units::content::definitions::UnitKind;
        use crate::units::spatial::SpatialEntry;

        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingExplosions>()
            .init_resource::<WeaponRegistry>()
            .init_resource::<SpatialIndex>()
            .init_resource::<Assets<Mesh>>();

        // Attacker on team 0 / System.
        let attacker = app.world_mut().spawn((TeamId(0), Faction::System)).id();

        // Friendly unit at z=30, directly on the bolt's path. Same
        // team + faction → must be skipped.
        let friendly = app
            .world_mut()
            .spawn((
                Transform::from_translation(Vec3::new(0.0, 0.0, 30.0)),
                GlobalTransform::from(Transform::from_translation(Vec3::new(0.0, 0.0, 30.0))),
                CollisionVolume::sphere(5.0),
                UnitType(UnitKind::Bit),
                TeamId(0),
                Faction::System,
            ))
            .id();

        // Hostile interloper at z=50, also on the path — not the
        // intended target. Should soak the broad-phase hit.
        let enemy = app
            .world_mut()
            .spawn((
                Transform::from_translation(Vec3::new(0.0, 0.0, 50.0)),
                GlobalTransform::from(Transform::from_translation(Vec3::new(0.0, 0.0, 50.0))),
                CollisionVolume::sphere(5.0),
                UnitType(UnitKind::Bug),
                TeamId(1),
                Faction::Hacker,
            ))
            .id();

        // Intended target way out at z=100.
        let intended = app
            .world_mut()
            .spawn((
                Transform::from_translation(Vec3::new(0.0, 0.0, 100.0)),
                GlobalTransform::from(Transform::from_translation(Vec3::new(0.0, 0.0, 100.0))),
                CollisionVolume::sphere(5.0),
                UnitType(UnitKind::Bug),
                TeamId(1),
                Faction::Hacker,
            ))
            .id();

        // Populate the spatial index with all three on-path units.
        let entries = [
            (friendly, Vec3::new(0.0, 0.0, 30.0), 0u8, Faction::System),
            (enemy, Vec3::new(0.0, 0.0, 50.0), 1u8, Faction::Hacker),
            (intended, Vec3::new(0.0, 0.0, 100.0), 1u8, Faction::Hacker),
        ];
        let mut spatial = app.world_mut().resource_mut::<SpatialIndex>();
        for (entity, pos, team, faction) in entries {
            spatial.insert_for_test(SpatialEntry {
                entity,
                pos,
                team,
                faction,
                kind: UnitKind::Bit,
                hp_positive: true,
                is_flying: false,
            });
        }

        let mesh_handle = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(super::super::shared::build_billboard_quad_mesh());

        app.world_mut().spawn((
            LaserBolt {
                origin: Vec3::ZERO,
                direction: Vec3::Z,
                total_distance: 100.0,
                speed: 100.0,
                max_length: 50.0,
                thickness: 1.0,
                elapsed: 0.0,
                mesh: mesh_handle,
                caps: None,
            },
            Transform::IDENTITY,
            DelayedHit {
                target: Some(intended),
                attacker,
                weapon: std::borrow::Cow::Borrowed("TestLaser"),
                attacker_distance: 100.0,
            },
        ));

        // Tick: 0.6 s — lead advances 0..60. Segment crosses
        // friendly's volume at z=25 first, but the friendly filter
        // skips it; the enemy at z=50 intercepts at z≈45.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(600));
        app.world_mut().run_system_once(tick_weapon_fx).unwrap();

        let queue = app.world().resource::<DamageQueue>();
        assert_eq!(queue.len(), 1, "exactly one hit (the enemy interloper)");
        let damage = queue.iter_snapshot_for_test().next().expect("damage entry");
        assert_eq!(
            damage.target,
            Some(enemy),
            "broad-phase must redirect damage to the interloper, NOT the original target",
        );
        assert!(
            (damage.impact_pos.z - 45.0).abs() < 0.5,
            "impact should land on enemy's near face (~45), got {}",
            damage.impact_pos.z,
        );
    }

    /// Mid-flight interception only fires when the bolt's segment
    /// actually crosses the target volume. A target offset off the
    /// bolt's flight line should NOT receive an early hit; the bolt
    /// continues to the predicted total_distance and the existing
    /// fallback path triggers there.
    #[test]
    fn laser_bolt_does_not_intercept_off_axis_target() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingExplosions>()
            .init_resource::<WeaponRegistry>()
            .init_resource::<SpatialIndex>()
            .init_resource::<Assets<Mesh>>();

        let attacker = app.world_mut().spawn_empty().id();
        // Target way off to the side — bolt path is along +Z so
        // segment never enters the target's x=50 sphere.
        let target = app
            .world_mut()
            .spawn((
                Transform::from_translation(Vec3::new(50.0, 0.0, 70.0)),
                GlobalTransform::from(Transform::from_translation(Vec3::new(50.0, 0.0, 70.0))),
                CollisionVolume::sphere(5.0),
                UnitType(crate::units::content::definitions::UnitKind::Bit),
            ))
            .id();

        let mesh_handle = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(super::super::shared::build_billboard_quad_mesh());

        app.world_mut().spawn((
            LaserBolt {
                origin: Vec3::ZERO,
                direction: Vec3::Z,
                total_distance: 100.0,
                speed: 100.0,
                max_length: 50.0,
                thickness: 1.0,
                elapsed: 0.0,
                mesh: mesh_handle,
                caps: None,
            },
            Transform::IDENTITY,
            DelayedHit {
                target: Some(target),
                attacker,
                weapon: std::borrow::Cow::Borrowed("TestLaser"),
                attacker_distance: 100.0,
            },
        ));

        // Tick: 0.7 s — lead at z=70, well past the off-axis target.
        // Mid-flight check must miss; lead has not yet reached z=100,
        // so no fallback fires either. Queue stays empty.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(700));
        app.world_mut().run_system_once(tick_weapon_fx).unwrap();
        assert!(
            app.world().resource::<DamageQueue>().is_empty(),
            "off-axis target must not trigger mid-flight interception",
        );

        // Tick 2: another 0.4 s → lead reaches 110 (clamped 100).
        // Fallback at total_distance now fires with the predicted
        // impact at z=100, NOT at the off-axis target's position.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(400));
        app.world_mut().run_system_once(tick_weapon_fx).unwrap();
        let queue = app.world().resource::<DamageQueue>();
        assert_eq!(queue.len(), 1);
        let impact = queue.iter_snapshot_for_test().next().unwrap().impact_pos;
        assert!(
            (impact.x - 0.0).abs() < 1e-3 && (impact.z - 100.0).abs() < 1e-3,
            "fallback impact should be at predicted total_distance (0,0,100), got {impact:?}",
        );
    }
}
