//! Dev tool: record a camera fly-over of an AI match to a video file,
//! then quit.
//!
//! Set `KP_RECORD=<file.mp4>` to enable (optionally `KP_DEMO_MAP=<map
//! stem>`, `KP_RECORD_WARMUP=<game seconds>` (default 150) and
//! `KP_RECORD_SECONDS=<video seconds>` (default 40); all read into
//! [`DevOptions`] at startup). The tool goes borderless-fullscreen,
//! starts the attract-mode all-AI skirmish as a spectated match, plays
//! the warmup at high speed so armies exist, then flies the camera
//! through the fights and homebases while every frame is read back at
//! window resolution and piped raw into `ffmpeg` (must be on `PATH`).
//!
//! The clock is stepped a fixed 1/[`FPS`] s per frame, so the video
//! plays at game speed however slowly the capture runs. Not compiled
//! for wasm (no disk, no processes).

use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::Duration;

use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use bevy::time::TimeUpdateStrategy;
use bevy::window::{MonitorSelection, PrimaryWindow, WindowMode};

use crate::game_setup::{AppState, DevOptions, GameSetup, demo_setup};
use crate::map_loading::MapCatalog;
use crate::rendering::camera::{MapBounds, RtsCamera, RtsCameraState};
use crate::units::combat::AimTarget;
use crate::units::components::{Homebase, UnitType};

/// Video frame rate; also the fixed per-frame clock step.
const FPS: u32 = 30;
/// Game clock multiplier while warming up.
const WARMUP_SPEED: f32 = 8.0;
/// Most points of interest the camera path visits.
const MAX_WAYPOINTS: usize = 6;
/// Top glide speed of the focus between points (elmos/s) and its
/// acceleration: it eases out of each point and into the next.
const GLIDE_SPEED: f32 = 100.0;
const GLIDE_ACCEL: f32 = 40.0;
/// Seconds the camera lingers on each point of interest.
const DWELL: f32 = 2.0;
/// Orbit speed (rad/s).
const ORBIT_SPEED: f32 = 0.04;

pub struct RecordPlugin;

impl Plugin for RecordPlugin {
    fn build(&self, app: &mut App) {
        let Some(dev) = app.world().get_resource::<DevOptions>() else {
            return;
        };
        let Some(out) = dev.record.clone() else {
            return;
        };
        let warmup = dev.record_warmup.unwrap_or(150.0);
        let seconds = dev.record_seconds.unwrap_or(40.0);
        app.insert_resource(Recorder {
            out,
            warmup,
            frames: (seconds * FPS as f32).round() as u32,
            step: Step::Boot,
            requested: 0,
            captured: 0,
            ffmpeg: None,
            failed: false,
            path: Vec::new(),
            glide: Glide::default(),
        })
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
            1.0 / FPS as f64,
        )))
        .add_systems(Startup, go_fullscreen)
        .add_systems(
            Update,
            run_recorder
                .after(crate::rendering::camera::camera_control)
                .before(crate::rendering::camera::camera_smoothing),
        );
    }
}

enum Step {
    /// Waiting for the map catalog, then entering the match.
    Boot,
    /// Waiting for the match's map to spawn.
    Loading,
    /// Fast-forwarding the match; the value is game seconds played.
    Warmup(f32),
    /// Capturing; the value is the frame index.
    Recording(u32),
    /// All frames requested; waiting for the last readbacks.
    Draining,
}

#[derive(Resource)]
struct Recorder {
    out: std::path::PathBuf,
    warmup: f32,
    frames: u32,
    step: Step,
    requested: u32,
    captured: u32,
    /// Spawned on the first captured frame, once its size and pixel
    /// format are known.
    ffmpeg: Option<(Child, ChildStdin)>,
    /// ffmpeg could not start or died; later frames are dropped.
    failed: bool,
    /// Camera focus waypoints, visited in order while time allows.
    path: Vec<Vec3>,
    glide: Glide,
}

/// Progress along [`Recorder::path`].
#[derive(Default)]
struct Glide {
    /// Waypoint being approached or dwelt on.
    target: usize,
    speed: f32,
    /// Seconds of dwell left at `target`; zero while travelling.
    dwell: f32,
}

fn go_fullscreen(mut window: Query<&mut Window, With<PrimaryWindow>>) {
    if let Ok(mut window) = window.single_mut() {
        window.mode = WindowMode::BorderlessFullscreen(MonitorSelection::Current);
    }
}

