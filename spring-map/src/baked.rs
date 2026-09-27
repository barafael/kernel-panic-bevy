//! Pre-baked map format.
//!
//! A `.kpmap` file is the result of running `bake-map` on a Spring
//! `.sd7` / `.sdz`: the archive is unpacked, Lua heightmap gadgets are
//! applied, SMT tiles are decoded, and the final terrain + texture +
//! metadata is serialized into a single deterministic blob. The game
//! can then load it with no archive / Lua / image dependencies, which
//! both cuts cold-start time and is a prerequisite for the WASM target
//! (plan §8.1) — `sevenz-rust` and `mlua` don't compile to wasm32.
//!
//! Format (all little-endian):
//!
//! ```text
//! magic        : 8 bytes
//!   kpmapv1\0  = body is the raw postcard payload
//!   kpmapv2\0  = body is DEFLATE(postcard payload) — the raw payload
//!                is dominated by solid-colour textures, so v2 shrinks
//!                a 270 MB v1 blob to ~5 MB and is what ships over
//!                HTTP for the web build
//!   kpmapv3\0  = body is ZSTD(postcard payload). Measured on the
//!                shipped maps, zstd beats the deflate stream by 35-94%
//!                at decode-parity (the payloads are long solid-colour
//!                texture runs that zstd's longer matches crush):
//!                Data_Cache_L1 314→49 KB, Memory_Bank_v3 5.3→0.32 MB,
//!                Hex_Farm_8 19.7→11.1 MB. Decode via ruzstd runs at
//!                220-610 MB/s native — parity with miniz inflate.
//!   kpmapv4\0  = ZSTD(postcard(BakedMap, Option<LuaCompositing>)):
//!                v3's body followed by the Lua-composited map data
//!                (Hex Farm's skin atlas) the runtime needs to draw
//!                the gadget's towers and bridges. Postcard isn't
//!                self-describing, so the extra trailing field needs
//!                the version bump; v1-v3 bodies still decode as a bare
//!                `BakedMap` with no compositing.
//! body_len     : u32      = body length in bytes (post-decode for v1,
//!                          compressed for v2/v3)
//! body         : [u8; N]  = postcard(BakedMap), deflated for v2
//! ```
//!
//! The magic + body_len header lets future format versions detect this
//! one without needing to fall back through schema-evolution rules.
//! Bumping the version number means the reader rejects mismatched
//! files instead of silently corrupting them.

use std::io::{Read, Write};

use serde::de::{Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::map_types::{GroundTexture, MapFeature, ParsedMap, SmfHeader, SmfParseError};
use crate::smd_parser::MapInfo;
use crate::{LuaCompositing, SpringMap};

const MAGIC_V1: &[u8; 8] = b"kpmapv1\0";
const MAGIC_V2: &[u8; 8] = b"kpmapv2\0";
const MAGIC_V3: &[u8; 8] = b"kpmapv3\0";
const MAGIC_V4: &[u8; 8] = b"kpmapv4\0";
/// Current writer version: v4 zstd-encodes the postcard body (level 19
/// at bake time — bake is native and offline, so the slowest-but-smallest
/// setting is free at load) and appends the Lua compositing data.
const MAGIC: &[u8; 8] = MAGIC_V4;

/// Body codec, keyed off the file magic.
#[derive(Clone, Copy)]
enum Codec {
    Raw,
    Deflate,
    Zstd,
}

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
struct BakedMap<'a> {
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
    #[serde(borrow)]
    ground_texture: Option<BakedTexture<'a>>,
}

#[derive(Serialize, Deserialize)]
struct BakedTexture<'a> {
    width: u32,
    height: u32,
    #[serde(borrow)]
    pixels: Bytes<'a>,
}

/// A byte run borrowed from the decoded payload.
///
/// Wire-identical to the `Vec<u8>` earlier bakes wrote (postcard encodes
/// both as a varint length followed by the raw bytes), but decoded as a
/// slice of the payload instead of an owned copy: `Vec<u8>` goes through
/// serde's generic sequence path, one `visit_u8` per pixel byte — over a
/// second for Memory_Bank_v3's 268 MB texture — and doubles the peak
/// memory while the copy is made. The reader then moves the pixels out
/// of the payload in place (see [`read_baked_map`]).
struct Bytes<'a>(&'a [u8]);

impl Serialize for Bytes<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(self.0)
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for Bytes<'a> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct BytesVisitor;
        impl<'de> Visitor<'de> for BytesVisitor {
            type Value = &'de [u8];
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a borrowed byte run")
            }
            fn visit_borrowed_bytes<E: serde::de::Error>(
                self,
                v: &'de [u8],
            ) -> Result<Self::Value, E> {
                Ok(v)
            }
        }
        deserializer.deserialize_bytes(BytesVisitor).map(Bytes)
    }
}

