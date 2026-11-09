//! `cargo xtask zisofs <extracted-dir> <out-dir>`: decode a
//! zisofs-compressed tree, what 7z leaves behind when it extracts an
//! mkzftree / AppImage-v1 ISO (files that start with the 8-byte magic
//! `37e45396c9dbd607`). Files without the magic are copied as they are.
//! Used to open the 2013 portablelinuxgames Kernel Panic 4.4 package.

use std::io::Read as _;
use std::path::Path;

use flate2::read::ZlibDecoder;

const MAGIC: [u8; 8] = [0x37, 0xe4, 0x53, 0x96, 0xc9, 0xdb, 0xd6, 0x07];

pub fn run(args: &[String]) -> Result<(), String> {
    let [src, dst] = args else {
        return Err("usage: cargo xtask zisofs <extracted-dir> <out-dir>".into());
    };
    let (src, dst) = (Path::new(src), Path::new(dst));
    let dst_abs = std::path::absolute(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    let src_abs = std::path::absolute(src).map_err(|e| format!("{}: {e}", src.display()))?;
    if dst_abs.starts_with(&src_abs) {
        return Err("the output directory is inside the input one".into());
    }
    let (mut decoded, mut copied) = (0usize, 0usize);
    walk(src, dst, &mut decoded, &mut copied)?;
    println!("decoded {decoded}, copied {copied}");
    Ok(())
}

fn walk(src: &Path, dst: &Path, decoded: &mut usize, copied: &mut usize) -> Result<(), String> {
    let io = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    std::fs::create_dir_all(dst).map_err(|e| io(dst, e))?;
    for entry in std::fs::read_dir(src).map_err(|e| io(src, e))? {
        let entry = entry.map_err(|e| io(src, e))?;
        let (from, to) = (entry.path(), dst.join(entry.file_name()));
        let kind = entry.file_type().map_err(|e| io(&from, e))?;
        if kind.is_dir() {
            walk(&from, &to, decoded, copied)?;
        } else if kind.is_file() {
            let data = std::fs::read(&from).map_err(|e| io(&from, e))?;
            let out = if data.starts_with(&MAGIC) {
                *decoded += 1;
                decode(&data).map_err(|e| format!("{}: {e}", from.display()))?
            } else {
                *copied += 1;
                data
            };
            std::fs::write(&to, out).map_err(|e| io(&to, e))?;
        }
    }
    Ok(())
}

/// Header: magic, u32 size, u8 header length in words, u8 log2 block
/// size; then `blocks + 1` u32 pointers to zlib streams. Two equal
/// pointers are a block of zeros.
fn decode(data: &[u8]) -> Result<Vec<u8>, String> {
    let u32_at = |at: usize| -> Result<usize, String> {
        data.get(at..at + 4)
            .and_then(|b| b.try_into().ok())
            .map(|b| u32::from_le_bytes(b) as usize)
            .ok_or_else(|| "truncated".to_string())
    };
    let size = u32_at(8)?;
    let (&header_words, &block_log2) = data
        .get(12)
        .zip(data.get(13))
        .ok_or_else(|| "truncated".to_string())?;
    if !(10..=24).contains(&block_log2) {
        return Err(format!("block size 2^{block_log2}"));
    }
    let block = 1usize << block_log2;
    let blocks = size.div_ceil(block);
    let table = usize::from(header_words) * 4;
    let mut out = Vec::with_capacity(size);
    for i in 0..blocks {
        let (a, b) = (u32_at(table + 4 * i)?, u32_at(table + 4 * i + 4)?);
        let want = block.min(size - i * block);
        if a == b {
            out.resize(out.len() + want, 0);
            continue;
        }
        let stream = data
            .get(a..b)
            .ok_or_else(|| format!("block {i}: bad pointers {a}..{b}"))?;
        let before = out.len();
        ZlibDecoder::new(stream)
            .take(want as u64)
            .read_to_end(&mut out)
            .map_err(|e| format!("block {i}: {e}"))?;
        if out.len() - before != want {
            return Err(format!("block {i}: {} bytes, expected {want}", out.len() - before));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use flate2::Compression;
    use flate2::write::ZlibEncoder;

    use super::*;

    /// A zisofs file of `blocks` (2 KiB each): `Some(bytes)` stored, `None` a hole.
    fn zisofs(size: usize, blocks: &[Option<Vec<u8>>]) -> Vec<u8> {
        let table = 16;
        let mut streams = Vec::new();
        let mut ptrs = Vec::new();
        let mut at = table + 4 * (blocks.len() + 1);
        for b in blocks {
            ptrs.push(at);
            if let Some(bytes) = b {
                let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
                z.write_all(bytes).unwrap();
                let z = z.finish().unwrap();
                at += z.len();
                streams.extend(z);
            }
        }
        ptrs.push(at);
        let mut d = MAGIC.to_vec();
        d.extend_from_slice(&(size as u32).to_le_bytes());
        d.extend_from_slice(&[4, 11, 0, 0]); // 16-byte header, 2 KiB blocks
        for p in ptrs {
            d.extend_from_slice(&(p as u32).to_le_bytes());
        }
        d.extend(streams);
        d
    }

    #[test]
    fn stored_blocks_holes_and_a_short_last_block() {
        let first = vec![7u8; 2048];
        let last = b"tail".to_vec();
        let d = zisofs(2048 * 2 + 4, &[Some(first.clone()), None, Some(last.clone())]);
        let out = decode(&d).unwrap();
        assert_eq!(out.len(), 2048 * 2 + 4);
        assert_eq!(out[..2048], first[..]);
        assert!(out[2048..4096].iter().all(|&b| b == 0));
        assert_eq!(out[4096..], last[..]);
    }

    #[test]
    fn damage_is_an_error() {
        let d = zisofs(4, &[Some(b"tail".to_vec())]);
        assert!(decode(&d[..d.len() - 3]).is_err());
        assert!(decode(&d[..10]).is_err());
    }
}
