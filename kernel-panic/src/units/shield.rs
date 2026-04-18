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

use bevy::prelude::*;

use super::components::UnitType;
use super::definitions::UnitKind;
use super::weapons::WeaponRegistry;

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

/// Any shielded unit that doesn't yet have a `ShieldState`
/// gets one this frame. Runs once per spawned entity; `Added<UnitType>`
/// filters keep the work proportional to actual spawns.
pub fn attach_shields(
    new_units: Query<(Entity, &UnitType), Added<UnitType>>,
    weapons: Res<WeaponRegistry>,
    mut commands: Commands,
) {
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
