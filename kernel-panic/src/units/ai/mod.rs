//! Enemy AI — a port of upstream Kernel Panic's Lua AI
//! (`LuaRules/Gadgets/KPAI.lua`, in its `KPAI_Fair.lua` flavour),
//! ticking once per second for every non-local team that owns a
//! homebase.
//!
//! Each tick, per AI team:
//!
//! 1. **Fairness**: recompute Fair KPAI's `Lack` budget — how many more
//!    spam / medium / building units the team may own than its enemies
//!    ([`build_orders::Lack`]); [`AiDifficulty`] widens the head start.
//! 2. **Build**: refill each homebase with KPAI's `OrderHomeBase` mix
//!    (constructor odds fall with every constructor owned; big armies
//!    buy heavies/artillery; otherwise batches of spam). Minifacs
//!    autospam on repeat for every team (`production::Producer::spamming`,
//!    upstream `kp_autospam.lua`); the AI only switches that repeat
//!    off while its spam budget is spent (`KPAI_Fair::OrderMiniFac`).
//! 3. **Expand**: every idle constructor claims a free datavent
//!    (`GetNiceGeo` sampling) and builds its minifac there, or — with
//!    three minifacs owned — the faction special one time in three.
//!    Constructors still inside a homebase's exit box, or idling with
//!    the building budget spent, are spread on a ring around the base
//!    instead of clogging its spawn.
//! 4. **Army**: if enemy units come within `DEFEND_RADIUS` of a
//!    homebase, idle army units fight their way there; otherwise, once
//!    `ARMY_THRESHOLD` units idle at home, the army attack-moves at
//!    the nearest enemy minifac (or a weak team's homebase once it is
//!    big). Units idling in the field reinforce the push straight
//!    away. Big armies split across `1 + (force + buffer) * 2%`
//!    fronts, round-robin, like upstream's mission loop.
//! 5. **Network**: teleporters counter-dispatch packets at nearby
//!    attackers and dump a full buffer toward the push target.
//! 6. **Specials**: SIGTERM the densest enemy cluster, Infection at the
//!    nearest enemy, NX Flags on crowds, a Firewall over the last spot
//!    allies took damage, and Bug ↔ Exploit deploys.
//! 7. **Late game**: with no enemy team left alive, the AI razes
//!    itself (`SuicideIfAlone`) so a stale game still ends.

mod army;
pub(crate) mod build_orders;
mod expansion;
mod specials;

use std::collections::HashMap;

use bevy::prelude::*;

use super::{
    components::{Health, TeamId, UnitStats, UnitType},
    construction::{Constructing, PendingBuild},
    player::LocalTeam,
    production::{Producer, minifac_spam},
    spatial::SpatialIndex,
};
use crate::units::spatial::flat_dist_sq;
use crate::{
    game_setup::AiDifficulty,
    interaction::movement::{MovePath, MoveTarget, QueuedCommand},
    rng::xorshift32,
    terrain::geovent::{GeoventSmoker, VentClaim},
    units::{
        combat::{AttackGroundOrder, AttackTargetOrder, Dying, SelfDestructCountdown},
        content::definitions::UnitKind,
        lifecycle::spawning::Emerging,
        mechanics::{
            command_fire::{CommandFireCooldown, CommandFireEvent, Protected},
            deploy::DeployEvent,
            network_buffer::{AutoDispatch, DISPATCH_MAX, DispatchEvent, PacketBuffer},
        },
    },
};
use army::{
    EnemyStructure, attack_move, is_army, mission_count, pick_attack_targets, scatter,
    spread_around,
};
use build_orders::{HomebaseOrder, Lack, RoleCounts, choose_homebase_order, homebase_roster};
use expansion::{choose_building, pick_datavent};
use specials::{
    COUNTER_DISPATCH_RANGE, CROWD_RADIUS, FIREWALL_ALLY_RADIUS, FIREWALL_ENEMY_RADIUS,
    NX_CROWD_MIN, NX_RANGE, OBELISK_RANGE, SIGTERM_CROWD_MIN, SIGTERM_INTERVAL, UNDEPLOY_MAX,
    bug_should_deploy, crowded_cluster, exploit_should_undeploy, firewall_should_cast,
    should_counter_dispatch,
};

/// Seconds between AI decisions. Upstream's slow update runs every 128
/// frames (~4 s); a faster cadence keeps factories from idling.
const AI_TICK_INTERVAL: f32 = 1.0;

/// Idle army units that must gather at home before the AI pushes.
const ARMY_THRESHOLD: usize = 8;

