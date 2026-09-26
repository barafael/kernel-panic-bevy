//! Shared visual constants for the in-game UI.
//!
//! The HUD widgets themselves (command panel, build bar, tooltip) use the
//! colours of the Spring / Kernel Panic originals they port; what is left
//! here is the remake's own chrome. Faction tints come from
//! [`crate::units::components::Faction::color`] — don't duplicate them.

use bevy::prelude::*;

/// Panel border / divider color (minimap frame).
pub const PANEL_BORDER: Color = Color::srgba(0.20, 0.85, 0.35, 0.85);
