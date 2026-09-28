//! Player render settings: multisampling, bloom, vsync and the window
//! mode, changed at runtime from the menu's Settings page and kept
//! between runs (a small key=value file natively, `localStorage` on the
//! web).
//!
//! [`RenderSettings`] is the single source of truth. The window is
//! created from it in `main`, the camera is spawned from it, and
//! [`apply_render_settings`] pushes every later change to the live
//! camera and window — so a click on the page takes effect on the next
//! frame, no restart.

use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use bevy::window::{MonitorSelection, PresentMode, PrimaryWindow, WindowMode, WindowResolution};

use crate::game_setup::DevOptions;
use crate::rendering::camera::RtsCamera;

/// The bloom look of the game (`Bloom::intensity`).
const BLOOM_INTENSITY: f32 = 0.15;

#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderSettings {
    /// Multisample count: 1 (off), 2, 4 or 8 — one of [`MSAA_CHOICES`].
    pub msaa: u8,
    pub bloom: bool,
    /// Vsynced presentation (`AutoVsync`); off is the platform's
    /// lowest-latency mode.
    pub vsync: bool,
    /// Borderless fullscreen on the primary monitor, else a window of
    /// `window_size`.
    pub fullscreen: bool,
    pub window_size: (u32, u32),
}

/// The sample counts the Settings page offers. WebGL2 knows only 1 and
/// 4; 8 is left out because not every desktop GPU supports it on an
/// HDR target, and wgpu panics rather than falls back.
#[cfg(target_arch = "wasm32")]
pub const MSAA_CHOICES: &[u8] = &[1, 4];
#[cfg(not(target_arch = "wasm32"))]
pub const MSAA_CHOICES: &[u8] = &[1, 2, 4];

/// Window sizes the Settings page offers for windowed mode.
pub const WINDOW_SIZES: &[(u32, u32)] = &[(1280, 720), (1600, 900), (1920, 1080), (2560, 1440)];

impl Default for RenderSettings {
    /// Native: Bevy's 4× MSAA, bloom, vsync, borderless fullscreen. Web:
    /// HDR + bloom at 4× MSAA is the dominant frame cost on the usual
    /// integrated GPU (the HDR target is rendered at four samples and
    /// resolved every frame), so multisampling starts off there; the
    /// canvas is sized by the page, so no fullscreen.
    fn default() -> Self {
        Self {
            msaa: if cfg!(target_arch = "wasm32") { 1 } else { 4 },
            bloom: true,
            vsync: true,
            fullscreen: !cfg!(target_arch = "wasm32"),
            window_size: (1600, 900),
        }
    }
}

impl RenderSettings {
    /// The saved settings (or the defaults), with the `KP_MSAA`,
    /// `KP_BLOOM` and `KP_WINDOW` dev switches on top — those are for
    /// A/B runs and must win over whatever was saved last time.
    pub fn startup(dev: &DevOptions) -> Self {
        let mut s = load().unwrap_or_default();
        if let Some(n) = dev.msaa {
            s.msaa = if n <= 1 { 1 } else { n };
        }
        if let Some(b) = dev.bloom {
            s.bloom = b;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(size) = dev.window {
            s.fullscreen = false;
            s.window_size = size;
        }
        s
    }

    pub fn msaa(&self) -> Msaa {
        match self.msaa {
            2 => Msaa::Sample2,
            4 => Msaa::Sample4,
            8 => Msaa::Sample8,
            _ => Msaa::Off,
        }
    }

    pub fn bloom(&self) -> Option<Bloom> {
        self.bloom.then(|| Bloom {
            intensity: BLOOM_INTENSITY,
            ..default()
        })
    }

    /// Off Windows, no-vsync is `AutoNoVsync`. On Windows it is pinned
    /// to `Immediate`: `AutoNoVsync` would pick Mailbox where available,
    /// and Intel Vulkan Mailbox has its own resize-reconfigure quirks
    /// there (see the `windows-resize` notes in `main`).
    pub fn present_mode(&self) -> PresentMode {
        if self.vsync {
            PresentMode::AutoVsync
        } else if cfg!(target_os = "windows") {
            PresentMode::Immediate
        } else {
            PresentMode::AutoNoVsync
        }
    }

    /// Native: borderless fullscreen sets winit's fullscreen attribute
    /// at window creation, so the surface is born at monitor size and
    /// never reconfigures at startup (the `windows-resize` notes in
    /// `main`). Web: there is no monitor selection on wasm; the canvas
    /// fills the page.
    pub fn window_mode(&self) -> WindowMode {
        if self.fullscreen && !cfg!(target_arch = "wasm32") {
            WindowMode::BorderlessFullscreen(MonitorSelection::Primary)
        } else {
            WindowMode::Windowed
        }
    }

    pub fn resolution(&self) -> WindowResolution {
        if self.fullscreen || cfg!(target_arch = "wasm32") {
            WindowResolution::default()
        } else {
            WindowResolution::new(self.window_size.0, self.window_size.1)
        }
    }

    fn to_text(self) -> String {
        format!(
            "msaa={}\nbloom={}\nvsync={}\nfullscreen={}\nwindow={}x{}\n",
            self.msaa,
            self.bloom as u8,
            self.vsync as u8,
            self.fullscreen as u8,
            self.window_size.0,
            self.window_size.1
        )
    }

    /// Parse the `key=value` lines of [`Self::to_text`]; unknown keys and
    /// unparsable values keep the default, so an old or hand-edited file
    /// never fails.
    fn from_text(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "msaa" => {
                    if let Ok(n) = value.parse::<u8>()
                        && MSAA_CHOICES.contains(&n)
                    {
                        s.msaa = n;
                    }
                }
                "bloom" => s.bloom = value != "0",
                "vsync" => s.vsync = value != "0",
                "fullscreen" => s.fullscreen = value != "0",
                "window" => {
                    if let Some((w, h)) = value.split_once('x')
                        && let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>())
                        && w >= 320
                        && h >= 240
                    {
                        s.window_size = (w, h);
                    }
                }
                _ => {}
            }
        }
        s
    }
}

