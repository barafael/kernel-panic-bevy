//! Render the HexFarm-style tower meshes captured from the synced
//! gadget. The Lua skin compositor in `spring-map` already produced a
//! [`LuaCompositing`] containing per-hex layout data and the decoded
//! atlas; this module turns that into a single Bevy mesh + material
//! and spawns it as map decoration.
//!
//! Geometry mirrors the gadget's `DrawHex` (lines 2181–2370 of
//! `HexFarm8.lua`):
//! - Top hexagon split into two trapezoidal quads, UV-mapped to atlas
//!   region 1 (with geo) or 5 (no geo).
//! - Six side walls, each a quad from the corner top down to
//!   `-VISUAL_PIT_DEPTH`, UV-mapped to region 3 (no bridge) or 4
//!   (bridge), V wrapping vertically based on the tower height.
//!
//! Bridges, animations, and team coloring stay out of scope until they
//! become the dominant visual gap.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, Mesh, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use spring_map::LuaCompositing;
use spring_map::lua_layout::HexTower;
use spring_map::lua_skin::SkinAtlas;

use crate::terrain::geovent::spawn_smoker_at;

/// Matches `local VisualPitDepth=1024` (line 1972 of `HexFarm8.lua`).
/// Side walls extend from the tower top down to this Y.
const VISUAL_PIT_DEPTH: f32 = 1024.0;

pub fn spawn_lua_compositing(
    compositing: &LuaCompositing,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
) {
    let layout = &compositing.layout;
    if layout.hexes.is_empty() {
        return;
    }

    let atlas_handle = upload_atlas(&compositing.atlas, images);
    let material = materials.add(StandardMaterial {
        base_color_texture: Some(atlas_handle),
        unlit: true,
        // The skin atlas is partly transparent (the gadget loads it via
        // `:a:` for alpha-aware sampling). Without alpha discard the
        // hex shape would be a rectangle. `Mask` is fine — there's no
        // soft edge that needs blending.
        alpha_mode: AlphaMode::Mask(0.5),
        // Trapezoid winding for the top face is reversed from Bevy's
        // CCW front-face convention, and side normals are unreliable
        // until we compute them properly. Disable culling for now.
        cull_mode: None,
        ..default()
    });

    let mesh = build_hex_mesh(&layout.hexes);
    let mesh_handle = meshes.add(mesh);
    commands.spawn((Mesh3d(mesh_handle), MeshMaterial3d(material)));

    // Mirror the Lua gadget's `RedoDatavents` (HexFarm8.lua:1120) — every
    // visible hex with `g` set carries a geovent at its center. Routing
    // through `spawn_smoker_at` means these vents participate in the
    // existing claim/build pipeline exactly like SMF-listed geovents.
    let mut geo_count = 0u32;
    for hex in &layout.hexes {
        if hex.hidden || hex.g == 0 {
            continue;
        }
        spawn_smoker_at(
            commands,
            Vec3::new(hex.center[0], hex.center[1], hex.center[2]),
        );
        geo_count += 1;
    }

    info!(
        "Hex Farm: spawned {} hex towers, {} geovents ({} bridges captured but not yet rendered)",
        layout.hexes.iter().filter(|h| !h.hidden).count(),
        geo_count,
        layout.bridges.len(),
    );
}

