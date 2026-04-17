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
