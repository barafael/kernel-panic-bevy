//! Terrain-texture construction: ground-texture pyramid, mipmap chain,
//! and the dark fallback used when the map has no ground texture.

use bevy::prelude::*;

use spring_map::map_types::{GroundTexture, MipmapData};

/// Cap the base ground texture at 8192² before building the mip chain.
///
/// Why: Bevy 0.18's default `WgpuSettings` widens `max_texture_dimension_2d`
/// to the adapter's resolution but leaves `max_buffer_size` at the wgpu
/// default of 256 MB. Hex_Farm_8 assembles a 12288×12288 SMT whose mip0
/// alone is ~576 MB, so the staging upload silently fails and the terrain
/// renders untextured. 8192² is exactly 256 MB at RGBA8 — the largest
/// power-of-two that fits, and every other shipped map is already ≤8192².
const MAX_GROUND_TEX_DIM: usize = 8192;

pub(super) fn dark_fallback_material(
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    materials.add(StandardMaterial {
        base_color: Color::srgb(0.02, 0.02, 0.02),
        unlit: true,
        ..default()
    })
}

/// `voidGround` (mapinfo, forced for Hex Farm by KP's `hotfixes.lua`
/// ~l.207): the engine alpha-tests the ground against `voidAlphaMin`
/// and Hex Farm's SMT is fully transparent, so no ground pixel is ever
/// drawn. Mirror it with an all-transparent alpha-masked material —
/// the terrain mesh stays in the world (so cursor ray-casts and the
/// placement ghost still hit the heightmap surface) but every fragment
/// is discarded, depth included, leaving the black void.
pub(super) fn void_ground_material(
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    materials.add(StandardMaterial {
        base_color: Color::NONE,
        alpha_mode: AlphaMode::Mask(0.9),
        unlit: true,
        ..default()
    })
}

/// The ground texture as a mipmapped `Image`, ready for `Assets<Image>`.
///
/// The baked pixels become mip level 0 in place — the chain is appended
/// to the texture's own buffer, so the only bulk allocation is the
/// buffer growing by a third. Render-world only: nothing reads the
/// terrain texture back (the minimap is painted from level 0 before the
/// image is handed over), so the CPU copy — a third of a gigabyte on
/// the biggest maps — is released as soon as the GPU has it. Pure, so
/// the loader runs it off the main thread.
pub(super) fn build_terrain_image(ground: GroundTexture) -> Image {
    let GroundTexture {
        width,
        height,
        pixels,
    } = ground;
    let (base_w, base_h) = downsample_dims(width, height, MAX_GROUND_TEX_DIM);

    let MipmapData {
        pixels: mipmap_pixels,
        level_count: mip_levels,
    } = generate_mipmaps(pixels, width, height, base_w, base_h);

    let size = bevy::render::render_resource::Extent3d {
        width: base_w as u32,
        height: base_h as u32,
        depth_or_array_layers: 1,
    };
    let format = bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb;

    let mut image = Image::new_uninit(
        size,
        bevy::render::render_resource::TextureDimension::D2,
        format,
        bevy::asset::RenderAssetUsages::RENDER_WORLD,
    );
    image.data = Some(mipmap_pixels);
    image.texture_descriptor.mip_level_count = mip_levels;
    image.sampler = bevy::image::ImageSampler::Descriptor(bevy::image::ImageSamplerDescriptor {
        min_filter: bevy::image::ImageFilterMode::Linear,
        mag_filter: bevy::image::ImageFilterMode::Linear,
        mipmap_filter: bevy::image::ImageFilterMode::Linear,
        anisotropy_clamp: 16,
        ..default()
    });

    if (base_w, base_h) != (width, height) {
        info!(
            "  Texture: {width}x{height} → {base_w}x{base_h} (capped at {MAX_GROUND_TEX_DIM}), {mip_levels} mip levels",
        );
    } else {
        info!("  Texture: {base_w}x{base_h}, {mip_levels} mip levels");
    }

    image
}

/// Mip level 0 of a terrain image built by [`build_terrain_image`]:
/// the base pixels and their size.
pub(super) fn base_level(image: &Image) -> (&[u8], usize, usize) {
    let (w, h) = (
        image.texture_descriptor.size.width as usize,
        image.texture_descriptor.size.height as usize,
    );
    let data = image.data.as_deref().unwrap_or_default();
    (&data[..w * h * 4], w, h)
}

/// Full mip chain for an RGBA8 image at its native size: the chained
/// pixel buffer (`pixels` become level 0) and its level count. Used for
/// Lua skin atlases.
pub(super) fn generate_mipmaps_rgba8(
    pixels: Vec<u8>,
    width: usize,
    height: usize,
) -> (Vec<u8>, u32) {
    let MipmapData {
        pixels,
        level_count,
    } = generate_mipmaps(pixels, width, height, width, height);
    (pixels, level_count)
}

