//! Types shared across the `weapon_fx` sub-modules: the event buffer,
//! visual marker components, the cached beam-material registry, and the
//! TDF-colour normaliser.

use std::sync::Arc;

use bevy::prelude::*;

use crate::units::content::weapons::WeaponId;

/// Describes a single attack for the visual system.
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
    /// Interned weapon id — the combat side resolves it from the
    /// `WeaponBinding` (or the unit's FBI weapon name) before pushing,
    /// so the fx side never string-matches the registry.
    pub weapon_id: WeaponId,
    /// Shared per salvo shot, so a multi-projectile shot clones a
    /// refcount rather than the name.
    pub muzzle_ceg: Option<Arc<str>>,
    pub delayed_hit: Option<DelayedHitInfo>,
    /// True for the Gateway's factory build ray: render the upstream
    /// `BuildArc` white lightning strand (gateway.bos `lua_BuildArc`)
    /// instead of the standard build-laser beam + sparkle.
    pub build_arc: bool,
}

/// Damage bookkeeping moved onto a traveling visual. The fields match
/// [`crate::units::combat::PendingDamage`] so `tick_weapon_fx` can
/// translate a `DelayedHit` verbatim onto `DamageQueue` on impact.
#[derive(Clone, Debug)]
pub struct DelayedHitInfo {
    pub target: Option<Entity>,
    pub attacker: Entity,
    pub attacker_distance: f32,
    /// Gameplay that happens where the shell actually lands, beyond the
    /// weapon's own damage (the NX Flag's denial zone).
    pub on_impact: Option<ImpactEffect>,
    /// World-space emit direction of the `QueryWeapon` piece at fire
    /// time (`CWeapon::weaponDir`) — the launch direction of a
    /// `fixedLauncher` missile / starburst.
    pub muzzle_dir: Vec3,
}

/// A side effect bound to a projectile's point of impact.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ImpactEffect {
    /// `areadenial.lua` on the NX Flag's explosion: spawn the owner's
    /// denial zone at the impact position.
    NxZone {
        owner_team: u8,
        owner_faction: crate::units::components::Faction,
    },
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
    pub weapon: WeaponId,
    pub attacker_distance: f32,
    pub on_impact: Option<ImpactEffect>,
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

/// Upstream `LuaRules/Gadgets/network_arceffect.lua`: the Connection's
/// GaussCannon is drawn as a procedural jagged lightning bolt instead of
/// the engine beam — the weapon's TDF `intensity=0` makes the ordinary
/// beam invisible by design, and `explosiongenerator=custom:none`
/// suppresses the impact burst too.
///
/// One entity per shot. `points` is a polyline of `SEGMENTS + 1` world
/// positions from muzzle to impact: the straight connecting line plus a
/// 160-elmo arch at the midpoint (`arch = 160 · (1 − (2t−1)²)`), with
/// per-point jitter up to ±15 elmos shrinking toward the ends — both
/// numbers lifted verbatim from the gadget's `BuildArc`.
#[derive(Component)]
pub(super) struct LightningArc {
    pub points: Vec<Vec3>,
    /// Half-width of the camera-facing ribbon, in elmos. The gadget
    /// draws with GL line width 4; as a world-space ribbon 3 reads the
    /// same at battle distances.
    pub width: f32,
    pub lifetime: f32,
    pub max_lifetime: f32,
    /// The shared white additive material the arc's quads are batched
    /// under ([`super::batch::FxQuadBatches`]); one quad per segment,
    /// re-oriented to the camera every tick.
    pub material: Handle<StandardMaterial>,
    /// Per-shot tint (electric green with randomized red/blue), applied
    /// through vertex colors so the shared white material in
    /// [`BeamMaterialCache`] is never cloned per arc.
    pub tint: LinearRgba,
}

