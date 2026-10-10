//! Parser for Spring Map Definition (.smd) files.
//!
//! The .smd format is a TDF (Tag Definition Format) with nested
//! `[SECTION] { key=value; }` blocks. It predates `mapinfo.lua` and
//! is used by all legacy Spring maps including the Kernel Panic set.

use crate::map_types::TerrainTypes;

/// Parsed map metadata from an .smd file.
#[derive(Debug, Clone, better_default::Default, serde::Serialize, serde::Deserialize)]
pub struct MapInfo {
    pub description: String,
    /// `MAP.gravity` in elmos/s² (positive = down). A map without the
    /// key gets the engine's default, 130 (`CMapInfo::ReadGlobal`,
    /// `maphelper/mapdefaults.lua`); the engine clamps it to ≥ 0.001
    /// and runs it as `-gravity / GAME_SPEED²` per frame².
    #[default(DEFAULT_GRAVITY)]
    pub gravity: f32,
    pub start_positions: Vec<StartPosition>,
    pub atmosphere: Atmosphere,
    pub lighting: Lighting,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StartPosition {
    pub team: u32,
    pub x: f32,
    pub z: f32,
}

#[derive(Debug, Clone, better_default::Default, serde::Serialize, serde::Deserialize)]
pub struct Atmosphere {
    pub fog_color: [f32; 3],
    #[default(0.999)]
    pub fog_start: f32,
    #[default([0.01, 0.01, 0.01])]
    pub sky_color: [f32; 3],
    #[default([1.0, 1.0, 1.0])]
    pub sun_color: [f32; 3],
    pub cloud_density: f32,
}

#[derive(Debug, Clone, better_default::Default, serde::Serialize, serde::Deserialize)]
pub struct Lighting {
    #[default([0.0, 1.0, 1.0])]
    pub sun_dir: [f32; 3],
    #[default([0.5, 0.5, 0.5])]
    pub ground_ambient: [f32; 3],
    #[default([0.5, 0.5, 0.5])]
    pub ground_sun_color: [f32; 3],
    #[default(1.0)]
    pub ground_shadow_density: f32,
}

/// The engine's gravity for a map that sets none (elmos/s²).
pub const DEFAULT_GRAVITY: f32 = 130.0;

/// Parse an .smd file from its raw text content.
pub fn parse_smd(text: &str) -> MapInfo {
    let mut info = MapInfo::default();
    walk_smd(text, |section, key, value| {
        apply_value(&mut info, section, key, value);
    });
    info
}

/// The `[TERRAINTYPEi]` blocks of an .smd: per type, the
/// `tankmovespeed` / `kbotmovespeed` / `hovermovespeed` /
/// `shipmovespeed` multipliers (the engine's `parse_tdf_map.lua` turns
/// `<class>movespeed` keys into `moveSpeeds.<class>`). Types and
/// classes the file leaves out stay at 1; negatives clamp to 0
/// (`CMapInfo::ReadTerrainTypes`).
pub fn parse_terrain_types(text: &str) -> TerrainTypes {
    let mut types = TerrainTypes::default();
    walk_smd(text, |section, key, value| {
        let upper = section.to_ascii_uppercase();
        let Some(index) = upper
            .strip_prefix("MAP.TERRAINTYPE")
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|&n| n < TerrainTypes::COUNT)
        else {
            return;
        };
        let speeds = &mut types.move_speeds[index];
        let speed = parse_f32(value).max(0.0);
        match key {
            "tankmovespeed" => speeds.tank = speed,
            "kbotmovespeed" => speeds.kbot = speed,
            "hovermovespeed" => speeds.hover = speed,
            "shipmovespeed" => speeds.ship = speed,
            _ => {}
        }
    });
    types
}

/// Walk an .smd's `key=value` pairs with the dotted path of the
/// sections they sit in (`MAP.ATMOSPHERE`, `MAP.TEAM0`, …).
fn walk_smd(text: &str, mut visit: impl FnMut(&str, &str, &str)) {
    let mut current_section: Vec<String> = vec![];

    for line in text.lines() {
        let line = strip_comment(line).trim().to_string();
        if line.is_empty() {
            continue;
        }

        // Section open: [SECTIONNAME] or [SECTIONNAME] {
        if line.starts_with('[') {
            let bracket_end = line.find(']').unwrap_or(line.len());
            let name = line[1..bracket_end].to_string();
            current_section.push(name);
            // If the opening brace is on the same line, consume it.
            continue;
        }

        // Block open/close
        if line == "{" {
            continue;
        }
        if line == "}" {
            current_section.pop();
            continue;
        }

        // Key=Value pair (trailing ; and whitespace stripped)
        if let Some((key, value)) = parse_key_value(&line) {
            let section_path = current_section.join(".");
            visit(&section_path, &key, &value);
        }
    }
}

