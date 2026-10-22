//! The map selector's turntable: the map under the pointer (or the
//! keyboard focus) turning slowly in the page's main area.
//!
//! Each map has a small preview, `assets/maps/previews/<stem>.png`
//! (colour = the ground texture, alpha = the height), written by
//! `spring-map`'s `gen_previews` example, and a line of
//! `previews/relief.txt` that turns the alpha back into elmos. On
//! hover the PNG loads (fetched, on the web), becomes a shaded terrain
//! mesh on its own render layer, and a camera of its own renders it
//! into [`MapPreview::target`], which the page shows as an image. The
//! camera and the mesh exist only while the map list is up.

use bevy::asset::RenderAssetUsages;
use bevy::camera::RenderTarget;
use bevy::camera::visibility::RenderLayers;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;

use super::{MenuButton, MenuFocus, MenuPage};
use crate::game_setup::SkirmishConfig;
use crate::map_loading::{MapCatalog, PersistentEntity};
use crate::sim::DetFloat;

/// The render layer only the preview camera and mesh are on.
const LAYER: usize = 23;
/// The preview image's size: 16:10, the page frames it at that ratio.
/// Twice the frame it is shown in on a 1440p page, so the turntable's
/// terraces stay crisp when the page is scaled up.
pub(super) const TARGET_SIZE: UVec2 = UVec2::new(2560, 1600);
/// Turntable speed (rad/s).
const SPIN: f32 = 0.25;
/// Heights relative to true scale. Drawn true: the 2× that was meant to
/// make terraces read from above made every map look spiky instead.
const RELIEF: f32 = 1.0;
/// One mesh vertex per this many preview texels.
const STEP: usize = 2;

/// The preview's state: its render target, which map it shows, and the
/// camera and mesh while the map list is up.
#[derive(Resource)]
pub(super) struct MapPreview {
    pub target: Handle<Image>,
    /// The map the page asks for: hovered, else the last hovered, else
    /// the skirmish's map, else the first.
    wanted: Option<String>,
    /// The map whose mesh is up, and the one whose PNG is loading.
    shown: Option<String>,
    loading: Option<(String, Handle<Image>)>,
    camera: Option<Entity>,
    terrain: Option<Entity>,
    angle: f32,
}

impl FromWorld for MapPreview {
    fn from_world(world: &mut World) -> Self {
        // An sRGB texture rendered into and sampled as such — not Bevy's
        // `render_to_texture` pair of a Unorm texture with an sRGB view
        // format: that needs wgpu's `VIEW_FORMATS` downlevel flag, which
        // WebGL2 (Firefox) lacks, and the first frame then quit the whole
        // app with a validation error.
        let image = Image::new_target_texture(
            TARGET_SIZE.x,
            TARGET_SIZE.y,
            TextureFormat::Rgba8UnormSrgb,
            None,
        );
        Self {
            target: world.resource_mut::<Assets<Image>>().add(image),
            wanted: None,
            shown: None,
            loading: None,
            camera: None,
            terrain: None,
            angle: 0.0,
        }
    }
}

impl MapPreview {
    /// The map the turntable shows (or is loading).
    pub fn map(&self) -> Option<&str> {
        self.wanted.as_deref()
    }
}

pub(super) struct MapPreviewPlugin;

impl Plugin for MapPreviewPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MapPreview>().add_systems(
            Update,
            (choose_map, drive_turntable)
                .chain()
                .run_if(in_state(crate::game_setup::AppState::Menu)),
        );
        app.add_systems(
            OnExit(crate::game_setup::AppState::Menu),
            |mut commands: Commands, mut preview: ResMut<MapPreview>| {
                tear_down(&mut commands, &mut preview);
            },
        );
    }
}

/// Follow the pointer (or keyboard focus) over the map list.
fn choose_map(
    page: Res<MenuPage>,
    focus: Res<MenuFocus>,
    buttons: Query<&MenuButton>,
    catalog: Option<Res<MapCatalog>>,
    config: Res<SkirmishConfig>,
    mut preview: ResMut<MapPreview>,
) {
    if *page != MenuPage::MapList {
        if preview.wanted.is_some() {
            preview.wanted = None;
        }
        return;
    }
    let Some(catalog) = catalog else {
        return;
    };
    let names = catalog.names();
    let hovered = focus
        .entity
        .and_then(|e| buttons.get(e).ok())
        .and_then(|b| match b.action {
            super::MenuAction::PickMap(i) => names.get(i).cloned(),
            _ => None,
        });
    let wanted = hovered.or_else(|| preview.wanted.clone()).or_else(|| {
        config
            .map
            .and_then(|i| names.get(i).cloned())
            .or_else(|| names.first().cloned())
    });
    if preview.wanted != wanted {
        preview.wanted = wanted;
    }
}

