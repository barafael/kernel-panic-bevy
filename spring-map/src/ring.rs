//! The Circular Buffer maps' ring: a regular n-gon corridor round a void
//! core, one map per player count ([`Ring::for_map`]), with two
//! directional fast lanes. The inner lane runs clockwise, the outer one
//! counter-clockwise: travel along a lane's direction is fast, against
//! it slow ([`Ring::step_multiplier`]), anywhere else unchanged.
//!
//! The generator draws the corridor, the lanes and their chevrons from
//! this (`examples/gen_circular_buffer.rs`); the game's movement applies
//! the multiplier (`map_events::lane_flow`).

use spring_detmath::DetFloat;

/// A directional lane: a band of the corridor and which way it runs,
/// seen from above with +x right and +z down the screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lane {
    /// Distance band from the centre along the edge normal (elmos).
    pub from: f32,
    pub to: f32,
    pub clockwise: bool,
}

/// A ring map's geometry.
#[derive(Clone, Debug, PartialEq)]
pub struct Ring {
    pub sides: usize,
    /// World size (square), elmos.
    pub world: f32,
    /// The corridor between these distances from the centre along each
    /// edge's normal (the polygons' apothems).
    pub inner: f32,
    pub outer: f32,
    pub lanes: [Lane; 2],
    /// Bias strength: a step along a lane is multiplied by
    /// `1 + alignment * strength`, floored at 0.1.
    pub strength: f32,
}

/// Corridor width and edge length along its middle (elmos).
const WIDTH: f32 = 704.0;
const EDGE: f32 = 2600.0;
/// Lane band: from 64 to 192 elmos in from the corridor's walls.
const LANE_IN: f32 = 64.0;
const LANE_OUT: f32 = 192.0;
/// Room outside the corridor at an edge's middle (the start pocket and
/// the rim) and at a corner (the rim).
const POCKET_ROOM: f32 = 900.0;
const CORNER_ROOM: f32 = 200.0;

impl Ring {
    /// The map stem for `sides` players.
    pub fn map_name(sides: usize) -> String {
        format!("circular_buffer_{sides}_v1")
    }

    /// The ring of a `circular_buffer_<n>_v1` map, `n` in 3..=6.
    pub fn for_map(name: &str) -> Option<Self> {
        let n: usize = name
            .to_ascii_lowercase()
            .strip_prefix("circular_buffer_")?
            .strip_suffix("_v1")?
            .parse()
            .ok()?;
        (3..=6).contains(&n).then(|| Self::new(n))
    }

    pub fn new(sides: usize) -> Self {
        let half = core::f32::consts::PI / sides as f32;
        let mid = EDGE * 0.5 / libm::tanf(half);
        let (inner, outer) = (mid - WIDTH * 0.5, mid + WIDTH * 0.5);
        // Room for the start pockets off the edges' middles and for the
        // outer polygon's corners, plus the rim; whole 1024-elmo blocks
        // (128 map squares).
        let extent = (outer + POCKET_ROOM).det_max(outer / libm::cosf(half) + CORNER_ROOM);
        let world = libm::ceilf(extent * 2.0 / 1024.0) * 1024.0;
        Self {
            sides,
            world,
            inner,
            outer,
            lanes: [
                Lane {
                    from: inner + LANE_IN,
                    to: inner + LANE_OUT,
                    clockwise: true,
                },
                Lane {
                    from: outer - LANE_OUT,
                    to: outer - LANE_IN,
                    clockwise: false,
                },
            ],
            strength: 0.6,
        }
    }

    pub fn centre(&self) -> (f32, f32) {
        (self.world * 0.5, self.world * 0.5)
    }

    /// Edge `k`'s outward normal: edge 0 faces north (−z), the rest
    /// follow clockwise.
    pub fn normal(&self, k: usize) -> (f32, f32) {
        let a = core::f32::consts::TAU * k as f32 / self.sides as f32;
        let (s, c) = libm::sincosf(a);
        (s, -c)
    }

    /// The edge whose sector holds `p`, and `p`'s distance from the
    /// centre along that edge's normal.
    pub fn edge_at(&self, p: (f32, f32)) -> (usize, f32) {
        let c = self.centre();
        let (dx, dz) = (p.0 - c.0, p.1 - c.1);
        (0..self.sides)
            .map(|k| {
                let n = self.normal(k);
                (k, dx * n.0 + dz * n.1)
            })
            .fold((0, f32::MIN), |best, e| if e.1 > best.1 { e } else { best })
    }

