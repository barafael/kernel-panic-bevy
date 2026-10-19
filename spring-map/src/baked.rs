//! The `.kpmap` map format.
//!
//! Every shipped map is a `.kpmap`: the terrain, ground texture,
//! metadata, terrain types, datavent facings and (Hex Farm) the Lua
//! skin atlas in one deterministic blob, loaded with no archive, Lua or
//! image dependencies on native and web alike. The files were converted
//! once from the original Spring archives (the converter is in the git
//! history, see README.md); the game and every lockstep peer read the
//! same bytes, which the lobby and replays hash (`net::compat`). The
//! datavent facings can be edited in place ([`set_datavent_facings`],
//! feature `write`; the game's `KP_MAP_EDIT` editor).
//!
//! Format (all little-endian):
//!
//! ```text
//! magic    : 8 bytes  "kpmapv6\0"
//! body_len : u32      length of the zstd frame
//! body     : ZSTD(postcard((BakedMap, Option<LuaCompositing>, BakedTerrain)))
//! ```
//!
//! The payload is dominated by solid-colour textures, which zstd's long
//! matches crush (Memory_Bank_v3's 268 MB texture is 0.3 MB on disk);
//! ruzstd decodes at 220-610 MB/s. The version digit in the magic is
//! part of the format: postcard is not self-describing, so a changed
//! payload layout needs a new magic, and a file with another one is
//! refused instead of mis-decoded.

use std::io::Read;

use serde::de::{Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::map_info::MapInfo;
use crate::map_types::{
    BadMapSize, Datavent, GroundTexture, MapFeature, MapHeader, ParsedMap, TerrainTypes,
};
use crate::{LuaCompositing, SpringMap};

const MAGIC: &[u8; 8] = b"kpmapv6\0";

/// The trailing terrain section.
#[derive(Serialize, Deserialize)]
struct BakedTerrain {
    /// `map_x/2 × map_y/2` terrain-type bytes, or empty.
    typemap: Vec<u8>,
    types: TerrainTypes,
}

#[derive(Debug, Error)]
pub enum BakedMapError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("postcard decode error: {0}")]
    PostcardDecode(postcard::Error),
    #[error("not a kpmap file (bad magic; this build reads {})", String::from_utf8_lossy(&MAGIC[..7]))]
    BadMagic,
    #[error("kpmap body truncated: declared {declared} bytes, file has {actual}")]
    Truncated { declared: usize, actual: usize },
    #[error("kpmap header truncated")]
    HeaderTruncated,
    #[error(transparent)]
    BadMapSize(#[from] BadMapSize),
    #[error("heightmap has {actual} samples, the map size needs {expected}")]
    HeightmapSize { expected: usize, actual: usize },
    #[error("metalmap has {actual} bytes, the map size needs {expected}")]
    MetalmapSize { expected: usize, actual: usize },
    #[error("typemap has {actual} bytes, the map size needs {expected}")]
    TypemapSize { expected: usize, actual: usize },
    #[error("ground texture pixel count mismatch: width*height*4 = {expected}, got {actual}")]
    TextureSizeMismatch { expected: usize, actual: usize },
    #[error("{actual} datavent facings given, the map has {expected} datavents")]
    DataventCount { expected: usize, actual: usize },
    #[error("datavent facing {0} is not 0 (south), 1 (east), 2 (north) or 3 (west)")]
    BadFacing(u8),
}

/// On-disk payload.
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
    /// The ground texture, RGBA8 row-major. `None` for a Lua-drawn map
    /// (Hex Farm), whose ground is hidden.
    #[serde(borrow)]
    ground_texture: Option<BakedTexture<'a>>,
    /// Every geovent with its factory facing (`crate::datavent`).
    datavents: Vec<Datavent>,
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
/// Wire-identical to a `Vec<u8>` (postcard encodes both as a varint
/// length followed by the raw bytes), but decoded as a slice of the
/// payload instead of an owned copy: `Vec<u8>` goes through serde's
/// generic sequence path, one `visit_u8` per pixel byte — over a second
/// for Memory_Bank_v3's 268 MB texture — and doubles the peak memory
/// while the copy is made. The reader then moves the pixels out of the
/// payload in place (see [`read_baked_map`]).
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

