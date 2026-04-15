use std::io::{Cursor, Read, Seek, SeekFrom};

use byteorder::{LittleEndian, ReadBytesExt};

use crate::map_types::{SMT_MAGIC, SmfHeader, SmfParseError, SmtParseError};

const TILE_BYTES: usize = 680;
const TILE_BASE_BYTES: usize = 512;
const TILE_PX: usize = 32;

/// Parse the tilemap from the SMF binary data.
///
/// Returns `(tilemap_width, tilemap_height, tile_indices)`.
pub fn parse_tilemap(
    smf_data: &[u8],
    header: &SmfHeader,
) -> Result<(usize, usize, Vec<u32>), SmfParseError> {
    let mut cursor = Cursor::new(smf_data);
    cursor.seek(SeekFrom::Start(header.tiles_ptr as u64))?;

    let num_tile_files = cursor.read_i32::<LittleEndian>()?;
    let _num_tiles_total = cursor.read_i32::<LittleEndian>()?;

    for _ in 0..num_tile_files {
        let _count = cursor.read_i32::<LittleEndian>()?;
        loop {
            if cursor.read_u8()? == 0 {
                break;
            }
        }
    }

    let tm_w = (header.map_x / 4) as usize;
    let tm_h = (header.map_y / 4) as usize;
    let count = tm_w * tm_h;
    let mut indices = Vec::with_capacity(count);
    for _ in 0..count {
        indices.push(cursor.read_u32::<LittleEndian>()?);
    }

    Ok((tm_w, tm_h, indices))
}

/// Parse all tiles from an SMT file's raw bytes.
///
/// Returns a vec of tiles, each tile is 32x32 RGBA pixels.
pub fn parse_smt_tiles(smt_data: &[u8]) -> Result<Vec<[u8; TILE_PX * TILE_PX * 4]>, SmtParseError> {
    let mut cursor = Cursor::new(smt_data);

    let mut magic = [0u8; 16];
    cursor.read_exact(&mut magic)?;
    if magic != *SMT_MAGIC {
        return Err(SmtParseError::BadMagic);
    }

    let _version = cursor.read_i32::<LittleEndian>()?;
    let num_tiles = cursor.read_i32::<LittleEndian>()? as usize;
    let _tile_size = cursor.read_i32::<LittleEndian>()?;
    let _compression_type = cursor.read_i32::<LittleEndian>()?;

    let mut tiles = Vec::with_capacity(num_tiles);
    let mut tile_buf = [0u8; TILE_BYTES];

    for _ in 0..num_tiles {
        cursor.read_exact(&mut tile_buf)?;
        let rgba = decode_dxt1_block_image(&tile_buf[..TILE_BASE_BYTES], TILE_PX, TILE_PX);
        tiles.push(rgba);
    }

    Ok(tiles)
}

/// Assemble the full ground texture from tiles and tilemap.
///
/// Returns `(width, height, rgba_pixels)`.
pub fn assemble_ground_texture(
    tiles: &[[u8; TILE_PX * TILE_PX * 4]],
    tilemap: &[u32],
    tm_w: usize,
    tm_h: usize,
) -> (usize, usize, Vec<u8>) {
    let tex_w = tm_w * TILE_PX;
    let tex_h = tm_h * TILE_PX;
    let mut pixels = vec![0u8; tex_w * tex_h * 4];

    for ty in 0..tm_h {
        for tx in 0..tm_w {
            let tile_idx = tilemap[ty * tm_w + tx] as usize;
            if tile_idx >= tiles.len() {
                continue;
            }
            let tile = &tiles[tile_idx];

            let dst_x = tx * TILE_PX;
            let dst_y = ty * TILE_PX;
            for row in 0..TILE_PX {
                let src_offset = row * TILE_PX * 4;
                let dst_offset = ((dst_y + row) * tex_w + dst_x) * 4;
                pixels[dst_offset..dst_offset + TILE_PX * 4]
                    .copy_from_slice(&tile[src_offset..src_offset + TILE_PX * 4]);
            }
        }
    }

    (tex_w, tex_h, pixels)
}

