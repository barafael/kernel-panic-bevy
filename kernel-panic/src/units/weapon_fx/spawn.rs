//! Spawning side of weapon visuals: drains `PendingAttacks` and routes each
//! event to the appropriate effect spawner (beam, burst, projectile, melee,
//! plus the bonus nanoframe sparkle for build lasers).

use std::borrow::Cow;

use crate::sim::{DetFloat, sq};
use bevy::prelude::*;

use crate::terrain::heightmap::Heightmap;

use crate::units::content::ceg_def::resolve_texture;
use crate::units::content::cegs::CegKind;
use crate::units::content::weapons::WeaponKind;

use super::ceg::{CegRenderAssets, ground_flash_normal, spawn_ceg};
use super::shared::{
    AttackEvent, BeamMaterialCache, BeamVisual, BoltCegTrail, DelayedHit, Flight, GroundFlash,
    GroundFlashAssets, ImpactBurst, ImpactBurstAssets, LaserBolt, LightningArc, PendingAttacks,
    PendingExplosions, ProjectileTrail, ProjectileVisual, TRAIL_SAMPLE_COUNT, WeaponFxMeshes,
    tdf_color, weapon_core_color, weapon_edge_color,
};
use crate::rng::{next_f32, next_signed};
use crate::sim::{GAME_SPEED, frames_to_secs};
use crate::sim::{RngStream, SimRng};
use crate::units::assets::meshes::{S3OModelCache, load_beam_texture, load_s3o_mesh};
use crate::units::components::TeamId;
use crate::units::content::weapons::WeaponRegistry;

/// Whether firing `weapon` drops a `BuildSparkle` at the target end.
/// Only the bare `BuildLaser` does: `BuildLaserNoEffect` is the same
/// weapon with its impact particles suppressed on purpose, so it sparks
/// in neither place. [`WeaponKind::is_build_laser`] is the wider
/// question — both variants — that the weapon filter and the targeting
/// paths ask, which is why this one has its own name.
fn spawns_build_sparkle(weapon: WeaponKind) -> bool {
    weapon == WeaponKind::BuildLaser
}

/// [`spawn_weapon_visuals`]'s random stream (CEG rolls, arc jitter).
/// Visual only, but match-scoped and seeded like the sim's streams, so
/// a replay draws the same effects.
pub struct WeaponVisualRng;
impl RngStream for WeaponVisualRng {
    const SALT: u64 = 0xF1_A5E5;
}

