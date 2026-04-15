//! Parser for Spring RTS engine `.s3o` unit model files and their textures.
//!
//! The s3o format stores a hierarchical tree of mesh pieces, each with
//! vertices, normals, UVs, and triangle indices. This crate is
//! engine-agnostic — it produces plain data structures that any renderer
//! can consume.

mod s3o_types;
pub mod tga;

pub use s3o_types::{S3OModel, S3OParseError, S3OPiece, S3OVertex};
pub use tga::{TgaImage, TgaParseError, parse_tga};

use std::io::{Cursor, Read, Seek, SeekFrom};

use byteorder::{LittleEndian, ReadBytesExt};
use s3o_types::PrimitiveType;

const S3O_MAGIC: &[u8; 12] = b"Spring unit\0";

/// Parse an `.s3o` model from raw file bytes.
pub fn parse_s3o(data: &[u8]) -> Result<S3OModel, S3OParseError> {
    let mut cursor = Cursor::new(data);

    // --- Header (44 bytes) ---
    let mut magic = [0u8; 12];
    cursor.read_exact(&mut magic)?;
    if magic != *S3O_MAGIC {
        return Err(S3OParseError::BadMagic);
    }

    let version = cursor.read_u32::<LittleEndian>()?;
    if version != 0 {
        return Err(S3OParseError::BadVersion(version));
    }

    let radius = cursor.read_f32::<LittleEndian>()?;
    let height = cursor.read_f32::<LittleEndian>()?;
    let midpoint = [
        cursor.read_f32::<LittleEndian>()?,
        cursor.read_f32::<LittleEndian>()?,
        cursor.read_f32::<LittleEndian>()?,
    ];
    let root_piece_offset = cursor.read_u32::<LittleEndian>()?;
    let _collision_data_offset = cursor.read_u32::<LittleEndian>()?;
    let texture1_offset = cursor.read_u32::<LittleEndian>()?;
    let texture2_offset = cursor.read_u32::<LittleEndian>()?;

    // --- Texture names ---
    let texture1 = read_null_terminated_string(data, texture1_offset as usize)?;
    let texture2 = read_null_terminated_string(data, texture2_offset as usize)?;

    // --- Piece tree ---
    let root_piece = parse_piece(data, root_piece_offset as usize)?;

    Ok(S3OModel {
        radius,
        height,
        midpoint,
        texture1,
        texture2,
        root_piece,
    })
}

fn parse_piece(data: &[u8], offset: usize) -> Result<S3OPiece, S3OParseError> {
    if offset + 52 > data.len() {
        return Err(S3OParseError::PieceTruncated);
    }

    let mut cursor = Cursor::new(data);
    cursor.seek(SeekFrom::Start(offset as u64))?;

    let name_offset = cursor.read_u32::<LittleEndian>()? as usize;
    let num_children = cursor.read_u32::<LittleEndian>()? as usize;
    let children_offset = cursor.read_u32::<LittleEndian>()? as usize;
    let num_verts = cursor.read_u32::<LittleEndian>()? as usize;
    let verts_offset = cursor.read_u32::<LittleEndian>()? as usize;
    let _vert_type = cursor.read_u32::<LittleEndian>()?;
    let primitive_type_raw = cursor.read_u32::<LittleEndian>()?;
    let vert_table_size = cursor.read_u32::<LittleEndian>()? as usize;
    let vert_table_offset = cursor.read_u32::<LittleEndian>()? as usize;
    let _collision_offset = cursor.read_u32::<LittleEndian>()?;
    let piece_offset = [
        cursor.read_f32::<LittleEndian>()?,
        cursor.read_f32::<LittleEndian>()?,
        cursor.read_f32::<LittleEndian>()?,
    ];

    let primitive_type = PrimitiveType::from_u32(primitive_type_raw)?;

    let name = read_null_terminated_string(data, name_offset)?;

    // --- Vertices ---
    let vertices = parse_vertices(data, verts_offset, num_verts)?;

    // --- Indices ---
    let raw_indices = parse_indices(data, vert_table_offset, vert_table_size)?;

    let indices = to_triangle_indices(raw_indices, primitive_type)?;

    // --- Children ---
    let children = parse_children(data, children_offset, num_children)?;

    Ok(S3OPiece {
        name,
        offset: piece_offset,
        vertices,
        indices,
        children,
    })
}

