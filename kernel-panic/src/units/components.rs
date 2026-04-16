use bevy::prelude::*;

use super::definitions::UnitKind;

/// Which faction a unit belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Component)]
pub enum Faction {
    System,
    Hacker,
    Network,
}

impl Faction {
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
            Faction::Network => UnitKind::Connection,
        }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Component)]
pub struct TeamId(pub u8);

/// Invisible child mesh used as a click target for unit selection.
/// The parent entity is the actual unit.
#[derive(Component)]
pub struct SelectionVolume;

/// Marks a unit as a homebase (Kernel/Hole/Connection).
/// Losing all homebases means defeat.
#[derive(Component)]
pub struct Homebase;
