use std::io::{Cursor, Read, Seek, SeekFrom};

use binrw::BinRead;
use byteorder::{LittleEndian, ReadBytesExt};

use crate::map_types::{FeatureType, MapFeature, ParsedMap, SMF_VERSION, SmfHeader, SmfParseError};

/// Parse a complete SMF file from raw bytes.
pub fn parse_smf(data: &[u8]) -> Result<ParsedMap, SmfParseError> {
    let mut cursor = Cursor::new(data);

    let header = read_header(&mut cursor)?;
    let heights = read_heightmap(&mut cursor, &header)?;
    let features = read_features(&mut cursor, &header)?;
    let metalmap = read_metalmap(&mut cursor, &header)?;

    Ok(ParsedMap {
        header,
        heights,
        features,
        metalmap,
    })
}

fn read_header(cursor: &mut Cursor<&[u8]>) -> Result<SmfHeader, SmfParseError> {
    let header = SmfHeader::read(cursor).map_err(|err| match err {
        binrw::Error::BadMagic { .. } => SmfParseError::BadMagic,
        binrw::Error::Io(io) => SmfParseError::Io(io),
        other => SmfParseError::Io(std::io::Error::other(other.to_string())),
    })?;
    if header.version != SMF_VERSION {
        return Err(SmfParseError::BadVersion(header.version));
    }
    Ok(header)
}

fn read_heightmap(
    cursor: &mut Cursor<&[u8]>,
    header: &SmfHeader,
) -> Result<Vec<f32>, SmfParseError> {
    cursor.seek(SeekFrom::Start(header.heightmap_ptr as u64))?;

    let expected = header.heightmap_len();
    let byte_count = expected * 2;
    let mut raw_bytes = vec![0u8; byte_count];
    cursor
        .read_exact(&mut raw_bytes)
        .map_err(|_| SmfParseError::HeightmapTruncated {
            expected,
            actual: 0,
        })?;

    let heights = raw_bytes
        .chunks_exact(2)
        .map(|chunk| {
            let raw = i16::from_le_bytes([chunk[0], chunk[1]]);
            header.sample_to_world_height(raw)
        })
        .collect();

    Ok(heights)
}

/// Read features and resolve type names inline.
fn read_features(
    cursor: &mut Cursor<&[u8]>,
    header: &SmfHeader,
) -> Result<Vec<MapFeature>, SmfParseError> {
    cursor.seek(SeekFrom::Start(header.feature_ptr as u64))?;

    let num_feature_types = cursor
        .read_i32::<LittleEndian>()
        .map_err(|_| SmfParseError::FeatureTruncated)?;
    let num_features = cursor
        .read_i32::<LittleEndian>()
        .map_err(|_| SmfParseError::FeatureTruncated)?;

    let num_feature_types =
        usize::try_from(num_feature_types).map_err(|_| SmfParseError::FeatureTruncated)?;
    let num_features =
        usize::try_from(num_features).map_err(|_| SmfParseError::FeatureTruncated)?;

    // No `with_capacity` on attacker-controlled counts: a 2GB claim would OOM before
    // we discover the file is truncated. Let the Vec grow on demand.
    let mut type_names = Vec::new();
    for _ in 0..num_feature_types {
        type_names.push(read_null_terminated_string(cursor)?);
    }

    let mut features = Vec::new();
    for _ in 0..num_features {
        let type_index_raw = cursor
            .read_i32::<LittleEndian>()
            .map_err(|_| SmfParseError::FeatureTruncated)?;
        let type_index = usize::try_from(type_index_raw).unwrap_or(usize::MAX);
        let x = cursor
            .read_f32::<LittleEndian>()
            .map_err(|_| SmfParseError::FeatureTruncated)?;
        let y = cursor
            .read_f32::<LittleEndian>()
            .map_err(|_| SmfParseError::FeatureTruncated)?;
        let z = cursor
            .read_f32::<LittleEndian>()
            .map_err(|_| SmfParseError::FeatureTruncated)?;
        let rotation = cursor
            .read_f32::<LittleEndian>()
            .map_err(|_| SmfParseError::FeatureTruncated)?;
        let relative_size = cursor
            .read_f32::<LittleEndian>()
            .map_err(|_| SmfParseError::FeatureTruncated)?;

        let raw_name = type_names
            .get(type_index)
            .map(String::as_str)
            .unwrap_or("Unknown");
        let feature_type = FeatureType::from_name(raw_name);

        features.push(MapFeature::new(
            feature_type,
            x,
            y,
            z,
            rotation,
            relative_size,
        ));
    }

    Ok(features)
}

fn read_metalmap(cursor: &mut Cursor<&[u8]>, header: &SmfHeader) -> Result<Vec<u8>, SmfParseError> {
    cursor.seek(SeekFrom::Start(header.metalmap_ptr as u64))?;

    let expected = header.metalmap_width() * header.metalmap_height();
    let mut metalmap = vec![0u8; expected];
    cursor
        .read_exact(&mut metalmap)
        .map_err(|_| SmfParseError::MetalmapTruncated {
            expected,
            actual: 0,
        })?;

    Ok(metalmap)
}

