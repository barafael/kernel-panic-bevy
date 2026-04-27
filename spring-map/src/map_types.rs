use binrw::binread;
use thiserror::Error;

#[cfg(test)]
pub(crate) const SMF_MAGIC: &[u8; 16] = b"spring map file\0";
pub(crate) const SMF_VERSION: i32 = 1;
pub const SQUARE_SIZE: i32 = 8;

// ---------------------------------------------------------------------------
// Error hierarchy
// ---------------------------------------------------------------------------

/// Top-level error for loading a Spring map end-to-end.
#[derive(Debug, Error)]
pub enum MapError {
    #[error("archive error: {0}")]
    Archive(#[from] ArchiveError),
    #[error("SMF parse error: {0}")]
    Smf(#[from] SmfParseError),
    #[error("SMT parse error: {0}")]
    Smt(#[from] SmtParseError),
}

#[derive(Debug, Error)]
pub enum SmfParseError {
    #[error("I/O error reading SMF: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid SMF magic bytes")]
    BadMagic,
    #[error("unsupported SMF version {0} (expected {SMF_VERSION})")]
    BadVersion(i32),
    #[error("heightmap truncated: expected {expected} samples, got {actual}")]
    HeightmapTruncated { expected: usize, actual: usize },
    #[error("feature data truncated")]
    FeatureTruncated,
    #[error("metalmap truncated: expected {expected} bytes, got {actual}")]
    MetalmapTruncated { expected: usize, actual: usize },
}

#[derive(Debug, Error)]
pub enum SmtParseError {
    #[error("I/O error reading SMT: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid SMT magic bytes")]
    BadMagic,
}

#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("7z extraction failed: {0}")]
    SevenZ(String),
    #[error("zip extraction failed: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("no .smf file found inside archive")]
    NoSmfFound,
    #[error("no .smt tile file found inside archive")]
    NoSmtFound,
    #[error("unsupported archive format: {0}")]
    UnsupportedFormat(String),
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Parsed SMF file header.
///
/// File-offset pointers are internal to the parser and not exposed.
#[binread]
#[derive(Debug, Clone)]
#[br(little, magic = b"spring map file\0")]
#[allow(dead_code)] // File-offset fields are part of the binary format
pub struct SmfHeader {
    pub(crate) version: i32,
    pub map_id: i32,
    pub map_x: i32,
    pub map_y: i32,
    #[br(temp)]
    _square_size: i32,
    #[br(temp)]
    _texel_per_square: i32,
    #[br(temp)]
    _tile_size: i32,
    pub min_height: f32,
    pub max_height: f32,

    // Internal — used by the parser / tilemap reader.
    pub(crate) heightmap_ptr: i32,
    pub(crate) type_map_ptr: i32,
    pub(crate) tiles_ptr: i32,
    pub(crate) minimap_ptr: i32,
    pub(crate) metalmap_ptr: i32,
    pub(crate) feature_ptr: i32,
    pub(crate) num_extra_headers: i32,
}

impl SmfHeader {
    /// Create a synthetic header for test/fallback maps.
    pub fn new_flat(map_x: i32, map_y: i32, min_height: f32, max_height: f32) -> Self {
        Self {
            version: SMF_VERSION,
            map_id: 0,
            map_x,
            map_y,
            min_height,
            max_height,
            heightmap_ptr: 0,
            type_map_ptr: 0,
            tiles_ptr: 0,
            minimap_ptr: 0,
            metalmap_ptr: 0,
            feature_ptr: 0,
            num_extra_headers: 0,
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

    pub fn sample_to_world_height(&self, raw: i16) -> f32 {
        let unsigned = raw as u16;
        self.min_height + (unsigned as f32 / 65535.0) * (self.max_height - self.min_height)
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

/// Fully parsed SMF map data.
#[derive(Debug, Clone)]
pub struct ParsedMap {
    pub header: SmfHeader,
    /// Row-major heightmap, already converted to world-space heights.
    pub heights: Vec<f32>,
    pub features: Vec<MapFeature>,
    pub metalmap: Vec<u8>,
}

/// A decoded 32x32 RGBA tile from an SMT file.
#[derive(Debug, Clone)]
pub struct Tile {
    /// 32×32×4 = 4096 bytes of RGBA pixel data.
    pub pixels: [u8; Self::SIZE],
}

impl Tile {
    pub const WIDTH: usize = 32;
    pub const HEIGHT: usize = 32;
    pub const SIZE: usize = Self::WIDTH * Self::HEIGHT * 4;
}

/// A 2D grid of tile indices referencing tiles in the SMT file.
#[derive(Debug, Clone)]
pub struct TileMap {
    pub width: usize,
    pub height: usize,
    pub indices: Vec<u32>,
}

impl TileMap {
    /// Look up the tile index at grid position `(x, y)`.
    pub fn get(&self, x: usize, y: usize) -> u32 {
        self.indices[y * self.width + x]
    }
}

/// A named Lua file extracted from a map archive.
#[derive(Debug, Clone)]
pub struct LuaFile {
    pub path: String,
    pub content: String,
}

/// A raw bitmap (PNG/JPG) shipped inside the map archive.
///
/// Lua-driven maps (e.g. Hex Farm) keep their actual diffuse textures
/// here under `bitmaps/MapTex/` and bind them at runtime via Spring's
/// graphics API. We pre-extract them so the texture compositor can pick
/// the right one without touching the archive again.
#[derive(Debug, Clone)]
pub struct BitmapFile {
    pub path: String,
    pub data: Vec<u8>,
}

/// One captured `SendToUnsynced(...)` call from a Lua gadget.
///
/// Spring gadgets run in two halves — synced (gameplay) and unsynced
/// (rendering). They communicate via `SendToUnsynced(msgName, ...args)`.
/// We only execute the synced half and capture the messages it would
/// have sent, so the renderer can mirror the unsynced compositing logic
/// from Rust.
pub type UnsyncedMessage = Vec<UnsyncedArg>;

#[derive(Debug, Clone)]
pub enum UnsyncedArg {
    Integer(i64),
    Number(f64),
    String(String),
    Bool(bool),
    Nil,
}

impl UnsyncedArg {
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(i) => Some(*i),
            Self::Number(n) => Some(*n as i64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s.as_str()),
            _ => None,
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

/// Assembled ground texture from tiled SMT data.
#[derive(Debug)]
pub struct GroundTexture {
    pub width: usize,
    pub height: usize,
    /// RGBA8 pixel data, row-major, `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}
