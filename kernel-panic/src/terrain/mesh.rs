use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
};

use spring_map::map_types::ParsedMap;

const CHUNK_SIZE: usize = 32;

pub struct TerrainChunk {
    pub mesh: Mesh,
    pub translation: Vec3,
}

/// Generate chunked terrain meshes from a parsed SMF map.
pub fn generate_terrain_chunks(map: &ParsedMap) -> Vec<TerrainChunk> {
    let hm_w = map.header.heightmap_width();
    let hm_h = map.header.heightmap_height();

    let chunks_x = (hm_w - 1).div_ceil(CHUNK_SIZE);
    let chunks_z = (hm_h - 1).div_ceil(CHUNK_SIZE);

    let mut chunks = Vec::with_capacity(chunks_x * chunks_z);

    for cz in 0..chunks_z {
        for cx in 0..chunks_x {
            chunks.push(build_chunk(map, cx, cz));
        }
    }

    chunks
}

fn build_chunk(map: &ParsedMap, cx: usize, cz: usize) -> TerrainChunk {
    let hm_w = map.header.heightmap_width();
    let hm_h = map.header.heightmap_height();
    let sq_size = spring_map::map_types::SQUARE_SIZE as f32;

    let vx_start = cx * CHUNK_SIZE;
    let vz_start = cz * CHUNK_SIZE;
    let vx_end = (vx_start + CHUNK_SIZE + 1).min(hm_w);
    let vz_end = (vz_start + CHUNK_SIZE + 1).min(hm_h);
    let local_w = vx_end - vx_start;
    let local_h = vz_end - vz_start;

    let num_verts = local_w * local_h;
    let mut positions = Vec::with_capacity(num_verts);
    let mut normals = Vec::with_capacity(num_verts);
    let mut uvs = Vec::with_capacity(num_verts);

    let origin_x = vx_start as f32 * sq_size;
    let origin_z = vz_start as f32 * sq_size;

    for local_z in 0..local_h {
        for local_x in 0..local_w {
            let global_x = vx_start + local_x;
            let global_z = vz_start + local_z;

            let height = map.heights[global_z * hm_w + global_x];
            positions.push([local_x as f32 * sq_size, height, local_z as f32 * sq_size]);

            let u = global_x as f32 / (hm_w - 1) as f32;
            let v = global_z as f32 / (hm_h - 1) as f32;
            uvs.push([u, v]);

            normals.push([0.0, 1.0, 0.0]);
        }
    }

    normals.fill([0.0, 0.0, 0.0]);

    let quads_w = local_w.saturating_sub(1);
    let quads_h = local_h.saturating_sub(1);
    let mut indices = Vec::with_capacity(quads_w * quads_h * 6);

    for quad_z in 0..quads_h {
        for quad_x in 0..quads_w {
            let top_left = (quad_z * local_w + quad_x) as u32;
            let top_right = top_left + 1;
            let bottom_left = ((quad_z + 1) * local_w + quad_x) as u32;
            let bottom_right = bottom_left + 1;

            indices.extend_from_slice(&[
                top_left,
                bottom_left,
                top_right,
                top_right,
                bottom_left,
                bottom_right,
            ]);

            let pos_tl = Vec3::from(positions[top_left as usize]);
            let pos_bl = Vec3::from(positions[bottom_left as usize]);
            let pos_tr = Vec3::from(positions[top_right as usize]);
            let pos_br = Vec3::from(positions[bottom_right as usize]);

            let normal_1 = (pos_bl - pos_tl).cross(pos_tr - pos_tl);
            let normal_2 = (pos_bl - pos_tr).cross(pos_br - pos_tr);

            for &idx in &[top_left, bottom_left, top_right] {
                let normal = &mut normals[idx as usize];
                normal[0] += normal_1.x;
                normal[1] += normal_1.y;
                normal[2] += normal_1.z;
            }
            for &idx in &[top_right, bottom_left, bottom_right] {
                let normal = &mut normals[idx as usize];
                normal[0] += normal_2.x;
                normal[1] += normal_2.y;
                normal[2] += normal_2.z;
            }
        }
    }

    for n in &mut normals {
        *n = Vec3::from(*n).normalize_or(Vec3::Y).to_array();
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));

    TerrainChunk {
        mesh,
        translation: Vec3::new(origin_x, 0.0, origin_z),
    }
}
