//! Per-unit projectile shields.
//!
//! Secondary factories (Socket / Window / Port), Firewall, Terminal,
//! Obelisk, and the Kernel / Hole homebases all carry a `Shield`.
//! Damage resolved by `apply_damage` is intercepted by the shield
//! first; once the shield HP hits zero, leftover damage leaks through
//! to the unit's `Health`. Shields regenerate at `regen_per_sec` when
//! not being hit.
//!
//! Upstream's `shieldpower=0` encodes an infinite shield (homebase,
//! minifac) — we model that as `None` in `ShieldState::current_power`.
//!
//! **Upstream only activates these shields in ONS gamemode.**
//! `anydefs_post.lua` unconditionally deletes `homebaseshieldgood` /
//! `minifacshieldgood` from every FBI when the `ons` mod option is
//! off or absent — and the default non-ONS `kernel_panic.modoptions`
//! leaves it off. Without that gating, every homebase / minifac /
//! firewall / obelisk / terminal becomes permanently invincible
//! because they all ship with `shieldpower=0` (infinite) shields.
//! [`OnsMode`] is the runtime toggle; [`attach_shields`] is gated on
//! it and stays a no-op for the default sandbox game.

use bevy::prelude::*;

use crate::units::assets::meshes::{S3OModelCache, load_beam_texture};
use crate::units::components::UnitType;
use crate::units::content::definitions::UnitKind;
use crate::units::content::weapons::WeaponRegistry;

/// A unit's shield HP pool. `current_power = None` means infinite
/// (upstream `shieldpower=0`); otherwise the shield has a finite pool
/// and regenerates at `regen_per_sec`.
#[derive(Component, Debug, Clone)]
pub struct ShieldState {
    pub max_power: Option<f32>,
    pub current_power: Option<f32>,
    pub regen_per_sec: f32,
}

impl ShieldState {
    /// Absorb `amount` of damage, returning what (if any) leaked
    /// through. Infinite shields absorb everything.
    pub fn absorb(&mut self, amount: f32) -> f32 {
        match (self.max_power, self.current_power.as_mut()) {
            (None, _) => 0.0,
            (Some(_), Some(current)) => {
                if *current >= amount {
                    *current -= amount;
                    0.0
                } else {
                    let leak = amount - *current;
                    *current = 0.0;
                    leak
                }
            }
            _ => amount,
        }
    }
}

/// Shield weapon name for a given unit kind. Returns `None` for units
/// that don't carry a shield. Upstream wires `homebaseshieldgood` into
/// weapon slot 2 of kernel / hole / carrier (see carrier.fbi — the
/// slot-1 BuildLaser exists only to keep Spring from reordering the
/// slots), and `minifacshieldgood` into the small structures.
pub fn shield_weapon_for(kind: UnitKind) -> Option<&'static str> {
    match kind {
        UnitKind::Kernel | UnitKind::Hole | UnitKind::Carrier => Some("homebaseshieldgood"),
        UnitKind::Socket
        | UnitKind::Window
        | UnitKind::Port
        | UnitKind::Firewall
        | UnitKind::Terminal
        | UnitKind::Obelisk => Some("minifacshieldgood"),
        _ => None,
    }
}

/// Build a `ShieldState` from the weapon registry, if `kind` has a
/// shield weapon that resolves to a known def.
pub fn shield_state_for(kind: UnitKind, weapons: &WeaponRegistry) -> Option<ShieldState> {
    let weapon = shield_weapon_for(kind)?;
    let Some(def) = weapons.get(weapon) else {
        warn!(
            "shield weapon {weapon:?} for {kind:?} missing from registry — unit spawns unshielded"
        );
        return None;
    };
    if !def.is_shield {
        warn!("weapon {weapon:?} for {kind:?} is not flagged is_shield — unit spawns unshielded");
        return None;
    }
    let (max_power, current_power) = if def.shield_power > 0.0 {
        (Some(def.shield_power), Some(def.shield_power))
    } else {
        (None, None)
    };
    Some(ShieldState {
        max_power,
        current_power,
        regen_per_sec: def.shield_power_regen,
    })
}