/// Serialize `map` to the `.kpmap` wire format: the reader's round-trip
/// tests. (The shipped files were written by the retired converter with
/// this same encoding, zstd level 19.)
#[cfg(test)]
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
        datavents: map.datavents.clone(),
        ground_texture: map.ground_texture.as_ref().map(|g| BakedTexture {
            width: g.width as u32,
            height: g.height as u32,
            pixels: Bytes(&g.pixels),
        }),
    };
    let terrain = BakedTerrain {
        typemap: map.parsed.typemap.clone(),
        types: map.terrain_types.clone(),
    };
    // A postcard tuple is the plain concatenation of its fields.
    let body = postcard::to_allocvec(&(core, &map.lua_compositing, terrain))
        .map_err(BakedMapError::PostcardDecode)?;
    encode_file(&body)
}

/// Wrap a postcard payload into the `.kpmap` container: magic, length,
/// zstd frame (level 19, as the shipped files).
#[cfg(any(test, feature = "write"))]
fn encode_file(body: &[u8]) -> Result<Vec<u8>, BakedMapError> {
    use std::io::Write;

    // The frame carries its decompressed size (`Frame_Content_Size`), so
    // the reader can allocate the payload buffer exactly once.
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 19)?;
    encoder.set_pledged_src_size(Some(body.len() as u64))?;
    encoder.include_contentsize(true)?;
    encoder.write_all(body)?;
    let compressed = encoder.finish()?;

    let body_len: u32 = compressed.len().try_into().expect("body exceeds u32::MAX");
    let mut out = Vec::with_capacity(MAGIC.len() + 4 + compressed.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&body_len.to_le_bytes());
    out.extend_from_slice(&compressed);
    Ok(out)
}

/// Rewrite the datavent facings of a `.kpmap` file (the datavent editor):
/// `facings[i]` replaces `datavents[i].facing`, in the file's order (the
/// order [`read_baked_map`] returns them in). Everything else in the
/// payload is decoded and re-encoded unchanged, so the new payload
/// differs from the old one only in those bytes.
#[cfg(any(test, feature = "write"))]
pub fn set_datavent_facings(file: &[u8], facings: &[u8]) -> Result<Vec<u8>, BakedMapError> {
    encode_file(&rewrite_facings(&decode_payload(file)?, facings)?)
}

/// [`set_datavent_facings`] on the decompressed payload.
#[cfg(any(test, feature = "write"))]
fn rewrite_facings(payload: &[u8], facings: &[u8]) -> Result<Vec<u8>, BakedMapError> {
    let (mut baked, lua_compositing, terrain): (
        BakedMap<'_>,
        Option<LuaCompositing>,
        BakedTerrain,
    ) = postcard::from_bytes(payload).map_err(BakedMapError::PostcardDecode)?;
    if facings.len() != baked.datavents.len() {
        return Err(BakedMapError::DataventCount {
            expected: baked.datavents.len(),
            actual: facings.len(),
        });
    }
    if let Some(&facing) = facings.iter().find(|&&f| f > 3) {
        return Err(BakedMapError::BadFacing(facing));
    }
    for (vent, &facing) in baked.datavents.iter_mut().zip(facings) {
        vent.facing = facing;
    }
    postcard::to_allocvec(&(baked, lua_compositing, terrain)).map_err(BakedMapError::PostcardDecode)
}

/// Largest buffer preallocated from a size claimed inside the file.
const MAX_PREALLOC: usize = 512 << 20;

/// Check the container and decompress the postcard payload.
fn decode_payload(bytes: &[u8]) -> Result<Vec<u8>, BakedMapError> {
    let magic = bytes
        .get(..MAGIC.len())
        .ok_or(BakedMapError::HeaderTruncated)?;
    if magic != MAGIC.as_slice() {
        return Err(BakedMapError::BadMagic);
    }

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

    let mut decoder = ruzstd::decoding::StreamingDecoder::new(stored)
        .map_err(|e| BakedMapError::Io(std::io::Error::other(e.to_string())))?;
    // Exact when the frame pledges the size (0 otherwise: the buffer
    // grows as it is read). Capped: the frame header's pledge is a
    // claim from the file; anything larger grows as it is read.
    let content_size = (decoder.decoder.content_size() as usize).min(MAX_PREALLOC);
    let mut payload = Vec::with_capacity(content_size);
    decoder.read_to_end(&mut payload)?;
    Ok(payload)
}

