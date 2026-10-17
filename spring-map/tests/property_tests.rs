use proptest::prelude::*;
use spring_map::map_types::*;

// ---------------------------------------------------------------------------
// FeatureType roundtrip: Display → from_name should be identity
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn feature_type_geovent_roundtrip(_dummy in 0..1u8) {
        let ft = FeatureType::GeoVent;
        let name = ft.to_string();
        let parsed = FeatureType::from_name(&name);
        prop_assert_eq!(parsed, FeatureType::GeoVent);
    }

    #[test]
    fn feature_type_tree_roundtrip(index in 0u8..20) {
        let ft = FeatureType::Tree(index);
        let name = ft.to_string();
        let parsed = FeatureType::from_name(&name);
        prop_assert_eq!(parsed, FeatureType::Tree(index));
    }

    #[test]
    fn feature_type_other_roundtrip(name in "[a-zA-Z][a-zA-Z0-9_]{1,30}") {
        let lower = name.to_ascii_lowercase();
        prop_assume!(!lower.eq_ignore_ascii_case("geovent"));
        prop_assume!(!lower.starts_with("treetype"));

        let ft = FeatureType::from_name(&name);
        prop_assert_eq!(&ft, &FeatureType::Other(name.clone()));
        prop_assert_eq!(ft.to_string(), name);
    }

    #[test]
    fn feature_type_tree_out_of_range(index in 20u8..=255) {
        let name = format!("TreeType{index}");
        let ft = FeatureType::from_name(&name);
        prop_assert!(matches!(ft, FeatureType::Other(_)));
    }
}

// ---------------------------------------------------------------------------
// Header dimensions
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn map_header_dimensions_consistent(
        map_x in (1i32..=32).prop_map(|x| x * 128),
        map_y in (1i32..=32).prop_map(|y| y * 128),
    ) {
        let header = MapHeader::new(map_x, map_y, 0.0, 100.0);

        // Heightmap width/height should be mapx+1, mapy+1.
        prop_assert_eq!(header.heightmap_width(), (map_x + 1) as usize);
        prop_assert_eq!(header.heightmap_height(), (map_y + 1) as usize);

        // Metalmap should be half resolution.
        prop_assert_eq!(header.metalmap_width(), (map_x / 2) as usize);
        prop_assert_eq!(header.metalmap_height(), (map_y / 2) as usize);

        // World dimensions.
        prop_assert_eq!(header.world_width(), (map_x * 8) as f32);
        prop_assert_eq!(header.world_depth(), (map_y * 8) as f32);

        // Heightmap length should equal width * height.
        prop_assert_eq!(
            header.heightmap_len(),
            header.heightmap_width() * header.heightmap_height()
        );
    }
}

// ---------------------------------------------------------------------------
// MapFeature rotation decoding
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn rotation_degrees_is_finite(raw_rotation in any::<f32>().prop_filter("finite", |f| f.is_finite())) {
        let feature = MapFeature::new(
            FeatureType::GeoVent,
            0.0, 0.0, 0.0,
            raw_rotation,
            1.0,
        );
        let degrees = feature.rotation_degrees();
        prop_assert!(degrees.is_finite(), "rotation_degrees({raw_rotation}) = {degrees}");
    }
}