/// Encode a zstd frame. The encoder (C zstd) only links on native —
/// baking is a native-only job; wasm builds just need the reader.
///
/// The frame carries its decompressed size (`Frame_Content_Size`), so
/// the reader can allocate the payload buffer exactly once instead of
/// growing it by doubling — a quarter-gigabyte texture otherwise ends
/// up in a half-gigabyte allocation.
#[cfg(not(target_arch = "wasm32"))]
fn encode_zstd(body: &[u8]) -> Result<Vec<u8>, BakedMapError> {
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 19)?;
    encoder.set_pledged_src_size(Some(body.len() as u64))?;
    encoder.include_contentsize(true)?;
    encoder.write_all(body)?;
    Ok(encoder.finish()?)
}

/// wasm stub: `write_baked_map` is only called by the native bake bin.
#[cfg(target_arch = "wasm32")]
fn encode_zstd(_body: &[u8]) -> Result<Vec<u8>, BakedMapError> {
    Err(BakedMapError::Io(std::io::Error::other(
        "kpmap baking requires the native target",
    )))
}

/// Serialize `map` to the `.kpmap` wire format.
pub fn write_baked_map(map: &SpringMap) -> Result<Vec<u8>, BakedMapError> {
    let core = BakedMap {
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
            pixels: Bytes(&g.pixels),
        }),
    };

    // v4: the v3 body with the Lua compositing data appended (a postcard
    // tuple is the plain concatenation of its fields).
    let body = postcard::to_allocvec(&(core, &map.lua_compositing))
        .map_err(BakedMapError::PostcardEncode)?;

    // v3: zstd the body at level 19. The payload is dominated by
    // solid-colour textures and zeroed maps; zstd's longer matches and
    // stronger entropy coding beat the v2 deflate stream by 35-94%.
    let compressed = encode_zstd(&body)?;

    let body_len: u32 = compressed
        .len()
        .try_into()
        .expect("deflated body exceeds u32::MAX");

    let mut out = Vec::with_capacity(MAGIC.len() + 4 + compressed.len());
    out.write_all(MAGIC)?;
    out.write_all(&body_len.to_le_bytes())?;
    out.write_all(&compressed)?;
    Ok(out)
}

