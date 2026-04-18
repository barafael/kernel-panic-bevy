//! Terrain-texture material construction: ground-texture pyramid, mipmap
//! chain, and the dark fallback used when the map has no ground texture.

use bevy::prelude::*;

use spring_map::map_types::{GroundTexture, MipmapData};

use crate::terrain::material::create_terrain_material;

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
    let MipmapData {
        pixels: mipmap_pixels,
        level_count: mip_levels,
    } = generate_mipmaps(&ground.pixels, ground.width, ground.height);

    let size = bevy::render::render_resource::Extent3d {
        width: ground.width as u32,
        height: ground.height as u32,
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

    info!(
        "  Texture: {}x{}, {mip_levels} mip levels",
        ground.width, ground.height,
    );

    let texture_handle = images.add(image);
    create_terrain_material(texture_handle, materials)
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
        let mut dst = vec![0u8; next_w * next_h * 4];

        for y in 0..next_h {
            for x in 0..next_w {
                let src_x = (x * 2).min(current_w - 1);
                let src_y = (y * 2).min(current_h - 1);
                let src_x1 = (src_x + 1).min(current_w - 1);
                let src_y1 = (src_y + 1).min(current_h - 1);

                let i00 = (src_y * current_w + src_x) * 4;
                let i10 = (src_y * current_w + src_x1) * 4;
                let i01 = (src_y1 * current_w + src_x) * 4;
                let i11 = (src_y1 * current_w + src_x1) * 4;

                for channel in 0..4 {
                    let avg = (src[i00 + channel] as u16
                        + src[i10 + channel] as u16
                        + src[i01 + channel] as u16
                        + src[i11 + channel] as u16)
                        / 4;
                    dst[(y * next_w + x) * 4 + channel] = avg as u8;
                }
            }
        }

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