/// Spawn the camera with the page, swap in the wanted map's mesh once
/// its PNG has loaded, and turn it.
#[allow(clippy::too_many_arguments)]
fn drive_turntable(
    time: Res<Time>,
    asset_server: Res<AssetServer>,
    images: Res<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut preview: ResMut<MapPreview>,
    mut transforms: Query<&mut Transform>,
    mut commands: Commands,
) {
    let preview = &mut *preview;
    let Some(wanted) = preview.wanted.clone() else {
        tear_down(&mut commands, preview);
        return;
    };
    if preview.camera.is_none() {
        preview.camera = Some(
            commands
                .spawn((
                    PersistentEntity,
                    Camera3d::default(),
                    Camera {
                        // Before the window's camera, which shows it.
                        order: -1,
                        clear_color: Color::srgb(0.02, 0.02, 0.03).into(),
                        ..default()
                    },
                    RenderTarget::Image(preview.target.clone().into()),
                    Msaa::Off,
                    Transform::from_xyz(0.0, 0.72, 1.12)
                        .looking_at(Vec3::new(0.0, -0.02, 0.0), Vec3::Y),
                    RenderLayers::layer(LAYER),
                ))
                .id(),
        );
    }
    if preview.shown.as_deref() != Some(wanted.as_str())
        && preview.loading.as_ref().is_none_or(|(m, _)| *m != wanted)
    {
        let handle = asset_server.load(format!("maps/previews/{wanted}.png"));
        preview.loading = Some((wanted.clone(), handle));
    }
    if let Some((map, handle)) = &preview.loading
        && let Some(image) = images.get(handle)
    {
        if let Some(mesh) = relief(map).and_then(|r| terrain_mesh(image, r)) {
            if let Some(old) = preview.terrain.take() {
                commands.entity(old).despawn();
            }
            let material = materials.add(StandardMaterial {
                base_color_texture: Some(handle.clone()),
                unlit: true,
                ..default()
            });
            preview.terrain = Some(
                commands
                    .spawn((
                        PersistentEntity,
                        Mesh3d(meshes.add(mesh)),
                        MeshMaterial3d(material),
                        Transform::from_rotation(Quat::from_rotation_y(preview.angle)),
                        RenderLayers::layer(LAYER),
                    ))
                    .id(),
            );
        }
        preview.shown = Some(map.clone());
        preview.loading = None;
    }
    preview.angle += SPIN * time.delta_secs();
    if let Some(mut tf) = preview.terrain.and_then(|e| transforms.get_mut(e).ok()) {
        tf.rotation = Quat::from_rotation_y(preview.angle);
    }
}

fn tear_down(commands: &mut Commands, preview: &mut MapPreview) {
    for e in [preview.camera.take(), preview.terrain.take()]
        .into_iter()
        .flatten()
    {
        commands.entity(e).despawn();
    }
    preview.shown = None;
    preview.loading = None;
}

/// A map's height range and size in elmos, from `previews/relief.txt`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Relief {
    pub low: f32,
    pub high: f32,
    pub width: f32,
    pub depth: f32,
}

const RELIEF_TABLE: &str = include_str!("../../../assets/maps/previews/relief.txt");

pub(super) fn relief(map: &str) -> Option<Relief> {
    RELIEF_TABLE.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        if words.next()? != map {
            return None;
        }
        let mut num = || words.next()?.parse::<f32>().ok();
        Some(Relief {
            low: num()?,
            high: num()?,
            width: num()?,
            depth: num()?,
        })
    })
}