#[allow(clippy::too_many_arguments)]
fn run_recorder(
    mut rec: ResMut<Recorder>,
    dev: Res<DevOptions>,
    catalog: Option<Res<MapCatalog>>,
    state: Res<State<AppState>>,
    mut next_state: ResMut<NextState<AppState>>,
    bounds: Res<MapBounds>,
    bases: Query<&GlobalTransform, With<Homebase>>,
    fighters: Query<&GlobalTransform, (With<AimTarget>, With<UnitType>)>,
    units: Query<&GlobalTransform, With<UnitType>>,
    mut time: ResMut<Time<Virtual>>,
    mut cam: Query<&mut RtsCameraState, With<RtsCamera>>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
    let dt = time.delta_secs();
    match rec.step {
        Step::Boot => {
            if catalog.is_some() {
                commands.insert_resource::<GameSetup>(demo_setup(&dev));
                next_state.set(AppState::InGame);
                rec.step = Step::Loading;
            }
        }
        Step::Loading => {
            // The menu's demo world may already stand; wait for the
            // match's own map (it re-inserts `MapBounds`) in `InGame`.
            if *state.get() == AppState::InGame && bounds.is_changed() {
                info!("KP_RECORD: map up, warming up {}s of game time", rec.warmup);
                time.set_max_delta(Duration::from_secs_f32(WARMUP_SPEED / FPS as f32 * 1.5));
                time.set_relative_speed(WARMUP_SPEED);
                rec.step = Step::Warmup(0.0);
            }
        }
        Step::Warmup(played) => {
            let played = played + dt;
            rec.step = Step::Warmup(played);
            if played >= rec.warmup {
                time.set_relative_speed(1.0);
                rec.path = plan_path(&bases, &fighters, &units, &bounds);
                info!(
                    "KP_RECORD: recording {} frames through {} waypoints",
                    rec.frames,
                    rec.path.len()
                );
                if let Ok(mut state) = cam.single_mut() {
                    state.snap_to(rec.path[0], 1150.0);
                }
                rec.glide = Glide {
                    dwell: DWELL,
                    ..default()
                };
                rec.step = Step::Recording(0);
            }
        }
        Step::Recording(i) => {
            if let Ok(mut state) = cam.single_mut() {
                let secs = i as f32 / FPS as f32;
                let Recorder { path, glide, .. } = rec.as_mut();
                state.focus = glide.advance(path, state.focus, dt);
                state.yaw = 0.3 + ORBIT_SPEED * secs;
                state.pitch = 0.66 + 0.05 * (secs * 0.1).sin();
                state.distance = 1150.0 + 150.0 * (secs * 0.08).sin();
            }
            commands
                .spawn(Screenshot::primary_window())
                .observe(on_frame);
            rec.requested += 1;
            rec.step = if i + 1 >= rec.frames {
                Step::Draining
            } else {
                Step::Recording(i + 1)
            };
        }
        Step::Draining => {
            if rec.captured >= rec.requested {
                if let Some((mut child, stdin)) = rec.ffmpeg.take() {
                    drop(stdin);
                    match child.wait() {
                        Ok(status) if status.success() => {
                            info!("KP_RECORD: wrote {}", rec.out.display());
                        }
                        other => error!("KP_RECORD: ffmpeg failed: {other:?}"),
                    }
                }
                exit.write(AppExit::Success);
            }
        }
    }
}

