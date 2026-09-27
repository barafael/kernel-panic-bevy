//! Geovent steam animation.
//!
//! Mirrors the Spring engine's `CGeoThermSmokeProjectile`: each geovent
//! feature emits a steady stream of grey smoke puffs that rise upward,
//! grow in size, fade out, and drift.
//!
//! Upstream reference (spring/spring master):
//!   * `rts/Sim/Features/Feature.cpp::EmitGeoSmoke` — one particle per sim
//!     frame (30 Hz); spawn pos is a random sphere of radius 10 centred
//!     10 elmos below the vent, speed is `UpVector*2 + rand*0.5`, TTL is
//!     50..57 frames.
//!   * `rts/Rendering/Env/Particles/Classes/GeoThermSmokeProjectile.cpp`
//!     — extends `CSmokeProjectile` with `startSize=6`, `sizeExpansion=0.35`,
//!     `color=0.8`, alpha = `(1 - age) * 255` per draw frame.
//!
//! Translated to wall-clock time: 30 Hz emission, lifetime ≈ 1.7 s, size
//! grows by 10.5 elmos/s (0.35 * 30), puffs drawn as camera-facing quads.
//!
//! The puffs are plain data: [`tick_geovent_smoke`] moves them every
//! render frame and pushes one quad each into a [`QuadBatches`] — one
//! dynamic mesh and draw call per glyph material, like the weapon fx
//! ribbons — instead of every puff being its own `Transform` +
//! `Mesh3d` entity propagated and extracted per frame (a busy map runs
//! several hundred at once).

use bevy::prelude::*;

use spring_map::map_types::MapFeature;

use crate::{
    rendering::camera::RtsCamera,
    rng::{next_f32, random_unit_sphere, xorshift32},
    sim::{GAME_SPEED, frames_to_secs},
    terrain::heightmap::Heightmap,
    units::{
        components::UnitType,
        content::unit_registry::UnitRegistry,
        lifecycle::construction::{Constructing, PendingBuild},
        weapon_fx::batch::{FxQuadBatch, QuadBatches, flush_quad_batches},
    },
};

/// How close a building has to sit to a vent for the building to inherit
/// the vent's claim. Datavents are point features; a finished structure
/// spawns at `feature.pos` ± a tiny snap offset. 16 elmos is tight enough
/// that only the actual factory on the vent counts, not a unit that
/// happens to wander over it.
const BUILDING_OCCUPANCY_RADIUS: f32 = 16.0;

/// Emits smoke puffs from a single geovent.
#[derive(Component)]
pub struct GeoventSmoker {
    /// World position of the vent (at ground level).
    pub pos: Vec3,
    /// Seconds until the next particle emission.
    pub emit_timer: f32,
    /// Per-smoker PRNG state so each vent's jitter is independent.
    pub rng: u32,
}

/// Marker on a `GeoventSmoker` entity that has been claimed by a builder
/// or a finished building sitting on top of it. Claimed vents stop
/// emitting smoke (so the vent glow doesn't poke through the factory) and
/// are filtered out of the placement snap so a second constructor can't
/// stack another building onto the same spot. Released by
/// `release_stale_vent_claims` once neither a committed builder nor a
/// finished building occupies the vent position.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct VentClaim;

/// A single rising smoke puff.
#[derive(Component)]
pub struct GeoventSmoke {
    pub pos: Vec3,
    pub lifetime: f32,
    pub max_lifetime: f32,
    pub velocity: Vec3,
    pub start_size: f32,
    /// Growth rate in world units / s.
    pub size_expansion: f32,
    /// The "0" or "1" glyph material this puff is drawn with.
    pub material: Handle<StandardMaterial>,
}

/// Assets shared by every puff. Two materials — one textured with a "0"
/// glyph, one with a "1" — picked randomly per emission so the rising puffs
/// read as a stream of binary digits venting from the ground.
#[derive(Resource, Default)]
pub struct GeoventAssets {
    pub material_zero: Option<Handle<StandardMaterial>>,
    pub material_one: Option<Handle<StandardMaterial>>,
}

/// Corner UVs in the batch's `[bl, br, tr, tl]` order with the glyph's
/// top edge along `+up` — the layout of the `Rectangle` mesh the puffs
/// used to be drawn with, so a "1" still stands upright.
const PUFF_UVS: [[f32; 2]; 4] = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];

