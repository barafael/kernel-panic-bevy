//! Minimap.
//!
//! At map load the [`setup_minimap`] entry point downsamples the
//! `ParsedMap.ground_texture` into a small RGBA image, spawns it as an
//! `ImageNode` in the corner, and inserts a [`MinimapState`] resource
//! holding the base pixels. Each frame the update system copies the
//! base pixels back into the image, then over-writes them with:
//!
//! 1. One 3×3 dot per spotted unit, faction-tinted.
//! 2. A four-corner viewport rectangle traced from the camera's screen
//!    corners projected onto the ground plane.
//!
//! Drawing pixel-perfect into the texture (rather than spawning Node
//! children for every dot) avoids per-frame UI tree churn for what is
//! a quintessentially raster overlay.

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::rendering::camera::RtsCamera;
use crate::units::components::Faction;
use crate::units::mechanics::cloak::Spotted;

use super::theme::PANEL_BORDER;

/// Minimap display size in logical pixels along the longer axis.
const MINIMAP_SIZE: f32 = 200.0;
/// Padding from the screen edge.
const MINIMAP_MARGIN: f32 = 8.0;
/// Refresh cadence in seconds (10 Hz).
const REFRESH_INTERVAL: f32 = 0.1;

pub struct MinimapPlugin;

impl Plugin for MinimapPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            update_minimap.run_if(resource_exists::<MinimapState>),
        );
    }
}

/// Marker for the minimap UI node.
#[derive(Component)]
struct MinimapNode;

/// Per-map minimap state.
#[derive(Resource)]
pub struct MinimapState {
    image_handle: Handle<Image>,
    width: u32,
    height: u32,
    /// World-space extent of the map in elmos.
    world_width: f32,
    world_depth: f32,
    /// Pristine downsampled terrain pixels — copied back over the image
    /// each refresh before drawing dots / viewport on top.
    base_pixels: Vec<u8>,
    timer: Timer,
}

