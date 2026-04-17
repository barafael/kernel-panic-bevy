//! Minimal DXT1 (BC1) encoder for generating SMT tiles and the SMF minimap.
//!
//! This is a simple encoder that produces valid DXT1 data. It uses a basic
//! bounding-box approach for endpoint selection — not optimal for visual quality,
//! but sufficient for test maps.

/// Encode an RGBA8 image into DXT1 compressed data.
///
/// `width` and `height` must both be multiples of 4.
/// Returns the compressed DXT1 bytes.
pub fn encode_dxt1(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    assert!(
        width.is_multiple_of(4) && height.is_multiple_of(4),
        "dimensions must be multiples of 4"
    );
    assert_eq!(rgba.len(), width * height * 4);

    let blocks_x = width / 4;
    let blocks_y = height / 4;
    let mut output = Vec::with_capacity(blocks_x * blocks_y * 8);

    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let mut block_pixels = [[0u8; 3]; 16];
            for row in 0..4 {
                for col in 0..4 {
                    let px = bx * 4 + col;
                    let py = by * 4 + row;
                    let offset = (py * width + px) * 4;
                    block_pixels[row * 4 + col] =
                        [rgba[offset], rgba[offset + 1], rgba[offset + 2]];
                }
            }
            encode_block(&block_pixels, &mut output);
        }
    }

    output
}

fn encode_block(pixels: &[[u8; 3]; 16], output: &mut Vec<u8>) {
    // Find bounding box of colors in RGB space.
    let mut min_r = 255u8;
    let mut min_g = 255u8;
    let mut min_b = 255u8;
    let mut max_r = 0u8;
    let mut max_g = 0u8;
    let mut max_b = 0u8;

    for &[r, g, b] in pixels {
        min_r = min_r.min(r);
        min_g = min_g.min(g);
        min_b = min_b.min(b);
        max_r = max_r.max(r);
        max_g = max_g.max(g);
        max_b = max_b.max(b);
    }

    let c0 = to_rgb565(max_r, max_g, max_b);
    let c1 = to_rgb565(min_r, min_g, min_b);

    // Ensure c0 > c1 for 4-color mode (no alpha).
    let (c0, c1) = if c0 > c1 {
        (c0, c1)
    } else if c0 < c1 {
        (c1, c0)
    } else {
        // All pixels are the same color — use 2-color mode is fine.
        (c0, c1)
    };

    output.extend_from_slice(&c0.to_le_bytes());
    output.extend_from_slice(&c1.to_le_bytes());

    // Build the 4-entry palette.
    let p0 = from_rgb565(c0);
    let p1 = from_rgb565(c1);
    let palette = if c0 > c1 {
        [p0, p1, lerp_color(p0, p1, 2, 1), lerp_color(p0, p1, 1, 2)]
    } else {
        [
            p0,
            p1,
            lerp_color(p0, p1, 1, 1),
            [0, 0, 0], // transparent black, but we won't use it
        ]
    };

    // For each pixel, find the closest palette entry.
    let mut lookup = 0u32;
    for (i, &pixel) in pixels.iter().enumerate() {
        let mut best_idx = 0u32;
        let mut best_dist = u32::MAX;
        let max_entries = if c0 > c1 { 4 } else { 3 };
        for (j, &pal) in palette.iter().enumerate().take(max_entries) {
            let dr = pixel[0] as i32 - pal[0] as i32;
            let dg = pixel[1] as i32 - pal[1] as i32;
            let db = pixel[2] as i32 - pal[2] as i32;
            let dist = (dr * dr + dg * dg + db * db) as u32;
            if dist < best_dist {
                best_dist = dist;
                best_idx = j as u32;
            }
        }
        lookup |= best_idx << (i * 2);
    }

    output.extend_from_slice(&lookup.to_le_bytes());
}

