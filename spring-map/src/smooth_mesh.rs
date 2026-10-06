//! Spring's `SmoothHeightMesh` (`rts/Sim/Misc/SmoothHeightMesh.{h,cpp}`,
//! the global `smoothGround`), ported.
//!
//! Hovering aircraft (`CHoverAirMoveType`) hold their cruise altitude
//! above this mesh instead of the raw heightmap, so they glide over
//! ridges and pits instead of bobbing along every bump. The mesh is:
//!
//! 1. **a square max filter** of the corner heightmap, sampled every
//!    `resolution` squares, over a window of `±smoothRadius / resolution`
//!    cells (`FindMaximumColumnHeights` / `FindRadialMaximum` /
//!    `AdvanceMaximas` — a sliding window max; computed here with the
//!    same window, so the result is identical);
//! 2. **a separable box blur** of half that window (`BlurHorizontal`
//!    then `BlurVertical`, running sums), each output clamped to at
//!    least the real ground height under it.
//!
//! Modinfo defaults (`smoothMeshResDivider = 2`,
//! `smoothMeshSmoothRadius = 40`, Kernel Panic sets neither) give 16-elmo
//! cells and a 320-elmo max-filter radius.
//!
//! Runtime terrain changes (`CBasicMapDamage::RecalcArea` →
//! [`SmoothHeightMesh::map_changed`]) mark 32×32-cell quads dirty; one
//! sim frame after [`SMOOTH_MESH_UPDATE_DELAY`] frames each
//! [`SmoothHeightMesh::update`] call recomputes one quad's maxima, then
//! (once every dirty quad has its maxima) one quad's horizontal blur,
//! then one quad's vertical blur — exactly the engine's double-buffered
//! queue, including its seam quirk (`CopyMeshPart` refreshes `tempMesh`
//! quad by quad, so a later quad's vertical blur can read an already
//! finished neighbour).
//!
//! Lua's `Spring.SetSmoothMesh` (inside `SetSmoothMeshFunc`) is
//! [`SmoothHeightMesh::set_smooth_mesh`]: it writes the final mesh only,
//! so a later dynamic update of that quad recomputes it from the terrain.

use std::collections::VecDeque;

/// `SmoothHeightMeshNamespace::SMOOTH_MESH_UPDATE_DELAY` (`GAME_SPEED`).
pub const SMOOTH_MESH_UPDATE_DELAY: u64 = 30;
/// `SmoothHeightMeshNamespace::SAMPLES_PER_QUAD`.
pub const SAMPLES_PER_QUAD: usize = 32;
/// `modInfo.smoothMeshResDivider` default.
pub const DEFAULT_RES_DIVIDER: usize = 2;
/// `modInfo.smoothMeshSmoothRadius` default (heightmap squares).
pub const DEFAULT_SMOOTH_RADIUS: usize = 40;
/// Spring's `SQUARE_SIZE` in elmos.
const SQUARE_SIZE: f32 = crate::map_types::SQUARE_SIZE as f32;

/// `SmoothHeightMesh::MapChangeTrack`.
#[derive(Debug, Clone, Default)]
struct MapChangeTrack {
    damage_map: Vec<bool>,
    damage_queue: [VecDeque<usize>; 2],
    horizontal_blur_queue: VecDeque<usize>,
    vertical_blur_queue: VecDeque<usize>,
    width: usize,
    height: usize,
    queue_release_on_frame: u64,
    active_buffer: usize,
}

/// The smoothed height mesh aircraft fly over. Heights are passed in
/// on every call that reads the terrain (the engine reads
/// `readMap->GetCornerHeightMapSynced()`), as a row-major corner
/// heightmap of `(mapx + 1) × (mapy + 1)` vertices.
#[derive(Debug, Clone)]
pub struct SmoothHeightMesh {
    maxx: usize,
    maxy: usize,
    fresolution: f32,
    resolution: usize,
    smooth_radius: usize,
    /// Corner-heightmap row stride (`mapDims.mapxp1`).
    stride: usize,
    maxima_mesh: Vec<f32>,
    mesh: Vec<f32>,
    temp_mesh: Vec<f32>,
    orig_mesh: Vec<f32>,
    track: MapChangeTrack,
}