/// Deserialize a `.kpmap` blob back into the same shape `load_map`
/// returns for a `.sd7` — the rest of the engine doesn't need to know
/// which path the data took. Accepts v1 (raw body) and v2 (deflated).
///
/// The ground texture dominates the payload (a quarter gigabyte on the
/// biggest maps), so it is never copied out: the pixels are shifted to
/// the front of the payload buffer, which becomes the texture's own
/// `Vec` — peak memory is one payload, not payload + copy. A
/// Lua-composited map (v4 with compositing data) gets no ground texture
/// at all: its SMT is hidden under `voidGround` and never drawn, so
/// materializing it would only cost memory.
pub fn read_baked_map(bytes: &[u8]) -> Result<SpringMap, BakedMapError> {
    let magic = bytes
        .get(..MAGIC.len())
        .ok_or(BakedMapError::HeaderTruncated)?;
    let codec = if magic == MAGIC_V1.as_slice() {
        Codec::Raw
    } else if magic == MAGIC_V2.as_slice() {
        Codec::Deflate
    } else if magic == MAGIC_V3.as_slice() || magic == MAGIC_V4.as_slice() {
        Codec::Zstd
    } else {
        return Err(BakedMapError::BadMagic);
    };

    if bytes.len() < MAGIC.len() + 4 {
        return Err(BakedMapError::HeaderTruncated);
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
    let stored = &body[..body_len];

    // v1 stores the postcard payload raw; v2 deflates it; v3 zstds it.
    let payload: Vec<u8> = match codec {
        Codec::Raw => stored.to_vec(),
        Codec::Deflate => {
            let mut decoder = flate2::read::DeflateDecoder::new(stored);
            let mut decoded = Vec::new();
            decoder.read_to_end(&mut decoded)?;
            decoded
        }
        Codec::Zstd => {
            let mut decoder = ruzstd::decoding::StreamingDecoder::new(stored)
                .map_err(|e| BakedMapError::Io(std::io::Error::other(e.to_string())))?;
            // Exact when the bake pledged the size (0 for older bakes,
            // which then grow the buffer as before).
            let content_size = decoder.decoder.content_size() as usize;
            let mut decoded = Vec::with_capacity(content_size);
            decoder.read_to_end(&mut decoded)?;
            decoded
        }
    };

    let (baked, lua_compositing): (BakedMap<'_>, Option<LuaCompositing>) =
        if magic == MAGIC_V4.as_slice() {
            postcard::from_bytes(&payload).map_err(BakedMapError::PostcardDecode)?
        } else {
            let core = postcard::from_bytes(&payload).map_err(BakedMapError::PostcardDecode)?;
            (core, None)
        };
    let BakedMap {
        map_x,
        map_y,
        min_height,
        max_height,
        heights,
        metalmap,
        features,
        map_info,
        ground_texture,
    } = baked;

    // Validate texture byte count before constructing GroundTexture so a
    // corrupt file fails loudly instead of producing a silently malformed
    // image asset later. Only the pixel slice still borrows `payload`:
    // note where it sits, then reuse the payload buffer as the pixel
    // buffer (an in-place shift, no second allocation).
    let texture_span = ground_texture
        .map(|t| {
            let expected = t.width as usize * t.height as usize * 4;
            if t.pixels.0.len() != expected {
                return Err(BakedMapError::TextureSizeMismatch {
                    expected,
                    actual: t.pixels.0.len(),
                });
            }
            let offset = t.pixels.0.as_ptr() as usize - payload.as_ptr() as usize;
            Ok((t.width as usize, t.height as usize, offset, expected))
        })
        .transpose()?;
    let ground_texture = match texture_span {
        Some((width, height, offset, len)) if lua_compositing.is_none() => {
            debug_assert!(offset + len <= payload.len());
            let mut pixels = payload;
            pixels.drain(..offset);
            pixels.truncate(len);
            Some(GroundTexture {
                width,
                height,
                pixels,
            })
        }
        _ => None,
    };

    let header = SmfHeader::new_flat(map_x, map_y, min_height, max_height);
    let expected_heights = header.heightmap_len();
    if heights.len() != expected_heights {
        return Err(BakedMapError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            SmfParseError::HeightmapTruncated {
                expected: expected_heights,
                actual: heights.len(),
            }
            .to_string(),
        )));
    }

    Ok(SpringMap {
        parsed: ParsedMap {
            header,
            heights,
            features,
            metalmap,
        },
        ground_texture,
        map_info,
        // No raw .smf bytes round-trip: the only consumer that ever
        // looked at them was the SMT decoder, which already ran during
        // bake. Anyone needing this in future would add it here.
        smf_data: Vec::new(),
        // v4 carries it; older bakes predate it (re-bake Hex Farm to get
        // its towers and bridges).
        lua_compositing,
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
    fn roundtrip_preserves_lua_compositing() {
        use crate::lua_skin::SkinAtlas;
        let mut original = sample_map();
        original.lua_compositing = Some(LuaCompositing {
            atlas: SkinAtlas {
                width: 1,
                height: 1,
                pixels: vec![1, 2, 3, 255],
            },
        });
        let loaded = read_baked_map(&write_baked_map(&original).unwrap()).unwrap();
        let lua = loaded.lua_compositing.expect("v4 keeps the compositing");
        assert_eq!(lua.atlas.pixels, vec![1, 2, 3, 255]);
        // The composited map's SMT is hidden under `voidGround`: not
        // materialized.
        assert!(loaded.ground_texture.is_none());
    }

    /// The pixel run is moved out of the payload in place; the bytes
    /// that came after it (mapinfo, compositing) must not leak in.
    #[test]
    fn ground_texture_pixels_survive_the_in_place_move() {
        let mut original = sample_map();
        let pixels: Vec<u8> = (0..16u8).map(|i| i.wrapping_mul(37)).collect();
        original.ground_texture.as_mut().unwrap().pixels = pixels.clone();
        let loaded = read_baked_map(&write_baked_map(&original).unwrap()).unwrap();
        let g = loaded.ground_texture.unwrap();
        assert_eq!((g.width, g.height), (2, 2));
        assert_eq!(g.pixels, pixels);
        assert_eq!(loaded.map_info.unwrap().gravity, 130.0);
    }

    /// Pre-v4 bakes (no trailing compositing field) must still load.
    #[test]
    fn reads_v3_body() {
        let map = sample_map();
        let core = BakedMap {
            map_x: map.parsed.header.map_x,
            map_y: map.parsed.header.map_y,
            min_height: map.parsed.header.min_height,
            max_height: map.parsed.header.max_height,
            heights: map.parsed.heights.clone(),
            metalmap: map.parsed.metalmap.clone(),
            features: map.parsed.features.clone(),
            map_info: map.map_info.clone(),
            ground_texture: None,
        };
        let body = encode_zstd(&postcard::to_allocvec(&core).unwrap()).unwrap();
        let mut bytes = MAGIC_V3.to_vec();
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&body);
        let loaded = read_baked_map(&bytes).unwrap();
        assert_eq!(loaded.parsed.heights, map.parsed.heights);
        assert!(loaded.lua_compositing.is_none());
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