/// If an enemy unit is inside this distance of a homebase, idle army
/// units defend instead of pushing. Also the radius that counts an
/// idle unit as "at home" (waiting for the push) vs. "in the field".
const DEFEND_RADIUS: f32 = 700.0;

/// Max distance a datavent can be from an existing building before we
/// consider it "unclaimed" — a safety net behind `VentClaim`, which the
/// geovent reconciler only refreshes periodically.
const DATAVENT_CLAIM_RADIUS: f32 = 120.0;

/// Homebase queues are refilled once they drop to the unit in progress,
/// so the factory never idles between two AI ticks.
const HOMEBASE_REFILL_AT: usize = 1;

/// Upstream `IsItOccupiedByAnyHomeBase`: a constructor this close (XZ)
/// to any homebase hasn't cleared the exit yet and must not start a
/// build from inside the base.
const HOMEBASE_EXIT_RADIUS: f32 = 64.0;

/// AI clock + per-team memory.
#[derive(Resource)]
pub struct AiTicker {
    accumulated: f32,
    /// Deterministic xorshift state for every dice roll the AI makes.
    rng: u32,
    /// Seconds since game start of each team's last SIGTERM.
    last_sigterm: HashMap<u8, f32>,
    /// Seconds since game start of each teleporter's last dispatch.
    last_dispatch: HashMap<Entity, f32>,
    /// When this team last saw a living enemy unit (`SuicideIfAlone`).
    no_enemies_since: HashMap<u8, f32>,
}

impl Default for AiTicker {
    fn default() -> Self {
        Self {
            accumulated: 0.0,
            rng: 0x2545_F491,
            last_sigterm: HashMap::new(),
            last_dispatch: HashMap::new(),
            no_enemies_since: HashMap::new(),
        }
    }
}

impl AiTicker {
    fn roll(&mut self) -> u32 {
        xorshift32(&mut self.rng)
    }
}

/// One unit's state captured at the top of an AI tick.
#[derive(Clone, Copy)]
struct Snap {
    entity: Entity,
    team: u8,
    kind: UnitKind,
    pos: Vec3,
    /// Can move at all (speed > 0).
    mobile: bool,
    /// Finished and carrying no order the AI would clobber.
    idle: bool,
    /// Finished and its command-fire ability is off cooldown.
    ready: bool,
    /// Still peeling packets off the team buffer (`AutoDispatch`).
    dispatching: bool,
    /// Under a Firewall reflector (`Protected`).
    protected: bool,
}

/// Scratch buffers reused across AI ticks so the 1 Hz snapshot rebuild
/// doesn't re-allocate.
#[derive(Default)]
pub struct AiScratch {
    units: Vec<Snap>,
    counts: HashMap<u8, RoleCounts>,
    force: HashMap<u8, u32>,
    ai_teams: Vec<u8>,
    structures: Vec<EnemyStructure>,
    vents: Vec<(Entity, Vec3)>,
    /// Homebase positions per team, for constructor nudges.
    homes: HashMap<u8, Vec<Vec3>>,
    enemy_pos: Vec<Vec3>,
    friend_pos: Vec<Vec3>,
    /// Indices into `units` of idle army units near / away from home.
    idle_home: Vec<usize>,
    idle_field: Vec<usize>,
    /// Per-team scratch: homebases of the team being decided.
    team_homes: Vec<Vec3>,
    /// Per-team scratch: enemy structures of the team being decided.
    enemy_structs: Vec<EnemyStructure>,
    /// Free-vent positions scratch for `run_constructors`.
    vent_pos: Vec<Vec3>,
}

type UnitQueryData = (
    Entity,
    &'static TeamId,
    &'static UnitType,
    &'static GlobalTransform,
    &'static UnitStats,
    Option<&'static MoveTarget>,
    Option<&'static MovePath>,
    Has<AttackTargetOrder>,
    Has<AttackGroundOrder>,
    Has<PendingBuild>,
    Has<Constructing>,
    Has<Emerging>,
    Has<CommandFireCooldown>,
    Has<AutoDispatch>,
    Has<Protected>,
);

/// Message writers the AI issues orders through — the same messages the
/// player's `D` hotkey writes.
#[derive(bevy::ecs::system::SystemParam)]
pub struct AiOrders<'w> {
    command_fire: MessageWriter<'w, CommandFireEvent>,
    deploy: MessageWriter<'w, DeployEvent>,
    dispatch: MessageWriter<'w, DispatchEvent>,
}