/// Regenerate finite shields toward their max over time.
/// Infinite shields are skipped — they have nothing to regen.
pub fn regen_shields(time: Res<Time>, mut query: Query<&mut ShieldState>) {
    let dt = time.delta_secs();
    for mut shield in &mut query {
        let (Some(max), regen) = (shield.max_power, shield.regen_per_sec) else {
            continue;
        };
        if let Some(current) = shield.current_power.as_mut()
            && *current < max
        {
            *current = (*current + regen * dt).min(max);
        }
    }
}

/// Runtime toggle for ONS mode.
///
/// Upstream's `anydefs_post.lua` scrubs every shielded FBI's weapon
/// slots back to `BuildLaser` when `Spring.GetModOptions()["ons"]`
/// is `nil` or `"0"` — i.e. a normal game. Only the dedicated ONS
/// gametype keeps the `homebaseshieldgood` / `minifacshieldgood`
/// weapons attached.
///
/// Defaults to off to match the standard sandbox we ship. Flip it
/// on if/when an ONS scenario loads and needs the indestructible
/// homebase / minifac shields.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct OnsMode {
    pub enabled: bool,
}

/// Any shielded unit that doesn't yet have a `ShieldState` gets one
/// this frame — **but only when `OnsMode::enabled` is true**. See the
/// module-level note: without ONS, upstream removes the shield weapons
/// outright, and our shield pool is `shieldpower=0` → infinite, so
/// every homebase / socket / terminal / firewall / obelisk becomes
/// unkillable if we unconditionally attach one. `Added<UnitType>`
/// keeps the scan proportional to actual spawns.
pub fn attach_shields(
    new_units: Query<(Entity, &UnitType), Added<UnitType>>,
    weapons: Res<WeaponRegistry>,
    ons: Res<OnsMode>,
    mut commands: Commands,
) {
    if !ons.enabled {
        return;
    }
    for (entity, unit) in &new_units {
        if let Some(state) = shield_state_for(unit.0, &weapons) {
            commands.entity(entity).insert(state);
        }
    }
}

// --- Shield shell visuals ------------------------------------------------
//
// Upstream's shield weapons set `visibleshield=1` so the engine draws a
// hex-textured repulsor dome (green at full power, red when down —
// `onsshield.tdf` colors + `texture1=hexgrid`). We approximate that with
// an unlit translucent sphere child, tinted by the remaining power
// ratio: full → upstream's `shieldColonColor` green `0 0.5 0`, empty →
// red `0.5 0 0` with the shell fading to `Visibility::Hidden` so a
// collapsed shield stops advertising invulnerability. Only ONS mode has
// `ShieldState` at all (see `attach_shields`), so sandbox games stay
// clean.

/// Full-power shell color: upstream `homebaseshieldgood`'s
/// `shieldColonColor=0 0.5 0`, lifted to a readable alpha.
const SHELL_COLOR_FULL: Color = Color::srgba(0.0, 0.5, 0.0, 0.16);
/// Depleted shell color: upstream's red `0.5 0 0`.
const SHELL_COLOR_EMPTY: Color = Color::srgba(0.5, 0.0, 0.0, 0.05);

/// Marker on the translucent dome child spawned for a shielded unit.
#[derive(Component)]
pub struct ShieldShell;

/// Shared sphere mesh + hex texture for all shells (radius comes from
/// per-entity `Transform::scale` — upstream radii are 128 for
/// homebases, 64 for minifacs).
#[derive(Resource, Default)]
pub struct ShieldShellAssets {
    mesh: Option<Handle<Mesh>>,
    texture: Option<Handle<Image>>,
}

/// How many times `hexgrid.tga` wraps the dome. Upstream tiles the
/// texture in engine screen space; on a UV sphere six repeats reads
/// as the same cell density at battle zoom.
const SHELL_HEX_TILES: f32 = 6.0;