fn decode_dxt1_block_image(
    data: &[u8],
    width: usize,
    height: usize,
) -> [u8; TILE_PX * TILE_PX * 4] {
    let mut output = [255u8; TILE_PX * TILE_PX * 4];
    let blocks_x = width / 4;
    let blocks_y = height / 4;

    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let block_offset = (by * blocks_x + bx) * 8;
            let block = &data[block_offset..block_offset + 8];
            decode_dxt1_block(block, &mut output, width, bx * 4, by * 4);
        }
    }

    output
}

fn decode_dxt1_block(block: &[u8], output: &mut [u8], stride: usize, px: usize, py: usize) {
    let c0_raw = u16::from_le_bytes([block[0], block[1]]);
    let c1_raw = u16::from_le_bytes([block[2], block[3]]);

    let c0 = rgb565_to_rgba(c0_raw);
    let c1 = rgb565_to_rgba(c1_raw);

    let palette = if c0_raw > c1_raw {
        [c0, c1, lerp_color(c0, c1, 2, 1), lerp_color(c0, c1, 1, 2)]
    } else {
        [c0, c1, lerp_color(c0, c1, 1, 1), [0, 0, 0, 0]]
    };

    let lookup = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);

    for row in 0..4 {
        for col in 0..4 {
            let bit_index = (row * 4 + col) * 2;
            let idx = ((lookup >> bit_index) & 0x3) as usize;
            let offset = ((py + row) * stride + px + col) * 4;
            output[offset..offset + 4].copy_from_slice(&palette[idx]);
        }
    }
}

fn rgb565_to_rgba(c: u16) -> [u8; 4] {
    let r5 = ((c >> 11) & 0x1F) as u8;
    let g6 = ((c >> 5) & 0x3F) as u8;
    let b5 = (c & 0x1F) as u8;
    [
        (r5 << 3) | (r5 >> 2),
        (g6 << 2) | (g6 >> 4),
        (b5 << 3) | (b5 >> 2),
        255,
    ]
}

fn lerp_color(a: [u8; 4], b: [u8; 4], w0: u16, w1: u16) -> [u8; 4] {
    let total = w0 + w1;
    [
        ((a[0] as u16 * w0 + b[0] as u16 * w1) / total) as u8,
        ((a[1] as u16 * w0 + b[1] as u16 * w1) / total) as u8,
        ((a[2] as u16 * w0 + b[2] as u16 * w1) / total) as u8,
        255,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb565_white() {
        assert_eq!(rgb565_to_rgba(0xFFFF), [255, 255, 255, 255]);
    }

    #[test]
    fn rgb565_black() {
        assert_eq!(rgb565_to_rgba(0x0000), [0, 0, 0, 255]);
    }

    #[test]
    fn load_marble_madness_tiles() {
        let sd7_path = [
            "kernel-panic/assets/maps/Marble_Madness_Map.sd7",
            "assets/maps/Marble_Madness_Map.sd7",
        ]
        .iter()
        .map(std::path::Path::new)
        .find(|p| p.exists());
        let Some(sd7_path) = sd7_path else {
            eprintln!("Skipping: map not found");
            return;
        };

        let extracted = crate::sd7_archive::load_map_archive(sd7_path).unwrap();
        let smf_header = crate::smf_parser::parse_smf(&extracted.smf_data).unwrap();

        let (tm_w, tm_h, tilemap) = parse_tilemap(&extracted.smf_data, &smf_header.header).unwrap();
        assert_eq!(tm_w, 64);
        assert_eq!(tm_h, 64);

        let smt_data = crate::sd7_archive::load_smt_from_archive(sd7_path).unwrap();
        let tiles = parse_smt_tiles(&smt_data).unwrap();
        assert!(!tiles.is_empty());

        let (tex_w, tex_h, pixels) = assemble_ground_texture(&tiles, &tilemap, tm_w, tm_h);
        assert_eq!(tex_w, 2048);
        assert_eq!(tex_h, 2048);
        assert_eq!(pixels.len(), 2048 * 2048 * 4);
    }
}