impl SmoothHeightMesh {
    /// `SmoothHeightMesh::Init` + `MakeSmoothMesh` with the modinfo
    /// defaults. `hm_width`/`hm_height` are the corner heightmap's
    /// vertex counts (`mapx + 1`, `mapy + 1`).
    pub fn new(heights: &[f32], hm_width: usize, hm_height: usize) -> Self {
        Self::with_params(
            heights,
            hm_width,
            hm_height,
            DEFAULT_RES_DIVIDER,
            DEFAULT_SMOOTH_RADIUS,
        )
    }

    /// `SmoothHeightMesh::Init(int2(mapx, mapy), res, smoothRad)`.
    pub fn with_params(
        heights: &[f32],
        hm_width: usize,
        hm_height: usize,
        res: usize,
        smooth_rad: usize,
    ) -> Self {
        assert_eq!(heights.len(), hm_width * hm_height);
        let (mapx, mapy) = (hm_width - 1, hm_height - 1);
        let res = res.max(1);
        // "we use SSE in performance sensitive code, don't let the
        // window size be too small."
        let smooth_rad = smooth_rad.max(4);
        let maxx = mapx / res;
        let maxy = mapy / res;
        let n = maxx * maxy;
        let tw = maxx / SAMPLES_PER_QUAD + usize::from(!maxx.is_multiple_of(SAMPLES_PER_QUAD));
        let th = maxy / SAMPLES_PER_QUAD + usize::from(!maxy.is_multiple_of(SAMPLES_PER_QUAD));
        let mut mesh = Self {
            maxx,
            maxy,
            fresolution: res as f32 * SQUARE_SIZE,
            resolution: res,
            smooth_radius: smooth_rad,
            stride: hm_width,
            maxima_mesh: vec![0.0; n],
            mesh: vec![0.0; n],
            temp_mesh: vec![0.0; n],
            orig_mesh: vec![0.0; n],
            track: MapChangeTrack {
                damage_map: vec![false; tw * th],
                width: tw,
                height: th,
                ..Default::default()
            },
        };
        mesh.make_smooth_mesh(heights);
        mesh
    }

    /// Mesh cells along x / z (`GetMaxX` / `GetMaxY`).
    pub fn dims(&self) -> (usize, usize) {
        (self.maxx, self.maxy)
    }

    /// Cell size in elmos (`GetResolution`).
    pub fn resolution(&self) -> f32 {
        self.fresolution
    }

    /// The current mesh, row-major (`GetMeshData`).
    pub fn mesh(&self) -> &[f32] {
        &self.mesh
    }

    /// The mesh as last fully rebuilt (`GetOriginalMeshData`).
    pub fn orig_mesh(&self) -> &[f32] {
        &self.orig_mesh
    }

    /// Whether dynamic-update work is queued (dirty quads or blur
    /// passes still to run).
    pub fn is_updating(&self) -> bool {
        let t = &self.track;
        t.damage_queue.iter().any(|q| !q.is_empty())
            || !t.horizontal_blur_queue.is_empty()
            || !t.vertical_blur_queue.is_empty()
    }

    /// `SmoothHeightMesh::GetHeight`: bilinear over the mesh, clamped
    /// to its extent (cells past the last one read as flat).
    pub fn get_height(&self, x: f32, z: f32) -> f32 {
        let (maxx, maxy) = (self.maxx, self.maxy);
        let x = (x / self.fresolution).clamp(0.0, maxx as f32);
        let y = (z / self.fresolution).clamp(0.0, maxy as f32);
        let sx = (x as usize).min(maxx - 1);
        let sy = (y as usize).min(maxy - 1);
        let dx = x - sx as f32;
        let dy = y - sy as f32;
        let sxp1 = (sx + 1).min(maxx - 1);
        let syp1 = (sy + 1).min(maxy - 1);
        let m = &self.mesh;
        let h1 = m[sx + sy * maxx];
        let h2 = m[sxp1 + sy * maxx];
        let h3 = m[sx + syp1 * maxx];
        let h4 = m[sxp1 + syp1 * maxx];
        let hi1 = h1 + (h2 - h1) * dx;
        let hi2 = h3 + (h4 - h3) * dx;
        hi1 + (hi2 - hi1) * dy
    }

    /// `SmoothHeightMesh::GetHeightAboveWater`.
    pub fn get_height_above_water(&self, x: f32, z: f32) -> f32 {
        self.get_height(x, z).max(0.0)
    }

