//! Mobile-builder construction pipeline.
//!
//! When the player picks a building on a constructor's command panel and
//! then a site on the map, a `BuildAt { kind, site }` command is queued
//! onto the builder (shift to append, plain click to replace). The
//! movement system walks the builder toward the site; this module takes
//! over once the unit is within its FBI `BuildDistance`
//! (`CBuilderCAI::ExecuteBuildCmd`):
//!
//! 1. A site another building already stands on (finished or a
//!    nanoframe) ends the order: Kernel Panic's constructors are
//!    `CanAssist=0`, so `IsBuildPosBlocked` finishes the command and the
//!    builder moves on to its next one. Checked on the way, too.
//! 2. Mobile units on the footprint hold the start: allied idle ones are
//!    told to move off ([`BuggerOffRequests`], `BuggerOffRectangle`),
//!    and the builder gives up after [`BUILD_WAIT_GIVE_UP`].
//! 3. Otherwise `MoveTarget` / `MovePath` / `PendingBuild` are swapped
//!    for `Constructing` in one go, so the builder always carries an
//!    active order.
//! 4. [`tick_construction`] places the building on the first tick
//!    (its own script draws the build beams) as a [`BuildDriven`]
//!    nanoframe and adds the builder's build power to it each tick
//!    (`CUnit::AddBuildPower`); when it completes, `Constructing` is
//!    removed.
//! 5. A nanoframe nobody builds — its builder stunned, killed or given
//!    another order — decays and vanishes ([`decay_nanoframes`],
//!    `modrules.tdf [CONSTRUCTION]`). Factory buildees follow the same
//!    rules (`production_system` drives them).
//!
//! Construction ends — completed, refused by a unit cap, the frame gone,
//! or cancelled by a new order / Stop (`interaction::clear_active_order`,
//! Spring's `StopBuild`) — by removing `Constructing` alone. `Constructing`
//! and `PendingBuild` count as active orders, so on the next tick
//! without one [`advance_command_queue`] promotes the next queued
//! command (Spring's `FinishCommand`): this module never touches
//! `CommandQueue`.
//!
//! [`advance_command_queue`]: crate::interaction::movement::advance_command_queue

use bevy::platform::collections::HashMap;

use bevy::prelude::*;

use super::production::{BuggerOff, BuggerOffRequests, PendingFadeInstall};
use super::spawning::{BuildDriven, EmergeStyle, Emerging, SpawnContext, spawn_unit_facing};
use crate::interaction::movement::{MovePath, MoveTarget, QueuedCommand};
use crate::net::NetId;
use crate::sim::{DetFloat, Heading, SQUARE_SIZE};
use crate::units::combat::{Dying, Stunned};
use crate::units::components::{Faction, TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::spatial::{SpatialIndex, flat_dist_sq};

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
    /// The building the constructor is welding into existence (Spring's
    /// `curBuild`). Populated on the first `tick_construction` frame
    /// after `start_construction` flips the constructor into this state.
    /// Its [`Emerging`] clock is the build progress, which only this
    /// builder advances ([`BuildDriven`]).
    pub building: Option<Entity>,
}

/// Buildings the constructor `kind` can erect on a datavent. Pulled from
/// upstream `[CANBUILD]` in `SIDEDATA.TDF`. The command panel lists these
/// as the constructor's build options.
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

/// How long a builder in range waits for mobile units to clear the
/// footprint before giving up on the order: `CBuilderCAI::
/// ExecuteBuildCmd` retries `StartBuild` once a SlowUpdate (16 frames)
/// and finishes the command after 30 tries.
pub const BUILD_WAIT_GIVE_UP: f32 = 30.0 * 16.0 / crate::sim::GAME_SPEED;

/// A builder in range of its site whose footprint mobile units still
/// stand on. Dropped when construction starts or the order ends; a
/// stale one (another site) is ignored.
#[derive(Component, Clone, Copy, Debug)]
#[component(storage = "SparseSet")]
pub struct BuildSiteWait {
    site: Vec3,
    /// Seconds spent waiting so far.
    waited: f32,
    /// Where the builder steps to when it stands on the footprint
    /// itself (`BuggerOffRectangle` excludes nobody, and the builder's
    /// own `NonMoving` moves it off).
    step_aside: Option<Vec3>,
}

/// A builder clearing a feature off its build site before it builds
/// (`CBuilderCAI::ReclaimFeature`; the build order stays). Set by
/// [`start_construction`], worked by [`clear_features`].
#[derive(Component, Clone, Copy, Debug)]
#[component(storage = "SparseSet")]
pub struct ClearingFeature {
    pub feature: Entity,
    /// Where it stands: what the builder's emitter aims at.
    pub at: Vec3,
}

/// Reach beyond a site's half-diagonal the feature scan covers: a 2×2
/// feature's half-diagonal, with margin.
const FEATURE_FOOTPRINT_REACH: f32 = 32.0;

/// Whether two XZ footprints (centre, half extents) overlap. Touching
/// edges don't count — Spring tests whole squares.
pub(crate) fn footprints_overlap(a: Vec2, a_half: Vec2, b: Vec2, b_half: Vec2) -> bool {
    (a.x - b.x).abs() < a_half.x + b_half.x - 0.01 && (a.y - b.y).abs() < a_half.y + b_half.y - 0.01
}

/// What a Shift-queued build order did to a builder's orders
/// (`CCommandAI::GiveAllowedCommand`, see [`queue_build`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueuedBuild {
    /// Appended to the queue.
    Queued,
    /// It landed on build orders the builder already has, which are
    /// cancelled instead (`CancelCommands`); `active` when the running
    /// one was among them.
    Cancelled { active: bool },
    /// Refused: it would partly overlap a build the builder already
    /// has (`GetOverlapQueued`).
    Refused,
}