/// [`spawn_pending_explosions`]'s random stream.
pub struct ExplosionVisualRng;
impl RngStream for ExplosionVisualRng {
    const SALT: u64 = 0xB1_A575;
}

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_weapon_visuals(
    mut pending: ResMut<PendingAttacks>,
    weapon_registry: Res<WeaponRegistry>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<S3OModelCache>,
    mut cache: ResMut<BeamMaterialCache>,
    (mut impact_assets, mut flash_assets): (ResMut<ImpactBurstAssets>, ResMut<GroundFlashAssets>),
    mut fx_meshes: ResMut<WeaponFxMeshes>,
    mut ceg_assets: ResMut<CegRenderAssets>,
    teams: Query<&TeamId>,
    gravity: Option<Res<crate::sim::MapGravity>>,
    mut rng: ResMut<SimRng<WeaponVisualRng>>,
    heightmap: Option<Res<Heightmap>>,
) {
    // The one read of the map's gravity for every shell spawned here.
    let map_gravity = gravity.as_deref().copied().unwrap_or_default().0;
    // gateway.bos `lua_BuildArc`: the builder's white strand.
    for arc in pending.build_arcs.drain(..) {
        spawn_lightning_arc(
            arc.from,
            arc.to,
            ArcStyle::build_arc(),
            &mut rng,
            &mut commands,
            &mut materials,
            &mut cache,
        );
    }
    for event in pending.events.drain(..) {
        // The combat side resolved this through the same registry before
        // pushing, so the lookup is infallible.
        let weapon = weapon_registry.by_kind(event.weapon);

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
        let is_gauss_arc = weapon_registry.is_gauss_cannon(event.weapon);

        if is_gauss_arc {
            let tint = LinearRgba::rgb(
                0.2 + 0.1 * next_f32(&mut rng),
                0.9,
                0.2 + 0.1 * next_f32(&mut rng),
            );
            spawn_lightning_arc(
                event.arc_from.unwrap_or(event.attacker_pos),
                event.target_pos,
                ArcStyle::big_arc(tint),
                &mut rng,
                &mut commands,
                &mut materials,
                &mut cache,
            );
        } else if is_melee {
            // Upstream worm.bos `FireWeapon1` telescopes the head and
            // detonates the Wormsplash weapon at the bite points
            // (`emit-sfx 4097 from head/end`) — the bite itself has
            // `explosiongenerator=custom:none` and no projectile, so
            // the cyan shockwave ring IS the visible attack. Replay
            // that CEG at the impact point; the generic melee flash is
            // for every other melee weapon.
            let bite_dir = dir / length.det_max(1e-6);
            if weapon_registry.is_worm_bite(event.weapon) {
                // The CEG is `[Wormsplash]`'s, so its `d` is that weapon's
                // damage (800), not the bite's (3200). `get`, not
                // `by_kind`: a test registry may hold the bite alone.
                let splash_damage = weapon_registry
                    .get(WeaponKind::Wormsplash)
                    .map_or(0.0, |splash| splash.damage.default);
                spawn_ceg(
                    CegKind::CorruptionWormSplash,
                    event.target_pos,
                    bite_dir,
                    Vec3::Y,
                    splash_damage,
                    &mut rng,
                    &mut commands,
                    &mut materials,
                    &mut images,
                    &mut model_cache,
                    &mut ceg_assets,
                    None,
                );
            } else {
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
                map_gravity,
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
            // The shooter is still alive here (shots are queued and
            // drained within one tick, before `cleanup_dying`), so its
            // team rides along for the rest of the flight.
            commands.entity(visual).insert(DelayedHit {
                target: delayed.target,
                attacker: delayed.attacker,
                owner_team: teams.get(delayed.attacker).ok().map(|t| t.0),
                weapon: event.weapon,
                attacker_distance: delayed.attacker_distance,
                on_impact: delayed.on_impact,
            });
        }

        // Muzzle flash: the CEG the unit's `FireWeapon1` emits from its
        // muzzle (`emit-sfx 1024+i`), resolved by combat into
        // `event.muzzle_ceg` — Bit's `oldskool_shot2`, Byte/Pointer's
        // soft-blue `oldskool_shot1`. Upstream has no automatic muzzle
        // flash, so a unit whose script emits nothing (Flow, Packet, …)
        // gets nothing: `muzzle_ceg` stays `None`, and every name an FBI
        // does emit has a def behind it
        // (`cegs::tests::every_shipped_ceg_reference_resolves`).
        if !is_melee
            && !spawns_build_sparkle(event.weapon)
            && !is_gauss_arc
            && let Some(muzzle_ceg) = event.muzzle_ceg
        {
            let muzzle_dir = (event.target_pos - event.attacker_pos).normalize_or(Vec3::Y);
            // A script's `emit-sfx` hands its CEG the unit's `cegDamage`
            // (`CUnitScript::EmitSfx`), 0 unless a script sets it, as
            // `dispatch_emit_sfx` already passes — not the weapon's.
            spawn_ceg(
                muzzle_ceg,
                event.attacker_pos,
                muzzle_dir,
                Vec3::Y,
                0.0,
                &mut rng,
                &mut commands,
                &mut materials,
                &mut images,
                &mut model_cache,
                &mut ceg_assets,
                None,
            );
        }

        // Build lasers also drop a short-lived "nanoframe pixel" sprite at
        // the target end: the authored `oldskool_build` CEG, replayed
        // through the runtime like any other explosion generator (the
        // NoEffect variant intentionally skips this). Before the CEG
        // runner this was hand-rolled here — with drifted speed/spread
        // and hash "jitter" — which is exactly the divergence the
        // runtime exists to prevent.
        if spawns_build_sparkle(event.weapon) {
            spawn_ceg(
                CegKind::OldskoolBuild,
                event.target_pos,
                Vec3::Y,
                Vec3::Y,
                weapon.damage.default,
                &mut rng,
                &mut commands,
                &mut materials,
                &mut images,
                &mut model_cache,
                &mut ceg_assets,
                None,
            );
        } else if !is_melee && !is_gauss_arc && event.delayed_hit.is_none() {
            // Upstream CEG is the source of truth for impact particles:
            // the weapon's `explosiongenerator=custom:NAME` names a
            // section of `gamedata/explosions/` — a `static` in
            // `units::content::ceg_defs` — and its colormap, size growth,
            // directional spread and lifetime all come from that script.
            let dir = (event.target_pos - event.attacker_pos).normalize_or(Vec3::Y);
            match weapon_registry.ceg(event.weapon) {
                // `custom:none` lands here too, and spawns nothing on
                // purpose: that is what the data asked for.
                Some(ceg) => spawn_ceg(
                    ceg,
                    event.target_pos,
                    dir,
                    Vec3::Y,
                    weapon.damage.default,
                    &mut rng,
                    &mut commands,
                    &mut materials,
                    &mut images,
                    &mut model_cache,
                    &mut ceg_assets,
                    None,
                ),
                // No `explosiongenerator` at all: synthesise the burst.
                None => spawn_generic_blast(
                    event.target_pos,
                    weapon.rgb_color,
                    weapon.area_of_effect,
                    &mut commands,
                    &mut meshes,
                    &mut materials,
                    &mut cache,
                    &mut impact_assets,
                    &mut flash_assets,
                    heightmap.as_deref(),
                ),
            }
        }
    }
}

/// Drain every [`PendingExplosions`] entry and spawn a matching
/// burst + ground flash. No beam, no muzzle flash, no projectile — this
/// path is for standalone detonations (unit-death `ExplodeAs`, kamikaze
/// triggers, command-fire area blasts). Both fall back to the same
/// [`spawn_generic_blast`], so a Logic Bomb's death boom reads the same
/// visual language as its in-flight hit.
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
    mut rng: ResMut<SimRng<ExplosionVisualRng>>,
    heightmap: Option<Res<Heightmap>>,
) {
    for event in pending.events.drain(..) {
        // Drive standalone explosions through the same CEG path: if the
        // caller named one (death `ExplodeAs`, mine kill, SIGTERM),
        // replay its emitters; otherwise fall back to the synthesised
        // burst so there's still feedback for unscripted detonations.
        match event.ceg {
            Some(ceg) => spawn_ceg(
                ceg,
                event.pos,
                event.up,
                event.up,
                event.damage,
                &mut rng,
                &mut commands,
                &mut materials,
                &mut images,
                &mut model_cache,
                &mut ceg_assets,
                heightmap.as_deref(),
            ),
            None => spawn_generic_blast(
                event.pos,
                event.rgb,
                event.radius,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut cache,
                &mut impact_assets,
                &mut flash_assets,
                heightmap.as_deref(),
            ),
        }
    }
}

