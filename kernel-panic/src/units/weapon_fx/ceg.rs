//! Custom Explosion Generator (CEG) runtime.
//!
//! Parsing now lives in [`spring_tdf::ExplosionDefs`] (typed `CegExpr`,
//! `ColorMap`, `EmitVector`, …). This module keeps only:
//!
//! - [`CegRegistry`]: the merged [`spring_tdf::ExplosionDefs`] of every
//!   `gamedata/explosions/*.tdf` (from the unit bundle) plus atlas-alias
//!   resolution (`circle` → `whitecircle.tga`).
//! - [`spawn_ceg`]: walks an [`ExplosionDef`]'s effect layers and
//!   instantiates the right runtime object for each class —
//!   `CSimpleParticleSystem` becomes a batch of [`CegParticle`]s,
//!   `CBitmapMuzzleFlame` becomes a [`CegFlame`] billboard,
//!   `CExpGenSpawner` becomes a [`CegDelayedSpawn`] timer that replays
//!   its target CEG on fire.
//! - [`tick_ceg_particles`] + [`tick_ceg_flames`] + [`tick_ceg_delayed_spawns`]:
//!   per-frame integration + despawn. Particles and flames are plain
//!   data drawn as camera-facing quads in the shared
//!   [`FxQuadBatches`] mesh of their texture's material, their
//!   `colorMap` colour on the vertices.
//!
//! All ambient "spread" math (`X rN`, `X iN`, `X dN`) is handled by
//! [`CegExpr::eval`] with an [`EvalCtx`] populated per particle — the
//! runtime never touches the raw TDF strings.

use std::collections::HashMap;

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use spring_tdf::{
    CegExpr, ColorMap, EffectProperties, EmitVector, EvalCtx, ExplosionDef, ExplosionDefs,
    FlameProperties, ParticleProperties, SpawnerProperties,
};

use super::batch::FxQuadBatches;
use crate::rng;
use crate::sim::GAME_SPEED;
use crate::units::assets::meshes::{S3OModelCache, load_beam_texture};

/// Registry of every parsed CEG. Internally wraps
/// [`spring_tdf::ExplosionDefs`] so parsing stays in one place.
#[derive(Resource, Default)]
pub struct CegRegistry {
    defs: ExplosionDefs,
}

/// Render assets every CEG spawn shares: one additive material per
/// texture (particles, flames), the spike material, and the colour
/// maps the live particles sample.
#[derive(Resource, Default)]
pub(super) struct CegRenderAssets {
    /// Shared additive `laserend` material for [`CegSpike`] streaks
    /// (per-spike colour rides on vertex colours).
    pub spike_material: Option<Handle<StandardMaterial>>,
    /// Texture → its white additive material. Particles and flames of
    /// every colour map draw through the same one — the colour is a
    /// vertex attribute of the batch quad — so a fight's whole particle
    /// load is one draw call per texture in use.
    materials_by_texture: HashMap<AssetId<Image>, Handle<StandardMaterial>>,
    palettes: Vec<CegPalette>,
}

/// One (texture, `colorMap`) pair: the material its particles draw
/// with and the map they sample their vertex colour from. Deduplicated
/// so a particle only carries an index.
pub(super) struct CegPalette {
    material: Handle<StandardMaterial>,
    color_map: ColorMap,
}

impl CegPalette {
    /// Upstream `colorMap[life]`: the colour at life fraction `frac`
    /// (0 = born, 1 = dead), interpolated between the map's stops.
    pub(super) fn sample(&self, frac: f32) -> [f32; 4] {
        self.color_map.sample(frac)
    }
}

impl CegRenderAssets {
    /// The additive, unlit material for `texture`, created on first use.
    ///
    /// `base_color` stays white: upstream multiplies the texture by
    /// `colorMap[life]` with no emissive boost — the colour IS the
    /// brightness under additive (GL_ONE/GL_ONE) blending, where
    /// brightness comes from stacking translucent particles. That
    /// colour is the batch quad's vertex colour here, which
    /// `StandardMaterial` multiplies into `base_color` the same way.
    /// An emissive multiplier would over-saturate every particle after
    /// tonemapping and turn the faint blue shot1 puff into a flashbang.
    fn texture_material(
        &mut self,
        texture: &Handle<Image>,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        self.materials_by_texture
            .entry(texture.id())
            .or_insert_with(|| {
                materials.add(StandardMaterial {
                    base_color: Color::WHITE,
                    base_color_texture: Some(texture.clone()),
                    unlit: true,
                    alpha_mode: AlphaMode::Add,
                    cull_mode: None,
                    ..default()
                })
            })
            .clone()
    }

    /// Palette for `texture` × `color_map`, created on first use. Looked
    /// up once per effect spawn, not per particle.
    fn palette(
        &mut self,
        texture: &Handle<Image>,
        color_map: &ColorMap,
        materials: &mut Assets<StandardMaterial>,
    ) -> usize {
        let material = self.texture_material(texture, materials);
        if let Some(i) = self
            .palettes
            .iter()
            .position(|p| p.material == material && p.color_map == *color_map)
        {
            return i;
        }
        self.palettes.push(CegPalette {
            material,
            color_map: color_map.clone(),
        });
        self.palettes.len() - 1
    }

