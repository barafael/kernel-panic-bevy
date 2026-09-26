//! Constructor decisions: which datavent to claim next and what to put
//! on it (upstream `KPAI.lua::DispatchCon` / `GetNiceGeo`).

use bevy::prelude::Vec3;

use crate::rng::xorshift32;
use crate::units::content::definitions::UnitKind;

/// Minifacs owned before a constructor starts rolling for the faction
/// special (`#ListOfMiniFacs >= 3` in `DispatchCon`).
pub const SPECIAL_MIN_MINIFACS: usize = 3;

/// Upstream `DispatchCon` build choice: the constructor's minifac
/// (assembler → socket, trojan → window, gateway → port), except that
/// once the team owns [`SPECIAL_MIN_MINIFACS`] minifacs a 1-in-3 roll
/// (`math.random(3) == 1`, here `roll3 == 0`) builds the faction
/// special instead — terminal / obelisk / firewall.
pub fn choose_building(
    constructor: UnitKind,
    owned_minifacs: usize,
    roll3: u32,
) -> Option<UnitKind> {
    let (minifac, special) = match constructor {
        UnitKind::Assembler => (UnitKind::Socket, UnitKind::Terminal),
        UnitKind::Trojan => (UnitKind::Window, UnitKind::Obelisk),
        UnitKind::Gateway => (UnitKind::Port, UnitKind::Firewall),
        _ => return None,
    };
    if owned_minifacs >= SPECIAL_MIN_MINIFACS && roll3.is_multiple_of(3) {
        Some(special)
    } else {
        Some(minifac)
    }
}

/// Upstream `GetNiceGeo`: rather than always the closest free vent
/// (which sends every constructor down the same line), sample
/// `max(n/2, 2)` random candidates and keep the nearest of those. Nearby
/// vents still win most of the time, but expansion spreads out.
/// Returns an index into `vents`.
pub fn pick_datavent(from: Vec3, vents: &[Vec3], rng: &mut u32) -> Option<usize> {
    if vents.is_empty() {
        return None;
    }
    let samples = (vents.len() / 2).max(2);
    (0..samples)
        .map(|_| xorshift32(rng) as usize % vents.len())
        .min_by(|&a, &b| {
            from.distance_squared(vents[a])
                .total_cmp(&from.distance_squared(vents[b]))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minifac_until_three_owned() {
        for roll in 0..3 {
            assert_eq!(
                choose_building(UnitKind::Assembler, 2, roll),
                Some(UnitKind::Socket)
            );
        }
        assert_eq!(
            choose_building(UnitKind::Trojan, 0, 0),
            Some(UnitKind::Window)
        );
        assert_eq!(choose_building(UnitKind::Bit, 5, 0), None);
    }

    /// With ≥3 minifacs, one roll in three builds the special.
    #[test]
    fn special_one_in_three_after_three_minifacs() {
        assert_eq!(
            choose_building(UnitKind::Assembler, 3, 0),
            Some(UnitKind::Terminal)
        );
        assert_eq!(
            choose_building(UnitKind::Trojan, 4, 3),
            Some(UnitKind::Obelisk)
        );
        assert_eq!(
            choose_building(UnitKind::Gateway, 3, 0),
            Some(UnitKind::Firewall)
        );
        assert_eq!(
            choose_building(UnitKind::Gateway, 3, 1),
            Some(UnitKind::Port)
        );
        assert_eq!(
            choose_building(UnitKind::Assembler, 3, 2),
            Some(UnitKind::Socket)
        );
    }

    #[test]
    fn pick_datavent_prefers_near_and_handles_edges() {
        let mut rng = 0x1234_5678;
        assert_eq!(pick_datavent(Vec3::ZERO, &[], &mut rng), None);
        let one = [Vec3::new(500.0, 0.0, 0.0)];
        assert_eq!(pick_datavent(Vec3::ZERO, &one, &mut rng), Some(0));
        // Over many draws the nearest vent is picked more than any
        // other single vent.
        let vents = [
            Vec3::new(100.0, 0.0, 0.0),
            Vec3::new(900.0, 0.0, 0.0),
            Vec3::new(1800.0, 0.0, 0.0),
            Vec3::new(2700.0, 0.0, 0.0),
        ];
        let mut hits = [0; 4];
        for _ in 0..400 {
            hits[pick_datavent(Vec3::ZERO, &vents, &mut rng).unwrap()] += 1;
        }
        assert!(hits[0] > hits[1] && hits[0] > hits[2] && hits[0] > hits[3]);
        assert!(hits[1] > 0, "sampling spreads expansion: {hits:?}");
    }
}
