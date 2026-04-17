//! Unit preview thumbnails shown in the build menu.
//!
//! One image per `UnitKind`, loaded from `assets/unitpics/` at startup.
//! Missing files fall back to a procedurally-generated faction-coloured
//! shape so the menu never shows a blank slot.

use bevy::prelude::*;

use crate::units::components::Faction;
use crate::units::definitions::{ALL_UNIT_KINDS, UnitKind};
use crate::units::unit_registry::UnitRegistry;

pub struct PreviewsPlugin;

impl Plugin for PreviewsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UnitPreviews>()
            .add_systems(Startup, load_unit_previews);
    }
}

/// Cached preview images for each unit kind, generated at startup.
#[derive(Resource, Default)]
pub struct UnitPreviews {
    images: Vec<(UnitKind, Handle<Image>)>,
}

impl UnitPreviews {
    pub fn get(&self, kind: UnitKind) -> Option<&Handle<Image>> {
        self.images.iter().find(|(k, _)| *k == kind).map(|(_, h)| h)
    }
}

/// Load one buildpic per unit from `assets/unitpics/`, falling back to a
/// procedurally-generated faction-coloured shape when the PNG is missing.
///
/// This mirrors the classic TA/Spring approach: each unit ships a small
/// pre-rendered bitmap (`BuildPic=...` in the FBI) displayed flat in the
/// build menu. Source `.pcx`/`.tga` files are converted to `.png` at
/// asset-cook time so Bevy's default image loader can read them.
fn load_unit_previews(
    mut previews: ResMut<UnitPreviews>,
    mut images: ResMut<Assets<Image>>,
    asset_server: Res<AssetServer>,
    unit_registry: Res<UnitRegistry>,
) {
    let assets_root = std::path::Path::new("kernel-panic/assets/unitpics");
    let assets_root_alt = std::path::Path::new("assets/unitpics");

    for kind in ALL_UNIT_KINDS {
        let declared = unit_registry.build_pic(kind);
        let stem = if declared.is_empty() {
            kind.unitname().to_string()
        } else {
            // Strip extension (.pcx/.tga/.png) and lowercase to match
            // the cooked filename convention.
            std::path::Path::new(declared)
                .file_stem()
                .map(|s| s.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_else(|| kind.unitname().to_string())
        };
        let relative = format!("unitpics/{stem}.png");

        let exists = assets_root.join(format!("{stem}.png")).is_file()
            || assets_root_alt.join(format!("{stem}.png")).is_file();

        let handle = if exists {
            asset_server.load(&relative)
        } else {
            warn!(
                "No buildpic for {:?} (looked for {}), falling back to procedural preview",
                kind, relative
            );
            let faction = kind.faction();
            let is_building = unit_registry.is_building(kind);
            images.add(generate_preview_image(faction, is_building))
        };
        previews.images.push((kind, handle));
    }
}

/// Create a 48x48 RGBA preview image for a unit kind.
fn generate_preview_image(faction: Faction, is_building: bool) -> Image {
    const SIZE: u32 = 48;

    let srgba = Srgba::from(faction.color());
    let r = (srgba.red * 255.0) as u8;
    let g = (srgba.green * 255.0) as u8;
    let b = (srgba.blue * 255.0) as u8;

    let mut pixels = vec![0u8; (SIZE * SIZE * 4) as usize];

    let center = SIZE as f32 / 2.0;
    let radius = center - 4.0;

    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let idx = ((y * SIZE + x) * 4) as usize;

            let dist = if is_building {
                dx.abs() + dy.abs()
            } else {
                (dx * dx + dy * dy).sqrt()
            };

            if dist >= radius - 1.5 && dist < radius + 0.5 {
                // Border
                pixels[idx] = r.saturating_add(40);
                pixels[idx + 1] = g.saturating_add(40);
                pixels[idx + 2] = b.saturating_add(40);
                pixels[idx + 3] = 255;
            } else if dist < radius {
                // Interior with gradient
                let brightness = 1.0 - (dist / radius) * 0.6;
                pixels[idx] = (r as f32 * brightness) as u8;
                pixels[idx + 1] = (g as f32 * brightness) as u8;
                pixels[idx + 2] = (b as f32 * brightness) as u8;
                pixels[idx + 3] = 220;
            }
        }
    }

    Image::new(
        bevy::render::render_resource::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        bevy::render::render_resource::TextureDimension::D2,
        pixels,
        bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
        bevy::asset::RenderAssetUsages::RENDER_WORLD | bevy::asset::RenderAssetUsages::MAIN_WORLD,
    )
}