/// Give a builder a Shift-queued `BuildAt { kind, site }` against its
/// running build order `active` (walking there or building) and its
/// `queue`, as `CCommandAI::GiveAllowedCommand` does with the build
/// command at the end of `commandQue`:
///
/// 1. `CancelCommands` → `GetCancelQueued`: every build order of any
///    kind whose centre lies within half the larger footprint of the
///    new one (`|Δx|·2 <= max(xsize)·SQUARE_SIZE`, same in z) is
///    removed and the new order dropped — Shift-placing on one of your
///    own queued sites un-queues it. If the running order was among
///    them, `active` is cleared and the caller stops it (the engine
///    pushes a `CMD_STOP` in front: `StopBuild`, then the next command).
/// 2. `GetOverlapQueued`: a site that would partly overlap one of the
///    builder's build orders is refused.
/// 3. Otherwise the order is appended.
///
/// `footprint` gives a kind's footprint in elmos.
pub(crate) fn queue_build(
    active: &mut Option<(UnitKind, Vec3)>,
    queue: &mut Vec<QueuedCommand>,
    kind: UnitKind,
    site: Vec3,
    footprint: impl Fn(UnitKind) -> Vec2,
) -> QueuedBuild {
    let size = footprint(kind);
    // Twice the centre distance, the larger and the summed footprint.
    let measure = |other: UnitKind, at: Vec3| {
        let other_size = footprint(other);
        (
            (site.xz() - at.xz()).abs() * 2.0,
            size.max(other_size),
            size + other_size,
        )
    };
    let cancels = |other: UnitKind, at: Vec3| {
        let (d2, max, _) = measure(other, at);
        d2.x <= max.x && d2.y <= max.y
    };
    let cancel_active = active.is_some_and(|(k, at)| cancels(k, at));
    let before = queue.len();
    queue.retain(|c| !matches!(*c, QueuedCommand::BuildAt { kind: k, site: at } if cancels(k, at)));
    if cancel_active || queue.len() != before {
        if cancel_active {
            *active = None;
        }
        return QueuedBuild::Cancelled {
            active: cancel_active,
        };
    }
    let overlaps = |other: UnitKind, at: Vec3| {
        let (d2, max, add) = measure(other, at);
        (d2.x > max.x || d2.y > max.y) && d2.x < add.x && d2.y < add.y
    };
    let queued_builds = queue.iter().filter_map(|c| match *c {
        QueuedCommand::BuildAt { kind, site } => Some((kind, site)),
        _ => None,
    });
    if active
        .iter()
        .copied()
        .chain(queued_builds)
        .any(|(k, at)| overlaps(k, at))
    {
        return QueuedBuild::Refused;
    }
    queue.push(QueuedCommand::BuildAt { kind, site });
    QueuedBuild::Queued
}

