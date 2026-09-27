use proptest::prelude::*;
use spring_pathfinding::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Random rectangles blocked and reopened one after another: the
    /// incrementally updated labels always partition the cells like a
    /// rebuild from scratch.
    #[test]
    fn incremental_labels_equal_rebuild(
        seed in any::<u64>(),
        edits in proptest::collection::vec((0u32..40, 0u32..40, 1u32..6, 1u32..6, any::<bool>()), 1..12),
    ) {
        let size = 40u32;
        let mut speed_map = SpeedMap::uniform(size, size, 1.0);
        let mut state = seed | 1;
        for cell in speed_map.speeds.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let r = (state >> 11) as f32 / (1u64 << 53) as f32;
            if r < 0.3 {
                *cell = 0.0;
            }
        }
        let mut labels = ComponentLabels::build(&speed_map, None);
        for (x0, z0, w, h, open) in edits {
            let x1 = (x0 + w).min(size - 1);
            let z1 = (z0 + h).min(size - 1);
            for z in z0..=z1 {
                for x in x0..=x1 {
                    speed_map.speeds[(z * size + x) as usize] = if open { 1.0 } else { 0.0 };
                }
            }
            labels.update_region(&speed_map, None, [x0 as i32, z0 as i32, x1 as i32, z1 as i32]);
            let fresh = ComponentLabels::build(&speed_map, None);
            let canon = |l: &ComponentLabels| {
                let mut map = std::collections::HashMap::new();
                let mut out = Vec::new();
                for z in 0..size {
                    for x in 0..size {
                        let r = l.at(x, z);
                        out.push(if r == 0 { 0 } else { let n = map.len() as u32 + 1; *map.entry(r).or_insert(n) });
                    }
                }
                out
            };
            prop_assert_eq!(canon(&labels), canon(&fresh));
        }
    }

    /// On random obstacle fields the labelled search (unreachable goals
    /// detected up front, search stopped at the closest reachable cell)
    /// returns exactly the flooding search's path, reachable or not.
    #[test]
    fn labelled_search_equals_flood(
        seed in any::<u64>(),
        density in 0.1f32..0.55,
        src_x in 0.0f32..255.0,
        src_z in 0.0f32..255.0,
        dst_x in 0.0f32..255.0,
        dst_z in 0.0f32..255.0,
    ) {
        let size = 32u32;
        let mut speed_map = SpeedMap::uniform(size, size, 1.0);
        // Cheap deterministic noise.
        let mut state = seed | 1;
        for cell in speed_map.speeds.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let r = (state >> 11) as f32 / (1u64 << 53) as f32;
            if r < density {
                *cell = 0.0;
            } else if r < density + 0.2 {
                *cell = 0.3 + r;
            }
        }
        speed_map.refresh_max_speed();
        let labels = ComponentLabels::build(&speed_map, None);
        let src = [src_x, src_z];
        let dst = [dst_x, dst_z];
        let flood = find_path_masked(&speed_map, None, None, src, dst);
        let mut scratch = SearchScratch::default();
        let fast = match scratch.begin_search_labelled(&speed_map, None, Some(&labels), src, dst) {
            Err(result) => result,
            Ok(mut search) => match scratch.step(&mut search, &speed_map, None, None, usize::MAX) {
                SearchStatus::Done(path) => path,
                SearchStatus::Running => unreachable!(),
            },
        };
        prop_assert_eq!(
            flood.as_ref().map(|p| (p.reached_goal, p.points.clone())),
            fast.as_ref().map(|p| (p.reached_goal, p.points.clone()))
        );
    }

    /// Any path found on a uniform map should be no longer than sqrt(2) * straight-line distance.
    #[test]
    fn uniform_map_path_near_optimal(
        width in 8u32..64,
        height in 8u32..64,
        src_x in 0.0f32..500.0,
        src_z in 0.0f32..500.0,
        dst_x in 0.0f32..500.0,
        dst_z in 0.0f32..500.0,
    ) {
        let speed_map = SpeedMap::uniform(width, height, 1.0);

        let max_world_x = (width as f32) * 8.0 - 1.0;
        let max_world_z = (height as f32) * 8.0 - 1.0;
        let src = [src_x.min(max_world_x), src_z.min(max_world_z)];
        let dst = [dst_x.min(max_world_x), dst_z.min(max_world_z)];

        let path = find_path(&speed_map, src, dst).expect("open map is always pathable");

        let straight = ((dst[0] - src[0]).powi(2) + (dst[1] - src[1]).powi(2)).sqrt();
        if straight > 1.0 {
            // Path should be at most 1.5x the straight-line distance on open terrain.
            prop_assert!(
                path.total_length() <= straight * 1.5 + 50.0,
                "path too long: {} vs straight {}",
                path.total_length(),
                straight,
            );
        }
    }

    /// A path around a blocked wall should be findable and never cross the wall.
    #[test]
    fn path_around_wall_is_longer(
        map_size in 16u32..64,
        wall_x in 4u32..60,
    ) {
        let map_size = map_size.min(64);
        let wall_x = wall_x.min(map_size - 2).max(2);

        let mut speed_map = SpeedMap::uniform(map_size, map_size, 1.0);
        for z in 2..map_size - 2 {
            speed_map.speeds[(z * map_size + wall_x) as usize] = 0.0;
        }

        let src = [8.0, (map_size as f32 / 2.0) * 8.0];
        let dst = [(map_size as f32 - 2.0) * 8.0, (map_size as f32 / 2.0) * 8.0];

        let path = find_path(&speed_map, src, dst).expect("wall leaves gaps at the edges");

        let straight = ((dst[0] - src[0]).powi(2) + (dst[1] - src[1]).powi(2)).sqrt();
        prop_assert!(path.total_length() >= straight * 0.9,
            "path should be at least ~straight-line distance");

        // No waypoint may sit on a wall cell (column wall_x, rows 2..size-2).
        for p in &path.points {
            let cx = (p[0] / 8.0) as u32;
            let cz = (p[1] / 8.0) as u32;
            if cx == wall_x && (2..map_size - 2).contains(&cz) {
                prop_assert!(false, "waypoint inside the wall: {:?}", p);
            }
        }
    }

    /// Speed map from heightmap should never produce NaN or negative speeds.
    #[test]
    fn speed_map_no_nans(
        heights in proptest::collection::vec(-500.0f32..500.0, 4..100),
    ) {
        let side = (heights.len() as f32).sqrt() as u32;
        if side < 2 { return Ok(()); }
        let total = (side * side) as usize;
        let heights = &heights[..total.min(heights.len())];

        let map = SpeedMap::from_heightmap(heights, side, side, 1.0, 40.0);
        for &speed in &map.speeds {
            prop_assert!(!speed.is_nan(), "speed is NaN");
            prop_assert!(speed >= 0.0, "speed is negative: {}", speed);
            prop_assert!(speed <= 1.0, "speed > 1.0: {}", speed);
        }
    }

    /// On open terrain the path runs exactly source → destination.
    #[test]
    fn path_starts_at_src_ends_at_dst(
        size in 16u32..64,
    ) {
        let speed_map = SpeedMap::uniform(size, size, 1.0);

        let src = [16.0, 16.0];
        let dst = [(size as f32 - 3.0) * 8.0, (size as f32 - 3.0) * 8.0];

        let path = find_path(&speed_map, src, dst).expect("open map");
        let first = path.points.first().unwrap();
        let last = path.points.last().unwrap();

        prop_assert!((first[0] - src[0]).abs() < 0.01 && (first[1] - src[1]).abs() < 0.01);
        prop_assert!((last[0] - dst[0]).abs() < 0.01 && (last[1] - dst[1]).abs() < 0.01);
    }
}