/// Write one read-back frame into ffmpeg, starting it on the first.
fn on_frame(frame: On<ScreenshotCaptured>, mut rec: ResMut<Recorder>) {
    rec.captured += 1;
    if rec.failed {
        return;
    }
    if rec.ffmpeg.is_none() {
        let size = frame.image.texture_descriptor.size;
        let pix_fmt = match frame.image.texture_descriptor.format {
            TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb => "bgra",
            TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb => "rgba",
            other => {
                error!("KP_RECORD: unsupported frame format {other:?}");
                rec.failed = true;
                return;
            }
        };
        info!(
            "KP_RECORD: capturing {}x{} {pix_fmt} into {}",
            size.width,
            size.height,
            rec.out.display()
        );
        let spawned = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "rawvideo", "-pix_fmt", pix_fmt])
            .args(["-s", &format!("{}x{}", size.width, size.height)])
            .args(["-r", &FPS.to_string(), "-i", "-"])
            // yuv420p needs even dimensions; fractional scaling can
            // leave the fullscreen window one pixel odd.
            .args(["-vf", "crop=trunc(iw/2)*2:trunc(ih/2)*2:0:0"])
            .args(["-c:v", "libx264", "-preset", "medium", "-crf", "16"])
            .args(["-pix_fmt", "yuv420p", "-movflags", "+faststart"])
            .arg(&rec.out)
            .stdin(Stdio::piped())
            .spawn();
        match spawned {
            Ok(mut child) => {
                let stdin = child.stdin.take().expect("piped stdin");
                rec.ffmpeg = Some((child, stdin));
            }
            Err(error) => {
                error!("KP_RECORD: could not start ffmpeg: {error}");
                rec.failed = true;
                return;
            }
        }
    }
    if let Some((_, stdin)) = rec.ffmpeg.as_mut()
        && let Some(data) = frame.image.data.as_ref()
        && let Err(error) = stdin.write_all(data)
    {
        // ffmpeg died (its own error is on stderr); stop feeding it.
        error!("KP_RECORD: ffmpeg pipe: {error}");
        rec.failed = true;
    }
}

/// Waypoints for the fly-over: a spread of fights (farthest-point
/// picks), then homebases, ordered as a nearest-neighbour tour so the
/// camera never doubles back across the map.
fn plan_path(
    bases: &Query<&GlobalTransform, With<Homebase>>,
    fighters: &Query<&GlobalTransform, (With<AimTarget>, With<UnitType>)>,
    units: &Query<&GlobalTransform, With<UnitType>>,
    bounds: &MapBounds,
) -> Vec<Vec3> {
    let fights: Vec<Vec3> = fighters.iter().map(|g| g.translation()).collect();
    let pool: Vec<Vec3> = if fights.is_empty() {
        units.iter().map(|g| g.translation()).collect()
    } else {
        fights
    };
    let mut points = farthest_points(&pool, MAX_WAYPOINTS / 2);
    for base in bases.iter().map(|g| g.translation()) {
        if points.len() >= MAX_WAYPOINTS {
            break;
        }
        if points.iter().all(|p| p.xz().distance(base.xz()) > 400.0) {
            points.push(base);
        }
    }
    if points.is_empty() {
        points.push((bounds.min + bounds.max) * 0.5);
    }
    // Nearest-neighbour tour from the first pick.
    let mut tour = vec![points.swap_remove(0)];
    while !points.is_empty() {
        let last = *tour.last().unwrap();
        let (i, _) = points
            .iter()
            .enumerate()
            .min_by(|a, b| {
                a.1.distance_squared(last)
                    .total_cmp(&b.1.distance_squared(last))
            })
            .unwrap();
        tour.push(points.swap_remove(i));
    }
    tour
}

fn farthest_points(pool: &[Vec3], k: usize) -> Vec<Vec3> {
    let Some(&first) = pool.first() else {
        return Vec::new();
    };
    let mut picked = vec![first];
    while picked.len() < k {
        let far = pool.iter().copied().max_by(|a, b| {
            let da = picked
                .iter()
                .map(|p| p.distance_squared(*a))
                .fold(f32::MAX, f32::min);
            let db = picked
                .iter()
                .map(|p| p.distance_squared(*b))
                .fold(f32::MAX, f32::min);
            da.total_cmp(&db)
        });
        match far {
            Some(p) if picked.iter().all(|q| q.distance(p) > 300.0) => picked.push(p),
            _ => break,
        }
    }
    picked
}

impl Glide {
    /// Step the focus one frame: dwell on the current point, then glide
    /// to the next, accelerating out and braking in so the pan never
    /// jerks. Stays on the last point once the tour is done.
    fn advance(&mut self, path: &[Vec3], focus: Vec3, dt: f32) -> Vec3 {
        if self.dwell > 0.0 {
            self.dwell -= dt;
            if self.dwell <= 0.0 && self.target + 1 < path.len() {
                self.target += 1;
            }
            return focus;
        }
        let to = path[self.target] - focus;
        let dist = to.length();
        let brake = (2.0 * GLIDE_ACCEL * dist).sqrt();
        self.speed = (self.speed + GLIDE_ACCEL * dt).min(GLIDE_SPEED).min(brake);
        let step = self.speed * dt;
        if dist <= step.max(0.5) {
            self.speed = 0.0;
            self.dwell = DWELL;
            path[self.target]
        } else {
            focus + to / dist * step
        }
    }
}
