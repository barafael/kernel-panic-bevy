//! Post-game statistics: per seat, over the match, the army value, the
//! unit count, the production and the actions per minute, plus the
//! moments a landmark building (homebase, minifac, special) was raised
//! or lost. The game-over panel's Statistics page plots them
//! (`ui::menu::stats_page`).
//!
//! Spring keeps team statistics in the engine (`CTeam::statHistory`, a
//! `TeamStatistics` every `statsFrames` = 16 s, shown by the end-game
//! box); Beyond All Reason's team stats panel draws them as one chart
//! per stat with the teams as series, milestones marked on the lines
//! (`luaui/Include/teamstats_graphs.lua`). This is that idea, much
//! smaller: four stats, one sample a second, build markers.
//!
//! Kernel Panic has no metal: a unit's *value* is its `buildtime`, the
//! one cost a KP unit has.
//!
//! **An observer, not part of the sim.** [`observe`] runs after the
//! gameplay chain and only reads sim components; [`MatchStats`] is not
//! in the sync hash and nothing in the sim reads it. The actions come
//! from the commands the bus applies (`net::apply`), so a replay or a
//! peer counts what every peer applied, not what one machine clicked.

use std::collections::{BTreeMap, BTreeSet};

use bevy::prelude::*;

use crate::game_setup::ActiveMatch;
use crate::map_loading::MatchScopeAppExt;
use crate::net::{CommandBus, NetId, SimCommand};
use crate::sim::GAME_SPEED;
use crate::units::GameplaySet;
use crate::units::combat::Dying;
use crate::units::components::{Faction, TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::lifecycle::spawning::Emerging;

/// Sim ticks between samples: one a second.
pub const SAMPLE_TICKS: u32 = GAME_SPEED as u32;

/// The actions per minute: the commands of the last this many samples
/// (a minute; fewer at the start of the match).
const APM_WINDOW: usize = 60;

/// The stats a chart can plot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stat {
    /// Value of the finished units alive, landmarks aside.
    ArmyValue,
    /// Finished units alive, buildings included.
    Units,
    /// Value of every unit finished so far.
    Production,
    /// Commands issued over the last minute.
    Apm,
}

impl Stat {
    pub const ALL: [Stat; 4] = [Stat::ArmyValue, Stat::Units, Stat::Production, Stat::Apm];

    pub fn label(self) -> &'static str {
        match self {
            Stat::ArmyValue => "Army value",
            Stat::Units => "Unit count",
            Stat::Production => "Production",
            Stat::Apm => "Actions/min",
        }
    }

    /// What the chart's numbers are.
    pub fn explain(self) -> &'static str {
        match self {
            Stat::ArmyValue => "Build time of the living units, factories and specials aside",
            Stat::Units => "Finished units alive, buildings included",
            Stat::Production => "Build time of every unit finished so far",
            Stat::Apm => "Commands given over the last minute",
        }
    }
}

/// One seat's series, one value per [`MatchStats::ticks`] entry.
#[derive(Clone, Debug, Default)]
pub struct SeatStats {
    pub faction: Option<Faction>,
    pub ai: bool,
    pub army_value: Vec<f32>,
    pub units: Vec<f32>,
    pub production: Vec<f32>,
    pub apm: Vec<f32>,
    /// Commands applied so far.
    actions: u32,
    /// [`Self::actions`] at each sample.
    actions_at: Vec<u32>,
    /// Value of every unit finished so far.
    produced: f32,
}

impl SeatStats {
    pub fn series(&self, stat: Stat) -> &[f32] {
        match stat {
            Stat::ArmyValue => &self.army_value,
            Stat::Units => &self.units,
            Stat::Production => &self.production,
            Stat::Apm => &self.apm,
        }
    }
}

/// A landmark building finished or lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildingEvent {
    pub tick: u32,
    pub owner: u8,
    pub kind: UnitKind,
    /// Finished (`true`) or destroyed.
    pub built: bool,
}

/// The match's statistics. Match-scoped: a new match starts empty.
#[derive(Resource, Default, Debug)]
pub struct MatchStats {
    /// The tick of every sample.
    pub ticks: Vec<u32>,
    /// Per seat (owner `i` is seat `i`).
    pub seats: Vec<SeatStats>,
    pub events: Vec<BuildingEvent>,
    /// Units seen finished: counted into production once.
    finished: BTreeSet<NetId>,
    /// The landmarks standing, by id: owner and kind.
    landmarks: BTreeMap<NetId, (u8, UnitKind)>,
    /// The last tick observed.
    last_tick: Option<u32>,
}

impl MatchStats {
    /// A command `player` gave, as the bus applied it. Only orders to
    /// units count; pausing, the speed, chat-like and host commands do
    /// not.
    pub fn count_action(&mut self, player: u8, cmd: &SimCommand) {
        if !is_action(cmd) {
            return;
        }
        if let Some(seat) = self.seats.get_mut(player as usize) {
            seat.actions += 1;
        }
    }

    /// Seconds into the match of sample `i`.
    pub fn seconds(&self, i: usize) -> f32 {
        self.ticks.get(i).map_or(0.0, |&t| t as f32 / GAME_SPEED)
    }
}