/// Main AI brain.
#[allow(clippy::too_many_arguments)]
pub fn ai_brain(
    time: Res<Time>,
    mut ticker: ResMut<AiTicker>,
    mut scratch: Local<AiScratch>,
    local: Res<LocalTeam>,
    difficulty: Res<AiDifficulty>,
    mut damage_log: ResMut<TeamDamageLog>,
    packet_buffer: Res<PacketBuffer>,
    spatial: Res<SpatialIndex>,
    units: Query<UnitQueryData, Without<Dying>>,
    mut producers: Query<(&TeamId, &UnitType, &mut Producer), Without<Dying>>,
    datavents: Query<(Entity, &GeoventSmoker), Without<VentClaim>>,
    suiciding: Query<(), With<SelfDestructCountdown>>,
    mut orders: AiOrders,
    mut commands: Commands,
) {
    ticker.accumulated += time.delta_secs();
    if ticker.accumulated < AI_TICK_INTERVAL {
        return;
    }
    ticker.accumulated = 0.0;
    let now = time.elapsed_secs();
    let ticker: &mut AiTicker = &mut ticker;
    let s: &mut AiScratch = &mut scratch;

    snapshot(s, &units, &datavents, local.0);
    // Forget dispatch timers of dead teleporters.
    ticker.last_dispatch.retain(|e, _| units.contains(*e));

    let teams = std::mem::take(&mut s.ai_teams);
    for &team in &teams {
        let own = s.counts.get(&team).copied().unwrap_or_default();
        let enemy = s.counts.iter().filter(|(t, _)| **t != team).fold(
            RoleCounts::default(),
            |mut acc, (_, c)| {
                acc.spams += c.spams;
                acc.mediums += c.mediums;
                acc.buildings += c.buildings;
                acc
            },
        );
        let mut lack = Lack::compute(difficulty.fairness_slack(), own, enemy);
        let force = s.force.get(&team).copied().unwrap_or(0);
        let buffer = packet_buffer.peek(team);

        run_factories(team, s, ticker, &mut producers, &mut lack, force, buffer);
        run_constructors(team, s, ticker, &mut lack, &mut commands);
        let target = run_army(team, s, ticker, &spatial, force + buffer, &mut commands);
        run_network(
            team,
            s,
            ticker,
            &spatial,
            now,
            buffer,
            &lack,
            target,
            &mut orders,
        );
        run_specials(team, s, ticker, &spatial, &mut damage_log, now, &mut orders);
        run_late_game(team, s, ticker, &suiciding, now, &mut commands);
    }
    s.ai_teams = teams;
}

/// Upstream `SuicideIfAlone`: when no enemy team owns a living unit
/// anymore (someone else finished them, or they never started), the
/// AI razes itself after `30 + (7*team) % 16` seconds so a stale game
/// still ends. Units already counting down keep their timer — a fresh
/// insert every tick would reset the 5 s fuse and it would never fire.
fn run_late_game(
    team: u8,
    s: &AiScratch,
    ticker: &mut AiTicker,
    suiciding: &Query<(), With<SelfDestructCountdown>>,
    now: f32,
    commands: &mut Commands,
) {
    if s.counts.keys().any(|t| *t != team) {
        ticker.no_enemies_since.remove(&team);
        return;
    }
    let since = *ticker.no_enemies_since.entry(team).or_insert(now);
    if now - since <= 30.0 + (7 * team as u32 % 16) as f32 {
        return;
    }
    for u in s.units.iter().filter(|u| u.team == team) {
        if suiciding.contains(u.entity) {
            continue;
        }
        commands.entity(u.entity).try_insert(SelfDestructCountdown {
            remaining: crate::units::combat::SELF_DESTRUCT_DELAY,
        });
    }
}

