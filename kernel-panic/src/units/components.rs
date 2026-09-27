use bevy::prelude::*;

use super::content::definitions::UnitKind;
use super::lifecycle::spawning::EmergeStyle;

/// Which faction a unit belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Component)]
pub enum Faction {
    System,
    Hacker,
    Network,
}

impl Faction {
    /// Every faction, in the original's cycle order: System → Hacker →
    /// Network.
    pub const ALL: [Faction; 3] = [Faction::System, Faction::Hacker, Faction::Network];

    /// Map a map-team-id to a faction, System → Hacker → Network, wrapping.
    /// Used to seed homebase factions at map load.
    pub fn from_team_id(team: u8) -> Self {
        Self::ALL[team as usize % Self::ALL.len()]
    }

    /// The faction whose homebase is `kind`, if it is one.
    pub fn of_homebase(kind: UnitKind) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.homebase() == kind)
    }

    /// The signature color for this faction (used for wireframe glow).
    pub fn color(&self) -> Color {
        match self {
            Faction::System => Color::linear_rgb(0.0, 1.0, 0.3), // green
            Faction::Hacker => Color::linear_rgb(1.0, 0.0, 0.2), // red
            Faction::Network => Color::linear_rgb(0.2, 0.5, 1.0), // blue
        }
    }

    /// The homebase unit kind for this faction.
    pub fn homebase(&self) -> UnitKind {
        match self {
            Faction::System => UnitKind::Kernel,
            Faction::Hacker => UnitKind::Hole,
            // Network's homebase is the upstream `carrier.fbi` (stationary
            // factory, 40,000 HP, yardmap). The mobile `Connection`
            // teleporter is a separate unit the player builds out of the
            // Carrier and then drives around.
            Faction::Network => UnitKind::Carrier,
        }
    }

    /// The basic-combat unit produced by this faction's homebase.
    pub fn basic_combat_unit(&self) -> UnitKind {
        match self {
            Faction::System => UnitKind::Bit,
            Faction::Hacker => UnitKind::Bug,
            Faction::Network => UnitKind::Packet,
        }
    }

    /// The mobile-constructor unit for this faction.
    pub fn constructor(&self) -> UnitKind {
        match self {
            Faction::System => UnitKind::Assembler,
            Faction::Hacker => UnitKind::Trojan,
            Faction::Network => UnitKind::Gateway,
        }
    }

    /// The datavent-built secondary factory (minifac) for this faction.
    pub fn secondary_factory(&self) -> UnitKind {
        match self {
            Faction::System => UnitKind::Socket,
            Faction::Hacker => UnitKind::Window,
            Faction::Network => UnitKind::Port,
        }
    }

    /// How this faction's units appear while being built: System units
    /// rise out of the ground (`BUILD_PERCENT_LEFT` lifts in the
    /// `.bos`), Hacker / Network units materialize in place with an alpha
    /// ramp.
    pub fn emerge_style(self) -> EmergeStyle {
        match self {
            Faction::System => EmergeStyle::Rise,
            Faction::Hacker | Faction::Network => EmergeStyle::Fade,
        }
    }

    /// Faction color as linear `[r, g, b]` in 0-1 range. Shared by weapon-fx
    /// SFX routing and death-explosion fallback coloring.
    pub fn rgb_f32(self) -> [f32; 3] {
        use bevy::color::LinearRgba;
        let c = LinearRgba::from(self.color());
        [c.red, c.green, c.blue]
    }
}

/// What type of unit this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Component)]
pub struct UnitType(pub UnitKind);

/// Current health.
#[derive(Debug, Clone, Component)]
pub struct Health {
    pub current: f32,
    pub max: f32,
}

impl Health {
    pub fn full(max: f32) -> Self {
        Self { current: max, max }
    }

    pub fn fraction(&self) -> f32 {
        self.current / self.max
    }
}