    /// The palette a particle was spawned with.
    pub(super) fn palette_at(&self, index: usize) -> &CegPalette {
        &self.palettes[index]
    }
}

/// Corner UVs for a particle or flame billboard, in the batch's
/// `[bl, br, tr, tl]` corner order: the image's top edge lies along
/// `+ydir`, as it did on the `Rectangle` quad these used to be drawn
/// with, so `hline` / `arrow` / `dosray` sprites keep their orientation.
const BILLBOARD_UVS: [[f32; 2]; 4] = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];

/// Push one billboard centred on `pos` spanning `±xdir·size ± ydir·size`
/// — `CSimpleParticleSystem::Draw`'s four `AddVertexTC` calls — with
/// `color` on every corner.
fn push_billboard(
    batches: &mut FxQuadBatches,
    material: &Handle<StandardMaterial>,
    pos: Vec3,
    xdir: Vec3,
    ydir: Vec3,
    size: f32,
    color: [f32; 4],
) {
    let x = xdir * size;
    let y = ydir * size;
    batches.push_quad(
        material,
        [pos - x - y, pos + x - y, pos + x + y, pos - x + y],
        BILLBOARD_UVS,
        [color; 4],
    );
}

/// Bundled CEG-spawn dependencies so `tick_weapon_fx` can replay
/// projectiles' `cegTag` trails (`corruption_BCtrail`) mid-flight
/// without pushing the system past Bevy's arg limit. Mirrors the
/// `VolumeHitCtx` pattern in `tick.rs`.
#[derive(SystemParam)]
pub(super) struct CegTrailCtx<'w, 's> {
    pub ceg_registry: Res<'w, CegRegistry>,
    pub materials: ResMut<'w, Assets<StandardMaterial>>,
    pub images: ResMut<'w, Assets<Image>>,
    pub model_cache: ResMut<'w, S3OModelCache>,
    pub ceg_assets: ResMut<'w, CegRenderAssets>,
    pub _marker: std::marker::PhantomData<&'s ()>,
}

impl CegRegistry {
    /// The baked `gamedata/explosions/*.tdf` from the unit bundle.
    pub fn load() -> Self {
        let registry = Self::from_defs(crate::units::content::bundle::bundle().explosions.clone());
        info!(
            "CEG registry: {} explosion generators loaded",
            registry.defs.explosions.len()
        );
        registry
    }

    /// Wrap parsed explosion defs (bundle-decoded or freshly parsed).
    pub fn from_defs(defs: ExplosionDefs) -> Self {
        Self { defs }
    }

    /// Look up a CEG by section name. Accepts both the raw name and the
    /// `custom:NAME` prefix the weapon TDFs use.
    pub fn get(&self, name: &str) -> Option<&ExplosionDef> {
        self.defs.get(name)
    }

    /// Re-shape the texture name the CEG authored (e.g. `circle`) into
    /// the on-disk filename via the same `RESOURCES.TDF` mapping the
    /// weapon atlases use. The subset reproduced here covers every
    /// texture referenced by CEGs KP actually ships; anything else
    /// returns `None`.
    pub fn resolve_texture(tex_name: &str) -> Option<&'static str> {
        let name = tex_name.trim();
        CEG_TEXTURES
            .iter()
            .find(|(alias, _)| alias.eq_ignore_ascii_case(name))
            .map(|&(_, file)| file)
    }
}

/// CEG texture alias → atlas file (`RESOURCES.TDF` subset). `none`
/// and unknown aliases resolve to nothing.
const CEG_TEXTURES: &[(&str, &str)] = &[
    ("circle", "whitecircle.tga"),
    ("hcircle", "hollowcircle.tga"),
    ("square", "solidwhite.tga"),
    ("squarehollow", "hollowsquare.tga"),
    ("squaretrans", "transparentwhite.tga"),
    ("hline", "horizontalline.tga"),
    ("vline", "verticalline.tga"),
    ("dosray", "dosray.tga"),
    ("arrow", "arrow.tga"),
    ("arrownoends", "arrownoends.tga"),
    ("arrowflare", "arrowflare.tga"),
    ("bytelaser", "bytemegabeam.tga"),
    ("bytelasermid", "bytemegabeammid.tga"),
    ("heart", "heart.tga"),
    ("shockwave", "shockwave.tga"),
    ("black", "black.tga"),
    ("linkbeam", "linkbeam.tga"),
    ("hexgrid", "hexgrid.tga"),
    ("hexgridhole", "hexgridhole.tga"),
    ("hexastar", "hexastar.tga"),
    ("pointertrail", "pointershottrail.tga"),
    ("flowtrail", "flowtrail1.tga"),
    ("firetrail", "firetrail.tga"),
    ("sparkle", "sparkle.tga"),
    ("bubbles", "bubbles.tga"),
    ("lobedincantation", "lobedincantation.tga"),
    // Engine default atlas (`ProjectileDrawer`'s `laserendtex`), used by
    // `explspike` streaks.
    ("laserend", "laserend.tga"),
];

