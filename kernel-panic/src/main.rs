mod game_setup;
mod interaction;
mod map_events;
mod map_loading;
mod paths;
mod rendering;
mod rng;
mod terrain;
mod ui;
mod units;

use bevy::asset::AssetPlugin;
use bevy::prelude::*;
use bevy::render::RenderPlugin;
// Pipelined rendering and explicit backend selection are native-only;
// the web build uses the platform's own WebGPU/WebGL path.
#[cfg(not(target_arch = "wasm32"))]
use bevy::render::pipelined_rendering::PipelinedRenderingPlugin;
#[cfg(not(target_arch = "wasm32"))]
use bevy::render::settings::{Backends, RenderCreation, WgpuSettings};
#[cfg(not(target_arch = "wasm32"))]
use bevy::window::MonitorSelection;
use bevy::window::{PresentMode, WindowMode, WindowResizeConstraints};

use interaction::InteractionPlugin;
use map_events::MapEventsPlugin;
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
    //
    // Web picks its own backend (WebGPU with WebGL2 fallback) — pinning
    // VULKAN|METAL there would select nothing.
    #[cfg(not(target_arch = "wasm32"))]
    let render_plugin = RenderPlugin {
        render_creation: RenderCreation::Automatic(WgpuSettings {
            backends: Some(Backends::VULKAN | Backends::METAL),
            ..default()
        }),
        ..default()
    };
    #[cfg(target_arch = "wasm32")]
    let render_plugin = RenderPlugin::default();

    // Native: resolve the assets path relative to the project root so
    // Bevy's AssetServer finds `kernel-panic/assets/...` regardless of
    // cwd. Web: fetch through the asset server instead (no fs) and skip
    // the processed-asset meta check since the site ships plain assets.
    #[allow(unused_mut)]
    let mut asset_plugin = AssetPlugin::default();
    #[cfg(not(target_arch = "wasm32"))]
    {
        let assets_dir = paths::from_project_root("kernel-panic/assets");
        asset_plugin.file_path = assets_dir.to_string_lossy().into_owned();
    }
    #[cfg(target_arch = "wasm32")]
    {
        asset_plugin.meta_check = bevy::asset::AssetMetaCheck::Never;
    }

    #[allow(unused_mut)]
    let mut default_plugins = DefaultPlugins.build().set(asset_plugin);
    // TODO(windows-resize): disable pipelined rendering so
    // the render world runs inline on the main thread. The
    // second thread is what deadlocks during
    // WM_ENTERSIZEMOVE — without it, resize is a sequence
    // of plain frames, slow but correct. (Native-only: the
    // plugin doesn't exist on wasm.)
    #[cfg(not(target_arch = "wasm32"))]
    let default_plugins = default_plugins.disable::<PipelinedRenderingPlugin>();

    App::new()
        .add_plugins(
            default_plugins
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
                        // TODO(windows-resize): launch directly into
                        // borderless fullscreen on the primary monitor.
                        // Prior attempts (windowed + Startup-maximize,
                        // with or without visible:false gymnastics) all
                        // triggered a live swapchain reconfigure at
                        // startup, which on Intel Iris Xe (Vulkan)
                        // either wedges the surface ("gray screen, no
                        // HUD") or flashes brief gray/white rectangles
                        // as the window transitions. Borderless-
                        // fullscreen sets winit's fullscreen attribute
                        // at window creation time, so the surface is
                        // born at monitor size and no reconfigure
                        // happens. Trade-off: no title bar / no
                        // built-in minimize-restore chrome. Acceptable
                        // for an RTS; swap back to `Windowed` once the
                        // upstream fix lands so the "windowed-maximize"
                        // UX returns.
                        // Native: borderless fullscreen on the primary
                        // monitor. Web: windowed + canvas-fill — there
                        // is no monitor selection on wasm, and the
                        // canvas is sized by the page.
                        mode: {
                            #[cfg(not(target_arch = "wasm32"))]
                            {
                                WindowMode::BorderlessFullscreen(MonitorSelection::Primary)
                            }
                            #[cfg(target_arch = "wasm32")]
                            {
                                WindowMode::Windowed
                            }
                        },
                        // Web: fill the Trunk page's canvas element.
                        #[cfg(target_arch = "wasm32")]
                        fit_canvas_to_parent: true,
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
            MapEventsPlugin,
        ))
        .init_state::<game_setup::AppState>()
        .init_resource::<game_setup::SkirmishConfig>()
        .init_resource::<game_setup::GameOverDismissed>()
        .insert_resource(game_setup::AiDifficulty(2))
        .add_message::<game_setup::RunGame>()
        .run();
}
