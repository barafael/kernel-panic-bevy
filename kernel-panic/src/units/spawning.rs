use bevy::prelude::*;

use spring_map::map_types::{ParsedMap, SQUARE_SIZE};
use spring_map::smd_parser::MapInfo;

use super::components::{Faction, Health, TeamId, UnitType};
use super::definitions::{UnitKind, stats};
use super::meshes::{unit_material, unit_mesh};
use crate::MapEntity;

/// Assign factions to teams in a round-robin pattern.
const FACTION_ORDER: [Faction; 3] = [Faction::System, Faction::Hacker, Faction::Network];

/// Spawn a homebase for each start position defined in the map info.
pub fn spawn_homebases(
    parsed: &ParsedMap,
    map_info: &MapInfo,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) {
    let square_size = SQUARE_SIZE as f32;

    for start_pos in &map_info.start_positions {
        let faction = FACTION_ORDER[start_pos.team as usize % FACTION_ORDER.len()];
        let kind = faction.homebase();
        let unit_stats = stats(kind);

        let mesh = unit_mesh(kind, meshes);
        let material = unit_material(faction, materials);

        // Sample terrain height at start position.
        let heightmap_w = parsed.header.heightmap_width();
        let heightmap_x = (start_pos.x / square_size).clamp(0.0, (heightmap_w - 1) as f32) as usize;
        let heightmap_z = (start_pos.z / square_size)
            .clamp(0.0, (parsed.header.heightmap_height() - 1) as f32)
            as usize;
        let height = parsed.heights[heightmap_z * heightmap_w + heightmap_x];

        // Place the unit on the terrain surface.
        let mesh_half_height = 6.0 * unit_stats.mesh_scale;

        commands.spawn((
            MapEntity,
            UnitType(kind),
            faction,
            TeamId(start_pos.team as u8),
            Health::full(unit_stats.max_health),
            Mesh3d(mesh),
            MeshMaterial3d(material),
            Transform::from_xyz(start_pos.x, height + mesh_half_height, start_pos.z),
        ));
    }

    info!(
        "Spawned {} homebases ({} System, {} Hacker, {} Network)",
        map_info.start_positions.len(),
        map_info
            .start_positions
            .iter()
            .filter(|s| s.team as usize % 3 == 0)
            .count(),
        map_info
            .start_positions
            .iter()
            .filter(|s| s.team as usize % 3 == 1)
            .count(),
        map_info
            .start_positions
            .iter()
            .filter(|s| s.team as usize % 3 == 2)
            .count(),
    );
}
