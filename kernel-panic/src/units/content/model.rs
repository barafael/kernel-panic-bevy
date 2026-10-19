//! The unit model and texture data the bundle carries: Spring `.s3o`
//! models and the RGBA pixels of their `.tga` textures, as the bake
//! parsed them. The parsers (`spring-unit-mesh`) are in the git history
//! (README.md); the game only ever reads the baked form.

use serde::{Deserialize, Serialize};

/// A parsed `.s3o` unit model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3OModel {
    /// Bounding sphere radius. Falls back to half the AABB diagonal when the
    /// header value is <= 0.01 (matches upstream `S3OParser::Load`).
    pub radius: f32,
    /// Total model height. Falls back to `maxs.y - mins.y` when the header
    /// value is <= 0.01.
    pub height: f32,
    /// Model center point `[x, y, z]`.
    pub midpoint: [f32; 3],
    /// World-space minimum corner of the AABB enclosing every piece that has
    /// vertices, with each piece's accumulated parent offsets applied. Empty
    /// pieces (no vertices) don't contribute. `[0; 3]` for models with no
    /// geometry at all (e.g. `nullobject.s3o`).
    pub mins: [f32; 3],
    /// World-space maximum corner of the AABB. See [`Self::mins`].
    pub maxs: [f32; 3],
    /// Primary texture filename (typically diffuse color). Empty when the
    /// header offset is 0 (= "no texture").
    pub texture1: String,
    /// Secondary texture filename (typically team-color or specular map).
    /// Empty when the header offset is 0.
    pub texture2: String,
    /// Root of the piece hierarchy.
    pub root_piece: S3OPiece,
}

/// A single piece (sub-mesh) in the s3o hierarchy.
///
/// Each piece has its own vertex/index data and an offset relative to its
/// parent. The full model is a tree of pieces rooted at [`S3OModel::root_piece`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3OPiece {
    /// Piece name (e.g. "base", "turret", "barrel").
    pub name: String,
    /// Position offset from parent piece `[x, y, z]`.
    pub offset: [f32; 3],
    /// Vertex data for this piece.
    pub vertices: Vec<S3OVertex>,
    /// Triangle indices into [`vertices`](Self::vertices).
    ///
    /// Always a flat triangle list (length is a multiple of 3), regardless
    /// of the original primitive type in the file (strips and quads were
    /// converted by the parser).
    pub indices: Vec<u32>,
    /// Local-space minimum corner of this piece's vertex AABB. `[0; 3]` for
    /// pieces with no vertices (typically empty container pieces).
    pub mins: [f32; 3],
    /// Local-space maximum corner of this piece's vertex AABB. `[0; 3]` for
    /// pieces with no vertices.
    pub maxs: [f32; 3],
    /// Child pieces attached to this one.
    pub children: Vec<S3OPiece>,
}

/// A single vertex with position, normal, and texture coordinates.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct S3OVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub texcoord: [f32; 2],
}

/// A decoded texture.
#[derive(Clone)]
pub struct TgaImage {
    pub width: u32,
    pub height: u32,
    /// RGBA8 pixel data, row-major from top-left, `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}
