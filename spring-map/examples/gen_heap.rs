//! Generates `kernel-panic/assets/maps/heap_fragmentation_v1.kpmap`.
//!
//! After Beyond All Reason's Sinkhole Network: a lattice of holes with
//! lanes between them. The holes are freed memory (void) on the inner
//! and outer rings and kernel blocks (sheer mesas) on the middle one.
//! Every lane segment between two crossings is a block the allocator
//! can raise (`spring_map::shifting::heap`): a few allocate and a few
//! free every 40 seconds, and the heap never falls apart. Quarter-turn
//! symmetric, eight starts on the outer crossings.
//!
//! ```sh
//! cargo run -p spring-map --example gen_heap --features write --release
//! ```

use spring_detmath::DetFloat;
use spring_map::kpgen::{BlockStyle, CELL, Cell, KpMap, Level};
use spring_map::shifting::{Shifting, heap};

const LANE: usize = 0;
const KERNEL: usize = 1;
const RIM: usize = 2;

fn levels() -> Vec<Level> {
    // Quad Core's olive and purple.
    vec![
        Level {
            height: 200.0,
            fill: [98, 97, 0],
            line: [160, 160, 30],
        },
        Level {
            height: 380.0,
            fill: [45, 0, 45],
            line: [120, 40, 120],
        },
        Level {
            height: 520.0,
            fill: [189, 190, 0],
            line: [240, 240, 90],
        },
    ]
}

/// Crossings with a datavent: the outer ring's corners and middles (the
/// starts), the inner diamond, the centre.
const VENT_NODES: [(usize, usize); 17] = [
    (0, 0),
    (6, 6),
    (6, 0),
    (0, 6),
    (3, 0),
    (3, 6),
    (6, 3),
    (0, 3),
    (1, 1),
    (5, 1),
    (5, 5),
    (1, 5),
    (3, 1),
    (5, 3),
    (3, 5),
    (1, 3),
    (3, 3),
];
/// The first eight vents are the starts: opposite corners first.
const STARTS: usize = 8;

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../kernel-panic/assets/maps/heap_fragmentation_v1.kpmap"
        )
        .into()
    });
    let n = heap::N;
    let mut map = KpMap::new(n, n, levels(), LANE, RIM);
    let mid = (heap::HOLES as f32 - 1.0) * 0.5;
    for (k, hole) in heap::holes().into_iter().enumerate() {
        let (i, j) = (k % heap::HOLES, k / heap::HOLES);
        let ring = (i as f32 - mid).abs().det_max((j as f32 - mid).abs());
        let kernel = (1.0..2.0).contains(&ring);
        map.fill(
            hole,
            if kernel {
                Cell::Floor(KERNEL)
            } else {
                Cell::Void
            },
        );
    }
    map.reset_rim();
    map.blocks = heap::edges().iter().map(|e| e.0).collect();
    map.block_style = BlockStyle::Faint;
    map.vents = VENT_NODES
        .iter()
        .map(|&(i, j)| {
            let (x, z) = heap::node(i, j).centre();
            (x, z)
        })
        .collect();
    map.starts = map.vents[..STARTS].to_vec();
    map.description = "Heap Fragmentation v1. Map for Kernel Panic, after Sinkhole Network. \
                       Blocks allocate and free during play. Intended for 2-8 players."
        .into();
    let built = map.write(&out);

    // Every allocator state keeps every crossing in reach.
    let shifting = Shifting::for_map(heap::MAP).expect("heap shifting");
    let w = built.parsed.header.heightmap_width();
    for state in &shifting.states {
        let mut heights = built.parsed.heights.clone();
        for (k, &up) in state.iter().enumerate() {
            if up {
                shifting.write_block(k, 1.0, &built.parsed.heights, &mut heights, w);
            }
        }
        map.check_reachable(&built, &heights, "allocator state");
    }
    let s = (n * CELL) as f32;
    for &(x, z) in &map.vents {
        let turned = spring_map::kpgen::rot90((x, z), s);
        assert!(
            map.vents.contains(&turned),
            "vent ({x}, {z}) has no quarter-turn twin"
        );
    }
}
