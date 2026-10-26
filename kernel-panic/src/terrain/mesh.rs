use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology, VertexAttributeValues},
    platform::collections::HashSet,
    prelude::*,
};

use crate::map_loading::TerrainChunkCoord;
use crate::terrain::heightmap::Heightmap;

/// Heightmap squares per terrain chunk side.
///
/// Each chunk is one entity / mesh / draw call, so this sets the
/// terrain's draw count: at 32 a 16×16 map (1025² heights) was 1024
/// chunks and Hex Farm (12288 elmos, 1537²) 2304; at 64 they are 256
/// and 576. Frustum culling gets coarser, which an RTS camera
/// looking at a few hundred squares never notices. A full chunk is
/// 65×65 = 4225 vertices, far inside even a u16 index range (the
/// builder uses u32 anyway). Hex Farm's rebuild-on-reshape and the
/// chunk coords in `map_loading` derive from this constant.
pub const CHUNK_SIZE: usize = 64;

pub struct TerrainChunk {
    pub mesh: Mesh,
    pub translation: Vec3,
}

/// Generate chunked terrain meshes from a `hm_w × hm_h` height grid.
pub fn generate_terrain_chunks(heights: &[f32], hm_w: usize, hm_h: usize) -> Vec<TerrainChunk> {
    let chunks_x = (hm_w - 1).div_ceil(CHUNK_SIZE);
    let chunks_z = (hm_h - 1).div_ceil(CHUNK_SIZE);

    let mut chunks = Vec::with_capacity(chunks_x * chunks_z);

    for cz in 0..chunks_z {
        for cx in 0..chunks_x {
            chunks.push(build_chunk(heights, hm_w, hm_h, cx, cz));
        }
    }

    chunks
}

/// Mesh of chunk `(cx, cz)` from a `hm_w × hm_h` height grid (runtime
/// edits patch it in place: [`rebuild_dirty_chunks`]).
pub fn build_chunk(
    heights: &[f32],
    hm_w: usize,
    hm_h: usize,
    cx: usize,
    cz: usize,
) -> TerrainChunk {
    let sq_size = spring_map::map_types::SQUARE_SIZE as f32;

    let vx_start = cx * CHUNK_SIZE;
    let vz_start = cz * CHUNK_SIZE;
    let vx_end = (vx_start + CHUNK_SIZE + 1).min(hm_w);
    let vz_end = (vz_start + CHUNK_SIZE + 1).min(hm_h);
    let local_w = vx_end - vx_start;
    let local_h = vz_end - vz_start;

    let num_verts = local_w * local_h;
    let mut positions = vec![[0.0f32; 3]; num_verts];
    let mut normals = vec![[0.0f32; 3]; num_verts];
    let mut uvs = Vec::with_capacity(num_verts);

    let origin_x = vx_start as f32 * sq_size;
    let origin_z = vz_start as f32 * sq_size;

    for local_z in 0..local_h {
        for local_x in 0..local_w {
            let u = (vx_start + local_x) as f32 / (hm_w - 1) as f32;
            let v = (vz_start + local_z) as f32 / (hm_h - 1) as f32;
            uvs.push([u, v]);
        }
    }
    fill_chunk_geometry(heights, hm_w, hm_h, cx, cz, &mut positions, &mut normals);

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
        }
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

/// Chunk `(cx, cz)`'s vertex positions (chunk-local, the heights from
/// `heights`) and smooth normals, written over `positions` and `normals`
/// (one per vertex, row-major): the geometry [`build_chunk`] makes, so a
/// reshaped chunk can be patched in its existing buffers.
fn fill_chunk_geometry(
    heights: &[f32],
    hm_w: usize,
    hm_h: usize,
    cx: usize,
    cz: usize,
    positions: &mut [[f32; 3]],
    normals: &mut [[f32; 3]],
) {
    let sq_size = spring_map::map_types::SQUARE_SIZE as f32;
    let vx_start = cx * CHUNK_SIZE;
    let vz_start = cz * CHUNK_SIZE;
    let local_w = (vx_start + CHUNK_SIZE + 1).min(hm_w) - vx_start;
    let local_h = (vz_start + CHUNK_SIZE + 1).min(hm_h) - vz_start;
    for local_z in 0..local_h {
        for local_x in 0..local_w {
            let height = heights[(vz_start + local_z) * hm_w + vx_start + local_x];
            positions[local_z * local_w + local_x] =
                [local_x as f32 * sq_size, height, local_z as f32 * sq_size];
        }
    }
    // The quads accumulate their face normals into their vertices; the
    // final pass normalises them.
    normals.fill([0.0; 3]);
    for quad_z in 0..local_h.saturating_sub(1) {
        for quad_x in 0..local_w.saturating_sub(1) {
            let top_left = quad_z * local_w + quad_x;
            let top_right = top_left + 1;
            let bottom_left = top_left + local_w;
            let bottom_right = bottom_left + 1;

            let pos_tl = Vec3::from(positions[top_left]);
            let pos_bl = Vec3::from(positions[bottom_left]);
            let pos_tr = Vec3::from(positions[top_right]);
            let pos_br = Vec3::from(positions[bottom_right]);

            let normal_1 = (pos_bl - pos_tl).cross(pos_tr - pos_tl);
            let normal_2 = (pos_bl - pos_tr).cross(pos_br - pos_tr);

            for idx in [top_left, bottom_left, top_right] {
                normals[idx] = (Vec3::from(normals[idx]) + normal_1).to_array();
            }
            for idx in [top_right, bottom_left, bottom_right] {
                normals[idx] = (Vec3::from(normals[idx]) + normal_2).to_array();
            }
        }
    }
    for n in normals {
        *n = Vec3::from(*n).normalize_or(Vec3::Y).to_array();
    }
}

