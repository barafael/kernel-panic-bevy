//! Network faction's packet-buffer mechanic.
//!
//! Ports don't spawn Packets directly. Instead each Port ticks a shared
//! per-team counter every `PORT_TICK_INTERVAL` seconds; the player
//! dispatches stored packets from any teleporter (Port or Connection)
//! by hotkey. Packets near a friendly teleporter can return to the
//! buffer, giving Network players mobile spawn points.
//!
//! Mirrors upstream `LuaRules/Gadgets/network_buffer.lua` (and the
//! dispatch / enter companion gadgets).

use std::collections::HashMap;

use bevy::prelude::*;

use super::combat::Dying;
use super::components::{Faction, TeamId, UnitType};
use super::definitions::UnitKind;

/// Seconds between per-Port buffer increments. Upstream uses 164 sim
/// frames at 30 fps ≈ 5.47s.
const PORT_TICK_INTERVAL: f32 = 164.0 / 30.0;

/// Max packets that can spawn in a single Dispatch — matches upstream's
/// 12-slot offset list.
pub const DISPATCH_MAX: usize = 12;

/// Seconds a freshly-spawned Packet must wait before it can re-enter
/// the buffer via the Enter command. Upstream `stunTime=180` frames.
pub const SPAWN_STUN_SECONDS: f32 = 180.0 / 30.0;

/// Max distance (elmos) at which a Packet can enter a teleporter.
/// Upstream `enterDist=150`.
pub const ENTER_DISTANCE: f32 = 150.0;

/// Dispatch spawns Packets in a ring around the teleporter at this
/// radius, matching the upstream offset pattern of ~48 elmos.
pub const DISPATCH_RING_RADIUS: f32 = 48.0;

/// Shared across-team packet counters. A team's Ports tick into it;
/// dispatch drains it; Packets re-entering top it back up.
#[derive(Resource, Default)]
pub struct PacketBuffer(pub HashMap<u8, u32>);

impl PacketBuffer {
    pub fn get(&self, team: u8) -> u32 {
        self.0.get(&team).copied().unwrap_or(0)
    }

    pub fn add(&mut self, team: u8, n: u32) {
        *self.0.entry(team).or_default() += n;
    }

    /// Drain up to `want` packets from the team's buffer, returning how
    /// many were actually available.
    pub fn take(&mut self, team: u8, want: u32) -> u32 {
        let entry = self.0.entry(team).or_default();
        let taken = (*entry).min(want);
        *entry -= taken;
        taken
    }
}

/// Per-Port timer component: when it reaches `PORT_TICK_INTERVAL`, the
/// Port increments its team's buffer and resets.
#[derive(Component, Default)]
pub struct PortTimer(pub f32);

/// Attached to freshly-dispatched Packets; blocks re-entry until the
/// timer expires, matching upstream's spawnStun behaviour.
#[derive(Component)]
pub struct PacketSpawnStun {
    pub remaining: f32,
}

/// Event: dispatch packets from the given teleporter toward `target`.
#[derive(Message, Debug, Clone, Copy)]
pub struct DispatchEvent {
    pub teleporter: Entity,
    pub target: Vec3,
}

/// Event: send the given Packet into the nearest friendly teleporter's
/// buffer. Silently ignored if the Packet is still stunned or too far.
#[derive(Message, Debug, Clone, Copy)]
pub struct EnterEvent {
    pub packet: Entity,
}

/// System: tick every Port's internal timer and top up its team's
/// buffer when the interval elapses.
pub fn tick_port_buffers(
    time: Res<Time>,
    mut ports: Query<(&UnitType, &TeamId, &mut PortTimer), Without<Dying>>,
    mut buffer: ResMut<PacketBuffer>,
) {
    let dt = time.delta_secs();
    for (unit, team, mut timer) in &mut ports {
        if unit.0 != UnitKind::Port {
            continue;
        }
        timer.0 += dt;
        if timer.0 >= PORT_TICK_INTERVAL {
            timer.0 -= PORT_TICK_INTERVAL;
            buffer.add(team.0, 1);
        }
    }
}

/// System: tick `PacketSpawnStun` and remove the component once the
/// stun window has expired.
pub fn tick_spawn_stun(
    time: Res<Time>,
    mut query: Query<(Entity, &mut PacketSpawnStun)>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (entity, mut stun) in &mut query {
        stun.remaining -= dt;
        if stun.remaining <= 0.0 {
            commands.entity(entity).remove::<PacketSpawnStun>();
        }
    }
}

/// Whether `kind` can dispatch from / absorb into the buffer.
pub fn is_teleporter(kind: UnitKind) -> bool {
    matches!(kind, UnitKind::Port | UnitKind::Connection)
}

