//! Types shared across the `weapon_fx` sub-modules: the event buffer,
//! visual marker components, the cached beam-material registry, and the
//! TDF-colour normaliser.

use std::borrow::Cow;

use bevy::prelude::*;

/// Describes a single attack for the visual system.
///
/// `weapon_name` is `Cow<'static, str>` so the hot build-laser path
/// (production.rs pushes `"BuildLaser"` per emitter per factory per
/// frame — 4×/kernel in steady state) uses a static borrow instead of
/// allocating a fresh `String` for each ray; combat's per-shot path
/// still allocates once per shot via `Cow::Owned`.
pub struct AttackEvent {
    pub attacker_pos: Vec3,
    pub target_pos: Vec3,
    pub weapon_name: Cow<'static, str>,
}

/// Buffer written by the combat system, drained by visual systems.
#[derive(Resource, Default)]
pub struct PendingAttacks {
    pub events: Vec<AttackEvent>,
}

/// A standalone explosion — no beam, no flying projectile, just a pop at
/// a point. Used for unit-death `ExplodeAs` blasts, kamikaze detonations,
/// and any future self-damage visual that shouldn't fake a shooter.
///
/// `radius` is the weapon's `area_of_effect`; the spawn side scales
/// both the fireball sphere and the ground flash from it so a Bit pop
/// looks smaller than a Terminal SIGTERM crater.
pub struct ExplosionEvent {
    pub pos: Vec3,
    pub rgb: [f32; 3],
    pub radius: f32,
}

/// Event buffer drained by [`spawn::spawn_pending_explosions`]. Separate
/// from [`PendingAttacks`] so systems that model a pure detonation don't
/// have to fake a zero-length beam.
#[derive(Resource, Default)]
pub struct PendingExplosions {
    pub events: Vec<ExplosionEvent>,
}

/// A beam visual that fades over its lifetime.
///
/// Mesh is the shared unit cuboid/sphere from [`WeaponFxMeshes`]; the real
/// dimensions live in `base_thickness` (X/Y) and `length` (Z) and are reapplied
/// to `Transform::scale` by the tick system with an animated fade factor.
#[derive(Component)]
pub(super) struct BeamVisual {
    pub lifetime: f32,
    pub max_lifetime: f32,
    pub base_thickness: f32,
    pub length: f32,
}

/// A projectile traveling from origin to target.
///
/// `trail_rgb` and `trail_interval` drive the in-flight smoke/dust trail
/// that upstream weapons configure via `smoketrail=1` or the `cegTag` CEG
/// reference. Interval is seconds between puffs; `None` skips trailing
/// entirely (e.g. the Bit's plain laser dot). `trail_accumulator` is
/// advanced every tick and consumed on each puff so frame-rate changes
/// don't change trail density.
#[derive(Component)]
pub(super) struct ProjectileVisual {
    pub origin: Vec3,
    pub target: Vec3,
    pub speed: f32,
    pub progress: f32,
    pub arc_height: f32,
    pub trail_rgb: Option<[f32; 3]>,
    pub trail_interval: f32,
    pub trail_accumulator: f32,
}

/// A burst of multiple small beam segments (spray weapons like PacketBeam).
#[derive(Component)]
pub(super) struct BurstSegment {
    pub lifetime: f32,
}

/// One pixelly square spawned at a build-laser impact point.
///
/// Mirrors upstream `oldskool_build` CEG: a hollow-square sprite with a
/// short upward drift, killed quickly by airdrag, fading from opaque white
/// to transparent over its lifetime. Per-pulse spawn count is 1, but the
/// production system pulses every frame so ~16 overlap at any moment,
/// producing the iconic TA "nanoframe pixels" cluster.
#[derive(Component)]
pub(super) struct BuildSparkle {
    pub lifetime: f32,
    pub max_lifetime: f32,
    pub velocity: Vec3,
    /// World-space size at full opacity; the visible scale shrinks below this
    /// during the second half of the particle's life to fake the colormap fade.
    pub base_size: f32,
}

/// Lazily-loaded texture + mesh for `BuildSparkle` particles. Created on first
/// use so we don't pay the asset load cost on maps that never produce anything.
#[derive(Resource, Default)]
pub(super) struct BuildSparkleAssets {
    pub mesh: Option<Handle<Mesh>>,
    pub material: Option<Handle<StandardMaterial>>,
}

/// Short-lived burst spawned at every weapon impact point, colored by
/// the weapon's `rgb_color`. The sphere scales up and fades over
/// `max_lifetime`; `decay_impact_bursts` despawns when the timer runs
/// out. A substitute for the full upstream CEG particle system.
#[derive(Component)]
pub(super) struct ImpactBurst {
    pub lifetime: f32,
    pub max_lifetime: f32,
    pub base_size: f32,
}

/// Shared sphere mesh reused across every ImpactBurst so we don't add
/// a new mesh asset per hit.
#[derive(Resource, Default)]
pub(super) struct ImpactBurstAssets {
    pub mesh: Option<Handle<Mesh>>,
}

