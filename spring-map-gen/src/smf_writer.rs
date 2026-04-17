//! Writer for Spring Map File (.smf) binary format.

use std::io::Write;

use byteorder::{LittleEndian, WriteBytesExt};

use crate::dxt1::encode_dxt1_with_mipmaps;
use crate::{Feature, MapGenError, Rgba};

const SMF_MAGIC: &[u8; 16] = b"spring map file\0";
const SMF_VERSION: i32 = 1;
const SQUARE_SIZE: i32 = 8;
const TEXEL_PER_SQUARE: i32 = 8;
const TILE_SIZE: i32 = 32;
const HEADER_SIZE: i32 = 80;

/// Minimap is always 1024x1024 DXT1 with 8 mip levels (1024 down to 4).
const MINIMAP_SIZE: usize = 1024;
const MINIMAP_MIP_LEVELS: usize = 9; // 1024, 512, 256, 128, 64, 32, 16, 8, 4

/// Builder for constructing an SMF file.
pub struct SmfBuilder {
    map_x: i32,
    map_y: i32,
    min_height: f32,
    max_height: f32,
    /// Row-major heightmap: (map_x+1) * (map_y+1) signed i16 values.
    heightmap: Vec<i16>,
    /// Row-major typemap: (map_x/2) * (map_y/2) bytes.
    typemap: Vec<u8>,
    /// Row-major metalmap: (map_x/2) * (map_y/2) bytes.
    metalmap: Vec<u8>,
    /// Tilemap indices: (map_x/4) * (map_y/4) u32 values.
    tilemap: Vec<u32>,
    /// Name of the SMT tile file referenced by the tilemap.
    smt_filename: String,
    /// Total number of tiles in the referenced SMT.
    num_smt_tiles: u32,
    /// Features placed on the map.
    features: Vec<Feature>,
    /// Color for generating the minimap (solid color for test maps).
    minimap_color: Rgba,
}

impl SmfBuilder {
    /// Create a new SMF builder with the given map dimensions.
    ///
    /// `map_x` and `map_y` must be divisible by 128 and at least 128.
    pub fn new(map_x: i32, map_y: i32) -> Result<Self, MapGenError> {
        if map_x < 128 || map_y < 128 || map_x % 128 != 0 || map_y % 128 != 0 {
            return Err(MapGenError::InvalidDimensions {
                mapx: map_x,
                mapy: map_y,
            });
        }

        let hm_w = (map_x + 1) as usize;
        let hm_h = (map_y + 1) as usize;
        let half_w = (map_x / 2) as usize;
        let half_h = (map_y / 2) as usize;
        let tile_w = (map_x / 4) as usize;
        let tile_h = (map_y / 4) as usize;

        Ok(Self {
            map_x,
            map_y,
            min_height: 0.0,
            max_height: 100.0,
            heightmap: vec![0; hm_w * hm_h],
            typemap: vec![0; half_w * half_h],
            metalmap: vec![0; half_w * half_h],
            tilemap: vec![0; tile_w * tile_h],
            smt_filename: "maps/test.smt".to_string(),
            num_smt_tiles: 1,
            features: Vec::new(),
            minimap_color: Rgba::BLACK,
        })
    }

    pub fn height_range(mut self, min: f32, max: f32) -> Self {
        self.min_height = min;
        self.max_height = max;
        self
    }

    /// Set a single heightmap sample. Coordinates are in heightmap space
    /// (0..=map_x, 0..=map_y).
    pub fn set_height(&mut self, x: usize, z: usize, value: i16) {
        let w = (self.map_x + 1) as usize;
        self.heightmap[z * w + x] = value;
    }

    /// Set the entire heightmap from a slice of i16 values.
    pub fn set_heightmap(&mut self, heights: &[i16]) -> Result<(), MapGenError> {
        let expected = ((self.map_x + 1) * (self.map_y + 1)) as usize;
        if heights.len() != expected {
            return Err(MapGenError::HeightmapSizeMismatch {
                expected,
                actual: heights.len(),
            });
        }
        self.heightmap = heights.to_vec();
        Ok(())
    }

    /// Fill the heightmap using a generator function.
    /// `f(x, z)` receives heightmap coordinates and returns i16.
    pub fn fill_heightmap<F: Fn(usize, usize) -> i16>(&mut self, f: F) {
        let w = (self.map_x + 1) as usize;
        let h = (self.map_y + 1) as usize;
        for z in 0..h {
            for x in 0..w {
                self.heightmap[z * w + x] = f(x, z);
            }
        }
    }

    /// Set a single metalmap cell.
    pub fn set_metal(&mut self, x: usize, z: usize, value: u8) {
        let w = (self.map_x / 2) as usize;
        self.metalmap[z * w + x] = value;
    }

    /// Set a single typemap cell.
    pub fn set_type(&mut self, x: usize, z: usize, value: u8) {
        let w = (self.map_x / 2) as usize;
        self.typemap[z * w + x] = value;
    }

    /// Set the tilemap and SMT reference.
    pub fn set_tilemap(
        &mut self,
        indices: Vec<u32>,
        smt_filename: &str,
        num_smt_tiles: u32,
    ) -> Result<(), MapGenError> {
        let expected = ((self.map_x / 4) * (self.map_y / 4)) as usize;
        if indices.len() != expected {
            return Err(MapGenError::TileCountMismatch {
                expected,
                actual: indices.len(),
            });
        }
        self.tilemap = indices;
        self.smt_filename = smt_filename.to_string();
        self.num_smt_tiles = num_smt_tiles;
        Ok(())
    }

    pub fn add_feature(&mut self, feature: Feature) {
        self.features.push(feature);
    }

