//! Spawning side of weapon visuals: drains `PendingAttacks` and routes each
//! event to the appropriate effect spawner (beam, burst, projectile, melee,
//! plus the bonus nanoframe sparkle for build lasers).

use bevy::prelude::*;

use crate::units::content::weapons::WeaponId;

use super::ceg::{CegRegistry, CegRenderAssets, spawn_ceg};
use super::shared::{
    AttackEvent, BeamMaterialCache, BeamVisual, DelayedHit, Flight, GroundFlash, GroundFlashAssets,
    ImpactBurst, ImpactBurstAssets, LaserBolt, LightningArc, PendingAttacks, PendingExplosions,
    ProjectileTrail, ProjectileVisual, TRAIL_SAMPLE_COUNT, WeaponFxMeshes, tdf_color,
    weapon_core_color, weapon_edge_color,
};
use crate::rng::{next_f32, next_signed};
use crate::sim::{GAME_SPEED, frames_to_secs};
use crate::units::assets::meshes::{S3OModelCache, load_beam_texture, load_s3o_mesh};
use crate::units::content::weapons::WeaponRegistry;

/// True for `BuildLaser` (the upstream build-laser weapon name). The
/// `BuildLaserNoEffect` variant intentionally suppresses the impact particles,
/// so only the bare-name version triggers `BuildSparkle` spawn.
fn is_build_laser(weapon_id: WeaponId) -> bool {
    weapon_id == WeaponId::BUILD_LASER
}

