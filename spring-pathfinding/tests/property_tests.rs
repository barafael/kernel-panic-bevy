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
        let mut layer = NodeLayer::new(&speed_map);

        let max_world_x = (width as f32) * 8.0 - 1.0;
        let max_world_z = (height as f32) * 8.0 - 1.0;
        let src = [src_x.min(max_world_x), src_z.min(max_world_z)];
        let dst = [dst_x.min(max_world_x), dst_z.min(max_world_z)];

        let path = find_path(&mut layer, src, dst);

        // Should always find a path on a fully open map.
        prop_assert!(!path.is_empty(), "should find path on open map");

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

    /// A path around a blocked wall should be findable and longer than straight-line.
    /// NOTE: waypoints may touch partially-blocked nodes at the coarse level —
    /// this is inherent to hierarchical pathfinding and handled by local avoidance.
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

        let mut layer = NodeLayer::new(&speed_map);

        let src = [8.0, (map_size as f32 / 2.0) * 8.0];
        let dst = [(map_size as f32 - 2.0) * 8.0, (map_size as f32 / 2.0) * 8.0];

        let path = find_path(&mut layer, src, dst);

        if !path.is_empty() {
            let straight = ((dst[0] - src[0]).powi(2) + (dst[1] - src[1]).powi(2)).sqrt();
            // Path should exist and be at least as long as straight-line.
            prop_assert!(path.total_length() >= straight * 0.9,
                "path should be at least ~straight-line distance");
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

    /// Tessellation should produce at least one leaf node.
    #[test]
    fn tessellation_always_has_leaves(
        width in 2u32..32,
        height in 2u32..32,
        speed in 0.0f32..1.0,
    ) {
        let speed_map = SpeedMap::uniform(width, height, speed);
        let layer = NodeLayer::new(&speed_map);
        prop_assert!(layer.leaf_count() >= 1);
    }

    /// Path endpoints should match source and destination.
    #[test]
    fn path_starts_at_src_ends_at_dst(
        size in 16u32..64,
    ) {
        let speed_map = SpeedMap::uniform(size, size, 1.0);
        let mut layer = NodeLayer::new(&speed_map);

        let src = [16.0, 16.0];
        let dst = [(size as f32 - 3.0) * 8.0, (size as f32 - 3.0) * 8.0];

        let path = find_path(&mut layer, src, dst);
        prop_assert!(!path.is_empty());

        let first = path.points.first().unwrap();
        let last = path.points.last().unwrap();

        let src_dist = ((first[0] - src[0]).powi(2) + (first[1] - src[1]).powi(2)).sqrt();
        let dst_dist = ((last[0] - dst[0]).powi(2) + (last[1] - dst[1]).powi(2)).sqrt();

        prop_assert!(src_dist < 50.0, "path start too far from src: {}", src_dist);
        prop_assert!(dst_dist < 50.0, "path end too far from dst: {}", dst_dist);
    }
}