/// The burst a detonation with no `explosiongenerator` behind it gets:
/// an owner-coloured impact burst and a ground ring, both `radius`
/// wide with a 4-elmo floor so a point blast still reads. The one look
/// for both the per-weapon impact and the standalone explosion, which
/// is what lets a Logic Bomb's death boom read like its in-flight hit.
#[allow(clippy::too_many_arguments)]
fn spawn_generic_blast(
    pos: Vec3,
    rgb: [f32; 3],
    radius: f32,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut BeamMaterialCache,
    impact_assets: &mut ImpactBurstAssets,
    flash_assets: &mut GroundFlashAssets,
    ground: Option<&Heightmap>,
) {
    let aoe = radius.det_max(4.0);
    spawn_impact_burst(
        pos,
        rgb,
        aoe,
        commands,
        meshes,
        materials,
        cache,
        impact_assets,
    );
    spawn_ground_flash(
        pos,
        rgb,
        aoe,
        commands,
        meshes,
        materials,
        cache,
        flash_assets,
        ground,
    );
}

/// Emissive ring on the ground under `pos`, in the terrain's plane.
/// Radius animates from 0.25× to 1.5× `aoe` over the lifetime so the
/// ring visibly "blooms" outward from the impact point before fading.
/// Mesh is a shared unit-circle; the spawn-time scale carries the
/// initial radius so the tick system only needs to grow `base_radius`
/// — no material mutation per frame.
///
/// A blast within its ring's reach of the ground puts the ring on the
/// terrain, tilted like the ceg ground flash; one higher up (Flow,
/// command-fire shells bursting mid-air) or with no `ground` keeps a
/// flat ring at the blast. Either way it sits half an elmo off the
/// surface so it doesn't z-fight.
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
    ground: Option<&Heightmap>,
) {
    let mesh = flash_assets
        .mesh
        .get_or_insert_with(|| meshes.add(Circle::new(1.0)))
        .clone();

    let color = tdf_color(rgb);
    let material = cache.get_or_create_textured(color, true, 2.0, None, materials);

    // Clamp so even mines / SIGTERM don't swallow the screen, but keep
    // beam pings (AoE=8) readable. Small ring radius feels snappier than
    // the full blast sphere.
    let base_radius = (aoe * 0.5).det_clamp(4.0, 80.0);
    let life = 0.45;
    let reach = base_radius * 1.5;

    let (centre, normal) = match ground {
        Some(h) if pos.y - h.sample(pos.x, pos.z) <= reach => {
            (h.place(pos.x, pos.z), ground_flash_normal(h, pos, reach))
        }
        _ => (pos, Vec3::Y),
    };
    // `Circle` lies in XY, facing +Z: turn that face to the ground's.
    let tilt = Quat::from_rotation_arc(Vec3::Z, normal);

    commands.spawn((
        GroundFlash {
            lifetime: life,
            max_lifetime: life,
            base_radius,
        },
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(centre + normal * 0.5)
            .with_rotation(tilt)
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
    let base_size = (aoe * 0.25).det_clamp(3.0, 24.0);
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
/// the alias itself. We consult [`resolve_texture`], the alias table,
/// first and fall back to a literal `{name}.tga` lookup for weapon
/// textures that don't happen to be aliased. Only that fallback builds a
/// filename, and [`tga_filename`] spells it into a stack buffer — both
/// paths run allocation-free, which matters because this is called for
/// every shot.
fn beam_texture(
    tex1: &str,
    model_cache: &mut S3OModelCache,
    images: &mut Assets<Image>,
) -> Option<(Handle<Image>, f32)> {
    if tex1.is_empty() || tex1.eq_ignore_ascii_case("none") {
        return None;
    }
    let (handle, w, h) = match resolve_texture(tex1) {
        Some(texture) => load_beam_texture(texture.file(), model_cache, images)?,
        None => {
            let mut buf = [0u8; 96];
            load_beam_texture(&tga_filename(tex1, &mut buf), model_cache, images)?
        }
    };
    let aspect = if h > 0 { w as f32 / h as f32 } else { 1.0 };
    Some((handle, aspect))
}

/// `{name}.tga` written into `buf`, falling back to a `String` only for a
/// name too long for it. The un-aliased beam path needs a filename per
/// shot, so the common case stays on the stack.
fn tga_filename<'a>(name: &str, buf: &'a mut [u8]) -> Cow<'a, str> {
    const SUFFIX: &[u8] = b".tga";
    let end = name.len() + SUFFIX.len();
    if end <= buf.len() {
        buf[..name.len()].copy_from_slice(name.as_bytes());
        buf[name.len()..end].copy_from_slice(SUFFIX);
        // `name` is valid UTF-8 and the suffix is ASCII, so the splice
        // at `name.len()` cannot split a code point.
        if let Ok(spelled) = std::str::from_utf8(&buf[..end]) {
            return Cow::Borrowed(spelled);
        }
    }
    Cow::Owned(format!("{name}.tga"))
}

/// Decode and upload every texture a shot can draw — the beam textures
/// ([`beam_texture_names`]) of each of `fired` drawn as a beam or bolt,
/// the BeamLaser `flare` and the CEG atlas — so the first shot of a kind
/// doesn't do it mid-fight (`rendering::prewarm`). Each is cached for
/// the app's life.
pub(crate) fn warm_shot_textures(
    weapons: &WeaponRegistry,
    fired: impl IntoIterator<Item = WeaponKind>,
    model_cache: &mut S3OModelCache,
    images: &mut Assets<Image>,
) {
    use crate::units::content::ceg_def::CegTexture;
    use spring_tdf::WeaponCategory::{BeamLaser, LaserCannon};
    use strum::VariantArray;
    for kind in fired {
        // The shot's own dispatch: the Gauss arc, melee and projectiles
        // draw no beam texture.
        if let Some(def) = weapons.get(kind)
            && matches!(def.category(), BeamLaser | LaserCannon)
            && !weapons.is_gauss_cannon(kind)
        {
            let (tex1, tex2) = beam_texture_names(def);
            beam_texture(tex1, model_cache, images);
            beam_texture(tex2, model_cache, images);
        }
    }
    beam_texture("flare", model_cache, images);
    // `bullet` is not in the bundle (`CegTexture::Bullet`).
    for texture in CegTexture::VARIANTS
        .iter()
        .filter(|t| **t != CegTexture::Bullet)
    {
        load_beam_texture(texture.file(), model_cache, images);
    }
}

/// The beam atlas a weapon draws with: its authored `texture1`/`texture2`,
/// or the engine's defaults for the category when the TDF leaves them
/// empty (`ProjectileDrawer::LoadWeaponTextures`: `laserfalloff` body and
/// `laserend` caps for every LaserCannon and BeamLaser). `none` stays
/// empty. Every result is either empty, a `&'static` default, or a
/// trimmed slice of the def — borrowed, not allocated: this runs per
/// beam/laser shot.
fn beam_texture_names(weapon: &spring_tdf::WeaponDef) -> (&str, &str) {
    fn defaulted<'a>(authored: &'a str, default: &'static str) -> &'a str {
        let t = authored.trim();
        if t.eq_ignore_ascii_case("none") {
            ""
        } else if t.is_empty() {
            default
        } else {
            t
        }
    }
    let beam_type = matches!(
        weapon.category(),
        spring_tdf::WeaponCategory::LaserCannon | spring_tdf::WeaponCategory::BeamLaser
    );
    if beam_type {
        (
            defaulted(&weapon.texture1, "laserfalloff"),
            defaulted(&weapon.texture2, "laserend"),
        )
    } else {
        (
            defaulted(&weapon.texture1, ""),
            defaulted(&weapon.texture2, ""),
        )
    }
}

/// `laserFlareSize` with the engine default of 15: the BeamLaser's
/// emitter flare is `thickness · laserFlareSize` elmos half-size.
fn laser_flare_size(weapon: &spring_tdf::WeaponDef) -> f32 {
    if weapon.laser_flare_size > 0.0 {
        weapon.laser_flare_size
    } else {
        15.0
    }
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
    let thickness = weapon.thickness.det_max(1.0);

    // BeamLaser lifetime comes from `beamtime`/`beamttl`. Color decay is
    // a separate per-frame multiplier on the vertex colors (see
    // `BeamVisual.decay`); it doesn't extend the lifetime, contrary to
    // an earlier port-ism. 0.08 s is a pragmatic floor so single-frame
    // shots still visibly flash.
    let lifetime = if is_beam_laser {
        let ttl_sec = frames_to_secs(weapon.beam_ttl);
        weapon.beam_time.det_max(ttl_sec).det_max(0.08)
    } else {
        weapon.duration.det_max(0.08)
    };

    // Tile the texture along beam length so `arrow`'s `>>>>` or
    // `dosray`'s `01010101` stream reads as a sequence of glyphs
    // rather than one stretched smear. 56 elmos/tile matches the
    // on-screen glyph size in upstream footage at default zoom.
    const ARROW_TILE_LENGTH: f32 = 56.0;
    let (tex1, tex2) = beam_texture_names(weapon);
    let texture = beam_texture(tex1, model_cache, images);
    let has_texture = texture.is_some();
    // The engine's `laserfalloff` is a cross-beam gradient, constant
    // along the length: drawn once, not tiled like a glyph strip.
    let tiles = if has_texture && !tex1.eq_ignore_ascii_case("laserfalloff") {
        (length / ARROW_TILE_LENGTH).round().det_clamp(1.0, 24.0)
    } else {
        1.0
    };

    // Outer (edge) pass. The beam entity is pure bookkeeping: the tick
    // system pushes its camera-facing quad into the batch for
    // `material` every frame (`super::batch`).
    let edge_color = weapon_edge_color(weapon);
    let outer_mat = cache.get_or_create_textured(
        edge_color,
        true,
        weapon.intensity,
        texture.as_ref().map(|(handle, _)| handle.clone()),
        materials,
    );
    // End caps (`texture2`) and, for a BeamLaser, the emitter flare
    // (`texture3`, always the atlas `flare`): `BeamLaserProjectile::Draw`.
    let cap_material = |color: bevy::color::LinearRgba,
                        intensity: f32,
                        materials: &mut Assets<StandardMaterial>,
                        images: &mut Assets<Image>,
                        model_cache: &mut S3OModelCache,
                        cache: &mut BeamMaterialCache|
     -> Option<Handle<StandardMaterial>> {
        let (handle, _) = beam_texture(tex2, model_cache, images)?;
        Some(cache.get_or_create_textured(color, true, intensity, Some(handle), materials))
    };
    let flare_material = |color: bevy::color::LinearRgba,
                          intensity: f32,
                          materials: &mut Assets<StandardMaterial>,
                          images: &mut Assets<Image>,
                          model_cache: &mut S3OModelCache,
                          cache: &mut BeamMaterialCache|
     -> Option<(Handle<StandardMaterial>, f32)> {
        if !is_beam_laser {
            return None;
        }
        let (handle, _) = beam_texture("flare", model_cache, images)?;
        let half = thickness * laser_flare_size(weapon);
        Some((
            cache.get_or_create_textured(color, true, intensity, Some(handle), materials),
            half,
        ))
    };
    commands.spawn(BeamVisual {
        start: event.attacker_pos,
        end: event.target_pos,
        thickness,
        lifetime,
        max_lifetime: lifetime,
        material: outer_mat,
        tiles,
        decay: weapon.beam_decay,
        caps: cap_material(
            edge_color,
            weapon.intensity,
            materials,
            images,
            model_cache,
            cache,
        ),
        flare: flare_material(
            edge_color,
            weapon.intensity,
            materials,
            images,
            model_cache,
            cache,
        ),
    });

    // Core pass: `corethickness × rgbColor2 (white) × texture`. Always
    // drawn when authored > 0. For `corethickness=1` the core fully
    // covers the outer and baked-colour textures (arrow cyan,
    // bytemegabeam magenta) come through intact. Lower ratios let the
    // outer halo peek around for a two-tone look.
    let core_ratio = weapon.core_thickness.det_clamp(0.0, 1.0);
    if core_ratio > 0.01 {
        let core_thickness = thickness * core_ratio;
        let core_color = weapon_core_color(weapon);
        let core_mat = cache.get_or_create_textured(
            core_color,
            true,
            weapon.intensity.det_max(1.0),
            texture.map(|(handle, _)| handle),
            materials,
        );
        commands.spawn(BeamVisual {
            start: event.attacker_pos,
            end: event.target_pos,
            thickness: core_thickness,
            lifetime,
            max_lifetime: lifetime,
            material: core_mat,
            tiles,
            decay: weapon.beam_decay,
            caps: cap_material(
                core_color,
                weapon.intensity,
                materials,
                images,
                model_cache,
                cache,
            ),
            flare: flare_material(
                core_color,
                weapon.intensity,
                materials,
                images,
                model_cache,
                cache,
            )
            .map(|(m, half)| (m, half * core_ratio)),
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
    let distance = delta.length().det_max(0.1);
    let direction = delta / distance;

    // Trust the authored thickness — see spawn_textured_beam.
    let thickness = weapon.thickness.det_max(1.0);

    let speed = if weapon.weapon_velocity > 0.0 {
        weapon.weapon_velocity
    } else {
        256.0
    };
    let duration = weapon.duration.det_max(0.05);
    // `max_length = duration * speed` per upstream, with a floor of
    // `thickness * 3` so very short ticks still read as a bolt.
    let max_length = (speed * duration).det_max(thickness * 3.0);

    let (tex1, _) = beam_texture_names(weapon);
    let texture = beam_texture(tex1, model_cache, images);

    // The bolt entity is pure bookkeeping: the tick system computes
    // the camera-facing corners each frame and pushes them into the
    // batch for `material` — world-space positions, no transform.
    let edge_color = weapon_edge_color(weapon);
    let outer_mat = cache.get_or_create_textured(
        edge_color,
        true,
        weapon.intensity,
        texture.as_ref().map(|(handle, _)| handle.clone()),
        materials,
    );
    // Optional endcaps from `texture2`. Upstream's `LaserProjectile::Draw`
    // wraps two extra half-quads at the lead and tail using `texture2`
    // to round off the bolt — `Byte`'s `MegaBeam` is the only weapon in
    // the KP roster that authors this (`texture2=bytelaser`); everyone
    // else either omits texture2 or sets it to `none`. Skipped when the
    // lookup fails so a typo in the TDF doesn't crash the game.
    let cap_material = bolt_cap_material(weapon, edge_color, materials, images, model_cache, cache);
    let mut outer = commands.spawn(LaserBolt {
        origin: event.attacker_pos,
        direction,
        total_distance: distance,
        speed,
        max_length,
        thickness,
        elapsed: 0.0,
        material: outer_mat,
        cap_material,
    });
    // The authored per-frame `cegTag` CEG, emitted by `tick_weapon_fx`.
    if !weapon.ceg_tag.is_empty() {
        outer.insert(BoltCegTrail {
            weapon: event.weapon,
            seed: 0x9e3779b9u32
                .wrapping_mul((event.attacker_pos.x * 131.0 + event.target_pos.z * 7.0).to_bits())
                | 1,
        });
    }
    let outer_entity = outer.id();

    // Core pass: `corethickness × white × texture`. For Bit's
    // `Line` (corethickness=1) the core is full width and fully covers
    // the outer pass, leaving the arrow texture's baked cyan intact.
    // For Byte's MegaBeam (corethickness=0.5) the outer magenta halo
    // surrounds a hot-white core.
    let core_ratio = weapon.core_thickness.det_clamp(0.0, 1.0);
    if core_ratio > 0.01 {
        let core_thickness = thickness * core_ratio;
        let core_color = weapon_core_color(weapon);
        let core_mat = cache.get_or_create_textured(
            core_color,
            true,
            weapon.intensity.det_max(1.0),
            texture.map(|(handle, _)| handle),
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
/// - `Cannon` (`ballistic=1` — Exploit's BugCannon): `CCannon::FireImpl`
///   and `CExplosiveProjectile` — launched at `weaponvelocity` along the
///   low-arc ballistic solve (or the fire side's sprayed launch
///   direction) and falling under `myGravity` (else the map's gravity),
///   per sim frame.
/// - everything else (AircraftBomb, a `ballistic=1` LaserCannon):
///   direct parametric lerp as before.
#[allow(clippy::too_many_arguments)]
fn spawn_projectile(
    event: &AttackEvent,
    weapon: &spring_tdf::WeaponDef,
    map_gravity: f32,
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
    let up = Vec3::Y;
    let muzzle_dir = event.delayed_hit.as_ref().map_or_else(
        || (event.target_pos - event.attacker_pos).normalize_or(up),
        |d| d.muzzle_dir,
    );
    let launch = super::flight::Launch {
        weapon,
        muzzle_pos: event.attacker_pos,
        muzzle_dir,
        target_pos: event.target_pos,
        map_gravity,
    };
    let mut spawn_pos = event.attacker_pos;
    let flight = match weapon.category() {
        spring_tdf::WeaponCategory::StarburstLauncher => {
            let (f, pos) = super::flight::StarburstFlight::launch(launch);
            spawn_pos = pos;
            Flight::Starburst(f)
        }
        spring_tdf::WeaponCategory::MissileLauncher => {
            let (f, pos) = super::flight::MissileFlight::launch(launch);
            spawn_pos = pos;
            Flight::Missile(f)
        }
        spring_tdf::WeaponCategory::Cannon => Flight::Cannon(super::flight::CannonFlight::launch(
            weapon,
            event.attacker_pos,
            event.target_pos,
            event.delayed_hit.as_ref().and_then(|d| d.launch_dir),
            map_gravity,
        )),
        _ => Flight::Direct,
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
                (weapon.size * 0.4).det_clamp(1.5, 6.0),
            )
        }
    } else {
        (
            fx_meshes.unit_sphere(meshes),
            (weapon.size * 0.4).det_clamp(1.5, 6.0),
        )
    };

    // Route through the shared cache so projectiles that re-use the same
    // color + intensity share a single StandardMaterial handle instead of
    // minting a fresh one per shot.
    let material = cache.get_or_create_textured(color, false, weapon.intensity, None, materials);

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
    let trail_ceg = (!weapon.ceg_tag.is_empty()).then_some(event.weapon);

    // Initial velocity for integrated flights (the tick takes over).
    let (velocity, speed) = match flight {
        Flight::Missile(m) => (m.dir * m.speed * GAME_SPEED, m.speed * GAME_SPEED),
        Flight::Starburst(s) => (s.dir * s.speed * GAME_SPEED, s.speed * GAME_SPEED),
        Flight::Cannon(c) => (c.velocity * GAME_SPEED, c.velocity.length() * GAME_SPEED),
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
    let (_, tex2) = beam_texture_names(weapon);
    if tex2.is_empty() {
        return None;
    }
    let texture = resolve_texture(tex2)?;
    let (handle, _, _) = load_beam_texture(texture.file(), model_cache, images)?;

    // Keyed by the texture so different texture2 weapons don't collide.
    Some(cache.get_or_create_textured(edge_color, true, weapon.intensity, Some(handle), materials))
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
        resolve_texture(&weapon.texture2)
            .and_then(|tex| load_beam_texture(tex.file(), model_cache, images))
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

/// How one `network_arceffect.lua` strand is drawn: `drawArc`'s width
/// and spray, plus the gadget's colour and lifetime.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ArcStyle {
    width: f32,
    jitter: f32,
    lifetime: f32,
    tint: LinearRgba,
}

impl ArcStyle {
    /// `BigArc`, the Connection's GaussCannon bolt: `drawArc(.., 4, 15)`
    /// in green with randomized red / blue (`tint`). The gadget's arc
    /// lives a single sim frame; we hold it for [`ARC_LIFETIME`] so a
    /// 4-second-reload shot still reads on screen.
    fn big_arc(tint: LinearRgba) -> Self {
        Self {
            width: ARC_WIDTH,
            jitter: ARC_JITTER,
            lifetime: ARC_LIFETIME,
            tint,
        }
    }

    /// `BuildArc`, the Gateway's construction strand: `gl.Color(1,1,1)`,
    /// `drawArc(.., 2, 5)`, alive for 16 frames (`liveUntil = frame + 16`).
    fn build_arc() -> Self {
        Self {
            width: 2.0,
            jitter: 5.0,
            lifetime: frames_to_secs(16.0),
            tint: LinearRgba::WHITE,
        }
    }
}

/// One `network_arceffect.lua` lightning strand (`drawArc`): the
/// straight `start`→`end` line, a 160-elmo arch at the midpoint
/// (`+160 * (1 − (2t−1)²)`), and `style.jitter` of spray shrinking
/// toward both ends.
#[allow(clippy::too_many_arguments)]
fn spawn_lightning_arc(
    start: Vec3,
    end: Vec3,
    style: ArcStyle,
    rng: &mut u32,
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut BeamMaterialCache,
) {
    let dir = (end - start).normalize_or(Vec3::Z);
    // Two world-space perpendiculars to jitter along; the ribbon
    // itself is re-oriented to the camera every tick.
    let perp1 = dir.cross(Vec3::Y).try_normalize().unwrap_or(Vec3::X);
    let perp2 = dir.cross(perp1).try_normalize().unwrap_or(Vec3::Y);

    let mut points = Vec::with_capacity(ARC_SEGMENTS + 1);
    for i in 0..=ARC_SEGMENTS {
        let t = i as f32 / ARC_SEGMENTS as f32;
        let arch = 1.0 - sq(2.0 * t - 1.0);
        // Same parabola for the jitter envelope — the gadget shakes
        // the strand most at mid-arc and pins both ends.
        let taper = arch;
        points.push(
            start.lerp(end, t)
                + Vec3::Y * (ARC_ARCH_HEIGHT * arch)
                + perp1 * (next_signed(rng) * style.jitter * taper)
                + perp2 * (next_signed(rng) * style.jitter * taper),
        );
    }

    let material = cache.get_or_create(LinearRgba::WHITE, true, materials);
    commands.spawn(LightningArc {
        points,
        width: style.width,
        lifetime: style.lifetime,
        max_lifetime: style.lifetime,
        material,
        tint: style.tint,
    });
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
/// Midpoint arch height, from `drawArc`'s `+160 * (1 − (2i−1)²)`.
const ARC_ARCH_HEIGHT: f32 = 160.0;
/// Maximum end-to-end jitter, from `BigArc`'s ±15 spray.
const ARC_JITTER: f32 = 15.0;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::test_support::InitSim;

    /// Untextured beams draw with the engine atlas (`laserfalloff` body,
    /// `laserend` caps); `none` opts out; authored names win.
    #[test]
    fn beam_textures_default_to_the_engine_atlas() {
        let beam = spring_tdf::WeaponDef {
            weapon_type: "BeamLaser".into(),
            ..Default::default()
        };
        assert_eq!(beam_texture_names(&beam), ("laserfalloff", "laserend"));
        let authored = spring_tdf::WeaponDef {
            texture1: "dosray".into(),
            texture2: "none".into(),
            ..beam
        };
        assert_eq!(beam_texture_names(&authored), ("dosray", ""));
        let missile = spring_tdf::WeaponDef {
            weapon_type: "MissileLauncher".into(),
            ..Default::default()
        };
        assert_eq!(beam_texture_names(&missile), ("", ""));
    }

    /// The un-aliased beam path spells `{name}.tga` into the caller's
    /// buffer; only a name too long for it falls back to a `String`, and
    /// both have to agree on the spelling.
    #[test]
    fn tga_filename_spells_the_extension_in_place() {
        let mut buf = [0u8; 16];
        assert_eq!(tga_filename("dosray", &mut buf), "dosray.tga");
        // Exactly the buffer's size: still borrowed.
        assert_eq!(tga_filename("abcdefghijkl", &mut buf), "abcdefghijkl.tga");
        // One over: the `String` fallback, same result.
        assert_eq!(tga_filename("abcdefghijklm", &mut buf), "abcdefghijklm.tga");
        // Multi-byte code points survive the byte-level splice.
        assert_eq!(tga_filename("stråle", &mut buf), "stråle.tga");
    }

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
            .init_sim();
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
        let gauss = weapons.insert_for_test(WeaponKind::GaussCannon, beam_def());
        app.insert_resource(weapons);

        app.world_mut()
            .resource_mut::<PendingAttacks>()
            .events
            .push(AttackEvent {
                attacker_pos: Vec3::ZERO,
                target_pos: Vec3::new(200.0, 0.0, 0.0),
                weapon: gauss,
                muzzle_ceg: None,
                delayed_hit: None,
                arc_from: None,
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
            WeaponKind::Wormbite,
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
                weapon: bite,
                muzzle_ceg: None,
                delayed_hit: None,
                arc_from: None,
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

    /// gateway.bos `lua_BuildArc` → `network_arceffect.lua` `BuildArc`:
    /// a white `drawArc(.., 2, 5)` strand from the builder piece to the
    /// buildee, alive 16 frames, arched 160 elmos at the middle.
    #[test]
    fn build_arc_event_spawns_the_white_builder_strand() {
        use crate::units::weapon_fx::BuildArcEvent;
        let mut app = fx_app();
        app.insert_resource(WeaponRegistry::default());
        let (from, to) = (Vec3::new(0.0, 20.0, 0.0), Vec3::new(80.0, 0.0, 0.0));
        app.world_mut()
            .resource_mut::<PendingAttacks>()
            .build_arcs
            .push(BuildArcEvent { from, to });
        app.world_mut()
            .run_system_once(spawn_weapon_visuals)
            .unwrap();

        let world = app.world_mut();
        let arcs: Vec<&LightningArc> = world
            .query_filtered::<&LightningArc, ()>()
            .iter(world)
            .collect();
        assert_eq!(arcs.len(), 1);
        let arc = arcs[0];
        assert_eq!(arc.width, 2.0);
        assert_eq!(arc.tint, LinearRgba::WHITE);
        assert!((arc.max_lifetime - 16.0 / 30.0).abs() < 1e-4);
        assert_eq!((arc.points[0], arc.points[ARC_SEGMENTS]), (from, to));
        let mid = arc.points[ARC_SEGMENTS / 2];
        assert!((mid.y - (10.0 + ARC_ARCH_HEIGHT)).abs() <= 5.0 * 2.0_f32.sqrt());
        assert!(world.resource::<PendingAttacks>().build_arcs.is_empty());
    }

    /// Ordinary beam weapons keep their regular visuals — the arc path
    /// must not swallow them.
    #[test]
    fn regular_beam_laser_still_spawns_beam() {
        let mut app = fx_app();
        let mut weapons = WeaponRegistry::default();
        let id = weapons.insert_for_test(WeaponKind::PacketBeam, beam_def());
        app.insert_resource(weapons);

        app.world_mut()
            .resource_mut::<PendingAttacks>()
            .events
            .push(AttackEvent {
                attacker_pos: Vec3::ZERO,
                target_pos: Vec3::new(100.0, 0.0, 0.0),
                weapon: id,
                muzzle_ceg: None,
                delayed_hit: None,
                arc_from: None,
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
