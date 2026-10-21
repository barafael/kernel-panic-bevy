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
//!   its target CEG on fire, the `[groundflash]` section a
//!   [`CegGroundFlash`] (the engine's `CStandardGroundFlash`).
//! - [`tick_ceg_particles`], [`tick_ceg_flames`], [`tick_ceg_ground_flashes`]
//!   and [`tick_ceg_delayed_spawns`]: per-frame integration + despawn.
//!   Particles, flames and ground flashes are plain data drawn as quads
//!   in the shared [`FxQuadBatches`] mesh of their texture's material,
//!   their colour (and alpha) on the vertices.
//!
//! All ambient "spread" math (`X rN`, `X iN`, `X dN`) is handled by
//! [`CegExpr::eval`] with an [`EvalCtx`] populated per particle — the
//! runtime never touches the raw TDF strings.

use crate::sim::{DetFloat, sq};
use bevy::math::ops;
use std::collections::HashMap;

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use spring_tdf::{
    CegExpr, ColorMap, EffectProperties, EmitVector, EvalCtx, ExplosionDef, ExplosionDefs,
    FlameProperties, ParticleProperties, SpawnerProperties,
};

use super::batch::FxQuadBatches;
use crate::rng;
use crate::sim::{GAME_SPEED, RngStream, SimRng};
use crate::terrain::heightmap::Heightmap;
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
    /// (Texture, premultiplied) → its white material ([`Self::texture_material`]).
    /// Particles and flames of
    /// every colour map draw through the same one — the colour is a
    /// vertex attribute of the batch quad — so a fight's whole particle
    /// load is one draw call per texture in use.
    materials_by_texture: HashMap<(AssetId<Image>, bool), Handle<StandardMaterial>>,
    palettes: Vec<CegPalette>,
    /// The `[groundflash]` materials, minted on first use.
    ground_flash: Option<GroundFlashMaterials>,
}

