//! Minimal TGA (Targa) image parser.
//!
//! Supports uncompressed (type 2) and RLE-compressed (type 10) true-color
//! images at 24-bit (RGB) and 32-bit (RGBA) depth. This covers all texture
//! files shipped with Kernel Panic.

use thiserror::Error;

/// A decoded TGA image.
pub struct TgaImage {
    pub width: u32,
    pub height: u32,
    /// RGBA8 pixel data, row-major from top-left, `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum TgaParseError {
    #[error("TGA data too short for header (need 18 bytes, got {0})")]
    HeaderTruncated(usize),
    #[error("unsupported TGA image type {0} (expected 2=uncompressed or 10=RLE)")]
    UnsupportedImageType(u8),
    #[error("unsupported TGA bit depth {0} (expected 24 or 32)")]
    UnsupportedBitDepth(u8),
    #[error("TGA pixel data truncated")]
    PixelDataTruncated,
    #[error("TGA image has zero dimensions ({0}x{1})")]
    ZeroDimensions(u16, u16),
}

/// Parse a TGA file from raw bytes into RGBA8 pixel data.
pub fn parse_tga(data: &[u8]) -> Result<TgaImage, TgaParseError> {
    if data.len() < 18 {
        return Err(TgaParseError::HeaderTruncated(data.len()));
    }

    let id_length = data[0] as usize;
    let image_type = data[2];
    let width = u16::from_le_bytes([data[12], data[13]]);
    let height = u16::from_le_bytes([data[14], data[15]]);
    let bpp = data[16];
    let descriptor = data[17];

    if width == 0 || height == 0 {
        return Err(TgaParseError::ZeroDimensions(width, height));
    }

    let bytes_per_pixel = match bpp {
        24 => 3,
        32 => 4,
        other => return Err(TgaParseError::UnsupportedBitDepth(other)),
    };

    let pixel_data_start = 18 + id_length;
    let pixel_data = &data[pixel_data_start..];

    let w = width as usize;
    let h = height as usize;
    let pixel_count = w * h;

    let raw_pixels = match image_type {
        2 => decode_uncompressed(pixel_data, pixel_count, bytes_per_pixel)?,
        10 => decode_rle(pixel_data, pixel_count, bytes_per_pixel)?,
        other => return Err(TgaParseError::UnsupportedImageType(other)),
    };

    // Convert BGR(A) to RGBA.
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for pixel in raw_pixels.chunks_exact(bytes_per_pixel) {
        rgba.push(pixel[2]); // R (TGA stores BGR)
        rgba.push(pixel[1]); // G
        rgba.push(pixel[0]); // B
        rgba.push(if bytes_per_pixel == 4 { pixel[3] } else { 255 }); // A
    }

    // Handle origin: TGA default is bottom-left.
    //   * descriptor bit 4 (0x10) — pixels run right-to-left within a row.
    //   * descriptor bit 5 (0x20) — rows run top-to-bottom.
    // KP textures all use the bottom-left default, so the horizontal-flip
    // path is dormant for shipped assets but required for spec compliance.
    let origin_right = descriptor & 0x10 != 0;
    let origin_top = descriptor & 0x20 != 0;
    if origin_right {
        let row_bytes = w * 4;
        for y in 0..h {
            let row = &mut rgba[y * row_bytes..(y + 1) * row_bytes];
            for x in 0..w / 2 {
                let l = x * 4;
                let r = (w - 1 - x) * 4;
                for byte in 0..4 {
                    row.swap(l + byte, r + byte);
                }
            }
        }
    }
    if !origin_top {
        let row_bytes = w * 4;
        for y in 0..h / 2 {
            let (top_half, bot_half) = rgba.split_at_mut((h - 1 - y) * row_bytes);
            let top_row = &mut top_half[y * row_bytes..y * row_bytes + row_bytes];
            let bot_row = &mut bot_half[..row_bytes];
            top_row.swap_with_slice(bot_row);
        }
    }

    Ok(TgaImage {
        width: width as u32,
        height: height as u32,
        pixels: rgba,
    })
}

fn decode_uncompressed(
    data: &[u8],
    pixel_count: usize,
    bytes_per_pixel: usize,
) -> Result<Vec<u8>, TgaParseError> {
    let needed = pixel_count * bytes_per_pixel;
    if data.len() < needed {
        return Err(TgaParseError::PixelDataTruncated);
    }
    Ok(data[..needed].to_vec())
}