fn read_null_terminated_string(cursor: &mut Cursor<&[u8]>) -> Result<String, SmfParseError> {
    let mut bytes = Vec::new();
    loop {
        let byte = cursor
            .read_u8()
            .map_err(|_| SmfParseError::FeatureTruncated)?;
        if byte == 0 {
            break;
        }
        bytes.push(byte);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_types::SMF_MAGIC;

    fn build_test_smf() -> Vec<u8> {
        let mut buf = Vec::new();

        let map_x: i32 = 128;
        let map_y: i32 = 128;
        let hm_width = (map_x + 1) as usize;
        let hm_height = (map_y + 1) as usize;
        let hm_samples = hm_width * hm_height;

        let header_size: i32 = 80;
        let heightmap_ptr = header_size;
        let heightmap_bytes = (hm_samples * 2) as i32;
        let metalmap_ptr = heightmap_ptr + heightmap_bytes;
        let mm_w = (map_x / 2) as usize;
        let mm_h = (map_y / 2) as usize;
        let metalmap_size = (mm_w * mm_h) as i32;
        let feature_ptr = metalmap_ptr + metalmap_size;

        buf.extend_from_slice(SMF_MAGIC);
        buf.extend_from_slice(&1i32.to_le_bytes());
        buf.extend_from_slice(&42i32.to_le_bytes());
        buf.extend_from_slice(&map_x.to_le_bytes());
        buf.extend_from_slice(&map_y.to_le_bytes());
        buf.extend_from_slice(&8i32.to_le_bytes());
        buf.extend_from_slice(&8i32.to_le_bytes());
        buf.extend_from_slice(&32i32.to_le_bytes());
        buf.extend_from_slice(&0.0f32.to_le_bytes());
        buf.extend_from_slice(&100.0f32.to_le_bytes());
        buf.extend_from_slice(&heightmap_ptr.to_le_bytes());
        buf.extend_from_slice(&0i32.to_le_bytes());
        buf.extend_from_slice(&0i32.to_le_bytes());
        buf.extend_from_slice(&0i32.to_le_bytes());
        buf.extend_from_slice(&metalmap_ptr.to_le_bytes());
        buf.extend_from_slice(&feature_ptr.to_le_bytes());
        buf.extend_from_slice(&0i32.to_le_bytes());
        assert_eq!(buf.len(), header_size as usize);

        for gz in 0..hm_height {
            for _gx in 0..hm_width {
                let t = gz as f32 / (hm_height - 1) as f32;
                let raw = (t * i16::MAX as f32) as i16;
                buf.extend_from_slice(&raw.to_le_bytes());
            }
        }

        let mut metalmap = vec![0u8; mm_w * mm_h];
        metalmap[32 * mm_w + 32] = 255;
        buf.extend_from_slice(&metalmap);

        buf.extend_from_slice(&1i32.to_le_bytes());
        buf.extend_from_slice(&1i32.to_le_bytes());
        buf.extend_from_slice(b"GeoVent\0");
        buf.extend_from_slice(&0i32.to_le_bytes());
        buf.extend_from_slice(&512.0f32.to_le_bytes());
        buf.extend_from_slice(&10.0f32.to_le_bytes());
        buf.extend_from_slice(&512.0f32.to_le_bytes());
        buf.extend_from_slice(&0.0f32.to_le_bytes());
        buf.extend_from_slice(&1.0f32.to_le_bytes());

        buf
    }

    #[test]
    fn parse_header() {
        let parsed = parse_smf(&build_test_smf()).unwrap();
        assert_eq!(parsed.header.map_x, 128);
        assert_eq!(parsed.header.map_y, 128);
        assert_eq!(parsed.header.heightmap_width(), 129);
        assert_eq!(parsed.header.heightmap_height(), 129);
    }

    #[test]
    fn parse_heightmap_values() {
        let parsed = parse_smf(&build_test_smf()).unwrap();
        assert_eq!(parsed.heights.len(), 129 * 129);
        assert!((parsed.heights[0]).abs() < 0.01);
        assert!((parsed.heights[128 * 129] - 50.0).abs() < 0.1);
    }

    #[test]
    fn parse_features_with_resolved_names() {
        let parsed = parse_smf(&build_test_smf()).unwrap();
        assert_eq!(parsed.features.len(), 1);
        assert!(parsed.features[0].feature_type.is_geovent());
        assert!((parsed.features[0].x - 512.0).abs() < 0.01);
    }

    #[test]
    fn parse_metalmap() {
        let parsed = parse_smf(&build_test_smf()).unwrap();
        assert_eq!(parsed.metalmap.len(), 64 * 64);
        assert_eq!(parsed.metalmap[32 * 64 + 32], 255);
    }

    #[test]
    fn reject_bad_magic() {
        let mut data = build_test_smf();
        data[0] = b'X';
        assert!(matches!(parse_smf(&data), Err(SmfParseError::BadMagic)));
    }

    #[test]
    fn reject_bad_version() {
        let mut data = build_test_smf();
        data[16..20].copy_from_slice(&99i32.to_le_bytes());
        assert!(matches!(
            parse_smf(&data),
            Err(SmfParseError::BadVersion(99))
        ));
    }

    #[test]
    fn parse_real_marble_madness() {
        let sd7_path = [
            "kernel-panic/assets/maps/Marble_Madness_Map.sd7",
            "assets/maps/Marble_Madness_Map.sd7",
        ]
        .iter()
        .map(std::path::Path::new)
        .find(|p| p.exists());
        let Some(sd7_path) = sd7_path else {
            eprintln!("Skipping: sd7 not found");
            return;
        };
        let extracted = crate::sd7_archive::load_map_archive(sd7_path).unwrap();
        let parsed = parse_smf(&extracted.smf_data).unwrap();

        assert_eq!(parsed.header.map_x, 256);
        assert_eq!(parsed.heights.len(), 257 * 257);
        assert!(!parsed.features.is_empty());

        let geovents: Vec<_> = parsed
            .features
            .iter()
            .filter(|f| f.feature_type.is_geovent())
            .collect();
        assert!(!geovents.is_empty(), "should have GeoVent features");
    }
}
