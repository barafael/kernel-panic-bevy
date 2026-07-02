//! Pre-baked map format.
//!
//! A `.kpmap` file is the result of running [`bake_map`] on a Spring
//! `.sd7` / `.sdz`: the archive is unpacked, Lua heightmap gadgets are
//! applied, SMT tiles are decoded, and the final terrain + texture +
//! metadata is serialized into a single deterministic blob. The game
//! can then load it without sevenz / zip / mlua at runtime, which both
//! cuts cold-start time and is a prerequisite for the WASM target
//! (plan §8.1) — `sevenz-rust` and `mlua` don't compile to wasm32.
//!
//! Format (all little-endian, postcard-encoded body):
//!
//! ```text
//! magic        : 8 bytes  = b"kpmapv1\0"
//! body_len     : u32      = postcard-encoded body length in bytes
//! body         : [u8; N]  = postcard(BakedMap)
//! ```
//!
//! The magic + body_len header lets future format versions detect this
//! one without needing to fall back through schema-evolution rules.
//! Bumping the version number means the reader rejects mismatched
//! files instead of silently corrupting them.

use std::io::Write;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::SpringMap;
use crate::map_types::{GroundTexture, MapFeature, ParsedMap, SmfHeader, SmfParseError};
use crate::smd_parser::MapInfo;

const MAGIC: &[u8; 8] = b"kpmapv1\0";

#[derive(Debug, Error)]
pub enum BakedMapError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("postcard encode error: {0}")]
    PostcardEncode(postcard::Error),
    #[error("postcard decode error: {0}")]
    PostcardDecode(postcard::Error),
    #[error("not a kpmap file (bad magic)")]
    BadMagic,
    #[error("kpmap body truncated: declared {declared} bytes, file has {actual}")]
    Truncated { declared: usize, actual: usize },
    #[error("kpmap header truncated")]
    HeaderTruncated,
    #[error("ground texture pixel count mismatch: width*height*4 = {expected}, got {actual}")]
    TextureSizeMismatch { expected: usize, actual: usize },
}

/// On-disk payload. Versioned implicitly via the file's magic; new fields
/// must be `Option<…>` (or behind a version bump) so older readers fail
/// with the explicit `BadMagic` error rather than mis-decoding.
#[derive(Serialize, Deserialize)]
struct BakedMap {
    map_x: i32,
    map_y: i32,
    min_height: f32,
    max_height: f32,
    /// Row-major heightmap, world-space heights. Length is
    /// `(map_x + 1) * (map_y + 1)`.
    heights: Vec<f32>,
    /// `map_x/2 × map_y/2` bytes.
    metalmap: Vec<u8>,
    features: Vec<MapFeature>,
    map_info: Option<MapInfo>,
    /// Assembled ground texture. `None` if the source archive shipped no
    /// `.smt`. Pixels are RGBA8, row-major. Stored raw — PNG / DXT
    /// compression can come later when filesize matters (i.e. when we
    /// actually ship over HTTP for the WASM build).
    ground_texture: Option<BakedTexture>,
}

