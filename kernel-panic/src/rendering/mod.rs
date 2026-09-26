pub mod camera;

use bevy::prelude::*;

use crate::game_setup::AppState;
use crate::rendering::camera::{
    CameraSettings, MapBounds, camera_control, camera_smoothing, spawn_camera,
};

pub struct RenderingPlugin;

impl Plugin for RenderingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CameraSettings>()
            .init_resource::<MapBounds>()
            .add_systems(Startup, spawn_camera)
            .add_systems(
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
