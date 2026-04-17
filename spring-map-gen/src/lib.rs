//! Programmatic generator for Spring RTS engine map files.
//!
//! Generates valid .smf, .smt, .smd files and packages them into .sdz archives
//! that can be loaded through `spring-map`.

mod dxt1;
pub mod sdz_writer;
pub mod smd_writer;
pub mod smf_writer;
pub mod smt_writer;

pub use sdz_writer::package_sdz;
pub use smd_writer::SmdBuilder;
pub use smf_writer::SmfBuilder;
pub use smt_writer::SmtBuilder;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MapGenError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error(
        "invalid map dimensions: mapx={mapx}, mapy={mapy} (must be divisible by 128, minimum 128)"
    )]
    InvalidDimensions { mapx: i32, mapy: i32 },

    #[error("tile count mismatch: tilemap needs {expected} tiles but SMT has {actual}")]
    TileCountMismatch { expected: usize, actual: usize },

    #[error("heightmap size mismatch: expected {expected} samples, got {actual}")]
    HeightmapSizeMismatch { expected: usize, actual: usize },
}

/// A feature to be placed on the map.
#[derive(Debug, Clone)]
pub struct Feature {
    pub type_name: String,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub rotation: f32,
    pub relative_size: f32,
}

impl Feature {
    pub fn geovent(x: f32, y: f32, z: f32) -> Self {
        Self {
            type_name: "GeoVent".to_string(),
            x,
            y,
            z,
            rotation: 0.0,
            relative_size: 1.0,
        }
    }

    pub fn tree(index: u8, x: f32, y: f32, z: f32) -> Self {
        Self {
            type_name: format!("TreeType{index}"),
            x,
            y,
            z,
            rotation: 0.0,
            relative_size: 1.0,
        }
    }
}

/// A start position for a team.
#[derive(Debug, Clone)]
pub struct StartPosition {
    pub team: u32,
    pub x: f32,
    pub z: f32,
}

/// RGBA color for tile generation.
#[derive(Debug, Clone, Copy)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const BLACK: Self = Self::new(0, 0, 0);
}

#[cfg(test)]
mod tests;