/// Capture every live unit, per-team tallies, the AI team list, enemy
/// structures and the free datavents.
fn snapshot(
    s: &mut AiScratch,
    units: &Query<UnitQueryData, Without<Dying>>,
    datavents: &Query<(Entity, &GeoventSmoker), Without<VentClaim>>,
    local_team: u8,
) {
    s.units.clear();
    s.counts.clear();
    s.force.clear();
    s.ai_teams.clear();
    s.structures.clear();
    s.homes.clear();
    for (
        e,
        team,
        ut,
        gtf,
        stats,
        mt,
        mp,
        attack,
        attack_ground,
        pending,
        constructing,
        emerging,
        cooldown,
        auto_dispatch,
        protected,
    ) in units
    {
        let kind = ut.0;
        let snap = Snap {
            entity: e,
            team: team.0,
            kind,
            pos: gtf.translation(),
            mobile: stats.speed > 0.0,
            idle: !emerging
                && mt.is_none()
                && mp.is_none()
                && !attack
                && !attack_ground
                && !pending
                && !constructing,
            ready: !emerging && !cooldown,
            dispatching: auto_dispatch,
            protected,
        };
        s.units.push(snap);
        // Units still emerging count too, so the budget already sees
        // what is being built and several factories can't overshoot.
        s.counts.entry(team.0).or_default().add(kind);
        // KPAI `forceSize`: one per unit of any kind, buildings included.
        *s.force.entry(team.0).or_default() += 1;
        let homebase = kind.is_homebase();
        if homebase && team.0 != local_team && !s.ai_teams.contains(&team.0) {
            s.ai_teams.push(team.0);
        }
        if homebase {
            s.homes.entry(team.0).or_default().push(snap.pos);
        }
        if homebase || kind.is_small_building() {
            s.structures.push(EnemyStructure {
                team: team.0,
                pos: snap.pos,
                homebase,
            });
        }
    }
    s.ai_teams.sort_unstable();

    let claim_sq = DATAVENT_CLAIM_RADIUS * DATAVENT_CLAIM_RADIUS;
    s.vents.clear();
    for (entity, vent) in datavents {
        let taken = s
            .units
            .iter()
            .any(|u| u.kind.is_building() && flat_dist_sq(u.pos, vent.pos) <= claim_sq);
        if !taken {
            s.vents.push((entity, vent.pos));
        }
    }
}

/// Homebase mix (`OrderHomeBase`) and the minifac spam toggle
/// (`OrderMiniFac`), both charged against the fairness budget.
fn run_factories(
    team: u8,
    s: &AiScratch,
    ticker: &mut AiTicker,
    producers: &mut Query<(&TeamId, &UnitType, &mut Producer), Without<Dying>>,
    lack: &mut Lack,
    force: u32,
    buffer: u32,
) {
    for (t, ut, mut producer) in producers.iter_mut() {
        if t.0 != team {
            continue;
        }
        if let Some(roster) = homebase_roster(ut.0) {
            if producer.queue().len() > HOMEBASE_REFILL_AT {
                continue;
            }
            let constructors = s
                .units
                .iter()
                .filter(|u| u.team == team && u.kind == roster.constructor)
                .count() as u32;
            let roll = ticker.roll() % 1000 + 1;
            let heavy_coin = ticker.roll() & 1 == 0;
            // Fair KPAI's `math.random(1,5)` spam batch. Drawn
            // unconditionally so the dice stream stays branch-free.
            let batch = ticker.roll() % 5 + 1;
            match choose_homebase_order(constructors, force, buffer, roll, heavy_coin, batch, lack)
            {
                HomebaseOrder::Constructor => {
                    producer.enqueue(roster.constructor);
                    lack.mediums -= 1;
                }
                HomebaseOrder::Heavy => {
                    producer.enqueue(roster.heavy);
                    lack.mediums -= 1;
                }
                HomebaseOrder::Arty => {
                    producer.enqueue(roster.arty);
                    lack.mediums -= 1;
                }
                HomebaseOrder::Spam(n) => {
                    for _ in 0..n {
                        producer.enqueue(roster.spam);
                    }
                    lack.spams -= n as i32;
                }
                HomebaseOrder::Nothing => {}
            }
        } else if let Some(spam) = minifac_spam(ut.0) {
            // Fair KPAI turns repeat off on every minifac and re-orders
            // one spam at a time while `Lack.spams > 0`; toggling our
            // repeat flag on the budget is the same thing.
            let want = lack.spams > 0;
            producer.set_repeat(want);
            if want && producer.queue().is_empty() {
                producer.enqueue(spam);
                lack.spams -= 1;
            }
        }
    }
}

