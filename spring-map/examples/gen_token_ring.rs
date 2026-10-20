//! Generates `kernel-panic/assets/maps/token_ring_v1.kpmap`.
//!
//! After Beyond All Reason's Asteroid Mines: four platforms in a square,
//! joined in a ring by bridges over the void. Each bridge carries a bus
//! lane and a firewall; once the opening is over, one firewall stands at
//! a time and passes round the ring like the token
//! (`spring_map::shifting::token_ring`). Quarter-turn symmetric; each
//! node has a raised start plateau ramped toward both its bridges.
//!
//! ```sh
//! cargo run -p spring-map --example gen_token_ring --features write --release
//! ```

use spring_map::kpgen::{CELL, Cell, KpMap, Level, Ramp, Rect, Side, rot90};
use spring_map::shifting::{Shifting, token_ring as tr};

const NODE: usize = 0;
const HIGH: usize = 1;
const RIM: usize = 2;

fn levels() -> Vec<Level> {
    // Dual Core's teals.
    vec![
        Level {
            height: 200.0,
            fill: [41, 129, 127],
            line: [90, 210, 205],
        },
        Level {
            height: 330.0,
            fill: [20, 64, 65],
            line: [65, 190, 189],
        },
        Level {
            height: 520.0,
            fill: [140, 230, 230],
            line: [210, 255, 255],
        },
    ]
}

/// The north-west node's start plateau and its ramps toward the north
/// and west bridges; the other nodes get these turned.
const PLATEAU: Rect = Rect::new(20, 20, 32, 32);
const PLATEAU_RAMPS: [(Rect, Side); 2] = [
    (Rect::new(32, 20, 38, 32), Side::East),
    (Rect::new(20, 32, 32, 38), Side::South),
];
/// The bus lane along the north bridge, node edge to node edge.
const LANE: Rect = Rect::new(45, 31, 83, 33);
/// The north-west node's datavents (cell corners); the first is its start.
const VENTS: [(usize, usize); 4] = [(26, 26), (42, 42), (24, 42), (42, 24)];

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../kernel-panic/assets/maps/token_ring_v1.kpmap"
        )
        .into()
    });
    let n = tr::N;
    let mut map = KpMap::new(n, n, levels(), NODE, RIM);
    map.fill(Rect::new(0, 0, n, n), Cell::Void);
    for bridge in tr::bridges() {
        map.fill(bridge, Cell::Floor(NODE));
    }
    let c = CELL as f32;
    for &(x, z) in &tr::NODES {
        map.fill_disc(
            (x as f32 * c, z as f32 * c),
            tr::NODE_RADIUS * c,
            Cell::Floor(NODE),
        );
    }
    let (mut plateau, mut ramps) = (
        PLATEAU,
        PLATEAU_RAMPS.map(|(rect, low_side)| Ramp {
            rect,
            low_side,
            low: NODE,
            high: HIGH,
        }),
    );
    let mut lane = LANE;
    for _ in 0..4 {
        map.fill(plateau, Cell::Floor(HIGH));
        for r in ramps {
            map.add_ramp(r);
        }
        map.lanes.push(lane);
        plateau = plateau.rot90(n);
        ramps = ramps.map(|r| r.rot90(n));
        lane = lane.rot90(n);
    }
    map.reset_rim();
    map.blocks = tr::gates().to_vec();

    let s = (n * CELL) as f32;
    let mut vents: Vec<(f32, f32)> = VENTS
        .iter()
        .map(|&(x, z)| ((x * CELL) as f32, (z * CELL) as f32))
        .collect();
    for q in 0..3 {
        let turned: Vec<_> = vents[q * 4..q * 4 + 4]
            .iter()
            .map(|&p| rot90(p, s))
            .collect();
        vents.extend(turned);
    }
    // Starts: the plateaus, in the order opposite nodes pair up for a duel.
    map.starts = [0, 2, 1, 3].iter().map(|&q| vents[q * 4]).collect();
    map.vents = vents;
    map.description = "Token Ring v1. Map for Kernel Panic, after Asteroid Mines. A firewall \
                       passes round the ring. Intended for 2-4 players."
        .into();
    let built = map.write(&out);

    // Every firewall state keeps every vent in reach.
    let shifting = Shifting::for_map(tr::MAP).expect("token ring shifting");
    let w = built.parsed.header.heightmap_width();
    for state in &shifting.states {
        let mut heights = built.parsed.heights.clone();
        for (k, &up) in state.iter().enumerate() {
            if up {
                shifting.write_block(k, 1.0, &built.parsed.heights, &mut heights, w);
            }
        }
        map.check_reachable(&built, &heights, "firewall state");
    }
    // Opposite nodes are cut off by no single firewall: a raised one
    // blocks its bridge.
    let mut heights = built.parsed.heights.clone();
    shifting.write_block(0, 1.0, &built.parsed.heights, &mut heights, w);
    let reach = spring_map::kpgen::reachable(&built.parsed, &heights, map.starts[0]);
    let gate = tr::GATE.centre();
    let mx = built.parsed.header.map_x as usize;
    assert!(
        !reach[(gate.1 as usize / 8) * mx + gate.0 as usize / 8],
        "a raised firewall is walkable"
    );
}
