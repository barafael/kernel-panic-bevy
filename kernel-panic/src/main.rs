mod interaction;
mod map_loading;
mod paths;
mod rendering;
mod terrain;
mod ui;
mod units;

use bevy::asset::AssetPlugin;
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::pipelined_rendering::PipelinedRenderingPlugin;
use bevy::render::settings::{Backends, RenderCreation, WgpuSettings};
use bevy::window::{PresentMode, PrimaryWindow, WindowResizeConstraints};

use interaction::InteractionPlugin;
use map_loading::MapLoadingPlugin;
use rendering::RenderingPlugin;
use terrain::TerrainPlugin;
use ui::UiPlugin;
use units::UnitsPlugin;

fn main() {
    // TODO(windows-resize): four linked workarounds for the Bevy 0.18 +
    // Windows "freeze on resize" bug. Each is noted inline; they can
    // be reverted independently when the upstream fix lands.
    //
    // Symptom in the wild: grab the window frame, see a single small
    // resize, then the app locks until force-closed.
    //
    // Root cause stack (top-down):
    //   1. Win32 enters a modal message loop (WM_ENTERSIZEMOVE) and the
    //      main thread is stuck inside the OS's DispatchMessage.
    //   2. Bevy's pipelined renderer runs the render world on a second
    //      thread. When it tries to coordinate with the main thread
    //      (swapchain acquire, extract sync) it deadlocks against the
    //      modal loop.
    //   3. Bevy 0.18 ships wgpu ~24, whose DX12 swapchain reconfigure
    //      has a separate known hang during the same modal loop.
    //   4. wgpu panics if the swapchain is ever reconfigured at 0x0,
    //      which happens naturally during a fast drag-to-nothing and
    //      leaves HDR+Bloom's intermediate render targets in a bad
    //      state.
    //
    // See the Bevy issue tracker (`Platform-Windows` label) and the
    // gfx-rs/wgpu issue tracker (search: "DX12 resize hang",
    // "WM_ENTERSIZEMOVE", "pipelined rendering deadlock",
    // "surface reconfigure 0x0").

    // TODO(windows-resize): force Vulkan only. The previous attempt
    // listed DX12 as a fallback, which meant Vulkan-less systems
    // silently fell back into the bug we're trying to avoid. If Vulkan
    // isn't available here we want to fail loudly so we notice, not
    // quietly accept the broken path.
    let render_plugin = RenderPlugin {
        render_creation: RenderCreation::Automatic(WgpuSettings {
            backends: Some(Backends::VULKAN | Backends::METAL),
            ..default()
        }),
        ..default()
    };

    // Resolve the assets path relative to the project root so Bevy's
    // AssetServer finds `kernel-panic/assets/...` regardless of cwd.
    let assets_dir = paths::from_project_root("kernel-panic/assets");
    let asset_plugin = AssetPlugin {
        file_path: assets_dir.to_string_lossy().into_owned(),
        ..default()
    };

    App::new()
        .add_plugins(
            DefaultPlugins
                .build()
                .set(asset_plugin)
                // TODO(windows-resize): disable pipelined rendering so
                // the render world runs inline on the main thread. The
                // second thread is what deadlocks during
                // WM_ENTERSIZEMOVE — without it, resize is a sequence
                // of plain frames, slow but correct.
                .disable::<PipelinedRenderingPlugin>()
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "Kernel Panic".to_string(),
                        // TODO(windows-resize): Immediate is pinned
                        // explicitly instead of AutoNoVsync. AutoNoVsync
                        // would pick Mailbox where available, and Intel
                        // Vulkan Mailbox has its own resize-reconfigure
                        // quirks on this hardware. Restore AutoVsync
                        // once the winit modal-loop fix is in.
                        present_mode: PresentMode::Immediate,
                        // TODO(windows-resize): 320x240 floor keeps the
                        // swapchain from ever reconfiguring at 0x0
                        // during a fast drag-to-nothing, which panics
                        // wgpu. Remove once wgpu handles 0x0
                        // reconfigure gracefully.
                        resize_constraints: WindowResizeConstraints {
                            min_width: 320.0,
                            min_height: 240.0,
                            ..default()
                        },
                        ..default()
                    }),
                    ..default()
                })
                .set(render_plugin),
        )
        .add_plugins((
            RenderingPlugin,
            InteractionPlugin,
            UiPlugin,
            UnitsPlugin,
            TerrainPlugin,
            MapLoadingPlugin,
        ))
        .add_systems(Startup, maximize_primary_window)
        .run();
}

/// Launch filling the screen. Bevy has no init-time "maximized" field,
/// so we flip the flag once the window exists. Keeping it as a windowed
/// maximize (rather than borderless fullscreen) preserves the title bar
/// and the usual Windows minimize/restore affordances.
fn maximize_primary_window(mut windows: Query<&mut Window, With<PrimaryWindow>>) {
    if let Ok(mut window) = windows.single_mut() {
        window.set_maximized(true);
    }
}