/// Radius (elmos) of the muzzle-flash burst at the firing unit. Small
/// enough that it reads as "that unit just shot" without obscuring the
/// unit itself; the underlying [`ImpactBurst`] decays over its fixed
/// lifetime so there's nothing to tune per weapon.
const MUZZLE_FLASH_RADIUS: f32 = 6.0;

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_weapon_visuals(
    mut pending: ResMut<PendingAttacks>,
    weapon_registry: Res<WeaponRegistry>,
    ceg_registry: Res<CegRegistry>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<S3OModelCache>,
    mut cache: ResMut<BeamMaterialCache>,
    mut impact_assets: ResMut<ImpactBurstAssets>,
    mut flash_assets: ResMut<GroundFlashAssets>,
    mut fx_meshes: ResMut<WeaponFxMeshes>,
    mut ceg_assets: ResMut<CegRenderAssets>,
    mut rng: Local<u32>,
) {
    for event in pending.events.drain(..) {
        // Ids are interned through this same registry at the combat
        // side, so the lookup is infallible.
        let weapon = weapon_registry.by_id(event.weapon_id);

        let dir = event.target_pos - event.attacker_pos;
        let length = dir.length();
        if length < 0.1 {
            continue;
        }

        // Classify through the typed category — `derived_weapon_type`
        // runs the `weapondefs_post.lua` legacy-tag shim so weapons that
        // authored `beamweapon=1 lineofsight=1` without a literal
        // `weaponType=` still land on `LaserCannon` (Bit `Line`, Byte
        // `MegaBeam`, RetroDeath family, MineLauncher). Shim details
        // live on [`spring_tdf::WeaponDef::derived_weapon_type`].
        let category = weapon.category();
        let is_melee = category == spring_tdf::WeaponCategory::Melee;
        let is_projectile = weapon.is_projectile();
        let is_beam_laser = category == spring_tdf::WeaponCategory::BeamLaser;
        let is_laser_cannon = category == spring_tdf::WeaponCategory::LaserCannon;

        // Primary visual entity — the projectile / bolt that carries
        // the `DelayedHit` if this attack has deferred damage.
        let mut primary_visual: Option<Entity> = None;

        // The Connection's GaussCannon replaces *all* engine visuals
        // with the gadget-drawn lightning arc (upstream
        // `network_arceffect.lua`): the TDF beam has `intensity=0`
        // (invisible by design) and `explosiongenerator=custom:none`.
        let is_gauss_arc = weapon_registry.is_gauss_cannon(event.weapon_id);

        if is_gauss_arc {
            spawn_lightning_arc(
                &event,
                &mut rng,
                &mut commands,
                &mut materials,
                &mut cache,
                ArcFlavor::BigArc,
            );
        } else if event.build_arc {
            // Upstream gateway.bos calls lua_BuildArc(center) each frame
            // it's building — a white, narrower, less jittery cousin of
            // the shot arc (drawArc shares the geometry; only width,
            // spray, color differ). Live 16 frames like the gadget.
            spawn_lightning_arc(
                &event,
                &mut rng,
                &mut commands,
                &mut materials,
                &mut cache,
                ArcFlavor::BuildArc,
            );
        } else if is_melee {
            // Upstream worm.bos `FireWeapon1` telescopes the head and
            // detonates the Wormsplash weapon at the bite points
            // (`emit-sfx 4097 from head/end`) — the bite itself has
            // `explosiongenerator=custom:none` and no projectile, so
            // the cyan shockwave ring IS the visible attack. Replay
            // that CEG at the impact point; the generic melee flash is
            // only the fallback for registries without the splash.
            let bite_dir = dir / length.max(1e-6);
            let splash_played = weapon_registry.is_worm_bite(event.weapon_id)
                && spawn_ceg(
                    "corruption_worm_splash",
                    event.target_pos,
                    bite_dir,
                    &ceg_registry,
                    &mut rng,
                    &mut commands,
                    &mut meshes,
                    &mut materials,
                    &mut images,
                    &mut model_cache,
                    &mut ceg_assets,
                );
            if !splash_played {
                spawn_melee_flash(
                    &event,
                    &mut commands,
                    &mut meshes,
                    &mut materials,
                    &mut cache,
                    &mut fx_meshes,
                    &mut impact_assets,
                );
            }
        } else if is_projectile {
            primary_visual = Some(spawn_projectile(
                &event,
                weapon,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut model_cache,
                &mut cache,
                &mut fx_meshes,
            ));
        } else if is_laser_cannon {
            primary_visual = Some(spawn_laser_bolt(
                &event,
                weapon,
                &mut commands,
                &mut materials,
                &mut images,
                &mut model_cache,
                &mut cache,
            ));
        } else if is_beam_laser {
            spawn_textured_beam(
                &event,
                weapon,
                true,
                &mut commands,
                &mut materials,
                &mut images,
                &mut model_cache,
                &mut cache,
            );
        } else {
            // Untyped weapon (shouldn't happen for KP's roster). Fall
            // back to a flat untextured beam so there's *some* feedback.
            spawn_textured_beam(
                &event,
                weapon,
                false,
                &mut commands,
                &mut materials,
                &mut images,
                &mut model_cache,
                &mut cache,
            );
        }

        // Hand the deferred payload to the visual `tick_weapon_fx`
        // will process on impact. The weapon's name is enough to
        // recover rgb / AoE / explosion CEG via the registry then.
        if let (Some(visual), Some(delayed)) = (primary_visual, event.delayed_hit.as_ref()) {
            commands.entity(visual).insert(DelayedHit {
                target: delayed.target,
                attacker: delayed.attacker,
                weapon: event.weapon_id,
                attacker_distance: delayed.attacker_distance,
                on_impact: delayed.on_impact,
            });
        }

        // Muzzle flash: the CEG the unit's `FireWeapon1` emits from its
        // muzzle (`emit-sfx 1024+i`), resolved by combat into
        // `event.muzzle_ceg` — Bit's cyan `arrowflare`, Byte/Pointer's
        // soft-blue `oldskool_shot1`. Upstream has no automatic muzzle
        // flash, so a unit whose script emits nothing (Flow, Packet, …)
        // gets nothing. The coloured sphere only stands in when the named
        // CEG is missing from the registry.
        if !is_melee
            && !is_build_laser(event.weapon_id)
            && !is_gauss_arc
            && let Some(muzzle_ceg) = event.muzzle_ceg.as_deref()
        {
            let muzzle_dir = (event.target_pos - event.attacker_pos).normalize_or(Vec3::Y);
            let ceg_spawned = spawn_ceg(
                muzzle_ceg,
                event.attacker_pos,
                muzzle_dir,
                &ceg_registry,
                &mut rng,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut model_cache,
                &mut ceg_assets,
            );
            if !ceg_spawned {
                spawn_impact_burst(
                    event.attacker_pos,
                    weapon.rgb_color,
                    MUZZLE_FLASH_RADIUS,
                    &mut commands,
                    &mut meshes,
                    &mut materials,
                    &mut cache,
                    &mut impact_assets,
                );
            }
        }

        // Build lasers also drop a short-lived "nanoframe pixel" sprite at
        // the target end: the authored `oldskool_build` CEG, replayed
        // through the runtime like any other explosion generator (the
        // NoEffect variant intentionally skips this). Before the CEG
        // runner this was hand-rolled here — with drifted speed/spread
        // and hash "jitter" — which is exactly the divergence the
        // runtime exists to prevent.
        if is_build_laser(event.weapon_id) && !event.build_arc {
            spawn_ceg(
                "oldskool_build",
                event.target_pos,
                Vec3::Y,
                &ceg_registry,
                &mut rng,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut model_cache,
                &mut ceg_assets,
            );
        } else if !is_melee && !is_gauss_arc && event.delayed_hit.is_none() {
            // Upstream CEG is the source of truth for impact particles:
            // the weapon's `explosiongenerator=custom:NAME` resolves to a
            // CSimpleParticleSystem definition in `gamedata/explosions/`.
            // Replay those particles faithfully — colormap, size growth,
            // directional spread, lifetime all come from the authored
            // script. Fall back to the synthesised burst + ground flash
            // only when the CEG is missing or references a class we
            // don't yet support (CBitmapMuzzleFlame etc).
            let dir = (event.target_pos - event.attacker_pos).normalize_or(Vec3::Y);
            let used_ceg = !weapon.explosion_generator.is_empty()
                && spawn_ceg(
                    &weapon.explosion_generator,
                    event.target_pos,
                    dir,
                    &ceg_registry,
                    &mut rng,
                    &mut commands,
                    &mut meshes,
                    &mut materials,
                    &mut images,
                    &mut model_cache,
                    &mut ceg_assets,
                );
            if !used_ceg {
                let aoe = weapon.area_of_effect.max(4.0);
                spawn_impact_burst(
                    event.target_pos,
                    weapon.rgb_color,
                    aoe,
                    &mut commands,
                    &mut meshes,
                    &mut materials,
                    &mut cache,
                    &mut impact_assets,
                );
                spawn_ground_flash(
                    event.target_pos,
                    weapon.rgb_color,
                    aoe,
                    &mut commands,
                    &mut meshes,
                    &mut materials,
                    &mut cache,
                    &mut flash_assets,
                );
            }
        }
    }
}

/// Drain every [`PendingExplosions`] entry and spawn a matching
/// burst + ground flash. No beam, no muzzle flash, no projectile — this
/// path is for standalone detonations (unit-death `ExplodeAs`, kamikaze
/// triggers, command-fire area blasts). Scaling matches the per-weapon
/// impact so a Logic Bomb's death boom reads the same visual language
/// as its in-flight hit.
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_pending_explosions(
    mut pending: ResMut<PendingExplosions>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<S3OModelCache>,
    mut cache: ResMut<BeamMaterialCache>,
    mut impact_assets: ResMut<ImpactBurstAssets>,
    mut flash_assets: ResMut<GroundFlashAssets>,
    mut ceg_assets: ResMut<CegRenderAssets>,
    ceg_registry: Res<CegRegistry>,
    mut rng: Local<u32>,
) {
    for event in pending.events.drain(..) {
        // Drive standalone explosions through the same CEG lookup: if
        // the caller passed a named CEG (death `ExplodeAs`, mine kill,
        // SIGTERM), replay its emitters; otherwise fall back to the
        // synthesised burst so there's still feedback for unscripted
        // detonations.
        let used_ceg = !event.ceg_name.is_empty()
            && spawn_ceg(
                &event.ceg_name,
                event.pos,
                Vec3::Y,
                &ceg_registry,
                &mut rng,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut model_cache,
                &mut ceg_assets,
            );
        if !used_ceg {
            let aoe = event.radius.max(4.0);
            spawn_impact_burst(
                event.pos,
                event.rgb,
                aoe,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut cache,
                &mut impact_assets,
            );
            spawn_ground_flash(
                event.pos,
                event.rgb,
                aoe,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut cache,
                &mut flash_assets,
            );
        }
    }
}

