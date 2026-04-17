//! Writer for Spring Map Tiles (.smt) binary format.

use std::io::Write;

use byteorder::{LittleEndian, WriteBytesExt};

use crate::dxt1::encode_dxt1_with_mipmaps;
use crate::{MapGenError, Rgba};

const SMT_MAGIC: &[u8; 16] = b"spring tilefile\0";
const SMT_VERSION: i32 = 1;
const TILE_SIZE: i32 = 32;
const COMPRESSION_TYPE: i32 = 1; // DXT1
const TILE_PIXELS: usize = 32;
const TILE_MIP_LEVELS: usize = 4; // 32, 16, 8, 4
const TILE_BYTES: usize = 680; // 512 + 128 + 32 + 8

/// Builder for constructing an SMT file.
#[derive(Default)]
pub struct SmtBuilder {
    tiles: Vec<Vec<u8>>,
}

impl SmtBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a solid-color tile and return its index.
    pub fn add_solid_tile(&mut self, color: Rgba) -> u32 {
        let mut rgba = vec![0u8; TILE_PIXELS * TILE_PIXELS * 4];
        for pixel in rgba.chunks_exact_mut(4) {
            pixel[0] = color.r;
            pixel[1] = color.g;
            pixel[2] = color.b;
            pixel[3] = color.a;
        }
        self.add_rgba_tile(&rgba)
    }

    /// Add a tile from raw 32x32 RGBA8 pixel data and return its index.
    pub fn add_rgba_tile(&mut self, rgba: &[u8]) -> u32 {
        assert_eq!(rgba.len(), TILE_PIXELS * TILE_PIXELS * 4);
        let dxt1 = encode_dxt1_with_mipmaps(rgba, TILE_PIXELS, TILE_MIP_LEVELS);
        assert_eq!(dxt1.len(), TILE_BYTES);
        let idx = self.tiles.len() as u32;
        self.tiles.push(dxt1);
        idx
    }

    /// Add a tile with a checkerboard pattern.
    pub fn add_checker_tile(&mut self, c0: Rgba, c1: Rgba, cell_size: usize) -> u32 {
        let mut rgba = vec![0u8; TILE_PIXELS * TILE_PIXELS * 4];
        for y in 0..TILE_PIXELS {
            for x in 0..TILE_PIXELS {
                let checker = ((x / cell_size) + (y / cell_size)).is_multiple_of(2);
                let c = if checker { c0 } else { c1 };
                let offset = (y * TILE_PIXELS + x) * 4;
                rgba[offset] = c.r;
                rgba[offset + 1] = c.g;
                rgba[offset + 2] = c.b;
                rgba[offset + 3] = c.a;
            }
        }
        self.add_rgba_tile(&rgba)
    }

    /// Add a tile with horizontal stripes.
    pub fn add_striped_tile(&mut self, c0: Rgba, c1: Rgba, stripe_height: usize) -> u32 {
        let mut rgba = vec![0u8; TILE_PIXELS * TILE_PIXELS * 4];
        for y in 0..TILE_PIXELS {
            for x in 0..TILE_PIXELS {
                let stripe = (y / stripe_height).is_multiple_of(2);
                let c = if stripe { c0 } else { c1 };
                let offset = (y * TILE_PIXELS + x) * 4;
                rgba[offset] = c.r;
                rgba[offset + 1] = c.g;
                rgba[offset + 2] = c.b;
                rgba[offset + 3] = c.a;
            }
        }
        self.add_rgba_tile(&rgba)
    }

    /// Add a tile with a gradient from top color to bottom color.
    pub fn add_gradient_tile(&mut self, top: Rgba, bottom: Rgba) -> u32 {
        let mut rgba = vec![0u8; TILE_PIXELS * TILE_PIXELS * 4];
        for y in 0..TILE_PIXELS {
            let t = y as f32 / (TILE_PIXELS - 1) as f32;
            let r = (top.r as f32 * (1.0 - t) + bottom.r as f32 * t) as u8;
            let g = (top.g as f32 * (1.0 - t) + bottom.g as f32 * t) as u8;
            let b = (top.b as f32 * (1.0 - t) + bottom.b as f32 * t) as u8;
            for x in 0..TILE_PIXELS {
                let offset = (y * TILE_PIXELS + x) * 4;
                rgba[offset] = r;
                rgba[offset + 1] = g;
                rgba[offset + 2] = b;
                rgba[offset + 3] = 255;
            }
        }
        self.add_rgba_tile(&rgba)
    }

    pub fn tile_count(&self) -> u32 {
        self.tiles.len() as u32
    }

    /// Build the SMT binary data.
    pub fn build(&self) -> Result<Vec<u8>, MapGenError> {
        let num_tiles = self.tiles.len();
        let total_size = 32 + num_tiles * TILE_BYTES;
        let mut buf = Vec::with_capacity(total_size);

        // Header (32 bytes).
        buf.write_all(SMT_MAGIC)?;
        buf.write_i32::<LittleEndian>(SMT_VERSION)?;
        buf.write_i32::<LittleEndian>(num_tiles as i32)?;
        buf.write_i32::<LittleEndian>(TILE_SIZE)?;
        buf.write_i32::<LittleEndian>(COMPRESSION_TYPE)?;

        // Tile data.
        for tile in &self.tiles {
            buf.write_all(tile)?;
        }

        assert_eq!(buf.len(), total_size);

        Ok(buf)
    }
}