/// Promote `PendingBuild` to `Constructing` once the builder is close
/// enough to its site and the footprint is clear (`CBuilderCAI::
/// ExecuteBuildCmd` → `CBuilder::StartBuild`). Runs after the motion
/// systems, which walk the builder there.
///
/// - A site another building (finished or a nanoframe) overlaps ends
///   the order, checked all the way there: Kernel Panic's constructors
///   can't assist (`CanAssist=0`), so `IsBuildPosBlocked` finishes the
///   command. Two builders arriving on the same tick count too.
/// - In range, the builder stops; a team at its unit limit waits there
///   with the order kept (`AtUnitLimit`). Mobile units on the footprint hold
///   the start (`TestUnitBuildSquare` → `BUILDSQUARE_OCCUPIED`): allied
///   idle ones are told to move off, the builder steps off its own
///   site, and after [`BUILD_WAIT_GIVE_UP`] the order is dropped.
/// - A stunned builder does nothing (`CUnit::SlowUpdate` skips the CAI).
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn start_construction(
    time: Res<Time>,
    mut commands: Commands,
    pending_q: Query<
        (
            Entity,
            &Transform,
            &UnitType,
            &TeamId,
            &PendingBuild,
            Option<&BuildSiteWait>,
            &NetId,
        ),
        (Without<Constructing>, Without<Dying>, Without<Stunned>),
    >,
    constructing_q: Query<&Constructing>,
    structures: Query<(&UnitType, &Transform), Without<Dying>>,
    features: Query<&crate::units::features::Feature>,
    spatial: Res<SpatialIndex>,
    registry: Res<UnitRegistry>,
    mut bugger_off: ResMut<BuggerOffRequests>,
    unit_counts: Res<super::bookkeeping::TeamUnitCounts>,
) {
    let dt = time.delta_secs();
    let half_of = |kind: UnitKind| registry.footprint_elmos(kind) * 0.5;
    // Footprints already claimed by a builder: running constructions,
    // plus the ones this pass starts (their inserts are deferred).
    let mut claimed: Vec<(Vec2, Vec2)> = constructing_q
        .iter()
        .map(|c| (c.site.xz(), half_of(c.kind)))
        .collect();

    // Unit-id order: the first builder claims a contested site (`sim`).
    for (entity, transform, builder_kind, team, pending, wait, _) in
        pending_q.iter().sort::<&NetId>()
    {
        let site = pending.site.xz();
        let half = half_of(pending.kind);
        let taken = claimed
            .iter()
            .any(|&(c, h)| footprints_overlap(site, half, c, h))
            || structures.iter().any(|(u, tf)| {
                (registry.is_building(u.0) || u.0.is_building())
                    && footprints_overlap(site, half, tf.translation.xz(), half_of(u.0))
            });
        if taken {
            // `StopMoveAndFinishCommand`.
            commands
                .entity(entity)
                .remove::<(PendingBuild, MoveTarget, MovePath, BuildSiteWait)>();
            continue;
        }
        // `MoveInBuildRange`: FBI `BuildDistance` plus the building's
        // model radius, in the plane.
        let range = registry.build_range(builder_kind.0, pending.kind);
        if flat_dist_sq(transform.translation, pending.site) > range * range {
            continue;
        }
        if unit_counts.at_limit(team.0) {
            // `AtUnitLimit`: wait in range, order kept, until a unit of
            // the team dies.
            commands.entity(entity).remove::<(MoveTarget, MovePath)>();
            continue;
        }

        // `TestUnitBuildSquare` → a blocking feature on the footprint:
        // the building it would turn into anyway (a Bad Block on a Bad
        // Block) ends the order; a clearable one is cleared away first
        // (`ReclaimFeature`, the order kept); any other blocks the site.
        let mut feature_on_site = None;
        spatial.query_radius(
            pending.site,
            half.length() + FEATURE_FOOTPRINT_REACH,
            |other| {
                if feature_on_site.is_some() || !other.feature {
                    return;
                }
                let Ok(feature) = features.get(other.entity) else {
                    return;
                };
                let feature_half = Vec2::new(
                    feature.def.footprint_x as f32,
                    feature.def.footprint_z as f32,
                ) * SQUARE_SIZE;
                if feature.def.blocking
                    && footprints_overlap(site, half, other.pos.xz(), feature_half)
                {
                    feature_on_site = Some((other.entity, *feature, other.pos));
                }
            },
        );
        if let Some((feature_entity, feature, at)) = feature_on_site {
            let mut ec = commands.entity(entity);
            if registry.finished_feature(pending.kind) == Some(feature.def.name.as_str())
                || !feature.def.clearable
            {
                ec.remove::<(PendingBuild, MoveTarget, MovePath, BuildSiteWait)>();
            } else {
                ec.insert(ClearingFeature {
                    feature: feature_entity,
                    at,
                })
                .remove::<(MoveTarget, MovePath, BuildSiteWait)>();
            }
            continue;
        }

        // Mobile units standing on the footprint (their own footprints,
        // as on the ground-blocking map).
        let own_half = half_of(builder_kind.0);
        let self_inside = footprints_overlap(site, half, transform.translation.xz(), own_half);
        let mut occupied = false;
        spatial.query_radius(
            pending.site,
            half.length() + MOBILE_FOOTPRINT_REACH,
            |other| {
                if occupied
                    || other.entity == entity
                    || other.is_flying
                    || !other.hp_positive
                    || registry.speed(other.kind) <= 0.0
                {
                    return;
                }
                occupied = footprints_overlap(site, half, other.pos.xz(), half_of(other.kind));
            },
        );

        let mut ec = commands.entity(entity);
        if occupied || self_inside {
            let (waited, mut step_aside) = match wait {
                Some(w) if w.site == pending.site => (w.waited + dt, w.step_aside),
                _ => (0.0, None),
            };
            if waited > BUILD_WAIT_GIVE_UP {
                ec.remove::<(PendingBuild, MoveTarget, MovePath, BuildSiteWait)>();
                continue;
            }
            if occupied {
                bugger_off.push(BuggerOff::rectangle(pending.site, half, team.0, entity));
            }
            if self_inside {
                // One step, far enough that the footprint is clear when
                // it ends: the finished leg only stops the builder
                // (`end_move_leg` keeps the build order), and it doesn't
                // step again for this wait.
                if step_aside.is_none() {
                    let away = (transform.translation - pending.site).with_y(0.0);
                    let dir = away.try_normalize().unwrap_or(Vec3::X);
                    let target = pending.site
                        + dir * (half.length() + own_half.length() + 2.0 * SQUARE_SIZE);
                    ec.insert(MoveTarget(target)).remove::<MovePath>();
                    step_aside = Some(target);
                }
            } else {
                ec.remove::<(MoveTarget, MovePath)>();
            }
            ec.insert(BuildSiteWait {
                site: pending.site,
                waited,
                step_aside,
            });
            continue;
        }

        claimed.push((site, half));
        ec.insert(Constructing {
            kind: pending.kind,
            site: pending.site,
            building: None,
        })
        .remove::<(MoveTarget, MovePath, PendingBuild, BuildSiteWait)>();
    }
}

/// Clear the features off build sites (`CBuilder::UpdateReclaim`):
/// once its emitter is turned onto the feature (`INBUILDSTANCE`), a
/// builder takes `WorkerTime / reclaimTime` of it per second, and the
/// feature is gone when all of it is — no `featureDead` left
/// (`DeleteFeature`), nothing gained (Kernel Panic has no metal). One
/// builder works a feature per frame (`multiReclaim=0`), the first in
/// id order. A builder whose build order went, or whose feature did,
/// stops.
#[allow(clippy::type_complexity)]
pub fn clear_features(
    time: Res<Time>,
    builders: Query<
        (
            Entity,
            &UnitType,
            &ClearingFeature,
            Has<PendingBuild>,
            Option<&crate::units::assets::animation::UnitAnimator>,
            &NetId,
        ),
        (Without<Dying>, Without<Stunned>),
    >,
    mut features: Query<(
        &crate::units::features::Feature,
        Option<&mut crate::units::features::ClearProgress>,
    )>,
    registry: Res<UnitRegistry>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    let mut worked: Vec<Entity> = Vec::new();
    for (builder, kind, clearing, pending, animator, _) in builders.iter().sort::<&NetId>() {
        let Ok((feature, progress)) = features.get_mut(clearing.feature) else {
            commands.entity(builder).remove::<ClearingFeature>();
            continue;
        };
        if !pending {
            commands.entity(builder).remove::<ClearingFeature>();
            continue;
        }
        if animator.and_then(|a| a.driver.in_build_stance()) == Some(false)
            || worked.contains(&clearing.feature)
        {
            continue;
        }
        worked.push(clearing.feature);
        let step = registry.worker_time(kind.0) * dt / feature.def.clear_time;
        let cleared = progress.map_or(0.0, |p| p.0) + step;
        if cleared >= 1.0 {
            commands.entity(clearing.feature).despawn();
            commands.entity(builder).remove::<ClearingFeature>();
        } else {
            commands
                .entity(clearing.feature)
                .insert(crate::units::features::ClearProgress(cleared));
        }
    }
}