/// Flat horizontal emissive ring at `pos`. Radius animates from 0.25× to
/// 1.5× `aoe` over the lifetime so the ring visibly "blooms" outward from
/// the impact point before fading. Mesh is a shared unit-circle; the
/// spawn-time scale carries the initial radius so the tick system only
/// needs to grow `base_radius` — no material mutation per frame.
///
/// The ground plane is approximated at `pos.y + 0.5` so the ring sits
/// slightly above terrain without z-fighting. Units that explode mid-air
/// (Flow, command-fire projectiles) still project their ring close to
/// the blast center — good enough; a world-space ground snap would
/// require a heightmap sample and isn't worth the dependency on this
/// visual-only path.
#[allow(clippy::too_many_arguments)]
fn spawn_ground_flash(
    pos: Vec3,
    rgb: [f32; 3],
    aoe: f32,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut BeamMaterialCache,
    flash_assets: &mut GroundFlashAssets,
) {
    let mesh = flash_assets
        .mesh
        .get_or_insert_with(|| meshes.add(Circle::new(1.0)))
        .clone();

    let color = tdf_color(rgb);
    let material = cache.get_or_create_tiled(color, true, 2.0, None, 0, materials);

    // Clamp so even mines / SIGTERM don't swallow the screen, but keep
    // beam pings (AoE=8) readable. Small ring radius feels snappier than
    // the full blast sphere.
    let base_radius = (aoe * 0.5).clamp(4.0, 80.0);
    let life = 0.45;

    // `Circle` is XY by default; rotate to lie flat on XZ so it hugs the ground.
    let flat = Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);

    commands.spawn((
        GroundFlash {
            lifetime: life,
            max_lifetime: life,
            base_radius,
            growth: 0.0,
        },
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(pos + Vec3::Y * 0.5)
            .with_rotation(flat)
            .with_scale(Vec3::splat(base_radius * 0.25)),
    ));
}

#[allow(clippy::too_many_arguments)]
fn spawn_impact_burst(
    target_pos: Vec3,
    rgb: [f32; 3],
    aoe: f32,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut BeamMaterialCache,
    impact_assets: &mut ImpactBurstAssets,
) {
    let mesh = impact_assets
        .mesh
        .get_or_insert_with(|| meshes.add(Sphere::new(1.0).mesh().ico(2).unwrap()))
        .clone();

    let color = tdf_color(rgb);
    let material = cache.get_or_create(color, true, materials);

    // Ground-hit weapons benefit from a bigger puff; single-target hit-scan
    // (AoE=8) gets a small blip, while Logic Bomb (AoE=512) bursts large.
    // Clamp so even the largest explosions stay readable.
    let base_size = (aoe * 0.25).clamp(3.0, 24.0);
    let life = 0.35;

    commands.spawn((
        ImpactBurst {
            lifetime: life,
            max_lifetime: life,
            base_size,
        },
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(target_pos + Vec3::Y * 2.0).with_scale(Vec3::splat(base_size)),
    ));
}

/// Resolve a TDF `texture1=...` name to a Bevy handle *with repeat
/// sampling enabled* plus the texture's pixel dimensions, so callers
/// can compute a tile count that preserves the texture's native
/// aspect ratio along the beam length. Returns `None` for textures
/// that couldn't be loaded (disk miss) or weapons that set `texture1=`
/// to empty / `none`.
///
/// Upstream `RESOURCES.TDF` declares atlas aliases — `bytelasermid`
/// points at the on-disk `bytemegabeammid.tga`, not a file named after
/// the alias itself. We consult [`CegRegistry::resolve_texture`] first
/// (which mirrors the same alias table) and fall back to a literal
/// `{name}.tga` lookup for weapon textures that don't happen to be
/// aliased. Only that fallback builds a filename; the aliased path
/// (every KP core weapon) runs allocation-free, which matters because
/// this is called for every shot.
fn beam_texture(
    tex1: &str,
    model_cache: &mut S3OModelCache,
    images: &mut Assets<Image>,
) -> Option<(Handle<Image>, f32)> {
    if tex1.is_empty() || tex1.eq_ignore_ascii_case("none") {
        return None;
    }
    let (handle, w, h) = match CegRegistry::resolve_texture(tex1) {
        Some(filename) => load_beam_texture(filename, model_cache, images)?,
        None => load_beam_texture(&format!("{tex1}.tga"), model_cache, images)?,
    };
    let aspect = if h > 0 { w as f32 / h as f32 } else { 1.0 };
    Some((handle, aspect))
}

