pub mod camera;
pub mod interpolation;
pub mod prewarm;
pub mod settings;

use bevy::prelude::*;

use crate::game_setup::AppState;
use crate::rendering::camera::{
    CameraSettings, MapBounds, camera_control, camera_smoothing, spawn_camera,
};

pub struct RenderingPlugin;

impl Plugin for RenderingPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            interpolation::SimInterpolationPlugin,
            prewarm::PrewarmPlugin,
        ))
        .init_resource::<CameraSettings>()
        .init_resource::<MapBounds>()
        .init_resource::<settings::MsaaSupport>()
        .add_systems(
            Startup,
            (settings::probe_msaa_support, spawn_camera).chain(),
        )
        .add_systems(Update, settings::apply_render_settings);
        // F11 flips the mode the same frame it is applied and saved.
        #[cfg(not(target_arch = "wasm32"))]
        app.add_systems(
            Update,
            settings::toggle_fullscreen_key.before(settings::apply_render_settings),
        );
        app.add_systems(
            Update,
            (
                // Player camera input only in a match: in the menu the
                // arrow keys navigate buttons and the attract-mode
                // camera owns the view.
                camera_control.run_if(in_state(AppState::InGame)),
                camera_smoothing,
            )
                .chain(),
        );
    }
}
