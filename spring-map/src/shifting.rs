//! Shifting terrain: blocks of cells that rise out of the ground and
//! sink back during play, on a schedule fixed by the sim frame alone,
//! so every lockstep peer moves the same ground on the same frame.
//!
//! Two of the port's own maps use it:
//!
//! - **Token Ring** ([`token_ring`]): four platforms joined in a ring by
//!   bridges over the void. After a quiet opening, a firewall rises on
//!   one bridge at a time and passes round the ring like the token.
//! - **Heap Fragmentation** ([`heap`]): a lattice of void holes with
//!   lanes between them. Blocks allocate (rise) and free (sink) on the
//!   lane segments, a few every step, never cutting the heap apart.
//!
//! This module holds the rules (which blocks, which are up on a given
//! frame, the heights they write); the map generators draw the blocks
//! (`examples/`), the game's `map_events::shifting` applies them.

use spring_detmath::DetFloat;

use crate::cells::{CELL, CELL_SQ, Rect};

/// Sim frames per second.
pub const GAME_SPEED: u32 = 30;

/// A block of cells that can rise.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Block {
    pub rect: Rect,
    /// How far its top rises above the ground.
    pub rise: f32,
    /// The axis units on it are pushed out along when it rises: the
    /// way the ground continues (a bridge's or a lane's length).
    pub push_along_x: bool,
}

/// A map's shifting blocks and their schedule.
#[derive(Clone, Debug)]
pub struct Shifting {
    pub blocks: Vec<Block>,
    /// Frames the first state holds.
    pub first_frames: u32,
    /// Frames every later state holds.
    pub step_frames: u32,
    /// Frames a block takes to rise or sink.
    pub move_frames: u32,
    /// The states, one flag per block: `states[0]` first, then
    /// `states[1..]` in turn, round and round.
    pub states: Vec<Vec<bool>>,
}

impl Shifting {
    /// The shifting terrain of the map with this file stem, if any.
    pub fn for_map(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case(token_ring::MAP) {
            Some(token_ring::shifting())
        } else if name.eq_ignore_ascii_case(heap::MAP) {
            Some(heap::shifting())
        } else {
            None
        }
    }

    /// The step `frame` falls in, and how many frames into it.
    pub fn step_at(&self, frame: u32) -> (usize, u32) {
        if frame < self.first_frames {
            (0, frame)
        } else {
            let since = frame - self.first_frames;
            (
                1 + (since / self.step_frames) as usize,
                since % self.step_frames,
            )
        }
    }

    /// The state of step `step`.
    pub fn state(&self, step: usize) -> &[bool] {
        match step {
            0 => &self.states[0],
            n => &self.states[1 + (n - 1) % (self.states.len() - 1)],
        }
    }

    /// How far block `k` is up on `frame`: 0 down, 1 up, between while
    /// it moves (a block changes only at the start of a step).
    pub fn level(&self, frame: u32, k: usize) -> f32 {
        let (step, since) = self.step_at(frame);
        let to = self.state(step)[k] as u8 as f32;
        if step == 0 {
            return to;
        }
        let from = self.state(step - 1)[k] as u8 as f32;
        let t = (since as f32 / self.move_frames as f32).det_min(1.0);
        from + (to - from) * t
    }

    /// Is block `k` up (or rising) on `frame`?
    pub fn raised(&self, frame: u32, k: usize) -> bool {
        let (step, _) = self.step_at(frame);
        self.state(step)[k]
    }

    /// Heightmap vertices block `k` moves: the inside ones, so its edge
    /// stays at ground level and the rise is a sheer wall. Inclusive
    /// `[x0, z0, x1, z1]`.
    pub fn vertex_box(&self, k: usize) -> [usize; 4] {
        let r = self.blocks[k].rect;
        [
            r.x0 * CELL_SQ + 1,
            r.z0 * CELL_SQ + 1,
            r.x1 * CELL_SQ - 1,
            r.z1 * CELL_SQ - 1,
        ]
    }

    /// Write block `k` at `level` over `ground` (the map's own heights)
    /// into `heights`, both `hm_w` vertices wide. Returns the vertex box.
    pub fn write_block(
        &self,
        k: usize,
        level: f32,
        ground: &[f32],
        heights: &mut [f32],
        hm_w: usize,
    ) -> [usize; 4] {
        let b = self.vertex_box(k);
        let rise = self.blocks[k].rise * level;
        for z in b[1]..=b[3] {
            for x in b[0]..=b[2] {
                let i = z * hm_w + x;
                heights[i] = ground[i] + rise;
            }
        }
        b
    }

