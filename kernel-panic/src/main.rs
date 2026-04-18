mod interaction;
mod map_loading;
mod rendering;
mod terrain;
mod ui;
mod units;

use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::settings::{Backends, RenderCreation, WgpuSettings};
use bevy::window::{PresentMode, WindowResizeConstraints};

use interaction::InteractionPlugin;
use map_loading::MapLoadingPlugin;
use rendering::RenderingPlugin;
use terrain::TerrainPlugin;
use ui::UiPlugin;
use units::UnitsPlugin;

fn main() {
    // Windows-specific workaround for the "freeze then crash on resize" bug:
    //
    // - Prefer Vulkan over DX12. Bevy 0.18's wgpu has a known DX12 swapchain
    //   reconfigure hang during the Win32 modal resize loop; every Windows
    //   GPU driver ships a working Vulkan runtime so we lose nothing.
    // - `AutoNoVsync` present mode stops vsync waits from piling up while
    //   Windows is in the WM_ENTERSIZEMOVE modal pump.
    // - Min size 320×240 keeps the swapchain from ever being reconfigured
    //   at 0×0 during a drag-to-nothing, which otherwise panics wgpu and
    //   drops HDR+Bloom's intermediate render targets in a bad state.
    let render_plugin = RenderPlugin {
        render_creation: RenderCreation::Automatic(WgpuSettings {
            backends: Some(Backends::VULKAN | Backends::METAL | Backends::DX12),
            ..default()
        }),
        ..default()
    };

    App::new()
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "Kernel Panic".to_string(),
                        present_mode: PresentMode::AutoNoVsync,
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
        .run();
}