    /// `SmoothHeightMesh::SetHeight`.
    pub fn set_height(&mut self, index: usize, h: f32) -> f32 {
        self.mesh[index] = h;
        h
    }

    /// `SmoothHeightMesh::AddHeight`.
    pub fn add_height(&mut self, index: usize, h: f32) -> f32 {
        self.mesh[index] += h;
        self.mesh[index]
    }

    /// `Spring.SetSmoothMesh(x, z, height [, terraform])`
    /// (`LuaSyncedCtrl::SetSmoothMesh`): quantize the world position to
    /// a cell (discarding off-mesh ones → `None`), then set it to
    /// `height`, or move it `terraform` of the way there. Returns the
    /// height difference applied.
    pub fn set_smooth_mesh(
        &mut self,
        x: f32,
        z: f32,
        h: f32,
        terraform: Option<f32>,
    ) -> Option<f32> {
        // `(int)(xl / res)`: truncation toward zero.
        let xi = (x / self.fresolution) as i64;
        let zi = (z / self.fresolution) as i64;
        if xi < 0 || xi > self.maxx as i64 - 1 || zi < 0 || zi > self.maxy as i64 - 1 {
            return None;
        }
        let index = zi as usize * self.maxx + xi as usize;
        let old = self.mesh[index];
        let height = match terraform {
            Some(t) => old + (h - old) * t,
            None => h,
        };
        self.set_height(index, height);
        Some(height - old)
    }

    /// `GetRealGroundHeight(x, y, resolution)`.
    #[inline]
    fn ground(&self, heights: &[f32], x: usize, y: usize) -> f32 {
        heights[(x + y * self.stride) * self.resolution]
    }

    fn win_size(&self) -> usize {
        self.smooth_radius / self.resolution
    }

    fn blur_size(&self) -> usize {
        (self.win_size() / 2).max(1)
    }

    /// Max filter for cells `x0..=x1 × y0..=y1` into `maxima_mesh`:
    /// each cell gets the highest sampled ground height in the
    /// `±winSize` square around it (window clamped to the mesh).
    fn update_maxima(&mut self, heights: &[f32], x0: usize, y0: usize, x1: usize, y1: usize) {
        let w = self.win_size();
        let (maxx, maxy) = (self.maxx, self.maxy);
        let cx0 = x0.saturating_sub(w);
        let cx1 = (x1 + w).min(maxx - 1);
        let cols = cx1 - cx0 + 1;
        // Column maxima over the row window, per output row.
        let mut col_max = vec![f32::NEG_INFINITY; cols * (y1 - y0 + 1)];
        let mut line = Vec::new();
        for (ci, x) in (cx0..=cx1).enumerate() {
            let ry0 = y0.saturating_sub(w);
            let ry1 = (y1 + w).min(maxy - 1);
            line.clear();
            line.extend((ry0..=ry1).map(|y| self.ground(heights, x, y)));
            sliding_max(&line, ry0, w, y0, y1, |y, v| {
                col_max[(y - y0) * cols + ci] = v;
            });
        }
        for y in y0..=y1 {
            let row = &col_max[(y - y0) * cols..(y - y0 + 1) * cols];
            let maxima = &mut self.maxima_mesh;
            sliding_max(row, cx0, w, x0, x1, |x, v| {
                maxima[x + y * maxx] = v;
            });
        }
    }

    /// `BlurHorizontal(map, min, max, blurSize, resolution, src, dst)`.
    fn blur_horizontal(
        &self,
        heights: &[f32],
        src: &[f32],
        dst: &mut [f32],
        min: [usize; 2],
        max: [usize; 2],
    ) {
        let line = self.maxx;
        let map_max_x = self.maxx as i64 - 1;
        let b = self.blur_size() as i64;
        let weight = 1.0 / (b * 2 + 1) as f32;
        let at = |i: i64, y: usize| src[i.clamp(0, map_max_x) as usize + y * line];
        for y in min[1]..=max[1] {
            let mut avg = 0.0f32;
            let (mut lv, mut rv) = (0.0f32, 0.0f32);
            let mut li = min[0] as i64 - b;
            let mut ri = min[0] as i64 + b;
            for x1 in li..=ri {
                avg += at(x1, y);
            }
            ri += 1;
            for x in min[0]..=max[0] {
                avg += -lv + rv;
                let gh = self.ground(heights, x, y);
                dst[x + y * line] = gh.max(avg * weight);
                lv = at(li, y);
                rv = at(ri, y);
                li += 1;
                ri += 1;
            }
        }
    }