/// The preview as a terrain mesh: the longest side one unit long,
/// centred on the origin, heights from the alpha at [`RELIEF`]× true
/// scale, the colour shaded by a fixed light baked into the vertex
/// colours (the material is unlit).
fn terrain_mesh(image: &Image, relief: Relief) -> Option<Mesh> {
    let data = image.data.as_ref()?;
    let (w, d) = (image.width() as usize, image.height() as usize);
    if data.len() < w * d * 4 || w < 2 || d < 2 {
        return None;
    }
    let (gw, gd) = ((w - 1) / STEP + 1, (d - 1) / STEP + 1);
    let longest = relief.width.det_max(relief.depth).det_max(1.0);
    let (sx, sz) = (relief.width / longest, relief.depth / longest);
    let lift = (relief.high - relief.low) / longest * RELIEF / 255.0;
    let height = |gx: usize, gz: usize| {
        let (x, z) = ((gx * STEP).min(w - 1), (gz * STEP).min(d - 1));
        data[(z * w + x) * 4 + 3] as f32 * lift
    };
    let mut positions = Vec::with_capacity(gw * gd);
    let mut uvs = Vec::with_capacity(gw * gd);
    for gz in 0..gd {
        for gx in 0..gw {
            let (u, v) = (
                (gx * STEP).min(w - 1) as f32 / (w - 1) as f32,
                (gz * STEP).min(d - 1) as f32 / (d - 1) as f32,
            );
            positions.push([(u - 0.5) * sx, height(gx, gz), (v - 0.5) * sz]);
            uvs.push([u, v]);
        }
    }
    let light = Vec3::new(-0.4, 1.0, -0.3).normalize();
    let mut colors = Vec::with_capacity(gw * gd);
    for gz in 0..gd {
        for gx in 0..gw {
            let at = |x: usize, z: usize| Vec3::from(positions[z * gw + x]);
            let (l, r) = (at(gx.saturating_sub(1), gz), at((gx + 1).min(gw - 1), gz));
            let (n, s) = (at(gx, gz.saturating_sub(1)), at(gx, (gz + 1).min(gd - 1)));
            let normal = (s - n).cross(r - l).normalize_or(Vec3::Y);
            let shade = 0.45 + 0.55 * normal.dot(light).det_max(0.0);
            colors.push([shade, shade, shade, 1.0]);
        }
    }
    let mut indices = Vec::with_capacity((gw - 1) * (gd - 1) * 6);
    for gz in 0..gd - 1 {
        for gx in 0..gw - 1 {
            let i = (gz * gw + gx) as u32;
            let (right, down) = (i + 1, i + gw as u32);
            indices.extend_from_slice(&[i, down, right, right, down, down + 1]);
        }
    }
    Some(
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
        .with_inserted_indices(Indices::U32(indices)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_map_has_a_relief_line() {
        for stem in crate::map_loading::MAP_CATALOG.iter() {
            let r = relief(stem).unwrap_or_else(|| panic!("{stem}: no relief line"));
            assert!(
                r.high >= r.low && r.width > 0.0 && r.depth > 0.0,
                "{stem}: {r:?}"
            );
        }
    }

    /// Each map's preview was made from the map as it is now (the hash
    /// in `relief.txt`); rerun `spring-map`'s `gen_previews` if not.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn previews_match_the_maps() {
        let fnv64 = |bytes: &[u8]| {
            bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
                (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
            })
        };
        let maps = crate::paths::assets_dir().join("maps");
        for stem in crate::map_loading::MAP_CATALOG.iter() {
            let bytes = std::fs::read(maps.join(format!("{stem}.kpmap"))).unwrap();
            let line = RELIEF_TABLE
                .lines()
                .find(|l| l.split_whitespace().next() == Some(stem))
                .unwrap();
            let hash = line.split_whitespace().nth(5).unwrap_or("");
            assert_eq!(
                hash,
                format!("{:016x}", fnv64(&bytes)),
                "{stem}: preview is stale, rerun gen_previews"
            );
            assert!(maps.join(format!("previews/{stem}.png")).exists(), "{stem}");
        }
    }

    #[test]
    fn the_mesh_spans_the_map_and_rises_with_the_alpha() {
        let (w, d) = (9usize, 5usize);
        let mut data = vec![0u8; w * d * 4];
        data[(2 * w + 4) * 4 + 3] = 255;
        let image = Image::new(
            bevy::render::render_resource::Extent3d {
                width: w as u32,
                height: d as u32,
                depth_or_array_layers: 1,
            },
            bevy::render::render_resource::TextureDimension::D2,
            data,
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::MAIN_WORLD,
        );
        let relief = Relief {
            low: 0.0,
            high: 100.0,
            width: 1000.0,
            depth: 500.0,
        };
        let mesh = terrain_mesh(&image, relief).unwrap();
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(p)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("positions");
        };
        assert_eq!(p.len(), 5 * 3);
        let max_x = p.iter().map(|v| v[0]).fold(f32::MIN, |a, b| a.det_max(b));
        let max_z = p.iter().map(|v| v[2]).fold(f32::MIN, |a, b| a.det_max(b));
        assert!((max_x - 0.5).abs() < 1e-5 && (max_z - 0.25).abs() < 1e-5);
        let top = p.iter().map(|v| v[1]).fold(f32::MIN, |a, b| a.det_max(b));
        assert!((top - 100.0 / 1000.0 * RELIEF).abs() < 1e-5, "{top}");
    }
}