/// A hitscan beam (Spring `BeamLaser`) drawn as a camera-facing
/// ribbon from `start` to `end`.
///
/// Mirrors `rts/Sim/Projectiles/WeaponProjectiles/BeamLaserProjectile.cpp::Draw`:
/// width axis `xdir = (cameraDir × beam_dir).normalize()`, quad corners
/// `(start ± xdir * thickness, end ± xdir * thickness)`. The tick
/// system pushes that quad into the batch for `material` every frame
/// — identical pattern to [`LaserBolt`], different only in that
/// `start` and `end` don't move.
#[derive(Component)]
pub(super) struct BeamVisual {
    pub start: Vec3,
    pub end: Vec3,
    pub thickness: f32,
    pub lifetime: f32,
    pub max_lifetime: f32,
    /// Cached beam material ([`BeamMaterialCache`]) the quad is
    /// batched under.
    pub material: Handle<StandardMaterial>,
    /// Per-sim-frame RGB multiplier from the weapon's `beamdecay`. Each
    /// tick the beam's vertex colors are scaled by this value raised to
    /// the elapsed-frames power, mirroring upstream
    /// `BeamLaserProjectile::Update`. `1.0` means no fade.
    pub decay: f32,
    /// `texture2` (`laserend` by default): a half-texture cap of
    /// `thickness` depth rounding off each end of the ribbon.
    pub caps: Option<Handle<StandardMaterial>>,
    /// `texture3` (`flare`): a camera-facing glow of this half-size at
    /// the emitter (`thickness · laserflaresize`), BeamLaser only.
    pub flare: Option<(Handle<StandardMaterial>, f32)>,
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
/// We reproduce that exactly: `tick_weapon_fx` computes those corners
/// from the current camera every frame and pushes the quad into the
/// batch for `material` — in world space, no transform. Fixed
/// crossed-quad meshes (the earlier approach) can't match this: they
/// read the wrong width from any non-axial angle and disappear
/// entirely when the camera looks along the beam axis.
///
/// The full upstream bolt also includes `texture2` end-caps (two
/// extra half-quads bent around each end via `dir2`); see
/// `cap_material`.
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
    /// Cached beam material ([`BeamMaterialCache`]) the body quad is
    /// batched under.
    pub material: Handle<StandardMaterial>,
    /// Material of the optional `texture2` end-caps that mirror
    /// upstream `LaserProjectile::Draw`'s endcap pass. When `Some`,
    /// the tick system pushes a lead and a tail cap quad each frame —
    /// each anchored at the bolt's tip, extending one `thickness`
    /// outward along the camera-aligned forward axis (`dir2`). Only
    /// `Byte`'s `MegaBeam` (`texture2=bytelaser`) sets this in the KP
    /// roster.
    pub cap_material: Option<Handle<StandardMaterial>>,
}

/// A projectile traveling from origin to target.
///
/// When `trail` is `Some`, the tick system appends a smoke-trail sample
/// every sim frame and rewrites the ribbon mesh (upstream
/// `smoketrail=1` → `CSmokeTrailProjectile`, textured with `texture2`).
#[derive(Component)]
pub(super) struct ProjectileVisual {
    pub origin: Vec3,
    pub target: Vec3,
    pub speed: f32,
    pub progress: f32,
    pub arc_height: f32,
    pub trail: Option<ProjectileTrail>,
    /// Per-category flight integration, transcribed from the Recoil
    /// engine's projectile classes (see [`super::flight`]). `Direct`
    /// keeps the legacy parametric lerp; the others integrate per tick.
    pub flight: Flight,
    /// Current velocity in elmos/s (integrated flights).
    pub velocity: Vec3,
    /// Accumulated flight time (seconds).
    pub elapsed: f32,
    /// `cegTag=` CEG the projectile emits at its position every sim
    /// frame while it has fuel (`explGenHandler.GenExplosion(cegID, …)`
    /// in each projectile's `Update`) — FlowMissile's
    /// `network_flowtrail` spikes, BugCannon's `corruption_BCtrail`.
    /// Holds the weapon whose `cegTag` it is; the tick reads the name
    /// from the registry instead of carrying a copy.
    pub trail_ceg: Option<WeaponId>,
    /// Per-projectile PRNG seed so ticks can call `spawn_ceg` without a
    /// system `Local` (each projectile gets a stable-but-different roll).
    pub trail_seed: u32,
    /// Last frame's tracked target position, to derive the target's
    /// velocity for the missile lead (`UpdateTargeting`'s `targetVel`).
    pub last_target_pos: Option<Vec3>,
}