// ─── Runtime components ─────────────────────────────────────────────

/// One live particle spawned from a `CSimpleParticleSystem`. Plain
/// data: no `Transform` or mesh — [`tick_ceg_particles`] pushes it into
/// the quad batch of its palette's material every sim tick.
#[derive(Component)]
pub(super) struct CegParticle {
    pub pos: Vec3,
    pub velocity: Vec3,
    pub gravity: Vec3,
    pub airdrag_per_sec: f32,
    pub size: f32,
    pub size_growth_per_sec: f32,
    pub size_mod_per_sec: f32,
    /// Index into [`CegRenderAssets`]' palettes.
    pub palette: usize,
    pub life: f32,
    pub max_life: f32,
    pub directional: bool,
}

/// One live `CBitmapMuzzleFlame` billboard — used for shockwaves
/// (expanding circle ring) and muzzle-exhaust cones. Batched like
/// [`CegParticle`].
#[derive(Component)]
pub(super) struct CegFlame {
    pub pos: Vec3,
    pub life_frames: f32,
    pub max_life_frames: f32,
    pub base_size: f32,
    pub size_growth_per_frame: f32,
    /// Index into [`CegRenderAssets`]' palettes.
    pub palette: usize,
}

/// One live `CExploSpikeProjectile` (`class=explspike`) — upstream
/// `Rendering/Env/Particles/Classes/ExploSpikeProjectile.cpp`: a
/// camera-facing `laserend` streak from `pos − dir·length` to
/// `pos + dir·length`, `width` wide, whose length grows by
/// `length_growth` and alpha drops by `alpha_decay` every frame; gone
/// at alpha 0. Drawn additively with colour `alpha · color`, as one
/// quad per tick in the batch for the shared spike material.
#[derive(Component)]
pub(super) struct CegSpike {
    pub pos: Vec3,
    /// Unnormalised (`dir /= lengthGrowth` in `Init`).
    pub dir: Vec3,
    pub length: f32,
    pub length_growth: f32,
    pub width: f32,
    pub alpha: f32,
    pub alpha_decay: f32,
    pub color: Vec3,
    pub material: Handle<StandardMaterial>,
}

/// A scheduled recursive CEG spawn (CExpGenSpawner).
///
/// Each `count` iteration of the spawner becomes one `CegDelayedSpawn`
/// entity with its own `delay_frames` countdown. When the timer hits
/// zero the runtime calls `spawn_ceg` again with `target_ceg`; the
/// entity then despawns.
#[derive(Component)]
pub(super) struct CegDelayedSpawn {
    pub delay_secs: f32,
    pub pos: Vec3,
    pub dir: Vec3,
    pub target_ceg: String,
}

// ─── Spawning ───────────────────────────────────────────────────────

