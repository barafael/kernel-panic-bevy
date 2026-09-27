//! The local (human) player.
//!
//! Exactly one team on the map is driven by the human sitting at this
//! machine; every other team with a homebase is driven by the AI
//! (`super::ai`). This module carries that identity so the input
//! pipeline (selection, orders, command panel) and the game-over check
//! have a single source of truth for "which side am I?".

use bevy::prelude::*;

/// Team id controlled by the human player: the ally team of the
/// [`GameSetup`](crate::game_setup::GameSetup)'s human seat, set at match
/// start, or [`SPECTATOR_TEAM`](crate::game_setup::SPECTATOR_TEAM) when
/// every seat is AI (the menu's attract demo).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Resource)]
pub struct LocalTeam(pub u8);