// Spring sim runs at 30 Hz; engine emits one puff per feature per frame.
// KP slows that down — at full rate the digit puffs cluster too thickly and
// stop reading as discrete bits. 18 Hz keeps the stream legible while still
// looking like a busy vent.
const EMIT_INTERVAL: f32 = 1.0 / 18.0;

// `startSize=6` elmos; `sizeExpansion=0.35` per frame → 10.5 elmos/s.
// `ttl=50..57` frames → 1.667..1.9 s.
const START_SIZE: f32 = 6.0;
const SIZE_EXPANSION_PER_S: f32 = 0.35 * GAME_SPEED;
const TTL_MIN_S: f32 = frames_to_secs(50.0);
const TTL_MAX_S: f32 = frames_to_secs(57.0);

// Initial speed: UpVector*2 elmos/frame + random sphere of radius 0.5/frame.
const UP_SPEED: f32 = 2.0 * GAME_SPEED;
const LATERAL_JITTER: f32 = 0.5 * GAME_SPEED;

// Spawn position: random sphere of radius 10 centred 10 elmos below the vent.
const SPAWN_RADIUS: f32 = 10.0;
const SPAWN_Y_OFFSET: f32 = -10.0;

pub fn spawn_geovent_smokers(
    features: &[MapFeature],
    heightmap: &Heightmap,
    commands: &mut Commands,
    assets: &mut GeoventAssets,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
) {
    ensure_assets(assets, materials, images);

    let mut count = 0u32;

    for feature in features {
        if !feature.feature_type.is_geovent() {
            continue;
        }
        let pos = heightmap.place(feature.x, feature.z);
        spawn_smoker_at(commands, pos);
        count += 1;
    }

    if count > 0 {
        info!("  {count} geovents (animated)");
    }
}

/// Spawn one [`GeoventSmoker`] at `pos`. Public so dynamic placers
/// (HexFarm's Lua-driven `g`-flagged hexes) can route through the same
/// path as static SMF features.
pub fn spawn_smoker_at(commands: &mut Commands, pos: Vec3) {
    // Seed the per-smoker PRNG deterministically from coords so
    // identical maps produce identical jitter across runs.
    let seed = pos.x.to_bits() ^ pos.z.to_bits().rotate_left(13);
    let mut rng = seed | 1; // avoid the all-zeros state
    let initial_timer = next_f32(&mut rng) * EMIT_INTERVAL;

    commands.spawn((
        GeoventSmoker {
            pos,
            emit_timer: initial_timer,
            rng,
        },
        Transform::from_translation(pos),
        Visibility::default(),
    ));
}

fn ensure_assets(
    assets: &mut GeoventAssets,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
) {
    if assets.material_zero.is_none() {
        assets.material_zero = Some(make_glyph_material(GLYPH_ZERO, materials, images));
    }
    if assets.material_one.is_none() {
        assets.material_one = Some(make_glyph_material(GLYPH_ONE, materials, images));
    }
}

// 8×8 pixel-art glyphs. Each row is a byte; bit 7 is the leftmost pixel.
// Set bits become opaque green; unset bits stay transparent so the digit
// silhouette reads cleanly against the dark terrain.
const GLYPH_ZERO: [u8; 8] = [
    0b00111100, 0b01100110, 0b01101110, 0b01110110, 0b01110110, 0b01101110, 0b01100110, 0b00111100,
];
const GLYPH_ONE: [u8; 8] = [
    0b00011000, 0b00111000, 0b01111000, 0b00011000, 0b00011000, 0b00011000, 0b00011000, 0b01111110,
];

fn make_glyph_material(
    glyph: [u8; 8],
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
) -> Handle<StandardMaterial> {
    let texture = images.add(glyph_to_image(glyph));
    // KP's digital aesthetic — bright neon green, additive so puffs glow.
    let green = LinearRgba::new(0.05, 0.35, 0.10, 1.0);
    materials.add(StandardMaterial {
        base_color: Color::LinearRgba(green),
        base_color_texture: Some(texture),
        emissive: green * 2.0,
        unlit: true,
        alpha_mode: AlphaMode::Add,
        cull_mode: None,
        ..default()
    })
}

