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

use super::spawning::{SelectionVolumeMaterial, spawn_unit};
use crate::interaction::movement::{MovePath, MoveTarget};
use crate::units::assets::animation::CobFileCache;
use crate::units::assets::meshes::S3OModelCache;
use crate::units::components::{Faction, TeamId};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::weapon_fx::{AttackEvent, PendingAttacks};

/// Marks a constructor unit that the player has ordered to build `kind`
/// at world position `site`. Set by the placement UI / build-menu flow.
/// Cleared once the unit reaches the site and construction starts, or
/// when the player replaces the order.
#[derive(Component, Clone, Copy, Debug)]
#[component(storage = "SparseSet")]
pub struct PendingBuild {
    pub kind: UnitKind,
    pub site: Vec3,
}

/// Active construction state on a builder that has arrived at its site.
#[derive(Component, Clone, Copy, Debug)]
#[component(storage = "SparseSet")]
pub struct Constructing {
    pub kind: UnitKind,
    pub site: Vec3,
    /// Seconds accumulated toward finishing.
    pub progress: f32,
}

/// Buildings the constructor `kind` can erect on a datavent. Pulled from
/// upstream `[CANBUILD]` in `SIDEDATA.TDF`.
pub fn buildings_for(kind: UnitKind) -> &'static [UnitKind] {
    match kind {
        UnitKind::Assembler => &[
            UnitKind::Socket,
            UnitKind::BadBlock,
            UnitKind::LogicBomb,
            UnitKind::Debug,
            UnitKind::Terminal,
        ],
        UnitKind::Trojan => &[
            UnitKind::Window,
            UnitKind::BadBlock,
            UnitKind::LogicBomb,
            UnitKind::Debug,
            UnitKind::Obelisk,
        ],
        UnitKind::Gateway => &[
            UnitKind::Port,
            UnitKind::Firewall,
            UnitKind::BadBlock,
            UnitKind::Debug,
            UnitKind::LogicBomb,
        ],
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

/// Promote `PendingBuild` to `Constructing` once the builder is
/// close enough to its datavent. Runs before `movement_system` so we can
/// intercept the step and pin the unit at the build site.
pub fn start_construction(
    mut commands: Commands,
    pending_q: Query<
        (Entity, &Transform, &PendingBuild),
        (Without<Constructing>, Without<crate::units::combat::Dying>),
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

/// Tick build progress on units with `Constructing`, emit a
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
        &mut Transform,
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

    for (entity, gtf, mut transform, faction, team, mut constructing) in &mut builders {
        constructing.progress += dt;

        // Pin the builder's yaw to face the build site so the beam leaves
        // the muzzle piece forward rather than out of the unit's hip —
        // mirrors the way a mobile constructor aims at a ghost before
        // committing.
        if let Some(forward) = forward_toward_site(gtf.translation(), constructing.site) {
            transform.rotation = Transform::from_translation(transform.translation)
                .looking_to(forward, Vec3::Y)
                .rotation;
        }

        // Emit a build beam from the builder's root to the site so the
        // player sees something is happening. Re-uses the factory
        // nanoemitter visual path — PendingAttacks coalesces them.
        let start = gtf.translation() + Vec3::new(0.0, 14.0, 0.0);
        pending_attacks.events.push(AttackEvent {
            attacker_pos: start,
            target_pos: constructing.site,
            weapon_name: std::borrow::Cow::Borrowed("BuildLaser"),
            // Builder BuildLaser also skips the muzzle flash CEG — see
            // the same-named call site in production.rs.
            muzzle_ceg: None,
            delayed_hit: None,
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

/// Horizontal (XZ) unit vector pointing from `builder_pos` toward
/// `site`. Returns `None` when the horizontal distance is below a
/// small epsilon so we don't snap the rotation when the builder is
/// effectively on top of the vent and floating-point noise flips the
/// sign each frame. The caller pipes the result through
/// [`Transform::looking_to`] so the unit's forward vector faces the
/// ghost while `tick_construction` emits build beams.
fn forward_toward_site(builder_pos: Vec3, site: Vec3) -> Option<Vec3> {
    let to_site = site - builder_pos;
    let flat = Vec3::new(to_site.x, 0.0, to_site.z);
    if flat.length_squared() <= 1e-4 {
        return None;
    }
    Some(flat.normalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The returned vector has unit length and lies in the XZ plane — so
    /// `Transform::looking_to(forward, Vec3::Y)` produces a pure yaw
    /// rotation (no pitch leaks in from vertical terrain offsets).
    #[test]
    fn forward_toward_site_is_normalized_and_planar() {
        let f = forward_toward_site(Vec3::ZERO, Vec3::new(10.0, 200.0, 0.0)).unwrap();
        assert!((f.length() - 1.0).abs() < 1e-5);
        assert!(f.y.abs() < 1e-6);
        assert!((f.x - 1.0).abs() < 1e-5);
        assert!(f.z.abs() < 1e-5);
    }

    /// Vertical offset (builder standing higher/lower than the site) must
    /// not influence the flat forward direction — build beams are aimed in XZ only.
    #[test]
    fn forward_toward_site_ignores_vertical_offset() {
        let flat = forward_toward_site(Vec3::ZERO, Vec3::new(20.0, 0.0, 20.0)).unwrap();
        let tall = forward_toward_site(Vec3::ZERO, Vec3::new(20.0, 200.0, 20.0)).unwrap();
        assert!((flat - tall).length() < 1e-5);
    }

    /// Builder on top of the site → no forward vector. If we returned
    /// any direction here the rotation would flicker each frame as
    /// sub-epsilon noise flipped `dx`/`dz`.
    #[test]
    fn forward_toward_site_returns_none_when_on_top() {
        assert_eq!(forward_toward_site(Vec3::ZERO, Vec3::ZERO), None);
        assert_eq!(
            forward_toward_site(
                Vec3::new(100.0, 0.0, -50.0),
                Vec3::new(100.005, 0.0, -50.005),
            ),
            None
        );
    }

    /// Opposite sites produce opposite vectors — catches accidental
    /// sign flips or absolute-value simplifications.
    #[test]
    fn forward_toward_site_flips_sign_across_origin() {
        let east = forward_toward_site(Vec3::ZERO, Vec3::new(50.0, 0.0, 0.0)).unwrap();
        let west = forward_toward_site(Vec3::ZERO, Vec3::new(-50.0, 0.0, 0.0)).unwrap();
        assert!((east + west).length() < 1e-5);
    }
}