/// `DispatchCon`: every idle constructor claims a datavent. Upstream's
/// guards come first: a constructor still inside any homebase's exit
/// box steps out before it may claim (fresh constructors would
/// otherwise build — and clog — from inside the base), and with the
/// building budget spent it is spread on a ring around its own home
/// instead of parking in the exit (upstream re-issues that spread
/// order every update, so broke constructors keep milling — faithful,
/// if noisy). With the budget there but every vent taken, the
/// constructor walks at the nearest enemy structure — upstream
/// re-colonizes enemy-held vents instead, but our buildings rise from
/// the ground, so we don't build on occupied sites; it rejoins
/// expansion the moment a vent frees up and it idles again.
fn run_constructors(
    team: u8,
    s: &mut AiScratch,
    ticker: &mut AiTicker,
    lack: &mut Lack,
    commands: &mut Commands,
) {
    let mut owned_minifacs = s
        .units
        .iter()
        .filter(|u| u.team == team && u.kind.is_minifac())
        .count();
    s.vent_pos.clear();
    s.vent_pos.extend(s.vents.iter().map(|(_, p)| *p));
    let exit_sq = HOMEBASE_EXIT_RADIUS * HOMEBASE_EXIT_RADIUS;
    for ctor in s
        .units
        .iter()
        .filter(|u| u.team == team && u.kind.is_constructor() && u.idle && u.mobile)
    {
        // Don't start building from inside a base: step out of the
        // exit box (any team's homebase — enemy bases count too).
        if let Some(base) = s
            .structures
            .iter()
            .filter(|st| st.homebase)
            .map(|st| st.pos)
            .find(|&p| flat_dist_sq(p, ctor.pos) <= exit_sq)
        {
            crate::interaction::replace_order(
                &mut commands.entity(ctor.entity),
                QueuedCommand::Move(spread_around(base, &mut ticker.rng)),
            );
            continue;
        }
        // Fair KPAI: with no building budget the constructor is spread
        // around the homebase rather than sent to claim.
        if lack.buildings <= 0 {
            if let Some(&home) = s.homes.get(&team).and_then(|homes| {
                homes.iter().min_by(|a, b| {
                    flat_dist_sq(**a, ctor.pos).total_cmp(&flat_dist_sq(**b, ctor.pos))
                })
            }) {
                crate::interaction::replace_order(
                    &mut commands.entity(ctor.entity),
                    QueuedCommand::Move(spread_around(home, &mut ticker.rng)),
                );
            }
            continue;
        }
        // No free vent anywhere: walk at the nearest enemy structure
        // and join the push (KP constructors are unarmed, so the
        // fight order degrades to a march).
        if s.vent_pos.is_empty() {
            if let Some(target) = s
                .structures
                .iter()
                .filter(|st| st.team != team)
                .map(|st| st.pos)
                .min_by(|a, b| flat_dist_sq(*a, ctor.pos).total_cmp(&flat_dist_sq(*b, ctor.pos)))
            {
                attack_move(ctor.entity, target, commands);
            }
            continue;
        }
        let Some(idx) = pick_datavent(ctor.pos, &s.vent_pos, &mut ticker.rng) else {
            break;
        };
        let Some(kind) = choose_building(ctor.kind, owned_minifacs, ticker.roll() % 3) else {
            continue;
        };
        let (vent_entity, site) = s.vents.swap_remove(idx);
        s.vent_pos.swap_remove(idx);
        crate::interaction::replace_order(
            &mut commands.entity(ctor.entity),
            QueuedCommand::BuildAt { kind, site },
        );
        // Claim now so neither another constructor this tick nor the
        // player's placement ghost can stack on the same vent.
        commands.entity(vent_entity).insert(VentClaim);
        lack.buildings -= 1;
        if kind.is_minifac() {
            owned_minifacs += 1;
        }
    }
}

/// Nearest living enemy of `team` within `radius` of `pos`, with its XZ
/// distance.
fn nearest_enemy(
    spatial: &SpatialIndex,
    team: u8,
    pos: Vec3,
    radius: f32,
    filter: impl Fn(UnitKind) -> bool,
) -> Option<(Vec3, f32)> {
    let r_sq = radius * radius;
    let mut best: Option<(Vec3, f32)> = None;
    spatial.query_radius(pos, radius, |e| {
        // The AI must not see what its detectors can't: undetected
        // cloaked Worms / Logic Bombs are invisible to it too.
        if e.team == team || !e.hp_positive || !e.targetable_by(team) || !filter(e.kind) {
            return;
        }
        let d = flat_dist_sq(e.pos, pos);
        if d <= r_sq && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((e.pos, d));
        }
    });
    best.map(|(p, d)| (p, d.sqrt()))
}

/// Enemies of `team` within `radius` of `pos` (same visibility rules
/// as [`nearest_enemy`]), for the Firewall's crowd counts.
fn count_enemies_within(spatial: &SpatialIndex, team: u8, pos: Vec3, radius: f32) -> usize {
    let mut n = 0;
    spatial.query_radius(pos, radius, |e| {
        if e.team != team && e.hp_positive && e.targetable_by(team) {
            n += 1;
        }
    });
    n
}