/// Flat horizontal emissive disc spawned at each ground-level impact —
/// a visual stand-in for the upstream `GroundFlash` CEG subsection that
/// most KP explosions mount. Separated from [`ImpactBurst`] (a 3D
/// fireball) so the two can fade on different curves: the burst rises
/// and fades, the ring expands and stays bright until the end.
#[derive(Component)]
pub(super) struct GroundFlash {
    pub lifetime: f32,
    pub max_lifetime: f32,
    pub base_radius: f32,
}

/// Shared flat-disc mesh for every [`GroundFlash`]. The mesh is a unit
/// circle; the spawn system scales it to the weapon's radius via
/// `Transform::scale`.
#[derive(Resource, Default)]
pub(super) struct GroundFlashAssets {
    pub mesh: Option<Handle<Mesh>>,
}

/// Unit-length primitives shared across every beam, burst, projectile, and
/// melee visual. Baking thickness/length into `Transform::scale` instead of
/// the mesh keeps `Assets<Mesh>` at a handful of handles regardless of how
/// many shots fly per second.
#[derive(Resource, Default)]
pub(super) struct WeaponFxMeshes {
    pub unit_cube: Option<Handle<Mesh>>,
    pub unit_sphere: Option<Handle<Mesh>>,
}

impl WeaponFxMeshes {
    pub(super) fn unit_cube(&mut self, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
        self.unit_cube
            .get_or_insert_with(|| meshes.add(Cuboid::new(1.0, 1.0, 1.0)))
            .clone()
    }

    pub(super) fn unit_sphere(&mut self, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
        self.unit_sphere
            .get_or_insert_with(|| meshes.add(Sphere::new(1.0)))
            .clone()
    }
}

/// Shared material cache to avoid per-frame allocations.
#[derive(Resource, Default)]
pub(super) struct BeamMaterialCache {
    entries: std::collections::HashMap<MaterialKey, Handle<StandardMaterial>>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct MaterialKey {
    r: u8,
    g: u8,
    b: u8,
    additive: bool,
    intensity: u8,
    /// Texture filename or empty for untextured. Keeps per-weapon
    /// atlas pickings (arrow / dosray / bytemegabeam) on their own
    /// cache slot so a textured DOS beam doesn't clobber the flat
    /// Bit line's material.
    texture: String,
}

impl BeamMaterialCache {
    pub(super) fn get_or_create(
        &mut self,
        color: LinearRgba,
        additive: bool,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        self.get_or_create_with_intensity(color, additive, 1.0, None, materials)
    }

    /// Like `get_or_create` but scales the emissive strength by
    /// `intensity` and optionally applies a beam texture. Upstream
    /// weapons use intensity to vary glow (BuildLightning=5 shines
    /// hard, GaussCannon=0 is flat), and `texture1=arrow` / `dosray` /
    /// `bytemegabeam` to atlas the beam with a weapon-specific glyph.
    /// Both are quantized into the cache key so we share materials
    /// across weapons that emit the same visual.
    pub(super) fn get_or_create_with_intensity(
        &mut self,
        color: LinearRgba,
        additive: bool,
        intensity: f32,
        texture: Option<(&str, Handle<Image>)>,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        let emissive_scale = (intensity.max(0.5) * 4.0).clamp(1.0, 40.0);
        let (tex_name, texture_handle) = match texture {
            Some((name, handle)) => (name.to_string(), Some(handle)),
            None => (String::new(), None),
        };
        let key = MaterialKey {
            r: (color.red.clamp(0.0, 1.0) * 15.0).round() as u8,
            g: (color.green.clamp(0.0, 1.0) * 15.0).round() as u8,
            b: (color.blue.clamp(0.0, 1.0) * 15.0).round() as u8,
            additive,
            intensity: (emissive_scale * 2.0).round() as u8,
            texture: tex_name,
        };
        self.entries
            .entry(key)
            .or_insert_with(|| {
                let alpha_mode = if additive {
                    AlphaMode::Add
                } else {
                    AlphaMode::Blend
                };
                materials.add(StandardMaterial {
                    base_color: Color::LinearRgba(color),
                    base_color_texture: texture_handle,
                    emissive: color * emissive_scale,
                    unlit: true,
                    alpha_mode,
                    ..default()
                })
            })
            .clone()
    }
}

/// TDF stores RGB either 0-255 or 0-1. Normalize to LinearRgba 0-1.
pub(super) fn tdf_color(rgb: [f32; 3]) -> LinearRgba {
    let [r, g, b] = rgb;
    if r > 2.0 || g > 2.0 || b > 2.0 {
        LinearRgba::new(r / 255.0, g / 255.0, b / 255.0, 1.0)
    } else if r == 0.0 && g == 0.0 && b == 0.0 {
        LinearRgba::new(0.7, 0.7, 0.7, 1.0)
    } else {
        LinearRgba::new(r, g, b, 1.0)
    }
}
