//! Lua-driven map-skin compositor.
//!
//! Spring maps like Hex Farm don't ship a baked SMT diffuse — the SMT
//! is a placeholder that the unsynced Lua gadget overwrites at runtime
//! by binding one of the `bitmaps/MapTex/hexfarm8_<skin>.<ext>` PNG/JPG
//! files via `gl.Texture` and `Spring.SetMapShadingTexture`. The skin
//! is chosen in the synced half (line 186 of `HexFarm8.lua`) and
//! shipped to unsynced via `SendToUnsynced("ReceiveHexFarmLayout",
//! "Skin", N)` — always 9 for Kernel Panic.
//!
//! We don't run the unsynced half (no GL VM), so we mirror the skin
//! lookup table here and decode the matching bitmap into a
//! [`SkinAtlas`]. The renderer UV-maps the gadget's tower and bridge
//! geometry into it exactly like `DrawHex` / `DrawRect` do; nothing is
//! ever tiled across the ground (the ground is `voidGround`-hidden).

use thiserror::Error;

use crate::map_types::BitmapFile;

/// HexFarm's skin table (mirrors lines 16–26 of `HexFarm8.lua`).
///
/// The skin `UseMapOptions` (l.186) picks when it detects Kernel Panic
/// (`UnitDefNames["kernel"]`) and no `hexfarm_skin` map option is set:
/// 9, "Digital".
pub const KERNEL_PANIC_SKIN: i64 = 9;

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

/// Decoded skin atlas — the same image the unsynced gadget binds via
/// `gl.Texture(":a:bitmaps/MapTex/hexfarm8_<skin>.<ext>")`. Width and
/// height match the source bitmap exactly so the renderer can map the
/// 8-region UV strips directly. Pixels are tightly packed RGBA8.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkinAtlas {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

#[derive(Debug, Error)]
enum CompositeError {
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

/// Decode HexFarm skin `skin`'s atlas from the archive's bitmaps.
/// `None` (logged) if it can't be located / decoded.
pub fn decode_skin_atlas(skin: i64, bitmaps: &[BitmapFile]) -> Option<SkinAtlas> {
    match try_decode_atlas(skin, bitmaps) {
        Ok(atlas) => Some(atlas),
        Err(error) => {
            eprintln!("Lua skin atlas decoding skipped: {error}");
            None
        }
    }
}

fn try_decode_atlas(skin: i64, bitmaps: &[BitmapFile]) -> Result<SkinAtlas, CompositeError> {
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

    Ok(SkinAtlas {
        width: img.width(),
        height: img.height(),
        pixels: img.into_raw(),
    })
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
