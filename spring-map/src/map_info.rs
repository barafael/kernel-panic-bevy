//! Map metadata: what the original `.smd` (legacy maps) or `mapinfo.lua`
//! (Hex Farm) declared, carried in the `.kpmap`.

/// The map's metadata.
#[derive(Debug, Clone, better_default::Default, serde::Serialize, serde::Deserialize)]
pub struct MapInfo {
    pub description: String,
    /// `MAP.gravity` in elmos/s² (positive = down). A map without the
    /// key gets the engine's default, 130 (`CMapInfo::ReadGlobal`,
    /// `maphelper/mapdefaults.lua`); the engine clamps it to ≥ 0.001
    /// and runs it as `-gravity / GAME_SPEED²` per frame².
    #[default(DEFAULT_GRAVITY)]
    pub gravity: f32,
    pub start_positions: Vec<StartPosition>,
    pub atmosphere: Atmosphere,
    pub lighting: Lighting,
}

impl MapInfo {
    /// The player counts the map's description says it is for: the
    /// number or range before the first "player" ("Intended for 2-6
    /// players", "Intended for 3 players (up to 6)", "2-8 Player Map",
    /// "Made for 3-9 players FFA"), if it says.
    pub fn intended_players(&self) -> Option<(u32, u32)> {
        let text = self.description.to_ascii_lowercase();
        let before = text[..text.find("player")?].trim_end();
        let token = before.rsplit(' ').next()?;
        let mut bounds = token.split('-').map(str::parse::<u32>);
        let low = bounds.next()?.ok()?;
        let high = match bounds.next() {
            Some(n) => n.ok()?,
            None => low,
        };
        (low >= 1 && low <= high).then_some((low, high))
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StartPosition {
    pub team: u32,
    pub x: f32,
    pub z: f32,
}

#[derive(Debug, Clone, better_default::Default, serde::Serialize, serde::Deserialize)]
pub struct Atmosphere {
    pub fog_color: [f32; 3],
    #[default(0.999)]
    pub fog_start: f32,
    #[default([0.01, 0.01, 0.01])]
    pub sky_color: [f32; 3],
    #[default([1.0, 1.0, 1.0])]
    pub sun_color: [f32; 3],
    pub cloud_density: f32,
}

#[derive(Debug, Clone, better_default::Default, serde::Serialize, serde::Deserialize)]
pub struct Lighting {
    #[default([0.0, 1.0, 1.0])]
    pub sun_dir: [f32; 3],
    #[default([0.5, 0.5, 0.5])]
    pub ground_ambient: [f32; 3],
    #[default([0.5, 0.5, 0.5])]
    pub ground_sun_color: [f32; 3],
    #[default(1.0)]
    pub ground_shadow_density: f32,
}

/// The engine's gravity for a map that sets none (elmos/s²).
pub const DEFAULT_GRAVITY: f32 = 130.0;

#[cfg(test)]
mod tests {
    use super::*;

    fn players(description: &str) -> Option<(u32, u32)> {
        MapInfo {
            description: description.into(),
            ..MapInfo::default()
        }
        .intended_players()
    }

    #[test]
    fn intended_players_reads_the_descriptions() {
        assert_eq!(
            players("Dual Core. Map for Kernel Panic. Intended for 2-6 players. Made by FireStorm"),
            Some((2, 6))
        );
        assert_eq!(
            players("Three-Way Handshake v1. Intended for 3 players (up to 6)."),
            Some((3, 3))
        );
        assert_eq!(
            players("2-8 Player Map for the mod Kernel Panic. By Runecrafter"),
            Some((2, 8))
        );
        assert_eq!(
            players("Speed Balls 16 Way. Made for 3-9 players FFA. Use fusions!"),
            Some((3, 9))
        );
        assert_eq!(players("Data Cache L1"), None);
        assert_eq!(players("Play this map with the Kernel Panic mod."), None);
        assert_eq!(players("multiplayer fun"), None);
    }
}
