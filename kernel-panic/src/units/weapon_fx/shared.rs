//! Types shared across the `weapon_fx` sub-modules: the event buffer,
//! visual marker components, the cached beam-material registry, and the
//! TDF-colour normaliser.

use bevy::prelude::*;

/// Describes a single attack for the visual system.
pub struct AttackEvent {
    pub attacker_pos: Vec3,
    pub target_pos: Vec3,
    pub weapon_name: String,
}

/// Buffer written by the combat system, drained by visual systems.
#[derive(Resource, Default)]
pub struct PendingAttacks {
    pub events: Vec<AttackEvent>,
}

/// A beam visual that fades over its lifetime.
#[derive(Component)]
pub(super) struct BeamVisual {
    pub lifetime: f32,
    pub max_lifetime: f32,
}

/// A projectile traveling from origin to target.
#[derive(Component)]
pub(super) struct ProjectileVisual {
    pub origin: Vec3,
    pub target: Vec3,
    pub speed: f32,
    pub progress: f32,
    pub arc_height: f32,
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

/// Shared material cache to avoid per-frame allocations.
#[derive(Resource, Default)]
pub(super) struct BeamMaterialCache {
    entries: Vec<CachedMaterial>,
}

struct CachedMaterial {
    key: MaterialKey,
    handle: Handle<StandardMaterial>,
}

#[derive(Clone, Copy, PartialEq)]
struct MaterialKey {
    r: u8,
    g: u8,
    b: u8,
    additive: bool,
}

impl BeamMaterialCache {
    pub(super) fn get_or_create(
        &mut self,
        color: LinearRgba,
        additive: bool,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        let key = MaterialKey {
            r: (color.red.clamp(0.0, 1.0) * 15.0).round() as u8,
            g: (color.green.clamp(0.0, 1.0) * 15.0).round() as u8,
            b: (color.blue.clamp(0.0, 1.0) * 15.0).round() as u8,
            additive,
        };
        for entry in &self.entries {
            if entry.key == key {
                return entry.handle.clone();
            }
        }
        let alpha_mode = if additive {
            AlphaMode::Add
        } else {
            AlphaMode::Blend
        };
        let handle = materials.add(StandardMaterial {
            base_color: Color::LinearRgba(color),
            emissive: color * 8.0,
            unlit: true,
            alpha_mode,
            ..default()
        });
        self.entries.push(CachedMaterial {
            key,
            handle: handle.clone(),
        });
        handle
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
