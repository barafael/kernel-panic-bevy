use thiserror::Error;

/// Elmos per heightmap square (`SQUARE_SIZE` in the engine).
pub const SQUARE_SIZE: i32 = 8;

/// Largest map side accepted, in squares (Spring's own maps top out at
/// 4096; the heightmap allocation is `(x+1)(y+1)` floats).
pub const MAX_MAP_SQUARES: i32 = 16_384;

/// Map squares per ground-texture tile side: the type and metal maps are
/// `(map_x / 2) × (map_y / 2)` cells, the texture `(map_x / 4) × (map_y
/// / 4)` tiles of 32 texels.
pub const TILE_SQUARES: i32 = 4;

/// The map's dimensions and height range (what survives of the Spring
/// SMF header in a `.kpmap`).
#[derive(Debug, Clone)]
pub struct MapHeader {
    pub map_x: i32,
    pub map_y: i32,
    pub min_height: f32,
    pub max_height: f32,
}

#[derive(Debug, Error)]
#[error(
    "map size {map_x}x{map_y} squares is not a multiple of {TILE_SQUARES} in \
     {TILE_SQUARES}..={MAX_MAP_SQUARES}"
)]
pub struct BadMapSize {
    pub map_x: i32,
    pub map_y: i32,
}

impl MapHeader {
    pub fn new(map_x: i32, map_y: i32, min_height: f32, max_height: f32) -> Self {
        Self {
            map_x,
            map_y,
            min_height,
            max_height,
        }
    }

    /// Reject sizes that would over-allocate or underflow downstream
    /// (`map_x + 1` as usize, `width - 1`, square-count products), and
    /// sides the tile / type / metal maps can't cover: a side of 1..3
    /// squares has no tile at all (an empty ground texture), and a
    /// non-multiple of 4 leaves the last squares without one. Spring's
    /// mapconv only produces multiples of 128.
    pub fn validate_size(&self) -> Result<(), BadMapSize> {
        let ok = |n: i32| (TILE_SQUARES..=MAX_MAP_SQUARES).contains(&n) && n % TILE_SQUARES == 0;
        if ok(self.map_x) && ok(self.map_y) {
            Ok(())
        } else {
            Err(BadMapSize {
                map_x: self.map_x,
                map_y: self.map_y,
            })
        }
    }

    pub fn heightmap_width(&self) -> usize {
        (self.map_x + 1) as usize
    }

    pub fn heightmap_height(&self) -> usize {
        (self.map_y + 1) as usize
    }

    pub fn heightmap_len(&self) -> usize {
        self.heightmap_width() * self.heightmap_height()
    }

    pub fn world_width(&self) -> f32 {
        (self.map_x * SQUARE_SIZE) as f32
    }

    pub fn world_depth(&self) -> f32 {
        (self.map_y * SQUARE_SIZE) as f32
    }

    pub fn metalmap_width(&self) -> usize {
        (self.map_x / 2) as usize
    }

    /// Typemap cells per row: one per 2×2 squares (`mapDims.hmapx`).
    pub fn typemap_width(&self) -> usize {
        (self.map_x / 2) as usize
    }

    pub fn typemap_height(&self) -> usize {
        (self.map_y / 2) as usize
    }

    pub fn metalmap_height(&self) -> usize {
        (self.map_y / 2) as usize
    }
}

/// Standard feature types defined by the Spring engine.
///
/// The engine hardcodes two categories: geothermal vents and trees.
/// All other feature types are mod-specific and captured by `Other`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum FeatureType {
    /// Geothermal vent — used by Kernel Panic as datavent (factory placement site).
    GeoVent,
    /// One of the 20 default tree types (index 0–19).
    Tree(u8),
    /// Mod-specific or unknown feature type.
    Other(String),
}

impl FeatureType {
    /// Parse a feature type name string into the enum.
    pub fn from_name(name: &str) -> Self {
        if name.eq_ignore_ascii_case("geovent") {
            return Self::GeoVent;
        }
        if let Some(index) = name
            .to_ascii_lowercase()
            .strip_prefix("treetype")
            .and_then(|s| s.parse::<u8>().ok())
            .filter(|&i| i <= 19)
        {
            return Self::Tree(index);
        }
        Self::Other(name.to_string())
    }

    pub fn is_geovent(&self) -> bool {
        matches!(self, Self::GeoVent)
    }

    pub fn is_tree(&self) -> bool {
        matches!(self, Self::Tree(_))
    }
}

