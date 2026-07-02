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
    /// Pristine downsampled terrain pixels — copied back into the
    /// touched-pixel set each refresh before drawing the new frame's
    /// dots / viewport on top.
    base_pixels: Vec<u8>,
    timer: Timer,
    /// Byte offsets of every pixel touched by the previous refresh's
    /// dots + viewport. On the next refresh these get restored from
    /// `base_pixels` first, then the new frame's draw calls populate
    /// the list afresh. Replaces a per-tick O(W·H) memcpy with
    /// O(touched_pixels) — at ~50 visible units + viewport rect that
    /// is roughly 30× cheaper. The `Vec`'s capacity is reused across
    /// refreshes, so steady state allocates nothing.
    dirty_byte_indices: Vec<usize>,
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
        dirty_byte_indices: Vec::new(),
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

    // Disjoint-field borrow: `base_pixels` (read) and
    // `dirty_byte_indices` (mutate) live on the same resource.
    let state = state.as_mut();
    let mm_w = state.width as usize;
    let mm_h = state.height as usize;

    // Restore *only* the pixels touched last frame. Replaces the
    // previous full-image memcpy and is the whole point of the dirty
    // tracking. Pixels not in the list are unchanged from the last
    // refresh — but everything we ever paint over is in the list, so
    // they're already at base-terrain colour.
    for &idx in &state.dirty_byte_indices {
        pixels[idx..idx + 4].copy_from_slice(&state.base_pixels[idx..idx + 4]);
    }
    state.dirty_byte_indices.clear();

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
                    write_dirty_pixel(pixels, &mut state.dirty_byte_indices, idx, [r, g, b, 255]);
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
                draw_line(
                    pixels,
                    &mut state.dirty_byte_indices,
                    mm_w,
                    mm_h,
                    x0,
                    y0,
                    x1,
                    y1,
                    frame,
                );
            }
        }
    }
}

/// Write `rgba` to `pixels[idx..idx+4]` and record the byte offset for
/// next refresh's restore pass. Inlined into both the dot loop and
/// `draw_line` so every pixel mutation participates in dirty tracking.
fn write_dirty_pixel(pixels: &mut [u8], dirty: &mut Vec<usize>, idx: usize, rgba: [u8; 4]) {
    pixels[idx..idx + 4].copy_from_slice(&rgba);
    dirty.push(idx);
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
    dirty: &mut Vec<usize>,
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
            write_dirty_pixel(pixels, dirty, idx, color);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_dirty_pixel_records_byte_offset() {
        let mut pixels = vec![0u8; 16];
        let mut dirty = Vec::new();
        write_dirty_pixel(&mut pixels, &mut dirty, 4, [10, 20, 30, 40]);
        assert_eq!(&pixels[4..8], &[10, 20, 30, 40]);
        assert_eq!(dirty, vec![4]);
    }

    /// Restore-from-base must reset every pixel the previous frame
    /// painted, leaving untouched pixels alone. Mirrors the
    /// `update_minimap` restore loop.
    #[test]
    fn restore_loop_resets_only_dirty_pixels() {
        // 2x2 image, RGBA = 16 bytes. Base is all 1s.
        let base = vec![1u8; 16];
        let mut pixels = base.clone();

        // "Last frame" painted pixel (0,0) and pixel (1,1) red.
        let mut dirty = Vec::new();
        write_dirty_pixel(&mut pixels, &mut dirty, 0, [255, 0, 0, 255]);
        write_dirty_pixel(&mut pixels, &mut dirty, 12, [255, 0, 0, 255]);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[1, 1, 1, 1], "pixel (1,0) untouched");

        // Now restore — the two dirty pixels return to base, others
        // unchanged. (And dirty is cleared so the next frame starts
        // fresh.)
        for &idx in &dirty {
            pixels[idx..idx + 4].copy_from_slice(&base[idx..idx + 4]);
        }
        dirty.clear();

        assert_eq!(pixels, base, "all pixels back to base after restore");
        assert!(dirty.is_empty());
    }

    /// `draw_line` must record every painted pixel into the dirty
    /// list so the next refresh's restore covers it.
    #[test]
    fn draw_line_populates_dirty_list() {
        let mut pixels = vec![0u8; 4 * 10 * 10];
        let mut dirty = Vec::new();
        // Horizontal line from (0,0) to (4,0): 5 pixels.
        draw_line(&mut pixels, &mut dirty, 10, 10, 0, 0, 4, 0, [9, 9, 9, 9]);
        assert_eq!(dirty.len(), 5);
        for &idx in &dirty {
            assert_eq!(&pixels[idx..idx + 4], &[9, 9, 9, 9]);
        }
    }

    /// Out-of-bounds pixel writes must NOT pollute the dirty list —
    /// otherwise the restore loop would index out of range next
    /// refresh.
    #[test]
    fn draw_line_clipped_writes_skip_dirty_list() {
        let mut pixels = vec![0u8; 4 * 4 * 4];
        let mut dirty = Vec::new();
        // Line crossing partly outside a 4x4 image.
        draw_line(&mut pixels, &mut dirty, 4, 4, -2, 1, 5, 1, [7, 7, 7, 7]);
        for &idx in &dirty {
            // every recorded byte offset must address a valid 4-byte slot.
            assert!(idx + 3 < pixels.len(), "dirty idx {idx} out of bounds");
        }
    }
}
