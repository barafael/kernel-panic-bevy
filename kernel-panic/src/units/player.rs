//! The local (human) player.
//!
//! Exactly one team on the map is driven by the human sitting at this
//! machine; every other team with a homebase is driven by the AI
//! (`super::ai`). This module carries that identity so the input
//! pipeline (selection, orders, build menu) and the game-over check
//! have a single source of truth for "which side am I?".

use bevy::prelude::*;

/// Team id controlled by the human player.
///
/// Team-to-faction mapping is fixed (`Faction::from_team_id`): team 0 is
/// System, 1 Hacker, 2 Network, so the slice ships as "you are System"
/// — the faction upstream marks as the recommended starting pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Resource)]
pub struct LocalTeam(pub u8);