fn glyph_to_image(glyph: [u8; 8]) -> Image {
    use bevy::asset::RenderAssetUsages;
    use bevy::image::{ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

    let mut data = Vec::with_capacity(8 * 8 * 4);
    for row in glyph {
        for col in 0..8 {
            let lit = (row >> (7 - col)) & 1 == 1;
            // White RGBA when lit (the material tints it green); transparent
            // black when not. Keeping RGB white avoids any srgb gamma fight
            // when the material multiplies through.
            if lit {
                data.extend_from_slice(&[255, 255, 255, 255]);
            } else {
                data.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }

    let mut image = Image::new(
        Extent3d {
            width: 8,
            height: 8,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    // Nearest filtering keeps the pixel-art look sharp instead of smearing
    // the glyph into a blob as the puff grows.
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        min_filter: ImageFilterMode::Nearest,
        mag_filter: ImageFilterMode::Nearest,
        mipmap_filter: ImageFilterMode::Nearest,
        ..default()
    });
    image
}

pub fn emit_geovent_smoke(
    time: Res<Time>,
    mut smokers: Query<&mut GeoventSmoker, Without<VentClaim>>,
    assets: Res<GeoventAssets>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    let Some(material_zero) = assets.material_zero.clone() else {
        return;
    };
    let Some(material_one) = assets.material_one.clone() else {
        return;
    };

    for mut smoker in &mut smokers {
        smoker.emit_timer -= dt;
        // Bevy frames can be longer than the emit interval; catch up by
        // emitting multiple puffs. Cap at a handful per frame to avoid
        // stalls if the game ever drops to single-digit FPS.
        let mut budget = 8;
        while smoker.emit_timer <= 0.0 && budget > 0 {
            smoker.emit_timer += EMIT_INTERVAL;
            budget -= 1;

            let offset =
                random_unit_sphere(&mut smoker.rng) * SPAWN_RADIUS + Vec3::Y * SPAWN_Y_OFFSET;
            let spawn_pos = smoker.pos + offset;

            let lateral = random_unit_sphere(&mut smoker.rng) * LATERAL_JITTER;
            let velocity = Vec3::new(lateral.x, UP_SPEED, lateral.z);

            let ttl = TTL_MIN_S + next_f32(&mut smoker.rng) * (TTL_MAX_S - TTL_MIN_S);

            // Coin-flip whether this puff is a 0 or a 1.
            let glyph_material = if xorshift32(&mut smoker.rng) & 1 == 0 {
                material_zero.clone()
            } else {
                material_one.clone()
            };

            commands.spawn(GeoventSmoke {
                pos: spawn_pos,
                lifetime: ttl,
                max_lifetime: ttl,
                velocity,
                start_size: START_SIZE,
                size_expansion: SIZE_EXPANSION_PER_S,
                material: glyph_material,
            });
        }
    }
}

/// How often `reconcile_vent_claims` scans. Sub-second granularity is
/// plenty: 250 ms of smoke still puffing out of a just-spawned vent is
/// not visually disruptive, and the equivalent add/remove lag on the
/// release side only matters when the player starts a second build on
/// the same vent.
const VENT_CLAIM_RECONCILE_INTERVAL: f32 = 0.25;

/// Throttle timer for `reconcile_vent_claims`. Kept as a resource so
/// the system can early-exit before touching any ECS query.
#[derive(Resource, Default)]
pub struct VentClaimReleaseTimer(pub f32);

/// Keep every vent's `VentClaim` in sync with whether a building (or
/// a committed / in-progress builder) occupies it.
///
/// The rule is **position-based**: a vent is claimed iff
/// (a) there's a `PendingBuild` whose `site` coincides with the vent,
/// (b) there's a `Constructing` whose `site` coincides with the vent, or
/// (c) a finished building sits within [`BUILDING_OCCUPANCY_RADIUS`] of the vent.
///
/// Adding the stamp side (previously only release was implemented) is
/// what stops the "0/1 spray keeps pouring out of a vent that already
/// has a socket / terminal built on it" visual — the starter-roster
/// spawner goes straight through [`spawn_unit`] at a datavent position
/// without touching the placement UI, so it never inserted the claim
/// itself. The reconciler picks that up on its next 250 ms tick.
///
/// Position-based reconciliation also means the natural hand-off —
/// builder finishes, building spawns at the same spot, builder walks
/// away — just works: at any moment something is at the vent, the
/// claim stays live.
#[allow(clippy::too_many_arguments)]
pub fn reconcile_vent_claims(
    time: Res<Time>,
    mut timer: ResMut<VentClaimReleaseTimer>,
    mut commands: Commands,
    vents: Query<(Entity, &GeoventSmoker, Option<&VentClaim>)>,
    pending: Query<&PendingBuild>,
    constructing: Query<&Constructing>,
    buildings: Query<(&UnitType, &GlobalTransform)>,
    unit_registry: Res<UnitRegistry>,
) {
    timer.0 += time.delta_secs();
    if timer.0 < VENT_CLAIM_RECONCILE_INTERVAL {
        return;
    }
    timer.0 = 0.0;

    let occupancy_sq = BUILDING_OCCUPANCY_RADIUS * BUILDING_OCCUPANCY_RADIUS;
    // Horizontal-only distance: `spawn_unit` lifts a building's root
    // by its model's `ground_lift`, and `Emerging` sinks it further
    // for the rise animation — both apply to Y only. Measuring 3D
    // distance to the vent (which sits at ground level) pushes a
    // building whose XZ *exactly* matches the vent outside a tight
    // 16-elmo sphere the moment its root Y diverges by 16+. Restrict
    // the check to the XZ plane so the occupancy test tracks the
    // building's footprint rather than its current emerge height.
    let horiz_dist_sq = crate::units::spatial::flat_dist_sq;
    for (vent_entity, vent, claim) in &vents {
        let builder_committed = pending
            .iter()
            .any(|p| horiz_dist_sq(p.site, vent.pos) < 1.0)
            || constructing
                .iter()
                .any(|c| horiz_dist_sq(c.site, vent.pos) < 1.0);
        let building_present = buildings.iter().any(|(ut, gtf)| {
            unit_registry.is_building(ut.0)
                && horiz_dist_sq(gtf.translation(), vent.pos) <= occupancy_sq
        });
        let should_claim = builder_committed || building_present;
        match (should_claim, claim.is_some()) {
            (true, false) => {
                commands.entity(vent_entity).insert(VentClaim);
            }
            (false, true) => {
                commands.entity(vent_entity).remove::<VentClaim>();
            }
            _ => {}
        }
    }
}

/// Move, grow and fade every puff, then rebuild the two glyph batch
/// meshes from them. Runs in `Update`, so the batch is a `Local` here
/// rather than the weapon-fx chain's per-sim-tick resource.
pub fn tick_geovent_smoke(
    time: Res<Time>,
    mut puffs: Query<(Entity, &mut GeoventSmoke)>,
    camera_q: Query<&GlobalTransform, With<RtsCamera>>,
    mut batches: Local<QuadBatches>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut batch_visibility: Query<&mut Visibility, With<FxQuadBatch>>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();

    let cam_pos = camera_q
        .single()
        .inspect_err(
            |error| warn!(%error, "geovent: camera query failed, using Vec3::Y*1000 fallback"),
        )
        .map(|gt| gt.translation())
        .unwrap_or(Vec3::Y * 1000.0);

    for (entity, mut puff) in &mut puffs {
        puff.lifetime -= dt;
        if puff.lifetime <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }

        let velocity = puff.velocity;
        puff.pos += velocity * dt;

        let elapsed = puff.max_lifetime - puff.lifetime;
        let size = puff.start_size + puff.size_expansion * elapsed;
        let age_frac = (elapsed / puff.max_lifetime).clamp(0.0, 1.0);

        // Upstream draw fades alpha linearly as (1 - age). The fade is
        // baked into quad size instead (kept from the shared-material
        // days): combined with the growth term above, puffs swell then
        // shrink back to nothing.
        let fade = 1.0 - age_frac;
        // The quad used to be a unit `Rectangle` scaled by this, so its
        // half extent is half of it.
        let half = size * fade * 0.5;

        let to_cam = (cam_pos - puff.pos).normalize_or(Vec3::Z);
        let right = Vec3::Y.cross(to_cam).normalize_or(Vec3::X) * half;
        let up = to_cam.cross(right).normalize_or(Vec3::Y) * half;
        let pos = puff.pos;
        batches.push_quad(
            &puff.material,
            [pos - right - up, pos + right - up, pos + right + up, pos - right + up],
            PUFF_UVS,
            // White: the glyph material's green tint is its `base_color`.
            [[1.0; 4]; 4],
        );
    }

    flush_quad_batches(&mut batches, &mut meshes, &mut batch_visibility, &mut commands);
}