/// Terrain chunks whose heights the sim changed (Shifting's blocks,
/// Hex Farm's towers), rebuilt once per rendered frame by
/// [`rebuild_dirty_chunks`] rather than on every tick that touched them:
/// a catch-up frame used to rebuild the same chunk once per tick.
#[derive(Resource, Default)]
pub struct DirtyTerrainChunks(pub HashSet<(usize, usize)>);

impl DirtyTerrainChunks {
    /// Mark the chunks holding heightmap vertices `x0..=x1` × `z0..=z1`;
    /// vertices on a chunk seam belong to both chunks.
    pub fn mark_vertices(&mut self, x0: usize, z0: usize, x1: usize, z1: usize) {
        for cz in z0.saturating_sub(1) / CHUNK_SIZE..=z1 / CHUNK_SIZE {
            for cx in x0.saturating_sub(1) / CHUNK_SIZE..=x1 / CHUNK_SIZE {
                self.0.insert((cx, cz));
            }
        }
    }
}

/// Patch the dirty chunks' positions and normals from the heightmap in
/// their existing buffers (indices and UVs never change). Bevy refits
/// their bounds from the changed asset (`calculate_bounds`).
pub fn rebuild_dirty_chunks(
    mut dirty: ResMut<DirtyTerrainChunks>,
    heightmap: Option<Res<Heightmap>>,
    chunks: Query<(&TerrainChunkCoord, &Mesh3d)>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    if dirty.0.is_empty() {
        return;
    }
    let Some(hm) = heightmap else {
        dirty.0.clear();
        return;
    };
    let (hw, hh) = hm.grid_size();
    for (coord, mesh) in &chunks {
        if !dirty.0.contains(&(coord.0, coord.1)) {
            continue;
        }
        let Some(mut mesh) = meshes.get_mut(&mesh.0) else {
            continue;
        };
        let mut normals = match mesh.remove_attribute(Mesh::ATTRIBUTE_NORMAL) {
            Some(VertexAttributeValues::Float32x3(n)) => n,
            _ => continue,
        };
        if let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
            && positions.len() == normals.len()
        {
            fill_chunk_geometry(
                hm.heights(),
                hw,
                hh,
                coord.0,
                coord.1,
                positions,
                &mut normals,
            );
        }
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    }
    dirty.0.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Patching a built chunk's buffers after a height edit gives the
    /// geometry of a chunk built from the edited heights.
    #[test]
    fn patched_chunk_matches_a_rebuilt_one() {
        let (w, h) = (CHUNK_SIZE * 2 + 1, CHUNK_SIZE + 1);
        let mut heights: Vec<f32> = (0..w * h).map(|i| (i % 7) as f32).collect();
        let mut mesh = build_chunk(&heights, w, h, 1, 0).mesh;
        for z in 10..30 {
            for x in CHUNK_SIZE..CHUNK_SIZE + 20 {
                heights[z * w + x] += 40.0;
            }
        }
        let mut normals = match mesh.remove_attribute(Mesh::ATTRIBUTE_NORMAL) {
            Some(VertexAttributeValues::Float32x3(n)) => n,
            _ => unreachable!(),
        };
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
        else {
            unreachable!()
        };
        fill_chunk_geometry(&heights, w, h, 1, 0, positions, &mut normals);
        let rebuilt = build_chunk(&heights, w, h, 1, 0).mesh;
        let attr = |m: &Mesh, a| match m.attribute(a) {
            Some(VertexAttributeValues::Float32x3(v)) => v.clone(),
            _ => unreachable!(),
        };
        assert_eq!(
            attr(&mesh, Mesh::ATTRIBUTE_POSITION),
            attr(&rebuilt, Mesh::ATTRIBUTE_POSITION)
        );
        assert_eq!(normals, attr(&rebuilt, Mesh::ATTRIBUTE_NORMAL));
    }

    /// Chunk counts the draw budget is sized on: a 16×16 map (8192
    /// elmos, 1025² heights) and Hex Farm (12288 elmos, 1537²). Also
    /// pins the per-chunk vertex count under the u16 index limit so
    /// the builder's u32 indices are never a hidden requirement.
    #[test]
    fn chunk_counts_and_vertex_budget() {
        let side = |elmos: usize| elmos / spring_map::map_types::SQUARE_SIZE as usize + 1;
        let (w, hex) = (side(8192), side(12288));
        assert_eq!(w, 1025);
        assert_eq!(hex, 1537);
        let count = |n: usize| (n - 1).div_ceil(CHUNK_SIZE).pow(2);
        assert_eq!(count(w), 256);
        assert_eq!(count(hex), 576);
        assert!((CHUNK_SIZE + 1).pow(2) < usize::from(u16::MAX));

        let heights = vec![0.0; w * w];
        let chunks = generate_terrain_chunks(&heights, w, w);
        assert_eq!(chunks.len(), count(w));
        assert_eq!(chunks[0].mesh.count_vertices(), (CHUNK_SIZE + 1).pow(2));
    }
}
