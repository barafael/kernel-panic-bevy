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
///
/// `muzzle_ceg` is the attacker's FBI-authored `[SFXTypes]` entry for
/// the index the COB `FireWeaponN` emits — e.g. Bit's `FireWeapon1`
/// opcodes `emit-sfx 1025 from gunpoint`, index 1, which resolves to
/// `custom:oldskool_shot2` (the cyan arrowflare). `None` when the
/// unit has no SFX table or combat didn't resolve it (e.g. BuildLaser
/// pulses, where the sparkle at the target side is the primary fx).
/// When `Some`, `spawn_weapon_visuals` replays that CEG at the muzzle
/// instead of the generic coloured sphere.
///
/// `delayed_hit` is populated for traveling weapons (projectiles,
/// laser bolts) where the damage + impact visual should only fire
/// when the shell actually reaches the target. Hitscan weapons
/// (beams, melee) leave it `None` — their damage is queued in
/// `DamageQueue` directly and the impact CEG spawns at fire time.
/// See [`DelayedHit`] for the component attached to the resulting
/// visual entity.
pub struct AttackEvent {
    pub attacker_pos: Vec3,
    pub target_pos: Vec3,
    pub weapon_name: Cow<'static, str>,
    pub muzzle_ceg: Option<Cow<'static, str>>,
    pub delayed_hit: Option<DelayedHitInfo>,
}

/// Damage bookkeeping moved onto a traveling visual. The fields match
/// [`crate::units::combat::PendingDamage`] so `tick_weapon_fx` can
/// translate a `DelayedHit` verbatim onto `DamageQueue` on impact.
#[derive(Clone, Debug)]
pub struct DelayedHitInfo {
    pub target: Option<Entity>,
    pub attacker: Entity,
    pub attacker_distance: f32,
}