fn decode_rle(
    data: &[u8],
    pixel_count: usize,
    bytes_per_pixel: usize,
) -> Result<Vec<u8>, TgaParseError> {
    let mut out = Vec::with_capacity(pixel_count * bytes_per_pixel);
    let mut pos = 0;
    let mut pixels_decoded = 0;

    while pixels_decoded < pixel_count {
        if pos >= data.len() {
            return Err(TgaParseError::PixelDataTruncated);
        }

        let header = data[pos];
        pos += 1;
        let count = (header & 0x7F) as usize + 1;

        if header & 0x80 != 0 {
            // RLE packet: one pixel repeated `count` times.
            if pos + bytes_per_pixel > data.len() {
                return Err(TgaParseError::PixelDataTruncated);
            }
            let pixel = &data[pos..pos + bytes_per_pixel];
            pos += bytes_per_pixel;
            for _ in 0..count {
                out.extend_from_slice(pixel);
            }
        } else {
            // Raw packet: `count` individual pixels.
            let needed = count * bytes_per_pixel;
            if pos + needed > data.len() {
                return Err(TgaParseError::PixelDataTruncated);
            }
            out.extend_from_slice(&data[pos..pos + needed]);
            pos += needed;
        }

        pixels_decoded += count;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn textures_dir() -> Option<PathBuf> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir.parent().unwrap_or(&manifest_dir);

        [
            workspace_root.join("upstream/Kernel-Panic/unittextures"),
            PathBuf::from("upstream/Kernel-Panic/unittextures"),
        ]
        .into_iter()
        .find(|p| p.is_dir())
    }

    #[test]
    fn parse_uncompressed_rgba() {
        let Some(dir) = textures_dir() else {
            eprintln!("Skipping: textures directory not found");
            return;
        };
        let data = std::fs::read(dir.join("kernel.tga")).unwrap();
        let img = parse_tga(&data).unwrap();

        assert_eq!(img.width, 256);
        assert_eq!(img.height, 256);
        assert_eq!(img.pixels.len(), 256 * 256 * 4);

        // Spot-check: pixels should not be all-zero (texture has content).
        assert!(img.pixels.iter().any(|&b| b > 0));
    }

    #[test]
    fn parse_rle_compressed() {
        let Some(dir) = textures_dir() else {
            eprintln!("Skipping: textures directory not found");
            return;
        };
        // kernel_base.tga is RLE-compressed 24bpp.
        let data = std::fs::read(dir.join("kernel_base.tga")).unwrap();
        let img = parse_tga(&data).unwrap();

        assert_eq!(img.width, 256);
        assert_eq!(img.height, 256);
        assert_eq!(img.pixels.len(), 256 * 256 * 4);

        // All alpha bytes should be 255 (24bpp source has no alpha).
        for (i, &byte) in img.pixels.iter().enumerate() {
            if i % 4 == 3 {
                assert_eq!(byte, 255, "alpha at pixel {} should be 255", i / 4);
            }
        }
    }

    #[test]
    fn parse_small_texture() {
        let Some(dir) = textures_dir() else {
            eprintln!("Skipping: textures directory not found");
            return;
        };
        // solid_bright.tga is 8x8.
        let data = std::fs::read(dir.join("solid_bright.tga")).unwrap();
        let img = parse_tga(&data).unwrap();

        assert_eq!(img.width, 8);
        assert_eq!(img.height, 8);
        assert_eq!(img.pixels.len(), 8 * 8 * 4);
    }

    #[test]
    fn parse_all_textures() {
        let Some(dir) = textures_dir() else {
            eprintln!("Skipping: textures directory not found");
            return;
        };

        let mut count = 0;
        let mut failures: Vec<String> = Vec::new();

        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("tga") {
                continue;
            }
            let name = path.file_stem().unwrap_or_default().to_string_lossy();
            let data = std::fs::read(&path).unwrap();

            match parse_tga(&data) {
                Ok(img) => {
                    assert_eq!(
                        img.pixels.len(),
                        (img.width * img.height * 4) as usize,
                        "{name}: pixel count mismatch"
                    );
                    eprintln!("  OK: {name} — {}x{}", img.width, img.height);
                    count += 1;
                }
                Err(error) => {
                    failures.push(format!("{name}: {error}"));
                }
            }
        }

        if !failures.is_empty() {
            panic!(
                "{} texture(s) failed to parse:\n  {}",
                failures.len(),
                failures.join("\n  ")
            );
        }

        eprintln!("All {count} textures parsed successfully");
        assert!(count > 0, "expected at least one .tga texture");
    }

    #[test]
    fn reject_truncated() {
        assert!(matches!(
            parse_tga(&[0; 10]),
            Err(TgaParseError::HeaderTruncated(10))
        ));
    }

    #[test]
    fn reject_unsupported_type() {
        let mut header = [0u8; 18];
        header[2] = 3; // grayscale, unsupported
        header[12] = 1; // width=1
        header[15] = 1; // height=1
        header[16] = 24; // bpp
        assert!(matches!(
            parse_tga(&header),
            Err(TgaParseError::UnsupportedImageType(3))
        ));
    }

    /// Build a 2x2 32bpp uncompressed TGA with a custom descriptor byte and
    /// four distinct BGRA pixels, then verify the decoded pixel layout.
    fn make_2x2_tga(descriptor: u8) -> Vec<u8> {
        let mut buf = Vec::<u8>::new();
        buf.push(0); // id length
        buf.push(0); // color map type
        buf.push(2); // image type: uncompressed true-color
        buf.extend_from_slice(&[0; 5]); // color map spec
        buf.extend_from_slice(&0u16.to_le_bytes()); // x origin
        buf.extend_from_slice(&0u16.to_le_bytes()); // y origin
        buf.extend_from_slice(&2u16.to_le_bytes()); // width
        buf.extend_from_slice(&2u16.to_le_bytes()); // height
        buf.push(32); // bpp
        buf.push(descriptor);
        // BGRA pixels in file order. Label them by (row, col) to make the
        // expected post-flip layout easy to follow:
        //   row 0 (file): (0,0)=red, (0,1)=green
        //   row 1 (file): (1,0)=blue, (1,1)=white
        buf.extend_from_slice(&[0x00, 0x00, 0xFF, 0xFF]); // red
        buf.extend_from_slice(&[0x00, 0xFF, 0x00, 0xFF]); // green
        buf.extend_from_slice(&[0xFF, 0x00, 0x00, 0xFF]); // blue
        buf.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // white
        buf
    }

    fn pixels(img: &TgaImage) -> Vec<[u8; 4]> {
        img.pixels
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect()
    }

    #[test]
    fn descriptor_top_origin_no_flip() {
        // descriptor 0x20: top-origin, left-to-right.
        let img = parse_tga(&make_2x2_tga(0x20)).unwrap();
        let p = pixels(&img);
        // Output as-stored: row 0 = red, green; row 1 = blue, white.
        assert_eq!(p[0], [0xFF, 0x00, 0x00, 0xFF]); // red
        assert_eq!(p[1], [0x00, 0xFF, 0x00, 0xFF]); // green
        assert_eq!(p[2], [0x00, 0x00, 0xFF, 0xFF]); // blue
        assert_eq!(p[3], [0xFF, 0xFF, 0xFF, 0xFF]); // white
    }

    #[test]
    fn descriptor_bottom_origin_vertical_flip() {
        // descriptor 0x00: default bottom-origin → flip rows.
        let img = parse_tga(&make_2x2_tga(0x00)).unwrap();
        let p = pixels(&img);
        // After vertical flip: row 0 = blue, white; row 1 = red, green.
        assert_eq!(p[0], [0x00, 0x00, 0xFF, 0xFF]); // blue
        assert_eq!(p[1], [0xFF, 0xFF, 0xFF, 0xFF]); // white
        assert_eq!(p[2], [0xFF, 0x00, 0x00, 0xFF]); // red
        assert_eq!(p[3], [0x00, 0xFF, 0x00, 0xFF]); // green
    }

    #[test]
    fn descriptor_horizontal_flip_top_origin() {
        // descriptor 0x30: top-origin + right-to-left → flip columns only.
        let img = parse_tga(&make_2x2_tga(0x30)).unwrap();
        let p = pixels(&img);
        // After horizontal flip: row 0 = green, red; row 1 = white, blue.
        assert_eq!(p[0], [0x00, 0xFF, 0x00, 0xFF]); // green
        assert_eq!(p[1], [0xFF, 0x00, 0x00, 0xFF]); // red
        assert_eq!(p[2], [0xFF, 0xFF, 0xFF, 0xFF]); // white
        assert_eq!(p[3], [0x00, 0x00, 0xFF, 0xFF]); // blue
    }

    #[test]
    fn descriptor_both_flips_bottom_origin() {
        // descriptor 0x10: bottom-origin + right-to-left → both flips.
        let img = parse_tga(&make_2x2_tga(0x10)).unwrap();
        let p = pixels(&img);
        // After H-flip then V-flip: row 0 = white, blue; row 1 = green, red.
        assert_eq!(p[0], [0xFF, 0xFF, 0xFF, 0xFF]); // white
        assert_eq!(p[1], [0x00, 0x00, 0xFF, 0xFF]); // blue
        assert_eq!(p[2], [0x00, 0xFF, 0x00, 0xFF]); // green
        assert_eq!(p[3], [0xFF, 0x00, 0x00, 0xFF]); // red
    }
}