fn to_rgb565(r: u8, g: u8, b: u8) -> u16 {
    let r5 = (r >> 3) as u16;
    let g6 = (g >> 2) as u16;
    let b5 = (b >> 3) as u16;
    (r5 << 11) | (g6 << 5) | b5
}

fn from_rgb565(c: u16) -> [u8; 3] {
    let r5 = ((c >> 11) & 0x1F) as u8;
    let g6 = ((c >> 5) & 0x3F) as u8;
    let b5 = (c & 0x1F) as u8;
    [
        (r5 << 3) | (r5 >> 2),
        (g6 << 2) | (g6 >> 4),
        (b5 << 3) | (b5 >> 2),
    ]
}

fn lerp_color(a: [u8; 3], b: [u8; 3], w0: u16, w1: u16) -> [u8; 3] {
    let total = w0 + w1;
    [
        ((a[0] as u16 * w0 + b[0] as u16 * w1) / total) as u8,
        ((a[1] as u16 * w0 + b[1] as u16 * w1) / total) as u8,
        ((a[2] as u16 * w0 + b[2] as u16 * w1) / total) as u8,
    ]
}

/// Generate a DXT1-compressed mipmap chain for a square image.
/// Returns the concatenated DXT1 data for all mip levels.
pub fn encode_dxt1_with_mipmaps(rgba: &[u8], size: usize, mip_levels: usize) -> Vec<u8> {
    let mut result = Vec::new();
    let mut current_rgba = rgba.to_vec();
    let mut current_size = size;

    for _ in 0..mip_levels {
        let dxt1 = encode_dxt1(&current_rgba, current_size, current_size);
        result.extend_from_slice(&dxt1);

        if current_size <= 4 {
            break;
        }

        // Downsample 2x.
        let next_size = current_size / 2;
        let mut next_rgba = vec![0u8; next_size * next_size * 4];
        for y in 0..next_size {
            for x in 0..next_size {
                let sx = x * 2;
                let sy = y * 2;
                for c in 0..4 {
                    let avg = (current_rgba[(sy * current_size + sx) * 4 + c] as u16
                        + current_rgba[(sy * current_size + sx + 1) * 4 + c] as u16
                        + current_rgba[((sy + 1) * current_size + sx) * 4 + c] as u16
                        + current_rgba[((sy + 1) * current_size + sx + 1) * 4 + c] as u16)
                        / 4;
                    next_rgba[(y * next_size + x) * 4 + c] = avg as u8;
                }
            }
        }

        current_rgba = next_rgba;
        current_size = next_size;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_solid_black_4x4() {
        let rgba = vec![0u8; 4 * 4 * 4];
        let dxt1 = encode_dxt1(&rgba, 4, 4);
        assert_eq!(dxt1.len(), 8); // One block = 8 bytes.
    }

    #[test]
    fn encode_solid_white_4x4() {
        let rgba = vec![255u8; 4 * 4 * 4];
        let dxt1 = encode_dxt1(&rgba, 4, 4);
        assert_eq!(dxt1.len(), 8);
    }

    #[test]
    fn encode_32x32_tile() {
        let rgba = vec![128u8; 32 * 32 * 4];
        let dxt1 = encode_dxt1(&rgba, 32, 32);
        // 32/4 * 32/4 = 64 blocks * 8 bytes = 512 bytes (base level).
        assert_eq!(dxt1.len(), 512);
    }

    #[test]
    fn rgb565_roundtrip() {
        // Black.
        assert_eq!(from_rgb565(to_rgb565(0, 0, 0)), [0, 0, 0]);
        // White.
        assert_eq!(from_rgb565(to_rgb565(255, 255, 255)), [255, 255, 255]);
    }

    #[test]
    fn mipmap_chain_sizes() {
        let rgba = vec![0u8; 32 * 32 * 4];
        let data = encode_dxt1_with_mipmaps(&rgba, 32, 4);
        // 32x32 = 512, 16x16 = 128, 8x8 = 32, 4x4 = 8 => 680 bytes total.
        assert_eq!(data.len(), 680);
    }
}