    pub fn minimap_color(mut self, color: Rgba) -> Self {
        self.minimap_color = color;
        self
    }

    /// Build the SMF binary data.
    pub fn build(&self) -> Result<Vec<u8>, MapGenError> {
        // Calculate section sizes and offsets.
        let heightmap_bytes = self.heightmap.len() * 2;
        let typemap_bytes = self.typemap.len();
        let metalmap_bytes = self.metalmap.len();

        let minimap_data = self.generate_minimap();
        let minimap_bytes = minimap_data.len();

        let tile_section = self.build_tile_section();
        let feature_section = self.build_feature_section();

        // Layout: header, heightmap, typemap, tiles, minimap, metalmap, features.
        let heightmap_ptr = HEADER_SIZE;
        let typemap_ptr = heightmap_ptr + heightmap_bytes as i32;
        let tiles_ptr = typemap_ptr + typemap_bytes as i32;
        let minimap_ptr = tiles_ptr + tile_section.len() as i32;
        let metalmap_ptr = minimap_ptr + minimap_bytes as i32;
        let feature_ptr = metalmap_ptr + metalmap_bytes as i32;

        let total_size = feature_ptr as usize + feature_section.len();
        let mut buf = Vec::with_capacity(total_size);

        // Write header (80 bytes).
        buf.write_all(SMF_MAGIC)?;
        buf.write_i32::<LittleEndian>(SMF_VERSION)?;
        buf.write_i32::<LittleEndian>(42)?; // mapid
        buf.write_i32::<LittleEndian>(self.map_x)?;
        buf.write_i32::<LittleEndian>(self.map_y)?;
        buf.write_i32::<LittleEndian>(SQUARE_SIZE)?;
        buf.write_i32::<LittleEndian>(TEXEL_PER_SQUARE)?;
        buf.write_i32::<LittleEndian>(TILE_SIZE)?;
        buf.write_f32::<LittleEndian>(self.min_height)?;
        buf.write_f32::<LittleEndian>(self.max_height)?;
        buf.write_i32::<LittleEndian>(heightmap_ptr)?;
        buf.write_i32::<LittleEndian>(typemap_ptr)?;
        buf.write_i32::<LittleEndian>(tiles_ptr)?;
        buf.write_i32::<LittleEndian>(minimap_ptr)?;
        buf.write_i32::<LittleEndian>(metalmap_ptr)?;
        buf.write_i32::<LittleEndian>(feature_ptr)?;
        buf.write_i32::<LittleEndian>(0)?; // numExtraHeaders
        assert_eq!(buf.len(), HEADER_SIZE as usize);

        // Write heightmap (i16 little-endian).
        for &h in &self.heightmap {
            buf.write_i16::<LittleEndian>(h)?;
        }

        // Write typemap.
        buf.write_all(&self.typemap)?;

        // Write tile section.
        buf.write_all(&tile_section)?;

        // Write minimap.
        buf.write_all(&minimap_data)?;

        // Write metalmap.
        buf.write_all(&self.metalmap)?;

        // Write feature section.
        buf.write_all(&feature_section)?;

        assert_eq!(buf.len(), total_size);

        Ok(buf)
    }

    fn generate_minimap(&self) -> Vec<u8> {
        let Rgba { r, g, b, a: _ } = self.minimap_color;
        let mut rgba = vec![0u8; MINIMAP_SIZE * MINIMAP_SIZE * 4];
        for pixel in rgba.chunks_exact_mut(4) {
            pixel[0] = r;
            pixel[1] = g;
            pixel[2] = b;
            pixel[3] = 255;
        }
        encode_dxt1_with_mipmaps(&rgba, MINIMAP_SIZE, MINIMAP_MIP_LEVELS)
    }

    fn build_tile_section(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // MapTileHeader.
        buf.extend_from_slice(&1i32.to_le_bytes()); // numTileFiles
        buf.extend_from_slice(&(self.num_smt_tiles as i32).to_le_bytes()); // numTiles

        // Single tile file entry: count + null-terminated filename.
        buf.extend_from_slice(&(self.num_smt_tiles as i32).to_le_bytes());
        buf.extend_from_slice(self.smt_filename.as_bytes());
        buf.push(0); // null terminator

        // Tilemap indices.
        for &idx in &self.tilemap {
            buf.extend_from_slice(&idx.to_le_bytes());
        }

        buf
    }

    fn build_feature_section(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Collect unique type names.
        let mut type_names: Vec<String> = Vec::new();
        for f in &self.features {
            if !type_names.iter().any(|n| n == &f.type_name) {
                type_names.push(f.type_name.clone());
            }
        }

        // MapFeatureHeader.
        buf.extend_from_slice(&(type_names.len() as i32).to_le_bytes());
        buf.extend_from_slice(&(self.features.len() as i32).to_le_bytes());

        // Type name strings (null-terminated).
        for name in &type_names {
            buf.extend_from_slice(name.as_bytes());
            buf.push(0);
        }

        // Feature structs (24 bytes each).
        for f in &self.features {
            let type_index = type_names
                .iter()
                .position(|n| n == &f.type_name)
                .unwrap_or(0);
            buf.extend_from_slice(&(type_index as i32).to_le_bytes());
            buf.extend_from_slice(&f.x.to_le_bytes());
            buf.extend_from_slice(&f.y.to_le_bytes());
            buf.extend_from_slice(&f.z.to_le_bytes());
            buf.extend_from_slice(&f.rotation.to_le_bytes());
            buf.extend_from_slice(&f.relative_size.to_le_bytes());
        }

        buf
    }
}