    /// `BlurVertical(map, min, max, blurSize, resolution, src, dst)`.
    fn blur_vertical(
        &self,
        heights: &[f32],
        src: &[f32],
        dst: &mut [f32],
        min: [usize; 2],
        max: [usize; 2],
    ) {
        let line = self.maxx;
        let map_max_y = self.maxy as i64 - 1;
        let b = self.blur_size() as i64;
        let weight = 1.0 / (b * 2 + 1) as f32;
        let at = |x: usize, i: i64| src[x + i.clamp(0, map_max_y) as usize * line];
        for x in min[0]..=max[0] {
            let mut avg = 0.0f32;
            let (mut lv, mut rv) = (0.0f32, 0.0f32);
            let mut li = min[1] as i64 - b;
            let mut ri = min[1] as i64 + b;
            for y1 in li..=ri {
                avg += at(x, y1);
            }
            ri += 1;
            for y in min[1]..=max[1] {
                avg += -lv + rv;
                let gh = self.ground(heights, x, y);
                dst[x + y * line] = gh.max(avg * weight);
                lv = at(x, li);
                rv = at(x, ri);
                li += 1;
                ri += 1;
            }
        }
    }

    /// `SmoothHeightMesh::MakeSmoothMesh`: full rebuild (also what
    /// `Spring.RebuildSmoothMesh` does).
    pub fn make_smooth_mesh(&mut self, heights: &[f32]) {
        let (mx, my) = (self.maxx - 1, self.maxy - 1);
        self.update_maxima(heights, 0, 0, mx, my);
        let maxima = std::mem::take(&mut self.maxima_mesh);
        let mut temp = std::mem::take(&mut self.temp_mesh);
        self.blur_horizontal(heights, &maxima, &mut temp, [0, 0], [mx, my]);
        let mut mesh = std::mem::take(&mut self.mesh);
        self.blur_vertical(heights, &temp, &mut mesh, [0, 0], [mx, my]);
        self.orig_mesh.copy_from_slice(&mesh);
        temp.copy_from_slice(&mesh);
        self.maxima_mesh = maxima;
        self.temp_mesh = temp;
        self.mesh = mesh;
    }

    /// `SmoothHeightMesh::MapChanged(x1, z1, x2, z2)` — heightmap
    /// squares, inclusive — at sim frame `frame`.
    pub fn map_changed(&mut self, x1: i64, z1: i64, x2: i64, z2: i64, frame: u64) {
        let t = &mut self.track;
        let queue_was_empty = t.damage_queue[t.active_buffer].is_empty();
        let res = (self.resolution * SAMPLES_PER_QUAD) as i64;
        let r = self.smooth_radius as i64;
        let (w, h) = (t.width as i64, t.height as i64);
        // C++ integer division truncates toward zero, as does Rust's.
        let min = [((x1 - r) / res).max(0), ((z1 - r) / res).max(0)];
        let max = [
            ((x2 + r - 1) / res).min(w - 1),
            ((z2 + r - 1) / res).min(h - 1),
        ];
        for y in min[1]..=max[1] {
            for x in min[0]..=max[0] {
                let i = (x + y * w) as usize;
                if !t.damage_map[i] {
                    t.damage_map[i] = true;
                    t.damage_queue[t.active_buffer].push_back(i);
                }
            }
        }
        let queue_was_updated = !t.damage_queue[t.active_buffer].is_empty();
        if queue_was_empty && queue_was_updated {
            t.queue_release_on_frame = frame + SMOOTH_MESH_UPDATE_DELAY;
        }
    }

    /// `UpdateSmoothMeshRequired`: flip buffers when the current flush
    /// is done and the pending one is due; `true` while work remains.
    fn update_required(&mut self, frame: u64) -> bool {
        let t = &mut self.track;
        let flush = 1 - t.active_buffer;
        let complete = t.damage_queue[flush].is_empty()
            && t.horizontal_blur_queue.is_empty()
            && t.vertical_blur_queue.is_empty();
        if complete
            && !t.damage_queue[t.active_buffer].is_empty()
            && frame >= t.queue_release_on_frame
        {
            t.active_buffer = flush;
        }
        !complete
    }

