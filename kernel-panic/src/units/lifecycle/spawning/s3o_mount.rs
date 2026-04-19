//! S3O piece-tree walking helpers used while mounting a fresh unit.
//!
//! Each helper is a depth-first traversal with a slightly different
//! accumulator: flatten all pieces in order, resolve a piece by DFS index,
//! resolve by name, compute the unit's ground-lift offset, or turn one
//! piece's geometry into a Bevy mesh. They're grouped here so `mod.rs`
//! isn't dominated by traversal plumbing.

use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use spring_unit_mesh::S3OPiece;

/// Flatten the piece tree depth-first, recording each piece's parent index.
pub(super) fn flatten_pieces(
    piece: &S3OPiece,
    parent_idx: Option<usize>,
    result: &mut Vec<Option<usize>>,
    offsets: &mut Vec<[f32; 3]>,
) {
    let my_idx = result.len();
    result.push(parent_idx);
    offsets.push(piece.offset);
    for child in &piece.children {
        flatten_pieces(child, Some(my_idx), result, offsets);
    }
}

/// Get a piece by its flattened index (depth-first order).
pub(super) fn get_piece_by_index(root: &S3OPiece, target: usize) -> Option<&S3OPiece> {
    let mut counter = 0;
    get_piece_recursive(root, target, &mut counter)
}

fn get_piece_recursive<'a>(
    piece: &'a S3OPiece,
    target: usize,
    counter: &mut usize,
) -> Option<&'a S3OPiece> {
    if *counter == target {
        return Some(piece);
    }
    *counter += 1;
    for child in &piece.children {
        if let Some(found) = get_piece_recursive(child, target, counter) {
            return Some(found);
        }
    }
    None
}

/// Find the flattened (depth-first) index of the first piece whose name
/// matches `target` case-insensitively. `None` if the model has no such
/// piece. Used by factories to cache their `nanoemitter` / `pad` indices.
pub(super) fn find_piece_index_by_name(root: &S3OPiece, target: &str) -> Option<usize> {
    let mut counter = 0;
    find_by_name_recursive(root, target, &mut counter)
}

fn find_by_name_recursive(piece: &S3OPiece, target: &str, counter: &mut usize) -> Option<usize> {
    if piece.name.eq_ignore_ascii_case(target) {
        return Some(*counter);
    }
    *counter += 1;
    for child in &piece.children {
        if let Some(found) = find_by_name_recursive(child, target, counter) {
            return Some(found);
        }
    }
    None
}

/// Walk the piece tree and return the Y-offset that lands the model's
/// lowest vertex on the heightmap, in elmos. Positive values lift the
/// model up (the root sits above the lowest vertex — e.g. Byte's
/// octaeder.s3o has blade vertices spanning y∈[-48,48], so `lift = 48`).
/// Negative values sink the model down (the root sits *below* the
/// lowest vertex — e.g. `carrier.s3o / network_base.s3o` is authored
/// with its base above the piece-tree origin, which reads as "floating"
/// when planted at heightmap y). Zero means the lowest vertex is
/// already at y=0 and no adjustment is needed.
///
/// Returning `-min_y` unconditionally handles all three cases with the
/// same formula, so the caller can just `position + lift * Y` without
/// branching.
pub(super) fn compute_ground_lift(piece: &S3OPiece, parent_origin: [f32; 3]) -> f32 {
    let min_y = walk_min_y(piece, parent_origin);
    if min_y.is_finite() { -min_y } else { 0.0 }
}

fn walk_min_y(piece: &S3OPiece, parent_origin: [f32; 3]) -> f32 {
    let origin = [
        parent_origin[0] + piece.offset[0],
        parent_origin[1] + piece.offset[1],
        parent_origin[2] + piece.offset[2],
    ];
    let mut min_y = f32::INFINITY;
    for v in &piece.vertices {
        min_y = min_y.min(origin[1] + v.position[1]);
    }
    for child in &piece.children {
        min_y = min_y.min(walk_min_y(child, origin));
    }
    min_y
}

/// Convert one S3O piece's geometry into a Bevy mesh.
pub(super) fn piece_to_mesh(piece: &S3OPiece) -> Mesh {
    let positions: Vec<[f32; 3]> = piece.vertices.iter().map(|v| v.position).collect();
    let normals: Vec<[f32; 3]> = piece.vertices.iter().map(|v| v.normal).collect();
    let uvs: Vec<[f32; 2]> = piece.vertices.iter().map(|v| v.texcoord).collect();

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::RENDER_WORLD | bevy::asset::RenderAssetUsages::MAIN_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(piece.indices.clone()));
    mesh
}