fn upload_atlas(atlas: &SkinAtlas, images: &mut Assets<Image>) -> Handle<Image> {
    let mut image = Image::new(
        Extent3d {
            width: atlas.width,
            height: atlas.height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        atlas.pixels.clone(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    // V wraps vertically because side-wall UVs go past 1.0 for tall
    // towers. U does not — atlas regions are stacked horizontally and
    // we don't want sample-bleed between regions 3 and 4.
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::Repeat,
        ..default()
    });
    images.add(image)
}

fn build_hex_mesh(hexes: &[HexTower]) -> Mesh {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    for hex in hexes {
        if hex.hidden {
            continue;
        }
        emit_hex(hex, &mut positions, &mut uvs, &mut normals, &mut indices);
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// `GetLeft(n)` from the gadget. n=1 and n=5 inset by the hex
/// half-diameter so the trapezoid UV doesn't bleed past the actual
/// hexagonal sub-region of the atlas.
fn get_left(n: u32) -> f32 {
    match n {
        1 => 0.133_974_6 / 8.0,
        5 => 4.133_974_6 / 8.0,
        _ => (n as f32 - 1.0) / 8.0,
    }
}

/// `GetRight(n)` from the gadget — symmetric to `get_left`.
fn get_right(n: u32) -> f32 {
    match n {
        2 => 1.866_025_4 / 8.0,
        6 => 5.866_025_4 / 8.0,
        _ => n as f32 / 8.0,
    }
}

fn emit_hex(
    hex: &HexTower,
    positions: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    normals: &mut Vec<[f32; 3]>,
    indices: &mut Vec<u32>,
) {
    let c = &hex.corners;
    let y_top = hex.center[1];
    let y_bot = -VISUAL_PIT_DEPTH;

    // Top face: regions 1 (geo) or 5 (no geo).
    let u_top = if hex.g != 0 { 1 } else { 5 };
    let lu = get_left(u_top);
    let ru = get_right(u_top);

    // Eight vertices in two trapezoidal quads, mirroring lines 2229–2250
    // of the gadget. c1 and c4 each carry two UV coords.
    let top_verts: [([f32; 3], [f32; 2]); 8] = [
        ([c[0][0], y_top, c[0][2]], [ru, 1.0]), // c1, right edge, top-V
        ([c[1][0], y_top, c[1][2]], [lu, 0.75]), // c2
        ([c[2][0], y_top, c[2][2]], [lu, 0.25]), // c3
        ([c[3][0], y_top, c[3][2]], [ru, 0.0]), // c4, right edge, bottom-V
        ([c[3][0], y_top, c[3][2]], [lu, 0.0]), // c4', left edge
        ([c[4][0], y_top, c[4][2]], [ru, 0.25]), // c5
        ([c[5][0], y_top, c[5][2]], [ru, 0.75]), // c6
        ([c[0][0], y_top, c[0][2]], [lu, 1.0]), // c1', left edge
    ];
    let top_base = positions.len() as u32;
    for (p, uv) in top_verts {
        positions.push(p);
        uvs.push(uv);
        normals.push([0.0, 1.0, 0.0]);
    }
    // Two trapezoidal quads → 4 triangles. Indices match the gadget's
    // GL_QUADS layout (c1,c2,c3,c4 and c4',c5,c6,c1') but we're CCW
    // from below; cull_mode: None lets either side render.
    indices.extend_from_slice(&[
        top_base,
        top_base + 1,
        top_base + 2,
        top_base,
        top_base + 2,
        top_base + 3,
        top_base + 4,
        top_base + 5,
        top_base + 6,
        top_base + 4,
        top_base + 6,
        top_base + 7,
    ]);

    // Side faces: 6 quads from corner s (at y_top) down to (corner s,
    // y_bot) and across to corner (s+1)%6. Region 3 if no bridge sits
    // on this side, 4 if there is one (matches `cu[s] = cb[s] != 0`).
    let side_hex = ((c[1][0] - c[0][0]).powi(2) + (c[1][2] - c[0][2]).powi(2)).sqrt();
    let v_depth = if side_hex > 0.0 {
        (y_top + VISUAL_PIT_DEPTH) / (2.0 * side_hex)
    } else {
        1.0
    };

    for s in 0..6 {
        let cur = c[s];
        let next = c[(s + 1) % 6];
        let u_side = if hex.corner_bridges[s] != 0 { 4 } else { 3 };
        let lu_s = get_left(u_side);
        let ru_s = get_right(u_side);

        let base = positions.len() as u32;
        let side_verts: [([f32; 3], [f32; 2]); 4] = [
            ([cur[0], y_top, cur[2]], [ru_s, 0.0]),
            ([cur[0], y_bot, cur[2]], [ru_s, v_depth]),
            ([next[0], y_bot, next[2]], [lu_s, v_depth]),
            ([next[0], y_top, next[2]], [lu_s, 0.0]),
        ];
        for (p, uv) in side_verts {
            positions.push(p);
            uvs.push(uv);
            // Side normals would need an outward direction per face.
            // unlit: true on the material makes this irrelevant.
            normals.push([0.0, 1.0, 0.0]);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}