    /// `SmoothHeightMesh::UpdateSmoothMesh`, once per sim frame: at most
    /// one quad's maxima, horizontal blur or vertical blur.
    pub fn update(&mut self, heights: &[f32], frame: u64) {
        if !self.update_required(frame) {
            return;
        }
        let flush = 1 - self.track.active_buffer;
        let update_maxima = !self.track.damage_queue[flush].is_empty();
        let do_horizontal = !self.track.horizontal_blur_queue.is_empty();
        let index = if update_maxima {
            self.track.damage_queue[flush].pop_front()
        } else if do_horizontal {
            self.track.horizontal_blur_queue.pop_front()
        } else {
            self.track.vertical_blur_queue.pop_front()
        }
        .expect("update_required guarantees a queued quad");

        let qx = index % self.track.width;
        let qy = index / self.track.width;
        let min = [
            (qx * SAMPLES_PER_QUAD).min(self.maxx - 1),
            (qy * SAMPLES_PER_QUAD).min(self.maxy - 1),
        ];
        let max = [
            (qx * SAMPLES_PER_QUAD + SAMPLES_PER_QUAD - 1).min(self.maxx - 1),
            (qy * SAMPLES_PER_QUAD + SAMPLES_PER_QUAD - 1).min(self.maxy - 1),
        ];

        if update_maxima {
            self.update_maxima(heights, min[0], min[1], max[0], max[1]);
            self.track.horizontal_blur_queue.push_back(index);
            self.track.damage_map[index] = false;
        } else if do_horizontal {
            let maxima = std::mem::take(&mut self.maxima_mesh);
            let mut temp = std::mem::take(&mut self.temp_mesh);
            self.blur_horizontal(heights, &maxima, &mut temp, min, max);
            self.maxima_mesh = maxima;
            self.temp_mesh = temp;
            self.track.vertical_blur_queue.push_back(index);
        } else {
            let temp = std::mem::take(&mut self.temp_mesh);
            let mut mesh = std::mem::take(&mut self.mesh);
            self.blur_vertical(heights, &temp, &mut mesh, min, max);
            self.temp_mesh = temp;
            // `CopyMeshPart(map.x, damageMin, damageMax, mesh, tempMesh)`
            for y in min[1]..=max[1] {
                let (a, b) = (min[0] + y * self.maxx, max[0] + y * self.maxx + 1);
                self.temp_mesh[a..b].copy_from_slice(&mesh[a..b]);
            }
            self.mesh = mesh;
        }
    }
}