/// Defend or push. Returns the primary push target (also used for
/// dispatch). Without a home threat the army is split over
/// [`mission_count`] fronts — upstream's mission loop — with idle
/// units round-robin assigned; every unit still scatters on its
/// target's ring.
fn run_army(
    team: u8,
    s: &mut AiScratch,
    ticker: &mut AiTicker,
    spatial: &SpatialIndex,
    force_plus_buffer: u32,
    commands: &mut Commands,
) -> Option<Vec3> {
    let homes = {
        s.team_homes.clear();
        s.team_homes
            .extend(s.homes.get(&team).into_iter().flatten().copied());
        &s.team_homes
    };
    let home = *homes.first()?;

    // Only enemy *units* count as a threat — an enemy minifac built
    // within 700 of the base must not pin the army at home forever.
    let threat = homes.iter().find_map(|h| {
        nearest_enemy(spatial, team, *h, DEFEND_RADIUS, |k| !k.is_building()).map(|(p, _)| p)
    });

    s.idle_home.clear();
    s.idle_field.clear();
    let home_sq = DEFEND_RADIUS * DEFEND_RADIUS;
    for (i, u) in s.units.iter().enumerate() {
        if u.team != team || !u.idle || !u.mobile || !is_army(u.kind) {
            continue;
        }
        if homes.iter().any(|h| flat_dist_sq(*h, u.pos) <= home_sq) {
            s.idle_home.push(i);
        } else {
            s.idle_field.push(i);
        }
    }

    let (targets, push_home) = match threat {
        Some(threat) => (vec![threat], true),
        None => {
            s.enemy_structs.clear();
            s.enemy_structs
                .extend(s.structures.iter().copied().filter(|e| e.team != team));
            let targets = pick_attack_targets(
                home,
                &s.enemy_structs,
                force_plus_buffer,
                mission_count(force_plus_buffer),
                &mut ticker.rng,
            );
            if targets.is_empty() {
                return None;
            }
            (targets, s.idle_home.len() >= ARMY_THRESHOLD)
        }
    };
    let home_movers = if push_home { &s.idle_home[..] } else { &[] };
    for (n, &i) in s.idle_field.iter().chain(home_movers).enumerate() {
        // Round-robin across the mission fronts (upstream dispatches
        // per-mission quotas; round-robin spreads a mixed army evenly).
        attack_move(
            s.units[i].entity,
            scatter(targets[n % targets.len()], &mut ticker.rng),
            commands,
        );
        // Ordered now: later phases this tick (Bug deploy) must not
        // treat it as idle and undo the order.
        s.units[i].idle = false;
    }
    Some(targets[0])
}

/// Packet dispatch. Counter-dispatch mirrors `KPAI_Fair.lua`'s
/// `UnitDamaged` teleporter branch (an attacker within 300, ≥3
/// buffered, `Lack.spams > 5`, 5 s cooldown); we treat any enemy that
/// close as the attacker. Offensively, a dispatch goes out from the
/// teleporter nearest the push target — upstream's mission loop
/// dispatches ports toward the spot it is attacking, with no say from
/// the fairness budget (only counter-dispatch consults `Lack`).
///
/// One `DispatchEvent` is not a 12-packet pulse: it flags the
/// teleporter `AutoDispatch`, and `tick_auto_dispatch` keeps peeling 12
/// packets per frame off the team buffer until it drains (upstream's
/// `not opts.alt` continuation rule). So the AI skips teleporters that
/// are still draining, and its local `buffer` bookkeeping is only a
/// conservative same-tick approximation — the resource is the truth.
#[allow(clippy::too_many_arguments)]
fn run_network(
    team: u8,
    s: &AiScratch,
    ticker: &mut AiTicker,
    spatial: &SpatialIndex,
    now: f32,
    mut buffer: u32,
    lack: &Lack,
    target: Option<Vec3>,
    orders: &mut AiOrders,
) {
    let teleporters = || {
        s.units
            .iter()
            .filter(move |u| u.team == team && u.kind.is_teleporter() && u.ready && !u.dispatching)
    };
    for tp in teleporters() {
        let since = ticker.last_dispatch.get(&tp.entity).map(|t| now - t);
        if !should_counter_dispatch(buffer, lack.spams, since) {
            continue;
        }
        let Some((attacker, _)) =
            nearest_enemy(spatial, team, tp.pos, COUNTER_DISPATCH_RANGE, |_| true)
        else {
            continue;
        };
        orders.dispatch.write(DispatchEvent {
            teleporter: tp.entity,
            target: attacker,
        });
        ticker.last_dispatch.insert(tp.entity, now);
        // The drain commits the buffer; locally pretend DISPATCH_MAX
        // of it is gone so a second teleporter this tick stays quiet.
        buffer = buffer.saturating_sub(DISPATCH_MAX as u32);
    }

    let Some(target) = target else {
        return;
    };
    if buffer < DISPATCH_MAX as u32 {
        return;
    }
    let off_cooldown = |tp: &&Snap| {
        ticker
            .last_dispatch
            .get(&tp.entity)
            .is_none_or(|t| now - t > specials::DISPATCH_COOLDOWN)
    };
    if let Some(tp) = teleporters()
        .filter(off_cooldown)
        .min_by(|a, b| flat_dist_sq(a.pos, target).total_cmp(&flat_dist_sq(b.pos, target)))
    {
        orders.dispatch.write(DispatchEvent {
            teleporter: tp.entity,
            target,
        });
        ticker.last_dispatch.insert(tp.entity, now);
    }
}

