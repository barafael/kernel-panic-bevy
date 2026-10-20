//! Generates `kernel-panic/assets/maps/circular_buffer_{3,4,5,6}_v1.kpmap`.
//!
//! The port's Circular Buffer, redrawn: for n players a regular n-gon
//! corridor round a void core, void outside, a start pocket off the
//! middle of every edge. Two fast lanes run round the ring, the inner
//! one clockwise and the outer one counter-clockwise
//! (`spring_map::ring`: with a lane fast, against it slow), drawn as
//! chevrons the way they run. At every corner kernel pillars stand in
//! the corridor's middle, leaving the two lanes either side: a slight
//! chokepoint. An n-th of a turn symmetric: datavents and starts
//! exactly, the corridor as nearly as the 32-elmo grid allows.
//!
//! ```sh
//! cargo run -p spring-map --example gen_circular_buffer --features write --release
//! ```

use spring_detmath::DetFloat;
use spring_map::kpgen::{Arrow, CELL, Cell, KpMap, Level};
use spring_map::ring::Ring;

const BASE: usize = 0;
const HIGH: usize = 1;
const RIM: usize = 2;

fn levels() -> Vec<Level> {
    vec![
        Level {
            height: 200.0,
            fill: [20, 60, 90],
            line: [60, 150, 210],
        },
        Level {
            height: 380.0,
            fill: [60, 20, 80],
            line: [170, 90, 220],
        },
        Level {
            height: 520.0,
            fill: [110, 190, 240],
            line: [190, 235, 255],
        },
    ]
}

/// Start pocket: how far out from the corridor and how wide.
const POCKET_DEPTH: f32 = 560.0;
const POCKET_HALF: f32 = 480.0;
/// Room each passage keeps beside a corner pillar, the pillar's largest
/// radius, and the room kept outside it where a sharp corner is trimmed.
const PASSAGE: f32 = 256.0;
const PILLAR_MAX: f32 = 260.0;
const OUTER_PASSAGE: f32 = 320.0;

const CLOCKWISE: [u8; 3] = [80, 220, 255];
const COUNTER: [u8; 3] = [255, 150, 40];

fn generate(n: usize, dir: &str) {
    let ring = Ring::new(n);
    let cells = ring.world as usize / CELL;
    let mut map = KpMap::new(cells, cells, levels(), BASE, RIM);
    let c = ring.centre();
    map.fill_where(Cell::Void, |_, _| true);
    map.fill_where(Cell::Floor(BASE), |x, z| ring.in_corridor((x, z)));
    for k in 0..n {
        let nrm = ring.normal(k);
        let tan = (-nrm.1, nrm.0);
        map.fill_where(Cell::Floor(BASE), |x, z| {
            let (dx, dz) = (x - c.0, z - c.1);
            let d = dx * nrm.0 + dz * nrm.1;
            let t = dx * tan.0 + dz * tan.1;
            (ring.outer..=ring.outer + POCKET_DEPTH).contains(&d) && t.abs() <= POCKET_HALF
        });
    }
    let half = core::f32::consts::PI / n as f32;
    let mid = (ring.inner + ring.outer) * 0.5;
    // Each pillar leaves `PASSAGE` either side along the corner's
    // bisector, up to `PILLAR_MAX` (a triangle's corners are wide).
    let pillar = ((ring.outer - ring.inner) / libm::cosf(half) - 2.0 * PASSAGE)
        .det_min(2.0 * PILLAR_MAX)
        * 0.5;
    // A sharp corner's tip past the outer passage is dead ground: trim it.
    let mid_corner = mid / libm::cosf(half);
    let clip = mid_corner + pillar + OUTER_PASSAGE;
    map.fill_where(Cell::Void, |x, z| {
        let (dx, dz) = (x - c.0, z - c.1);
        (dx * dx + dz * dz).sqrt() > clip
    });
    for k in 0..n {
        map.fill_disc(ring.corner(k, mid), pillar, Cell::Floor(HIGH));
    }
    map.reset_rim();
    map.arrows = vec![None; cells * cells];
    for cz in 0..cells {
        for cx in 0..cells {
            let p = ((cx * CELL + CELL / 2) as f32, (cz * CELL + CELL / 2) as f32);
            if map.cell(cx, cz) != Cell::Floor(BASE) || !ring.in_corridor(p) {
                continue;
            }
            if let Some(dir) = ring.lane_direction(p) {
                let (_, d) = ring.edge_at(p);
                let colour = if d < mid { CLOCKWISE } else { COUNTER };
                map.arrows[cz * cells + cx] = Some(Arrow { dir, colour });
            }
        }
    }

    let edge = |k: usize| {
        let pocket = ring.outer + 300.0;
        let quarter = (ring.corner(0, mid).0 - ring.on_edge(0, mid, 0.0).0).abs() * 0.5;
        [
            ring.on_edge(k, pocket, -220.0),
            ring.on_edge(k, pocket, 220.0),
            ring.on_edge(k, mid, -quarter),
            ring.on_edge(k, mid, quarter),
        ]
    };
    map.vents = (0..n).flat_map(edge).collect();
    // Seats: opposite edges first when there are any, so a duel on the
    // even rings faces across the core.
    let order: Vec<usize> = if n.is_multiple_of(2) {
        (0..n / 2).flat_map(|k| [k, k + n / 2]).collect()
    } else {
        (0..n).collect()
    };
    map.starts = order.iter().map(|&k| edge(k)[0]).collect();
    map.description = format!(
        "Circular Buffer ({n}) v1. Map for Kernel Panic. Fast lanes run both ways round the \
         ring: inner clockwise, outer counter-clockwise. Intended for {n} players."
    );
    let path = format!("{dir}/{}.kpmap", Ring::map_name(n));
    map.write(&path);
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../kernel-panic/assets/maps").into()
    });
    for n in 3..=6 {
        generate(n, &dir);
    }
}
