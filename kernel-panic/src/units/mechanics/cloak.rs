//! Visibility: cloak + fog-of-war.
//!
//! Both systems are gated on [`FogEnabled`], which
//! [`sync_fog_enabled`] derives every tick: on for a seated player
//! still in play, off for a spectator (a replay's viewer, the menu's
//! attract demo) and for a player whose owner has left play — upstream's
//! `CPlayer::StartSpectating` gives the defeated `spectatingFullView`.
//! When off, every unit stays `Visible` and gets `Spotted`.
//!
//! Two compose-able visibility systems live here, both throttled to
//! the same [`VISIBILITY_REFRESH_INTERVAL`] and both seen from the
//! human player's [`LocalTeam`]:
//!
//! - **Cloak** ([`update_cloak_visibility`]) — Logic Bombs
//!   (`Init_Cloaked=1`) and Worms (stealth ambushers per upstream)
//!   are hidden from the player until a player-team detector
//!   (Assembler / Trojan / Gateway with non-zero FBI `RadarDistance`)
//!   enters range. Active detection only — no memory. The detection
//!   itself is gameplay, not rendering: [`update_cloak_detection`]
//!   keeps a per-team [`DetectedBy`] mask on every cloaked unit
//!   regardless of [`FogEnabled`], and auto-targeting refuses cloaked
//!   units the shooter's team hasn't detected.
//!
//! - **Fog of war** ([`update_fog_visibility`]) — non-cloaked enemy
//!   units are visible iff currently within any player-team unit's
//!   FBI `SightDistance`; the [`Spotted`] marker is added/removed in
//!   lockstep so downstream consumers (minimap filter) get cheap
//!   reads. Full active LoS, not the memory variant.
//!
//! The two systems partition on [`Cloaked`]: cloak handles entities
//! with the marker, fog handles everything else, so their writes to
//! [`Visibility`] don't race. The fog only affects rendering; the
//! cloak's targeting rule applies to every team, AI included.

use bevy::ecs::entity::{EntityHashMap, EntityHashSet};
use bevy::prelude::*;

use crate::units::assets::model_fade::{ModelMaterials, restore_model_materials};
use crate::units::combat::Dying;
use crate::units::components::{TeamId, UnitType};
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::lifecycle::bookkeeping::Detector;
use crate::units::lifecycle::production::PendingFadeInstall;
use crate::units::lifecycle::spawning::FadeMaterials;
use crate::units::player::LocalTeam;
use crate::units::teams::{Alliances, TeamFates};

/// Master switch for fog-of-war / cloak-hiding from the [`LocalTeam`]'s
/// perspective, kept by [`sync_fog_enabled`]. Off, the visibility
/// systems keep `Spotted` in sync but treat every team as friendly.
/// Cloak *detection* (and the targeting rule it feeds) runs regardless.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct FogEnabled(pub bool);

/// Whether the local player sees through line of sight: seated and
/// still in play. A spectator, a replay's viewer, the attract demo and a
/// defeated or resigned owner see everything (`CPlayer::StartSpectating`
/// sets `spectatingFullView`); a dropped owner who comes back
/// (`TeamFates::rejoin`) is fogged again.
pub fn fog_applies(local: LocalTeam, fates: &TeamFates) -> bool {
    local.seat().is_some_and(|seat| !fates.is_dead(seat))
}

/// Keep [`FogEnabled`] at [`fog_applies`] (and off under `KP_NO_FOG`).
/// Derived every tick rather than set once at match start: the
/// networked seat arrives a frame after `begin_match`, and defeat and
/// rejoin flip it mid-match. Written only on a change, so nothing
/// downstream sees a changed resource every tick.
pub fn sync_fog_enabled(
    local: Res<LocalTeam>,
    fates: Res<TeamFates>,
    dev: Option<Res<crate::game_setup::DevOptions>>,
    mut fog: ResMut<FogEnabled>,
) {
    let next = fog_applies(*local, &fates) && !dev.is_some_and(|d| d.no_fog);
    if fog.0 != next {
        fog.0 = next;
    }
}