/// Spawn a dome child for every newly-shielded unit. Reads the shield
/// radius from the unit's shield weapon def so Kernel / Hole / Carrier
/// (128) and minifacs (64) match upstream's `shieldradius`.
pub fn spawn_shield_shells(
    new_shields: Query<(Entity, &UnitType), Added<ShieldState>>,
    weapons: Res<WeaponRegistry>,
    mut assets: ResMut<ShieldShellAssets>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<S3OModelCache>,
    mut commands: Commands,
) {
    for (entity, unit) in &new_shields {
        let Some(radius) = shield_weapon_for(unit.0)
            .and_then(|w| weapons.get(w))
            .map(|def| def.shield_radius)
            .filter(|r| *r > 0.0)
        else {
            continue;
        };
        let mesh = assets
            .mesh
            .get_or_insert_with(|| meshes.add(Sphere::new(1.0)))
            .clone();
        // Upstream `texture1=hexgrid` (onsshield.tdf): the dome is a
        // visible hex-patterned repulsor, not a bare bubble. Falls
        // back to the flat tint when the upstream bitmap isn't on
        // disk (wasm bundle without assets).
        let texture = match &assets.texture {
            Some(handle) => Some(handle.clone()),
            None => load_beam_texture("hexgrid.tga", &mut model_cache, &mut images).map(
                |(handle, _, _)| {
                    assets.texture = Some(handle.clone());
                    handle
                },
            ),
        };
        let material = materials.add(StandardMaterial {
            base_color: SHELL_COLOR_FULL,
            base_color_texture: texture,
            unlit: true,
            alpha_mode: AlphaMode::Blend,
            // Double-sided: the camera looks at the dome from outside,
            // but units inside still see its inner surface.
            cull_mode: None,
            uv_transform: bevy::math::Affine2::from_scale(Vec2::splat(SHELL_HEX_TILES)),
            ..default()
        });
        commands.entity(entity).with_children(|parent| {
            parent.spawn((
                ShieldShell,
                Mesh3d(mesh),
                MeshMaterial3d(material),
                Transform::from_scale(Vec3::splat(radius)),
                Visibility::Inherited,
                InheritedVisibility::VISIBLE,
                ViewVisibility::default(),
            ));
        });
    }
}

