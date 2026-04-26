//! Per-unit thumbnail cache for the build menu / info panel.
//!
//! Loads one image per `UnitKind` at startup. Filename comes from the
//! FBI `BuildPic` field (e.g. Network units declare `network_big.png`,
//! `network_minifac.png`, etc.) — only the System / Hacker units happen
//! to share filenames with their unitname stem. Falling back to the
//! unitname for units without a declared `BuildPic` keeps the simple
//! mapping working for shared assets like `bit.png`.
//!
//! Missing files fall back to a procedural faction-tinted shape so the
//! menu never shows a blank slot.

use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::units::components::Faction;
use crate::units::content::definitions::{ALL_UNIT_KINDS, UnitKind};
use crate::units::content::unit_registry::UnitRegistry;

pub(super) struct PreviewsPlugin;

impl Plugin for PreviewsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UnitPreviews>()
            .add_systems(Startup, load_previews);
    }
}

/// Cache of unit-portrait `Handle<Image>`s, keyed by `UnitKind`. Populated
/// once at startup; subsequent lookups are O(N) over the small vec
/// (N is the number of unit kinds, ~30).
#[derive(Resource, Default)]
pub(super) struct UnitPreviews {
    cache: Vec<(UnitKind, Handle<Image>)>,
}

impl UnitPreviews {
    pub fn get(&self, kind: UnitKind) -> Option<&Handle<Image>> {
        self.cache.iter().find(|(k, _)| *k == kind).map(|(_, h)| h)
    }
}

fn load_previews(
    mut previews: ResMut<UnitPreviews>,
    mut images: ResMut<Assets<Image>>,
    asset_server: Res<AssetServer>,
    unit_registry: Res<UnitRegistry>,
) {
    let assets_root = crate::paths::from_project_root("kernel-panic/assets/unitpics");

    for &kind in ALL_UNIT_KINDS {
        // Resolve filename via the FBI BuildPic (without extension), then
        // fall back to the unitname so units that don't declare one still
        // get their own asset rather than a placeholder.
        let declared = unit_registry.build_pic(kind);
        let stem = if declared.is_empty() {
            kind.unitname().to_string()
        } else {
            std::path::Path::new(declared)
                .file_stem()
                .map(|s| s.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_else(|| kind.unitname().to_string())
        };
        let relative = format!("unitpics/{stem}.png");
        let exists = assets_root.join(format!("{stem}.png")).is_file();

        let handle = if exists {
            asset_server.load(&relative)
        } else {
            warn!(
                "No buildpic for {:?} (looked for {}), using procedural placeholder",
                kind, relative,
            );
            let is_building = unit_registry.is_building(kind);
            images.add(synthesise_placeholder(kind.faction(), is_building))
        };
        previews.cache.push((kind, handle));
    }
}

/// 48×48 procedural fallback. Buildings get a diamond, units get a
/// circle; both filled with the faction tint.
fn synthesise_placeholder(faction: Faction, is_building: bool) -> Image {
    use bevy::asset::RenderAssetUsages;

    const SIZE: u32 = 48;
    let [r, g, b] = faction.rgb_f32();
    let r = (r.clamp(0.0, 1.0) * 255.0) as u8;
    let g = (g.clamp(0.0, 1.0) * 255.0) as u8;
    let b = (b.clamp(0.0, 1.0) * 255.0) as u8;

    let mut data = vec![0u8; (SIZE * SIZE * 4) as usize];
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
                data[idx] = r.saturating_add(40);
                data[idx + 1] = g.saturating_add(40);
                data[idx + 2] = b.saturating_add(40);
                data[idx + 3] = 255;
            } else if dist < radius {
                let brightness = 1.0 - (dist / radius) * 0.6;
                data[idx] = (r as f32 * brightness) as u8;
                data[idx + 1] = (g as f32 * brightness) as u8;
                data[idx + 2] = (b as f32 * brightness) as u8;
                data[idx + 3] = 220;
            }
        }
    }

    Image::new(
        Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    )
}
