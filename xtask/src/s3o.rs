//! `cargo xtask s3o <model.s3o>`: a model's piece tree with each piece's
//! offset from its parent and its vertex count. The animation drivers
//! (`kernel-panic/src/units/assets/animation/units/`) address pieces by
//! name, and this is the quick check of which pieces a model has and
//! where they hang. Layout: Recoil's `S3OParser.h` (`S3OHeader`, `S3OPiece`).

use std::fmt::Write as _;

const MAGIC: &[u8; 12] = b"Spring unit\0";
const HEADER_LEN: usize = 52;
const PIECE_LEN: usize = 52;
/// Deeper than any real model; stops a child pointer that loops back.
const MAX_DEPTH: usize = 64;

pub fn run(args: &[String]) -> Result<(), String> {
    let [path] = args else {
        return Err("usage: cargo xtask s3o <model.s3o>".into());
    };
    let data = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    print!("{}", piece_tree(&data).map_err(|e| format!("{path}: {e}"))?);
    Ok(())
}

fn piece_tree(data: &[u8]) -> Result<String, String> {
    if data.get(..12) != Some(&MAGIC[..]) {
        return Err("not an S3O file (no \"Spring unit\" magic)".into());
    }
    if data.len() < HEADER_LEN {
        return Err("truncated header".into());
    }
    let version = i32_at(data, 12)?;
    if version != 0 {
        return Err(format!("S3O version {version}, only 0 is known"));
    }
    let mut out = String::new();
    let (radius, height) = (f32_at(data, 16)?, f32_at(data, 20)?);
    let _ = writeln!(out, "# radius={radius:.1} height={height:.1}");
    piece(data, offset(i32_at(data, 36)?)?, 0, &mut out)?;
    Ok(out)
}

fn piece(data: &[u8], at: usize, depth: usize, out: &mut String) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!(
            "piece tree deeper than {MAX_DEPTH}: a child loops back"
        ));
    }
    if at + PIECE_LEN > data.len() {
        return Err(format!("piece at {at} runs past the end of the file"));
    }
    let name = cstr_at(data, offset(i32_at(data, at)?)?)?;
    let children = i32_at(data, at + 4)?;
    let children_at = offset(i32_at(data, at + 8)?)?;
    let vertices = i32_at(data, at + 12)?;
    let (x, y, z) = (
        f32_at(data, at + 40)?,
        f32_at(data, at + 44)?,
        f32_at(data, at + 48)?,
    );
    let _ = writeln!(
        out,
        "{}{name}  off=({x:.2},{y:.2},{z:.2}) verts={vertices}",
        "  ".repeat(depth)
    );
    for i in 0..usize::try_from(children).map_err(|_| format!("{name}: {children} children"))? {
        let child = offset(i32_at(data, children_at + 4 * i)?)?;
        piece(data, child, depth + 1, out)?;
    }
    Ok(())
}

fn offset(v: i32) -> Result<usize, String> {
    usize::try_from(v).map_err(|_| format!("negative offset {v}"))
}

fn word(data: &[u8], at: usize) -> Result<[u8; 4], String> {
    data.get(at..at + 4)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| format!("read past the end of the file at {at}"))
}

fn i32_at(data: &[u8], at: usize) -> Result<i32, String> {
    word(data, at).map(i32::from_le_bytes)
}

fn f32_at(data: &[u8], at: usize) -> Result<f32, String> {
    word(data, at).map(f32::from_le_bytes)
}

fn cstr_at(data: &[u8], at: usize) -> Result<String, String> {
    let tail = data
        .get(at..)
        .ok_or_else(|| format!("string at {at} is past the end"))?;
    let len = tail
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| format!("string at {at} has no terminator"))?;
    Ok(tail[..len].iter().map(|&b| char::from(b)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Header, a root with one child; names after the pieces.
    fn model() -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(MAGIC);
        d.extend_from_slice(&0i32.to_le_bytes()); // version
        for f in [10.0f32, 20.0, 0.0, 0.0, 0.0] {
            d.extend_from_slice(&f.to_le_bytes()); // radius, height, mid
        }
        for i in [HEADER_LEN as i32, 0, 0, 0] {
            d.extend_from_slice(&i.to_le_bytes()); // root, collision, tex1, tex2
        }
        let root = d.len();
        let child = root + PIECE_LEN;
        let child_table = child + PIECE_LEN;
        let names = child_table + 4;
        let piece = |name: usize, nch: i32, ch: usize, nv: i32, off: [f32; 3]| {
            let mut p = Vec::new();
            for i in [name as i32, nch, ch as i32, nv, 0, 0, 0, 0, 0, 0] {
                p.extend_from_slice(&i.to_le_bytes());
            }
            for f in off {
                p.extend_from_slice(&f.to_le_bytes());
            }
            p
        };
        d.extend(piece(names, 1, child_table, 0, [0.0; 3]));
        d.extend(piece(names + 5, 0, 0, 12, [1.0, 2.5, -3.0]));
        d.extend_from_slice(&(child as i32).to_le_bytes());
        d.extend_from_slice(b"base\0arm\0");
        d
    }

    #[test]
    fn prints_the_tree() {
        assert_eq!(
            piece_tree(&model()).unwrap(),
            "# radius=10.0 height=20.0\nbase  off=(0.00,0.00,0.00) verts=0\n  arm  off=(1.00,2.50,-3.00) verts=12\n"
        );
    }

    #[test]
    fn refuses_other_files_and_truncations() {
        assert!(piece_tree(b"not a model at all, really not").is_err());
        let d = model();
        assert!(piece_tree(&d[..d.len() - 12]).is_err());
        assert!(piece_tree(&d[..30]).is_err());
    }

    #[test]
    fn a_looping_child_is_an_error() {
        let mut d = model();
        let child_table = HEADER_LEN + 2 * PIECE_LEN;
        d[child_table..child_table + 4].copy_from_slice(&(HEADER_LEN as i32).to_le_bytes());
        assert!(piece_tree(&d).unwrap_err().contains("loops back"));
    }
}
