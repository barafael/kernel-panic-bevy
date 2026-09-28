//! Bug ↔ Exploit deploy.
//!
//! Bugs can deploy into stationary Exploits (long-range artillery that
//! deal more damage the farther their target is). The player triggers
//! the deploy/pack-up cycle via the `D` hotkey; health is transferred
//! proportionally so a 50%-HP Bug becomes a 50%-HP Exploit.

use bevy::prelude::*;

use crate::units::combat::Dying;
use crate::units::components::{Faction, Health, TeamId, UnitType};
use crate::units::lifecycle::bookkeeping::TotalUnitCount;
use crate::units::lifecycle::spawning::{SpawnContext, spawn_unit};

/// Event: the listed unit should deploy to its paired form. The
/// deploy system resolves the target kind from the source.
#[derive(Message, Debug, Clone, Copy)]
pub struct DeployEvent {
    pub entity: Entity,
}

/// Process deploy events by despawning the source and spawning
/// its pair with proportional HP at the same position.
pub fn process_deploy(
    mut events: MessageReader<DeployEvent>,
    // `Without<Dying>`: a unit in its death animation must not be
    // swapped for a fresh pair (the corpse would be resurrected as a
    // 1 % pair while the death pipeline still owns it).
    query: Query<(&UnitType, &Faction, &TeamId, &Transform, &Health), Without<Dying>>,
    mut ctx: SpawnContext,
    mut count: ResMut<TotalUnitCount>,
) {
    for event in events.read() {
        let Ok((unit, faction, team, transform, health)) = query.get(event.entity) else {
            continue;
        };
        if health.current <= 0.0 {
            continue;
        }
        let Some(target_kind) = unit.0.deploy_pair() else {
            continue;
        };

        // HP carry-over: preserve the health fraction. A 1-HP Bug
        // deploys into a 1-HP-equivalent Exploit, not a pristine one.
        let hp_fraction = health.fraction().clamp(0.01, 1.0);
        let new_max = ctx.unit_registry.max_health(target_kind);
        let new_current = (new_max * hp_fraction).max(1.0);

        // The pair is spawned in its place; the source skips `Dying`,
        // so it leaves the unit count here rather than in
        // `track_dying_units`.
        ctx.commands.entity(event.entity).despawn();
        count.0 = count.0.saturating_sub(1);

        let spawned = spawn_unit(
            target_kind,
            *faction,
            team.0,
            transform.translation,
            &mut ctx,
        );
        ctx.commands.entity(spawned).insert(Health {
            current: new_current,
            max: new_max,
        });
    }
}
