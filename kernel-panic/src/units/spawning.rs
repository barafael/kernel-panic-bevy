use bevy::prelude::*;

use spring_map::map_types::{ParsedMap, SQUARE_SIZE};
use spring_map::smd_parser::MapInfo;

use super::components::{Faction, Health, Homebase, SelectionVolume, TeamId, UnitType};
use super::definitions::{UnitKind, stats};
use super::meshes::{S3OModelCache, unit_material, unit_mesh, unit_radius};
use super::production::default_production;
use crate::MapEntity;

/// Assign factions to teams in a round-robin pattern.
const FACTION_ORDER: [Faction; 3] = [Faction::System, Faction::Hacker, Faction::Network];

/// Shared invisible material for all selection volumes.
#[derive(Resource, Clone)]
pub struct SelectionVolumeMaterial(pub Handle<StandardMaterial>);

/// Spawn a homebase for each start position defined in the map info.
pub fn spawn_homebases(
    parsed: &ParsedMap,
    map_info: &MapInfo,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
) {
    let invisible_mat = get_or_create_invisible_material(commands, materials);
    let square_size = SQUARE_SIZE as f32;

    for start_pos in &map_info.start_positions {
        let faction = FACTION_ORDER[start_pos.team as usize % FACTION_ORDER.len()];
        let kind = faction.homebase();

        let heightmap_w = parsed.header.heightmap_width();
        let heightmap_x = (start_pos.x / square_size).clamp(0.0, (heightmap_w - 1) as f32) as usize;
        let heightmap_z = (start_pos.z / square_size)
            .clamp(0.0, (parsed.header.heightmap_height() - 1) as f32)
            as usize;
        let height = parsed.heights[heightmap_z * heightmap_w + heightmap_x];
        let position = Vec3::new(start_pos.x, height, start_pos.z);

        spawn_unit(
            kind,
            faction,
            start_pos.team as u8,
            position,
            commands,
            meshes,
            materials,
            images,
            model_cache,
            &invisible_mat,
        );
    }

    info!("Spawned {} homebases", map_info.start_positions.len());
}

/// Spawn a single unit at the given world position.
pub fn spawn_unit(
    kind: UnitKind,
    faction: Faction,
    team: u8,
    position: Vec3,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    invisible_mat: &SelectionVolumeMaterial,
) {
    let unit_stats = stats(kind);
    let mesh = unit_mesh(kind, meshes, model_cache);
    let material = unit_material(kind, faction, materials, images, model_cache);
    let radius = unit_radius(kind, model_cache);

    let selection_sphere = meshes.add(Sphere::new(radius).mesh().ico(3).unwrap());

    let mut entity_commands = commands.spawn((
        MapEntity,
        UnitType(kind),
        faction,
        TeamId(team),
        Health::full(unit_stats.max_health),
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(position),
    ));

    if let Some(producer) = default_production(kind) {
        entity_commands.insert(producer);
    }

    if matches!(
        kind,
        UnitKind::Kernel | UnitKind::Hole | UnitKind::Connection
    ) {
        entity_commands.insert(Homebase);
    }

    entity_commands.with_child((
        SelectionVolume,
        Mesh3d(selection_sphere),
        MeshMaterial3d(invisible_mat.0.clone()),
        Transform::from_xyz(0.0, radius * 0.5, 0.0),
    ));
}

fn get_or_create_invisible_material(
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
) -> SelectionVolumeMaterial {
    let mat = SelectionVolumeMaterial(materials.add(StandardMaterial {
        base_color: Color::srgba(0.0, 0.0, 0.0, 0.0),
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        ..default()
    }));
    commands.insert_resource(mat.clone());
    mat
}
