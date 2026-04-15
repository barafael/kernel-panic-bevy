use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use spring_unit_mesh::{S3OModel, S3OPiece, TgaImage};

use super::components::Faction;
use super::definitions::{UnitKind, stats};

// ---------------------------------------------------------------------------
// Caches
// ---------------------------------------------------------------------------

/// Cached s3o model data, textures, and Bevy handles loaded from disk.
#[derive(Resource, Default)]
pub struct S3OModelCache {
    models: HashMap<&'static str, Option<S3OModel>>,
    raw_textures: HashMap<String, Option<TgaImage>>,
    colored_textures: HashMap<(String, Faction), Handle<Image>>,
    mesh_handles: HashMap<&'static str, Handle<Mesh>>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Create a material for a unit. Builds a faction-colored texture from the
/// s3o model's tex1 alpha channel, falling back to a flat emissive material.
///
/// Spring's s3o shader uses tex1's alpha as the detail pattern and RGB=black
/// to mean "100% team color". We bake the faction color into the texture so
/// that Bevy's standard unlit material renders it correctly.
pub fn unit_material(
    kind: UnitKind,
    faction: Faction,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    cache: &mut S3OModelCache,
) -> Handle<StandardMaterial> {
    let unit_stats = stats(kind);

    let tex1_name = load_s3o_cached(unit_stats.model, cache).map(|model| model.texture1.clone());

    if let Some(tex1_name) = tex1_name {
        if let Some(handle) = build_faction_texture(&tex1_name, faction, images, cache) {
            return materials.add(StandardMaterial {
                base_color_texture: Some(handle),
                unlit: true,
                ..default()
            });
        }
    }

    let color = faction.color();
    materials.add(StandardMaterial {
        base_color: color,
        emissive: LinearRgba::from(color) * 4.0,
        unlit: true,
        ..default()
    })
}

/// Create a mesh for a unit type, loading the real s3o model if available.
/// Returns a cached handle — multiple units of the same type share one mesh.
pub fn unit_mesh(
    kind: UnitKind,
    meshes: &mut Assets<Mesh>,
    cache: &mut S3OModelCache,
) -> Handle<Mesh> {
    let unit_stats = stats(kind);

    if let Some(handle) = cache.mesh_handles.get(unit_stats.model) {
        return handle.clone();
    }

    if let Some(model) = load_s3o_cached(unit_stats.model, cache) {
        let mesh = s3o_to_bevy_mesh(&model.root_piece, 1.0);
        let handle = meshes.add(mesh);
        cache.mesh_handles.insert(unit_stats.model, handle.clone());
        return handle;
    }

    // Fallback: procedural cylinder.
    let scale = unit_stats.mesh_scale;
    let mesh = match kind {
        UnitKind::Kernel | UnitKind::Hole | UnitKind::Connection => {
            Cylinder::new(20.0 * scale, 12.0 * scale)
        }
        UnitKind::Socket | UnitKind::Window | UnitKind::Port => {
            Cylinder::new(15.0 * scale, 6.0 * scale)
        }
        UnitKind::Firewall | UnitKind::Exploit => Cylinder::new(10.0 * scale, 8.0 * scale),
        UnitKind::Bit | UnitKind::Bug | UnitKind::Packet | UnitKind::Virus | UnitKind::Signal => {
            Cylinder::new(3.0 * scale, 4.0 * scale)
        }
        UnitKind::Assembler
        | UnitKind::Worm
        | UnitKind::Dos
        | UnitKind::Pointer
        | UnitKind::LogicBomb => Cylinder::new(6.0 * scale, 6.0 * scale),
        UnitKind::Byte => Cylinder::new(12.0 * scale, 10.0 * scale),
    };
    meshes.add(mesh)
}

// ---------------------------------------------------------------------------
// Model / texture loading with shared disk-read helper
// ---------------------------------------------------------------------------

fn load_s3o_cached<'a>(
    filename: &'static str,
    cache: &'a mut S3OModelCache,
) -> Option<&'a S3OModel> {
    cache
        .models
        .entry(filename)
        .or_insert_with(|| load_asset_from_disk(filename, |data| spring_unit_mesh::parse_s3o(data)))
        .as_ref()
}

fn load_raw_tga_cached<'a>(tex_name: &str, cache: &'a mut S3OModelCache) -> Option<&'a TgaImage> {
    let key = tex_name.to_string();
    cache
        .raw_textures
        .entry(key)
        .or_insert_with(|| load_asset_from_disk(tex_name, |data| spring_unit_mesh::parse_tga(data)))
        .as_ref()
}

/// Try each candidate path, read the file, and parse it. Returns `None` with
/// a warning if no path succeeds.
fn load_asset_from_disk<T, E: fmt::Display>(
    filename: &str,
    parse: impl Fn(&[u8]) -> Result<T, E>,
) -> Option<T> {
    for path in find_asset_paths(filename) {
        match std::fs::read(&path) {
            Ok(data) => match parse(&data) {
                Ok(result) => {
                    info!("Loaded asset: {filename} ({} bytes)", data.len());
                    return Some(result);
                }
                Err(err) => {
                    warn!("Failed to parse {filename}: {err}");
                }
            },
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                warn!("I/O error reading {}: {err}", path.display());
            }
            Err(_) => {}
        }
    }

    warn!("Asset not found: {filename}");
    None
}

// ---------------------------------------------------------------------------
// Faction-colored texture building
// ---------------------------------------------------------------------------

