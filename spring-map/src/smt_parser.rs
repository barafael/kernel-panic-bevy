use std::io::{Cursor, Read, Seek, SeekFrom};
use std::num::NonZeroU16;

use binrw::BinRead;
use byteorder::{LittleEndian, ReadBytesExt};

use crate::map_types::{GroundTexture, ParsedMap, SmfParseError, SmtParseError, Tile, TileMap};

const TILE_BYTES: usize = 680;
const TILE_BASE_BYTES: usize = 512;

/// 32-byte SMT file header.
#[derive(Debug, Clone, BinRead)]
#[br(little, magic = b"spring tilefile\0")]
struct SmtHeader {
    _version: i32,
    num_tiles: i32,
    _tile_size: i32,
    _compression_type: i32,
}

/// Parse the tilemap from SMF data using the already-parsed header.
pub(crate) fn parse_tilemap(smf_data: &[u8], map: &ParsedMap) -> Result<TileMap, SmfParseError> {
    let mut cursor = Cursor::new(smf_data);
    cursor.seek(SeekFrom::Start(map.header.tiles_ptr as u64))?;

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

    let width = (map.header.map_x / 4) as usize;
    let height = (map.header.map_y / 4) as usize;
    let count = width * height;
    let mut indices = Vec::with_capacity(count);
    for _ in 0..count {
        indices.push(cursor.read_u32::<LittleEndian>()?);
    }

    Ok(TileMap {
        width,
        height,
        indices,
    })
}

/// Parse all tiles from an SMT file's raw bytes.
pub fn parse_smt_tiles(smt_data: &[u8]) -> Result<Vec<Tile>, SmtParseError> {
    let mut cursor = Cursor::new(smt_data);

    let header = SmtHeader::read(&mut cursor).map_err(|err| match err {
        binrw::Error::BadMagic { .. } => SmtParseError::BadMagic,
        binrw::Error::Io(io) => SmtParseError::Io(io),
        other => SmtParseError::Io(std::io::Error::other(other.to_string())),
    })?;
    let num_tiles = usize::try_from(header.num_tiles).unwrap_or(0);

    let mut tiles = Vec::with_capacity(num_tiles);
    let mut tile_buf = [0u8; TILE_BYTES];

    for _ in 0..num_tiles {
        cursor.read_exact(&mut tile_buf)?;
        let pixels =
            decode_dxt1_block_image(&tile_buf[..TILE_BASE_BYTES], Tile::WIDTH, Tile::HEIGHT);
        tiles.push(Tile { pixels });
    }

    Ok(tiles)
}

/// Assemble the full ground texture from tiles and a parsed map.
pub fn assemble_ground_texture(
    smf_data: &[u8],
    map: &ParsedMap,
    tiles: &[Tile],
) -> Result<GroundTexture, SmfParseError> {
    let tilemap = parse_tilemap(smf_data, map)?;

    let tex_w = tilemap.width * Tile::WIDTH;
    let tex_h = tilemap.height * Tile::HEIGHT;
    let mut pixels = vec![0u8; tex_w * tex_h * 4];

    for tile_y in 0..tilemap.height {
        for tile_x in 0..tilemap.width {
            let tile_idx = tilemap.get(tile_x, tile_y) as usize;
            if tile_idx >= tiles.len() {
                continue;
            }
            let tile = &tiles[tile_idx];

            let dst_x = tile_x * Tile::WIDTH;
            let dst_y = tile_y * Tile::HEIGHT;
            for row in 0..Tile::HEIGHT {
                let src_offset = row * Tile::WIDTH * 4;
                let dst_offset = ((dst_y + row) * tex_w + dst_x) * 4;
                pixels[dst_offset..dst_offset + Tile::WIDTH * 4]
                    .copy_from_slice(&tile.pixels[src_offset..src_offset + Tile::WIDTH * 4]);
            }
        }
    }

    Ok(GroundTexture {
        width: tex_w,
        height: tex_h,
        pixels,
    })
}

fn decode_dxt1_block_image(data: &[u8], width: usize, height: usize) -> [u8; Tile::SIZE] {
    let mut output = [255u8; Tile::SIZE];
    let blocks_x = width / 4;
    let blocks_y = height / 4;

    for block_y in 0..blocks_y {
        for block_x in 0..blocks_x {
            let block_offset = (block_y * blocks_x + block_x) * 8;
            let Some(block) = data.get(block_offset..block_offset + 8) else {
                // Truncated tile data: leave remaining pixels as the default fill.
                return output;
            };
            decode_dxt1_block(block, &mut output, width, block_x * 4, block_y * 4);
        }
    }

    output
}

fn decode_dxt1_block(block: &[u8], output: &mut [u8], stride: usize, px: usize, py: usize) {
    let c0_raw = u16::from_le_bytes([block[0], block[1]]);
    let c1_raw = u16::from_le_bytes([block[2], block[3]]);

    let c0 = rgb565_to_rgba(c0_raw);
    let c1 = rgb565_to_rgba(c1_raw);

    // Weights are compile-time constants from the DXT1 spec, so `NonZeroU16::new(..).unwrap()`
    // is evaluated once and never fails.
    let two = NonZeroU16::new(2).unwrap();
    let one = NonZeroU16::new(1).unwrap();
    let palette = if c0_raw > c1_raw {
        [
            c0,
            c1,
            lerp_color(c0, c1, two, one),
            lerp_color(c0, c1, one, two),
        ]
    } else {
        [c0, c1, lerp_color(c0, c1, one, one), [0, 0, 0, 0]]
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

fn lerp_color(a: [u8; 4], b: [u8; 4], w0: NonZeroU16, w1: NonZeroU16) -> [u8; 4] {
    let w0 = w0.get();
    let w1 = w1.get();
    // Sum is non-zero because both inputs are non-zero; division is therefore safe.
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
        let parsed = crate::smf_parser::parse_smf(&extracted.smf_data).unwrap();

        let smt_data = extracted.smt_data.expect("should have SMT data");
        let tiles = parse_smt_tiles(&smt_data).unwrap();
        assert!(!tiles.is_empty());

        let ground = assemble_ground_texture(&extracted.smf_data, &parsed, &tiles).unwrap();
        assert_eq!(ground.width, 2048);
        assert_eq!(ground.height, 2048);
        assert_eq!(ground.pixels.len(), 2048 * 2048 * 4);
    }
}