/// Attached to every traveling-projectile / laser-bolt visual that
/// owes a hit. On the frame the visual's lead reaches the target,
/// `tick_weapon_fx` drains it into `DamageQueue` + `PendingExplosions`
/// and removes this component — once-and-done. The impact position
/// and explosion parameters (rgb / AoE / CEG name) are recovered from
/// the visual's geometry and the `WeaponRegistry` at trigger time, so
/// this component carries only what can't be re-derived.
#[derive(Component)]
pub(super) struct DelayedHit {
    pub target: Option<Entity>,
    pub attacker: Entity,
    pub weapon: Cow<'static, str>,
    pub attacker_distance: f32,
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
///
/// `ceg_name` is the upstream `explosiongenerator=custom:...` value
/// (without the `custom:` prefix) so the CEG particle system can
/// replay the authored emitters at the blast point. Empty string =
/// no scripted CEG; the spawner falls back to a generic sphere +
/// ground ring in that case.
pub struct ExplosionEvent {
    pub pos: Vec3,
    pub rgb: [f32; 3],
    pub radius: f32,
    pub ceg_name: String,
}

/// Event buffer drained by [`spawn::spawn_pending_explosions`]. Separate
/// from [`PendingAttacks`] so systems that model a pure detonation don't
/// have to fake a zero-length beam.
#[derive(Resource, Default)]
pub struct PendingExplosions {
    pub events: Vec<ExplosionEvent>,
}

/// A hitscan beam (Spring `BeamLaser`) drawn as a camera-facing
/// ribbon from `start` to `end`.
///
/// Mirrors `rts/Sim/Projectiles/WeaponProjectiles/BeamLaserProjectile.cpp::Draw`:
/// width axis `xdir = (cameraDir × beam_dir).normalize()`, quad corners
/// `(start ± xdir * thickness, end ± xdir * thickness)`. Each entity
/// carries its own 4-vertex mesh the tick system rewrites per frame —
/// identical pattern to [`LaserBolt`], different only in that `start`
/// and `end` don't move.
#[derive(Component)]
pub(super) struct BeamVisual {
    pub start: Vec3,
    pub end: Vec3,
    pub thickness: f32,
    pub lifetime: f32,
    pub max_lifetime: f32,
    pub mesh: Handle<Mesh>,
    /// Per-sim-frame RGB multiplier from the weapon's `beamdecay`. Each
    /// tick the beam's vertex colors are scaled by this value raised to
    /// the elapsed-frames power, mirroring upstream
    /// `BeamLaserProjectile::Update`. `1.0` means no fade.
    pub decay: f32,
}

/// A traveling laser bolt from a `LaserCannon`-category weapon (Bit
/// Line, Byte MegaBeam, Bug BugShot, RetroDeath streaks, MineLauncher).
///
/// Upstream Spring's
/// `rts/Sim/Projectiles/WeaponProjectiles/LaserProjectile.cpp::Draw`
/// writes a camera-facing ribbon quad each frame whose corners are:
///
/// ```text
/// dir1 = ((pos - cam) × beam_dir).normalize()
/// A = lead  - dir1 * thickness
/// B = tail  - dir1 * thickness
/// C = tail  + dir1 * thickness
/// D = lead  + dir1 * thickness
/// ```
///
/// We reproduce that exactly: each [`LaserBolt`] entity carries its
/// own 4-vertex mesh, and `tick_weapon_fx` rewrites `ATTRIBUTE_POSITION`
/// every frame from the current camera. Transforms stay `IDENTITY` —
/// the mesh lives in world space. Fixed crossed-quad meshes (the
/// earlier approach) can't match this: they read the wrong width
/// from any non-axial angle and disappear entirely when the camera
/// looks along the beam axis.
///
/// The full upstream bolt also includes `texture2` end-caps (two
/// extra half-quads bent around each end via `dir2`). Bit's `Line`
/// sets `texture2=none` and Byte's `MegaBeam` uses a round-cap, so
/// skipping the caps is only wrong for the byte — see §7 TODO.
#[derive(Component)]
pub(super) struct LaserBolt {
    pub origin: Vec3,
    /// Normalized direction from origin toward target.
    pub direction: Vec3,
    /// Distance from origin to the hit point. Lead stops advancing once
    /// it reaches this distance.
    pub total_distance: f32,
    /// Travel speed in elmos/second (`weapon_velocity` from the TDF).
    pub speed: f32,
    /// Maximum segment length — `duration * speed`. The bolt's tail
    /// trails the lead by this distance once fully extended.
    pub max_length: f32,
    /// Full-width half-extent of the ribbon in world units. Upstream
    /// uses `thickness` verbatim (so total quad width = 2 × thickness),
    /// which is why I keep the authored TDF value as-is.
    pub thickness: f32,
    /// Seconds since spawn; drives the lead/tail positions.
    pub elapsed: f32,
    /// Per-entity mesh the tick system rewrites each frame.
    pub mesh: Handle<Mesh>,
}

/// A projectile traveling from origin to target.
///
/// When `trail` is `Some`, the tick system maintains a ribbon of
/// recent positions behind the projectile and rewrites a triangle-
/// strip mesh from those samples every frame. Upstream Spring drives
/// the same visual via `smoketrail=1` with `texture2` pointing at the
/// weapon's trail atlas (`pointertrail`, `firetrail`, `flametrail`…).
#[derive(Component)]
pub(super) struct ProjectileVisual {
    pub origin: Vec3,
    pub target: Vec3,
    pub speed: f32,
    pub progress: f32,
    pub arc_height: f32,
    pub trail: Option<ProjectileTrail>,
}

/// Number of samples retained in the projectile trail's ring buffer.
/// The ribbon mesh draws `N - 1` quads; 16 lands the per-frame
/// rewrite at ~96 vertices per projectile, which is negligible next
/// to the cost of the projectile mesh itself but gives enough length
/// that a Pointer arc's head-to-tail reads as one continuous ribbon.
pub(super) const TRAIL_SAMPLE_COUNT: usize = 16;

/// State for a single projectile's trailing ribbon. Lives on the same
/// entity as the `ProjectileVisual`; the companion trail-ribbon entity
/// is referenced via `ribbon_entity` so both despawn together.
pub(super) struct ProjectileTrail {
    /// Entity carrying the ribbon's `Mesh3d` / material.
    pub ribbon_entity: Entity,
    /// Mesh the tick system rewrites each frame.
    pub mesh: Handle<Mesh>,
    /// Ring-buffer of recent world positions, oldest first. Sized by
    /// `TRAIL_SAMPLE_COUNT`; the tick system prepends the projectile's
    /// current pos and drops the oldest.
    pub samples: Vec<Vec3>,
    /// Ribbon half-width in world units — set at spawn from the
    /// weapon's authored `size` so a Pointer shell leaves a thicker
    /// trail than a BugCannon pellet.
    pub half_width: f32,
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

/// Unit-length primitives shared across projectile / impact visuals.
/// Beams and bolts each own their own per-entity 4-vertex mesh (see
/// [`build_billboard_quad_mesh`]); only the sphere is still shared.
#[derive(Resource, Default)]
pub(super) struct WeaponFxMeshes {
    pub unit_sphere: Option<Handle<Mesh>>,
}

impl WeaponFxMeshes {
    pub(super) fn unit_sphere(&mut self, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
        self.unit_sphere
            .get_or_insert_with(|| meshes.add(Sphere::new(1.0)))
            .clone()
    }
}

/// Build an empty 4-vertex triangle-list mesh ready for per-frame
/// vertex rewrites (see `LaserBolt` / `BeamVisual` tick paths). The
/// caller fills `ATTRIBUTE_POSITION` each frame with the camera-facing
/// corners; UVs are set once at spawn time and never change.
///
/// Vertex order is `[bl, br, tr, tl]` — that is:
///
/// ```text
/// tl(3) --- tr(2)       (UV 0,1)    (UV 1,1)
///   |    \    |           .         .
///   |     \   |           .         .
/// bl(0) --- br(1)       (UV 0,0)    (UV 1,0)
/// ```
///
/// Texture U runs from bl→br (along the ribbon's long axis) and V runs
/// from bl→tl (the ribbon's thickness). With that layout, a single
/// span of `arrow.tga` (four chevrons baked in) stretches once across
/// the bolt's length — matching upstream's `CLaserProjectile::Draw`
/// where `tex1->xstart..xend` is assigned to `tail..lead`.
pub(super) fn build_billboard_quad_mesh() -> Mesh {
    use bevy::asset::RenderAssetUsages;
    use bevy::mesh::{Indices, PrimitiveTopology};

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0_f32; 3]; 4]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0_f32]; 4]);
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_UV_0,
        vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
    );
    // Vertex colors multiply with the cached material's base color.
    // `tick_weapon_fx` rewrites these per frame to apply the weapon's
    // `beamdecay` fade without per-beam material clones; bolts that
    // never decay leave them at white and the multiply is a no-op.
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0_f32; 4]; 4]);
    // Two tris, both orientations so the quad is visible from either side.
    mesh.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3, 0, 2, 1, 0, 3, 2]));
    mesh
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
    /// UV-tile count along the beam length, quantized to integer. Zero
    /// means no tiling (untextured or 1× mapping). Different tile counts
    /// get separate materials so `spawn_beam_laser` can pick a material
    /// whose `uv_transform` matches the beam length.
    tile_count: u32,
}