fn build_faction_texture(
    tex_name: &str,
    faction: Faction,
    images: &mut Assets<Image>,
    cache: &mut S3OModelCache,
) -> Option<Handle<Image>> {
    let key = (tex_name.to_string(), faction);
    if let Some(handle) = cache.colored_textures.get(&key) {
        return Some(handle.clone());
    }

    // Load the raw TGA; extract dimensions and build colored pixels in a free
    // function so the borrow on `cache` is released before we insert the result.
    let tga = load_raw_tga_cached(tex_name, cache)?;
    let pixels = colorize_texture(tga, faction);
    let (w, h) = (tga.width, tga.height);

    let image = create_rgba8_image(w, h, pixels);
    info!("Built faction texture: {tex_name} x {faction:?} ({w}x{h})");

    let handle = images.add(image);
    cache.colored_textures.insert(key, handle.clone());
    Some(handle)
}

/// Apply faction color to a raw TGA texture using Spring's tex1 convention:
/// RGB=black → 100% team color, alpha → brightness mask.
fn colorize_texture(tga: &TgaImage, faction: Faction) -> Vec<u8> {
    let color = faction.color();
    let linear = LinearRgba::from(color);
    let fr = (linear.red.clamp(0.0, 1.0) * 255.0) as u8;
    let fg = (linear.green.clamp(0.0, 1.0) * 255.0) as u8;
    let fb = (linear.blue.clamp(0.0, 1.0) * 255.0) as u8;

    let pixel_count = (tga.width * tga.height) as usize;
    let mut pixels = Vec::with_capacity(pixel_count * 4);

    for chunk in tga.pixels.chunks_exact(4) {
        let (src_r, src_g, src_b) = (chunk[0], chunk[1], chunk[2]);
        let alpha = chunk[3] as u16;

        let brightness = src_r.max(src_g).max(src_b);
        let (base_r, base_g, base_b) = if brightness == 0 {
            (fr, fg, fb)
        } else {
            let t = brightness as u16;
            (
                ((fr as u16 * (255 - t) + src_r as u16 * t) / 255) as u8,
                ((fg as u16 * (255 - t) + src_g as u16 * t) / 255) as u8,
                ((fb as u16 * (255 - t) + src_b as u16 * t) / 255) as u8,
            )
        };

        // alpha=0 → dim (~10% base), alpha=255 → full brightness. Always opaque.
        let scale = 25 + (alpha * 230 / 255);
        let r = (base_r as u16 * scale / 255) as u8;
        let g = (base_g as u16 * scale / 255) as u8;
        let b = (base_b as u16 * scale / 255) as u8;
        pixels.extend_from_slice(&[r, g, b, 255]);
    }

    pixels
}

// ---------------------------------------------------------------------------
// Bevy Image helper
// ---------------------------------------------------------------------------

/// Create a Bevy `Image` from raw RGBA8 pixel data with linear filtering.
fn create_rgba8_image(width: u32, height: u32, pixels: Vec<u8>) -> Image {
    let size = bevy::render::render_resource::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let format = bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb;
    let usage =
        bevy::asset::RenderAssetUsages::RENDER_WORLD | bevy::asset::RenderAssetUsages::MAIN_WORLD;

    let mut image = Image::new(
        size,
        bevy::render::render_resource::TextureDimension::D2,
        pixels,
        format,
        usage,
    );
    image.sampler = bevy::image::ImageSampler::Descriptor(bevy::image::ImageSamplerDescriptor {
        min_filter: bevy::image::ImageFilterMode::Linear,
        mag_filter: bevy::image::ImageFilterMode::Linear,
        mipmap_filter: bevy::image::ImageFilterMode::Linear,
        ..default()
    });
    image
}

// ---------------------------------------------------------------------------
// Path resolution
// ---------------------------------------------------------------------------

const ASSET_DIRS: &[&str] = &[
    "upstream/Kernel-Panic/objects3d",
    "upstream/Kernel-Panic/unittextures",
    "kernel-panic/upstream/Kernel-Panic/objects3d",
    "kernel-panic/upstream/Kernel-Panic/unittextures",
];

/// Lazily find the first existing asset path for a filename.
fn find_asset_paths(filename: &str) -> impl Iterator<Item = PathBuf> + '_ {
    ASSET_DIRS
        .iter()
        .map(move |dir| PathBuf::from(format!("{dir}/{filename}")))
}

// ---------------------------------------------------------------------------
// Mesh conversion
// ---------------------------------------------------------------------------

/// Flatten an s3o piece tree into a single Bevy `Mesh`, applying
/// hierarchical offsets and a uniform scale factor.
fn s3o_to_bevy_mesh(root: &S3OPiece, scale: f32) -> Mesh {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    collect_piece(
        root,
        [0.0, 0.0, 0.0],
        scale,
        &mut positions,
        &mut normals,
        &mut uvs,
        &mut indices,
    );

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::RENDER_WORLD | bevy::asset::RenderAssetUsages::MAIN_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

fn collect_piece(
    piece: &S3OPiece,
    parent_offset: [f32; 3],
    scale: f32,
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    indices: &mut Vec<u32>,
) {
    let world_offset = [
        parent_offset[0] + piece.offset[0],
        parent_offset[1] + piece.offset[1],
        parent_offset[2] + piece.offset[2],
    ];

    let base_index = positions.len() as u32;

    for v in &piece.vertices {
        positions.push([
            (v.position[0] + world_offset[0]) * scale,
            (v.position[1] + world_offset[1]) * scale,
            (v.position[2] + world_offset[2]) * scale,
        ]);
        normals.push(v.normal);
        uvs.push(v.texcoord);
    }

    for &idx in &piece.indices {
        indices.push(base_index + idx);
    }

    for child in &piece.children {
        collect_piece(child, world_offset, scale, positions, normals, uvs, indices);
    }
}
