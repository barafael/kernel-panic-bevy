pub mod camera;

use bevy::prelude::*;

use crate::rendering::camera::{CameraSettings, MapBounds, camera_control, spawn_camera};

pub struct RenderingPlugin;

impl Plugin for RenderingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CameraSettings>()
            .init_resource::<MapBounds>()
            .add_systems(Startup, spawn_camera)
            .add_systems(Update, camera_control);
    }
}
