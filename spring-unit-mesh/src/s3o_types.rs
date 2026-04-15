use thiserror::Error;

/// A parsed `.s3o` unit model.
#[derive(Debug, Clone)]
pub struct S3OModel {
    /// Bounding sphere radius.
    pub radius: f32,
    /// Total model height.
    pub height: f32,
    /// Model center point `[x, y, z]`.
    pub midpoint: [f32; 3],
    /// Primary texture filename (typically diffuse color).
    pub texture1: String,
    /// Secondary texture filename (typically team-color or specular map).
    pub texture2: String,
    /// Root of the piece hierarchy.
    pub root_piece: S3OPiece,
}

/// A single piece (sub-mesh) in the s3o hierarchy.
///
/// Each piece has its own vertex/index data and an offset relative to its
/// parent. The full model is a tree of pieces rooted at [`S3OModel::root_piece`].
#[derive(Debug, Clone)]
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
    /// of the original primitive type in the file (strips and quads are
    /// converted during parsing).
    pub indices: Vec<u32>,
    /// Child pieces attached to this one.
    pub children: Vec<S3OPiece>,
}

/// A single vertex with position, normal, and texture coordinates.
#[derive(Debug, Clone, Copy)]
pub struct S3OVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub texcoord: [f32; 2],
}

/// Geometry primitive type stored in the s3o piece header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrimitiveType {
    Triangles,
    TriangleStrips,
    Quads,
}

impl PrimitiveType {
    pub(crate) fn from_u32(value: u32) -> Result<Self, S3OParseError> {
        match value {
            0 => Ok(Self::Triangles),
            1 => Ok(Self::TriangleStrips),
            2 => Ok(Self::Quads),
            other => Err(S3OParseError::UnknownPrimitiveType(other)),
        }
    }
}

#[derive(Debug, Error)]
pub enum S3OParseError {
    #[error("I/O error reading s3o: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid s3o magic bytes")]
    BadMagic,
    #[error("unsupported s3o version {0} (expected 0)")]
    BadVersion(u32),
    #[error("piece data truncated")]
    PieceTruncated,
    #[error("vertex data truncated: expected {expected} bytes, {available} available")]
    VertexDataTruncated { expected: usize, available: usize },
    #[error("index data truncated: expected {expected} bytes, {available} available")]
    IndexDataTruncated { expected: usize, available: usize },
    #[error("invalid index count {count} for {primitive} (not evenly divisible)")]
    InvalidIndexCount {
        count: usize,
        primitive: &'static str,
    },
    #[error("unknown primitive type {0}")]
    UnknownPrimitiveType(u32),
    #[error("string offset {offset} is out of bounds")]
    StringOutOfBounds { offset: usize },
}
