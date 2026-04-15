use bevy::prelude::*;

pub fn create_terrain_material(
    texture: Handle<Image>,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    materials.add(StandardMaterial {
        base_color_texture: Some(texture),
        unlit: true,
        ..default()
    })
}

pub fn create_datavent_material(
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.4, 0.0),
        emissive: LinearRgba::new(2.0, 0.8, 0.0, 1.0),
        unlit: false,
        ..default()
    })
}
