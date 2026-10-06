use proptest::prelude::*;
use spring_pathfinding::qtpfs::{NodeLayer, QtScratch, QtSearch};
use spring_pathfinding::*;

fn search(map: &SpeedMap, src: [f32; 2], dst: [f32; 2]) -> Option<qtpfs::QtPath> {
    let mut layer = NodeLayer::new(map, None);
    let mut scratch = QtScratch::default();
    match QtSearch::begin(&mut layer, &mut scratch, map, None, src, dst, 8.0) {
        Err(p) => Some(p),
        Ok(mut s) => s.step(&layer, &mut scratch, usize::MAX).expect("unbounded"),
    }
}

/// Transition points lie on square edges and corners; a point counts as
/// open when any square it touches is open.
fn near_open(map: &SpeedMap, p: [f32; 2]) -> bool {
    [-0.01f32, 0.01].iter().any(|dx| {
        [-0.01f32, 0.01].iter().any(|dz| {
            let (x, z) = (p[0] + dx, p[1] + dz);
            x >= 0.0 && z >= 0.0 && map.get((x / 8.0) as u32, (z / 8.0) as u32) > 0.0
        })
    })
}

fn length(points: &[[f32; 2]]) -> f32 {
    points
        .windows(2)
        .map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt())
        .sum()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Open ground: the raw search answers with the straight line.
    #[test]
    fn uniform_map_is_a_straight_line(
        width in 8u32..64,
        height in 8u32..64,
        src_x in 0.0f32..500.0,
        src_z in 0.0f32..500.0,
        dst_x in 0.0f32..500.0,
        dst_z in 0.0f32..500.0,
    ) {
        let map = SpeedMap::uniform(width, height, 1.0);
        let max_x = width as f32 * 8.0 - 1.0;
        let max_z = height as f32 * 8.0 - 1.0;
        let src = [src_x.min(max_x), src_z.min(max_z)];
        let dst = [dst_x.min(max_x), dst_z.min(max_z)];
        let path = search(&map, src, dst).expect("open map is always pathable");
        prop_assert!(path.full);
        prop_assert_eq!(path.points.len(), 2);
    }

    /// A wall with gaps at both ends: the path is found, complete, every
    /// point lies on open ground, and it is not absurdly long.
    #[test]
    fn path_around_wall_stays_on_open_ground(
        blocks in 1u32..5,
        wall_x in 4u32..60,
    ) {
        // Engine maps are multiples of 16 squares; odd sizes leave
        // 1-wide mixed nodes that cannot split further.
        let map_size = blocks * 16;
        let wall_x = wall_x.min(map_size - 4);
        let mut map = SpeedMap::uniform(map_size, map_size, 1.0);
        for z in 2..(map_size - 2) {
            map.speeds[(z * map_size + wall_x) as usize] = 0.0;
        }
        let mid = map_size as f32 * 4.0;
        let src = [8.0, mid];
        let dst = [map_size as f32 * 8.0 - 8.0, mid];
        let path = search(&map, src, dst).expect("wall leaves gaps at the edges");
        prop_assert!(path.full, "{:?}", path.points);
        for p in &path.points {
            prop_assert!(near_open(&map, *p), "{:?}", p);
        }
        let straight = dst[0] - src[0];
        prop_assert!(length(&path.points) < straight + 2.0 * map_size as f32 * 8.0 + 64.0);
    }

    /// Random closed squares: whatever the search returns starts at the
    /// source, every point lies on open ground, and a complete path
    /// ends at the goal.
    #[test]
    fn paths_never_enter_closed_squares(
        seed in any::<u64>(),
        blocked in proptest::collection::vec((0u32..48, 0u32..48), 0..120),
    ) {
        let size = 48u32;
        let mut map = SpeedMap::uniform(size, size, 1.0);
        for (x, z) in &blocked {
            map.speeds[(z * size + x) as usize] = 0.0;
        }
        let pick = |s: u64, lo: f32, hi: f32| lo + (s % 1000) as f32 / 1000.0 * (hi - lo);
        let src = [pick(seed, 4.0, 372.0), pick(seed / 7, 4.0, 372.0)];
        let dst = [pick(seed / 13, 4.0, 372.0), pick(seed / 31, 4.0, 372.0)];
        prop_assume!(map.get((src[0] / 8.0) as u32, (src[1] / 8.0) as u32) > 0.0);
        if let Some(path) = search(&map, src, dst) {
            prop_assert_eq!(path.points[0], src);
            // The goal itself may sit on a closed square inside a mixed
            // leaf (one too narrow to split further); the engine accepts
            // that too. Every other point must be on open ground.
            for p in &path.points[..path.points.len() - 1] {
                prop_assert!(near_open(&map, *p), "{:?}", p);
            }
            if path.full {
                prop_assert_eq!(*path.points.last().unwrap(), dst);
            }
        }
    }
}
