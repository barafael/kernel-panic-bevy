//! Mobile-builder construction pipeline.
//!
//! When the player clicks a building in a constructor's build menu and then
//! a datavent on the map, a `BuildAt { kind, site }` command is queued onto
//! the builder (shift to append, plain click to replace). The movement
//! system walks the builder toward the site; this module takes over once
//! the unit is within its FBI `BuildDistance`:
//!
//! 1. Clear `MoveTarget` / `MovePath` so the builder stops in place.
//! 2. Insert `Constructing` with a progress timer and target.
//! 3. Each frame emit a build-laser beam from builder → site (re-uses the
//!    existing factory-style "BuildLaser" attack-event pipeline).
//! 4. When progress ≥ build time, spawn the finished building at the
//!    datavent with the builder's team/faction, despawn the `Constructing`
//!    component, and let `movement_system` promote the next queued order.

use bevy::prelude::*;

use super::animation::CobFileCache;
use super::components::{Faction, TeamId};
use super::definitions::UnitKind;
use super::meshes::S3OModelCache;
use super::spawning::{SelectionVolumeMaterial, spawn_unit};
use super::unit_registry::UnitRegistry;
use super::weapon_fx::{AttackEvent, PendingAttacks};
use crate::interaction::movement::{MovePath, MoveTarget};

/// Marks a constructor unit that the player has ordered to build `kind`
/// at world position `site`. Set by the placement UI / build-menu flow.
/// Cleared once the unit reaches the site and construction starts, or
/// when the player replaces the order.
#[derive(Component, Clone, Copy, Debug)]
pub struct PendingBuild {
    pub kind: UnitKind,
    pub site: Vec3,
}

/// Active construction state on a builder that has arrived at its site.
#[derive(Component, Clone, Copy, Debug)]
pub struct Constructing {
    pub kind: UnitKind,
    pub site: Vec3,
    /// Seconds accumulated toward finishing.
    pub progress: f32,
}

/// Which units can erect structures on datavents. Mirrors the "builder"
/// units in upstream KP's `SIDEDATA.TDF` (assembler / trojan / gateway).
pub fn is_constructor(kind: UnitKind) -> bool {
    matches!(
        kind,
        UnitKind::Assembler | UnitKind::Trojan | UnitKind::Gateway
    )
}

/// Buildings the constructor `kind` can erect on a datavent. Pulled from
/// upstream `[CANBUILD]` in `SIDEDATA.TDF`, trimmed to the units we have
/// in our roster (we lack Badblock / Mineblaster / Obelisk).
pub fn buildings_for(kind: UnitKind) -> &'static [UnitKind] {
    match kind {
        UnitKind::Assembler => &[UnitKind::Socket, UnitKind::LogicBomb],
        UnitKind::Trojan => &[UnitKind::Window, UnitKind::LogicBomb],
        UnitKind::Gateway => &[UnitKind::Port, UnitKind::Firewall, UnitKind::LogicBomb],
        _ => &[],
    }
}

/// Spring's Trojan/Gateway FBIs use `BuildDistance=384`. Assembler doesn't
/// declare one (upstream fell back to the engine default ~128). We use a
/// single constant here since our placement system snaps onto a discrete
/// datavent, not an arbitrary patch of ground — once the builder is close
/// enough that the laser reads as "attached to the site", construction
/// can start.
const BUILD_DISTANCE: f32 = 180.0;

/// System: promote `PendingBuild` to `Constructing` once the builder is
/// close enough to its datavent. Runs before `movement_system` so we can
/// intercept the step and pin the unit at the build site.
pub fn start_construction(
    mut commands: Commands,
    pending_q: Query<
        (Entity, &Transform, &PendingBuild),
        (Without<Constructing>, Without<super::combat::Dying>),
    >,
) {
    for (entity, transform, pending) in &pending_q {
        let dx = transform.translation.x - pending.site.x;
        let dz = transform.translation.z - pending.site.z;
        let dist_sq = dx * dx + dz * dz;
        if dist_sq <= BUILD_DISTANCE * BUILD_DISTANCE {
            commands
                .entity(entity)
                .insert(Constructing {
                    kind: pending.kind,
                    site: pending.site,
                    progress: 0.0,
                })
                .remove::<MoveTarget>()
                .remove::<MovePath>()
                .remove::<PendingBuild>();
        }
    }
}

/// System: tick build progress on units with `Constructing`, emit a
/// build-laser beam, and spawn the finished structure when the timer
/// elapses. The unit is freed from `Constructing` at completion; the
/// movement queue's next order (if any) is promoted next frame by
/// `movement_system` once it sees `MoveTarget` is absent.
#[allow(clippy::too_many_arguments)]
pub fn tick_construction(
    time: Res<Time>,
    mut commands: Commands,
    mut builders: Query<(
        Entity,
        &GlobalTransform,
        &Faction,
        &TeamId,
        &mut Constructing,
    )>,
    unit_registry: Res<UnitRegistry>,
    mut pending_attacks: ResMut<PendingAttacks>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<S3OModelCache>,
    mut cob_cache: ResMut<CobFileCache>,
    invisible_mat: Option<Res<SelectionVolumeMaterial>>,
) {
    let dt = time.delta_secs();
    let Some(invisible_mat) = invisible_mat else {
        return;
    };
    let invisible_mat_clone = SelectionVolumeMaterial(invisible_mat.0.clone());

    for (entity, gtf, faction, team, mut constructing) in &mut builders {
        constructing.progress += dt;

        // Emit a build beam from the builder's root to the site so the
        // player sees something is happening. Re-uses the factory
        // nanoemitter visual path — PendingAttacks coalesces them.
        let start = gtf.translation() + Vec3::new(0.0, 14.0, 0.0);
        pending_attacks.events.push(AttackEvent {
            attacker_pos: start,
            target_pos: constructing.site,
            weapon_name: "BuildLaser".to_string(),
        });

        let build_time = unit_registry.build_time(constructing.kind);
        if build_time > 0.0 && constructing.progress >= build_time {
            // Spawn the structure at the datavent.
            spawn_unit(
                constructing.kind,
                *faction,
                team.0,
                constructing.site,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut model_cache,
                &mut cob_cache,
                &invisible_mat_clone,
                &unit_registry,
            );
            commands.entity(entity).remove::<Constructing>();
        }
    }
}