/// Flight integration for a projectile. Spawn computes the launch
/// state from the weapon TDF; the tick integrates it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Flight {
    /// Parametric straight line (legacy path — AircraftBomb, etc.).
    Direct,
    /// `CMissileProjectile` (`weapontype=MissileLauncher` — Pointer's
    /// Geometric, NX Flag).
    Missile(super::flight::MissileFlight),
    /// `CStarburstProjectile` (`weapontype=StarburstLauncher` — Flow's
    /// FlowMissile).
    Starburst(super::flight::StarburstFlight),
    /// Ballistic shell (`ballistic=1` + `myGravity` — Exploit's
    /// BugCannon): fixed launch velocity from the ballistic solve +
    /// constant gravity `g` elmos/s² (`myGravity × map gravity`).
    Ballistic { velocity: Vec3, gravity: f32 },
}

impl Flight {
    /// Unit flight direction of a guided projectile.
    pub fn guided_dir(&self) -> Option<Vec3> {
        match self {
            Flight::Missile(m) => Some(m.dir),
            Flight::Starburst(s) => Some(s.dir),
            _ => None,
        }
    }

    /// Whether the projectile still burns fuel (`ttl > 0`) — the window
    /// in which it emits its `cegTag`.
    pub fn has_fuel(&self) -> bool {
        match self {
            Flight::Missile(m) => m.ttl > 0,
            Flight::Starburst(s) => s.ttl > 0,
            Flight::Ballistic { .. } => true,
            Flight::Direct => false,
        }
    }
}

/// Upstream smoke-trail defaults (`WeaponDef.cpp:246-249`), none of which
/// a Kernel Panic weapon overrides: a new trail segment every frame
/// (`smokePeriod=1`), each lingering `smokeTime` = 60 frames, half-width
/// growing `1 + t·smokeSize` (t = age / smokeTime, `smokeSize=7`),
/// brightness `smokeColor=0.65`.
pub(super) const SMOKE_TIME_FRAMES: f32 = 60.0;
pub(super) const SMOKE_SIZE: f32 = 7.0;
pub(super) const SMOKE_COLOR: f32 = 0.65;

/// Ring capacity of a trail ribbon: one sample per sim frame over the
/// `smokeTime` window plus the head.
pub(super) const TRAIL_SAMPLE_COUNT: usize = SMOKE_TIME_FRAMES as usize + 2;

/// One smoke-trail vertex pair: where the projectile was, which way it
/// flew, and how many sim frames ago (`CSmokeTrailProjectile` segment
/// endpoints `pos1/dir1`, `pos2/dir2`, `creationTime`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TrailSample {
    pub pos: Vec3,
    pub dir: Vec3,
    pub age: f32,
    /// Drawn at zero alpha: the launch point (`firstSegment`) and the
    /// impact point (`lastSegment`).
    pub hidden: bool,
}

/// State for a projectile's smoke-trail ribbon. Lives on the projectile
/// while it flies; on impact it moves onto its own entity as a
/// [`FadingTrail`] so the smoke lingers its full `smokeTime` like
/// upstream's independent trail segments.
pub(super) struct ProjectileTrail {
    /// Premultiplied `texture2` material
    /// ([`BeamMaterialCache::get_or_create_trail`]) the ribbon's quads
    /// — one per pair of consecutive samples — are batched under each
    /// tick.
    pub material: Handle<StandardMaterial>,
    /// Samples, oldest first.
    pub samples: std::collections::VecDeque<TrailSample>,
}

/// A trail whose projectile is gone, still fading out.
#[derive(Component)]
pub(super) struct FadingTrail(pub ProjectileTrail);

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