    /// The block at world point `(x, z)` that is up (or rising) on
    /// `frame`, if any.
    pub fn raised_block_at(&self, frame: u32, x: f32, z: f32) -> Option<usize> {
        let (cx, cz) = (x / CELL as f32, z / CELL as f32);
        if cx < 0.0 || cz < 0.0 {
            return None;
        }
        let (cx, cz) = (cx as usize, cz as usize);
        (0..self.blocks.len())
            .find(|&k| self.raised(frame, k) && self.blocks[k].rect.contains(cx, cz))
    }

    /// Where a unit at `(x, z)` on block `k` goes when it rises: off
    /// the nearer end along the block's push axis, `margin` elmos clear.
    pub fn push_out(&self, k: usize, x: f32, z: f32, margin: f32) -> (f32, f32) {
        let b = &self.blocks[k];
        let r = b.rect;
        let c = CELL as f32;
        if b.push_along_x {
            let (lo, hi) = (r.x0 as f32 * c, r.x1 as f32 * c);
            let x = if x - lo < hi - x {
                lo - margin
            } else {
                hi + margin
            };
            (x, z)
        } else {
            let (lo, hi) = (r.z0 as f32 * c, r.z1 as f32 * c);
            let z = if z - lo < hi - z {
                lo - margin
            } else {
                hi + margin
            };
            (x, z)
        }
    }
}

/// Token Ring: the layout the generator draws and the firewalls' turn.
pub mod token_ring {
    use super::*;

    pub const MAP: &str = "token_ring_v1";
    /// Cells across and down.
    pub const N: usize = 128;
    /// The four nodes' centres (cells), clockwise from the north-west.
    pub const NODES: [(usize, usize); 4] = [(32, 32), (96, 32), (96, 96), (32, 96)];
    /// Half a node's side, cells: each node is a square block.
    pub const NODE_HALF: usize = 17;
    /// The north bridge; the others are it turned.
    pub const BRIDGE: Rect = Rect::new(32, 27, 96, 37);
    /// The north bridge's firewall.
    pub const GATE: Rect = Rect::new(60, 27, 68, 37);
    /// How high a firewall stands.
    pub const RISE: f32 = 220.0;

    /// The four nodes, clockwise from the north-west.
    pub fn nodes() -> [Rect; 4] {
        NODES.map(|(x, z)| Rect::new(x - NODE_HALF, z - NODE_HALF, x + NODE_HALF, z + NODE_HALF))
    }

    /// The four bridges, clockwise from the north one.
    pub fn bridges() -> [Rect; 4] {
        let mut r = BRIDGE;
        std::array::from_fn(|_| {
            let b = r;
            r = r.rot90(N);
            b
        })
    }

    /// The four firewalls, clockwise from the north bridge's.
    pub fn gates() -> [Rect; 4] {
        let mut r = GATE;
        std::array::from_fn(|_| {
            let b = r;
            r = r.rot90(N);
            b
        })
    }

    /// Two quiet minutes, then the token: one firewall up at a time for
    /// a minute each, clockwise.
    pub fn shifting() -> Shifting {
        let blocks = gates()
            .iter()
            .enumerate()
            .map(|(k, &rect)| Block {
                rect,
                rise: RISE,
                // The north and south bridges run along x.
                push_along_x: k % 2 == 0,
            })
            .collect();
        let mut states = vec![vec![false; 4]];
        states.extend((0..4).map(|k| (0..4).map(|i| i == k).collect()));
        Shifting {
            blocks,
            first_frames: 120 * GAME_SPEED,
            step_frames: 60 * GAME_SPEED,
            move_frames: 3 * GAME_SPEED,
            states,
        }
    }
}

/// Heap Fragmentation: the lattice the generator draws and the
/// allocator's schedule.
pub mod heap {
    use super::*;

    pub const MAP: &str = "heap_fragmentation_v1";
    /// Cells across and down.
    pub const N: usize = 128;
    /// Holes per side.
    pub const HOLES: usize = 6;
    /// Hole side and lane width, cells; the lattice starts at the rim.
    pub const HOLE: usize = 11;
    pub const LANE: usize = 8;
    pub const ORIGIN: usize = 3;
    /// How high an allocated block stands.
    pub const RISE: f32 = 160.0;