/// Retint every shell from its owner's remaining power. Runs every
/// frame over the (small) set of shielded units — the tint lerp is
/// trivial and keeps the shell honest after each hit/regen tick.
#[allow(clippy::type_complexity)]
pub fn tick_shield_shells(
    shields: Query<(&ShieldState, &Children)>,
    mut shells: Query<(
        &ShieldShell,
        &MeshMaterial3d<StandardMaterial>,
        &mut Visibility,
    )>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (state, children) in &shields {
        let ratio = match (state.max_power, state.current_power) {
            // Infinite shield (upstream `shieldpower=0`): always full.
            (None, _) => 1.0,
            (Some(max), Some(current)) if max > 0.0 => (current / max).clamp(0.0, 1.0),
            _ => 0.0,
        };
        let full = SHELL_COLOR_FULL.to_srgba();
        let empty = SHELL_COLOR_EMPTY.to_srgba();
        let tinted = Color::srgba(
            empty.red + (full.red - empty.red) * ratio,
            empty.green + (full.green - empty.green) * ratio,
            empty.blue + (full.blue - empty.blue) * ratio,
            empty.alpha + (full.alpha - empty.alpha) * ratio,
        );
        for child in children {
            let Ok((_, material, mut visibility)) = shells.get_mut(*child) else {
                continue;
            };
            if let Some(mat) = materials.get_mut(&material.0) {
                mat.base_color = tinted;
            }
            *visibility = if ratio <= 0.0 {
                Visibility::Hidden
            } else {
                Visibility::Inherited
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shield_weapon_mapping_matches_upstream() {
        assert_eq!(
            shield_weapon_for(UnitKind::Kernel),
            Some("homebaseshieldgood")
        );
        assert_eq!(
            shield_weapon_for(UnitKind::Hole),
            Some("homebaseshieldgood")
        );
        // carrier.fbi wires homebaseshieldgood into weapon slot 2.
        assert_eq!(
            shield_weapon_for(UnitKind::Carrier),
            Some("homebaseshieldgood")
        );
        assert_eq!(
            shield_weapon_for(UnitKind::Socket),
            Some("minifacshieldgood")
        );
        assert_eq!(
            shield_weapon_for(UnitKind::Firewall),
            Some("minifacshieldgood")
        );
        // Connection (the mobile teleporter) has no shield in upstream.
        assert!(shield_weapon_for(UnitKind::Connection).is_none());
        assert!(shield_weapon_for(UnitKind::Bit).is_none());
    }

    #[test]
    fn infinite_shield_absorbs_all() {
        let mut shield = ShieldState {
            max_power: None,
            current_power: None,
            regen_per_sec: 0.0,
        };
        assert_eq!(shield.absorb(100.0), 0.0);
        assert_eq!(shield.absorb(1_000_000.0), 0.0);
    }

    #[test]
    fn finite_shield_absorbs_until_depleted() {
        let mut shield = ShieldState {
            max_power: Some(500.0),
            current_power: Some(500.0),
            regen_per_sec: 0.0,
        };
        assert_eq!(shield.absorb(100.0), 0.0);
        assert_eq!(shield.current_power, Some(400.0));
        assert_eq!(shield.absorb(500.0), 100.0);
        assert_eq!(shield.current_power, Some(0.0));
        // Fully depleted now — damage leaks through entirely.
        assert_eq!(shield.absorb(50.0), 50.0);
    }
}

#[cfg(test)]
mod shell_tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use spring_tdf::WeaponDef;

    fn shell_app() -> (App, Entity) {
        let mut app = App::new();
        app.init_resource::<ShieldShellAssets>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<Assets<Image>>()
            .init_resource::<S3OModelCache>()
            .init_resource::<OnsMode>();
        let mut weapons = WeaponRegistry::default();
        weapons.insert_for_test(
            "homebaseshieldgood",
            WeaponDef {
                is_shield: true,
                shield_radius: 128.0,
                shield_power: 0.0, // infinite, upstream homebase style
                ..Default::default()
            },
        );
        app.insert_resource(weapons);
        app.world_mut().resource_mut::<OnsMode>().enabled = true;

        let kernel = app.world_mut().spawn(UnitType(UnitKind::Kernel)).id();
        app.world_mut().run_system_once(attach_shields).unwrap();
        (app, kernel)
    }

    fn shell_base_color(world: &mut World) -> Srgba {
        let (_, mat_handle) = world
            .query::<(&ShieldShell, &MeshMaterial3d<StandardMaterial>)>()
            .single(world)
            .unwrap()
            .clone();
        world
            .get_resource::<Assets<StandardMaterial>>()
            .unwrap()
            .get(&mat_handle.0)
            .unwrap()
            .base_color
            .to_srgba()
    }

    /// A shielded unit grows a dome child scaled to the weapon's
    /// `shieldradius`, and the tint tracks the remaining power:
    /// full → upstream green, half → halfway toward red, empty → the
    /// shell hides instead of advertising protection.
    #[test]
    fn shell_spawn_scale_and_power_tint() {
        let (mut app, kernel) = shell_app();

        // attach_shields must have granted an infinite pool.
        let state = app.world().get::<ShieldState>(kernel).unwrap();
        assert_eq!(state.max_power, None);

        app.world_mut()
            .run_system_once(spawn_shield_shells)
            .unwrap();

        let mut shell = None;
        for child in app
            .world()
            .get::<Children>(kernel)
            .expect("shell child spawned")
            .iter()
        {
            if app.world().get::<ShieldShell>(child).is_some() {
                shell = Some(child);
            }
        }
        let shell = shell.expect("ShieldShell child");
        let scale = app.world().get::<Transform>(shell).unwrap().scale.x;
        assert_eq!(scale, 128.0, "homebase radius comes from the shield def");

        // Full power (infinite → ratio 1): green dominates.
        app.world_mut().run_system_once(tick_shield_shells).unwrap();
        let full = shell_base_color(&mut app.world_mut());
        assert!(
            full.green > full.red,
            "full shield reads green, got {full:?}"
        );

        // Half power: red channel climbs above its full-power value.
        app.world_mut().entity_mut(kernel).insert(ShieldState {
            max_power: Some(100.0),
            current_power: Some(50.0),
            regen_per_sec: 0.0,
        });
        app.world_mut().run_system_once(tick_shield_shells).unwrap();
        let half = shell_base_color(&mut app.world_mut());
        assert!(
            half.red > full.red && half.green < full.green,
            "half power must sit between the green and red anchors: {half:?} vs {full:?}"
        );

        // Depleted → shell hides.
        app.world_mut().entity_mut(kernel).insert(ShieldState {
            max_power: Some(100.0),
            current_power: Some(0.0),
            regen_per_sec: 0.0,
        });
        app.world_mut().run_system_once(tick_shield_shells).unwrap();
        let visibility = app.world().get::<Visibility>(shell).unwrap();
        assert_eq!(*visibility, Visibility::Hidden);
    }
}