fn is_action(cmd: &SimCommand) -> bool {
    use SimCommand::*;
    match cmd {
        Order { .. }
        | PerUnit { .. }
        | Path { .. }
        | Stop { .. }
        | Build { .. }
        | Factory { .. }
        | SetRepeat { .. }
        | SetAutoHold { .. }
        | SetTarget { .. }
        | UnsetTarget { .. }
        | SelfDestruct { .. }
        | Deploy { .. }
        | Enter { .. }
        | Cast { .. } => true,
        SetSpeed { .. }
        | Pause { .. }
        | Resign
        | Dev(_)
        | Take { .. }
        | Dropped
        | InternalSpeed { .. }
        | Returned => false,
    }
}

/// The buildings worth a marker: homebases, minifacs and specials.
pub fn is_landmark(kind: UnitKind) -> bool {
    Faction::ALL
        .iter()
        .any(|f| f.homebase() == kind || f.secondary_factory() == kind)
        || matches!(
            kind,
            UnitKind::Terminal | UnitKind::Obelisk | UnitKind::Firewall
        )
}

pub struct MatchStatsPlugin;

impl Plugin for MatchStatsPlugin {
    fn build(&self, app: &mut App) {
        app.init_match_resource::<MatchStats>()
            .add_systems(FixedUpdate, observe.after(GameplaySet::Cleanup));
    }
}

/// What [`observe`] reads of a unit: id, kind, owner, under
/// construction, dying.
type UnitView = (
    &'static NetId,
    &'static UnitType,
    &'static TeamId,
    Has<Emerging>,
    Has<Dying>,
);

/// Once a tick: production and landmarks; every [`SAMPLE_TICKS`] a
/// sample of every series.
fn observe(
    mut stats: ResMut<MatchStats>,
    bus: Option<Res<CommandBus>>,
    active: Option<Res<ActiveMatch>>,
    registry: Res<UnitRegistry>,
    units: Query<UnitView>,
) {
    let (Some(bus), Some(active)) = (bus, active) else {
        return;
    };
    // Ticks applied, this one included; nothing new while paused.
    let tick = bus.tick;
    if stats.last_tick == Some(tick) {
        return;
    }
    let stats = &mut *stats;
    let first = stats.last_tick.is_none();
    stats.last_tick = Some(tick);
    if stats.seats.len() != active.0.players.len() {
        stats.seats = active
            .0
            .players
            .iter()
            .map(|p| SeatStats {
                faction: Some(p.faction),
                ai: p.ai,
                ..default()
            })
            .collect();
    }
    let seats = stats.seats.len();
    let sample = first || tick.is_multiple_of(SAMPLE_TICKS);
    let mut army = vec![0.0f32; seats];
    let mut count = vec![0.0f32; seats];
    let mut standing = BTreeSet::new();
    for (&id, unit, team, emerging, dying) in &units {
        let owner = team.0 as usize;
        if emerging || dying || owner >= seats {
            continue;
        }
        let kind = unit.0;
        let value = registry.raw_build_time(kind);
        if stats.finished.insert(id) && !first {
            stats.seats[owner].produced += value;
        }
        if is_landmark(kind) {
            standing.insert(id);
            if stats.landmarks.insert(id, (team.0, kind)).is_none() && !first {
                stats.events.push(BuildingEvent {
                    tick,
                    owner: team.0,
                    kind,
                    built: true,
                });
            }
        }
        if sample {
            count[owner] += 1.0;
            if !is_landmark(kind) {
                army[owner] += value;
            }
        }
    }
    let mut lost: Vec<(NetId, (u8, UnitKind))> = Vec::new();
    stats.landmarks.retain(|id, &mut entry| {
        let alive = standing.contains(id);
        if !alive {
            lost.push((*id, entry));
        }
        alive
    });
    for (_, (owner, kind)) in lost {
        stats.events.push(BuildingEvent {
            tick,
            owner,
            kind,
            built: false,
        });
    }
    if !sample {
        return;
    }
    stats.ticks.push(tick);
    for (i, seat) in stats.seats.iter_mut().enumerate() {
        seat.army_value.push(army[i]);
        seat.units.push(count[i]);
        seat.production.push(seat.produced);
        seat.actions_at.push(seat.actions);
        let n = seat.actions_at.len();
        let since = seat.actions_at[n.saturating_sub(1 + APM_WINDOW)];
        seat.apm.push((seat.actions - since) as f32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landmarks_are_the_factories_and_specials() {
        for kind in [
            UnitKind::Kernel,
            UnitKind::Hole,
            UnitKind::Carrier,
            UnitKind::Socket,
            UnitKind::Window,
            UnitKind::Port,
            UnitKind::Terminal,
            UnitKind::Obelisk,
            UnitKind::Firewall,
        ] {
            assert!(is_landmark(kind), "{kind:?}");
        }
        for kind in [UnitKind::Bit, UnitKind::BadBlock, UnitKind::LogicBomb] {
            assert!(!is_landmark(kind), "{kind:?}");
        }
    }

    #[test]
    fn only_unit_orders_are_actions() {
        let mut stats = MatchStats {
            seats: vec![SeatStats::default(); 2],
            ..default()
        };
        stats.count_action(
            1,
            &SimCommand::Stop {
                units: crate::net::Units::of([NetId(3)]),
            },
        );
        stats.count_action(1, &SimCommand::Pause { on: true });
        stats.count_action(9, &SimCommand::Resign);
        assert_eq!(stats.seats[0].actions, 0);
        assert_eq!(stats.seats[1].actions, 1);
    }
}