impl std::fmt::Display for FeatureType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GeoVent => write!(f, "GeoVent"),
            Self::Tree(index) => write!(f, "TreeType{index}"),
            Self::Other(name) => write!(f, "{name}"),
        }
    }
}

/// A datavent (geovent feature) with the facing a factory built on it
/// takes (`crate::datavent`): `0` south, `1` east, `2` north, `3` west.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Datavent {
    pub x: f32,
    pub z: f32,
    pub facing: u8,
}

/// A single feature placement on the map.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MapFeature {
    pub feature_type: FeatureType,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    raw_rotation: f32,
    pub relative_size: f32,
}

impl MapFeature {
    pub fn new(
        feature_type: FeatureType,
        x: f32,
        y: f32,
        z: f32,
        raw_rotation: f32,
        relative_size: f32,
    ) -> Self {
        Self {
            feature_type,
            x,
            y,
            z,
            raw_rotation,
            relative_size,
        }
    }

    /// Decoded rotation in degrees.
    pub fn rotation_degrees(&self) -> f32 {
        -32767.0 + (self.raw_rotation / 65535.0) * 360.0
    }
}

/// The map's terrain data.
#[derive(Debug, Clone)]
pub struct ParsedMap {
    pub header: MapHeader,
    /// Row-major heightmap, world-space heights.
    pub heights: Vec<f32>,
    pub features: Vec<MapFeature>,
    pub metalmap: Vec<u8>,
    /// Terrain-type map: one index per 2×2 squares, `typemap_width() ×
    /// typemap_height()` bytes, row-major — what [`TerrainTypes`] is
    /// indexed with. Empty when the map carries none (synthetic maps):
    /// every square is then terrain type 0 with neutral speeds.
    pub typemap: Vec<u8>,
}

/// One terrain type's move-speed multipliers per speed-mod class
/// (`mapInfo->terrainTypes[i].{tank,kbot,hover,ship}Speed`, read by
/// `CMapInfo::ReadTerrainTypes`). Each defaults to 1 and is clamped to
/// ≥ 0; 0 makes the type impassable to that class.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TerrainMoveSpeeds {
    pub tank: f32,
    pub kbot: f32,
    pub hover: f32,
    pub ship: f32,
}

impl Default for TerrainMoveSpeeds {
    fn default() -> Self {
        Self {
            tank: 1.0,
            kbot: 1.0,
            hover: 1.0,
            ship: 1.0,
        }
    }
}

/// The map's terrain types: what [`ParsedMap::typemap`] indexes
/// (`CMapInfo::NUM_TERRAIN_TYPES` = 256 entries).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TerrainTypes {
    pub move_speeds: Vec<TerrainMoveSpeeds>,
}

impl TerrainTypes {
    /// `NUM_TERRAIN_TYPES` (MapInfo.h).
    pub const COUNT: usize = 256;

    /// Speeds of type `index` (neutral for an index the map left out).
    pub fn get(&self, index: u8) -> TerrainMoveSpeeds {
        self.move_speeds
            .get(index as usize)
            .copied()
            .unwrap_or_default()
    }

    /// Does any type change any class's speed? (Most maps leave every
    /// type at 1.)
    pub fn is_neutral(&self) -> bool {
        self.move_speeds
            .iter()
            .all(|s| *s == TerrainMoveSpeeds::default())
    }
}

impl Default for TerrainTypes {
    fn default() -> Self {
        Self {
            move_speeds: vec![TerrainMoveSpeeds::default(); Self::COUNT],
        }
    }
}

/// Pre-computed mipmap chain for a texture.
pub struct MipmapData {
    /// Concatenated pixel data for all mip levels (level 0 first).
    pub pixels: Vec<u8>,
    /// Total number of mip levels (including level 0).
    pub level_count: u32,
}

/// The ground texture.
#[derive(Debug)]
pub struct GroundTexture {
    pub width: usize,
    pub height: usize,
    /// RGBA8 pixel data, row-major, `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_sides_must_cover_whole_tiles() {
        let size = |x, y| MapHeader::new(x, y, 0.0, 1.0).validate_size();
        assert!(size(128, 256).is_ok());
        assert!(size(4, 4).is_ok());
        assert!(size(MAX_MAP_SQUARES, 4).is_ok());
        for bad in [0, 1, 3, 6, 130, -128, MAX_MAP_SQUARES + 4] {
            assert!(size(bad, 128).is_err(), "map_x {bad} accepted");
            assert!(size(128, bad).is_err(), "map_y {bad} accepted");
        }
    }
}