/// A hostile unit spawned (or cloaked) this tick starts hidden under
/// fog instead of showing for the up-to-three ticks until the next
/// visibility scan: a Bit stepping out of an unseen Socket, or a Logic
/// Bomb laid in the dark, used to blink once. The scans take it from
/// there (`Without<Cloaked>` / `With<Cloaked>` as they partition).
#[allow(clippy::type_complexity)]
pub fn hide_new_hostiles(
    fog: Res<FogEnabled>,
    local: Res<LocalTeam>,
    alliances: Res<Alliances>,
    mut fresh: Query<(&TeamId, &mut Visibility), Or<(Added<UnitType>, Added<Cloaked>)>>,
) {
    if !fog.0 {
        return;
    }
    for (team, mut vis) in &mut fresh {
        if !alliances.friendly(team.0, local.0) && *vis != Visibility::Hidden {
            *vis = Visibility::Hidden;
        }
    }
}

/// Marker: this unit hides from enemies unless a detector is close.
/// Logic Bombs carry it until they detonate; Worms drop it while
/// surfaced to bite and regain it once the aim resets
/// (`mechanics::worm`, upstream worm.bos `AimWeapon1` / `ResetAim`).
#[derive(Component)]
#[require(DetectedBy)]
pub struct Cloaked;

/// Bitmask of the owners (bit `t` = `TeamId(t)`) that currently see
/// this cloaked unit: a detector in range reveals it to its whole ally
/// team (Spring keeps decloak coverage per allyteam), so every allied
/// owner's bit is set. Refreshed by [`update_cloak_detection`]; only
/// meaningful while [`Cloaked`] is present. Owners ≥ 64 never detect
/// (no map ships that many).
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DetectedBy(pub u64);

impl DetectedBy {
    pub fn contains(self, team: u8) -> bool {
        self.0 & team_bit(team) != 0
    }
}

/// `DetectedBy` bit for `team`; `0` for teams that don't fit the mask.
pub fn team_bit(team: u8) -> u64 {
    1u64.checked_shl(u32::from(team)).unwrap_or(0)
}

/// Marker: this unit is currently visible to the [`LocalTeam`].
/// Friendly units always carry it (they're always visible); enemies
/// have it added when they enter sight and removed when they leave.
/// Maintained in lockstep with [`Visibility`] by
/// [`update_fog_visibility`] so minimap / HUD systems can filter on
/// it cheaply.
#[derive(Component)]
pub struct Spotted;

/// Seconds between visibility scans. A late reveal (~80ms) is well
/// below the perception threshold, while every-frame scans would be
/// O(cloaked × detectors) + O(enemies × observers) — no benefit at
/// 60 Hz. Matches the cadence of `count_small_buildings`.
pub const VISIBILITY_REFRESH_INTERVAL: f32 = 0.1;

/// One 30 Hz sim tick, in seconds — the phase step that spreads the
/// three visibility scans over different ticks. All three used to
/// accumulate from zero and so all fired on the same tick, stacking
/// their spatial sweeps on top of each other every third frame.
const SCAN_PHASE_STEP: f32 = 1.0 / 30.0;

/// Refresh timer for `update_cloak_visibility`. `update_fog_visibility`
/// uses its own [`FogRefreshTimer`]; the two ticks land on different
/// frames by construction — both feed `Visibility` and don't depend
/// on each other.
#[derive(Resource)]
pub struct VisibilityRefreshTimer(pub f32);

impl Default for VisibilityRefreshTimer {
    /// One tick ahead of `update_cloak_detection` (which starts at 0).
    fn default() -> Self {
        Self(SCAN_PHASE_STEP)
    }
}

/// Refresh timer for `update_fog_visibility`, two ticks ahead of
/// `update_cloak_detection`. Match-scoped like the other two.
#[derive(Resource)]
pub struct FogRefreshTimer(f32);

impl Default for FogRefreshTimer {
    fn default() -> Self {
        Self(2.0 * SCAN_PHASE_STEP)
    }
}

/// Refresh timer for `update_cloak_detection`, whose [`SightedBy`] and
/// [`DetectedBy`] masks feed targeting: sim state, so match-scoped
/// (every match scans on the same ticks, whatever ran before).
#[derive(Resource, Default)]
pub struct CloakDetectionTimer(f32);

/// Which owners' allyteams currently have a unit in their line of sight
/// or on their radar (`unit->losStatus[allyteam]`: `LOS_INLOS` /
/// `LOS_INRADAR`). Bit `t` is owner `t`; an ally's sight counts for
/// every ally ([`Alliances::vision_mask`]). A weapon may only target
/// what its own team has in either (`CWeapon::TestTarget`,
/// `Weapon.cpp:1039-1040`); a radar-only contact is aimed at a drifting
/// point and ranks far below what is seen.
///
/// Refreshed by [`update_cloak_detection`]. A cloaked unit is never in an
/// enemy's line of sight (`LosHandler.cpp:879-883`), only on radar.
/// The default (a unit that has not been scanned yet) is "seen by
/// everyone": a fresh unit triggers a scan on the tick it appears, so
/// the default is never read.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SightedBy {
    pub los: u64,
    pub radar: u64,
}