/// Push a changed [`RenderSettings`] to the live camera and window.
/// Runs on the frame after a click; the first (startup) pass only
/// records what `main` and `spawn_camera` already built from the same
/// resource. Every later change is also saved.
pub fn apply_render_settings(
    settings: Res<RenderSettings>,
    mut cameras: Query<(Entity, &mut Msaa, Has<Bloom>), With<RtsCamera>>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    mut commands: Commands,
    mut applied: Local<Option<RenderSettings>>,
) {
    if *applied == Some(*settings) {
        return;
    }
    let Ok((camera, mut msaa, has_bloom)) = cameras.single_mut() else {
        return;
    };
    let first = applied.is_none();
    *applied = Some(*settings);
    if first {
        return;
    }

    if *msaa != settings.msaa() {
        *msaa = settings.msaa();
    }
    match (settings.bloom(), has_bloom) {
        (Some(bloom), false) => {
            commands.entity(camera).insert(bloom);
        }
        (None, true) => {
            commands.entity(camera).remove::<Bloom>();
        }
        _ => {}
    }

    if let Ok(mut window) = windows.single_mut() {
        // Only touch what changed: each write reconfigures the
        // swapchain, and a windowed → windowed resolution write while
        // fullscreen would fight the monitor size.
        let mode = settings.window_mode();
        if window.mode != mode {
            window.mode = mode;
        }
        if !settings.fullscreen && !cfg!(target_arch = "wasm32") {
            let (w, h) = settings.window_size;
            if window.resolution.size() != Vec2::new(w as f32, h as f32) {
                window.resolution.set(w as f32, h as f32);
            }
        }
        let present_mode = settings.present_mode();
        if window.present_mode != present_mode {
            window.present_mode = present_mode;
        }
    }

    save(*settings);
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
fn settings_path() -> Option<std::path::PathBuf> {
    use std::env::var_os;
    use std::path::PathBuf;
    let base = if cfg!(target_os = "windows") {
        var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(base.join("kernel-panic").join("settings.txt"))
}

#[cfg(not(target_arch = "wasm32"))]
fn load() -> Option<RenderSettings> {
    let text = std::fs::read_to_string(settings_path()?).ok()?;
    Some(RenderSettings::from_text(&text))
}

#[cfg(not(target_arch = "wasm32"))]
fn save(settings: RenderSettings) {
    let Some(path) = settings_path() else {
        return;
    };
    let written = path
        .parent()
        .map(std::fs::create_dir_all)
        .unwrap_or(Ok(()))
        .and_then(|_| std::fs::write(&path, settings.to_text()));
    if let Err(e) = written {
        warn!("could not save settings to {}: {e}", path.display());
    }
}

#[cfg(target_arch = "wasm32")]
const STORAGE_KEY: &str = "kernel-panic.settings";

#[cfg(target_arch = "wasm32")]
fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

#[cfg(target_arch = "wasm32")]
fn load() -> Option<RenderSettings> {
    let text = local_storage()?.get_item(STORAGE_KEY).ok()??;
    Some(RenderSettings::from_text(&text))
}

#[cfg(target_arch = "wasm32")]
fn save(settings: RenderSettings) {
    if let Some(storage) = local_storage()
        && storage.set_item(STORAGE_KEY, &settings.to_text()).is_err()
    {
        warn!("could not save settings to localStorage");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trip() {
        let s = RenderSettings {
            msaa: 2,
            bloom: false,
            vsync: false,
            fullscreen: false,
            window_size: (1920, 1080),
        };
        assert_eq!(RenderSettings::from_text(&s.to_text()), s);
    }

    #[test]
    fn bad_values_keep_defaults() {
        let d = RenderSettings::default();
        let s = RenderSettings::from_text("msaa=3\nwindow=10x10\nnonsense\nbloom=0\n");
        assert_eq!(s.msaa, d.msaa);
        assert_eq!(s.window_size, d.window_size);
        assert!(!s.bloom);
    }
}