impl BeamMaterialCache {
    pub(super) fn get_or_create(
        &mut self,
        color: LinearRgba,
        additive: bool,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        self.get_or_create_tiled(color, additive, 1.0, None, 0, materials)
    }

    /// Route into the tiled-material cache. `tile_count=0` means no
    /// `uv_transform` scaling (1× — the texture is drawn once along the
    /// beam, or no texture). Positive values scale the texture to repeat
    /// `tile_count` times along the beam's V-axis, which Bevy's default
    /// `Cuboid` UVs put along the rotated Z axis (i.e. the beam length).
    /// Textures must have a Repeat address-mode sampler (see
    /// [`load_beam_texture`]) or tiling clamps to the last row.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn get_or_create_tiled(
        &mut self,
        color: LinearRgba,
        additive: bool,
        intensity: f32,
        texture: Option<(&str, Handle<Image>)>,
        tile_count: u32,
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
            tile_count,
        };
        self.entries
            .entry(key)
            .or_insert_with(|| {
                let alpha_mode = if additive {
                    AlphaMode::Add
                } else {
                    AlphaMode::Blend
                };
                // The shared `beam_quad` mesh authors UVs so texture U
                // runs along local +Z (beam length) and V across the
                // perpendicular axis. Scaling U by `tile_count` tiles
                // the atlas along the beam without any axis-swap
                // arithmetic.
                let uv_transform = if tile_count > 1 {
                    bevy::math::Affine2::from_scale(Vec2::new(tile_count as f32, 1.0))
                } else {
                    bevy::math::Affine2::IDENTITY
                };
                materials.add(StandardMaterial {
                    base_color: Color::LinearRgba(color),
                    base_color_texture: texture_handle,
                    emissive: color * emissive_scale,
                    unlit: true,
                    alpha_mode,
                    uv_transform,
                    ..default()
                })
            })
            .clone()
    }
}

/// TDF stores RGB either 0-255 or 0-1. Normalize to LinearRgba 0-1.
///
/// Preferred entry point for generic call sites that don't have a
/// [`spring_tdf::WeaponDef`] in hand (impact bursts, build sparkles,
/// ground flashes). Weapon-specific callers should use
/// [`weapon_edge_color`] / [`weapon_core_color`] instead so the
/// upstream per-category defaults apply.
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

/// Resolve the outer-edge tint for a weapon's beam/projectile.
///
/// Delegates to [`spring_tdf::WeaponDef::resolved_rgb`] which runs the
/// three-tier cascade upstream does:
/// 1. explicit `rgbColor` (normalised from 0-255 or 0-1 as needed),
/// 2. synthesised from the legacy `color=` palette via `hs2rgb`
///    (this is the path that gives `RetroDeath` its bright yellow —
///    `color=40` ≈ hue 0.157 → `(1.0, 0.94, 0.0)`),
/// 3. type-aware default (Cannon orange, EmgCannon yellow, lasers white
///    so the core pass preserves the baked texture colour).
pub(super) fn weapon_edge_color(weapon: &spring_tdf::WeaponDef) -> LinearRgba {
    let [r, g, b] = weapon.resolved_rgb();
    LinearRgba::new(r, g, b, 1.0)
}

/// Resolve the inner-core tint. Upstream's two-pass laser draw uses
/// `rgbColor2`, which defaults to white — and that's the reason
/// baked-colour textures (`arrow.tga` cyan, `bytemegabeam.tga`
/// magenta) render in their native colour: the white core pass
/// multiplies the texture by 1 and covers the outer tinted pass when
/// `corethickness` approaches 1.
///
/// We don't yet parse `rgbColor2` (it's not on [`spring_tdf::WeaponDef`]
/// today) so we always return white. If that changes in future,
/// this helper is the one-line update point.
pub(super) fn weapon_core_color(_weapon: &spring_tdf::WeaponDef) -> LinearRgba {
    LinearRgba::WHITE
}