/// Spawn a `BeamLaser` hit-scan ribbon from attacker to target.
///
/// Mirrors the upstream two-pass draw in
/// `rts/Sim/Projectiles/WeaponProjectiles/BeamLaserProjectile.cpp`:
/// the outer pass draws the full-thickness quad with the texture
/// tinted by `rgbColor` ([`weapon_edge_color`]); the core pass draws
/// a `thickness * corethickness` quad on top with `rgbColor2` =
/// white ([`weapon_core_color`]), which is what preserves baked-colour
/// textures unchanged in the center. Both passes use the same beam
/// texture (`texture1`); end caps (`texture2`) would go through
/// `visuals.texture2` but we haven't ported that detail yet.
#[allow(clippy::too_many_arguments)]
fn spawn_textured_beam(
    event: &AttackEvent,
    weapon: &spring_tdf::WeaponDef,
    is_beam_laser: bool,
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    cache: &mut BeamMaterialCache,
) {
    let dir = event.target_pos - event.attacker_pos;
    let length = dir.length();
    if length < 0.1 {
        return;
    }
    // Trust the TDF-authored thickness verbatim. The earlier 0.9×
    // squeeze + clamp(1.5, 12.0) suppressed Byte's authored 16-elmo
    // MegaBeam down to a 12-unit stripe, which is why the impact
    // flare and core were reading as a unit hairline rather than the
    // fat upstream ribbon.
    let thickness = weapon.thickness.max(1.0);

    // BeamLaser lifetime comes from `beamtime`/`beamttl`. Color decay is
    // a separate per-frame multiplier on the vertex colors (see
    // `BeamVisual.decay`); it doesn't extend the lifetime, contrary to
    // an earlier port-ism. 0.08 s is a pragmatic floor so single-frame
    // shots still visibly flash.
    let lifetime = if is_beam_laser {
        let ttl_sec = frames_to_secs(weapon.beam_ttl);
        weapon.beam_time.max(ttl_sec).max(0.08)
    } else {
        weapon.duration.max(0.08)
    };

    // Tile the texture along beam length so `arrow`'s `>>>>` or
    // `dosray`'s `01010101` stream reads as a sequence of glyphs
    // rather than one stretched smear. 56 elmos/tile matches the
    // on-screen glyph size in upstream footage at default zoom.
    const ARROW_TILE_LENGTH: f32 = 56.0;
    let texture = beam_texture(&weapon.texture1, model_cache, images);
    let has_texture = texture.is_some();
    let tile_count = if has_texture {
        ((length / ARROW_TILE_LENGTH).round() as u32).clamp(1, 24)
    } else {
        0
    };

    // Outer (edge) pass. The beam entity is pure bookkeeping: the tick
    // system pushes its camera-facing quad into the batch for
    // `material` every frame (`super::batch`).
    let edge_color = weapon_edge_color(weapon);
    let outer_mat = cache.get_or_create_tiled(
        edge_color,
        true,
        weapon.intensity,
        texture.as_ref().map(|(handle, _)| handle.clone()),
        tile_count,
        materials,
    );
    commands.spawn(BeamVisual {
        start: event.attacker_pos,
        end: event.target_pos,
        thickness,
        lifetime,
        max_lifetime: lifetime,
        material: outer_mat,
        decay: weapon.beam_decay,
    });

    // Core pass: `corethickness × rgbColor2 (white) × texture`. Always
    // drawn when authored > 0. For `corethickness=1` the core fully
    // covers the outer and baked-colour textures (arrow cyan,
    // bytemegabeam magenta) come through intact. Lower ratios let the
    // outer halo peek around for a two-tone look.
    let core_ratio = weapon.core_thickness.clamp(0.0, 1.0);
    if core_ratio > 0.01 {
        let core_thickness = thickness * core_ratio;
        let core_color = weapon_core_color(weapon);
        let core_mat = cache.get_or_create_tiled(
            core_color,
            true,
            weapon.intensity.max(1.0),
            texture.map(|(handle, _)| handle),
            tile_count,
            materials,
        );
        commands.spawn(BeamVisual {
            start: event.attacker_pos,
            end: event.target_pos,
            thickness: core_thickness,
            lifetime,
            max_lifetime: lifetime,
            material: core_mat,
            decay: weapon.beam_decay,
        });
    }
}

/// Spawn a traveling laser bolt for `LaserCannon` weapons (Bit `Line`,
/// Byte `MegaBeam`, RetroDeath death streaks, Bug `BugShot`, Virus
/// `VirusBeam`, MineLauncher). Upstream's
/// `rts/Sim/Projectiles/WeaponProjectiles/LaserProjectile.cpp`
/// renders a short segment of length
/// `max_length = duration * weapon_velocity` flying at
/// `weapon_velocity` elmos/sec: the lead extends from the muzzle,
/// the tail trails by up to `max_length`, then contracts after
/// impact. [`tick_weapon_fx`] animates position + length.
///
/// The atlas stretches once across the moving bolt — we don't tile,
/// matching upstream's `tex1->xstart..xend` assignment at tail/lead.
/// Same two-pass draw as `spawn_textured_beam`: outer edge with
/// `rgbColor × texture`, core with white × texture (covered fully
/// when `corethickness=1`).
fn spawn_laser_bolt(
    event: &AttackEvent,
    weapon: &spring_tdf::WeaponDef,
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    cache: &mut BeamMaterialCache,
) -> Entity {
    let delta = event.target_pos - event.attacker_pos;
    let distance = delta.length().max(0.1);
    let direction = delta / distance;

    // Trust the authored thickness — see spawn_textured_beam.
    let thickness = weapon.thickness.max(1.0);

    let speed = if weapon.weapon_velocity > 0.0 {
        weapon.weapon_velocity
    } else {
        256.0
    };
    let duration = weapon.duration.max(0.05);
    // `max_length = duration * speed` per upstream, with a floor of
    // `thickness * 3` so very short ticks still read as a bolt.
    let max_length = (speed * duration).max(thickness * 3.0);

    let texture = beam_texture(&weapon.texture1, model_cache, images);
    let has_texture = texture.is_some();
    let tile_count: u32 = if has_texture { 1 } else { 0 };

    // The bolt entity is pure bookkeeping: the tick system computes
    // the camera-facing corners each frame and pushes them into the
    // batch for `material` — world-space positions, no transform.
    let edge_color = weapon_edge_color(weapon);
    let outer_mat = cache.get_or_create_tiled(
        edge_color,
        true,
        weapon.intensity,
        texture.as_ref().map(|(handle, _)| handle.clone()),
        tile_count,
        materials,
    );
    // Optional endcaps from `texture2`. Upstream's `LaserProjectile::Draw`
    // wraps two extra half-quads at the lead and tail using `texture2`
    // to round off the bolt — `Byte`'s `MegaBeam` is the only weapon in
    // the KP roster that authors this (`texture2=bytelaser`); everyone
    // else either omits texture2 or sets it to `none`. Skipped when the
    // lookup fails so a typo in the TDF doesn't crash the game.
    let cap_material = bolt_cap_material(weapon, edge_color, materials, images, model_cache, cache);
    let outer_entity = commands
        .spawn(LaserBolt {
            origin: event.attacker_pos,
            direction,
            total_distance: distance,
            speed,
            max_length,
            thickness,
            elapsed: 0.0,
            material: outer_mat,
            cap_material,
        })
        .id();

    // Core pass: `corethickness × white × texture`. For Bit's
    // `Line` (corethickness=1) the core is full width and fully covers
    // the outer pass, leaving the arrow texture's baked cyan intact.
    // For Byte's MegaBeam (corethickness=0.5) the outer magenta halo
    // surrounds a hot-white core.
    let core_ratio = weapon.core_thickness.clamp(0.0, 1.0);
    if core_ratio > 0.01 {
        let core_thickness = thickness * core_ratio;
        let core_color = weapon_core_color(weapon);
        let core_mat = cache.get_or_create_tiled(
            core_color,
            true,
            weapon.intensity.max(1.0),
            texture.map(|(handle, _)| handle),
            tile_count,
            materials,
        );
        commands.spawn(LaserBolt {
            origin: event.attacker_pos,
            direction,
            total_distance: distance,
            speed,
            max_length,
            thickness: core_thickness,
            elapsed: 0.0,
            material: core_mat,
            // Caps live on the outer bolt only — duplicating them on
            // the core would just sit a second pair on top.
            cap_material: None,
        });
    }
    outer_entity
}