impl Default for SightedBy {
    fn default() -> Self {
        Self {
            los: u64::MAX,
            radar: 0,
        }
    }
}

/// Elmos a unit of sight `distance` sees: `losMipLevel = 1` squares of
/// 16 elmos (`(distance / 8) >> 1` of them): a sight of 300 is 288.
pub fn los_radius(distance: f32) -> f32 {
    ((distance / 8.0).floor() / 2.0).floor() * 16.0
}

/// Elmos a radar of `distance` covers: `radarMipLevel = 2` squares of 32
/// elmos (`ModInfo.cpp:119`): a radar of 500 covers 480.
pub fn radar_radius(distance: f32) -> f32 {
    ((distance / 8.0).floor() / 4.0).floor() * 32.0
}

/// `defBaseRadarErrorSize` (`LosHandler.h:319`): how far off a radar
/// contact's apparent position can be, in elmos.
pub const RADAR_ERROR: f32 = 96.0;

/// Where a radar-only contact at `pos` appears to be on `frame`: a
/// drifting offset of up to [`RADAR_ERROR`] elmos, new every slow update
/// (`Unit::UpdatePosErrorParams`; `GetUnitPositionWithError`). Hashed
/// from the position and the update count, so every peer aims at the
/// same point.
pub fn radar_blip(pos: Vec3, frame: u64) -> Vec3 {
    let mut h = (u64::from(pos.x.to_bits()) << 32 | u64::from(pos.z.to_bits()))
        ^ (frame / u64::from(crate::sim::SLOW_UPDATE_RATE)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut next = || {
        // splitmix64
        h = h.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = h;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        // [-1, 1)
        (z >> 40) as f32 / (1u64 << 23) as f32 - 1.0
    };
    let (dx, dz) = (next(), next());
    pos + Vec3::new(dx, 0.0, dz) * RADAR_ERROR
}

/// Recompute every unit's [`SightedBy`] — who has it in line of sight
/// and who has it on radar — and the [`DetectedBy`] mask of each cloaked
/// one (its radar coverage).
///
/// Every unit sees out to its FBI `SightDistance` ([`los_radius`]); the
/// units with an active radar ([`Detector`]: Assembler, Trojan,
/// Gateway; not while stunned) cover [`radar_radius`] around them. A
/// building under construction has no sensors. Each sees for its whole
/// ally team.
///
/// Runs for every team independently of [`FogEnabled`] — it feeds combat
/// targeting, not just the player's rendering. Throttled to
/// [`VISIBILITY_REFRESH_INTERVAL`], and run at once when a unit appears;
/// positions come from the spatial snapshot (at most one sim tick stale).
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn update_cloak_detection(
    time: Res<Time>,
    mut timer: ResMut<CloakDetectionTimer>,
    spatial: Res<crate::units::spatial::SpatialIndex>,
    unit_registry: Res<UnitRegistry>,
    viewers_q: Query<
        (
            &TeamId,
            &UnitType,
            &Transform,
            Option<&Detector>,
            Has<crate::units::combat::Stunned>,
        ),
        (
            Without<Dying>,
            Without<crate::units::lifecycle::spawning::Emerging>,
        ),
    >,
    mut sighted_q: Query<(Entity, &TeamId, &mut SightedBy)>,
    mut cloaked_q: Query<(Entity, &mut DetectedBy), With<Cloaked>>,
    fresh: Query<(), Added<UnitType>>,
    alliances: Res<Alliances>,
    // Retained across frames so the refresh tick doesn't reallocate;
    // cleared on every scan and only looked up (the OR is commutative).
    mut seen: Local<EntityHashMap<(u64, u64)>>,
) {
    if fresh.is_empty() {
        timer.0 += time.delta_secs();
        if timer.0 < VISIBILITY_REFRESH_INTERVAL {
            return;
        }
    }
    timer.0 = 0.0;

    // Invert the scan: instead of testing every unit against every
    // viewer, radius-query the shared spatial index once per viewer and
    // OR its ally team's bits into each hostile entry in reach.
    seen.clear();
    for (team, unit, tf, detector, stunned) in &viewers_q {
        let bit = alliances.vision_mask(team.0);
        let vp = tf.translation;
        let sight = los_radius(unit_registry.sight_distance(unit.0));
        spatial.query_radius(vp, sight, |candidate| {
            // A cloaked unit is never in an enemy's sight.
            if candidate.feature
                || candidate.cloaked
                || alliances.friendly(candidate.team, team.0)
                || candidate.pos.distance_squared(vp) > sight * sight
            {
                return;
            }
            seen.entry(candidate.entity).or_default().0 |= bit;
        });
        // A stunned unit's sensors are off (`LosHandler.cpp:182-186`).
        let Some(detector) = detector.filter(|_| !stunned) else {
            continue;
        };
        let radar = radar_radius(detector.radius);
        spatial.query_radius(vp, radar, |candidate| {
            if !candidate.feature
                && !alliances.friendly(candidate.team, team.0)
                && candidate.pos.distance_squared(vp) <= radar * radar
            {
                seen.entry(candidate.entity).or_default().1 |= bit;
            }
        });
    }

    for (entity, team, mut sighted) in &mut sighted_q {
        let (los, radar) = seen.get(&entity).copied().unwrap_or((0, 0));
        let next = SightedBy {
            // A unit is always in its own allyteam's sight.
            los: los | alliances.vision_mask(team.0),
            radar,
        };
        if *sighted != next {
            *sighted = next;
        }
    }
    for (entity, mut mask) in &mut cloaked_q {
        let next = DetectedBy(seen.get(&entity).map_or(0, |s| s.1));
        if *mask != next {
            *mask = next;
        }
    }
}

/// Visibility for cloaked units (Worms / Logic Bombs) from the
/// [`LocalTeam`]'s perspective.
///
/// - Friendly cloaked: always visible (`install_cloak_fade_materials`
///   handles the half-alpha rendering).
/// - Enemy cloaked: hidden unless [`update_cloak_detection`] reports a
///   player-team detector in range.
///
/// Throttled to [`VISIBILITY_REFRESH_INTERVAL`].
pub fn update_cloak_visibility(
    time: Res<Time>,
    mut timer: ResMut<VisibilityRefreshTimer>,
    fog: Res<FogEnabled>,
    player: Res<LocalTeam>,
    alliances: Res<Alliances>,
    mut cloaked_q: Query<(&TeamId, &DetectedBy, &mut Visibility), With<Cloaked>>,
) {
    timer.0 += time.delta_secs();
    if timer.0 < VISIBILITY_REFRESH_INTERVAL {
        return;
    }
    timer.0 = 0.0;

    for (team, detected_by, mut vis) in &mut cloaked_q {
        // Sandbox mode (fog off): nothing hides, every cloaked unit
        // just stays visible.
        let target =
            if !fog.0 || alliances.friendly(team.0, player.0) || detected_by.contains(player.0) {
                Visibility::Visible
            } else {
                Visibility::Hidden
            };
        if *vis != target {
            *vis = target;
        }
    }
}

/// Alpha applied to friendly cloaked units so the player can see
/// they're cloaked at a glance (FEATURES.md §17).
const CLOAK_FADE_ALPHA: f32 = 0.5;

/// Swap records produced by [`install_cloak_fade_materials`]: each
/// model piece wears the shared `FadeMaterialCache` variant of its
/// material at [`CLOAK_FADE_ALPHA`]. On de-cloak
/// ([`restore_cloak_fade_materials`]) the original handles are
/// re-installed so the unit pops back to fully opaque.
#[derive(Component)]
pub struct CloakFadeMaterials {
    /// (piece_entity, original) — the shared material to put back.
    pub(crate) overrides: Vec<(Entity, Handle<StandardMaterial>)>,
}

/// Give friendly cloaked units the half-alpha variant of their model
/// material ([`ModelMaterials::fade`]). Enemy cloaked units are either
/// hidden entirely or revealed at full opacity (detector reveal), so
/// they skip this pass.
///
/// Waits out a running emerge fade (`FadeMaterials` /
/// `PendingFadeInstall`): both swap the same piece materials, and
/// whichever restored second would have put the other's variant back
/// for good — a Worm or Logic Bomb leaving its factory would have ended up
/// opaque while cloaked, or translucent forever. Not keyed on
/// `Added<Cloaked>` for the same reason: a unit cloaked while emerging
/// gets its cloak look as soon as the emerge fade is done. A Worm
/// re-cloaks after every bite and wears the same shared variant again.
#[allow(clippy::type_complexity)]
pub fn install_cloak_fade_materials(
    mut commands: Commands,
    fog: Res<FogEnabled>,
    player: Res<LocalTeam>,
    alliances: Res<Alliances>,
    new_cloaked: Query<
        (Entity, &TeamId, &Children),
        (
            With<Cloaked>,
            Without<CloakFadeMaterials>,
            Without<FadeMaterials>,
            Without<PendingFadeInstall>,
        ),
    >,
    mut model: ModelMaterials,
) {
    for (entity, team, children) in &new_cloaked {
        // While fog is off, every team is "friendly" — fade them all so
        // the player can see at a glance which units are cloaked.
        if fog.0 && !alliances.friendly(team.0, player.0) {
            continue;
        }
        let overrides = model.fade(children, CLOAK_FADE_ALPHA, &mut commands);
        // Inserted even when nothing was swapped (a unit without model
        // pieces), so the unit isn't re-walked every tick.
        commands
            .entity(entity)
            .insert(CloakFadeMaterials { overrides });
    }
}

/// Revert each swapped piece back to its pre-cloak material once
/// [`Cloaked`] is gone (a Logic Bomb detonating, a Worm surfacing).
/// Paired with [`install_cloak_fade_materials`]. A query rather than
/// `RemovedComponents<Cloaked>`: removal messages are cleared once per
/// app frame, so a `FixedUpdate` reader misses most of them whenever
/// the frame rate exceeds the 30 Hz tick.
pub fn restore_cloak_fade_materials(
    mut commands: Commands,
    uncloaked: Query<(Entity, &CloakFadeMaterials), Without<Cloaked>>,
) {
    for (entity, fade) in &uncloaked {
        restore_model_materials(&fade.overrides, &mut commands);
        commands.entity(entity).remove::<CloakFadeMaterials>();
    }
}

/// Active fog-of-war over non-cloaked units, applied from the
/// [`LocalTeam`]'s perspective. Friendly units (team == player)
/// stay visible. Enemy units are visible iff *currently* within
/// any friendly unit's FBI `SightDistance`; the [`Spotted`] marker
/// is added/removed in lockstep with that visibility so downstream
/// systems (minimap dot filter) can read it cheaply. Sight is
/// revoked when the friendly observer dies or moves away — full
/// LoS, not the memory variant.
///
/// AI-controlled teams ignore this entirely; their decision systems
/// query the world directly.
///
/// Throttled to [`VISIBILITY_REFRESH_INTERVAL`]; partitioned from
/// `update_cloak_visibility` via `Without<Cloaked>` so neither
/// system writes `Visibility` on the same entity.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn update_fog_visibility(
    time: Res<Time>,
    mut timer: ResMut<FogRefreshTimer>,
    fog: Res<FogEnabled>,
    player: Res<LocalTeam>,
    alliances: Res<Alliances>,
    unit_registry: Res<UnitRegistry>,
    spatial: Res<crate::units::spatial::SpatialIndex>,
    // A building under construction has no sight (`LosHandler.cpp:173`).
    viewers_q: Query<
        (&TeamId, &UnitType, &GlobalTransform),
        (
            Without<Dying>,
            Without<crate::units::lifecycle::spawning::Emerging>,
        ),
    >,
    mut targets_q: Query<
        (
            Entity,
            &TeamId,
            &GlobalTransform,
            &mut Visibility,
            Has<Spotted>,
        ),
        (With<UnitType>, Without<Cloaked>, Without<Dying>),
    >,
    mut commands: Commands,
    // Retained across frames so the refresh tick doesn't reallocate.
    mut in_sight: Local<EntityHashSet>,
) {
    timer.0 += time.delta_secs();
    if timer.0 < VISIBILITY_REFRESH_INTERVAL {
        return;
    }
    timer.0 = 0.0;

    // Sandbox mode: every unit visible + spotted, no enemy distinction.
    // Keeping the same loop (vs. just early-returning) ensures the
    // `Spotted` marker is set on units that spawned while fog was
    // disabled, so minimap / HUD filters keyed off `Spotted` light up.
    if !fog.0 {
        for (entity, _, _, mut vis, was_spotted) in &mut targets_q {
            if *vis != Visibility::Visible {
                *vis = Visibility::Visible;
            }
            if !was_spotted {
                commands.entity(entity).insert(Spotted);
            }
        }
        return;
    }

    // Invert the scan: instead of testing every target against every
    // viewer (O(viewers × targets) at the refresh cadence), radius-query
    // the shared spatial index once per viewer and mark the enemy
    // entities that are in sight. Viewer positions come from the
    // Simulate-head snapshot — at most one sim tick stale, invisible at
    // a 10 Hz visibility refresh.
    in_sight.clear();
    viewers_q
        .iter()
        // Shared vision: every allied owner's units see for the player.
        .filter(|(team, _, _)| alliances.friendly(team.0, player.0))
        .map(|(_, ut, gtf)| (gtf.translation(), unit_registry.sight_distance(ut.0)))
        .for_each(|(vp, sight)| {
            spatial.query_radius(vp, sight, |candidate| {
                if !alliances.friendly(candidate.team, player.0)
                    && candidate.pos.distance_squared(vp) <= sight * sight
                {
                    in_sight.insert(candidate.entity);
                }
            });
        });

    for (entity, team, _gtf, mut vis, was_spotted) in &mut targets_q {
        // Friendly units always visible to the player; their Spotted
        // marker stays in sync so the minimap shows them.
        if alliances.friendly(team.0, player.0) {
            if *vis != Visibility::Visible {
                *vis = Visibility::Visible;
            }
            if !was_spotted {
                commands.entity(entity).insert(Spotted);
            }
            continue;
        }

        let in_sight = in_sight.contains(&entity);

        if in_sight {
            if !was_spotted {
                commands.entity(entity).insert(Spotted);
            }
            if *vis != Visibility::Visible {
                *vis = Visibility::Visible;
            }
        } else {
            if was_spotted {
                commands.entity(entity).remove::<Spotted>();
            }
            if *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::assets::animation::PieceIndex;
    use crate::units::assets::model_fade::ModelRoot;
    use bevy::ecs::system::RunSystemOnce;

    /// Fog is for a seated player in play: not a spectator, not an
    /// owner who left, until it comes back.
    #[test]
    fn fog_applies_to_a_seated_player_in_play() {
        let mut fates = TeamFates::default();
        assert!(fog_applies(LocalTeam(1), &fates));
        assert!(!fog_applies(
            LocalTeam(crate::game_setup::SPECTATOR_TEAM),
            &fates
        ));
        fates.leave(1);
        assert!(!fog_applies(LocalTeam(1), &fates), "defeated: full view");
        assert!(fog_applies(LocalTeam(0), &fates), "someone else left");
        fates.rejoin(1);
        assert!(fog_applies(LocalTeam(1), &fates));
    }

    /// Under fog a hostile unit is hidden the tick it appears; a friend
    /// is not touched, and with fog off neither is.
    #[test]
    fn a_new_hostile_starts_hidden_under_fog() {
        use crate::units::content::definitions::UnitKind;
        let mut world = World::new();
        world.insert_resource(FogEnabled(true));
        world.insert_resource(LocalTeam(0));
        world.insert_resource(Alliances::from_seats([(0, 0), (1, 1)]));
        let enemy = world
            .spawn((UnitType(UnitKind::Bit), TeamId(1), Visibility::default()))
            .id();
        let friend = world
            .spawn((UnitType(UnitKind::Bit), TeamId(0), Visibility::default()))
            .id();
        world.run_system_once(hide_new_hostiles).unwrap();
        assert_eq!(*world.get::<Visibility>(enemy).unwrap(), Visibility::Hidden);
        assert_eq!(
            *world.get::<Visibility>(friend).unwrap(),
            Visibility::default()
        );

        let mut world = World::new();
        world.insert_resource(FogEnabled(false));
        world.insert_resource(LocalTeam(0));
        world.insert_resource(Alliances::from_seats([(0, 0), (1, 1)]));
        let enemy = world
            .spawn((UnitType(UnitKind::Bit), TeamId(1), Visibility::default()))
            .id();
        world.run_system_once(hide_new_hostiles).unwrap();
        assert_eq!(
            *world.get::<Visibility>(enemy).unwrap(),
            Visibility::default()
        );
    }

    /// Fog on: an enemy beyond every friendly unit's `SightDistance` is
    /// hidden and unspotted, one inside is visible and spotted, and a
    /// friend is always both.
    #[test]
    fn fog_hides_what_no_friendly_unit_sees() {
        use crate::units::content::definitions::UnitKind;
        use crate::units::spatial::{SpatialEntry, SpatialIndex};
        let mut world = World::new();
        world.init_resource::<Time>();
        world.insert_resource(FogRefreshTimer(VISIBILITY_REFRESH_INTERVAL));
        world.insert_resource(FogEnabled(true));
        world.insert_resource(LocalTeam(0));
        world.insert_resource(Alliances::from_seats([(0, 0), (1, 1)]));
        world.insert_resource(UnitRegistry::load());
        let mut spatial = SpatialIndex::default();
        let mut unit = |world: &mut World, kind, team, x: f32| {
            let e = world
                .spawn((
                    UnitType(kind),
                    TeamId(team),
                    GlobalTransform::from_xyz(x, 0.0, 0.0),
                    Visibility::default(),
                ))
                .id();
            spatial.insert_for_test(SpatialEntry {
                entity: e,
                pos: Vec3::new(x, 0.0, 0.0),
                hit_radius: 10.0,
                mid_y: 0.0,
                up: Vec3::Y,
                team,
                kind,
                hp_positive: true,
                health: 1000.0,
                armored: false,
                is_flying: false,
                cloaked: false,
                detected_by: 0,
                los_by: u64::MAX,
                feature: false,
            });
            e
        };
        let kernel = unit(&mut world, UnitKind::Kernel, 0, 0.0);
        let sight = world
            .resource::<UnitRegistry>()
            .sight_distance(UnitKind::Kernel);
        assert!(sight > 0.0);
        let near = unit(&mut world, UnitKind::Bit, 1, sight - 10.0);
        let far = unit(&mut world, UnitKind::Bit, 1, sight + 100.0);
        world.insert_resource(spatial);
        world.run_system_once(update_fog_visibility).unwrap();
        world.flush();
        let state = |world: &World, e| {
            (
                *world.get::<Visibility>(e).unwrap(),
                world.get::<Spotted>(e).is_some(),
            )
        };
        assert_eq!(state(&world, kernel), (Visibility::Visible, true));
        assert_eq!(state(&world, near), (Visibility::Visible, true));
        assert_eq!(state(&world, far), (Visibility::Hidden, false));
    }

    fn world_with_cloaked_unit() -> (World, Entity, Entity, Handle<StandardMaterial>) {
        let mut world = World::new();
        world.init_resource::<Assets<StandardMaterial>>();
        world.init_resource::<crate::units::assets::model_fade::FadeMaterialCache>();
        world.insert_resource(FogEnabled(false));
        world.insert_resource(LocalTeam(0));
        world.init_resource::<Alliances>();
        let shared = world
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        let unit = world.spawn((TeamId(0), Cloaked)).id();
        let root = world.spawn((ModelRoot, ChildOf(unit))).id();
        let piece = world
            .spawn((PieceIndex, MeshMaterial3d(shared.clone()), ChildOf(root)))
            .id();
        (world, unit, piece, shared)
    }

    fn piece_alpha(world: &World, piece: Entity) -> f32 {
        let handle = &world
            .get::<MeshMaterial3d<StandardMaterial>>(piece)
            .unwrap()
            .0;
        world
            .resource::<Assets<StandardMaterial>>()
            .get(handle)
            .unwrap()
            .base_color
            .alpha()
    }

    /// The cloak look reaches the S3O pieces under the model root, waits
    /// for a running emerge fade, and comes off once `Cloaked` is gone.
    #[test]
    fn cloak_fade_waits_for_emerge_and_restores_on_decloak() {
        let (mut world, unit, piece, shared) = world_with_cloaked_unit();
        world.entity_mut(unit).insert(FadeMaterials {
            step: 0,
            overrides: Vec::new(),
        });
        world.run_system_once(install_cloak_fade_materials).unwrap();
        assert_eq!(
            piece_alpha(&world, piece),
            1.0,
            "emerge fade owns the pieces"
        );

        world.entity_mut(unit).remove::<FadeMaterials>();
        world.run_system_once(install_cloak_fade_materials).unwrap();
        assert_eq!(piece_alpha(&world, piece), CLOAK_FADE_ALPHA);
        assert!(world.get::<CloakFadeMaterials>(unit).is_some());

        world.entity_mut(unit).remove::<Cloaked>();
        world.run_system_once(restore_cloak_fade_materials).unwrap();
        let handle = &world
            .get::<MeshMaterial3d<StandardMaterial>>(piece)
            .unwrap()
            .0;
        assert_eq!(*handle, shared);
        assert!(world.get::<CloakFadeMaterials>(unit).is_none());
    }

    /// The engine rounds sight to 16-elmo and radar to 32-elmo squares.
    #[test]
    fn sensor_radii_follow_the_los_squares() {
        assert_eq!(los_radius(300.0), 288.0);
        assert_eq!(radar_radius(500.0), 480.0);
        assert_eq!(radar_radius(200.0), 192.0);
    }

    /// A radar blip stays within the error radius, is the same for
    /// every peer, and moves on each slow update.
    #[test]
    fn radar_blip_drifts_within_the_error_radius() {
        let at = Vec3::new(120.0, 4.0, 700.0);
        let blip = |frame| radar_blip(at, frame);
        for frame in [0u64, 14, 15, 90, 1000] {
            let off = blip(frame) - at;
            assert!(off.x.abs() <= RADAR_ERROR && off.z.abs() <= RADAR_ERROR);
            assert_eq!(off.y, 0.0);
            assert_eq!(blip(frame), blip(frame), "deterministic");
        }
        assert_eq!(blip(0), blip(14), "one offset per slow update");
        assert_ne!(blip(0), blip(15));
    }

    /// Who has a unit in sight and who on radar: sight reaches
    /// `los_radius`, a cloaked unit is never in an enemy's sight, an
    /// active radar covers what sight does not — cloaked or not — and a
    /// unit is always in its own team's sight.
    #[test]
    fn sight_and_radar_masks() {
        use crate::units::content::definitions::UnitKind;
        use crate::units::spatial::{SpatialEntry, SpatialIndex};
        let mut world = World::new();
        world.init_resource::<Time>();
        world.init_resource::<CloakDetectionTimer>();
        world.insert_resource(Alliances::from_seats([(0, 0), (1, 1)]));
        let registry = UnitRegistry::load();
        let sight = los_radius(registry.sight_distance(UnitKind::Kernel));
        let asm_sight = los_radius(registry.sight_distance(UnitKind::Assembler));
        let asm_radar = radar_radius(registry.radar_distance(UnitKind::Assembler));
        assert!(sight > 0.0 && asm_radar > asm_sight + 60.0);
        let mut spatial = SpatialIndex::default();
        let mut unit = |world: &mut World, kind: UnitKind, team: u8, x: f32, cloaked: bool| {
            let mut e = world.spawn((
                UnitType(kind),
                TeamId(team),
                Transform::from_xyz(x, 0.0, 0.0),
            ));
            if cloaked {
                e.insert(Cloaked);
            }
            if kind.activates_radar() {
                e.insert(Detector {
                    radius: registry.radar_distance(kind),
                });
            }
            let e = e.id();
            spatial.insert_for_test(SpatialEntry {
                entity: e,
                pos: Vec3::new(x, 0.0, 0.0),
                hit_radius: 10.0,
                mid_y: 0.0,
                up: Vec3::Y,
                team,
                kind,
                hp_positive: true,
                health: 1000.0,
                armored: false,
                is_flying: false,
                cloaked,
                detected_by: 0,
                los_by: u64::MAX,
                feature: false,
            });
            e
        };
        let kernel = unit(&mut world, UnitKind::Kernel, 0, 0.0, false);
        let near = unit(&mut world, UnitKind::Bit, 1, sight - 20.0, false);
        let far = unit(&mut world, UnitKind::Bit, 1, sight + 200.0, false);
        let mine = unit(&mut world, UnitKind::LogicBomb, 1, 50.0, true);
        // An Assembler far away at x = 5000 with a plain enemy just
        // beyond its sight and a mine inside its radar.
        let asm = unit(&mut world, UnitKind::Assembler, 0, 5000.0, false);
        let blip = unit(&mut world, UnitKind::Bit, 1, 5000.0 + asm_sight + 60.0, false);
        let buried = unit(&mut world, UnitKind::LogicBomb, 1, 5000.0 + asm_sight + 60.0, true);
        world.insert_resource(registry);
        world.insert_resource(spatial);
        world.run_system_once(update_cloak_detection).unwrap();
        let masks = |world: &World, e| {
            let s = world.get::<SightedBy>(e).unwrap();
            (s.los & 1 != 0, s.radar & 1 != 0)
        };
        assert_eq!(masks(&world, kernel), (true, false), "own unit");
        assert_eq!(masks(&world, asm), (true, false), "own unit");
        assert_eq!(masks(&world, near), (true, false), "inside sight");
        assert_eq!(masks(&world, far), (false, false), "beyond sight");
        assert_eq!(masks(&world, mine), (false, false), "cloaked is never seen");
        assert_eq!(masks(&world, blip), (false, true), "radar only");
        assert_eq!(masks(&world, buried), (false, true), "radar shows the cloaked");
        assert!(world.get::<DetectedBy>(buried).unwrap().contains(0));
        assert!(!world.get::<DetectedBy>(mine).unwrap().contains(0));
        // The other team has its own unit in its own sight.
        assert!(world.get::<SightedBy>(near).unwrap().los & 2 != 0);
    }
}