/// System: drain `DispatchEvent`s, consuming packets from the team
/// buffer and requesting spawns from the teleporter's position. Each
/// dispatched packet gets a MoveTarget toward the event's `target`.
#[allow(clippy::too_many_arguments)]
pub fn process_dispatch(
    mut events: MessageReader<DispatchEvent>,
    teleporters: Query<(&UnitType, &TeamId, &Faction, &Transform)>,
    mut buffer: ResMut<PacketBuffer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<super::meshes::S3OModelCache>,
    mut cob_cache: ResMut<super::animation::CobFileCache>,
    invisible_mat: Res<super::spawning::SelectionVolumeMaterial>,
    unit_registry: Res<super::unit_registry::UnitRegistry>,
    mut commands: Commands,
) {
    for event in events.read() {
        let Ok((unit, team, faction, transform)) = teleporters.get(event.teleporter) else {
            continue;
        };
        if !is_teleporter(unit.0) {
            continue;
        }

        let available = buffer.take(team.0, DISPATCH_MAX as u32) as usize;
        if available == 0 {
            continue;
        }

        let origin = transform.translation;
        for i in 0..available {
            let angle = (i as f32 / DISPATCH_MAX as f32) * std::f32::consts::TAU;
            let spawn_pos = origin
                + Vec3::new(
                    angle.cos() * DISPATCH_RING_RADIUS,
                    0.0,
                    angle.sin() * DISPATCH_RING_RADIUS,
                );
            let spawned = super::spawning::spawn_unit(
                UnitKind::Packet,
                *faction,
                team.0,
                spawn_pos,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut model_cache,
                &mut cob_cache,
                &invisible_mat,
                &unit_registry,
            );
            commands.entity(spawned).insert((
                crate::interaction::movement::MoveTarget(event.target),
                PacketSpawnStun {
                    remaining: SPAWN_STUN_SECONDS,
                },
            ));
        }
    }
}

/// System: drain `EnterEvent`s — absorb Packets that are close to a
/// friendly teleporter and have finished their post-dispatch stun.
pub fn process_enter(
    mut events: MessageReader<EnterEvent>,
    packets: Query<(&UnitType, &TeamId, &Transform, Option<&PacketSpawnStun>), Without<Dying>>,
    teleporters: Query<(&UnitType, &TeamId, &Transform), Without<Dying>>,
    mut buffer: ResMut<PacketBuffer>,
    mut commands: Commands,
) {
    let enter_sq = ENTER_DISTANCE * ENTER_DISTANCE;
    for event in events.read() {
        let Ok((packet_unit, packet_team, packet_tf, stun)) = packets.get(event.packet) else {
            continue;
        };
        if packet_unit.0 != UnitKind::Packet || stun.is_some() {
            continue;
        }
        // Nearest friendly teleporter within range.
        let nearest = teleporters
            .iter()
            .filter(|(t_unit, t_team, _)| is_teleporter(t_unit.0) && t_team.0 == packet_team.0)
            .map(|(_, _, t_tf)| packet_tf.translation.distance_squared(t_tf.translation))
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        if nearest.is_some_and(|d_sq| d_sq <= enter_sq) {
            buffer.add(packet_team.0, 1);
            commands.entity(event.packet).despawn();
        }
    }
}

/// Whether the player team is controlling the Network faction, so the
/// HUD knows to show the buffer counter. Used by hud/top_bar.
pub fn player_plays_network(player_team: u8, homebases: &[(u8, Faction)]) -> bool {
    homebases
        .iter()
        .any(|(t, f)| *t == player_team && matches!(f, Faction::Network))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_add_and_take_round_trip() {
        let mut b = PacketBuffer::default();
        assert_eq!(b.get(0), 0);
        b.add(0, 5);
        assert_eq!(b.get(0), 5);
        assert_eq!(b.take(0, 3), 3);
        assert_eq!(b.get(0), 2);
        assert_eq!(b.take(0, 10), 2);
        assert_eq!(b.get(0), 0);
    }

    #[test]
    fn buffers_are_team_scoped() {
        let mut b = PacketBuffer::default();
        b.add(0, 3);
        b.add(1, 7);
        assert_eq!(b.get(0), 3);
        assert_eq!(b.get(1), 7);
        assert_eq!(b.take(0, 2), 2);
        assert_eq!(b.get(0), 1);
        assert_eq!(b.get(1), 7);
    }

    #[test]
    fn is_teleporter_classifies_correctly() {
        assert!(is_teleporter(UnitKind::Port));
        assert!(is_teleporter(UnitKind::Connection));
        assert!(!is_teleporter(UnitKind::Packet));
        assert!(!is_teleporter(UnitKind::Kernel));
    }

    #[test]
    fn network_detection() {
        let bases = [(0, Faction::Network), (1, Faction::System)];
        assert!(player_plays_network(0, &bases));
        assert!(!player_plays_network(1, &bases));
        assert!(!player_plays_network(2, &bases));
    }
}