/// Command-fire specials and Hacker deploys (KPAI slow update).
fn run_specials(
    team: u8,
    s: &mut AiScratch,
    ticker: &mut AiTicker,
    spatial: &SpatialIndex,
    damage_log: &mut TeamDamageLog,
    now: f32,
    orders: &mut AiOrders,
) {
    s.enemy_pos.clear();
    s.friend_pos.clear();
    let ally_r_sq = FIREWALL_ALLY_RADIUS * FIREWALL_ALLY_RADIUS;
    for u in &s.units {
        if u.team == team {
            s.friend_pos.push(u.pos);
        } else {
            s.enemy_pos.push(u.pos);
        }
    }
    let ready = |kind: UnitKind| {
        s.units
            .iter()
            .filter(move |u| u.team == team && u.kind == kind && u.ready)
    };

    // Terminal: SIGTERM the most crowded enemy cluster, at most once per
    // 15 s per team (`crowdedarea > 0 and td.lastNuke < f - 15*30`).
    let sigterm_due = ticker
        .last_sigterm
        .get(&team)
        .is_none_or(|t| now - t >= SIGTERM_INTERVAL);
    if sigterm_due
        && let Some(terminal) = ready(UnitKind::Terminal).next()
        && let Some((center, _)) =
            crowded_cluster(&s.enemy_pos, &s.friend_pos, CROWD_RADIUS, SIGTERM_CROWD_MIN)
    {
        orders.command_fire.write(CommandFireEvent {
            attacker: terminal.entity,
            target: center,
        });
        ticker.last_sigterm.insert(team, now);
    }

    // Obelisk: Infection at the nearest enemy within 1500. The gas
    // spares friends, so no crowd check is needed.
    for obelisk in ready(UnitKind::Obelisk) {
        if let Some((pos, _)) = nearest_enemy(spatial, team, obelisk.pos, OBELISK_RANGE, |_| true) {
            orders.command_fire.write(CommandFireEvent {
                attacker: obelisk.entity,
                target: pos,
            });
        }
    }

    // Firewall: drop the reflector over the spot where allies were
    // last hit, once that fight is big enough to be worth shielding
    // (upstream `CMD_FIREWALL`). One cast, then the memory is spent.
    if let Some((t, pos)) = damage_log.latest(team)
        && let Some(firewall) = ready(UnitKind::Firewall).next()
        && firewall_should_cast(
            now - t,
            count_enemies_within(spatial, team, pos, FIREWALL_ENEMY_RADIUS),
            s.units
                .iter()
                .filter(|u| u.team == team && flat_dist_sq(u.pos, pos) <= ally_r_sq)
                .count(),
            s.units
                .iter()
                .any(|u| u.team == team && u.protected && flat_dist_sq(u.pos, pos) <= ally_r_sq),
        )
    {
        orders.command_fire.write(CommandFireEvent {
            attacker: firewall.entity,
            target: pos,
        });
        damage_log.forget(team);
    }

    // Pointer: upstream `DispatchArty` plants an NX Flag instead of a
    // plain attack when the target spot is crowded. One flag per tick.
    if let Some((center, _)) =
        crowded_cluster(&s.enemy_pos, &s.friend_pos, CROWD_RADIUS, NX_CROWD_MIN)
        && let Some(pointer) =
            ready(UnitKind::Pointer).find(|p| flat_dist_sq(p.pos, center) <= NX_RANGE * NX_RANGE)
    {
        orders.command_fire.write(CommandFireEvent {
            attacker: pointer.entity,
            target: center,
        });
    }

    // Hacker: idle Bugs bombard from range, Exploits pack up when the
    // fight leaves (or overruns) them.
    let clear_sq = specials::DEPLOY_CLEAR_RADIUS * specials::DEPLOY_CLEAR_RADIUS;
    for u in s.units.iter().filter(|u| u.team == team) {
        let deploy = match u.kind {
            UnitKind::Bug if u.idle => {
                // Upstream `IsItOccupied`: no deploy while standing on
                // any team's building (homebase / minifac / special).
                let on_building = s
                    .structures
                    .iter()
                    .any(|st| flat_dist_sq(st.pos, u.pos) <= clear_sq);
                bug_should_deploy(
                    nearest_enemy(spatial, team, u.pos, specials::DEPLOY_MAX, |_| true)
                        .map(|(_, d)| d),
                    on_building,
                )
            }
            UnitKind::Exploit if u.ready => exploit_should_undeploy(
                nearest_enemy(spatial, team, u.pos, UNDEPLOY_MAX, |_| true).map(|(_, d)| d),
            ),
            _ => false,
        };
        if deploy {
            orders.deploy.write(DeployEvent { entity: u.entity });
        }
    }
}

