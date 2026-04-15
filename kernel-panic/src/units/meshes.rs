use bevy::prelude::*;

use super::components::Faction;
use super::definitions::{UnitKind, stats};

/// Create a `StandardMaterial` for a unit with faction-colored emissive glow.
pub fn unit_material(
    faction: Faction,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    let color = faction.color();
    materials.add(StandardMaterial {
        base_color: color,
        emissive: LinearRgba::from(color) * 4.0,
        unlit: true,
        ..default()
    })
}

/// Create a mesh for a unit type. Each unit gets a distinct geometric shape.
pub fn unit_mesh(kind: UnitKind, meshes: &mut Assets<Mesh>) -> Handle<Mesh> {
    let unit_stats = stats(kind);
    let scale = unit_stats.mesh_scale;

    let mesh = match kind {
        // Homebases: large octagonal prisms (tall cylinders with 8 sides)
        UnitKind::Kernel | UnitKind::Hole | UnitKind::Connection => {
            Cylinder::new(20.0 * scale, 12.0 * scale)
        }

        // Factories: shorter, wider cylinders
        UnitKind::Socket | UnitKind::Window | UnitKind::Port => {
            Cylinder::new(15.0 * scale, 6.0 * scale)
        }

        // Defensive structures
        UnitKind::Firewall | UnitKind::Exploit => Cylinder::new(10.0 * scale, 8.0 * scale),

        // Swarm units: small shapes
        UnitKind::Bit | UnitKind::Bug | UnitKind::Packet | UnitKind::Virus | UnitKind::Signal => {
            Cylinder::new(3.0 * scale, 4.0 * scale)
        }

        // Medium units
        UnitKind::Assembler
        | UnitKind::Worm
        | UnitKind::Dos
        | UnitKind::Pointer
        | UnitKind::LogicBomb => Cylinder::new(6.0 * scale, 6.0 * scale),

        // Heavy: Byte
        UnitKind::Byte => Cylinder::new(12.0 * scale, 10.0 * scale),
    };

    meshes.add(mesh)
}