/// Map a 0.0..1.0 health fraction to a red→yellow→green color.
pub fn health_color(frac: f32) -> Color {
    if frac > 0.5 {
        let t = (frac - 0.5) * 2.0;
        Color::linear_rgb(1.0 - t, 1.0, 0.0)
    } else {
        let t = frac * 2.0;
        Color::linear_rgb(1.0, t, 0.0)
    }
}

/// Which team/player owns this unit.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Component)]
pub struct TeamId(pub u8);

/// Static per-unit stats cached at spawn so hot-path systems (movement,
/// combat, spatial index) read them straight off the entity.
///
/// Values are derived from the FBI at spawn time; the registry stays
/// authoritative for any stat not captured here (build time, HP max,
/// auto-heal parameters — none of which are queried per frame per unit).
#[derive(Debug, Clone, Copy, Component)]
pub struct UnitStats {
    /// Footprint-derived collision radius used by unit-unit collision
    /// (`ground_move::ground_collision_system`, the air movement's
    /// `HandleCollisions`). Tighter than the mesh bounds so units can
    /// pack in formation without overlap.
    pub radius: f32,
    /// Volumetric hit radius from the S3O bounding sphere — what Spring's
    /// `CCollisionHandler` tests. Used by `apply_damage` to decide whether
    /// a `spray_angle`-perturbed shot landed on the primary target.
    /// Typically 2-3× larger than `radius` for the same unit.
    pub hit_radius: f32,
    /// Top speed in elmos/s (FBI `MaxVelocity` × 30).
    pub speed: f32,
    /// `maxAcc` in elmos/frame² (FBI `Acceleration`), kept in Spring's
    /// per-frame unit because the ground move type applies it per sim
    /// frame ([`UnitRegistry::acc_rate`]).
    ///
    /// [`UnitRegistry::acc_rate`]: crate::units::content::unit_registry::UnitRegistry::acc_rate
    pub acc_rate: f32,
    /// `maxDec` in elmos/frame² (FBI `BrakeRate`, defaulting to
    /// `acc_rate`) — the braking side of the same ramp.
    pub dec_rate: f32,
    /// Turn speed in rad/s (FBI `TurnRate`); 0 means "snap".
    pub turn_rate: f32,
    pub can_fly: bool,
    pub no_chase_vtol: bool,
}

impl UnitStats {
    /// The per-kind stats `spawn_unit` caches, from the FBI registry.
    /// `hit_radius` comes from the S3O model, which the registry
    /// doesn't know.
    pub fn from_registry(
        kind: UnitKind,
        registry: &crate::units::content::unit_registry::UnitRegistry,
        hit_radius: f32,
    ) -> Self {
        Self {
            radius: registry.collision_radius(kind),
            hit_radius,
            speed: registry.speed(kind),
            acc_rate: registry.acc_rate(kind),
            dec_rate: registry.dec_rate(kind),
            turn_rate: registry.turn_rate(kind),
            can_fly: registry.can_fly(kind),
            no_chase_vtol: registry.no_chase_vtol(kind),
        }
    }
}

/// Two units count as friendly when they share a team. `TeamId` is the
/// ally team (upstream's allyteam): every seat the menu places on the
/// same side shares it, so mirror matches (System vs System) fight and
/// Outgunned's AI seats cooperate.
pub fn is_friendly(unit_team: u8, other_team: u8) -> bool {
    unit_team == other_team
}

/// Invisible child mesh used as a click target for unit selection.
/// The parent entity is the actual unit.
#[derive(Component)]
pub struct SelectionVolume;

/// Marks a unit as a homebase (Kernel/Hole/Connection).
/// Losing all homebases means defeat.
#[derive(Component)]
pub struct Homebase;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faction_from_team_id_wraps() {
        assert_eq!(Faction::from_team_id(0), Faction::System);
        assert_eq!(Faction::from_team_id(1), Faction::Hacker);
        assert_eq!(Faction::from_team_id(2), Faction::Network);
        assert_eq!(Faction::from_team_id(3), Faction::System);
    }
}