/// The two ground-flash textures as additive materials: `groundring`
/// and `groundflash`, which KP's `RESOURCES.TDF [groundfx]` points at
/// `kpsfx/hollowsquare.tga` and `kpsfx/solidwhite.tga`. Colour and
/// alpha ride on the vertex colour, so one pair serves every CEG.
#[derive(Clone)]
pub(super) struct GroundFlashMaterials {
    pub ring: Handle<StandardMaterial>,
    pub flash: Handle<StandardMaterial>,
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
    /// The unlit material for `texture`, created on first use.
    ///
    /// `base_color` stays white: upstream multiplies the texture by
    /// `colorMap[life]` with no emissive boost — that colour is the batch
    /// quad's vertex colour here, which `StandardMaterial` multiplies
    /// into `base_color` the same way. An emissive multiplier would
    /// over-saturate every particle after tonemapping.
    ///
    /// Particles and flames blend like `CProjectileDrawer`'s particle
    /// pass, `GL_ONE, GL_ONE_MINUS_SRC_ALPHA` (`premultiplied`): the
    /// colour is added in full and the alpha only darkens what lies
    /// behind, so a `colorMap` alpha of .1 (the MineLauncher's
    /// hexastars) is a bright, nearly additive sprite. Ground flashes
    /// use `GL_SRC_ALPHA, GL_ONE`, Bevy's `Add`.
    fn texture_material(
        &mut self,
        texture: &Handle<Image>,
        premultiplied: bool,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        self.materials_by_texture
            .entry((texture.id(), premultiplied))
            .or_insert_with(|| {
                materials.add(StandardMaterial {
                    base_color: Color::WHITE,
                    base_color_texture: Some(texture.clone()),
                    unlit: true,
                    alpha_mode: if premultiplied {
                        AlphaMode::Premultiplied
                    } else {
                        AlphaMode::Add
                    },
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
        let material = self.texture_material(texture, true, materials);
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

    /// The ring and flash materials, loading their textures on first
    /// use; `None` when the bundle has no such textures (headless).
    fn ground_flash_materials(
        &mut self,
        materials: &mut Assets<StandardMaterial>,
        images: &mut Assets<Image>,
        model_cache: &mut S3OModelCache,
    ) -> Option<GroundFlashMaterials> {
        if let Some(found) = &self.ground_flash {
            return Some(found.clone());
        }
        let mut material = |alias: &str| {
            let file = CegRegistry::resolve_texture(alias)?;
            let (texture, _, _) = load_beam_texture(file, model_cache, images)?;
            Some(self.texture_material(&texture, false, materials))
        };
        let ring = material("squarehollow")?;
        let flash = material("square")?;
        let found = GroundFlashMaterials { ring, flash };
        self.ground_flash = Some(found.clone());
        Some(found)
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
    pub heightmap: Option<Res<'w, Heightmap>>,
    pub _marker: std::marker::PhantomData<&'s ()>,
}

impl CegRegistry {
    /// The embedded `explosions/*.tdf` (`units::content::defs`).
    pub fn load() -> Self {
        let registry = Self::from_defs(crate::units::content::defs::defs().explosions.clone());
        info!(
            "CEG registry: {} explosion generators loaded",
            registry.defs.explosions.len()
        );
        registry
    }

    /// Wrap parsed explosion defs.
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
    ("minelauncher", "hexastar.tga"),
    ("pointertrail", "pointershottrail.tga"),
    ("flowtrail", "flowtrail1.tga"),
    ("firetrail", "firetrail.tga"),
    ("sparkle", "sparkle.tga"),
    ("bubbles", "bubbles.tga"),
    ("lobedincantation", "lobedincantation.tga"),
    // Engine default atlas (`ProjectileDrawer`): the beam body, its end
    // caps (also `explspike` streaks) and the BeamLaser emitter flare.
    ("laserfalloff", "laserfalloff.tga"),
    ("laserend", "laserend.tga"),
    ("flare", "flare.tga"),
];

// ─── Runtime components ─────────────────────────────────────────────

/// One live particle spawned from a `CSimpleParticleSystem`. Plain
/// data: no `Transform` or mesh — [`tick_ceg_particles`] pushes it into
/// the quad batch of its palette's material every sim tick.
#[derive(Component)]
pub(super) struct CegParticle {
    pub pos: Vec3,
    /// Position at the previous sim tick — the batch draw lerps
    /// `prev_pos → pos` by `Time<Fixed>::overstep_fraction()` so
    /// particles render at the render clock, not at 30 Hz steps (the
    /// old per-particle `Transform` + `SimPose` contract, kept without
    /// the per-particle render components).
    pub prev_pos: Vec3,
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

/// One live `CBitmapMuzzleFlame` — upstream
/// `Rendering/Env/Particles/Classes/BitmapMuzzleFlame.cpp`: a
/// `frontTexture` quad square to `dir` (KP's shockwaves point `dir` up,
/// so their ring lies flat on the ground) plus, with a `sideTexture`,
/// two crossed quads along `dir`. Batched like [`CegParticle`].
#[derive(Component)]
pub(super) struct CegFlame {
    pub pos: Vec3,
    /// Unit emit direction (`dir /= dirLen` in `Init`).
    pub dir: Vec3,
    pub size: f32,
    /// `length` × the authored `dir`'s length (`Init`).
    pub length: f32,
    pub size_growth: f32,
    pub front_offset: f32,
    pub life_frames: f32,
    pub max_life_frames: f32,
    /// Palettes (indices into [`CegRenderAssets`]) of the front and side
    /// textures; `None` where the CEG says `none`.
    pub front: Option<usize>,
    pub side: Option<usize>,
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
    /// The spawning CEG's up vector, passed on to `target_ceg`.
    pub up: Vec3,
    /// `Arc<str>` because one spawner fans out into `count` timers
    /// (`system_nx`: 240 per cast) — a refcount bump each, not a fresh
    /// `String` allocation each.
    pub target_ceg: std::sync::Arc<str>,
}

/// One live `CStandardGroundFlash` (`Rendering/GroundFlash.cpp`): two
/// additive quads on the ground at `pos` (ground height + 1), in the
/// plane of the terrain under them (`CGroundFlash::CalcNormal`). The
/// `groundring` square is `circleGrowth` (half-size) at birth and grows
/// that much every frame while its alpha falls from `circleAlpha` to 0
/// over `ttl`; the `groundflash` square stays `flashSize` and its alpha
/// ramps up over the first 9 % of `ttl`, then fades out. The engine
/// draws nothing for `ttl = 0` (`flashAgeSpeed = 0`, `Update` ends it).
#[derive(Component)]
pub(super) struct CegGroundFlash {
    pub pos: Vec3,
    /// The quads' axes in the ground plane: world X and Z tilted onto
    /// the terrain ([`ground_axes`]).
    pub axes: (Vec3, Vec3),
    pub flash_size: f32,
    pub flash_alpha: f32,
    pub circle_growth: f32,
    pub circle_alpha: f32,
    pub ttl: f32,
    pub color: [f32; 3],
    /// Sim frames since spawn.
    pub age_frames: f32,
}

impl CegGroundFlash {
    fn new(gf: &spring_tdf::GroundFlash, pos: Vec3, normal: Vec3) -> Self {
        Self {
            pos,
            axes: ground_axes(normal),
            flash_size: gf.flash_size,
            flash_alpha: gf.flash_alpha,
            circle_growth: gf.circle_growth,
            circle_alpha: gf.circle_alpha,
            ttl: gf.ttl,
            color: gf.color,
            age_frames: 0.0,
        }
    }

    /// `circleSize`: `circleGrowth` at birth, `+= circleGrowth` per frame.
    fn ring_half_size(&self, age: f32) -> f32 {
        self.circle_growth * (age + 1.0)
    }

    /// `circleAlpha -= circleAlpha / ttl` per frame.
    fn ring_alpha(&self, age: f32) -> f32 {
        (self.circle_alpha * (1.0 - age / self.ttl)).det_clamp(0.0, 1.0)
    }

    /// `flashAge += 1 / ttl` per frame: `flashAlpha · age · 10` while
    /// under 0.091, `flashAlpha · (1 − age)` after, nothing from 1.
    fn flash_alpha(&self, age: f32) -> f32 {
        let i_age = age / self.ttl;
        if i_age >= 1.0 {
            return 0.0;
        }
        let alpha = if i_age < 0.091 {
            self.flash_alpha * i_age * 10.0
        } else {
            self.flash_alpha * (1.0 - i_age)
        };
        alpha.det_clamp(0.0, 1.0)
    }
}

/// A square of half-size `s` around `pos` spanned by `axes`, corners
/// in `push_quad`'s `[bl, br, tr, tl]` order.
fn ground_square(pos: Vec3, (x, z): (Vec3, Vec3), s: f32) -> [Vec3; 4] {
    [
        pos + (-x - z) * s,
        pos + (x - z) * s,
        pos + (x + z) * s,
        pos + (-x + z) * s,
    ]
}

/// `CGroundFlash::CalcNormal`: the plane through the ground heights
/// `quad_size` either side of `pos` along X and along Z, so a flash
/// lies on a ramp rather than cutting into it.
pub(super) fn ground_flash_normal(ground: &Heightmap, pos: Vec3, quad_size: f32) -> Vec3 {
    let s = quad_size.det_max(1.0);
    let at = |dx: f32, dz: f32| ground.place(pos.x + dx, pos.z + dz);
    let (p0, p1, p2, p3) = (at(s, 0.0), at(-s, 0.0), at(0.0, s), at(0.0, -s));
    // The engine's cross products, operands swapped: its come out
    // pointing down, which it doesn't mind as it draws both faces.
    let n0 = (p3 - p0).cross(p2 - p0).normalize_or(Vec3::Y);
    let n1 = (p2 - p1).cross(p3 - p1).normalize_or(Vec3::Y);
    (n0 + n1).normalize_or(Vec3::Y)
}

/// World X and Z turned onto the plane with `normal` (the engine's
/// `side1 = normal × X`, `side2 = side1 × normal`, as `(side2, −side1)`
/// so flat ground gives X and Z back).
fn ground_axes(normal: Vec3) -> (Vec3, Vec3) {
    let side1 = normal.cross(Vec3::X).normalize_or(Vec3::NEG_Z);
    let side2 = side1.cross(normal).normalize_or(Vec3::X);
    (side2, -side1)
}

// ─── Spawning ───────────────────────────────────────────────────────

/// Spawn a CEG at `pos` with firing direction `dir`. Returns true iff
/// the CEG name was found; a missing entry lets callers fall back to a
/// synthesised burst. `up` is the emitter's up vector: the CEG's
/// literal vectors and offsets (`emitVector=0, 1, 0`, `pos=0, 1, 0`)
/// are authored for an upright emitter and turn with it, so a unit
/// dying on a ramp bursts square to the ramp. `ground` puts the
/// `[groundflash]` on the terrain (`CGround::GetHeightReal`), in its
/// plane; without it the flash lies flat at `pos`.
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_ceg(
    ceg_name: &str,
    pos: Vec3,
    dir: Vec3,
    up: Vec3,
    registry: &CegRegistry,
    rng: &mut u32,
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    ceg_assets: &mut CegRenderAssets,
    ground: Option<&Heightmap>,
) -> bool {
    let Some(def) = registry.get(ceg_name) else {
        return false;
    };

    let dir = dir.normalize_or(Vec3::Y);
    let up = up.normalize_or(Vec3::Y);
    let frame = Quat::from_rotation_arc(Vec3::Y, up);

    for effect in &def.effects {
        match &effect.properties {
            EffectProperties::Particle(p) => spawn_particle_system(
                effect.count,
                p,
                pos,
                dir,
                frame,
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
                dir,
                frame,
                rng,
                commands,
                materials,
                images,
                model_cache,
                ceg_assets,
            ),
            EffectProperties::Spawner(s) => {
                spawn_delayed(effect.count, s, pos, dir, up, commands);
            }
            EffectProperties::Spike(sp) => spawn_spikes(
                effect.count,
                sp,
                pos,
                frame,
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
    // The `[groundflash]` section (`oldskool_vbig`, `oldskool_big`,
    // `mine`, `network_dissolve` ship visible ones; the rest `ttl=0`).
    if let Some(gf) = &def.ground_flash
        && gf.ttl > 0.0
        && ceg_assets
            .ground_flash_materials(materials, images, model_cache)
            .is_some()
    {
        let (ground_y, normal) = ground.map_or((pos.y, Vec3::Y), |h| {
            (
                h.sample(pos.x, pos.z),
                ground_flash_normal(h, pos, gf.flash_size),
            )
        });
        commands.spawn(CegGroundFlash::new(
            gf,
            Vec3::new(pos.x, ground_y + 1.0, pos.z),
            normal,
        ));
    }
    true
}

/// An acceleration authored per sim frame² (CEG `gravity`, added to
/// the per-frame speed every frame) in elmos/s².
fn per_frame_accel_to_per_sec(per_frame: Vec3) -> Vec3 {
    per_frame * GAME_SPEED * GAME_SPEED
}

#[allow(clippy::too_many_arguments)]
fn spawn_particle_system(
    count: u32,
    props: &ParticleProperties,
    origin: Vec3,
    dir: Vec3,
    frame: Quat,
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
            frame * Vec3::from_array(v.eval(&EvalCtx::default())).normalize_or(Vec3::Y)
        }
    };

    // One batched spawn instead of one command per particle: a SIGTERM
    // death fans out ~320 of these, and the trail CEGs of every
    // fuel-carrying cegTag projectile re-emit a burst per sim frame
    // (`tick_projectiles`), so this is the app's highest spawn-rate
    // loop. `spawn_batch` preserves order, so the RNG streams above and
    // the query iteration order the sim hash sees are unchanged.
    let mut particles = Vec::new();

    for fire_index in 0..count {
        let n_particles = props
            .num_particles
            .eval(&EvalCtx {
                index: fire_index,
                ..Default::default()
            })
            .round()
            .det_max(1.0) as u32;

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
            .det_max(1.0);
            let life_secs = life_frames / GAME_SPEED;

            let size = eval_with_spread(
                &props.particle_size,
                &props.particle_size_spread,
                rng,
                ctx_base,
            )
            .det_max(0.1);

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
            let particle_pos = origin + frame * Vec3::new(pos_x, pos_y, pos_z);

            // `CSimpleParticleSystem::Update` adds `gravity` to the
            // per-frame speed every frame: elmos/frame², so ×30² for
            // elmos/s² (×30 alone left every CEG 30× too floaty).
            let gravity_per_sec =
                per_frame_accel_to_per_sec(Vec3::from_array(props.gravity.eval(&ctx_base)));

            let airdrag = props.airdrag.eval(&ctx_base);
            let airdrag_per_sec = if airdrag <= 0.0 || airdrag >= 1.0 {
                1.0
            } else {
                // v(t) = v0 * drag^(t*fps) → per-sec scale = drag^fps
                ops::powf(airdrag, GAME_SPEED)
            };

            let size_mod = props.size_mod.eval(&ctx_base);
            let size_mod_per_sec = if size_mod > 0.0 && size_mod != 1.0 {
                ops::powf(size_mod, GAME_SPEED)
            } else {
                1.0
            };
            let size_growth_per_sec = props.size_growth.eval(&ctx_base) * GAME_SPEED;

            particles.push(CegParticle {
                pos: particle_pos,
                prev_pos: particle_pos,
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
    commands.spawn_batch(particles);
}

#[allow(clippy::too_many_arguments)]
fn spawn_flame(
    count: u32,
    props: &FlameProperties,
    origin: Vec3,
    spawn_dir: Vec3,
    frame: Quat,
    rng: &mut u32,
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    ceg_assets: &mut CegRenderAssets,
) {
    let mut palette_for = |tex_name: &str| {
        let filename = CegRegistry::resolve_texture(tex_name)?;
        let (tex, _w, _h) = load_beam_texture(filename, model_cache, images)?;
        Some(ceg_assets.palette(&tex, &props.color_map, materials))
    };
    let front = palette_for(&props.front_texture);
    let side = palette_for(&props.side_texture);
    if front.is_none() && side.is_none() {
        return;
    }

    // Batched like [`spawn_particle_system`]: same per-item RNG rolls,
    // one command for the whole burst.
    let mut flames = Vec::with_capacity(count as usize);
    for _ in 0..count {
        // Every CEG expression is rolled per projectile, each `r` term
        // with its own random number.
        let mut roll = |e: &CegExpr| {
            e.eval(&EvalCtx {
                rand01: next_unit(rng),
                ..Default::default()
            })
        };
        let pos = Vec3::new(roll(&props.pos.x), roll(&props.pos.y), roll(&props.pos.z));
        let authored = Vec3::new(roll(&props.dir.x), roll(&props.dir.y), roll(&props.dir.z));
        // `dir = dir` (the explosion's own direction) parses to no
        // literal: fall back to the spawn direction.
        let dir = if authored.length_squared() > 1e-12 {
            frame * authored
        } else {
            spawn_dir
        };
        let ttl = roll(&props.ttl).det_max(1.0);
        flames.push(CegFlame {
            pos: origin + frame * pos,
            dir: dir.normalize_or(Vec3::Y),
            size: roll(&props.size),
            length: roll(&props.length) * dir.length(),
            size_growth: roll(&props.size_growth),
            front_offset: roll(&props.front_offset),
            life_frames: ttl,
            max_life_frames: ttl,
            front,
            side,
        });
    }
    commands.spawn_batch(flames);
}

/// `CExploSpikeProjectile` spawn + `Init`: every CEG expression is
/// rolled per spike; `lengthGrowth = |dir|·(0.5 + r·0.4)` and `dir` is
/// divided by it.
#[allow(clippy::too_many_arguments)]
fn spawn_spikes(
    count: u32,
    props: &spring_tdf::SpikeProperties,
    origin: Vec3,
    frame: Quat,
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
    // Batched like [`spawn_particle_system`].
    let mut spikes = Vec::with_capacity(count as usize);
    for index in 0..count {
        let roll = |e: &CegExpr, rng: &mut u32| {
            e.eval(&EvalCtx {
                index,
                damage: 0.0,
                rand01: next_unit(rng),
            })
        };
        let raw_dir = frame
            * Vec3::new(
                roll(&props.dir.x, rng),
                roll(&props.dir.y, rng),
                roll(&props.dir.z, rng),
            );
        let offset = frame
            * Vec3::new(
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
        spikes.push(CegSpike {
            pos: origin + offset,
            dir,
            length: roll(&props.length, rng),
            length_growth,
            width: roll(&props.width, rng),
            alpha: roll(&props.alpha, rng),
            alpha_decay: roll(&props.alpha_decay, rng).det_max(1e-3),
            color,
            material: material.clone(),
        });
    }
    commands.spawn_batch(spikes);
}

/// Grow / fade [`CegSpike`]s one sim frame per tick and push their
/// camera-facing quads (`CExploSpikeProjectile::Update` + `Draw`).
pub(super) fn tick_ceg_spikes(
    time: Res<Time>,
    mut spikes: Query<&mut CegSpike>,
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
    for mut spike in &mut spikes {
        spike.length += spike.length_growth * frames;
        spike.alpha = (spike.alpha - spike.alpha_decay * frames).det_max(0.0);
        if spike.alpha <= 0.0 {
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
    commands.despawn_all_where::<&CegSpike, ()>(|s| s.alpha <= 0.0);
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
    up: Vec3,
    commands: &mut Commands,
) {
    if props.explosion_generator.is_empty() {
        return;
    }
    // One allocation per spawner effect; the `count` timers share it.
    let target_ceg: std::sync::Arc<str> = props.explosion_generator.as_str().into();
    // `system_nx` fans out 240 timers per cast — one command, not 240.
    let mut timers = Vec::with_capacity(count as usize);
    for i in 0..count {
        let ctx = EvalCtx {
            index: i,
            ..Default::default()
        };
        let delay_frames = props.delay.eval(&ctx).det_max(0.0);
        let delay_secs = delay_frames / GAME_SPEED;
        let pos_offset = Vec3::from_array(props.pos.eval(&ctx));
        timers.push(CegDelayedSpawn {
            delay_secs,
            pos: origin + Quat::from_rotation_arc(Vec3::Y, up) * pos_offset,
            dir,
            up,
            target_ceg: std::sync::Arc::clone(&target_ceg),
        });
    }
    commands.spawn_batch(timers);
}

// ─── Tick ───────────────────────────────────────────────────────────

/// Physics update for live CEG particles, then one camera-facing quad
/// each into the batch (`CSimpleParticleSystem::Update` + `Draw`).
///
/// The batch mesh is rebuilt per sim tick, but each particle carries
/// its previous position and the draw lerps by
/// `Time<Fixed>::overstep_fraction()` — the same alpha
/// `interpolate_sim_pose` uses for unit models, so particles glide at
/// the render rate instead of juddering at 30 Hz.
///
/// Particles are presentation only (nothing in the sim reads them), so
/// the two per-particle `powf`s per tick use the platform's fast `powf`
/// instead of libm's (~5× slower on glibc); last-bit differences between
/// clients are invisible here (workspace `clippy.toml`).
#[allow(clippy::disallowed_methods)]
pub(super) fn tick_ceg_particles(
    time: Res<Time>,
    fixed: Res<Time<Fixed>>,
    mut particles: Query<&mut CegParticle>,
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

    // Same render-clock alpha `interpolate_sim_pose` blends models by.
    let alpha = fixed.overstep_fraction().clamp(0.0, 1.0);

    // Dead particles are collected by one bulk-despawn command after
    // the loop (0.20's batched despawn path) instead of one entity
    // command each; the loop still skips drawing them this tick.
    for mut p in &mut particles {
        p.life -= dt;
        if p.life <= 0.0 {
            continue;
        }

        p.prev_pos = p.pos;
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
            p.prev_pos.lerp(p.pos, alpha),
            xdir,
            ydir,
            p.size.max(0.01),
            palette.sample(frac),
        );
    }
    commands.despawn_all_where::<&CegParticle, ()>(|p| p.life <= 0.0);
}

/// Corner UVs of a muzzle-flame quad in its corner order:
/// `(xstart, ystart), (xend, ystart), (xend, yend), (xstart, yend)`.
const FLAME_UVS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

/// `CBitmapMuzzleFlame::Draw`'s quads at life fraction `life`:
/// `isize = size·(1 + sizeGrowth·(1 − (1 − life)²))` (growth eases out
/// and scales the authored size — it is not elmos per frame), the
/// same factor on `length`. With the default `fixedSideDir`, the
/// planes are oriented by world up (forward when `dir` is vertical),
/// not by the camera. Returns the front quad (square to `dir`,
/// `frontOffset·ilength` along it) and the two side quads (along
/// `dir`).
fn muzzle_flame_quads(f: &CegFlame, life: f32) -> ([Vec3; 4], [[Vec3; 4]; 2]) {
    let igrowth = f.size_growth * (1.0 - sq(1.0 - life));
    let isize = f.size * (igrowth + 1.0);
    let ilength = f.length * (igrowth + 1.0);
    let zdir = if f.dir.dot(Vec3::Y).abs() >= 0.99 {
        Vec3::Z
    } else {
        Vec3::Y
    };
    let xdir = f.dir.cross(zdir).normalize_or(Vec3::X);
    let ydir = f.dir.cross(xdir).normalize_or(Vec3::Z);
    let (x, y, along) = (xdir * isize, ydir * isize, f.dir * ilength);
    let p = f.pos;
    let fpos = p + f.dir * f.front_offset * ilength;
    (
        [fpos - x + y, fpos + x + y, fpos + x - y, fpos - x - y],
        [
            [p + x, p + x + along, p - x + along, p - x],
            [p + y, p + y + along, p - y + along, p - y],
        ],
    )
}

/// Grow + fade `CBitmapMuzzleFlame`s and push their quads. Ticked in
/// sim-frame terms: `ttl` is authored in frames, so `life_frames`
/// advances by `dt * GAME_SPEED`.
pub(super) fn tick_ceg_flames(
    time: Res<Time>,
    mut flames: Query<&mut CegFlame>,
    ceg_assets: Res<CegRenderAssets>,
    mut batches: ResMut<FxQuadBatches>,
    mut commands: Commands,
) {
    if flames.is_empty() {
        return;
    }
    let dt_frames = time.delta_secs() * GAME_SPEED;

    for mut f in &mut flames {
        f.life_frames -= dt_frames;
        if f.life_frames <= 0.0 {
            continue;
        }
        let life = 1.0 - (f.life_frames / f.max_life_frames).det_clamp(0.0, 1.0);
        let (front, sides) = muzzle_flame_quads(&f, life);
        if let Some(palette) = f.front {
            let palette = ceg_assets.palette_at(palette);
            let color = palette.sample(life);
            batches.push_quad(&palette.material, front, FLAME_UVS, [color; 4]);
        }
        if let Some(palette) = f.side {
            let palette = ceg_assets.palette_at(palette);
            let color = palette.sample(life);
            for side in sides {
                batches.push_quad(&palette.material, side, FLAME_UVS, [color; 4]);
            }
        }
    }
    commands.despawn_all_where::<&CegFlame, ()>(|f| f.life_frames <= 0.0);
}

/// Age every [`CegGroundFlash`] and push its ring and flash quads for
/// this frame; gone once `ttl` frames old.
pub(super) fn tick_ceg_ground_flashes(
    time: Res<Time>,
    mut flashes: Query<(Entity, &mut CegGroundFlash)>,
    ceg_assets: Res<CegRenderAssets>,
    mut batches: ResMut<FxQuadBatches>,
    mut commands: Commands,
) {
    if flashes.is_empty() {
        return;
    }
    let dt_frames = time.delta_secs() * GAME_SPEED;
    for (entity, mut f) in &mut flashes {
        let age = f.age_frames;
        f.age_frames += dt_frames;
        if age >= f.ttl {
            commands.entity(entity).despawn();
            continue;
        }
        let Some(mats) = &ceg_assets.ground_flash else {
            continue;
        };
        let [r, g, b] = f.color;
        let ring_alpha = f.ring_alpha(age);
        if ring_alpha > 0.0 {
            let corners = ground_square(f.pos, f.axes, f.ring_half_size(age));
            batches.push_flat_quad(&mats.ring, corners, [r, g, b, ring_alpha]);
        }
        let flash_alpha = f.flash_alpha(age);
        if flash_alpha > 0.0 {
            let corners = ground_square(f.pos, f.axes, f.flash_size);
            batches.push_flat_quad(&mats.flash, corners, [r, g, b, flash_alpha]);
        }
    }
}

/// [`tick_ceg_delayed_spawns`]'s random stream (visual, match-scoped
/// like [`super::spawn::WeaponVisualRng`]).
pub struct DelayedCegRng;
impl RngStream for DelayedCegRng {
    const SALT: u64 = 0xA3C5_9AC3;
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
    mut rng: ResMut<SimRng<DelayedCegRng>>,
    heightmap: Option<Res<Heightmap>>,
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
            timer.up,
            &registry,
            &mut rng,
            &mut commands,
            &mut materials,
            &mut images,
            &mut model_cache,
            &mut ceg_assets,
            heightmap.as_deref(),
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
    if let (Some(b), Some(s)) = (base.literal(), spread.literal())
        && s == 0.0
    {
        return b;
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
/// streams live in zero-initialised projectile fields,
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
            .init_resource::<Time<Fixed>>()
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
            prev_pos: Vec3::ZERO,
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
            dir: Vec3::Y,
            size: 8.0,
            length: 1.0,
            size_growth: 1.0,
            front_offset: 0.0,
            life_frames: 30.0,
            max_life_frames: 30.0,
            front: Some(palette),
            side: None,
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

    /// `oldskool_vbig`'s `[groundflash]` (flashSize 96, circleGrowth
    /// 6.4, ttl 64, both alphas 1) as `CGroundFlash.cpp` runs it.
    fn vbig_flash() -> CegGroundFlash {
        CegGroundFlash::new(
            &spring_tdf::GroundFlash {
                flash_size: 96.0,
                flash_alpha: 1.0,
                circle_growth: 6.4,
                circle_alpha: 1.0,
                ttl: 64.0,
                color: [1.0; 3],
            },
            Vec3::new(100.0, 5.0, 200.0),
            Vec3::Y,
        )
    }

    #[test]
    fn ground_ring_grows_one_step_per_frame_and_fades_over_ttl() {
        let f = vbig_flash();
        assert!((f.ring_half_size(0.0) - 6.4).abs() < 1e-3);
        assert!((f.ring_half_size(63.0) - 409.6).abs() < 1e-3);
        assert_eq!(f.ring_alpha(0.0), 1.0);
        assert!((f.ring_alpha(63.0) - 1.0 / 64.0).abs() < 1e-5);
        assert_eq!(f.ring_alpha(64.0), 0.0);
    }

    #[test]
    fn ground_flash_alpha_ramps_up_then_fades() {
        let f = vbig_flash();
        assert_eq!(f.flash_alpha(0.0), 0.0);
        // Up to 0.091 of the life (frame 5.8), then down.
        let peak = f.flash_alpha(0.091 * 64.0);
        assert!((peak - 0.909).abs() < 1e-3, "{peak}");
        assert!(f.flash_alpha(5.0) < peak && f.flash_alpha(7.0) < peak);
        assert!(f.flash_alpha(32.0) > f.flash_alpha(48.0));
        assert_eq!(f.flash_alpha(64.0), 0.0);
        // `flashAlpha=0` (the impact CEGs): no flash quad at all.
        let mut dark = vbig_flash();
        dark.flash_alpha = 0.0;
        assert_eq!(dark.flash_alpha(6.0), 0.0);
    }

    /// A newborn flash batches its ring (full alpha) but no flash quad
    /// yet (alpha ramps from 0); a flash at `ttl` is gone.
    #[test]
    fn ground_flash_batches_two_quads_then_dies() {
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<CegRenderAssets>()
            .init_resource::<FxQuadBatches>();
        let world = app.world_mut();
        let (ring, flash) = {
            let mut materials = world.resource_mut::<Assets<StandardMaterial>>();
            (
                materials.add(StandardMaterial::default()),
                materials.add(StandardMaterial::default()),
            )
        };
        world.resource_mut::<CegRenderAssets>().ground_flash = Some(GroundFlashMaterials {
            ring: ring.clone(),
            flash: flash.clone(),
        });
        let born = world.spawn(vbig_flash()).id();
        let mut mid = vbig_flash();
        mid.age_frames = 6.0;
        world.spawn(mid);
        let mut dead = vbig_flash();
        dead.age_frames = 64.0;
        let dead = world.spawn(dead).id();

        world.run_system_once(tick_ceg_ground_flashes).unwrap();

        let batches = world.resource::<FxQuadBatches>();
        assert_eq!(batches.pending_quads(&ring), 2, "born and mid rings");
        assert_eq!(batches.pending_quads(&flash), 1, "only mid has a flash yet");
        let ring_colors = batches.pending_colors(&ring);
        assert_eq!(
            ring_colors[0],
            [1.0, 1.0, 1.0, 1.0],
            "born ring at circleAlpha"
        );
        let flash_colors = batches.pending_colors(&flash);
        assert!((flash_colors[0][3] - vbig_flash().flash_alpha(6.0)).abs() < 1e-6);
        assert!(world.get::<CegGroundFlash>(born).is_some());
        assert!(world.get::<CegGroundFlash>(dead).is_none(), "ttl reached");
    }

    #[test]
    fn ground_square_lies_flat_around_pos() {
        let [bl, br, tr, tl] = ground_square(Vec3::new(10.0, 1.0, 20.0), ground_axes(Vec3::Y), 6.4);
        for c in [bl, br, tr, tl] {
            assert_eq!(c.y, 1.0);
        }
        assert_eq!(bl, Vec3::new(3.6, 1.0, 13.6));
        assert_eq!(tr, Vec3::new(16.4, 1.0, 26.4));
        assert!((br.x - tl.x - 12.8).abs() < 1e-5);
    }

    /// On a ramp rising along X the flash's corners sit on the ramp:
    /// each corner's height off the ground is the centre's, and the
    /// square keeps its size.
    #[test]
    fn ground_flash_lies_on_a_ramp() {
        let (w, h) = (65, 65);
        let heights = (0..w * h).map(|i| (i % w) as f32 * 4.0).collect();
        let ground = Heightmap::from_raw(heights, w, h);
        let pos = ground.place(256.0, 256.0);
        let normal = ground_flash_normal(&ground, pos, 48.0);
        assert!(normal.x < -0.4 && normal.z.abs() < 1e-4, "{normal}");
        let s = 40.0;
        for c in ground_square(pos, ground_axes(normal), s) {
            assert!((c.y - ground.sample(c.x, c.z)).abs() < 0.5, "{c}");
        }
        let [bl, br, _, tl] = ground_square(pos, ground_axes(normal), s);
        assert!(((br - bl).length() - 2.0 * s).abs() < 1e-3);
        assert!(((tl - bl).length() - 2.0 * s).abs() < 1e-3);
    }

    /// `system_nx`'s shockwave (`size=1 sizeGrowth=120 ttl=6`,
    /// `dir=0,1,0`): upstream it ends 121 elmos across a flat ring, not
    /// `1 + 120·6` = 721, and it lies on the ground instead of facing
    /// the camera.
    #[test]
    fn muzzle_flame_grows_by_factor_in_the_plane_square_to_dir() {
        let flame = CegFlame {
            pos: Vec3::ZERO,
            dir: Vec3::Y,
            size: 1.0,
            length: 1.0,
            size_growth: 120.0,
            front_offset: 0.0,
            life_frames: 6.0,
            max_life_frames: 6.0,
            front: Some(0),
            side: None,
        };
        let (front, _) = muzzle_flame_quads(&flame, 1.0);
        for corner in front {
            assert!(corner.y.abs() < 1e-4, "ring lies flat: {corner}");
        }
        let half_width = (front[1] - front[0]).length() / 2.0;
        assert!((half_width - 121.0).abs() < 1e-3, "{half_width}");
        // Half way through its life the growth has eased to 3/4.
        let (front, _) = muzzle_flame_quads(&flame, 0.5);
        let half_width = (front[1] - front[0]).length() / 2.0;
        assert!((half_width - 91.0).abs() < 1e-3, "{half_width}");
    }

    /// `corruption_const` (`frontOffset=1 length=4`, `dir` down): the
    /// front quad sits `length` along `dir`; side quads run along it.
    #[test]
    fn muzzle_flame_front_offset_and_sides_follow_dir() {
        let flame = CegFlame {
            pos: Vec3::new(0.0, 50.0, 0.0),
            dir: Vec3::NEG_Y,
            size: 1.0,
            length: 4.0,
            size_growth: 0.0,
            front_offset: 1.0,
            life_frames: 12.0,
            max_life_frames: 12.0,
            front: Some(0),
            side: Some(0),
        };
        let (front, sides) = muzzle_flame_quads(&flame, 0.0);
        let centre = front.iter().copied().sum::<Vec3>() / 4.0;
        assert!(
            centre.abs_diff_eq(Vec3::new(0.0, 46.0, 0.0), 1e-4),
            "{centre}"
        );
        for side in sides {
            assert!((side[1] - side[0]).abs_diff_eq(Vec3::NEG_Y * 4.0, 1e-4));
        }
    }

    /// `gravity` is elmos/frame² (`speed += gravity` every frame): one
    /// second of `gravity = 0, -0.1, 0` adds 30 × 0.1 = 3 elmos/frame of
    /// fall speed — 90 elmos/s, so the acceleration is 90 elmos/s².
    #[test]
    fn particle_gravity_is_per_frame_squared() {
        let g = per_frame_accel_to_per_sec(Vec3::new(0.0, -0.1, 0.0));
        assert!((g.y + 90.0).abs() < 1e-3, "{g}");
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
            min = min.det_min(v);
            max = max.det_max(v);
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
