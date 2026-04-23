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
/// that don't carry a shield.
pub fn shield_weapon_for(kind: UnitKind) -> Option<&'static str> {
    match kind {
        UnitKind::Kernel | UnitKind::Hole => Some("homebaseshieldgood"),
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
        assert_eq!(
            shield_weapon_for(UnitKind::Socket),
            Some("minifacshieldgood")
        );
        assert_eq!(
            shield_weapon_for(UnitKind::Firewall),
            Some("minifacshieldgood")
        );
        // Connection (Network homebase) has no shield in upstream.
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