/// Sliding-window maximum over `line` (whose first element is at index
/// `base`), window `±w` clamped to the line, reported for indices
/// `out0..=out1` via `emit(index, max)`. Monotonic deque, O(n).
fn sliding_max(
    line: &[f32],
    base: usize,
    w: usize,
    out0: usize,
    out1: usize,
    mut emit: impl FnMut(usize, f32),
) {
    let last = base + line.len() - 1;
    let mut dq: VecDeque<usize> = VecDeque::new();
    let mut next = out0.saturating_sub(w).max(base);
    for i in out0..=out1 {
        let hi = (i + w).min(last);
        while next <= hi {
            let v = line[next - base];
            while dq.back().is_some_and(|&j| line[j - base] <= v) {
                dq.pop_back();
            }
            dq.push_back(next);
            next += 1;
        }
        let lo = i.saturating_sub(w);
        while dq.front().is_some_and(|&j| j < lo) {
            dq.pop_front();
        }
        emit(i, line[dq[0] - base]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bumpy 257×257 corner heightmap (256×256 squares → 128×128
    /// cells): rolling hills, a sharp spike and a deep pit.
    fn terrain() -> (Vec<f32>, usize, usize) {
        let (w, h) = (257usize, 257usize);
        let mut v = vec![0.0f32; w * h];
        for z in 0..h {
            for x in 0..w {
                let (fx, fz) = (x as f32, z as f32);
                let mut y = 100.0 + 60.0 * (fx * 0.05).sin() * (fz * 0.07).cos();
                if (120..124).contains(&x) && (60..64).contains(&z) {
                    y += 400.0; // spike
                }
                if (40..80).contains(&x) && (150..200).contains(&z) {
                    y -= 90.0; // pit
                }
                v[z * w + x] = y;
            }
        }
        (v, w, h)
    }

    fn brute_max(m: &SmoothHeightMesh, hm: &[f32], x: usize, y: usize) -> f32 {
        let w = m.win_size();
        let mut best = f32::NEG_INFINITY;
        for cy in y.saturating_sub(w)..=(y + w).min(m.maxy - 1) {
            for cx in x.saturating_sub(w)..=(x + w).min(m.maxx - 1) {
                best = best.max(m.ground(hm, cx, cy));
            }
        }
        best
    }

    #[test]
    fn dims_follow_modinfo_defaults() {
        let (hm, w, h) = terrain();
        let m = SmoothHeightMesh::new(&hm, w, h);
        assert_eq!(m.dims(), (128, 128));
        assert_eq!(m.resolution(), 16.0);
        assert_eq!(m.win_size(), 20);
        assert_eq!(m.blur_size(), 10);
    }

    #[test]
    fn maxima_is_the_window_max() {
        let (hm, w, h) = terrain();
        let m = SmoothHeightMesh::new(&hm, w, h);
        for &(x, y) in &[(0, 0), (5, 90), (60, 30), (127, 127), (64, 64), (20, 100)] {
            assert_eq!(
                m.maxima_mesh[x + y * m.maxx],
                brute_max(&m, &hm, x, y),
                "({x},{y})"
            );
        }
    }

    /// The max filter keeps the mesh above the terrain around it, not
    /// just under it.
    #[test]
    fn mesh_clears_the_ground() {
        let (hm, w, h) = terrain();
        let m = SmoothHeightMesh::new(&hm, w, h);
        // Every blur input is a window max over ±winSize, so the blurred
        // value still clears every sample within ±(winSize - blurSize)
        // cells (160 elmos) of the cell, per axis.
        let r = m.win_size() - m.blur_size();
        for y in (0..m.maxy).step_by(3) {
            for x in (0..m.maxx).step_by(3) {
                let mut g = f32::NEG_INFINITY;
                for cy in y.saturating_sub(r)..=(y + r).min(m.maxy - 1) {
                    for cx in x.saturating_sub(r)..=(x + r).min(m.maxx - 1) {
                        g = g.max(m.ground(&hm, cx, cy));
                    }
                }
                assert!(m.mesh[x + y * m.maxx] >= g - 1e-3, "({x},{y})");
            }
        }
        // The spike lifts the mesh well beyond its own footprint: the
        // blurred max over ±320 elmos still carries a large share of it.
        let spike_cell = (61, 31);
        let near = m.mesh[(spike_cell.0 + 12) + spike_cell.1 * m.maxx];
        assert!(near > 300.0, "spike plateau {near}");
    }

    /// Smoothness: a box blur of width 2b+1 over a field bounded by
    /// `[lo, hi]` changes by at most `(hi - lo) / (2b + 1)` per cell,
    /// unless the ground clamp takes over (only at cells where the
    /// ground itself pokes above the blur).
    #[test]
    fn mesh_is_smooth() {
        let (hm, w, h) = terrain();
        let m = SmoothHeightMesh::new(&hm, w, h);
        let lo = m.maxima_mesh.iter().cloned().fold(f32::INFINITY, f32::min);
        let hi = m
            .maxima_mesh
            .iter()
            .cloned()
            .fold(f32::NEG_INFINITY, f32::max);
        let bound = (hi - lo) / (2 * m.blur_size() + 1) as f32 * 2.0 + 1e-2;
        let mut checked = 0;
        for y in 0..m.maxy {
            for x in 0..m.maxx - 1 {
                let (a, b) = (m.mesh[x + y * m.maxx], m.mesh[x + 1 + y * m.maxx]);
                let clamped = [a, b]
                    .iter()
                    .zip([m.ground(&hm, x, y), m.ground(&hm, x + 1, y)])
                    .any(|(v, g)| (*v - g).abs() < 1e-3);
                if !clamped {
                    assert!((a - b).abs() <= bound, "({x},{y}) {a} vs {b} > {bound}");
                    checked += 1;
                }
            }
        }
        assert!(checked > 10_000);
    }

    #[test]
    fn sliding_max_matches_brute_force() {
        let line: Vec<f32> = (0..50).map(|i| ((i * 7919) % 31) as f32).collect();
        for w in [1, 3, 20, 60] {
            for (o0, o1) in [(0, 49), (10, 20), (49, 49)] {
                sliding_max(&line, 0, w, o0, o1, |i, v| {
                    let lo = i.saturating_sub(w);
                    let hi = (i + w).min(49);
                    let b = line[lo..=hi]
                        .iter()
                        .cloned()
                        .fold(f32::NEG_INFINITY, f32::max);
                    assert_eq!(v, b, "w={w} i={i}");
                });
            }
        }
    }

    /// Terrain edit → `map_changed` → per-frame `update` until idle
    /// reproduces a full rebuild on the new terrain (up to the running
    /// sums' float drift and the engine's quad-seam quirk, which only
    /// shows where a vertical blur reads a finished neighbour).
    #[test]
    fn region_update_matches_full_rebuild() {
        let (mut hm, w, h) = terrain();
        let mut m = SmoothHeightMesh::new(&hm, w, h);
        // Raise a plateau (heightmap squares 150..170 × 100..130).
        for z in 100..=130 {
            for x in 150..=170 {
                hm[z * w + x] += 250.0;
            }
        }
        m.map_changed(150, 100, 170, 130, 0);
        // Nothing happens before the release delay.
        for frame in 0..SMOOTH_MESH_UPDATE_DELAY {
            let before = m.mesh.clone();
            m.update(&hm, frame);
            assert_eq!(before, m.mesh);
        }
        let mut frame = SMOOTH_MESH_UPDATE_DELAY;
        let mut guard = 0;
        m.update(&hm, frame); // buffer flip frame
        while m.is_updating() {
            frame += 1;
            m.update(&hm, frame);
            guard += 1;
            assert!(guard < 10_000);
        }
        let full = SmoothHeightMesh::new(&hm, w, h);
        // Rows whose vertical blur window reaches into the quad above:
        // there `tempMesh` already holds that quad's finished mesh.
        let seam = |y: usize| y >= SAMPLES_PER_QUAD && y % SAMPLES_PER_QUAD < m.blur_size();
        let (mut worst, mut worst_seam) = (0.0f32, 0.0f32);
        for (i, (a, b)) in m.mesh.iter().zip(&full.mesh).enumerate() {
            let d = (a - b).abs();
            if seam(i / m.maxx) {
                worst_seam = worst_seam.max(d);
            } else {
                worst = worst.max(d);
            }
        }
        // Running sums drift a little over a 128-cell row vs a 32-cell quad.
        assert!(worst < 0.25, "max deviation {worst}");
        // The quirk stays a blur-sized ripple, far below the edit.
        assert!(worst_seam < 50.0, "seam deviation {worst_seam}");
        // And the plateau really propagated.
        let (cx, cy) = (80, 57);
        assert!(m.mesh[cx + cy * m.maxx] > 330.0);
        // Idle again: no work queued, nothing changes.
        let snapshot = m.mesh.clone();
        m.update(&hm, frame + 1);
        assert_eq!(snapshot, m.mesh);
    }

    #[test]
    fn set_smooth_mesh_quantizes_and_discards() {
        let (hm, w, h) = terrain();
        let mut m = SmoothHeightMesh::new(&hm, w, h);
        let old = m.mesh[3 + 2 * 128];
        assert_eq!(
            m.set_smooth_mesh(63.9, 47.0, 500.0, None),
            Some(500.0 - old)
        );
        assert_eq!(m.mesh[3 + 2 * 128], 500.0);
        assert_eq!(m.set_smooth_mesh(2048.0, 0.0, 1.0, None), None);
        // `(int)(-1 / 16)` truncates to cell 0, like the engine.
        let m0 = m.mesh[0];
        assert_eq!(
            m.set_smooth_mesh(-1.0, 0.0, 1.0, Some(0.5)),
            Some(0.5 * (1.0 - m0))
        );
        let before = m.mesh[5];
        m.set_smooth_mesh(80.0, 0.0, before + 10.0, Some(0.5));
        assert!((m.mesh[5] - (before + 5.0)).abs() < 1e-4);
    }
}