    /// Lane `k`'s first cell (lanes `0..=HOLES`).
    pub const fn lane(k: usize) -> usize {
        ORIGIN + k * (HOLE + LANE)
    }

    /// The holes, row-major.
    pub fn holes() -> Vec<Rect> {
        let mut v = Vec::new();
        for j in 0..HOLES {
            for i in 0..HOLES {
                let (x, z) = (lane(i) + LANE, lane(j) + LANE);
                v.push(Rect::new(x, z, x + HOLE, z + HOLE));
            }
        }
        v
    }

    /// A lattice node: the crossing of lanes `i` (x) and `j` (z).
    pub fn node(i: usize, j: usize) -> Rect {
        Rect::new(lane(i), lane(j), lane(i) + LANE, lane(j) + LANE)
    }

    /// The lane segments between neighbouring nodes, each with the two
    /// nodes it joins (`j * (HOLES + 1) + i`) and whether it runs along x.
    pub fn edges() -> Vec<(Rect, usize, usize, bool)> {
        let n = HOLES + 1;
        let mut v = Vec::new();
        for j in 0..n {
            for i in 0..HOLES {
                let x = lane(i) + LANE;
                v.push((
                    Rect::new(x, lane(j), x + HOLE, lane(j) + LANE),
                    j * n + i,
                    j * n + i + 1,
                    true,
                ));
            }
        }
        for j in 0..HOLES {
            for i in 0..n {
                let z = lane(j) + LANE;
                v.push((
                    Rect::new(lane(i), z, lane(i) + LANE, z + HOLE),
                    j * n + i,
                    (j + 1) * n + i,
                    false,
                ));
            }
        }
        v
    }

    /// The edges in groups of four, each a quarter turn of the next, so
    /// every state stays rotationally symmetric.
    pub fn orbits() -> Vec<[usize; 4]> {
        let edges = edges();
        let index = |r: Rect| {
            edges
                .iter()
                .position(|e| e.0 == r)
                .expect("lattice is symmetric")
        };
        let mut seen = vec![false; edges.len()];
        let mut orbits = Vec::new();
        for k in 0..edges.len() {
            if seen[k] {
                continue;
            }
            let mut r = edges[k].0;
            let orbit: [usize; 4] = std::array::from_fn(|_| {
                let i = index(r);
                r = r.rot90(N);
                i
            });
            for &i in &orbit {
                seen[i] = true;
            }
            orbits.push(orbit);
        }
        orbits
    }

    /// Does every node still reach every other with the `up` edges cut?
    pub fn connected(up: &[bool]) -> bool {
        let n = (HOLES + 1) * (HOLES + 1);
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(p: &mut [usize], mut a: usize) -> usize {
            while p[a] != a {
                p[a] = p[p[a]];
                a = p[a];
            }
            a
        }
        for (k, &(_, a, b, _)) in edges().iter().enumerate() {
            if !up[k] {
                let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                parent[ra] = rb;
            }
        }
        let root = find(&mut parent, 0);
        (0..n).all(|a| find(&mut parent, a) == root)
    }

    /// States in the cycle after the first.
    const STEPS: usize = 48;
    /// Groups allocated at the start, and freed / allocated per step.
    const FIRST: usize = 6;
    const CHURN: usize = 2;

