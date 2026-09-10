use proptest::prelude::*;
use spring_pathfinding::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

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