/// Projectile (Pointer Geometric / NX, BugCannon, SigTerm bomb,
/// Logic Bomb end-game blast, Cannon shells).
///
/// Loads the weapon's authored [`spring_tdf::WeaponDef::model`]
/// (`octashot.s3o` for the Pointer, `sigterm.s3o` for the Terminal's
/// airstrike) through the shared [`S3OModelCache`] so every shot of
/// the same weapon reuses one mesh handle. When no model is set the
/// projectile falls back to a small unit sphere — this covers plain
/// cannon/plasma weapons that upstream Spring renders as a sprite
/// billboard.
///
/// Flight model selection, transcribed from the Recoil engine (see
/// [`super::flight`]):
///
/// - `MissileLauncher` (Pointer's Geometric, NX): `CMissileLauncher::
///   FireImpl` + `CMissileProjectile` — launch toward the target (up-biased
///   by `trajectoryHeight`, or along the muzzle when `fixedLauncher`),
///   `extraHeight` arc, per-frame vector steering by `turnrate`.
/// - `StarburstLauncher` (Flow's FlowMissile): launch from 2 elmos above
///   the muzzle along the muzzle piece's emit dir (`fixedLauncher`; else
///   straight up), `weapontimer` frames of ascent, a `turnrate` swing
///   onto the target, then homing + acceleration to `weaponvelocity`.
/// - `ballistic=1` + `myGravity` (Exploit's BugCannon):
///   `CannonProjectile` integrates `myGravity × map gravity`; the
///   launch angle is the low-arc ballistic solve so the shell lands on
///   the target.
/// - everything else (AircraftBomb, plain cannons): direct parametric
///   lerp as before.
#[allow(clippy::too_many_arguments)]
fn spawn_projectile(
    event: &AttackEvent,
    weapon: &spring_tdf::WeaponDef,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    cache: &mut BeamMaterialCache,
    fx_meshes: &mut WeaponFxMeshes,
) -> Entity {
    // BugCannon's shell is `texture1=black` — an opaque near-black ball,
    // not the default orange Cannon marker. Darken the fallback sphere.
    let color = if weapon.texture1.trim().eq_ignore_ascii_case("black") {
        LinearRgba::new(0.02, 0.02, 0.02, 1.0)
    } else {
        tdf_color(weapon.rgb_color)
    };

    let speed = if weapon.weapon_velocity > 0.0 {
        weapon.weapon_velocity
    } else {
        400.0
    };

    let arc_height = weapon.trajectory_height * 0.4;

    // ── Flight model (see the doc comment above) ──
    let to_target = event.target_pos - event.attacker_pos;
    let up = Vec3::Y;
    const MAP_GRAVITY: f32 = 50.0; // elmos/s², from the map's gravity=
    let muzzle_dir = event
        .delayed_hit
        .as_ref()
        .map_or_else(|| to_target.normalize_or(up), |d| d.muzzle_dir);
    let launch = super::flight::Launch {
        weapon,
        muzzle_pos: event.attacker_pos,
        muzzle_dir,
        target_pos: event.target_pos,
    };
    let mut spawn_pos = event.attacker_pos;
    let flight = if weapon.category() == spring_tdf::WeaponCategory::StarburstLauncher {
        let (f, pos) = super::flight::StarburstFlight::launch(launch);
        spawn_pos = pos;
        Flight::Starburst(f)
    } else if weapon.category() == spring_tdf::WeaponCategory::MissileLauncher {
        let (f, pos) = super::flight::MissileFlight::launch(launch);
        spawn_pos = pos;
        Flight::Missile(f)
    // `myGravity > 0` is the discriminator: every KPK ballistic shell
    // carries a gravity multiplier (BugCannon .3, WMD .4). A
    // `ballistic=1` tag with no gravity (SwallowDamage's burnblow) is a
    // point-blank damage weapon, not a lobbed projectile — solving with
    // g = 0 would divide by zero.
    } else if weapon.category() == spring_tdf::WeaponCategory::Cannon
        && weapon.ballistic
        && weapon.my_gravity > 0.0
    {
        // Low-arc ballistic solve: sin(2θ) = g·d / v²  (+ height term).
        let g = weapon.my_gravity * MAP_GRAVITY;
        let horizontal = Vec3::new(to_target.x, 0.0, to_target.z);
        let d = horizontal.length();
        let h = to_target.y;
        let v = if weapon.start_velocity > 0.0 {
            weapon.start_velocity
        } else {
            speed
        };
        // tanθ = (v² − √(v⁴ − g(g·d² + 2·h·v²))) / (g·d) — low arc.
        let disc = v * v * v * v - g * (g * d * d + 2.0 * h * v * v);
        let velocity = if d > 1.0 && disc >= 0.0 {
            let tan_theta = (v * v - disc.sqrt()) / (g * d);
            let cos_theta = 1.0 / (1.0 + tan_theta * tan_theta).sqrt();
            let dir = horizontal / d;
            dir * (v * cos_theta) + up * (v * cos_theta * tan_theta)
        } else {
            // Out of ballistic reach — 45° max-range lob.
            let v45 = (g * d * d).max(v * v * 0.5).sqrt();
            (horizontal / d.max(1.0)) * (v45 * std::f32::consts::FRAC_1_SQRT_2)
                + up * (v45 * std::f32::consts::FRAC_1_SQRT_2)
        };
        Flight::Ballistic {
            velocity,
            gravity: g,
        }
    } else {
        Flight::Direct
    };

    // Prefer the authored S3O model (octashot, sigterm, etc.). The s3o
    // meshes are authored at their real upstream size in elmos, so the
    // `proj_size` scale we apply to the fallback sphere/cube (derived
    // from the weapon's `size=`) is inappropriate here — leave real
    // models at 1.0× and the model's own geometry dictates on-screen
    // size. Unknown / missing models drop to a small sphere sized by
    // the weapon's authored `size=`, which matches upstream Spring's
    // sprite-projectile fallback for Cannon weapons.
    let model_name = weapon.model.trim().trim_end_matches(';');
    let (mesh, visual_scale) = if !model_name.is_empty() {
        if let Some(handle) = load_s3o_mesh(model_name, meshes, model_cache) {
            (handle, 1.0)
        } else {
            (
                fx_meshes.unit_sphere(meshes),
                (weapon.size * 0.4).clamp(1.5, 6.0),
            )
        }
    } else {
        (
            fx_meshes.unit_sphere(meshes),
            (weapon.size * 0.4).clamp(1.5, 6.0),
        )
    };

    // Route through the shared cache so projectiles that re-use the same
    // color + intensity share a single StandardMaterial handle instead of
    // minting a fresh one per shot.
    let material = cache.get_or_create_tiled(color, false, weapon.intensity, None, 0, materials);

    // Upstream weapons with `smoketrail=1` leave a smoke-trail ribbon
    // textured with the weapon's `texture2` (`pointertrail` /
    // `flowtrail` / `firetrail`).
    let trail = if weapon.smoke_trail {
        Some(build_projectile_trail(
            weapon,
            materials,
            images,
            model_cache,
            cache,
        ))
    } else {
        None
    };
    // The authored per-frame `cegTag` CEG, emitted by `tick_weapon_fx`.
    let trail_ceg = (!weapon.ceg_tag.is_empty()).then_some(event.weapon_id);

    // Initial velocity for integrated flights (the tick takes over).
    let (velocity, speed) = match flight {
        Flight::Missile(m) => (m.dir * m.speed * GAME_SPEED, m.speed * GAME_SPEED),
        Flight::Starburst(s) => (s.dir * s.speed * GAME_SPEED, s.speed * GAME_SPEED),
        Flight::Ballistic { velocity, .. } => (velocity, velocity.length()),
        Flight::Direct => (Vec3::ZERO, speed),
    };
    let rotation = flight
        .guided_dir()
        .map_or(Quat::IDENTITY, super::tick::projectile_orientation);

    commands
        .spawn((
            ProjectileVisual {
                origin: spawn_pos,
                target: event.target_pos,
                speed,
                progress: 0.0,
                arc_height,
                flight,
                velocity,
                elapsed: 0.0,
                trail,
                trail_ceg,
                trail_seed: 0x9e3779b9u32.wrapping_mul(
                    (event.attacker_pos.x * 131.0 + event.target_pos.z * 7.0).to_bits(),
                ) | 1,
                last_target_pos: None,
            },
            Mesh3d(mesh),
            MeshMaterial3d(material),
            Transform::from_translation(spawn_pos)
                .with_rotation(rotation)
                .with_scale(Vec3::splat(visual_scale)),
        ))
        .id()
}