fn parse_vertices(
    data: &[u8],
    offset: usize,
    count: usize,
) -> Result<Vec<S3OVertex>, S3OParseError> {
    let byte_len = count * 32;
    if offset + byte_len > data.len() {
        return Err(S3OParseError::VertexDataTruncated {
            expected: byte_len,
            available: data.len().saturating_sub(offset),
        });
    }

    let mut cursor = Cursor::new(data);
    cursor.seek(SeekFrom::Start(offset as u64))?;

    let mut vertices = Vec::with_capacity(count);
    for _ in 0..count {
        let position = [
            cursor.read_f32::<LittleEndian>()?,
            cursor.read_f32::<LittleEndian>()?,
            cursor.read_f32::<LittleEndian>()?,
        ];
        let normal = [
            cursor.read_f32::<LittleEndian>()?,
            cursor.read_f32::<LittleEndian>()?,
            cursor.read_f32::<LittleEndian>()?,
        ];
        let texcoord = [
            cursor.read_f32::<LittleEndian>()?,
            cursor.read_f32::<LittleEndian>()?,
        ];
        vertices.push(S3OVertex {
            position,
            normal,
            texcoord,
        });
    }
    Ok(vertices)
}

fn parse_indices(data: &[u8], offset: usize, count: usize) -> Result<Vec<u32>, S3OParseError> {
    let byte_len = count * 4;
    if offset + byte_len > data.len() {
        return Err(S3OParseError::IndexDataTruncated {
            expected: byte_len,
            available: data.len().saturating_sub(offset),
        });
    }

    let mut cursor = Cursor::new(data);
    cursor.seek(SeekFrom::Start(offset as u64))?;

    let mut indices = Vec::with_capacity(count);
    for _ in 0..count {
        indices.push(cursor.read_u32::<LittleEndian>()?);
    }
    Ok(indices)
}

/// Convert raw indices into a plain triangle list regardless of the source
/// primitive type.
fn to_triangle_indices(
    raw: Vec<u32>,
    primitive_type: PrimitiveType,
) -> Result<Vec<u32>, S3OParseError> {
    match primitive_type {
        PrimitiveType::Triangles => {
            if !raw.len().is_multiple_of(3) {
                return Err(S3OParseError::InvalidIndexCount {
                    count: raw.len(),
                    primitive: "triangles",
                });
            }
            Ok(raw)
        }
        PrimitiveType::TriangleStrips => {
            if raw.len() < 3 {
                return Ok(vec![]);
            }
            let mut tris = Vec::with_capacity((raw.len() - 2) * 3);
            for i in 0..raw.len() - 2 {
                if i % 2 == 0 {
                    tris.extend_from_slice(&[raw[i], raw[i + 1], raw[i + 2]]);
                } else {
                    // Flip winding on odd triangles to maintain consistent face orientation.
                    tris.extend_from_slice(&[raw[i + 1], raw[i], raw[i + 2]]);
                }
            }
            Ok(tris)
        }
        PrimitiveType::Quads => {
            if !raw.len().is_multiple_of(4) {
                return Err(S3OParseError::InvalidIndexCount {
                    count: raw.len(),
                    primitive: "quads",
                });
            }
            let mut tris = Vec::with_capacity(raw.len() / 4 * 6);
            for quad in raw.chunks_exact(4) {
                tris.extend_from_slice(&[quad[0], quad[1], quad[2]]);
                tris.extend_from_slice(&[quad[0], quad[2], quad[3]]);
            }
            Ok(tris)
        }
    }
}

fn parse_children(
    data: &[u8],
    offset: usize,
    count: usize,
) -> Result<Vec<S3OPiece>, S3OParseError> {
    if count == 0 {
        return Ok(vec![]);
    }

    let byte_len = count * 4;
    if offset + byte_len > data.len() {
        return Err(S3OParseError::PieceTruncated);
    }

    let mut cursor = Cursor::new(data);
    cursor.seek(SeekFrom::Start(offset as u64))?;

    let mut child_offsets = Vec::with_capacity(count);
    for _ in 0..count {
        child_offsets.push(cursor.read_u32::<LittleEndian>()? as usize);
    }

    let mut children = Vec::with_capacity(count);
    for child_offset in child_offsets {
        children.push(parse_piece(data, child_offset)?);
    }
    Ok(children)
}