#[derive(Serialize, Deserialize)]
struct BakedTexture {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// Serialize `map` to the `.kpmap` wire format.
pub fn write_baked_map(map: &SpringMap) -> Result<Vec<u8>, BakedMapError> {
    let baked = BakedMap {
        map_x: map.parsed.header.map_x,
        map_y: map.parsed.header.map_y,
        min_height: map.parsed.header.min_height,
        max_height: map.parsed.header.max_height,
        heights: map.parsed.heights.clone(),
        metalmap: map.parsed.metalmap.clone(),
        features: map.parsed.features.clone(),
        map_info: map.map_info.clone(),
        ground_texture: map.ground_texture.as_ref().map(|g| BakedTexture {
            width: g.width as u32,
            height: g.height as u32,
            pixels: g.pixels.clone(),
        }),
    };

    let body = postcard::to_allocvec(&baked).map_err(BakedMapError::PostcardEncode)?;
    let body_len: u32 = body
        .len()
        .try_into()
        .expect("postcard body exceeds u32::MAX");

    let mut out = Vec::with_capacity(MAGIC.len() + 4 + body.len());
    out.write_all(MAGIC)?;
    out.write_all(&body_len.to_le_bytes())?;
    out.write_all(&body)?;
    Ok(out)
}

/// Deserialize a `.kpmap` blob back into the same shape `load_map`
/// returns for a `.sd7` — the rest of the engine doesn't need to know
/// which path the data took.
pub fn read_baked_map(bytes: &[u8]) -> Result<SpringMap, BakedMapError> {
    if bytes.len() < MAGIC.len() + 4 {
        return Err(BakedMapError::HeaderTruncated);
    }
    if &bytes[..MAGIC.len()] != MAGIC {
        return Err(BakedMapError::BadMagic);
    }

    let len_bytes: [u8; 4] = bytes[MAGIC.len()..MAGIC.len() + 4].try_into().unwrap();
    let body_len = u32::from_le_bytes(len_bytes) as usize;
    let body = &bytes[MAGIC.len() + 4..];
    if body.len() < body_len {
        return Err(BakedMapError::Truncated {
            declared: body_len,
            actual: body.len(),
        });
    }

    let baked: BakedMap =
        postcard::from_bytes(&body[..body_len]).map_err(BakedMapError::PostcardDecode)?;

    // Validate texture byte count before constructing GroundTexture so a
    // corrupt file fails loudly instead of producing a silently malformed
    // image asset later.
    let ground_texture = if let Some(t) = baked.ground_texture {
        let expected = t.width as usize * t.height as usize * 4;
        if t.pixels.len() != expected {
            return Err(BakedMapError::TextureSizeMismatch {
                expected,
                actual: t.pixels.len(),
            });
        }
        Some(GroundTexture {
            width: t.width as usize,
            height: t.height as usize,
            pixels: t.pixels,
        })
    } else {
        None
    };

    let header = SmfHeader::new_flat(baked.map_x, baked.map_y, baked.min_height, baked.max_height);
    let expected_heights = header.heightmap_len();
    if baked.heights.len() != expected_heights {
        return Err(BakedMapError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            SmfParseError::HeightmapTruncated {
                expected: expected_heights,
                actual: baked.heights.len(),
            }
            .to_string(),
        )));
    }

    Ok(SpringMap {
        parsed: ParsedMap {
            header,
            heights: baked.heights,
            features: baked.features,
            metalmap: baked.metalmap,
        },
        ground_texture,
        map_info: baked.map_info,
        // No raw .smf bytes round-trip: the only consumer that ever
        // looked at them was the SMT decoder, which already ran during
        // bake. Anyone needing this in future would add it here.
        smf_data: Vec::new(),
        // Baked maps don't carry the captured Lua layout — the bake
        // path runs before this work landed and tiles the skin into the
        // ground texture only. Re-bake to get hex meshes via .kpmap.
        lua_compositing: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_types::{FeatureType, ParsedMap, SmfHeader};
    use crate::smd_parser::{Atmosphere, Lighting, MapInfo, StartPosition};

    fn sample_map() -> SpringMap {
        let header = SmfHeader::new_flat(4, 4, 0.0, 100.0);
        let heights = vec![42.0; header.heightmap_len()];
        let metalmap = vec![0u8; header.metalmap_width() * header.metalmap_height()];
        SpringMap {
            parsed: ParsedMap {
                header,
                heights,
                features: vec![MapFeature::new(
                    FeatureType::GeoVent,
                    100.0,
                    0.0,
                    100.0,
                    0.0,
                    1.0,
                )],
                metalmap,
            },
            ground_texture: Some(GroundTexture {
                width: 2,
                height: 2,
                pixels: vec![255; 16],
            }),
            map_info: Some(MapInfo {
                description: "test".into(),
                gravity: 130.0,
                start_positions: vec![StartPosition {
                    team: 0,
                    x: 16.0,
                    z: 16.0,
                }],
                atmosphere: Atmosphere::default(),
                lighting: Lighting::default(),
            }),
            smf_data: Vec::new(),
            lua_compositing: None,
        }
    }

    #[test]
    fn roundtrip_preserves_all_fields() {
        let original = sample_map();
        let bytes = write_baked_map(&original).unwrap();
        let loaded = read_baked_map(&bytes).unwrap();

        assert_eq!(loaded.parsed.header.map_x, original.parsed.header.map_x);
        assert_eq!(loaded.parsed.heights, original.parsed.heights);
        assert_eq!(loaded.parsed.metalmap, original.parsed.metalmap);
        assert_eq!(loaded.parsed.features.len(), 1);
        assert_eq!(loaded.parsed.features[0].feature_type, FeatureType::GeoVent);

        let g = loaded.ground_texture.as_ref().unwrap();
        assert_eq!(g.width, 2);
        assert_eq!(g.pixels.len(), 16);

        let info = loaded.map_info.as_ref().unwrap();
        assert_eq!(info.gravity, 130.0);
        assert_eq!(info.start_positions.len(), 1);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = write_baked_map(&sample_map()).unwrap();
        bytes[0] = b'x';
        assert!(matches!(
            read_baked_map(&bytes),
            Err(BakedMapError::BadMagic)
        ));
    }

    #[test]
    fn rejects_truncated_header() {
        assert!(matches!(
            read_baked_map(&[]),
            Err(BakedMapError::HeaderTruncated)
        ));
        assert!(matches!(
            read_baked_map(&MAGIC[..4]),
            Err(BakedMapError::HeaderTruncated)
        ));
    }

    #[test]
    fn rejects_truncated_body() {
        let bytes = write_baked_map(&sample_map()).unwrap();
        let truncated = &bytes[..MAGIC.len() + 4 + 8];
        assert!(matches!(
            read_baked_map(truncated),
            Err(BakedMapError::Truncated { .. })
        ));
    }
}