/// Resolve `weapon.texture2` into the material a [`LaserBolt`]'s lead
/// and tail endcap quads are batched under. Returns `None` when the
/// texture is unset / not in the resolver, so callers fall back
/// cleanly to a body-only bolt.
fn bolt_cap_material(
    weapon: &spring_tdf::WeaponDef,
    edge_color: bevy::color::LinearRgba,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    cache: &mut BeamMaterialCache,
) -> Option<Handle<StandardMaterial>> {
    if weapon.texture2.is_empty() || weapon.texture2.eq_ignore_ascii_case("none") {
        return None;
    }
    let filename = CegRegistry::resolve_texture(&weapon.texture2)?;
    let (handle, _, _) = load_beam_texture(filename, model_cache, images)?;

    // Keyed by the texture so different texture2 weapons don't collide.
    Some(cache.get_or_create_tiled(
        edge_color,
        true,
        weapon.intensity,
        Some(handle),
        0,
        materials,
    ))
}

/// Smoke-trail state for a projectile (upstream `CSmokeTrailProjectile`,
/// one segment per sim frame). The samples start empty — the tick
/// system pushes one quad per consecutive sample pair into the batch
/// for `material` every frame. Colour follows upstream: grey
/// `smokeColor` (0.65) times the weapon's `texture2`, faded per vertex
/// through premultiplied vertex colours (the effects pass draws
/// `GL_ONE, GL_ONE_MINUS_SRC_ALPHA` with colour and alpha both scaled
/// by the fade). The weapon's `rgbColor` plays no part.
fn build_projectile_trail(
    weapon: &spring_tdf::WeaponDef,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    cache: &mut BeamMaterialCache,
) -> ProjectileTrail {
    let texture = if !weapon.texture2.is_empty() && weapon.texture2 != "none" {
        CegRegistry::resolve_texture(&weapon.texture2)
            .and_then(|filename| load_beam_texture(filename, model_cache, images))
            .map(|(handle, _, _)| handle)
    } else {
        None
    };
    ProjectileTrail {
        material: cache.get_or_create_trail(texture, materials),
        samples: std::collections::VecDeque::with_capacity(TRAIL_SAMPLE_COUNT),
    }
}