    /// A minute and a half on the opening heap, then every 40 seconds
    /// two groups free and two others allocate.
    pub fn shifting() -> Shifting {
        let edges = edges();
        let orbits = orbits();
        let mut rng = 0x5EED_4EA9u32;
        let mut next = move |n: usize| {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            rng as usize % n
        };
        let state_of = |groups: &[bool]| {
            let mut up = vec![false; edges.len()];
            for (g, &on) in groups.iter().enumerate() {
                if on {
                    for &k in &orbits[g] {
                        up[k] = true;
                    }
                }
            }
            up
        };
        let mut groups = vec![false; orbits.len()];
        loop {
            let mut g = vec![false; orbits.len()];
            let mut placed = 0;
            while placed < FIRST {
                let i = next(orbits.len());
                if !g[i] {
                    g[i] = true;
                    placed += 1;
                }
            }
            if connected(&state_of(&g)) {
                groups = g;
                break;
            }
        }
        let mut states = vec![state_of(&groups)];
        for _ in 0..STEPS {
            loop {
                let mut g = groups.clone();
                for want in [true, false] {
                    // Free CHURN allocated groups, then allocate CHURN free ones.
                    let mut done = 0;
                    while done < CHURN {
                        let i = next(orbits.len());
                        if g[i] == want && groups[i] == want {
                            g[i] = !want;
                            done += 1;
                        }
                    }
                }
                if connected(&state_of(&g)) {
                    groups = g;
                    break;
                }
            }
            states.push(state_of(&groups));
        }
        let blocks = edges
            .iter()
            .map(|&(rect, _, _, along_x)| Block {
                rect,
                rise: RISE,
                push_along_x: along_x,
            })
            .collect();
        Shifting {
            blocks,
            first_frames: 90 * GAME_SPEED,
            step_frames: 40 * GAME_SPEED,
            move_frames: 3 * GAME_SPEED,
            states,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_passes_one_firewall_at_a_time() {
        let s = token_ring::shifting();
        assert!((0..4).all(|k| !s.raised(0, k)));
        for step in 1..12 {
            let frame = s.first_frames + (step - 1) * s.step_frames + 5;
            let up: Vec<usize> = (0..4).filter(|&k| s.raised(frame, k)).collect();
            assert_eq!(up, vec![(step as usize - 1) % 4], "step {step}");
        }
    }

    #[test]
    fn blocks_move_over_their_move_time() {
        let s = token_ring::shifting();
        let start = s.first_frames;
        assert_eq!(s.level(start, 0), 0.0);
        assert!((s.level(start + s.move_frames / 2, 0) - 0.5).abs() < 0.02);
        assert_eq!(s.level(start + s.move_frames, 0), 1.0);
        // It sinks when the token moves on.
        let next = start + s.step_frames;
        assert_eq!(s.level(next, 0), 1.0);
        assert_eq!(s.level(next + s.move_frames, 0), 0.0);
        assert_eq!(s.level(next + s.move_frames, 1), 1.0);
    }

    #[test]
    fn token_ring_is_a_quarter_turn_symmetric() {
        let g = token_ring::gates();
        for k in 0..4 {
            assert_eq!(g[k].rot90(token_ring::N), g[(k + 1) % 4]);
        }
    }

    #[test]
    fn every_heap_state_is_connected_and_symmetric() {
        let s = heap::shifting();
        let edges = heap::edges();
        let index = |r: Rect| edges.iter().position(|e| e.0 == r).unwrap();
        for (n, state) in s.states.iter().enumerate() {
            assert!(heap::connected(state), "state {n} cuts the heap");
            for (k, e) in edges.iter().enumerate() {
                assert_eq!(state[k], state[index(e.0.rot90(heap::N))], "state {n}");
            }
        }
        // Something allocates, something frees, every step.
        for w in s.states.windows(2) {
            assert!(w[0].iter().zip(&w[1]).any(|(a, b)| !a && *b));
            assert!(w[0].iter().zip(&w[1]).any(|(a, b)| *a && !*b));
        }
    }

    #[test]
    fn heap_lattice_fits_inside_the_rim() {
        let last = heap::lane(heap::HOLES) + heap::LANE;
        assert_eq!(last, heap::N - heap::ORIGIN);
        assert_eq!(heap::orbits().len() * 4, heap::edges().len());
    }

    #[test]
    fn push_out_leaves_along_the_axis() {
        let s = token_ring::shifting();
        // North firewall runs x 60..68 cells; a unit west of its middle
        // goes out west.
        let (x, z) = s.push_out(0, 61.0 * 32.0, 32.0 * 32.0, 24.0);
        assert_eq!((x, z), (60.0 * 32.0 - 24.0, 32.0 * 32.0));
        // East firewall runs along z.
        let (x, z) = s.push_out(1, 95.0 * 32.0, 67.0 * 32.0, 24.0);
        assert_eq!((x, z), (95.0 * 32.0, 68.0 * 32.0 + 24.0));
    }

    #[test]
    fn writing_a_block_raises_only_its_inside() {
        let s = token_ring::shifting();
        let w = token_ring::N * CELL_SQ + 1;
        let ground = vec![100.0; w * w];
        let mut h = ground.clone();
        let b = s.write_block(0, 1.0, &ground, &mut h, w);
        assert_eq!(h[b[1] * w + b[0]], 100.0 + token_ring::RISE);
        assert_eq!(h[b[1] * w + b[0] - 1], 100.0);
        assert_eq!(h[(b[3] + 1) * w + b[2]], 100.0);
    }
}
