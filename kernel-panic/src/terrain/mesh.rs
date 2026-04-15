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
    let sq_size = map.header.square_size as f32;

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

    for lz in 0..local_h {
        for lx in 0..local_w {
            let gx = vx_start + lx;
            let gz = vz_start + lz;

            let height = map.heights[gz * hm_w + gx];
            positions.push([lx as f32 * sq_size, height, lz as f32 * sq_size]);

            let u = gx as f32 / (hm_w - 1) as f32;
            let v = gz as f32 / (hm_h - 1) as f32;
            uvs.push([u, v]);

            normals.push([0.0, 1.0, 0.0]);
        }
    }

    normals.fill([0.0, 0.0, 0.0]);

    let quads_w = local_w.saturating_sub(1);
    let quads_h = local_h.saturating_sub(1);
    let mut indices = Vec::with_capacity(quads_w * quads_h * 6);

    for qz in 0..quads_h {
        for qx in 0..quads_w {
            let tl = (qz * local_w + qx) as u32;
            let tr = tl + 1;
            let bl = ((qz + 1) * local_w + qx) as u32;
            let br = bl + 1;

            indices.extend_from_slice(&[tl, bl, tr, tr, bl, br]);

            let p_tl = Vec3::from(positions[tl as usize]);
            let p_bl = Vec3::from(positions[bl as usize]);
            let p_tr = Vec3::from(positions[tr as usize]);
            let p_br = Vec3::from(positions[br as usize]);

            let n1 = (p_bl - p_tl).cross(p_tr - p_tl);
            let n2 = (p_bl - p_tr).cross(p_br - p_tr);

            for &idx in &[tl, bl, tr] {
                let n = &mut normals[idx as usize];
                n[0] += n1.x;
                n[1] += n1.y;
                n[2] += n1.z;
            }
            for &idx in &[tr, bl, br] {
                let n = &mut normals[idx as usize];
                n[0] += n2.x;
                n[1] += n2.y;
                n[2] += n2.z;
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