/// Melee flash (Wormbite): a short-lived orange `ImpactBurst` at the
/// midpoint. Reuses the impact-burst component so `tick_weapon_fx`
/// handles fade/despawn uniformly; no beam geometry involved.
fn spawn_melee_flash(
    event: &AttackEvent,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut BeamMaterialCache,
    _fx_meshes: &mut WeaponFxMeshes,
    impact_assets: &mut ImpactBurstAssets,
) {
    let flash_pos = (event.attacker_pos + event.target_pos) / 2.0;
    spawn_impact_burst(
        flash_pos,
        [1.0, 0.3, 0.0],
        16.0,
        commands,
        meshes,
        materials,
        cache,
        impact_assets,
    );
}

/// GaussCannon lightning bolt, mirroring upstream
/// `LuaRules/Gadgets/network_arceffect.lua::BuildArc`: straight
/// muzzle→impact line, a 160-elmo arch at the midpoint
/// (`+160 * (1 − (2t−1)²)`), ±15-elmo jitter shrinking toward the
/// ends, electric-green tint with randomized red/blue channels.
/// The gadget's arc lives a single sim frame; we hold it for
/// [`ARC_LIFETIME`] so a 4-second-reload shot still reads on screen.
fn spawn_lightning_arc(
    event: &AttackEvent,
    rng: &mut Local<u32>,
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut BeamMaterialCache,
    flavor: ArcFlavor,
) {
    let (width, jitter, lifetime, tint) = match flavor {
        ArcFlavor::BigArc => (
            ARC_WIDTH,
            ARC_JITTER,
            ARC_LIFETIME,
            LinearRgba::rgb(0.2 + 0.1 * next_f32(rng), 0.9, 0.2 + 0.1 * next_f32(rng)),
        ),
        // gateway.bos BuildArc: `gl.Color(1,1,1)`, drawArc called with
        // width 2 / spray 5, arc lives 16 frames.
        ArcFlavor::BuildArc => (2.0, 5.0, frames_to_secs(16.0), LinearRgba::WHITE),
    };
    let start = event.attacker_pos;
    let end = event.target_pos;
    let dir = (end - start).normalize_or(Vec3::Z);
    // Two world-space perpendiculars to jitter along; the ribbon
    // itself is re-oriented to the camera every tick.
    let perp1 = dir.cross(Vec3::Y).try_normalize().unwrap_or(Vec3::X);
    let perp2 = dir.cross(perp1).try_normalize().unwrap_or(Vec3::Y);

    let mut points = Vec::with_capacity(ARC_SEGMENTS + 1);
    for i in 0..=ARC_SEGMENTS {
        let t = i as f32 / ARC_SEGMENTS as f32;
        let arch = 1.0 - (2.0 * t - 1.0).powi(2);
        // Same parabola for the jitter envelope — the gadget shakes
        // the strand most at mid-arc and pins both ends.
        let taper = arch;
        points.push(
            start.lerp(end, t)
                + Vec3::Y * (ARC_ARCH_HEIGHT * arch)
                + perp1 * (next_signed(rng) * jitter * taper)
                + perp2 * (next_signed(rng) * jitter * taper),
        );
    }

    let material = cache.get_or_create(LinearRgba::WHITE, true, materials);
    commands.spawn(LightningArc {
        points,
        width,
        lifetime,
        max_lifetime: lifetime,
        material,
        tint,
    });
}

/// The two arc flavors of upstream `network_arceffect.lua`: the Gauss
/// shot bolt (`BigArc`) and the Gateway's construction strand
/// (`BuildArc`).
enum ArcFlavor {
    BigArc,
    BuildArc,
}

