use proptest::prelude::*;
use spring_map::map_types::*;
use spring_map::smd_parser;
use spring_map::smf_parser;

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
// Heightmap conversion: monotonic and bounded
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn sample_to_world_height_bounded(
        raw in i16::MIN..=i16::MAX,
        min_h in -500.0f32..500.0,
        max_h_offset in 0.1f32..1000.0,
    ) {
        let max_h = min_h + max_h_offset;
        let header = SmfHeader::new_flat(128, 128, min_h, max_h);
        let height = header.sample_to_world_height(raw);
        prop_assert!(height >= min_h, "height {height} < min {min_h}");
        prop_assert!(height <= max_h, "height {height} > max {max_h}");
    }

    #[test]
    fn sample_to_world_height_monotonic(
        a in i16::MIN..=i16::MAX,
        b in i16::MIN..=i16::MAX,
    ) {
        let header = SmfHeader::new_flat(128, 128, 0.0, 100.0);
        let ha = header.sample_to_world_height(a);
        let hb = header.sample_to_world_height(b);
        // As unsigned interpretation: if a_u16 <= b_u16 then ha <= hb
        let au = a as u16;
        let bu = b as u16;
        if au <= bu {
            prop_assert!(ha <= hb, "not monotonic: raw {a} ({au}) → {ha}, raw {b} ({bu}) → {hb}");
        } else {
            prop_assert!(ha >= hb);
        }
    }
}

// ---------------------------------------------------------------------------
// SMF parser: malformed input shouldn't panic (should return Err)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn smf_parser_doesnt_panic_on_garbage(data in proptest::collection::vec(any::<u8>(), 0..1024)) {
        // Should return Err, never panic.
        let _ = smf_parser::parse_smf(&data);
    }

    #[test]
    fn smf_parser_doesnt_panic_on_truncated_header(len in 0usize..80) {
        let mut data = vec![0u8; 80];
        data[..16].copy_from_slice(b"spring map file\0");
        data[16..20].copy_from_slice(&1i32.to_le_bytes()); // version
        let _ = smf_parser::parse_smf(&data[..len]);
    }
}

// ---------------------------------------------------------------------------
// SMD parser: never panics on any input
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn smd_parser_doesnt_panic_on_garbage(text in ".*") {
        let _ = smd_parser::parse_smd(&text);
    }

    #[test]
    fn smd_parser_empty_input(_dummy in 0..1u8) {
        let info = smd_parser::parse_smd("");
        prop_assert!(info.start_positions.is_empty());
    }
}

// ---------------------------------------------------------------------------
// DXT1 RGB565 conversion properties
// ---------------------------------------------------------------------------

// We can't directly test the private rgb565_to_rgba, but we can test
// through the public tile parsing interface. Instead, test the properties
// we know must hold about the conversion.

proptest! {
    #[test]
    fn smf_header_dimensions_consistent(
        map_x in (1i32..=32).prop_map(|x| x * 128),
        map_y in (1i32..=32).prop_map(|y| y * 128),
    ) {
        let header = SmfHeader::new_flat(map_x, map_y, 0.0, 100.0);

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
