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
    // TODO(windows-resize): three linked workarounds for the Bevy 0.18 +
    // Windows "freeze then crash on resize" bug. Each can be reverted
    // independently when the upstream fix lands; they're grouped here so
    // a future reader understands why the plain
    // `DefaultPlugins + Window { title, ..default() }` we want isn't
    // enough today.
    //
    // Symptom in the wild: grab the window frame, drag — the app locks
    // for the duration of the drag, and often wgpu panics with either a
    // surface-reconfigure error or a device-lost when the drag ends.
    //
    // See the Bevy issue tracker (`Platform-Windows` label) and the
    // gfx-rs/wgpu issue tracker (search: "DX12 resize hang",
    // "WM_ENTERSIZEMOVE", "surface reconfigure 0x0") for the upstream
    // threads — the project's Cargo.lock pins Bevy 0.18.1 which bundles
    // wgpu ~24 and therefore predates the fixes.

    // TODO(windows-resize): prefer Vulkan over DX12 to sidestep the
    // known DX12 swapchain reconfigure hang during the Win32 modal
    // resize loop (WM_ENTERSIZEMOVE). Vulkan is available on every
    // modern Windows GPU driver, so we lose nothing by preferring it.
    // Remove once Bevy ships a wgpu with the DX12 fix and we've
    // re-tested resize on Intel / AMD / NVIDIA.
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
                        // TODO(windows-resize): AutoNoVsync stops the
                        // vsync queue from piling up while Windows is
                        // inside WM_ENTERSIZEMOVE. Restore AutoVsync
                        // once the DX12 / winit modal-loop fix is in.
                        present_mode: PresentMode::AutoNoVsync,
                        // TODO(windows-resize): 320x240 floor keeps the
                        // swapchain from ever reconfiguring at 0x0
                        // during a fast drag-to-nothing, which panics
                        // wgpu and drops HDR+Bloom's intermediate render
                        // targets in a bad state. wgpu's 0-sized-surface
                        // behaviour is the root cause; remove this once
                        // wgpu handles 0x0 reconfigure gracefully.
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
