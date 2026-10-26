//! BC1 (DXT1) encoding of the ground texture.
//!
//! Spring keeps the ground as the SMT's DXT1 tiles, mips included; the
//! baked maps carry it decoded, and as RGBA8 an 8192² ground with its
//! chain is ~341 MB to upload in one frame and keep in VRAM. Re-encoded
//! it is an eighth of that. Pixels that came from DXT1 tiles hold at
//! most four colours on a line per block, which the principal-axis fit
//! below recovers almost exactly; the many single-colour blocks of
//! Kernel Panic's grounds take a fast path.

use bevy::math::Vec3;

/// Is every pixel of `rgba` opaque enough for BC1's opaque mode? A
/// ground with real transparency stays RGBA8.
pub(super) fn opaque(rgba: &[u8]) -> bool {
    rgba.as_chunks::<4>().0.iter().all(|p| p[3] >= 128)
}

/// Encode a chained RGBA8 mip pyramid (level 0 `w × h`, each next level
/// halved, at least 1) as the matching chain of BC1 levels; smaller
/// than 4×4 levels take one (edge-padded) block per 4×4, as the GPU
/// lays them out. Rows of blocks are encoded in parallel on the compute
/// pool (the map is prepared off the main thread).
pub(super) fn encode_chain(rgba: &[u8], w: usize, h: usize, levels: u32) -> Vec<u8> {
    let mut out = vec![0u8; chain_len(w, h, levels)];
    let (mut lw, mut lh, mut src, mut dst) = (w, h, 0, 0);
    for _ in 0..levels {
        let len = lw * lh * 4;
        let blocks = lw.div_ceil(4) * lh.div_ceil(4) * 8;
        encode_level(&rgba[src..src + len], lw, lh, &mut out[dst..dst + blocks]);
        src += len;
        dst += blocks;
        lw = (lw / 2).max(1);
        lh = (lh / 2).max(1);
    }
    out
}

/// Bytes of a BC1 chain: 8 per 4×4 block of every level.
fn chain_len(w: usize, h: usize, levels: u32) -> usize {
    let (mut lw, mut lh, mut len) = (w, h, 0);
    for _ in 0..levels {
        len += lw.div_ceil(4) * lh.div_ceil(4) * 8;
        lw = (lw / 2).max(1);
        lh = (lh / 2).max(1);
    }
    len
}

/// Block rows per compute task: 64 rows of an 8192² level are ~1 MB of
/// texels.
const ROWS_PER_TASK: usize = 64;

fn encode_level(rgba: &[u8], w: usize, h: usize, out: &mut [u8]) {
    let row_bytes = w.div_ceil(4) * 8;
    let encode_rows = |first_row: usize, out: &mut [u8]| {
        let mut block = [[0u8; 3]; 16];
        for (r, row) in out.chunks_exact_mut(row_bytes).enumerate() {
            let by = first_row + r;
            for (bx, dst) in row.as_chunks_mut::<8>().0.iter_mut().enumerate() {
                for (i, texel) in block.iter_mut().enumerate() {
                    // Past the edge of a level narrower than a block:
                    // repeat the last texel.
                    let x = (bx * 4 + i % 4).min(w - 1);
                    let y = (by * 4 + i / 4).min(h - 1);
                    let p = (y * w + x) * 4;
                    *texel = [rgba[p], rgba[p + 1], rgba[p + 2]];
                }
                *dst = encode_block(&block);
            }
        }
    };
    let task_bytes = row_bytes * ROWS_PER_TASK;
    if out.len() <= task_bytes {
        encode_rows(0, out);
        return;
    }
    bevy::tasks::ComputeTaskPool::get_or_init(bevy::tasks::TaskPool::default).scope(|scope| {
        for (t, chunk) in out.chunks_mut(task_bytes).enumerate() {
            let encode_rows = &encode_rows;
            scope.spawn(async move { encode_rows(t * ROWS_PER_TASK, chunk) });
        }
    });
}

fn to_565(c: Vec3) -> u16 {
    let c = c.clamp(Vec3::ZERO, Vec3::splat(255.0));
    let r = (c.x * 31.0 / 255.0).round() as u16;
    let g = (c.y * 63.0 / 255.0).round() as u16;
    let b = (c.z * 31.0 / 255.0).round() as u16;
    (r << 11) | (g << 5) | b
}

