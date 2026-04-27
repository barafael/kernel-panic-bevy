//! Lua-driven map-skin compositor.
//!
//! Spring maps like Hex Farm don't ship a baked SMT diffuse — the SMT
//! is a placeholder that the unsynced Lua gadget overwrites at runtime
//! by binding one of the `bitmaps/MapTex/hexfarm8_<skin>.<ext>` PNG/JPG
//! files via `gl.Texture` and `Spring.SetMapShadingTexture`. The skin
//! is chosen in the synced half (line 211 of `HexFarm8.lua`) and
//! shipped to unsynced via `SendToUnsynced("ReceiveHexFarmLayout",
//! "Skin", N)`.
//!
//! We don't run the unsynced half (no GL VM), so we mirror the skin
//! lookup table here, capture the synced→unsynced message, decode the
//! matching bitmap, and tile it across a fresh [`GroundTexture`].

use thiserror::Error;

use crate::lua_heightmap::LuaGadgetResult;
use crate::map_types::{BitmapFile, GroundTexture, ParsedMap, UnsyncedMessage};

/// HexFarm's skin table (mirrors lines 16–26 of `HexFarm8.lua`).
///
/// Index = the integer the gadget sends via `("ReceiveHexFarmLayout",
/// "Skin", N)`. `(name, ext)` is the lower-case stem and original
/// extension of `bitmaps/MapTex/hexfarm8_<name>.<ext>`. We match
/// case-insensitively against extracted bitmap paths, so the casing
/// here is just for documentation.
const HEXFARM_SKINS: &[(i64, &str, &str)] = &[
    (1, "pastoral", "jpg"),
    (2, "medieval", "png"),
    (3, "industrial", "png"),
    (4, "technical", "png"),
    (5, "capital", "png"),
    (6, "geological", "jpg"),
    (7, "summital", "jpg"),
    (8, "crystal", "jpg"),
    (9, "digital", "png"),
];

/// Output dimensions of the tiled diffuse. The SMT-derived texture for
/// most maps is 8 px / elmo, but the renderer caps to 8192² anyway, so
/// we don't gain anything from going larger here. 4096² leaves the
/// PNG visibly tileable without burning hundreds of MB of VRAM on a
/// pattern that's just repeating itself.
const COMPOSITE_TEXTURE_SIZE: usize = 4096;

#[derive(Debug, Error)]
enum CompositeError {
    #[error("no `Skin` message captured from synced gadget")]
    NoSkin,
    #[error("skin {0} is not in the HexFarm skin table")]
    UnknownSkin(i64),
    #[error("no bitmap matching `bitmaps/MapTex/hexfarm8_{0}.*` in archive")]
    NoBitmap(String),
    #[error("failed to decode bitmap `{path}`: {error}")]
    Decode {
        path: String,
        #[source]
        error: image::ImageError,
    },
}

/// If a gadget told us which Lua skin to use, decode the matching
/// bitmap and return a tiled ground texture. Returns `None` when no
/// skin was selected (most maps) or the matching bitmap is missing or
/// unreadable — caller falls back to the SMT-derived texture.
pub fn composite_ground_texture(
    gadget_results: &[LuaGadgetResult],
    bitmaps: &[BitmapFile],
    _parsed: &ParsedMap,
) -> Option<GroundTexture> {
    match try_composite(gadget_results, bitmaps) {
        Ok(texture) => Some(texture),
        Err(CompositeError::NoSkin) => None,
        Err(error) => {
            eprintln!("Lua skin compositing skipped: {error}");
            None
        }
    }
}

fn try_composite(
    gadget_results: &[LuaGadgetResult],
    bitmaps: &[BitmapFile],
) -> Result<GroundTexture, CompositeError> {
    let skin = find_skin_message(gadget_results).ok_or(CompositeError::NoSkin)?;
    let (name, ext) = HEXFARM_SKINS
        .iter()
        .find(|(n, _, _)| *n == skin)
        .map(|(_, name, ext)| (*name, *ext))
        .ok_or(CompositeError::UnknownSkin(skin))?;

    let bitmap = find_bitmap(bitmaps, name).ok_or_else(|| CompositeError::NoBitmap(name.into()))?;

    let img = image::load_from_memory(&bitmap.data)
        .map_err(|error| CompositeError::Decode {
            path: bitmap.path.clone(),
            error,
        })?
        .to_rgba8();

    eprintln!(
        "Composited Lua skin texture: {} ({}x{}, .{ext})",
        bitmap.path,
        img.width(),
        img.height()
    );

    Ok(tile_to_ground_texture(
        &img,
        COMPOSITE_TEXTURE_SIZE,
        COMPOSITE_TEXTURE_SIZE,
    ))
}

/// Walk every captured `SendToUnsynced(...)` looking for the
/// `("ReceiveHexFarmLayout", "Skin", N)` shape used by HexFarm.
fn find_skin_message(gadget_results: &[LuaGadgetResult]) -> Option<i64> {
    for result in gadget_results {
        for msg in &result.unsynced_messages {
            if let Some(skin) = match_skin(msg) {
                return Some(skin);
            }
        }
    }
    None
}

fn match_skin(msg: &UnsyncedMessage) -> Option<i64> {
    let action = msg.first()?.as_str()?;
    if !action.eq_ignore_ascii_case("ReceiveHexFarmLayout") {
        return None;
    }
    let key = msg.get(1)?.as_str()?;
    if !key.eq_ignore_ascii_case("Skin") {
        return None;
    }
    msg.get(2)?.as_i64()
}

/// Case-insensitive lookup for `bitmaps/MapTex/hexfarm8_<skin>.<ext>`.
/// Falls back to any extension since some skins ship `.JPG` while the
/// gadget table says `.PNG` and vice versa.
fn find_bitmap<'a>(bitmaps: &'a [BitmapFile], skin_name: &str) -> Option<&'a BitmapFile> {
    let needle = format!("hexfarm8_{}", skin_name.to_ascii_lowercase());
    bitmaps.iter().find(|b| {
        let lower = b.path.to_ascii_lowercase().replace('\\', "/");
        lower.contains(&needle)
    })
}

/// Repeat `src` enough times to fill a `dst_w × dst_h` RGBA8 buffer.
/// Per-pixel modulo into the source image — coarse but matches the
/// "tiled texture" effect Spring achieves with a wrapping sampler on
/// the original SMF UVs.
fn tile_to_ground_texture(src: &image::RgbaImage, dst_w: usize, dst_h: usize) -> GroundTexture {
    let sw = src.width() as usize;
    let sh = src.height() as usize;
    let mut pixels = vec![0u8; dst_w * dst_h * 4];
    let raw = src.as_raw();
    for y in 0..dst_h {
        let sy = y % sh;
        for x in 0..dst_w {
            let sx = x % sw;
            let src_i = (sy * sw + sx) * 4;
            let dst_i = (y * dst_w + x) * 4;
            pixels[dst_i..dst_i + 4].copy_from_slice(&raw[src_i..src_i + 4]);
        }
    }
    GroundTexture {
        width: dst_w,
        height: dst_h,
        pixels,
    }
}