/// Arc geometry constants — see [`spawn_lightning_arc`]. Segment count
/// 16 matches the upstream gadget's strip resolution.
const ARC_SEGMENTS: usize = 16;
/// Ribbon half-width in elmos (upstream GL line width 4).
const ARC_WIDTH: f32 = 3.0;
/// How long the bolt stays visible. Upstream redraws for exactly one
/// sim frame; with the port's 4 s reload a single frame would be a
/// subliminal flicker, so hold it a touch longer.
const ARC_LIFETIME: f32 = 0.12;
/// Midpoint arch height, from `BuildArc`'s `+160 * (1 − (2i−1)²)`.
const ARC_ARCH_HEIGHT: f32 = 160.0;
/// Maximum end-to-end jitter, from `BuildArc`'s ±15 spray.
const ARC_JITTER: f32 = 15.0;

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::MinimalPlugins;
    use bevy::asset::AssetPlugin;
    use bevy::ecs::system::RunSystemOnce;

    /// Headless harness with every resource `spawn_weapon_visuals`
    /// touches. MinimalPlugins + AssetPlugin provide the `AssetServer`
    /// the build-sparkle path borrows; no actual assets load here.
    fn fx_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_resource::<PendingAttacks>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<Assets<Image>>()
            .init_resource::<CegRenderAssets>()
            .init_resource::<S3OModelCache>()
            .init_resource::<BeamMaterialCache>()
            .init_resource::<ImpactBurstAssets>()
            .init_resource::<GroundFlashAssets>()
            .init_resource::<WeaponFxMeshes>()
            .insert_resource(CegRegistry::load());
        app
    }

    fn beam_def() -> spring_tdf::WeaponDef {
        spring_tdf::WeaponDef {
            weapon_type: "BeamLaser".into(),
            rgb_color: [0.2, 1.0, 0.2],
            ..Default::default()
        }
    }

    /// The Connection's GaussCannon must render as a lightning arc —
    /// no engine beam (its TDF `intensity=0` is invisible by design),
    /// no impact burst (`explosiongenerator=custom:none`), no
    /// synthesized muzzle flash.
    #[test]
    fn gauss_cannon_spawns_arc_instead_of_beam() {
        let mut app = fx_app();
        let mut weapons = WeaponRegistry::default();
        let gauss = weapons.insert_for_test("GaussCannon", beam_def());
        app.insert_resource(weapons);

        app.world_mut()
            .resource_mut::<PendingAttacks>()
            .events
            .push(AttackEvent {
                attacker_pos: Vec3::ZERO,
                target_pos: Vec3::new(200.0, 0.0, 0.0),
                weapon_id: gauss,
                muzzle_ceg: None,
                delayed_hit: None,
                build_arc: false,
            });

        app.world_mut()
            .run_system_once(spawn_weapon_visuals)
            .unwrap();

        let world = app.world_mut();
        let arcs = world
            .query_filtered::<&LightningArc, ()>()
            .iter(world)
            .count();
        let beams = world
            .query_filtered::<&BeamVisual, ()>()
            .iter(world)
            .count();
        let impacts = world
            .query_filtered::<&ImpactBurst, ()>()
            .iter(world)
            .count();
        assert_eq!(arcs, 1, "gauss shot must spawn exactly one arc");
        assert_eq!(beams, 0, "gauss beam is invisible upstream (intensity=0)");
        assert_eq!(impacts, 0, "gauss impact CEG is custom:none upstream");
    }

    /// Upstream worm.bos detonates the Wormsplash weapon at the bite
    /// points, so the melee hit shows the cyan `corruption_worm_splash`
    /// shockwave — not the generic orange flash, and no burst/beam.
    #[test]
    fn worm_bite_replays_the_splash_ceg() {
        let mut app = fx_app();
        let mut weapons = WeaponRegistry::default();
        let bite = weapons.insert_for_test(
            "Wormbite",
            spring_tdf::WeaponDef {
                weapon_type: "Melee".into(),
                ..Default::default()
            },
        );
        app.insert_resource(weapons);

        app.world_mut()
            .resource_mut::<PendingAttacks>()
            .events
            .push(AttackEvent {
                attacker_pos: Vec3::ZERO,
                target_pos: Vec3::new(60.0, 0.0, 0.0),
                weapon_id: bite,
                muzzle_ceg: None,
                delayed_hit: None,
                build_arc: false,
            });

        app.world_mut()
            .run_system_once(spawn_weapon_visuals)
            .unwrap();

        let world = app.world_mut();
        let flashes = world
            .query_filtered::<&ImpactBurst, ()>()
            .iter(world)
            .count();
        let flames = world
            .query_filtered::<&super::super::ceg::CegFlame, ()>()
            .iter(world)
            .count();
        assert_eq!(
            flashes, 0,
            "the bite must not fall back to the generic melee flash"
        );
        assert!(
            flames > 0,
            "the splash CEG (CBitmapMuzzleFlame class) must spawn"
        );
    }

    /// The Gateway's factory build ray renders as the white BuildArc
    /// (narrower, calmer, no tint randomness) and suppresses the
    /// standard build sparkle — upstream gateway.bos replaces the
    /// ordinary build-laser fx with lua_BuildArc.
    #[test]
    fn gateway_build_ray_spawns_white_build_arc() {
        let mut app = fx_app();
        let weapons = WeaponRegistry::default();
        let build_laser = weapons.intern("BuildLaser").unwrap();
        app.insert_resource(weapons);

        app.world_mut()
            .resource_mut::<PendingAttacks>()
            .events
            .push(AttackEvent {
                attacker_pos: Vec3::ZERO,
                target_pos: Vec3::new(80.0, 0.0, 0.0),
                weapon_id: build_laser,
                muzzle_ceg: None,
                delayed_hit: None,
                build_arc: true,
            });

        app.world_mut()
            .run_system_once(spawn_weapon_visuals)
            .unwrap();

        let world = app.world_mut();
        let arcs: Vec<&LightningArc> = world
            .query_filtered::<&LightningArc, ()>()
            .iter(world)
            .collect();
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].width, 2.0, "BuildArc draws at width 2");
        assert_eq!(arcs[0].tint, LinearRgba::WHITE);
        assert!((arcs[0].max_lifetime - 16.0 / 30.0).abs() < 1e-4);

        let sparkles = world
            .query_filtered::<&super::super::ceg::CegParticle, ()>()
            .iter(world)
            .count();
        assert_eq!(
            sparkles, 0,
            "gateway arcs replace the standard build sparkle"
        );
    }

    /// Ordinary beam weapons keep their regular visuals — the arc path
    /// must not swallow them.
    #[test]
    fn regular_beam_laser_still_spawns_beam() {
        let mut app = fx_app();
        let mut weapons = WeaponRegistry::default();
        let id = weapons.insert_for_test("PacketBeam", beam_def());
        app.insert_resource(weapons);

        app.world_mut()
            .resource_mut::<PendingAttacks>()
            .events
            .push(AttackEvent {
                attacker_pos: Vec3::ZERO,
                target_pos: Vec3::new(100.0, 0.0, 0.0),
                weapon_id: id,
                muzzle_ceg: None,
                delayed_hit: None,
                build_arc: false,
            });

        app.world_mut()
            .run_system_once(spawn_weapon_visuals)
            .unwrap();

        let world = app.world_mut();
        let arcs = world
            .query_filtered::<&LightningArc, ()>()
            .iter(world)
            .count();
        let beams = world
            .query_filtered::<&BeamVisual, ()>()
            .iter(world)
            .count();
        assert_eq!(arcs, 0);
        assert!(beams > 0, "BeamLaser weapons must keep their beams");
    }
}