/// Spawn a CEG at `pos` with firing direction `dir`. Returns true iff
/// the CEG name was found; a missing entry lets callers fall back to a
/// synthesised burst.
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_ceg(
    ceg_name: &str,
    pos: Vec3,
    dir: Vec3,
    registry: &CegRegistry,
    rng: &mut u32,
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    ceg_assets: &mut CegRenderAssets,
) -> bool {
    let Some(def) = registry.get(ceg_name) else {
        return false;
    };

    let dir = dir.normalize_or(Vec3::Y);

    for effect in &def.effects {
        match &effect.properties {
            EffectProperties::Particle(p) => spawn_particle_system(
                effect.count,
                p,
                pos,
                dir,
                rng,
                commands,
                materials,
                images,
                model_cache,
                ceg_assets,
            ),
            EffectProperties::Flame(f) => spawn_flame(
                effect.count,
                f,
                pos,
                commands,
                materials,
                images,
                model_cache,
                ceg_assets,
            ),
            EffectProperties::Spawner(s) => {
                spawn_delayed(effect.count, s, pos, dir, commands);
            }
            EffectProperties::Spike(sp) => spawn_spikes(
                effect.count,
                sp,
                pos,
                rng,
                commands,
                materials,
                images,
                model_cache,
                ceg_assets,
            ),
            EffectProperties::Raw(_) => {
                // Unsupported class (CStars, etc.) — silently skipped.
            }
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn spawn_particle_system(
    count: u32,
    props: &ParticleProperties,
    origin: Vec3,
    dir: Vec3,
    rng: &mut u32,
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    ceg_assets: &mut CegRenderAssets,
) {
    let Some(filename) = CegRegistry::resolve_texture(&props.texture) else {
        return;
    };
    let Some((tex, _w, _h)) = load_beam_texture(filename, model_cache, images) else {
        return;
    };
    let palette = ceg_assets.palette(&tex, &props.color_map, materials);

    let base_dir = match &props.emit_vector {
        EmitVector::Direction => dir,
        EmitVector::Literal(v) => {
            Vec3::from_array(v.eval(&EvalCtx::default())).normalize_or(Vec3::Y)
        }
    };

    for fire_index in 0..count {
        let n_particles = props
            .num_particles
            .eval(&EvalCtx {
                index: fire_index,
                ..Default::default()
            })
            .round()
            .max(1.0) as u32;

        for particle_index in 0..n_particles {
            let ctx_base = EvalCtx {
                index: fire_index * n_particles + particle_index,
                damage: 0.0,
                rand01: next_unit(rng),
            };

            let life_frames = eval_with_spread(
                &props.particle_life,
                &props.particle_life_spread,
                rng,
                ctx_base,
            )
            .max(1.0);
            let life_secs = life_frames / GAME_SPEED;

            let size = eval_with_spread(
                &props.particle_size,
                &props.particle_size_spread,
                rng,
                ctx_base,
            )
            .max(0.1);

            let speed_per_frame = eval_with_spread(
                &props.particle_speed,
                &props.particle_speed_spread,
                rng,
                ctx_base,
            );
            let speed_per_sec = speed_per_frame * GAME_SPEED;

            let rot_deg = eval_with_spread(&props.emit_rot, &props.emit_rot_spread, rng, ctx_base);
            let rot_rad = rot_deg.to_radians();
            let perp = perpendicular_to(base_dir, next_signed(rng));
            let rotated_dir = Quat::from_axis_angle(perp, rot_rad) * base_dir;
            let velocity = rotated_dir.normalize_or(Vec3::Y) * speed_per_sec;

            // Evaluate `pos` with a fresh rand per axis so `pos=-30 r60,
            // 1.0, -30 r60` samples the square uniformly instead of
            // landing on the same corner every particle.
            let pos_x = props.pos.x.eval(&EvalCtx {
                rand01: next_unit(rng),
                ..ctx_base
            });
            let pos_y = props.pos.y.eval(&EvalCtx {
                rand01: next_unit(rng),
                ..ctx_base
            });
            let pos_z = props.pos.z.eval(&EvalCtx {
                rand01: next_unit(rng),
                ..ctx_base
            });
            let particle_pos = origin + Vec3::new(pos_x, pos_y, pos_z);

            let gravity_per_frame = Vec3::from_array(props.gravity.eval(&ctx_base));
            let gravity_per_sec = gravity_per_frame * GAME_SPEED;

            let airdrag = props.airdrag.eval(&ctx_base);
            let airdrag_per_sec = if airdrag <= 0.0 || airdrag >= 1.0 {
                1.0
            } else {
                // v(t) = v0 * drag^(t*fps) → per-sec scale = drag^fps
                airdrag.powf(GAME_SPEED)
            };

            let size_mod = props.size_mod.eval(&ctx_base);
            let size_mod_per_sec = if size_mod > 0.0 && size_mod != 1.0 {
                size_mod.powf(GAME_SPEED)
            } else {
                1.0
            };
            let size_growth_per_sec = props.size_growth.eval(&ctx_base) * GAME_SPEED;

            commands.spawn(CegParticle {
                pos: particle_pos,
                velocity,
                gravity: gravity_per_sec,
                airdrag_per_sec,
                size,
                size_growth_per_sec,
                size_mod_per_sec,
                palette,
                life: life_secs,
                max_life: life_secs,
                directional: props.directional,
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_flame(
    count: u32,
    props: &FlameProperties,
    origin: Vec3,
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    ceg_assets: &mut CegRenderAssets,
) {
    // We render only the `frontTexture` plane — the camera-facing
    // billboard — which is what every upstream shockwave
    // (`frontTexture=shockwave`, sideTexture=none) actually uses.
    let front = props.front_texture.trim();
    let Some(filename) = CegRegistry::resolve_texture(front) else {
        return;
    };
    let Some((tex, _w, _h)) = load_beam_texture(filename, model_cache, images) else {
        return;
    };

    let ctx = EvalCtx::default();
    let base_size = props.size.eval(&ctx).max(0.1);
    let size_growth_per_frame = props.size_growth.eval(&ctx);
    let ttl = props.ttl.eval(&ctx).max(1.0);
    let pos_offset = Vec3::from_array(props.pos.eval(&ctx));

    let palette = ceg_assets.palette(&tex, &props.color_map, materials);

    for _ in 0..count {
        commands.spawn(CegFlame {
            pos: origin + pos_offset,
            life_frames: ttl,
            max_life_frames: ttl,
            base_size,
            size_growth_per_frame,
            palette,
        });
    }
}

/// `CExploSpikeProjectile` spawn + `Init`: every CEG expression is
/// rolled per spike; `lengthGrowth = |dir|·(0.5 + r·0.4)` and `dir` is
/// divided by it.
#[allow(clippy::too_many_arguments)]
fn spawn_spikes(
    count: u32,
    props: &spring_tdf::SpikeProperties,
    origin: Vec3,
    rng: &mut u32,
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    ceg_assets: &mut CegRenderAssets,
) {
    if count == 0 {
        return;
    }
    let material = match &ceg_assets.spike_material {
        Some(m) => m.clone(),
        None => {
            let texture = CegRegistry::resolve_texture("laserend")
                .and_then(|f| load_beam_texture(f, model_cache, images))
                .map(|(h, _, _)| h);
            let m = materials.add(StandardMaterial {
                base_color: Color::WHITE,
                base_color_texture: texture,
                unlit: true,
                alpha_mode: AlphaMode::Add,
                cull_mode: None,
                ..default()
            });
            ceg_assets.spike_material = Some(m.clone());
            m
        }
    };
    for index in 0..count {
        let roll = |e: &CegExpr, rng: &mut u32| {
            e.eval(&EvalCtx {
                index,
                damage: 0.0,
                rand01: next_unit(rng),
            })
        };
        let raw_dir = Vec3::new(
            roll(&props.dir.x, rng),
            roll(&props.dir.y, rng),
            roll(&props.dir.z, rng),
        );
        let offset = Vec3::new(
            roll(&props.pos.x, rng),
            roll(&props.pos.y, rng),
            roll(&props.pos.z, rng),
        );
        let color = props.color.as_ref().map_or(Vec3::new(1.0, 0.8, 0.5), |c| {
            Vec3::new(roll(&c.x, rng), roll(&c.y, rng), roll(&c.z, rng))
        });
        let length_growth = raw_dir.length() * (0.5 + next_unit(rng) * 0.4);
        let dir = if length_growth > 0.0 {
            raw_dir / length_growth
        } else {
            raw_dir
        };
        commands.spawn(CegSpike {
            pos: origin + offset,
            dir,
            length: roll(&props.length, rng),
            length_growth,
            width: roll(&props.width, rng),
            alpha: roll(&props.alpha, rng),
            alpha_decay: roll(&props.alpha_decay, rng).max(1e-3),
            color,
            material: material.clone(),
        });
    }
}

/// Grow / fade [`CegSpike`]s one sim frame per tick and push their
/// camera-facing quads (`CExploSpikeProjectile::Update` + `Draw`).
pub(super) fn tick_ceg_spikes(
    time: Res<Time>,
    mut spikes: Query<(Entity, &mut CegSpike)>,
    mut batches: ResMut<FxQuadBatches>,
    camera_q: Query<&GlobalTransform, With<crate::rendering::camera::RtsCamera>>,
    mut commands: Commands,
) {
    if spikes.is_empty() {
        return;
    }
    let frames = time.delta_secs() * GAME_SPEED;
    let cam_pos = camera_q
        .single()
        .map(|gt| gt.translation())
        .unwrap_or(Vec3::Y * 1000.0);
    for (entity, mut spike) in &mut spikes {
        spike.length += spike.length_growth * frames;
        spike.alpha = (spike.alpha - spike.alpha_decay * frames).max(0.0);
        if spike.alpha <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        let dif = (spike.pos - cam_pos).normalize_or(Vec3::NEG_Y);
        let w = dif.cross(spike.dir).normalize_or(Vec3::X) * spike.width;
        let l = spike.dir * spike.length;
        let c = spike.color * spike.alpha;
        batches.push_flat_quad(
            &spike.material,
            [
                spike.pos - l - w,
                spike.pos + l - w,
                spike.pos + l + w,
                spike.pos - l + w,
            ],
            [c.x, c.y, c.z, 1.0],
        );
    }
}

/// Queue `count` recursive spawns of `props.explosion_generator`, each
/// with its own `delay`-derived countdown. Evaluated via the `iN`
/// (per-spawn index) expression tokens so `delay=8 i8 count=240` fires
/// at 8, 16, 24, … frames as upstream intends.
fn spawn_delayed(
    count: u32,
    props: &SpawnerProperties,
    origin: Vec3,
    dir: Vec3,
    commands: &mut Commands,
) {
    if props.explosion_generator.is_empty() {
        return;
    }
    for i in 0..count {
        let ctx = EvalCtx {
            index: i,
            ..Default::default()
        };
        let delay_frames = props.delay.eval(&ctx).max(0.0);
        let delay_secs = delay_frames / GAME_SPEED;
        let pos_offset = Vec3::from_array(props.pos.eval(&ctx));
        commands.spawn(CegDelayedSpawn {
            delay_secs,
            pos: origin + pos_offset,
            dir,
            target_ceg: props.explosion_generator.clone(),
        });
    }
}

// ─── Tick ───────────────────────────────────────────────────────────

/// Physics update for live CEG particles, then one camera-facing quad
/// each into the batch (`CSimpleParticleSystem::Update` + `Draw`).
///
/// The batch mesh is rebuilt per sim tick and drawn as-is until the
/// next one, so a particle's position is not interpolated between
/// ticks the way the old per-particle `Transform` + `SimPose` was —
/// same trade the beams and spikes made.
pub(super) fn tick_ceg_particles(
    time: Res<Time>,
    mut particles: Query<(Entity, &mut CegParticle)>,
    ceg_assets: Res<CegRenderAssets>,
    mut batches: ResMut<FxQuadBatches>,
    camera_q: Query<&GlobalTransform, With<crate::rendering::camera::RtsCamera>>,
    mut commands: Commands,
) {
    if particles.is_empty() {
        return;
    }
    let dt = time.delta_secs();
    // Upstream `CSimpleParticleSystem::Draw` billboards using
    // `camera->GetRight()` and `camera->GetUp()` (or the forward-
    // crossed variant for directional). We take those axes from the
    // camera's GlobalTransform rather than deriving them from world-
    // Y crossed with the camera-to-particle vector — the world-Y
    // trick falls apart when the RTS camera pitches steeply.
    let (cam_pos, cam_right, cam_up, cam_fwd) = camera_q
        .single()
        .map(|gt| {
            let t = gt.compute_transform();
            (
                t.translation,
                t.right().as_vec3(),
                t.up().as_vec3(),
                -t.forward().as_vec3(),
            )
        })
        .unwrap_or_else(|_| (Vec3::Y * 1000.0, Vec3::X, Vec3::Y, Vec3::Z));

    for (entity, mut p) in &mut particles {
        p.life -= dt;
        if p.life <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }

        if p.airdrag_per_sec > 0.0 && p.airdrag_per_sec != 1.0 {
            let drag_step = p.airdrag_per_sec.powf(dt);
            p.velocity *= drag_step;
        }
        let gravity = p.gravity;
        p.velocity += gravity * dt;
        let velocity = p.velocity;
        p.pos += velocity * dt;

        p.size += p.size_growth_per_sec * dt;
        if p.size_mod_per_sec > 0.0 && p.size_mod_per_sec != 1.0 {
            p.size *= p.size_mod_per_sec.powf(dt);
        }

        // The camera-facing (xdir, ydir) pair upstream spans the quad
        // with: corners at `±xdir*size ± ydir*size`.
        let (xdir, ydir) = if p.directional && p.velocity.length_squared() > 1e-3 {
            // `directional=1` (hline tracers): align Y with velocity,
            // X with the camera-facing perpendicular.
            let zdir = (p.pos - cam_pos).normalize_or(-cam_fwd);
            let fwd = p.velocity.normalize();
            let xdir = zdir.cross(fwd).normalize_or(cam_right);
            let ydir = xdir.cross(zdir).normalize_or(fwd);
            (xdir, ydir)
        } else {
            (cam_right, cam_up)
        };

        let frac = 1.0 - (p.life / p.max_life).clamp(0.0, 1.0);
        let palette = ceg_assets.palette_at(p.palette);
        push_billboard(
            &mut batches,
            &palette.material,
            p.pos,
            xdir,
            ydir,
            p.size.max(0.01),
            palette.sample(frac),
        );
    }
}

/// Grow + fade `CBitmapMuzzleFlame` billboards and push their quads.
/// Ticked in sim-frame terms: `size_growth` is authored per frame,
/// `ttl` is authored in frames, so we advance `life_frames` by
/// `dt * GAME_SPEED`. No emissive boost on the colour, so the
/// shockwave's faint peach→transparent gradient looks like a wispy
/// ring rather than a pulsing ember.
pub(super) fn tick_ceg_flames(
    time: Res<Time>,
    mut flames: Query<(Entity, &mut CegFlame)>,
    ceg_assets: Res<CegRenderAssets>,
    mut batches: ResMut<FxQuadBatches>,
    camera_q: Query<&GlobalTransform, With<crate::rendering::camera::RtsCamera>>,
    mut commands: Commands,
) {
    if flames.is_empty() {
        return;
    }
    let dt_frames = time.delta_secs() * GAME_SPEED;
    let cam_pos = camera_q
        .single()
        .map(|gt| gt.translation())
        .unwrap_or(Vec3::Y * 1000.0);

    for (entity, mut f) in &mut flames {
        f.life_frames -= dt_frames;
        if f.life_frames <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }

        // Grow: size = base + growth * frames_elapsed.
        let frames_elapsed = f.max_life_frames - f.life_frames;
        let size = f.base_size + f.size_growth_per_frame * frames_elapsed;

        // Face the camera.
        let to_cam = (cam_pos - f.pos).normalize_or(Vec3::Z);
        let right = Vec3::Y.cross(to_cam).normalize_or(Vec3::X);
        let up = to_cam.cross(right).normalize_or(Vec3::Y);

        let frac = 1.0 - (f.life_frames / f.max_life_frames).clamp(0.0, 1.0);
        let palette = ceg_assets.palette_at(f.palette);
        push_billboard(
            &mut batches,
            &palette.material,
            f.pos,
            right,
            up,
            size.max(0.01),
            palette.sample(frac),
        );
    }
}

/// Count down each pending recursive spawn and fire its target CEG when
/// the timer hits zero. Despawns the timer entity after firing.
#[allow(clippy::too_many_arguments)]
pub(super) fn tick_ceg_delayed_spawns(
    time: Res<Time>,
    mut timers: Query<(Entity, &mut CegDelayedSpawn)>,
    registry: Res<CegRegistry>,
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<S3OModelCache>,
    mut ceg_assets: ResMut<CegRenderAssets>,
    mut rng: Local<u32>,
) {
    if timers.is_empty() {
        return;
    }
    let dt = time.delta_secs();

    for (entity, mut timer) in &mut timers {
        timer.delay_secs -= dt;
        if timer.delay_secs > 0.0 {
            continue;
        }
        commands.entity(entity).despawn();
        spawn_ceg(
            &timer.target_ceg,
            timer.pos,
            timer.dir,
            &registry,
            &mut rng,
            &mut commands,
            &mut materials,
            &mut images,
            &mut model_cache,
            &mut ceg_assets,
        );
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────

/// Evaluate `base + uniform(0, 1) * spread` — upstream's *exact*
/// convention per `CSimpleParticleSystem::Init`:
///
/// ```text
/// p.size = particleSize + guRNG.NextFloat() * particleSizeSpread;
/// p.decayrate = 1.0 / (particleLife + guRNG.NextFloat() * particleLifeSpread);
/// ```
///
/// I had this as `base + signed[-1, 1] * spread * 0.5` — same mean,
/// but a *symmetric* range. For `oldskool`'s squarecloud with
/// `particleSize=14 spread=10` the symmetric form produced sizes in
/// `[9, 19]`; upstream gives `[14, 24]` — so my particles averaged
/// ~14 elmos instead of ~19, visibly smaller. The same regression on
/// `particleLife=12 spread=24` meant particles lived 0–24 frames
/// (mean 12) instead of 12–36 (mean 24) — the impact cloud dissolved
/// twice as fast as authored. Fixed by matching upstream's one-sided
/// range verbatim.
fn eval_with_spread(base: &CegExpr, spread: &CegExpr, rng: &mut u32, mut ctx: EvalCtx) -> f32 {
    // Fast path: most properties are folded literals — skip the RNG
    // draws and op loop entirely (the draws would only add 0).
    if let (Some(b), Some(s)) = (base.literal(), spread.literal()) {
        if s == 0.0 {
            return b;
        }
    }
    ctx.rand01 = next_unit(rng);
    let b = base.eval(&ctx);
    ctx.rand01 = next_unit(rng);
    let s = spread.eval(&ctx);
    b + next_unit(rng) * s
}

fn perpendicular_to(dir: Vec3, rand: f32) -> Vec3 {
    let ref_axis = if dir.y.abs() < 0.9 { Vec3::Y } else { Vec3::X };
    let tangent = dir.cross(ref_axis).normalize_or(Vec3::X);
    let angle = rand * std::f32::consts::PI;
    Quat::from_axis_angle(dir.normalize_or(Vec3::Y), angle) * tangent
}

/// [`rng::next_f32`] for a stream that may arrive unseeded: CEG
/// streams live in zero-initialised `Local<u32>`s / projectile fields,
/// so a zero state (which xorshift would never leave) is seeded on the
/// first draw.
fn next_unit(state: &mut u32) -> f32 {
    if *state == 0 {
        *state = 0xA3C59AC3;
    }
    rng::next_f32(state)
}

/// [`next_unit`] mapped to `[-1, 1)` (`rng::next_signed` with the seed guard).
fn next_signed(state: &mut u32) -> f32 {
    next_unit(state) * 2.0 - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_texture_known_aliases() {
        assert_eq!(
            CegRegistry::resolve_texture("circle"),
            Some("whitecircle.tga")
        );
        assert_eq!(
            CegRegistry::resolve_texture("square"),
            Some("solidwhite.tga")
        );
        assert_eq!(
            CegRegistry::resolve_texture("hline"),
            Some("horizontalline.tga")
        );
        assert_eq!(
            CegRegistry::resolve_texture("bytelaser"),
            Some("bytemegabeam.tga")
        );
        assert_eq!(
            CegRegistry::resolve_texture("pointertrail"),
            Some("pointershottrail.tga")
        );
        assert_eq!(
            CegRegistry::resolve_texture("flowtrail"),
            Some("flowtrail1.tga")
        );
    }

    #[test]
    fn resolve_texture_case_insensitive_and_trim() {
        assert_eq!(
            CegRegistry::resolve_texture("CIRCLE"),
            Some("whitecircle.tga")
        );
        assert_eq!(
            CegRegistry::resolve_texture("  square  "),
            Some("solidwhite.tga")
        );
    }

    #[test]
    fn resolve_texture_none_and_empty_skip() {
        assert_eq!(CegRegistry::resolve_texture("none"), None);
        assert_eq!(CegRegistry::resolve_texture(""), None);
        assert_eq!(CegRegistry::resolve_texture("nonsense"), None);
    }

    /// Live particles and flames become one quad each in the batch of
    /// their texture's material, coloured by the colour map at their
    /// life fraction; a dead particle is despawned without a quad.
    #[test]
    fn particles_and_flames_batch_with_palette_colours() {
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<CegRenderAssets>()
            .init_resource::<FxQuadBatches>();

        let texture = Handle::<Image>::default();
        let red_to_blue = ColorMap::parse("1 0 0 1   0 0 1 0.5");
        let world = app.world_mut();
        let (palette, material) =
            world.resource_scope(|world, mut assets: Mut<CegRenderAssets>| {
                let mut materials = world.resource_mut::<Assets<StandardMaterial>>();
                let palette = assets.palette(&texture, &red_to_blue, &mut materials);
                // Same texture, same map: same palette; same texture,
                // other map: same material, other palette.
                assert_eq!(
                    assets.palette(&texture, &red_to_blue, &mut materials),
                    palette
                );
                let other = assets.palette(&texture, &ColorMap::parse("0 1 0 1"), &mut materials);
                assert_ne!(other, palette);
                assert_eq!(
                    assets.palette_at(other).material,
                    assets.palette_at(palette).material
                );
                assert_eq!(materials.len(), 1, "one material per texture");
                (palette, assets.palette_at(palette).material.clone())
            });
        let particle = |life: f32| CegParticle {
            pos: Vec3::ZERO,
            velocity: Vec3::ZERO,
            gravity: Vec3::ZERO,
            airdrag_per_sec: 1.0,
            size: 4.0,
            size_growth_per_sec: 0.0,
            size_mod_per_sec: 1.0,
            palette,
            life,
            max_life: 2.0,
            directional: false,
        };
        world.spawn(particle(2.0)); // just born
        world.spawn(particle(1.0)); // half way
        world.spawn(particle(0.0)); // dead: despawned, no quad
        world.spawn(CegFlame {
            pos: Vec3::X * 10.0,
            life_frames: 30.0,
            max_life_frames: 30.0,
            base_size: 8.0,
            size_growth_per_frame: 1.0,
            palette,
        });

        world.run_system_once(tick_ceg_particles).unwrap();
        world.run_system_once(tick_ceg_flames).unwrap();

        let batches = world.resource::<FxQuadBatches>();
        assert_eq!(batches.pending_quads(&material), 3);
        let colors = batches.pending_colors(&material);
        assert_eq!(colors.len(), 12);
        assert_eq!(colors[0], [1.0, 0.0, 0.0, 1.0], "born: first stop");
        assert_eq!(
            colors[3],
            [1.0, 0.0, 0.0, 1.0],
            "same colour on every corner"
        );
        assert_eq!(colors[4], [0.5, 0.0, 0.5, 0.75], "half way: midpoint");
        assert_eq!(colors[8], [1.0, 0.0, 0.0, 1.0], "flame just born");
        let mut live = world.query::<&CegParticle>();
        assert_eq!(live.iter(world).count(), 2, "the dead particle is gone");
    }

    #[test]
    fn rng_zero_seed_recovers() {
        let mut s = 0;
        // First call must produce a non-zero state; the returned value
        // then depends on the auto-seeded state (`0xA3C59AC3`).
        let _ = next_unit(&mut s);
        assert_ne!(s, 0);
    }

    #[test]
    fn eval_with_spread_falls_back_when_spread_is_zero() {
        let base = CegExpr::parse("10");
        let spread = CegExpr::parse("0");
        let mut rng = 1234u32;
        let v = eval_with_spread(&base, &spread, &mut rng, EvalCtx::default());
        assert_eq!(v, 10.0);
    }

    #[test]
    fn eval_with_spread_matches_upstream_one_sided_range() {
        // Upstream's `particleSize + rand(0, 1) * spread` puts the
        // result in `[base, base + spread)`. base=100 spread=40 →
        // result in [100, 140). The old symmetric impl gave [80, 120]
        // instead, shaving 20% off the average and 40% off the upper
        // tail — the root of the "explosions are way too small" bug.
        let base = CegExpr::parse("100");
        let spread = CegExpr::parse("40");
        let mut rng = 0xFEED_FACEu32;
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        let mut sum = 0.0;
        const N: usize = 4000;
        for _ in 0..N {
            let v = eval_with_spread(&base, &spread, &mut rng, EvalCtx::default());
            assert!((100.0..=140.0).contains(&v), "out of range: {v}");
            min = min.min(v);
            max = max.max(v);
            sum += v;
        }
        let mean = sum / N as f32;
        // Mean should be ~120 (midpoint of [100, 140)) and min/max
        // must hug the bounds within a handful of buckets.
        assert!((mean - 120.0).abs() < 2.0, "mean = {mean}, expected ~120");
        assert!(min < 102.0, "min = {min}, expected near 100");
        assert!(max > 138.0, "max = {max}, expected near 140");
    }
}
