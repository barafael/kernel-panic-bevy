//! Per-model piece layout used while mounting a fresh unit.
//!
//! An s3o model is walked exactly once (depth-first, the order the COB
//! piece indices assume) into a [`PieceLayout`]: parent links, offsets,
//! the emit vector, and one shared `Handle<Mesh>` per piece with
//! geometry. `S3OModelCache` keeps the layout per model file, so every
//! later spawn of the same model is a flat loop over the specs — no
//! O(n²) `get_piece_by_index` rescans, no per-unit mesh uploads — and
//! all units of one model draw with the same mesh assets, which is what
//! lets the renderer batch them.

use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use spring_unit_mesh::{S3OModel, S3OPiece};

use crate::units::assets::animation::PieceEmit;

/// One piece of a flattened s3o model, in depth-first order.
pub struct PieceSpec {
    /// Piece name as authored (`base`, `turret`, …), matched
    /// case-insensitively by [`PieceLayout::index_by_name`].
    pub name: String,
    /// Index of the parent piece in the same layout; `None` for the root.
    pub parent: Option<usize>,
    /// Position offset from the parent piece.
    pub offset: [f32; 3],
    /// Shared mesh for the piece's geometry; `None` for empty pieces
    /// (pure pivots / emit points).
    pub mesh: Option<Handle<Mesh>>,
    /// Emit origin/direction derived from the first two vertices.
    pub emit: PieceEmit,
}

/// Everything `spawn_unit` needs from an s3o model, computed once per
/// model file.
pub struct PieceLayout {
    /// Y-offset that lands the model's lowest vertex on the heightmap
    /// (see [`compute_ground_lift`]).
    pub ground_lift: f32,
    /// Pieces in depth-first order (root first).
    pub pieces: Vec<PieceSpec>,
}

impl PieceLayout {
    /// Flatten `model` and upload one mesh per piece with geometry.
    pub fn build(model: &S3OModel, meshes: &mut Assets<Mesh>) -> Self {
        let mut pieces = Vec::new();
        flatten_pieces(&model.root_piece, None, meshes, &mut pieces);
        Self {
            ground_lift: compute_ground_lift(model),
            pieces,
        }
    }

    /// Depth-first index of the first piece named `target`
    /// (case-insensitive), or `None` if the model has no such piece.
    /// Used to map the static per-kind piece table onto the model and
    /// by factories to find their `nanoemitter` / `pad` pieces.
    pub fn index_by_name(&self, target: &str) -> Option<usize> {
        self.pieces
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case(target))
    }
}

/// Flatten the piece tree depth-first, recording each piece's parent index.
fn flatten_pieces(
    piece: &S3OPiece,
    parent: Option<usize>,
    meshes: &mut Assets<Mesh>,
    out: &mut Vec<PieceSpec>,
) {
    let my_idx = out.len();
    let emit_vertices: Vec<[f32; 3]> = piece.vertices.iter().take(2).map(|v| v.position).collect();
    out.push(PieceSpec {
        name: piece.name.clone(),
        parent,
        offset: piece.offset,
        mesh: (!piece.vertices.is_empty()).then(|| meshes.add(piece_to_mesh(piece))),
        emit: PieceEmit::from_vertices(&emit_vertices),
    });
    for child in &piece.children {
        flatten_pieces(child, Some(my_idx), meshes, out);
    }
}

/// Return the Y-offset that lands the model's lowest vertex on the
/// heightmap, in elmos. Positive values lift the model up (the root sits
/// above the lowest vertex — e.g. Byte's octaeder.s3o has blade vertices
/// spanning y∈[-48,48], so `lift = 48`). Negative values sink the model
/// down (the root sits *below* the lowest vertex — e.g.
/// `carrier.s3o / network_base.s3o` is authored with its base above the
/// piece-tree origin, which reads as "floating" when planted at heightmap
/// y). Zero means the lowest vertex is already at y=0 and no adjustment
/// is needed.
///
/// Returning `-mins.y` unconditionally handles all three cases with the
/// same formula, so the caller can just `position + lift * Y` without
/// branching.
fn compute_ground_lift(model: &S3OModel) -> f32 {
    -model.mins[1]
}

/// Convert one S3O piece's geometry into a Bevy mesh.
fn piece_to_mesh(piece: &S3OPiece) -> Mesh {
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
