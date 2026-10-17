//! Hex Farm's Lua-drawn skin.
//!
//! Spring maps like Hex Farm don't ship a ground texture: the unsynced
//! half of `HexFarm8.lua` binds one of the `bitmaps/MapTex/hexfarm8_
//! <skin>.<ext>` images via `gl.Texture` and UV-maps its tower and
//! bridge geometry into it (`DrawHex` / `DrawRect`). The skin the gadget
//! picks for Kernel Panic (`UseMapOptions`, l.186) is 9, "Digital"; its
//! decoded image is carried in the `.kpmap` as a [`SkinAtlas`] and the
//! renderer (`map_loading::lua_compositing`) maps the same geometry into
//! it. Nothing is ever tiled across the ground: the ground is hidden by
//! `voidGround`.

/// The skin image the gadget binds. Width and height match the source
/// bitmap exactly so the renderer can map the 8-region UV strips
/// directly. Pixels are tightly packed RGBA8.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkinAtlas {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}