/// 2×2 box-filter `src` (`src_w`×`src_h`) into `dst` (`dst_w`×`dst_h`
/// RGBA8, exactly `dst_w * dst_h * 4` bytes). Used by both the initial
/// size-cap pass and the mipmap-chain build.
fn box_filter_2x(
    src: &[u8],
    src_w: usize,
    src_h: usize,
    dst: &mut [u8],
    dst_w: usize,
    dst_h: usize,
) {
    debug_assert_eq!(dst.len(), dst_w * dst_h * 4);
    for y in 0..dst_h {
        for x in 0..dst_w {
            let src_x = (x * 2).min(src_w - 1);
            let src_y = (y * 2).min(src_h - 1);
            let src_x1 = (src_x + 1).min(src_w - 1);
            let src_y1 = (src_y + 1).min(src_h - 1);

            let i00 = (src_y * src_w + src_x) * 4;
            let i10 = (src_y * src_w + src_x1) * 4;
            let i01 = (src_y1 * src_w + src_x) * 4;
            let i11 = (src_y1 * src_w + src_x1) * 4;

            for channel in 0..4 {
                let avg = (src[i00 + channel] as u16
                    + src[i10 + channel] as u16
                    + src[i01 + channel] as u16
                    + src[i11 + channel] as u16)
                    / 4;
                dst[(y * dst_w + x) * 4 + channel] = avg as u8;
            }
        }
    }
}

/// Halve `width`/`height` (min 1) until both are ≤ `max_dim`.
fn downsample_dims(width: usize, height: usize, max_dim: usize) -> (usize, usize) {
    let (mut cw, mut ch) = (width, height);
    while cw > max_dim || ch > max_dim {
        cw = (cw / 2).max(1);
        ch = (ch / 2).max(1);
    }
    (cw, ch)
}

/// Bytes of the full RGBA8 mip chain from a `w × h` level down to 1×1.
fn mip_chain_len(mut w: usize, mut h: usize) -> usize {
    let mut len = 0;
    loop {
        len += w * h * 4;
        if w == 1 && h == 1 {
            return len;
        }
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
}

/// Build a full mipmap chain by 2×2 box-filtering the source texture
/// down to 1×1. When `base_w/base_h` are smaller than `width/height`
/// (size-cap path), the chain starts with the box-filtered base level.
///
/// The chain lives in one buffer: `pixels` is grown (once, to the exact
/// chain size) and each level is filtered straight from the previous
/// level's slice into the space after it, so the source is never
/// duplicated. Returns the chained pixel buffer and the level count,
/// ready for Bevy's `texture_descriptor.mip_level_count`.
fn generate_mipmaps(
    pixels: Vec<u8>,
    width: usize,
    height: usize,
    base_w: usize,
    base_h: usize,
) -> MipmapData {
    // Level 0: the base (box-filtered once up front if capping applied;
    // the uncapped source is dropped right after).
    let (mut all_data, w0, h0) = if (base_w, base_h) == (width, height) {
        (pixels, width, height)
    } else {
        let mut base = vec![0u8; base_w * base_h * 4];
        box_filter_2x(&pixels, width, height, &mut base, base_w, base_h);
        drop(pixels);
        (base, base_w, base_h)
    };
    debug_assert_eq!(all_data.len(), w0 * h0 * 4);

    all_data.reserve_exact(mip_chain_len(w0, h0) - all_data.len());
    let mut levels = 1u32;

    let mut current_w = w0;
    let mut current_h = h0;
    let mut src_start = 0usize;

    while current_w > 1 || current_h > 1 {
        let next_w = (current_w / 2).max(1);
        let next_h = (current_h / 2).max(1);
        let dst_start = all_data.len();
        all_data.resize(dst_start + next_w * next_h * 4, 0);
        let (done, dst) = all_data.split_at_mut(dst_start);
        box_filter_2x(
            &done[src_start..],
            current_w,
            current_h,
            dst,
            next_w,
            next_h,
        );

        src_start = dst_start;
        levels += 1;
        current_w = next_w;
        current_h = next_h;
    }

    MipmapData {
        pixels: all_data,
        level_count: levels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The in-place chain matches a level-by-level rebuild: each level
    /// is the 2×2 box filter of the previous one, ending at 1×1.
    #[test]
    fn chain_levels_are_box_filtered_in_place() {
        let (w, h) = (4usize, 2usize);
        let pixels: Vec<u8> = (0..w * h * 4).map(|i| (i * 13 % 251) as u8).collect();
        let MipmapData {
            pixels: chain,
            level_count,
        } = generate_mipmaps(pixels.clone(), w, h, w, h);
        assert_eq!(level_count, 3);
        assert_eq!(chain.len(), mip_chain_len(w, h));
        assert_eq!(&chain[..pixels.len()], &pixels[..]);

        let mut l1 = vec![0u8; 2 * 1 * 4];
        box_filter_2x(&pixels, w, h, &mut l1, 2, 1);
        assert_eq!(&chain[pixels.len()..pixels.len() + l1.len()], &l1[..]);
        let mut l2 = vec![0u8; 4];
        box_filter_2x(&l1, 2, 1, &mut l2, 1, 1);
        assert_eq!(&chain[pixels.len() + l1.len()..], &l2[..]);
    }
}