/// Reach beyond a site's half-diagonal the occupancy scan covers: the
/// largest mobile footprint's half-diagonal, with margin.
const MOBILE_FOOTPRINT_REACH: f32 = 48.0;

/// Advance every construction (`CBuilder::UpdateBuild` →
/// `CUnit::AddBuildPower`). The first tick places the building — its
/// rise/fade and the builder's beams follow from there — as a
/// [`BuildDriven`] nanoframe whose [`Emerging`] clock is the build
/// progress. Each later tick the builder adds `WorkerTime / BuildTime`
/// of it per second; when the clock runs out, or the building is gone
/// (decayed, killed), `Constructing` is removed and the order system
/// advances the queue.
///
/// A stunned builder adds nothing (`CBuilder::Update` skips
/// `UpdateBuild`), so its nanoframe starts to decay
/// ([`decay_nanoframes`]); so does one whose builder took a new order.
///
/// A kind with a per-team cap (`UnitRestricted`: Logic Bombs) is
/// refused — construction cancelled before the nanoframe appears — when
/// the team already fields the cap, as Spring refuses the build order.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn tick_construction(
    time: Res<Time>,
    // A dead builder's CAI never runs again (`CUnit::Update` skips the
    // dead), and a dead frame takes no build power (`AddBuildPower`
    // returns false): it ends the build like a vanished one.
    mut builders: Query<
        (
            Entity,
            &GlobalTransform,
            &mut Transform,
            &UnitType,
            &Faction,
            &TeamId,
            &mut Constructing,
            Option<&crate::units::assets::animation::UnitAnimator>,
            Has<Stunned>,
            &NetId,
        ),
        Without<Dying>,
    >,
    mut frames: Query<(&mut Emerging, &mut BuildDriven), Without<Dying>>,
    live_units: Query<(&UnitType, &TeamId), Without<Dying>>,
    vents: Query<&crate::terrain::geovent::GeoventSmoker>,
    mut ctx: SpawnContext,
    mut hex_farm: Option<ResMut<crate::map_events::hex_farm::HexFarmInbox>>,
) {
    let dt = time.delta_secs();
    // Lazily-built per-(kind, team) census for capped kinds, bumped as
    // this pass starts nanoframes so two builders starting on the same
    // frame can't both slip under the cap.
    let mut capped_counts: HashMap<UnitKind, std::collections::BTreeMap<u8, u32>> = HashMap::new();

    for (
        entity,
        gtf,
        mut transform,
        builder_kind,
        faction,
        team,
        mut constructing,
        animator,
        stunned,
        _,
    ) in builders.iter_mut().sort::<&NetId>()
    {
        if stunned {
            continue;
        }
        let raw_build_time = ctx.unit_registry.raw_build_time(constructing.kind);
        let build_time = ctx.unit_registry.build_time(constructing.kind);

        match constructing.building {
            None => {
                if let Some(limit) = ctx.unit_registry.team_limit(constructing.kind) {
                    let kind = constructing.kind;
                    let counts = capped_counts.entry(kind).or_insert_with(|| {
                        crate::units::lifecycle::bookkeeping::team_kind_counts(kind, &live_units)
                    });
                    let count = counts.entry(team.0).or_default();
                    if *count >= limit {
                        ctx.commands.entity(entity).remove::<Constructing>();
                        continue;
                    }
                    *count += 1;
                }
                // A degenerate build time would make a frozen emerge
                // with `total = 0` that divides by zero downstream.
                if build_time <= 0.0 {
                    ctx.commands.entity(entity).remove::<Constructing>();
                    continue;
                }
                // The building stands at its final height from the
                // first frame (no nanoframe drawn in KP); its own
                // script sinks and raises pieces with
                // `BUILD_PERCENT_LEFT`.
                let style = faction.emerge_style();
                // A factory on a datavent fronts the vent's baked facing
                // (`spring_map::datavent`); anything else keeps the
                // engine's default, south.
                let heading = vents
                    .iter()
                    .find(|v| v.pos.distance_squared(constructing.site) < 1.0)
                    .map_or(Heading::default(), |v| Heading::from_facing(v.facing));
                let new_entity = spawn_unit_facing(
                    constructing.kind,
                    *faction,
                    team.0,
                    constructing.site,
                    heading,
                    &mut ctx,
                );
                ctx.commands.entity(new_entity).insert((
                    Emerging {
                        target_y: constructing.site.y,
                        remaining: build_time,
                        total: build_time,
                        rally_point: None,
                        rally_then: None,
                        style,
                    },
                    BuildDriven::default(),
                ));
                if matches!(style, EmergeStyle::Fade) {
                    ctx.commands.entity(new_entity).insert(PendingFadeInstall);
                }
                constructing.building = Some(new_entity);
            }
            Some(building) => {
                let Ok((mut clock, mut driven)) = frames.get_mut(building) else {
                    // Finished, decayed or destroyed: the builder's
                    // `DependentDied` / `FinishCommand`.
                    ctx.commands.entity(entity).remove::<Constructing>();
                    continue;
                };
                // Keeping the frame alive is the builder's presence
                // (`AddBuildPower(this, 0)` semantics); progress waits
                // for the script's `INBUILDSTANCE`, as the engine's
                // `StartBuild` does before the nanoframe exists.
                driven.unpowered = 0.0;
                let in_stance = animator.is_none_or(|a| a.driver.in_build_stance() != Some(false));
                if in_stance && raw_build_time > 0.0 {
                    // Fraction of the build this tick, in the frame's
                    // clock seconds.
                    let step = (ctx.unit_registry.worker_time(builder_kind.0) * dt
                        / raw_build_time)
                        .det_min(clock.remaining / clock.total.det_max(f32::EPSILON));
                    clock.remaining = (clock.remaining - step * clock.total).det_max(0.0);
                    // `AllowUnitBuildStep` for Hex Farm: this tick's
                    // share of the build, in `buildTime` points.
                    if let Some(inbox) = hex_farm.as_deref_mut() {
                        inbox.build_step(constructing.site, step * raw_build_time);
                    }
                }
                if clock.remaining <= 0.0 {
                    // `emerge_system` finishes the building this tick.
                    ctx.commands.entity(entity).remove::<Constructing>();
                    continue;
                }
            }
        }

        // Pin the builder's yaw to face the build site so the beam leaves
        // the muzzle piece forward rather than out of the unit's hip —
        // mirrors the way a mobile constructor aims at a ghost before
        // committing. The build beams are the builder's own script's
        // (`emit-sfx 2048` from its emitter once `StartBuilding` has
        // aimed it).
        if let Some(forward) = forward_toward_site(gtf.translation(), constructing.site) {
            transform.rotation = Transform::from_translation(transform.translation)
                .looking_to(forward, Vec3::Y)
                .rotation;
        }
    }
}

