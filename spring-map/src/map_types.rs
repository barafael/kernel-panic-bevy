use thiserror::Error;

pub const SMF_MAGIC: &[u8; 16] = b"spring map file\0";
pub const SMT_MAGIC: &[u8; 16] = b"spring tilefile\0";
pub const SMF_VERSION: i32 = 1;
pub const SQUARE_SIZE: i32 = 8;

/// The main SMF file header.
///
/// All integer fields are little-endian. Pointer fields are absolute byte
/// offsets into the file. Fields that are always constant (`square_size` = 8,
/// `texel_per_square` = 8, `tile_size` = 32) are validated during parsing
/// but not stored.
#[derive(Debug, Clone)]
pub struct SmfHeader {
    pub map_id: i32,
    /// Map width in Spring map-squares. Always divisible by 128.
    pub map_x: i32,
    /// Map depth in Spring map-squares. Always divisible by 128.
    pub map_y: i32,
    pub square_size: i32,
    pub min_height: f32,
    pub max_height: f32,

    // File-offset pointers (internal to parsing, but kept public for tilemap access)
    pub heightmap_ptr: i32,
    pub type_map_ptr: i32,
    pub tiles_ptr: i32,
    pub minimap_ptr: i32,
    pub metalmap_ptr: i32,
    pub feature_ptr: i32,
    pub num_extra_headers: i32,
}

impl SmfHeader {
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
        (self.map_x * self.square_size) as f32
    }

    pub fn world_depth(&self) -> f32 {
        (self.map_y * self.square_size) as f32
    }

    pub fn metalmap_width(&self) -> usize {
        (self.map_x / 2) as usize
    }

    pub fn metalmap_height(&self) -> usize {
        (self.map_y / 2) as usize
    }
}

#[derive(Debug, Clone)]
pub struct MapFeature {
    pub feature_type: i32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// Encoded rotation. Decode as: `degrees = -32767 + (rotation / 65535) * 360`.
    pub rotation: f32,
    pub relative_size: f32,
}

/// Fully parsed SMF map data.
#[derive(Debug, Clone)]
pub struct ParsedMap {
    pub header: SmfHeader,
    /// Row-major heightmap, already converted to world-space heights.
    pub heights: Vec<f32>,
    pub feature_type_names: Vec<String>,
    pub features: Vec<MapFeature>,
    pub metalmap: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Errors — one per parser module, colocated with the types they describe
// ---------------------------------------------------------------------------

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