/// Build the minimap image, spawn the UI node, and insert
/// [`MinimapState`]. Call once at map load with the map's ground texture
/// pixels (or `None` if the map has no ground texture).
pub fn setup_minimap(
    commands: &mut Commands,
    images: &mut Assets<Image>,
    ground_pixels: Option<&[u8]>,
    ground_width: usize,
    ground_height: usize,
    world_width: f32,
    world_depth: f32,
) {
    let aspect = world_width / world_depth;
    let (mm_w, mm_h) = if aspect >= 1.0 {
        (MINIMAP_SIZE as u32, (MINIMAP_SIZE / aspect) as u32)
    } else {
        ((MINIMAP_SIZE * aspect) as u32, MINIMAP_SIZE as u32)
    };

    let base_pixels = downsample_terrain(ground_pixels, ground_width, ground_height, mm_w, mm_h);

    let image = Image::new(
        Extent3d {
            width: mm_w,
            height: mm_h,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        base_pixels.clone(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    let image_handle = images.add(image);

    commands.spawn((
        MinimapNode,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(MINIMAP_MARGIN),
            top: Val::Px(MINIMAP_MARGIN),
            width: Val::Px(mm_w as f32),
            height: Val::Px(mm_h as f32),
            border: UiRect::all(Val::Px(2.0)),
            ..default()
        },
        BorderColor::all(PANEL_BORDER),
        BackgroundColor(Color::BLACK),
        ImageNode::new(image_handle.clone()),
    ));

    commands.insert_resource(MinimapState {
        image_handle,
        width: mm_w,
        height: mm_h,
        world_width,
        world_depth,
        base_pixels,
        timer: Timer::from_seconds(REFRESH_INTERVAL, TimerMode::Repeating),
    });
}

#[allow(clippy::type_complexity)]
fn update_minimap(
    time: Res<Time>,
    mut state: ResMut<MinimapState>,
    mut images: ResMut<Assets<Image>>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    unit_q: Query<(&Transform, &Faction), With<Spotted>>,
    windows: Query<&Window>,
) {
    state.timer.tick(time.delta());
    if !state.timer.just_finished() {
        return;
    }

    let Some(image) = images.get_mut(&state.image_handle) else {
        return;
    };
    let Some(pixels) = image.data.as_mut() else {
        return;
    };

    let mm_w = state.width as usize;
    let mm_h = state.height as usize;

    // Reset to base terrain.
    pixels.copy_from_slice(&state.base_pixels);

    // Unit dots.
    for (transform, faction) in &unit_q {
        let (r, g, b) = faction_rgb(*faction);
        let mx = ((transform.translation.x / state.world_width) * mm_w as f32) as i32;
        let mz = ((transform.translation.z / state.world_depth) * mm_h as f32) as i32;

        for dy in -1..=1 {
            for dx in -1..=1 {
                let px = mx + dx;
                let pz = mz + dy;
                if px >= 0 && px < mm_w as i32 && pz >= 0 && pz < mm_h as i32 {
                    let idx = (pz as usize * mm_w + px as usize) * 4;
                    pixels[idx] = r;
                    pixels[idx + 1] = g;
                    pixels[idx + 2] = b;
                    pixels[idx + 3] = 255;
                }
            }
        }
    }

    // Viewport rectangle: cast rays from the four screen corners onto
    // the ground plane (Y = 0) and connect the hit points.
    if let (Ok((camera, camera_global)), Ok(window)) = (camera_q.single(), windows.single()) {
        let screen_w = window.width();
        let screen_h = window.height();
        let corners = [
            Vec2::new(0.0, 0.0),
            Vec2::new(screen_w, 0.0),
            Vec2::new(screen_w, screen_h),
            Vec2::new(0.0, screen_h),
        ];

        let mut hits = [(0i32, 0i32); 4];
        let mut filled = 0;
        for screen_pos in &corners {
            if let Ok(ray) = camera.viewport_to_world(camera_global, *screen_pos)
                && let Some(world_point) = ray_ground_intersect(&ray)
            {
                let mx = (world_point.x / state.world_width * mm_w as f32) as i32;
                let mz = (world_point.z / state.world_depth * mm_h as f32) as i32;
                hits[filled] = (mx, mz);
                filled += 1;
            }
        }

        if filled == 4 {
            let frame = [255, 255, 255, 200];
            for i in 0..4 {
                let (x0, y0) = hits[i];
                let (x1, y1) = hits[(i + 1) % 4];
                draw_line(pixels, mm_w, mm_h, x0, y0, x1, y1, frame);
            }
        }
    }
}

fn ray_ground_intersect(ray: &Ray3d) -> Option<Vec3> {
    let origin = ray.origin;
    let dir = *ray.direction;
    if dir.y.abs() < 1e-6 {
        return None;
    }
    let t = -origin.y / dir.y;
    if t < 0.0 {
        return None;
    }
    // Cap distant intersections so a near-horizontal ray doesn't blow
    // out the int conversion further down.
    let t = t.min(50_000.0);
    Some(origin + dir * t)
}

#[allow(clippy::too_many_arguments)]
fn draw_line(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    color: [u8; 4],
) {
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let mut x = x0;
    let mut y = y0;

    loop {
        if x >= 0 && x < width as i32 && y >= 0 && y < height as i32 {
            let idx = (y as usize * width + x as usize) * 4;
            pixels[idx..idx + 4].copy_from_slice(&color);
        }
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

fn faction_rgb(faction: Faction) -> (u8, u8, u8) {
    let srgba = Srgba::from(faction.color());
    (
        (srgba.red * 255.0) as u8,
        (srgba.green * 255.0) as u8,
        (srgba.blue * 255.0) as u8,
    )
}

fn downsample_terrain(
    source: Option<&[u8]>,
    src_w: usize,
    src_h: usize,
    dst_w: u32,
    dst_h: u32,
) -> Vec<u8> {
    let dst_w = dst_w as usize;
    let dst_h = dst_h as usize;
    let mut result = vec![20u8; dst_w * dst_h * 4];

    let Some(source) = source else {
        // Fill alpha for the dark default so it isn't transparent.
        for chunk in result.chunks_exact_mut(4) {
            chunk[3] = 255;
        }
        return result;
    };

    if src_w == 0 || src_h == 0 {
        return result;
    }

    for dst_y in 0..dst_h {
        for dst_x in 0..dst_w {
            let src_x = (dst_x * src_w / dst_w).min(src_w - 1);
            let src_y = (dst_y * src_h / dst_h).min(src_h - 1);
            let src_idx = (src_y * src_w + src_x) * 4;
            let dst_idx = (dst_y * dst_w + dst_x) * 4;
            if src_idx + 3 < source.len() {
                result[dst_idx] = source[src_idx];
                result[dst_idx + 1] = source[src_idx + 1];
                result[dst_idx + 2] = source[src_idx + 2];
                result[dst_idx + 3] = 255;
            }
        }
    }

    result
}
