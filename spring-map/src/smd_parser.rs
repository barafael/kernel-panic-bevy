//! Parser for Spring Map Definition (.smd) files.
//!
//! The .smd format is a TDF (Tag Definition Format) with nested
//! `[SECTION] { key=value; }` blocks. It predates `mapinfo.lua` and
//! is used by all legacy Spring maps including the Kernel Panic set.

/// Parsed map metadata from an .smd file.
#[derive(Debug, Clone, Default)]
pub struct MapInfo {
    pub description: String,
    pub gravity: f32,
    pub start_positions: Vec<StartPosition>,
    pub atmosphere: Atmosphere,
    pub lighting: Lighting,
}

#[derive(Debug, Clone)]
pub struct StartPosition {
    pub team: u32,
    pub x: f32,
    pub z: f32,
}

#[derive(Debug, Clone)]
pub struct Atmosphere {
    pub fog_color: [f32; 3],
    pub fog_start: f32,
    pub sky_color: [f32; 3],
    pub sun_color: [f32; 3],
    pub cloud_density: f32,
}

impl Default for Atmosphere {
    fn default() -> Self {
        Self {
            fog_color: [0.0, 0.0, 0.0],
            fog_start: 0.999,
            sky_color: [0.01, 0.01, 0.01],
            sun_color: [1.0, 1.0, 1.0],
            cloud_density: 0.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Lighting {
    pub sun_dir: [f32; 3],
    pub ground_ambient: [f32; 3],
    pub ground_sun_color: [f32; 3],
    pub ground_shadow_density: f32,
}

impl Default for Lighting {
    fn default() -> Self {
        Self {
            sun_dir: [0.0, 1.0, 1.0],
            ground_ambient: [0.5, 0.5, 0.5],
            ground_sun_color: [0.5, 0.5, 0.5],
            ground_shadow_density: 1.0,
        }
    }
}

/// Parse an .smd file from its raw text content.
pub fn parse_smd(text: &str) -> MapInfo {
    let mut info = MapInfo::default();

    let mut current_section: Vec<String> = vec![];

    for line in text.lines() {
        let line = strip_comment(line).trim().to_string();
        if line.is_empty() {
            continue;
        }

        // Section open: [SECTIONNAME]
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].to_string();
            current_section.push(name);
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
            apply_value(&mut info, &section_path, &key, &value);
        }
    }

    info
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
        let sd7_path = [
            "kernel-panic/assets/maps/Central_Hub.sd7",
            "assets/maps/Central_Hub.sd7",
        ]
        .iter()
        .map(std::path::Path::new)
        .find(|p| p.exists());
        let Some(sd7_path) = sd7_path else {
            eprintln!("Skipping: map not found");
            return;
        };
        let extracted = crate::sd7_archive::load_map_archive(sd7_path).unwrap();
        let smd_text = extracted.smd_text.expect("Central Hub should have .smd");
        let info = parse_smd(&smd_text);

        // Central Hub has 16 team positions
        assert_eq!(info.start_positions.len(), 16);
        // First team at (606, 606)
        assert!((info.start_positions[0].x - 606.0).abs() < 0.1);
        // KP atmosphere: dark sky
        assert!(info.atmosphere.sky_color[0] < 0.1);
    }
}