fn from_565(c: u16) -> Vec3 {
    let r = (c >> 11) & 31;
    let g = (c >> 5) & 63;
    let b = c & 31;
    Vec3::new(
        f32::from((r << 3) | (r >> 2)),
        f32::from((g << 2) | (g >> 4)),
        f32::from((b << 3) | (b >> 2)),
    )
}

/// One 4×4 block (row-major texels) in opaque four-colour mode: the
/// endpoints are the extremes along the block's principal axis, refined
/// once by least squares over the indices they gave.
fn encode_block(block: &[[u8; 3]; 16]) -> [u8; 8] {
    let rgb = |[r, g, b]: [u8; 3]| Vec3::new(f32::from(r), f32::from(g), f32::from(b));
    if block.iter().all(|t| t == &block[0]) {
        let c = to_565(rgb(block[0]));
        return pack(c, c, 0);
    }
    let texels = block.map(rgb);
    let mean = texels.iter().sum::<Vec3>() / 16.0;
    let mut cov = [0.0f32; 6];
    for t in &texels {
        let d = *t - mean;
        cov[0] += d.x * d.x;
        cov[1] += d.x * d.y;
        cov[2] += d.x * d.z;
        cov[3] += d.y * d.y;
        cov[4] += d.y * d.z;
        cov[5] += d.z * d.z;
    }
    // Power iteration from the widest channel's direction.
    let mut axis = Vec3::new(cov[0], cov[3], cov[5]);
    for _ in 0..4 {
        axis = Vec3::new(
            cov[0] * axis.x + cov[1] * axis.y + cov[2] * axis.z,
            cov[1] * axis.x + cov[3] * axis.y + cov[4] * axis.z,
            cov[2] * axis.x + cov[4] * axis.y + cov[5] * axis.z,
        );
        axis = axis.normalize_or(Vec3::ONE.normalize());
    }
    let (mut lo, mut hi) = (texels[0], texels[0]);
    let (mut lo_d, mut hi_d) = (f32::INFINITY, f32::NEG_INFINITY);
    for t in &texels {
        let d = t.dot(axis);
        if d < lo_d {
            (lo_d, lo) = (d, *t);
        }
        if d > hi_d {
            (hi_d, hi) = (d, *t);
        }
    }
    let (c0, c1, indices) = fit(&texels, hi, lo);
    let (r0, r1, r_indices) =
        refine(&texels, indices).map_or((c0, c1, indices), |(a, b)| fit(&texels, a, b));
    if block_error(&texels, r0, r1, r_indices) < block_error(&texels, c0, c1, indices) {
        pack(r0, r1, r_indices)
    } else {
        pack(c0, c1, indices)
    }
}

/// Quantize the endpoints and pick each texel's nearest palette entry.
/// The endpoints are ordered `c0 > c1` (four-colour mode); equal ones
/// make a single-colour block.
fn fit(texels: &[Vec3; 16], a: Vec3, b: Vec3) -> (u16, u16, u32) {
    let (mut c0, mut c1) = (to_565(a), to_565(b));
    if c0 == c1 {
        return (c0, c1, 0);
    }
    if c0 < c1 {
        std::mem::swap(&mut c0, &mut c1);
    }
    let palette = palette(c0, c1);
    let mut indices = 0u32;
    for (i, t) in texels.iter().enumerate() {
        let best = (0..4)
            .min_by(|&x, &y| {
                palette[x]
                    .distance_squared(*t)
                    .total_cmp(&palette[y].distance_squared(*t))
            })
            .unwrap_or(0) as u32;
        indices |= best << (2 * i);
    }
    (c0, c1, indices)
}

fn palette(c0: u16, c1: u16) -> [Vec3; 4] {
    let (a, b) = (from_565(c0), from_565(c1));
    [a, b, (2.0 * a + b) / 3.0, (a + 2.0 * b) / 3.0]
}