    /// Is `p` in the corridor?
    pub fn in_corridor(&self, p: (f32, f32)) -> bool {
        let (_, d) = self.edge_at(p);
        (self.inner..=self.outer).contains(&d)
    }

    /// The direction of travel of the lane at `p`, if `p` is on one.
    pub fn lane_direction(&self, p: (f32, f32)) -> Option<(f32, f32)> {
        let (k, d) = self.edge_at(p);
        let lane = self.lanes.iter().find(|l| (l.from..=l.to).contains(&d))?;
        let n = self.normal(k);
        // Clockwise round the centre, seen with +z down the screen: on
        // the north edge (normal −z) that is +x.
        let cw = (-n.1, n.0);
        Some(if lane.clockwise { cw } else { (-cw.0, -cw.1) })
    }

    /// Multiplier for a step from `p` heading `forward` (any length).
    pub fn step_multiplier(&self, p: (f32, f32), forward: (f32, f32)) -> f32 {
        let Some(dir) = self.lane_direction(p) else {
            return 1.0;
        };
        let len = (forward.0 * forward.0 + forward.1 * forward.1).sqrt();
        if len < 1e-6 {
            return 1.0;
        }
        let along = (dir.0 * forward.0 + dir.1 * forward.1) / len;
        let m = 1.0 + along * self.strength;
        if m < 0.1 { 0.1 } else { m }
    }

    /// The point `along` elmos from edge `k`'s middle (clockwise
    /// positive) at distance `d` from the centre along its normal.
    pub fn on_edge(&self, k: usize, d: f32, along: f32) -> (f32, f32) {
        let c = self.centre();
        let n = self.normal(k);
        let cw = (-n.1, n.0);
        (c.0 + n.0 * d + cw.0 * along, c.1 + n.1 * d + cw.1 * along)
    }

    /// Corner `k` (between edges `k` and `k + 1`) at distance `d` from
    /// the centre along the edge normals.
    pub fn corner(&self, k: usize, d: f32) -> (f32, f32) {
        let half = core::f32::consts::PI / self.sides as f32;
        self.on_edge(k, d, d * libm::tanf(half))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for n in 3..=6 {
            assert_eq!(Ring::for_map(&Ring::map_name(n)).unwrap().sides, n);
        }
        assert!(Ring::for_map("circular_buffer_7_v1").is_none());
        assert!(Ring::for_map("Circular_Buffer").is_none());
    }

    #[test]
    fn the_lanes_run_opposite_ways() {
        let r = Ring::new(4);
        let c = r.centre();
        let inner = (c.0, c.1 - (r.lanes[0].from + r.lanes[0].to) * 0.5);
        let outer = (c.0, c.1 - (r.lanes[1].from + r.lanes[1].to) * 0.5);
        // North edge: clockwise is east.
        let (dx, _) = r.lane_direction(inner).unwrap();
        assert!(dx > 0.99);
        let (dx, _) = r.lane_direction(outer).unwrap();
        assert!(dx < -0.99);
        assert!((r.step_multiplier(inner, (1.0, 0.0)) - 1.6).abs() < 1e-4);
        assert!((r.step_multiplier(inner, (-1.0, 0.0)) - 0.4).abs() < 1e-4);
        assert!((r.step_multiplier(outer, (-1.0, 0.0)) - 1.6).abs() < 1e-4);
        // Mid-corridor and off the ring: unchanged.
        let mid = (c.0, c.1 - (r.inner + r.outer) * 0.5);
        assert_eq!(r.step_multiplier(mid, (1.0, 0.0)), 1.0);
        assert_eq!(r.step_multiplier(c, (1.0, 0.0)), 1.0);
    }

    #[test]
    fn every_ring_fits_its_map() {
        for n in 3..=6 {
            let r = Ring::new(n);
            for k in 0..n {
                let (x, z) = r.corner(k, r.outer);
                assert!(
                    x > 200.0 && z > 200.0 && x < r.world - 200.0 && z < r.world - 200.0,
                    "{n}"
                );
            }
            assert!(r.world % 1024.0 == 0.0);
        }
    }

    #[test]
    fn clockwise_follows_the_ring() {
        // Going clockwise along the inner lane from edge 0 reaches
        // edge 1.
        let r = Ring::new(5);
        let d = (r.lanes[0].from + r.lanes[0].to) * 0.5;
        let mut p = r.on_edge(0, d, 0.0);
        for _ in 0..400 {
            let dir = r.lane_direction(p).unwrap();
            p = (p.0 + dir.0 * 8.0, p.1 + dir.1 * 8.0);
        }
        assert_eq!(r.edge_at(p).0, 1);
    }
}
