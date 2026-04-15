use bevy::prelude::*;

use crate::units::components::Faction;

/// Minimap display size in logical pixels.
const MINIMAP_SIZE: f32 = 200.0;
/// Padding from screen edge.
const MINIMAP_MARGIN: f32 = 12.0;
/// How often to refresh the minimap overlay (seconds).
const REFRESH_INTERVAL: f32 = 0.1;

/// Resource holding the generated minimap texture data.
#[derive(Resource)]
pub struct MinimapState {
    /// Handle to the minimap image asset (updated each frame).
    pub image_handle: Handle<Image>,
    /// Pixel width of the minimap texture.
    pub width: u32,
    /// Pixel height of the minimap texture.
    pub height: u32,
    /// World-space bounds of the map.
    pub world_width: f32,
    pub world_depth: f32,
    /// Base terrain pixels (copied from ground texture, downscaled).
    pub base_pixels: Vec<u8>,
    /// Timer for refresh throttling.
    pub timer: Timer,
}

/// Marker for the minimap UI node.
#[derive(Component)]
struct MinimapNode;

pub struct MinimapPlugin;

impl Plugin for MinimapPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            update_minimap.run_if(resource_exists::<MinimapState>),
        );
    }
}

/// Create the minimap resources and UI node. Called from main.rs after map load.
pub fn setup_minimap(
    commands: &mut Commands,
    images: &mut Assets<Image>,
    ground_pixels: Option<&[u8]>,
    ground_width: usize,
    ground_height: usize,
    world_width: f32,
    world_depth: f32,
) {
    // Determine minimap pixel dimensions maintaining aspect ratio.
    let aspect = world_width / world_depth;
    let (mm_w, mm_h) = if aspect >= 1.0 {
        (MINIMAP_SIZE as u32, (MINIMAP_SIZE / aspect) as u32)
    } else {
        ((MINIMAP_SIZE * aspect) as u32, MINIMAP_SIZE as u32)
    };

    // Downsample the ground texture to minimap resolution.
    let base_pixels = downsample_terrain(ground_pixels, ground_width, ground_height, mm_w, mm_h);

    // Create the image asset.
    let image = Image::new(
        bevy::render::render_resource::Extent3d {
            width: mm_w,
            height: mm_h,
            depth_or_array_layers: 1,
        },
        bevy::render::render_resource::TextureDimension::D2,
        base_pixels.clone(),
        bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
        bevy::asset::RenderAssetUsages::RENDER_WORLD | bevy::asset::RenderAssetUsages::MAIN_WORLD,
    );

    let image_handle = images.add(image);

    // Spawn UI node in bottom-right corner.
    commands.spawn((
        MinimapNode,
        crate::MapEntity,
        Node {
            position_type: PositionType::Absolute,
            right: Val::Px(MINIMAP_MARGIN),
            bottom: Val::Px(MINIMAP_MARGIN),
            width: Val::Px(mm_w as f32),
            height: Val::Px(mm_h as f32),
            border: UiRect::all(Val::Px(2.0)),
            ..default()
        },
        BorderColor::all(Color::linear_rgb(0.0, 0.6, 0.0)),
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

/// Per-frame update: overlay unit dots and viewport rectangle on the minimap.
fn update_minimap(
    time: Res<Time>,
    mut state: ResMut<MinimapState>,
    mut images: ResMut<Assets<Image>>,
    camera_query: Query<(&Camera, &GlobalTransform), With<crate::rendering::camera::RtsCamera>>,
    unit_query: Query<(&Transform, &Faction)>,
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

    // Draw unit dots.
    for (transform, faction) in &unit_query {
        let (r, g, b) = faction_rgb(faction);
        let mx = ((transform.translation.x / state.world_width) * mm_w as f32) as i32;
        let mz = ((transform.translation.z / state.world_depth) * mm_h as f32) as i32;

        // Draw a 3x3 dot.
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

    // Draw viewport by casting rays from the four screen corners to the
    // ground plane (Y = avg terrain height) and connecting the hit points.
    if let (Ok((camera, camera_global)), Ok(window)) = (camera_query.single(), windows.single()) {
        let screen_w = window.width();
        let screen_h = window.height();

        let screen_corners = [
            Vec2::new(0.0, 0.0),           // top-left
            Vec2::new(screen_w, 0.0),      // top-right
            Vec2::new(screen_w, screen_h), // bottom-right
            Vec2::new(0.0, screen_h),      // bottom-left
        ];

        let mut ground_hits: Vec<(i32, i32)> = Vec::new();

        for screen_pos in &screen_corners {
            if let Ok(ray) = camera.viewport_to_world(camera_global, *screen_pos) {
                // Intersect ray with the ground plane Y = focus_y.
                // Ray: P = origin + t * direction
                // Solve for t where P.y = ground_y
                if let Some(world_point) = ray_ground_intersect(&ray) {
                    let mx = (world_point.x / state.world_width * mm_w as f32) as i32;
                    let mz = (world_point.z / state.world_depth * mm_h as f32) as i32;
                    ground_hits.push((mx, mz));
                }
            }
        }

        if ground_hits.len() == 4 {
            let white = [255, 255, 255, 200];
            for i in 0..4 {
                let (x0, y0) = ground_hits[i];
                let (x1, y1) = ground_hits[(i + 1) % 4];
                draw_line(pixels, mm_w, mm_h, x0, y0, x1, y1, white);
            }
        }
    }
}

/// Intersect a ray with the ground plane (Y = 0).
/// Returns the world-space hit point, or `None` if the ray is parallel or pointing away.
fn ray_ground_intersect(ray: &Ray3d) -> Option<Vec3> {
    let origin = ray.origin;
    let dir = *ray.direction;

    // Ground plane: Y = 0. Solve origin.y + t * dir.y = 0.
    if dir.y.abs() < 1e-6 {
        return None; // Ray parallel to ground.
    }

    let t = -origin.y / dir.y;
    if t < 0.0 {
        return None; // Hit is behind camera.
    }

    // Clamp t to avoid extremely distant intersections for near-horizontal rays.
    let t = t.min(50000.0);

    Some(origin + dir * t)
}

/// Bresenham line drawing between two points.
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

fn faction_rgb(faction: &Faction) -> (u8, u8, u8) {
    match faction {
        Faction::System => (0, 255, 80),    // green
        Faction::Hacker => (255, 50, 50),   // red
        Faction::Network => (50, 130, 255), // blue
    }
}

/// Downsample a large ground texture to minimap resolution using box filtering.
fn downsample_terrain(
    source: Option<&[u8]>,
    src_w: usize,
    src_h: usize,
    dst_w: u32,
    dst_h: u32,
) -> Vec<u8> {
    let dst_w = dst_w as usize;
    let dst_h = dst_h as usize;
    let mut result = vec![20u8; dst_w * dst_h * 4]; // dark default

    let Some(source) = source else {
        // Fill alpha channel.
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