/// Kernel Panic's `modrules.tdf [CONSTRUCTION]`: a nanoframe nobody
/// adds build power to for longer than `constructionDecayTime` = 0.1 s
/// starts to decay.
const DECAY_DELAY: f32 = 0.1;
/// `constructionDecaySpeed` = 0.001, which `CModInfo` raises to its
/// floor of 0.01 (ModInfo.cpp soft constraints).
const DECAY_SPEED: f32 = 0.01;
/// `CUnit::SlowUpdate` cadence (`UNIT_SLOWUPDATE_RATE` frames), at
/// which the decay steps.
const SLOW_UPDATE_RATE: f32 = 15.0;

/// `CUnit::SlowUpdate`'s construction decay for every [`BuildDriven`]
/// nanoframe, mobile builders' and factories' alike: once
/// [`DECAY_DELAY`] has passed without build power, each SlowUpdate
/// takes `1 / (buildTime · constructionDecaySpeed)` of the build away
/// (applied smoothly here), and a frame back at zero is killed as
/// reclaimed — despawned, no death blast. A Bad Block's frame is gone in
/// about a second, a Socket's in about twenty.
pub fn decay_nanoframes(
    time: Res<Time>,
    mut frames: Query<(Entity, &UnitType, &mut Emerging, &mut BuildDriven)>,
    registry: Res<UnitRegistry>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (entity, kind, mut clock, mut driven) in &mut frames {
        driven.unpowered += dt;
        if driven.unpowered <= DECAY_DELAY {
            continue;
        }
        let raw_build_time = registry.raw_build_time(kind.0);
        let per_second = crate::sim::GAME_SPEED
            / SLOW_UPDATE_RATE
            / (raw_build_time * DECAY_SPEED).det_max(0.001);
        clock.remaining = (clock.remaining + per_second * dt * clock.total).det_min(clock.total);
        if clock.remaining >= clock.total {
            commands.entity(entity).try_despawn();
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

    // --- construction lifecycle ------------------------------------------

    use crate::units::content::weapons::WeaponRegistry;
    use crate::units::lifecycle::bookkeeping::TeamUnitCounts;
    use crate::units::spatial::SpatialEntry;
    use bevy::ecs::system::RunSystemOnce;
    use spring_tdf::{UnitDef, UnitDefs};
    use std::time::Duration;

    /// Assembler (`WorkerTime=128`, `BuildDistance=384`), Bad Block (`BuildTime=160`, 2×2
    /// footprint = 32 elmos) and a Bit, from the upstream FBIs.
    fn registry() -> UnitRegistry {
        let mut defs = UnitDefs::default();
        let mut add = |name: &str, def: UnitDef| {
            defs.units.insert(name.into(), def);
        };
        add(
            "assembler",
            UnitDef {
                worker_time: 128.0,
                build_distance: 384.0,
                build_time: 1600.0,
                footprint_x: 2.0,
                footprint_z: 2.0,
                max_velocity: 2.0,
                can_move: true,
                ..UnitDef::default()
            },
        );
        add(
            "badblock",
            UnitDef {
                build_time: 160.0,
                footprint_x: 2.0,
                footprint_z: 2.0,
                is_feature: true,
                corpse: "badblock".into(),
                ..UnitDef::default()
            },
        );
        add(
            "bit",
            UnitDef {
                build_time: 240.0,
                footprint_x: 1.0,
                footprint_z: 1.0,
                max_velocity: 3.0,
                can_move: true,
                ..UnitDef::default()
            },
        );
        UnitRegistry::for_test(defs)
    }

    fn world() -> World {
        let mut world = World::new();
        world.insert_resource(registry());
        world.init_resource::<SpatialIndex>();
        world.init_resource::<crate::units::teams::Alliances>();
        world.init_resource::<BuggerOffRequests>();
        world.init_resource::<TeamUnitCounts>();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<StandardMaterial>>();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<crate::units::assets::meshes::S3OModelCache>();
        world.init_resource::<WeaponRegistry>();
        world.insert_resource(Time::<()>::default());
        world
    }

    fn advance(world: &mut World, secs: f32) {
        world
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs_f32(secs));
    }

    fn assembler(world: &mut World, pos: Vec3, site: Vec3) -> Entity {
        world
            .spawn((
                UnitType(UnitKind::Assembler),
                TeamId(0),
                Faction::System,
                Transform::from_translation(pos),
                GlobalTransform::from_translation(pos),
                PendingBuild {
                    kind: UnitKind::BadBlock,
                    site,
                },
                MoveTarget(site),
            ))
            .id()
    }

    /// #16: several builders sent to one site build one building. Kernel
    /// Panic's constructors are `CanAssist=0`, so the later ones find
    /// the site blocked and drop the order (`IsBuildPosBlocked`).
    #[test]
    fn one_building_per_site_later_builders_drop_the_order() {
        let mut world = world();
        let site = Vec3::new(500.0, 0.0, 500.0);
        let a = assembler(&mut world, site + Vec3::X * 100.0, site);
        let b = assembler(&mut world, site - Vec3::X * 100.0, site);
        world.run_system_once(start_construction).unwrap();
        let started = [a, b]
            .iter()
            .filter(|e| world.get::<Constructing>(**e).is_some())
            .count();
        assert_eq!(started, 1, "exactly one builder starts");
        for e in [a, b] {
            assert!(world.get::<PendingBuild>(e).is_none());
            assert!(world.get::<MoveTarget>(e).is_none());
        }

        // A building already standing there ends a walking builder's
        // order long before it arrives.
        world.spawn((
            UnitType(UnitKind::BadBlock),
            Transform::from_translation(Vec3::new(900.0, 0.0, 900.0)),
        ));
        let c = assembler(&mut world, Vec3::ZERO, Vec3::new(910.0, 0.0, 900.0));
        world.run_system_once(start_construction).unwrap();
        assert!(world.get::<PendingBuild>(c).is_none());
        assert!(world.get::<Constructing>(c).is_none());
    }

    /// `GetCancelQueued`: a Shift-queued build on one of the builder's
    /// own build sites cancels every build order there (queued or
    /// running) instead of queueing; one that would partly overlap a
    /// queued build is refused (`GetOverlapQueued`); others queue.
    #[test]
    fn queued_build_cancels_or_refuses_like_the_command_ai() {
        // 32-elmo footprints (Bad Block, Socket), as the empty registry.
        let fp = |_: UnitKind| Vec2::splat(32.0);
        let at = |x: f32| Vec3::new(x, 0.0, 0.0);
        let bad = |x: f32| QueuedCommand::BuildAt {
            kind: UnitKind::BadBlock,
            site: at(x),
        };
        let sites = |q: &[QueuedCommand]| q.iter().map(|c| c.position().x).collect::<Vec<_>>();

        let mut active = Some((UnitKind::BadBlock, at(0.0)));
        let mut queue = vec![QueuedCommand::Move(at(500.0)), bad(100.0), bad(200.0)];
        // Within half a footprint of the queued site at 100, any kind.
        assert_eq!(
            queue_build(&mut active, &mut queue, UnitKind::LogicBomb, at(110.0), fp),
            QueuedBuild::Cancelled { active: false }
        );
        assert_eq!(sites(&queue), [500.0, 200.0]);
        assert!(active.is_some());
        // Partly overlapping the site at 200: refused, nothing changes.
        assert_eq!(
            queue_build(&mut active, &mut queue, UnitKind::BadBlock, at(225.0), fp),
            QueuedBuild::Refused
        );
        assert_eq!(sites(&queue), [500.0, 200.0]);
        // Clear of everything: queued.
        assert_eq!(
            queue_build(&mut active, &mut queue, UnitKind::BadBlock, at(300.0), fp),
            QueuedBuild::Queued
        );
        assert_eq!(sites(&queue), [500.0, 200.0, 300.0]);
        // On the running order's site: it is cancelled too.
        assert_eq!(
            queue_build(&mut active, &mut queue, UnitKind::BadBlock, at(0.0), fp),
            QueuedBuild::Cancelled { active: true }
        );
        assert!(active.is_none());
        assert_eq!(sites(&queue), [500.0, 200.0, 300.0]);
    }

    /// The builder stops and starts building once within its FBI
    /// `BuildDistance` (384 for the Assembler) of the site — measured
    /// in the plane (`buildRange3D` off) — not before.
    #[test]
    fn construction_starts_within_the_fbi_build_distance() {
        let mut world = world();
        let site = Vec3::new(1000.0, 0.0, 1000.0);
        let far = assembler(&mut world, site + Vec3::X * 390.0, site);
        // 380 away in the plane, far below the site: still in range.
        let near = assembler(&mut world, site + Vec3::new(0.0, -300.0, 380.0), site);
        world.run_system_once(start_construction).unwrap();
        assert!(world.get::<Constructing>(far).is_none(), "out of range");
        assert!(world.get::<PendingBuild>(far).is_some());
        assert!(world.get::<MoveTarget>(far).is_some(), "keeps walking");
        assert!(world.get::<Constructing>(near).is_some(), "in range");
        assert!(world.get::<MoveTarget>(near).is_none(), "stopped");
    }

    /// #26: a unit standing on the footprint holds the start instead of
    /// being buried; an allied idle one is told to move off, and the
    /// builder gives up after `BUILD_WAIT_GIVE_UP`.
    #[test]
    fn occupied_site_waits_and_buggers_off_allies() {
        let mut world = world();
        let site = Vec3::new(500.0, 0.0, 500.0);
        let builder = assembler(&mut world, site + Vec3::X * 100.0, site);
        let squatter = world
            .spawn((
                UnitType(UnitKind::Bit),
                TeamId(0),
                Transform::from_translation(site + Vec3::Z * 4.0),
            ))
            .id();
        world
            .resource_mut::<SpatialIndex>()
            .insert_for_test(SpatialEntry {
                entity: squatter,
                pos: site + Vec3::Z * 4.0,
                hit_radius: 8.0,
                mid_y: 0.0,
                team: 0,
                kind: UnitKind::Bit,
                hp_positive: true,
                is_flying: false,
                cloaked: false,
                detected_by: 0,
                feature: false,
            });
        advance(&mut world, 0.1);
        world.run_system_once(start_construction).unwrap();
        assert!(world.get::<Constructing>(builder).is_none(), "held");
        assert!(world.get::<PendingBuild>(builder).is_some(), "order kept");
        assert!(
            world.get::<MoveTarget>(builder).is_none(),
            "stopped in range"
        );
        assert!(world.get::<BuildSiteWait>(builder).is_some());

        world
            .run_system_once(super::super::production::apply_bugger_off)
            .unwrap();
        let goal = world.get::<MoveTarget>(squatter).expect("moved off").0;
        let half = Vec2::splat(16.0);
        assert!(
            !footprints_overlap(site.xz(), half, goal.xz(), Vec2::splat(8.0)),
            "the move ends clear of the footprint: {goal}"
        );

        world.get_mut::<BuildSiteWait>(builder).unwrap().waited = BUILD_WAIT_GIVE_UP;
        world.run_system_once(start_construction).unwrap();
        assert!(world.get::<PendingBuild>(builder).is_none(), "gave up");
        assert!(world.get::<BuildSiteWait>(builder).is_none());
    }

    /// A team at its unit limit keeps the order and waits (`AtUnitLimit`).
    #[test]
    fn unit_limit_holds_construction() {
        let mut world = world();
        let site = Vec3::new(500.0, 0.0, 500.0);
        let builder = assembler(&mut world, site + Vec3::X * 100.0, site);
        for _ in 0..crate::units::lifecycle::bookkeeping::MAX_UNITS_PER_TEAM {
            world.spawn((UnitType(UnitKind::Bit), TeamId(0)));
        }
        world
            .run_system_once(crate::units::lifecycle::bookkeeping::track_added_units)
            .unwrap();
        world.run_system_once(start_construction).unwrap();
        assert!(world.get::<Constructing>(builder).is_none());
        assert!(world.get::<PendingBuild>(builder).is_some());
    }

    /// #27: the builder's `WorkerTime` drives the frame; a stunned
    /// builder adds nothing; a frame nobody builds decays and vanishes,
    /// and the builder's `Constructing` then ends.
    #[test]
    fn builder_drives_the_frame_and_abandoned_frames_decay() {
        let mut world = world();
        let site = Vec3::new(500.0, 0.0, 500.0);
        // 160 build points at 128 a second: 1.25 s.
        let total = 160.0 / 128.0;
        let frame = world
            .spawn((
                UnitType(UnitKind::BadBlock),
                Emerging {
                    target_y: 0.0,
                    remaining: total,
                    total,
                    rally_point: None,
                    rally_then: None,
                    style: EmergeStyle::Rise,
                },
                BuildDriven::default(),
            ))
            .id();
        let builder = assembler(&mut world, site, site);
        world
            .entity_mut(builder)
            .remove::<(PendingBuild, MoveTarget)>()
            .insert(Constructing {
                kind: UnitKind::BadBlock,
                site,
                building: Some(frame),
            });
        let remaining = |world: &World| world.get::<Emerging>(frame).unwrap().remaining;

        // Six sim ticks (0.2 s).
        let ticks = |world: &mut World| {
            for _ in 0..6 {
                advance(world, 1.0 / 30.0);
                world.run_system_once(tick_construction).unwrap();
                world.run_system_once(decay_nanoframes).unwrap();
            }
        };
        for _ in 0..3 {
            ticks(&mut world);
        }
        assert!((remaining(&world) - (total - 0.6)).abs() < 1e-3);

        // Stunned: no build power, and past the 0.1 s grace it decays.
        world.entity_mut(builder).insert(Stunned { remaining: 5.0 });
        let before = remaining(&world);
        ticks(&mut world);
        assert!(
            remaining(&world) > before,
            "decays while the builder is stunned"
        );

        // Abandoned for good: a Bad Block frame decays away within about
        // a second (1 / (160 · 0.01) of the build per 15 frames).
        for _ in 0..8 {
            ticks(&mut world);
        }
        assert!(world.get_entity(frame).is_err(), "decayed away");
        world.entity_mut(builder).remove::<Stunned>();
        world.run_system_once(tick_construction).unwrap();
        assert!(world.get::<Constructing>(builder).is_none());
    }

    /// A dead builder adds no build power (its CAI never runs again), and
    /// a dead frame takes none (`AddBuildPower` refuses it): the build
    /// ends as if the frame were gone.
    #[test]
    fn dead_builders_and_frames_build_nothing() {
        let mut world = world();
        let site = Vec3::new(500.0, 0.0, 500.0);
        let emerging = || Emerging {
            target_y: 0.0,
            remaining: 1.0,
            total: 1.25,
            rally_point: None,
            rally_then: None,
            style: EmergeStyle::Rise,
        };
        let constructing = |frame| Constructing {
            kind: UnitKind::BadBlock,
            site,
            building: Some(frame),
        };
        // A killed builder's corpse adds no build power.
        let frame = world
            .spawn((
                UnitType(UnitKind::BadBlock),
                emerging(),
                BuildDriven::default(),
            ))
            .id();
        let corpse = assembler(&mut world, site, site);
        world
            .entity_mut(corpse)
            .remove::<(PendingBuild, MoveTarget)>()
            .insert((constructing(frame), Dying { timer: 1.0 }));
        // A builder of a killed frame stops building it.
        let dead_frame = world
            .spawn((
                UnitType(UnitKind::BadBlock),
                emerging(),
                BuildDriven::default(),
                Dying { timer: 1.0 },
            ))
            .id();
        let builder = assembler(&mut world, site, site);
        world
            .entity_mut(builder)
            .remove::<(PendingBuild, MoveTarget)>()
            .insert(constructing(dead_frame));
        advance(&mut world, 0.1);
        world.run_system_once(tick_construction).unwrap();
        assert_eq!(world.get::<Emerging>(frame).unwrap().remaining, 1.0);
        assert_eq!(world.get::<Emerging>(dead_frame).unwrap().remaining, 1.0);
        assert!(world.get::<Constructing>(builder).is_none());
    }

    /// A re-issued build order starts a fresh footprint wait: the old
    /// order's `BuildSiteWait` goes with it.
    #[test]
    fn a_new_order_drops_the_site_wait() {
        let mut world = world();
        let site = Vec3::new(500.0, 0.0, 500.0);
        let builder = assembler(&mut world, site, site);
        world.entity_mut(builder).insert(BuildSiteWait {
            site,
            waited: BUILD_WAIT_GIVE_UP - 0.1,
            step_aside: Some(Vec3::X),
        });
        world
            .run_system_once(move |mut commands: Commands| {
                crate::interaction::replace_order(
                    &mut commands.entity(builder),
                    QueuedCommand::BuildAt {
                        kind: UnitKind::BadBlock,
                        site,
                    },
                );
            })
            .unwrap();
        assert!(world.get::<BuildSiteWait>(builder).is_none());
        assert!(world.get::<PendingBuild>(builder).is_some());
    }

    /// The build ends the tick the builder completes it.
    #[test]
    fn completing_the_frame_ends_construction() {
        let mut world = world();
        let site = Vec3::new(500.0, 0.0, 500.0);
        let frame = world
            .spawn((
                UnitType(UnitKind::BadBlock),
                Emerging {
                    target_y: 0.0,
                    remaining: 0.01,
                    total: 1.25,
                    rally_point: None,
                    rally_then: None,
                    style: EmergeStyle::Rise,
                },
                BuildDriven::default(),
            ))
            .id();
        let builder = assembler(&mut world, site, site);
        world
            .entity_mut(builder)
            .remove::<(PendingBuild, MoveTarget)>()
            .insert(Constructing {
                kind: UnitKind::BadBlock,
                site,
                building: Some(frame),
            });
        advance(&mut world, 0.1);
        world.run_system_once(tick_construction).unwrap();
        assert_eq!(world.get::<Emerging>(frame).unwrap().remaining, 0.0);
        assert!(world.get::<Constructing>(builder).is_none());
    }

    /// A feature of `name` at `pos`, in the spatial index as features are.
    fn feature(world: &mut World, name: &str, pos: Vec3) -> Entity {
        let def = crate::units::content::defs::defs()
            .features
            .get(name)
            .unwrap();
        let entity = world
            .spawn((
                crate::units::features::Feature { def, owner: None },
                Transform::from_translation(pos),
            ))
            .id();
        world
            .resource_mut::<SpatialIndex>()
            .insert_for_test(SpatialEntry {
                entity,
                pos,
                hit_radius: 20.0,
                mid_y: 8.0,
                team: crate::units::features::FEATURE_TEAM,
                kind: UnitKind::BadBlock,
                hp_positive: true,
                is_flying: false,
                cloaked: false,
                detected_by: 0,
                feature: true,
            });
        entity
    }

    /// A builder whose site a feature stands on clears it away first,
    /// its order kept: `WorkerTime=128` against a Bad Block's
    /// `reclaimTime` 384 is 3 s. Then it builds there.
    #[test]
    fn a_feature_on_the_site_is_cleared_first() {
        let mut world = world();
        let site = Vec3::new(100.0, 0.0, 100.0);
        let builder = assembler(&mut world, Vec3::new(150.0, 0.0, 100.0), site);
        world.entity_mut(builder).insert((
            NetId(1),
            PendingBuild {
                kind: UnitKind::Bit,
                site,
            },
        ));
        let block = feature(&mut world, "badblock", site + Vec3::new(12.0, 0.0, 0.0));
        world.run_system_once(start_construction).unwrap();
        assert_eq!(
            world.get::<ClearingFeature>(builder).map(|c| c.feature),
            Some(block)
        );
        assert!(world.get::<Constructing>(builder).is_none());

        let mut frames = 0;
        while world.get_entity(block).is_ok() {
            advance(&mut world, 1.0 / 30.0);
            world.run_system_once(start_construction).unwrap();
            world.run_system_once(clear_features).unwrap();
            frames += 1;
            assert!(frames < 200, "never cleared");
        }
        assert_eq!(frames, 90);
        assert!(world.get::<ClearingFeature>(builder).is_none());
        world.run_system_once(start_construction).unwrap();
        assert_eq!(
            world.get::<Constructing>(builder).map(|c| c.kind),
            Some(UnitKind::Bit)
        );
    }

    /// A Bad Block ordered where a Bad Block feature already stands is
    /// done (`bi.def->wreckName == f->def->name`): the order ends and the
    /// feature stays.
    #[test]
    fn a_bad_block_on_a_bad_block_is_already_built() {
        let mut world = world();
        let site = Vec3::new(100.0, 0.0, 100.0);
        let builder = assembler(&mut world, Vec3::new(150.0, 0.0, 100.0), site);
        world.entity_mut(builder).insert(NetId(1));
        let block = feature(&mut world, "badblock", site);
        world.run_system_once(start_construction).unwrap();
        assert!(world.get::<PendingBuild>(builder).is_none());
        assert!(world.get::<ClearingFeature>(builder).is_none());
        assert!(world.get_entity(block).is_ok());
    }
}