/// Least-squares endpoints for the given indices (stb_dxt's
/// `RefineBlock`); `None` when every texel took one weight.
fn refine(texels: &[Vec3; 16], indices: u32) -> Option<(Vec3, Vec3)> {
    // Index → weight of endpoint 0.
    const W0: [f32; 4] = [1.0, 0.0, 2.0 / 3.0, 1.0 / 3.0];
    let (mut aa, mut bb, mut ab) = (0.0f32, 0.0f32, 0.0f32);
    let (mut at, mut bt) = (Vec3::ZERO, Vec3::ZERO);
    for (i, t) in texels.iter().enumerate() {
        let a = W0[((indices >> (2 * i)) & 3) as usize];
        let b = 1.0 - a;
        aa += a * a;
        bb += b * b;
        ab += a * b;
        at += a * *t;
        bt += b * *t;
    }
    let det = aa * bb - ab * ab;
    if det.abs() < 1e-6 {
        return None;
    }
    Some(((at * bb - bt * ab) / det, (bt * aa - at * ab) / det))
}

fn block_error(texels: &[Vec3; 16], c0: u16, c1: u16, indices: u32) -> f32 {
    let palette = palette(c0, c1);
    texels
        .iter()
        .enumerate()
        .map(|(i, t)| palette[((indices >> (2 * i)) & 3) as usize].distance_squared(*t))
        .sum()
}

fn pack(c0: u16, c1: u16, indices: u32) -> [u8; 8] {
    let [a, b] = c0.to_le_bytes();
    let [c, d] = c1.to_le_bytes();
    let [e, f, g, h] = indices.to_le_bytes();
    [a, b, c, d, e, f, g, h]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode one block back to texels (the reference decoder: c0 > c1
    /// four-colour mode, otherwise three colours and black).
    fn decode(block: [u8; 8]) -> [Vec3; 16] {
        let c0 = u16::from_le_bytes([block[0], block[1]]);
        let c1 = u16::from_le_bytes([block[2], block[3]]);
        let idx = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
        let pal = if c0 > c1 {
            palette(c0, c1)
        } else {
            let (a, b) = (from_565(c0), from_565(c1));
            [a, b, (a + b) / 2.0, Vec3::ZERO]
        };
        std::array::from_fn(|i| pal[((idx >> (2 * i)) & 3) as usize])
    }

    #[test]
    fn solid_blocks_round_trip_through_565() {
        let c = [255, 0, 255];
        let block = encode_block(&[c; 16]);
        assert!(
            decode(block)
                .iter()
                .all(|t| *t == Vec3::new(255.0, 0.0, 255.0))
        );
    }

    /// A block that already is DXT1 (two 565 endpoints and their thirds)
    /// comes back exactly.
    #[test]
    fn dxt1_blocks_come_back_exactly() {
        let (c0, c1) = (
            to_565(Vec3::new(240.0, 40.0, 200.0)),
            to_565(Vec3::new(10.0, 90.0, 20.0)),
        );
        let (c0, c1) = (c0.max(c1), c0.min(c1));
        let pal = palette(c0, c1);
        let source: [[u8; 3]; 16] = std::array::from_fn(|i| {
            let p = pal[(i * 7 + i / 3) % 4].round();
            [p.x as u8, p.y as u8, p.z as u8]
        });
        let back = decode(encode_block(&source));
        for (s, b) in source.iter().zip(back) {
            let s = Vec3::new(f32::from(s[0]), f32::from(s[1]), f32::from(s[2]));
            assert!(s.distance(b) < 1.5, "{s} vs {b}");
        }
    }

    /// Four colours on a line for sixteen steps: every channel within
    /// about half a palette step (a sixth of the range) of its own.
    #[test]
    fn a_gradient_stays_close() {
        let source: [[u8; 3]; 16] = std::array::from_fn(|i| {
            let v = (i * 16) as u8;
            [v, v / 2, 255 - v]
        });
        let back = decode(encode_block(&source));
        for (s, b) in source.iter().zip(back) {
            let s = Vec3::new(f32::from(s[0]), f32::from(s[1]), f32::from(s[2]));
            assert!((s - b).abs().max_element() < 40.0, "{s} vs {b}");
        }
    }

    #[test]
    fn chain_lays_out_every_level_in_blocks() {
        // 8×4, 4×2, 2×1, 1×1: 2 + 1 + 1 + 1 blocks.
        let (w, h, levels) = (8, 4, 4);
        let texels = 8 * 4 + 4 * 2 + 2 + 1;
        let rgba = vec![200u8; texels * 4];
        let chain = encode_chain(&rgba, w, h, levels);
        assert_eq!(chain.len(), 5 * 8);
        assert_eq!(chain.len(), chain_len(w, h, levels));
    }
}