/// Upstream's `UnitDamaged` hook, approximated from the outside: watch
/// every unit's HP and remember where each team was last hurt, so the
/// Firewall trigger (`run_specials`) can react without combat
/// internals knowing the AI exists. First sight seeds the cache
/// without logging; a unit that dies the same frame it is hit is
/// missed (`Dying` excluded) — every earlier hit of that fight still
/// registers, which is enough signal.
#[derive(Resource, Default)]
pub struct TeamDamageLog {
    /// (when, where) of each team's most recent landed hit.
    latest: HashMap<u8, (f32, Vec3)>,
}

impl TeamDamageLog {
    /// The team's freshest damage, as `(age_in_seconds, position)`.
    pub fn latest(&self, team: u8) -> Option<(f32, Vec3)> {
        self.latest.get(&team).copied()
    }

    fn record(&mut self, team: u8, when: f32, pos: Vec3) {
        self.latest.insert(team, (when, pos));
    }

    fn forget(&mut self, team: u8) {
        self.latest.remove(&team);
    }
}

/// Fills [`TeamDamageLog`] every sim frame. Must stay outside
/// `ai_brain`'s 1 Hz gate — HP drops between ticks would be missed.
type DamageWatchData = (
    Entity,
    &'static TeamId,
    &'static Health,
    &'static GlobalTransform,
);

pub fn track_team_damage(
    time: Res<Time>,
    units: Query<DamageWatchData, (With<UnitType>, Without<Dying>)>,
    mut last: Local<HashMap<Entity, f32>>,
    mut log: ResMut<TeamDamageLog>,
) {
    let now = time.elapsed_secs();
    last.retain(|e, _| units.contains(*e));
    for (e, team, hp, gtf) in &units {
        if let Some(&prev) = last.get(&e)
            && hp.current < prev
        {
            log.record(team.0, now, gtf.translation());
        }
        last.insert(e, hp.current);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::spatial::SpatialEntry;

    fn entry(entity: u32, team: u8, kind: UnitKind, x: f32) -> SpatialEntry {
        SpatialEntry {
            entity: Entity::from_raw_u32(entity).unwrap(),
            pos: Vec3::new(x, 0.0, 0.0),
            hit_radius: 0.0,
            mid_y: 0.0,
            team,
            kind,
            hp_positive: true,
            is_flying: false,
            cloaked: false,
            detected_by: 0,
        }
    }

    /// `nearest_enemy` skips friends, applies the kind filter and the
    /// radius, and reports the XZ distance.
    #[test]
    fn nearest_enemy_filters_team_kind_and_range() {
        let mut index = SpatialIndex::default();
        index.insert_for_test(entry(1, 1, UnitKind::Bit, 50.0));
        index.insert_for_test(entry(2, 0, UnitKind::Socket, 200.0));
        index.insert_for_test(entry(3, 0, UnitKind::Bit, 500.0));
        index.insert_for_test(entry(4, 0, UnitKind::Bit, 900.0));
        let (pos, d) = nearest_enemy(&index, 1, Vec3::ZERO, 700.0, |_| true).unwrap();
        assert_eq!(pos.x, 200.0);
        assert!((d - 200.0).abs() < 1e-3);
        let (pos, _) = nearest_enemy(&index, 1, Vec3::ZERO, 700.0, |k| !k.is_building()).unwrap();
        assert_eq!(pos.x, 500.0);
        assert!(nearest_enemy(&index, 1, Vec3::ZERO, 150.0, |_| true).is_none());
    }
}