fn strip_comment(line: &str) -> &str {
    // Strip // comments, but be careful not to strip inside strings.
    // Simple approach: find first // and truncate.
    match line.find("//") {
        Some(pos) => &line[..pos],
        None => line,
    }
}

fn parse_key_value(line: &str) -> Option<(String, String)> {
    let line = line.trim_end_matches(';').trim();
    let eq_pos = line.find('=')?;
    let key = line[..eq_pos].trim().to_ascii_lowercase();
    let value = line[eq_pos + 1..].trim().to_string();
    Some((key, value))
}

fn parse_f32(value: &str) -> f32 {
    value.trim().parse().unwrap_or(0.0)
}

fn parse_color3(value: &str) -> [f32; 3] {
    let parts: Vec<f32> = value.split_whitespace().map(parse_f32).collect();
    [
        parts.first().copied().unwrap_or(0.0),
        parts.get(1).copied().unwrap_or(0.0),
        parts.get(2).copied().unwrap_or(0.0),
    ]
}

fn apply_value(info: &mut MapInfo, section: &str, key: &str, value: &str) {
    // Handle TEAM sections: "MAP.TEAM0", "MAP.TEAM1", etc.
    let section_upper = section.to_ascii_uppercase();
    if let Some(team_num) = section_upper
        .strip_prefix("MAP.TEAM")
        .and_then(|s| s.parse::<u32>().ok())
    {
        let entry = info
            .start_positions
            .iter_mut()
            .find(|sp| sp.team == team_num);
        match entry {
            Some(sp) => match key {
                "startposx" => sp.x = parse_f32(value),
                "startposz" => sp.z = parse_f32(value),
                _ => {}
            },
            None => {
                let mut sp = StartPosition {
                    team: team_num,
                    x: 0.0,
                    z: 0.0,
                };
                match key {
                    "startposx" => sp.x = parse_f32(value),
                    "startposz" => sp.z = parse_f32(value),
                    _ => {}
                }
                info.start_positions.push(sp);
            }
        }
        return;
    }

    match (section_upper.as_str(), key) {
        // Top-level MAP section
        ("MAP", "description") => info.description = value.trim_end_matches(';').to_string(),
        ("MAP", "gravity") => info.gravity = parse_f32(value),

        // Atmosphere
        ("MAP.ATMOSPHERE", "fogcolor") => info.atmosphere.fog_color = parse_color3(value),
        ("MAP.ATMOSPHERE", "fogstart") => info.atmosphere.fog_start = parse_f32(value),
        ("MAP.ATMOSPHERE", "skycolor") => info.atmosphere.sky_color = parse_color3(value),
        ("MAP.ATMOSPHERE", "suncolor") => info.atmosphere.sun_color = parse_color3(value),
        ("MAP.ATMOSPHERE", "clouddensity") => info.atmosphere.cloud_density = parse_f32(value),

        // Lighting
        ("MAP.LIGHT", "sundir") => info.lighting.sun_dir = parse_color3(value),
        ("MAP.LIGHT", "groundambientcolor") => info.lighting.ground_ambient = parse_color3(value),
        ("MAP.LIGHT", "groundsuncolor") => info.lighting.ground_sun_color = parse_color3(value),
        ("MAP.LIGHT", "groundshadowdensity") => {
            info.lighting.ground_shadow_density = parse_f32(value);
        }

        _ => {} // ignore unknown keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARBLE_MADNESS_SMD: &str = r#"
[MAP]
{
    Description=Play this map with the Marble Madness mod.;
    Gravity=50;
    [ATMOSPHERE]
    {
        FogColor=0 0 0;
        FogStart=0.001;
        SkyColor=0.01 0.01 0.01;
        SunColor=1 1 1;
        CloudDensity=0;
    }
    [LIGHT]
    {
        SunDir=0 1 1;
        GroundAmbientColor=0.5 0.5 0.5;
        GroundSunColor=0.5 0.5 0.5;
        GroundShadowDensity=1.0;
    }
    [TEAM0]
    {
        StartPosX=1792;
        StartPosZ=1792;
    }
    [TEAM1]
    {
        StartPosX=256;
        StartPosZ=256;
    }
    [TEAM2]
    {
        StartPosX=1792;
        StartPosZ=256;
    }
    [TEAM3]
    {
        StartPosX=256;
        StartPosZ=1792;
    }
}
"#;

    #[test]
    fn parse_start_positions() {
        let info = parse_smd(MARBLE_MADNESS_SMD);
        assert_eq!(info.start_positions.len(), 4);
        assert_eq!(info.start_positions[0].team, 0);
        assert!((info.start_positions[0].x - 1792.0).abs() < 0.1);
        assert!((info.start_positions[0].z - 1792.0).abs() < 0.1);
        assert_eq!(info.start_positions[1].team, 1);
        assert!((info.start_positions[1].x - 256.0).abs() < 0.1);
    }

    #[test]
    fn parse_atmosphere() {
        let info = parse_smd(MARBLE_MADNESS_SMD);
        assert!((info.atmosphere.fog_start - 0.001).abs() < 0.0001);
        assert!((info.atmosphere.sky_color[0] - 0.01).abs() < 0.001);
        assert!((info.atmosphere.sun_color[0] - 1.0).abs() < 0.001);
    }

    #[test]
    fn parse_lighting() {
        let info = parse_smd(MARBLE_MADNESS_SMD);
        assert!((info.lighting.sun_dir[1] - 1.0).abs() < 0.001);
        assert!((info.lighting.ground_ambient[0] - 0.5).abs() < 0.001);
    }

    #[test]
    fn parse_gravity() {
        let info = parse_smd(MARBLE_MADNESS_SMD);
        assert!((info.gravity - 50.0).abs() < 0.1);
    }

    #[test]
    fn parse_real_map() {
        let sd7_path = crate::shipped_map("Central_Hub.sd7");
        let extracted = crate::sd7_archive::load_map_archive(&sd7_path).unwrap();
        let smd_text = extracted.smd_text.expect("Central Hub should have .smd");
        let info = parse_smd(&smd_text);

        // Central Hub has 16 team positions
        assert_eq!(info.start_positions.len(), 16);
        // First team at (606, 606)
        assert!((info.start_positions[0].x - 606.0).abs() < 0.1);
        // KP atmosphere: dark sky
        assert!(info.atmosphere.sky_color[0] < 0.1);
    }

    /// A map without `gravity=` runs on the engine's default.
    #[test]
    fn missing_gravity_is_engine_default() {
        let info = parse_smd("[MAP]\n{\n  Description=x;\n}\n");
        assert_eq!(info.gravity, DEFAULT_GRAVITY);
    }

    #[test]
    fn parse_terrain_type_speeds() {
        let text = "[MAP]\n{\n\
            \t[TERRAINTYPE0]\n\t{\n\t\tname=ROM;\n\t\ttankmovespeed=0;\n\t\tkbotmovespeed=0;\n\t}\n\
            \t[TERRAINTYPE3] {\n\t\tkbotmovespeed=4;\n\t\thovermovespeed=-2;\n\t}\n\
            }\n";
        let types = parse_terrain_types(text);
        assert_eq!(types.get(0).kbot, 0.0);
        assert_eq!(types.get(0).tank, 0.0);
        assert_eq!(types.get(0).ship, 1.0, "unset class stays neutral");
        assert_eq!(types.get(3).kbot, 4.0);
        assert_eq!(types.get(3).hover, 0.0, "negative clamps to 0");
        assert_eq!(types.get(7).kbot, 1.0, "unset type stays neutral");
        assert!(parse_terrain_types(MARBLE_MADNESS_SMD).is_neutral());
    }

    /// Direct Memory Access's memory classes — ROM closed, EEPROM slow,
    /// RAM normal, SDRAM fast — and its typemap paints all four.
    #[test]
    fn real_map_terrain_types() {
        let sd7_path = crate::shipped_map("Direct_Memory_Access_0.5e_beta.sd7");
        let extracted = crate::sd7_archive::load_map_archive(&sd7_path).unwrap();
        let types = parse_terrain_types(&extracted.smd_text.unwrap());
        let kbot: Vec<f32> = (0..4).map(|i| types.get(i).kbot).collect();
        assert_eq!(kbot, vec![0.0, 0.25, 1.0, 4.0]);
        let parsed = crate::smf_parser::parse_smf(&extracted.smf_data).unwrap();
        assert_eq!(
            parsed.typemap.len(),
            parsed.header.typemap_width() * parsed.header.typemap_height()
        );
        let mut seen = [0usize; 256];
        for &t in &parsed.typemap {
            seen[t as usize] += 1;
        }
        assert!(seen[..4].iter().all(|&n| n > 0), "{:?}", &seen[..4]);
    }
}