fn read_null_terminated_string(data: &[u8], offset: usize) -> Result<String, S3OParseError> {
    if offset >= data.len() {
        return Err(S3OParseError::StringOutOfBounds { offset });
    }
    let slice = &data[offset..];
    let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    Ok(String::from_utf8_lossy(&slice[..end]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn models_dir() -> Option<PathBuf> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir.parent().unwrap_or(&manifest_dir);

        [
            workspace_root.join("upstream/Kernel-Panic/objects3d"),
            PathBuf::from("upstream/Kernel-Panic/objects3d"),
        ]
        .into_iter()
        .find(|p| p.is_dir())
    }

    fn textures_dir() -> Option<PathBuf> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir.parent().unwrap_or(&manifest_dir);

        [
            workspace_root.join("upstream/Kernel-Panic/unittextures"),
            PathBuf::from("upstream/Kernel-Panic/unittextures"),
        ]
        .into_iter()
        .find(|p| p.is_dir())
    }

    #[test]
    fn parse_cube() {
        let Some(dir) = models_dir() else {
            eprintln!("Skipping: models directory not found");
            return;
        };
        let data = std::fs::read(dir.join("cube.s3o")).unwrap();
        let model = parse_s3o(&data).unwrap();

        assert!(model.radius > 0.0);
        assert!(model.height > 0.0);
        assert!(!model.root_piece.name.is_empty());

        // The root piece may be an empty container with geometry in children.
        let total_verts = count_vertices(&model.root_piece);
        let total_tris = count_triangles(&model.root_piece);
        assert!(total_verts > 0);
        assert!(total_tris > 0);

        eprintln!(
            "cube.s3o: radius={:.1}, height={:.1}, piece=\"{}\", {} total verts, {} total tris, tex1=\"{}\", tex2=\"{}\"",
            model.radius,
            model.height,
            model.root_piece.name,
            total_verts,
            total_tris,
            model.texture1,
            model.texture2,
        );
    }

    #[test]
    fn parse_kernel() {
        let Some(dir) = models_dir() else {
            eprintln!("Skipping: models directory not found");
            return;
        };
        let data = std::fs::read(dir.join("kernel.s3o")).unwrap();
        let model = parse_s3o(&data).unwrap();

        // Kernel has child pieces (the blinking lights / sub-structures).
        let total_pieces = count_pieces(&model.root_piece);
        assert!(total_pieces >= 1);

        let total_verts = count_vertices(&model.root_piece);
        assert!(total_verts > 0);

        eprintln!(
            "kernel.s3o: {} pieces, {} total verts, midpoint=[{:.1}, {:.1}, {:.1}]",
            total_pieces, total_verts, model.midpoint[0], model.midpoint[1], model.midpoint[2],
        );
    }

    #[test]
    fn parse_all_models() {
        let Some(dir) = models_dir() else {
            eprintln!("Skipping: models directory not found");
            return;
        };

        let mut count = 0;
        let mut failures: Vec<String> = Vec::new();

        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("s3o") {
                continue;
            }

            let name = path.file_stem().unwrap_or_default().to_string_lossy();
            let data = std::fs::read(&path).unwrap();

            match parse_s3o(&data) {
                Ok(model) => {
                    let pieces = count_pieces(&model.root_piece);
                    let verts = count_vertices(&model.root_piece);
                    let tris = count_triangles(&model.root_piece);

                    // nullobject.s3o is deliberately empty — skip content assertions.
                    if name != "nullobject" {
                        assert!(verts > 0, "{name}: model has no vertices");
                        assert!(tris > 0, "{name}: model has no triangles");
                    }

                    eprintln!(
                        "  OK: {name} — {pieces} pieces, {verts} verts, {tris} tris, tex1=\"{}\"",
                        model.texture1,
                    );
                    count += 1;
                }
                Err(err) => {
                    failures.push(format!("{name}: {err}"));
                }
            }
        }

        if !failures.is_empty() {
            panic!(
                "{} model(s) failed to parse:\n  {}",
                failures.len(),
                failures.join("\n  ")
            );
        }

        eprintln!("All {count} models parsed successfully");
        assert!(count > 0, "expected at least one .s3o model");
    }

    #[test]
    fn texture_files_exist_for_models() {
        let Some(model_dir) = models_dir() else {
            eprintln!("Skipping: models directory not found");
            return;
        };
        let Some(tex_dir) = textures_dir() else {
            eprintln!("Skipping: textures directory not found");
            return;
        };

        let mut checked = 0;
        for entry in std::fs::read_dir(&model_dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("s3o") {
                continue;
            }

            let data = std::fs::read(&path).unwrap();
            let model = parse_s3o(&data).unwrap();
            let name = path.file_stem().unwrap_or_default().to_string_lossy();

            // texture1 is the primary diffuse — it should exist if referenced.
            if !model.texture1.is_empty() {
                let tex_path = tex_dir.join(&model.texture1);
                if !tex_path.exists() {
                    eprintln!(
                        "  WARN: {name} references missing texture1: {}",
                        model.texture1
                    );
                }
            }
            checked += 1;
        }
        assert!(checked > 0);
    }

    /// Every unit model used by Kernel Panic must load successfully
    /// and have its primary texture parseable.
    #[test]
    fn kp_unit_models_load_with_textures() {
        let Some(model_dir) = models_dir() else {
            eprintln!("Skipping: models directory not found");
            return;
        };
        let Some(tex_dir) = textures_dir() else {
            eprintln!("Skipping: textures directory not found");
            return;
        };

        // The 19 s3o models actually used by KP unit definitions.
        let kp_units: &[(&str, &str)] = &[
            ("kernel.s3o", "Kernel"),
            ("assembler.s3o", "Assembler"),
            ("ball.s3o", "Bit"),
            ("octaeder.s3o", "Byte"),
            ("cube.s3o", "Pointer"),
            ("socket.s3o", "Socket"),
            ("network_super.s3o", "Firewall"),
            ("holeNEW.s3o", "Hole"),
            ("bugNEW.s3o", "Bug"),
            ("wormNEW.s3o", "Worm"),
            ("virus.s3o", "Virus"),
            ("dos.s3o", "DOS"),
            ("window.s3o", "Window"),
            ("logic_bomb.s3o", "LogicBomb"),
            ("network_big.s3o", "Connection"),
            ("network_minifac.s3o", "Port"),
            ("network_spam.s3o", "Packet"),
            ("signal.s3o", "Signal"),
        ];

        for &(model_file, unit_name) in kp_units {
            let model_path = model_dir.join(model_file);
            let data = std::fs::read(&model_path).unwrap_or_else(|e| panic!("{unit_name}: {e}"));
            let model =
                parse_s3o(&data).unwrap_or_else(|e| panic!("{unit_name} ({model_file}): {e}"));

            let verts = count_vertices(&model.root_piece);
            let tris = count_triangles(&model.root_piece);
            assert!(verts > 0, "{unit_name}: no vertices");
            assert!(tris > 0, "{unit_name}: no triangles");

            // Primary texture must exist and parse.
            assert!(!model.texture1.is_empty(), "{unit_name}: empty texture1");
            let tex_path = tex_dir.join(&model.texture1);
            let tex_data = std::fs::read(&tex_path)
                .unwrap_or_else(|e| panic!("{unit_name}: texture1 '{}': {e}", model.texture1));
            let tga = crate::tga::parse_tga(&tex_data)
                .unwrap_or_else(|e| panic!("{unit_name}: TGA parse '{}': {e}", model.texture1));
            assert!(tga.width > 0 && tga.height > 0);
            assert_eq!(
                tga.pixels.len(),
                (tga.width * tga.height * 4) as usize,
                "{unit_name}: pixel data size mismatch"
            );

            eprintln!(
                "  OK: {unit_name} ({model_file}) — {verts} verts, {tris} tris, tex={}x{}",
                tga.width, tga.height,
            );
        }
    }

    /// All indices in every piece must reference valid vertices.
    #[test]
    fn indices_within_bounds() {
        let Some(dir) = models_dir() else {
            eprintln!("Skipping: models directory not found");
            return;
        };

        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("s3o") {
                continue;
            }
            let name = path.file_stem().unwrap_or_default().to_string_lossy();
            let data = std::fs::read(&path).unwrap();
            let model = parse_s3o(&data).unwrap();

            check_indices_recursive(&model.root_piece, &name);
        }
    }

    fn check_indices_recursive(piece: &S3OPiece, model_name: &str) {
        let vert_count = piece.vertices.len() as u32;
        for (i, &idx) in piece.indices.iter().enumerate() {
            assert!(
                idx < vert_count,
                "{model_name}/\"{}\": index[{i}]={idx} >= vertex count {vert_count}",
                piece.name,
            );
        }
        for child in &piece.children {
            check_indices_recursive(child, model_name);
        }
    }

    /// UV coordinates should be finite (not NaN/Inf).
    #[test]
    fn uv_coordinates_finite() {
        let Some(dir) = models_dir() else {
            eprintln!("Skipping: models directory not found");
            return;
        };

        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("s3o") {
                continue;
            }
            let name = path.file_stem().unwrap_or_default().to_string_lossy();
            let data = std::fs::read(&path).unwrap();
            let model = parse_s3o(&data).unwrap();

            check_uvs_recursive(&model.root_piece, &name);
        }
    }

    fn check_uvs_recursive(piece: &S3OPiece, model_name: &str) {
        for (i, v) in piece.vertices.iter().enumerate() {
            assert!(
                v.texcoord[0].is_finite() && v.texcoord[1].is_finite(),
                "{model_name}/\"{}\": vertex[{i}] has non-finite UV ({}, {})",
                piece.name,
                v.texcoord[0],
                v.texcoord[1],
            );
            assert!(
                v.normal[0].is_finite() && v.normal[1].is_finite() && v.normal[2].is_finite(),
                "{model_name}/\"{}\": vertex[{i}] has non-finite normal",
                piece.name,
            );
        }
        for child in &piece.children {
            check_uvs_recursive(child, model_name);
        }
    }

    /// The tex1 alpha channel should have non-zero content for game units
    /// (it provides the visible detail pattern).
    #[test]
    fn kp_textures_have_alpha_content() {
        let Some(tex_dir) = textures_dir() else {
            eprintln!("Skipping: textures directory not found");
            return;
        };

        // Textures used by the 19 KP game units.
        let kp_textures = [
            "kernel.tga",
            "assembler.tga",
            "sphere.tga",
            "octaeder.tga",
            "cube.tga",
            "socket.tga",
            "network_super.tga",
            "holeNEW.tga",
            "newbug.tga",
            "wormNEW.tga",
            "virus.tga",
            "dos.tga",
            "window.tga",
            "minetex.tga",
            "network_connection.tga",
            "network_port.tga",
            "network_packet.tga",
            "signal.tga",
        ];

        for tex_name in &kp_textures {
            let tex_path = tex_dir.join(tex_name);
            let data = std::fs::read(&tex_path).unwrap_or_else(|e| panic!("{tex_name}: {e}"));
            let tga = crate::tga::parse_tga(&data).unwrap_or_else(|e| panic!("{tex_name}: {e}"));

            let alpha_nonzero = tga.pixels.chunks_exact(4).filter(|px| px[3] > 0).count();
            let total = (tga.width * tga.height) as usize;

            assert!(
                alpha_nonzero > 0,
                "{tex_name}: alpha channel is entirely zero — no visible content"
            );

            eprintln!(
                "  {tex_name}: {alpha_nonzero}/{total} alpha>0 ({:.1}%)",
                100.0 * alpha_nonzero as f64 / total as f64,
            );
        }
    }

    #[test]
    fn reject_bad_magic() {
        let data = b"not a spring model file at all, just garbage bytes";
        assert!(matches!(parse_s3o(data), Err(S3OParseError::BadMagic)));
    }

    #[test]
    fn reject_truncated_header() {
        let data = b"Spring unit\0";
        assert!(parse_s3o(data).is_err());
    }

    fn count_pieces(piece: &S3OPiece) -> usize {
        1 + piece.children.iter().map(count_pieces).sum::<usize>()
    }

    fn count_vertices(piece: &S3OPiece) -> usize {
        piece.vertices.len() + piece.children.iter().map(count_vertices).sum::<usize>()
    }

    fn count_triangles(piece: &S3OPiece) -> usize {
        piece.indices.len() / 3 + piece.children.iter().map(count_triangles).sum::<usize>()
    }
}
