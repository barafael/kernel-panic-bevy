//! Writer for Spring Map Definition (.smd) metadata files.

use crate::StartPosition;

/// Builder for constructing an SMD metadata file.
#[derive(Clone)]
pub struct SmdBuilder {
    description: String,
    gravity: f32,
    start_positions: Vec<StartPosition>,
    fog_color: [f32; 3],
    fog_start: f32,
    sky_color: [f32; 3],
    sun_color: [f32; 3],
    cloud_density: f32,
    sun_dir: [f32; 3],
    ground_ambient: [f32; 3],
    ground_sun_color: [f32; 3],
    ground_shadow_density: f32,
}

impl Default for SmdBuilder {
    fn default() -> Self {
        Self {
            description: "Generated test map".to_string(),
            gravity: 50.0,
            start_positions: Vec::new(),
            fog_color: [0.0, 0.0, 0.0],
            fog_start: 0.999,
            sky_color: [0.01, 0.01, 0.01],
            sun_color: [1.0, 1.0, 1.0],
            cloud_density: 0.0,
            sun_dir: [0.0, 1.0, 1.0],
            ground_ambient: [0.5, 0.5, 0.5],
            ground_sun_color: [0.5, 0.5, 0.5],
            ground_shadow_density: 1.0,
        }
    }
}

impl SmdBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn description(mut self, desc: &str) -> Self {
        self.description = desc.to_string();
        self
    }

    pub fn gravity(mut self, g: f32) -> Self {
        self.gravity = g;
        self
    }

    pub fn add_start_position(&mut self, team: u32, x: f32, z: f32) {
        self.start_positions.push(StartPosition { team, x, z });
    }

    pub fn fog_color(mut self, rgb: [f32; 3]) -> Self {
        self.fog_color = rgb;
        self
    }

    pub fn fog_start(mut self, start: f32) -> Self {
        self.fog_start = start;
        self
    }

    pub fn sky_color(mut self, rgb: [f32; 3]) -> Self {
        self.sky_color = rgb;
        self
    }

    pub fn sun_color(mut self, rgb: [f32; 3]) -> Self {
        self.sun_color = rgb;
        self
    }

    pub fn sun_dir(mut self, dir: [f32; 3]) -> Self {
        self.sun_dir = dir;
        self
    }

    pub fn ground_ambient(mut self, rgb: [f32; 3]) -> Self {
        self.ground_ambient = rgb;
        self
    }

    pub fn ground_sun_color(mut self, rgb: [f32; 3]) -> Self {
        self.ground_sun_color = rgb;
        self
    }

    /// Build the .smd text content.
    pub fn build(&self) -> String {
        let mut s = String::new();
        s.push_str("[MAP]\n{\n");
        s.push_str(&format!("    Description={};\n", self.description));
        s.push_str(&format!("    Gravity={};\n", self.gravity));

        // Atmosphere.
        s.push_str("    [ATMOSPHERE]\n    {\n");
        s.push_str(&format!(
            "        FogColor={} {} {};\n",
            self.fog_color[0], self.fog_color[1], self.fog_color[2]
        ));
        s.push_str(&format!("        FogStart={};\n", self.fog_start));
        s.push_str(&format!(
            "        SkyColor={} {} {};\n",
            self.sky_color[0], self.sky_color[1], self.sky_color[2]
        ));
        s.push_str(&format!(
            "        SunColor={} {} {};\n",
            self.sun_color[0], self.sun_color[1], self.sun_color[2]
        ));
        s.push_str(&format!("        CloudDensity={};\n", self.cloud_density));
        s.push_str("    }\n");

        // Lighting.
        s.push_str("    [LIGHT]\n    {\n");
        s.push_str(&format!(
            "        SunDir={} {} {};\n",
            self.sun_dir[0], self.sun_dir[1], self.sun_dir[2]
        ));
        s.push_str(&format!(
            "        GroundAmbientColor={} {} {};\n",
            self.ground_ambient[0], self.ground_ambient[1], self.ground_ambient[2]
        ));
        s.push_str(&format!(
            "        GroundSunColor={} {} {};\n",
            self.ground_sun_color[0], self.ground_sun_color[1], self.ground_sun_color[2]
        ));
        s.push_str(&format!(
            "        GroundShadowDensity={};\n",
            self.ground_shadow_density
        ));
        s.push_str("    }\n");

        // Start positions.
        for sp in &self.start_positions {
            s.push_str(&format!("    [TEAM{}]\n    {{\n", sp.team));
            s.push_str(&format!("        StartPosX={};\n", sp.x));
            s.push_str(&format!("        StartPosZ={};\n", sp.z));
            s.push_str("    }\n");
        }

        s.push_str("}\n");
        s
    }
}
