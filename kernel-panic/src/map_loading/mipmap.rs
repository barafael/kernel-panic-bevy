//! Terrain-texture material construction: ground-texture pyramid, mipmap
//! chain, and the dark fallback used when the map has no ground texture.

use bevy::prelude::*;

use spring_map::map_types::{GroundTexture, MipmapData};

use crate::terrain::material::create_terrain_material;

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

pub(super) fn build_terrain_material_from_texture(
    ground: &GroundTexture,
    images: &mut ResMut<Assets<Image>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) -> Handle<StandardMaterial> {
    let (base_pixels, base_w, base_h) = downsample_to_fit(
        &ground.pixels,
        ground.width,
        ground.height,
        MAX_GROUND_TEX_DIM,
    );

    let MipmapData {
        pixels: mipmap_pixels,
        level_count: mip_levels,
    } = generate_mipmaps(&base_pixels, base_w, base_h);

    let size = bevy::render::render_resource::Extent3d {
        width: base_w as u32,
        height: base_h as u32,
        depth_or_array_layers: 1,
    };
    let format = bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb;
    let usage =
        bevy::asset::RenderAssetUsages::RENDER_WORLD | bevy::asset::RenderAssetUsages::MAIN_WORLD;

    let mut image = Image::new_uninit(
        size,
        bevy::render::render_resource::TextureDimension::D2,
        format,
        usage,
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

    if (base_w, base_h) != (ground.width, ground.height) {
        info!(
            "  Texture: {}x{} → {base_w}x{base_h} (capped at {MAX_GROUND_TEX_DIM}), {mip_levels} mip levels",
            ground.width, ground.height,
        );
    } else {
        info!("  Texture: {base_w}x{base_h}, {mip_levels} mip levels");
    }

    let texture_handle = images.add(image);
    create_terrain_material(texture_handle, materials)
}

/// 2×2 box-filter `src` (`src_w`×`src_h`) into a buffer sized `dst_w`×`dst_h`.
/// Used by both the initial size-cap pass and the mipmap-chain build.
fn box_filter_2x(src: &[u8], src_w: usize, src_h: usize, dst_w: usize, dst_h: usize) -> Vec<u8> {
    let mut dst = vec![0u8; dst_w * dst_h * 4];
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
    dst
}

/// Halve the texture until both dimensions are ≤ `max_dim`, box-filtering
/// at each step. No-op (returns a copy) when already within bounds.
fn downsample_to_fit(
    pixels: &[u8],
    width: usize,
    height: usize,
    max_dim: usize,
) -> (Vec<u8>, usize, usize) {
    if width <= max_dim && height <= max_dim {
        return (pixels.to_vec(), width, height);
    }
    let mut current = pixels.to_vec();
    let mut cw = width;
    let mut ch = height;
    while cw > max_dim || ch > max_dim {
        let nw = (cw / 2).max(1);
        let nh = (ch / 2).max(1);
        current = box_filter_2x(&current, cw, ch, nw, nh);
        cw = nw;
        ch = nh;
    }
    (current, cw, ch)
}

/// Build a full mipmap chain by 2×2 box-filtering the source texture
/// down to 1×1. Returns the chained pixel buffer (all levels
/// concatenated) and the level count, ready to feed into Bevy's
/// `texture_descriptor.mip_level_count`.
fn generate_mipmaps(pixels: &[u8], width: usize, height: usize) -> MipmapData {
    let mut all_data = Vec::with_capacity(pixels.len() * 4 / 3);
    all_data.extend_from_slice(pixels);
    let mut levels = 1u32;

    let mut current_w = width;
    let mut current_h = height;
    let mut src = pixels.to_vec();

    while current_w > 1 || current_h > 1 {
        let next_w = (current_w / 2).max(1);
        let next_h = (current_h / 2).max(1);
        let dst = box_filter_2x(&src, current_w, current_h, next_w, next_h);

        all_data.extend_from_slice(&dst);
        levels += 1;
        src = dst;
        current_w = next_w;
        current_h = next_h;
    }

    MipmapData {
        pixels: all_data,
        level_count: levels,
    }
}
