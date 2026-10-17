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