/// Flat horizontal emissive disc spawned at each ground-level impact.
/// Two flavours share the component and its tick:
///
/// * *Synthetic* (weapon-AoE fallback): fixed 0.25×→1.5× expand curve,
///   `growth == 0.0`.
/// * *Authored* (a CEG's `[groundflash]` section, engine
///   `CGroundFlash`): `base_radius` starts at `flashSize` and grows by
///   `growth` elmos/second (`circleGrowth` elmos/frame ×
///   [`GAME_SPEED`]) for `max_lifetime` (`ttl` frames).
///
/// Separated from [`ImpactBurst`] (a 3D fireball) so the two can fade
/// on different curves: the burst rises and fades, the ring expands and
/// stays bright until the end.
#[derive(Component)]
pub(super) struct GroundFlash {
    pub lifetime: f32,
    pub max_lifetime: f32,
    pub base_radius: f32,
    /// Linear growth in elmos/second on top of `base_radius` (authored
    /// `circleGrowth`; `0.0` = synthetic ring).
    pub growth: f32,
}

/// Shared flat-disc mesh for every [`GroundFlash`]. The mesh is a unit
/// circle; the spawn system scales it to the weapon's radius via
/// `Transform::scale`.
#[derive(Resource, Default)]
pub(super) struct GroundFlashAssets {
    pub mesh: Option<Handle<Mesh>>,
}

/// Unit-length primitives shared across projectile / impact visuals.
/// Beams, bolts and the other ribbons draw through the per-material
/// quad batches (`super::batch`); only the sphere is a shared asset.
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
    /// Smoke trails: premultiplied blend, white — see
    /// [`BeamMaterialCache::get_or_create_trail`].
    premultiplied: bool,
    intensity: u8,
    /// Texture image, `None` for untextured. Keeps per-weapon atlas
    /// pickings (arrow / dosray / bytemegabeam) on their own cache
    /// slot so a textured DOS beam doesn't clobber the flat Bit line's
    /// material. Keyed by asset id rather than name so the per-shot
    /// lookup never allocates.
    texture: Option<AssetId<Image>>,
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
        texture: Option<Handle<Image>>,
        tile_count: u32,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        let emissive_scale = (intensity.max(0.5) * 4.0).clamp(1.0, 40.0);
        let key = MaterialKey {
            r: (color.red.clamp(0.0, 1.0) * 15.0).round() as u8,
            g: (color.green.clamp(0.0, 1.0) * 15.0).round() as u8,
            b: (color.blue.clamp(0.0, 1.0) * 15.0).round() as u8,
            additive,
            premultiplied: false,
            intensity: (emissive_scale * 2.0).round() as u8,
            texture: texture.as_ref().map(Handle::id),
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
                    base_color_texture: texture,
                    emissive: color * emissive_scale,
                    unlit: true,
                    alpha_mode,
                    uv_transform,
                    ..default()
                })
            })
            .clone()
    }

    /// Smoke-trail material for a weapon's `texture2`: white ×
    /// texture, premultiplied — upstream's effects pass draws trails
    /// with `GL_ONE, GL_ONE_MINUS_SRC_ALPHA`, colour and alpha both
    /// scaled by the per-vertex fade. One per texture, so every trail
    /// of a weapon (and every weapon sharing the texture) batches into
    /// one mesh instead of minting a material per projectile.
    pub(super) fn get_or_create_trail(
        &mut self,
        texture: Option<Handle<Image>>,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        let key = MaterialKey {
            r: 15,
            g: 15,
            b: 15,
            additive: false,
            premultiplied: true,
            intensity: 0,
            texture: texture.as_ref().map(Handle::id),
            tile_count: 0,
        };
        self.entries
            .entry(key)
            .or_insert_with(|| {
                // Default (back-face) culling: the batch emits both
                // windings per quad, so the ribbon reads from either
                // side while each face is drawn once — the same
                // coverage as the old unculled triangle strip.
                materials.add(StandardMaterial {
                    base_color: Color::WHITE,
                    base_color_texture: texture,
                    unlit: true,
                    alpha_mode: AlphaMode::Premultiplied,
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