/// Decode a `.kpmap`.
///
/// The ground texture dominates the payload (a quarter gigabyte on the
/// biggest maps), so it is never copied out: the pixels are shifted to
/// the front of the payload buffer, which becomes the texture's own
/// `Vec` — peak memory is one payload, not payload + copy. A
/// Lua-composited map gets no ground texture at all: its ground is
/// hidden under `voidGround` and never drawn, so materializing it would
/// only cost memory.
pub fn read_baked_map(bytes: &[u8]) -> Result<SpringMap, BakedMapError> {
    let payload = decode_payload(bytes)?;
    let (baked, lua_compositing, terrain): (BakedMap<'_>, Option<LuaCompositing>, BakedTerrain) =
        postcard::from_bytes(&payload).map_err(BakedMapError::PostcardDecode)?;
    let BakedTerrain {
        typemap,
        types: terrain_types,
    } = terrain;
    let BakedMap {
        datavents,
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
            // A zero-extent texture would become a zero-sized GPU image
            // (fatal) and `checked_mul` keeps a bogus size from wrapping.
            let expected = (t.width as usize)
                .checked_mul(t.height as usize)
                .and_then(|n| n.checked_mul(4))
                .filter(|_| t.width > 0 && t.height > 0)
                .ok_or(BakedMapError::TextureSizeMismatch {
                    expected: 0,
                    actual: t.pixels.0.len(),
                })?;
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

    let header = MapHeader::new(map_x, map_y, min_height, max_height);
    header.validate_size()?;
    let expected = header.metalmap_width() * header.metalmap_height();
    if metalmap.len() != expected {
        return Err(BakedMapError::MetalmapSize {
            expected,
            actual: metalmap.len(),
        });
    }
    let expected = header.heightmap_len();
    if heights.len() != expected {
        return Err(BakedMapError::HeightmapSize {
            expected,
            actual: heights.len(),
        });
    }
    let expected = header.typemap_width() * header.typemap_height();
    if !typemap.is_empty() && typemap.len() != expected {
        return Err(BakedMapError::TypemapSize {
            expected,
            actual: typemap.len(),
        });
    }

    Ok(SpringMap {
        parsed: ParsedMap {
            header,
            heights,
            features,
            metalmap,
            typemap,
        },
        ground_texture,
        map_info,
        terrain_types,
        datavents,
        lua_compositing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_info::{Atmosphere, Lighting, MapInfo, StartPosition};
    use crate::map_types::{FeatureType, MapHeader, ParsedMap};

    fn sample_map() -> SpringMap {
        let header = MapHeader::new(4, 4, 0.0, 100.0);
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
                typemap: vec![0, 1, 2, 3],
            },
            datavents: vec![Datavent {
                x: 100.0,
                z: 100.0,
                facing: 1,
            }],
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
            terrain_types: {
                let mut types = TerrainTypes::default();
                types.move_speeds[0].kbot = 0.0;
                types.move_speeds[3].kbot = 4.0;
                types
            },
            lua_compositing: None,
        }
    }

    /// Every shipped map decodes and is sane: heightmap, metalmap and
    /// typemap sized to the header, finite features, a texture (or the
    /// Lua skin), start positions inside the map.
    #[test]
    fn shipped_maps_load() {
        let dir = crate::shipped_maps_dir();
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "kpmap") {
                continue;
            }
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let bytes = std::fs::read(&path).unwrap();
            let map = read_baked_map(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            let p = &map.parsed;
            assert_eq!(p.heights.len(), p.header.heightmap_len(), "{name}");
            assert_eq!(
                p.metalmap.len(),
                p.header.metalmap_width() * p.header.metalmap_height(),
                "{name}"
            );
            assert_eq!(
                p.typemap.len(),
                p.header.typemap_width() * p.header.typemap_height(),
                "{name}: typemap"
            );
            for f in &p.features {
                assert!(
                    f.x.is_finite() && f.y.is_finite() && f.z.is_finite(),
                    "{name}"
                );
            }
            match (&map.ground_texture, &map.lua_compositing) {
                (Some(ground), None) => {
                    assert_eq!(
                        ground.pixels.len(),
                        ground.width * ground.height * 4,
                        "{name}"
                    );
                }
                (None, Some(lua)) => {
                    let atlas = &lua.atlas;
                    assert_eq!(
                        atlas.pixels.len(),
                        (atlas.width * atlas.height * 4) as usize,
                        "{name}: atlas"
                    );
                }
                _ => panic!("{name}: neither a ground texture nor a Lua skin"),
            }
            let info = map
                .map_info
                .as_ref()
                .unwrap_or_else(|| panic!("{name}: no map info"));
            assert!(info.start_positions.len() >= 2, "{name}");
            let (w, d) = (p.header.world_width(), p.header.world_depth());
            for sp in &info.start_positions {
                assert!(
                    (0.0..=w).contains(&sp.x) && (0.0..=d).contains(&sp.z),
                    "{name}: start position team {} at ({}, {}) is outside {w}x{d}",
                    sp.team,
                    sp.x,
                    sp.z
                );
            }
            count += 1;
        }
        assert!(count >= 18, "expected the 18 shipped maps, found {count}");
    }

    #[test]
    fn roundtrip_preserves_terrain_types() {
        let original = sample_map();
        let loaded = read_baked_map(&write_baked_map(&original).unwrap()).unwrap();
        assert_eq!(loaded.parsed.typemap, vec![0, 1, 2, 3]);
        assert_eq!(loaded.terrain_types, original.terrain_types);
    }

    /// Another format version is refused by its magic, not mis-decoded.
    #[test]
    fn rejects_other_versions() {
        let bytes = write_baked_map(&sample_map()).unwrap();
        for version in *b"157x" {
            let mut other = bytes.clone();
            other[6] = version;
            assert!(matches!(
                read_baked_map(&other),
                Err(BakedMapError::BadMagic)
            ));
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
        assert_eq!(loaded.datavents, sample_map().datavents);

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
        let lua = loaded.lua_compositing.expect("the compositing round-trips");
        assert_eq!(lua.atlas.pixels, vec![1, 2, 3, 255]);
        // The composited map's ground is hidden under `voidGround`: not
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

    /// Rewriting a shipped map with its own facings reproduces its
    /// payload byte for byte: the editor changes nothing but the facings.
    #[test]
    fn set_datavent_facings_keeps_shipped_payloads() {
        let dir = crate::shipped_maps_dir();
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "kpmap") {
                continue;
            }
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let bytes = std::fs::read(&path).unwrap();
            let facings: Vec<u8> = read_baked_map(&bytes)
                .unwrap()
                .datavents
                .iter()
                .map(|v| v.facing)
                .collect();
            // The payload, not the file: zstd level 19 on all 18 maps
            // would take a minute, and the container is covered by the
            // round-trip tests.
            let payload = decode_payload(&bytes).unwrap();
            assert!(
                rewrite_facings(&payload, &facings).unwrap() == payload,
                "{name}: payload changed"
            );
            count += 1;
        }
        assert!(count >= 18, "expected the 18 shipped maps, found {count}");
    }

    #[test]
    fn set_datavent_facings_changes_only_the_facings() {
        let mut original = sample_map();
        original.datavents.push(Datavent {
            x: 300.0,
            z: 50.0,
            facing: 0,
        });
        let bytes = write_baked_map(&original).unwrap();
        let loaded = read_baked_map(&set_datavent_facings(&bytes, &[3, 2]).unwrap()).unwrap();
        let facings: Vec<u8> = loaded.datavents.iter().map(|v| v.facing).collect();
        assert_eq!(facings, vec![3, 2]);
        let positions: Vec<(f32, f32)> = loaded.datavents.iter().map(|v| (v.x, v.z)).collect();
        assert_eq!(positions, vec![(100.0, 100.0), (300.0, 50.0)]);
        assert_eq!(loaded.parsed.heights, original.parsed.heights);
        assert_eq!(loaded.ground_texture.unwrap().pixels, vec![255; 16]);
        assert_eq!(loaded.map_info.unwrap().gravity, 130.0);
    }

    #[test]
    fn set_datavent_facings_rejects_bad_input() {
        let bytes = write_baked_map(&sample_map()).unwrap();
        assert!(matches!(
            set_datavent_facings(&bytes, &[]),
            Err(BakedMapError::DataventCount {
                expected: 1,
                actual: 0
            })
        ));
        assert!(matches!(
            set_datavent_facings(&bytes, &[4]),
            Err(BakedMapError::BadFacing(4))
        ));
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
