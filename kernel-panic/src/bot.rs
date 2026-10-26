//! Dev tool: a file-driven bot that plays the game without a mouse.
//!
//! Set `KP_BOT=<dir>` (optionally `KP_BOT_MAP=<map stem>`, default
//! `Data_Cache_L1`, and `KP_BOT_ENEMY=1` to seat an AI Hacker opponent).
//! The tool boots straight into a skirmish as System, then every sim
//! frame reads the lines appended to `<dir>/cmd` and executes them in
//! order, writing results to `<dir>/log` (append) and unit state to
//! `<dir>/state.txt` (replaced on `dump` / every `every` frames).
//!
//! Units are addressed by their entity index (`id` in dumps). A
//! selector is `all` (every mobile unit), `<id>[,<id>…]`, `kind:<name>`
//! (FBI unitname, e.g. `kind:bit`), `team:<n>`, `mine` (the local
//! player's mobile units), `mine:<name>` (the local player's units of
//! that kind, buildings too), or `last` (the most recently spawned unit).
//! Where a command takes a team, `me` is the local player's: in a
//! networked match the seat is the lobby's to give (the host sits
//! first), and the log says which at the first tick (`team <n>`).
//!
//! Commands (one per line, `#` comments):
//! - `spawn <kind> <team> <x> <z> [count [spacing]]` — spawn units of
//!   `kind` (FBI unitname) for `team` at `(x, z)`, `count` of them in a
//!   row `spacing` elmos apart (defaults 1 / 24).
//! - `move <sel> <x> <z>` — replace the selection's orders with a move;
//!   `queue <sel> <x> <z>` appends one instead (shift-move).
//! - `attackmove <sel> <x> <z>`, `attack <sel> <id>`, `stop <sel>`,
//!   `kill <sel>` (removes the units outright: no death blast, no death
//!   script), `hp <sel> <value>` (sets health; `0` kills through the
//!   normal death path, blast and all).
//! - `cam <x> <z> [distance]` — snap the camera onto that ground point.
//! - `shot <name>` — screenshot to `<dir>/<name>.png`.
//! - `build <sel> <kind> [count]` — queue `count` (default 1) of `kind`
//!   at the factories; `nx <sel> <x> <z>` — cast the selection's aimed
//!   abilities at that point.
//! - `dispatch <sel> <x> <z> [alt]` — the Ports and Connections dispatch
//!   Packets towards that point (`alt`: until the buffer is empty);
//!   `enter <sel> [<id>]` — the Packets walk into teleporter `<id>`, or
//!   without one enter the nearest in reach (the E key's order).
//! - `select <sel>` — the player's selection becomes these units (the
//!   command panel, highlights and bars follow it).
//! - The player's mouse and keyboard, for the HUD: `mouse <sx> <sy>`
//!   puts the cursor at that window position (logical pixels from the
//!   top left), `point <x> <z>` over that ground point (`point <id>`:
//!   over that unit) and `button <name>` on the command panel's button
//!   of that name; `click
//!   [left|right|middle]` presses and releases there, `press` /
//!   `release <button>` do one half (a drag: `wait` after the press,
//!   move, `wait`, release); `key <key>` taps a key (`d`, `e`, `esc`,
//!   `1`, `f1`…), `hold` / `unhold <key>` keeps one down (`alt`,
//!   `shift`, `ctrl`). Each press and release takes a frame of its own,
//!   as a player's would, and the commands after them wait until the
//!   HUD has seen them (whatever the game speed).
//! - `ui` — log what the player sees: the cursor (a screenshot has
//!   none), the armed order, the selection, the command panel's page
//!   and the packet buffer.
//! - `vents <x> <z> [count]` — log the datavents nearest that point
//!   (default 8), and which are taken.
//! - `speed <factor>`, `pause`, `resume` — the game clock.
//! - `room` — "Back to room" (a networked match: resign, back to the
//!   lobby with the same connection); `menu` — leave to the main menu.
//!
//! When no tick runs — a paused match, one that ended (the host left),
//! the menu after `room` — the first `resume`, `quit`, `room` or `menu`
//! among the commands is taken out of turn, as a key or button press
//! would be (`room` and `menu` once the bot has played a match: before,
//! their buttons are not on screen); the others wait for the ticks. The frame count starts over
//! with each match the bot plays (the log's `team <n>` marks it).
//! - `resign` — give up (the bot's owner leaves play: with an ally in
//!   play its units wait for a take, else they blow up within 4 s).
//! - `take <owner>` — the TAKE button: every remaining unit of `owner`,
//!   an ally that has left play.
//! - `god on|off` — command every team (on from the start).
//! - `wait <frames>` — hold the following commands that many sim frames.
//! - `dump` — write `state.txt` now; `every <frames>` — and that often.
//! - `trace <sel> on|off` — per-frame CSV of each unit's movement state
//!   to `<dir>/trace_<id>.csv`.
//! - `path <sel>` — log the units' remaining waypoints.
//! - `nav <x> <z>` — log the nav grid's view of that square (per bucket:
//!   slope, speed, structure blocking).
//! - `line <x1> <z1> <x2> <z2>` — log whether a LIGHT mover can drive
//!   that segment straight (`DoRawSearch`).
//! - `stats [army|units|production|apm]` — open the game-over panel on
//!   its Statistics page (`stats`), on that stat.
//! - `ping <token>` — echo `ack <token>` to the log (a sync point).
//! - `quit` — exit the game.
//!
//! Everything that changes the match goes through the command bus as
//! the local player (team 0), like mouse input: orders as
//! [`SimCommand`]s; spawn, kill, hp and commanding other teams as
//! cheats ([`DevCommand`]: the bot's match has one human seat, so they
//! are allowed). Commands apply on the tick they are read. A `spawn`
//! holds the following lines one tick, until the new units exist.
//!
//! Launched into a room (`--room` / `KP_ROOM`), the bot boots no
//! skirmish and plays the networked match the lobby starts as this
//! game's seat, with god mode off; `cargo testbox` (`xtask`) drives a
//! bot in every game of the room. Spawns and other cheats then need the
//! room's cheats on (`KP_AUTO_CHEATS=1`, `net::autopilot`).
//!
//! Not compiled for wasm (no disk).

use bevy::math::ops;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::PathBuf;

use bevy::ecs::system::SystemParam;
use bevy::input::InputSystems;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use strum::VariantArray;

use crate::game_setup::{ActiveMatch, AppState, DevOptions, GameSetup, PlayerSpec};
use crate::interaction::ability::OrderCursorModes;
use crate::interaction::cursor::CursorState;
use crate::interaction::ground_move::GroundMover;
use crate::interaction::movement::{
    CommandQueue, MovePath, MoveTarget, NavGridSet, ground_clamp_system, movement_system,
};
use crate::interaction::selection::Selected;
use crate::net::control::{DevSpawned, MAX_SPEED_PERCENT, MIN_SPEED_PERCENT};
use crate::net::{
    CommandBus, DevCommand, FactoryOp, IdList, NetId, NetPos, Order, SimCommand, Units,
};
use crate::rendering::camera::{RtsCamera, RtsCameraState};
use crate::sim::{DetFloat, GAME_SPEED};
use crate::terrain::geovent::{GeoventSmoker, VentClaim};
use crate::terrain::heightmap::Heightmap;
use crate::ui::hud::command_panel::{PanelCommands, PanelPage, button_centre};
use crate::units::combat::Dying;
use crate::units::components::{Faction, Health, TeamId, UnitStats, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::mechanics::network_buffer::PacketBuffer;
use crate::units::player::LocalTeam;

/// Frames of menu before starting the skirmish (the map catalog must
/// be scanned first).
const MENU_FRAMES: u32 = 60;

pub struct BotPlugin;

impl Plugin for BotPlugin {
    fn build(&self, app: &mut App) {
        let Some(dev) = app.world().get_resource::<DevOptions>() else {
            return;
        };
        let Some(dir) = dev.bot.clone() else {
            return;
        };
        let _ = fs::create_dir_all(&dir);
        // A fresh log and command file per run.
        let _ = fs::write(dir.join("log"), "");
        if !dir.join("cmd").exists() {
            let _ = fs::write(dir.join("cmd"), "");
        }
        app.insert_resource(Bot {
            dir,
            map: dev
                .bot_map
                .clone()
                .unwrap_or_else(|| "Data_Cache_L1".into()),
            enemy: dev.bot_enemy,
            // In a room the lobby starts the match.
            started: crate::net::lobby::launch_room().is_some(),
            menu_frames: 0,
            frame: 0,
            cmd_offset: 0,
            pending: VecDeque::new(),
            wait_until: 0,
            dump_every: 0,
            traced: Vec::new(),
            last_spawned: None,
            // Networked: the bot commands its own seat only.
            god: crate::net::lobby::launch_room().is_some(),
            team: 0,
            seen_frame: 0,
            stalled_for: 0,
            played: false,
        })
        .init_resource::<DevSpawned>()
        .init_resource::<BotInput>()
        .add_systems(Update, (bot_boot, bot_stalled))
        // After the window's own input: the HUD reads the bot's presses
        // as it reads a player's, this frame.
        .add_systems(PreUpdate, bot_input.after(InputSystems))
        .add_systems(
            FixedUpdate,
            (
                // Issues before the apply system: commands land on the
                // tick they are read.
                bot_tick
                    .before(crate::net::SimCommandSet)
                    .before(movement_system),
                bot_trace.after(ground_clamp_system),
            )
                .run_if(in_state(AppState::InGame))
                .run_if(own_match),
        );
    }
}

/// The bot's match is the running one. `InGame` comes first: the menu's
/// demo world keeps running until the new map is swapped in
/// (`ActiveMatch`), and the bot must neither count its ticks nor send
/// it commands.
fn own_match(active: Option<Res<ActiveMatch>>) -> bool {
    active.is_some_and(|m| !m.0.demo)
}

#[derive(Resource)]
struct Bot {
    dir: PathBuf,
    map: String,
    enemy: bool,
    started: bool,
    menu_frames: u32,
    /// Sim frames since the match started.
    frame: u64,
    /// Bytes of `cmd` already consumed.
    cmd_offset: u64,
    pending: VecDeque<String>,
    wait_until: u64,
    dump_every: u64,
    traced: Vec<Entity>,
    last_spawned: Option<Entity>,
    /// God mode requested (once, at the first tick).
    god: bool,
    /// The local player's team (`LocalTeam`), as of this tick.
    team: u8,
    /// `bot.frame` at the last `Update`, and how many `Update`s since
    /// it moved: a stalled match ([`bot_stalled`]).
    seen_frame: u64,
    stalled_for: u32,
    /// A tick of the bot's own match ran.
    played: bool,
}

impl Bot {
    fn log(&self, line: &str) {
        if let Ok(mut f) = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(self.dir.join("log"))
        {
            let _ = writeln!(f, "[{}] {line}", self.frame);
        }
        info!("KP_BOT: {line}");
    }

    /// New complete lines appended to `cmd` since the last read. Runs
    /// every tick: the file's length says whether anything was added,
    /// and only the new end of it is read.
    fn read_new_commands(&mut self) {
        let path = self.dir.join("cmd");
        let Ok(len) = fs::metadata(&path).map(|m| m.len()) else {
            return;
        };
        if len < self.cmd_offset {
            // Truncated: start over.
            self.cmd_offset = 0;
        }
        if len == self.cmd_offset {
            return;
        }
        let mut bytes = Vec::new();
        let read = fs::File::open(&path).and_then(|mut f| {
            f.seek(SeekFrom::Start(self.cmd_offset))?;
            f.read_to_end(&mut bytes)
        });
        if read.is_err() {
            return;
        }
        let mut rest = &bytes[..];
        while let Some(nl) = rest.iter().position(|&b| b == b'\n') {
            let line = String::from_utf8_lossy(&rest[..nl]).trim().to_string();
            self.cmd_offset += nl as u64 + 1;
            rest = &rest[nl + 1..];
            if !line.is_empty() && !line.starts_with('#') {
                self.pending.push_back(line);
            }
        }
    }
}

/// Start the skirmish once the menu has scanned the map catalog.
fn bot_boot(
    mut bot: ResMut<Bot>,
    mut commands: Commands,
    catalog: Option<Res<crate::map_loading::MapCatalog>>,
    mut app_state: ResMut<NextState<AppState>>,
) {
    if bot.started {
        return;
    }
    bot.menu_frames += 1;
    if bot.menu_frames < MENU_FRAMES {
        return;
    }
    let Some(catalog) = catalog else {
        return;
    };
    let names = catalog.names();
    if !names.iter().any(|n| *n == bot.map) {
        bot.log(&format!("map {} not in catalog {names:?}", bot.map));
    }
    let mut players = vec![PlayerSpec {
        faction: Faction::System,
        ally_team: 0,
        ai: false,
    }];
    if bot.enemy {
        players.push(PlayerSpec {
            faction: Faction::Hacker,
            ally_team: 1,
            ai: true,
        });
    }
    commands.insert_resource(GameSetup {
        map: bot.map.clone(),
        players,
        difficulty: 1,
        demo: false,
        showcase: None,
        sandbox: true,
        seed: crate::game_setup::random_seed(),
        rules: Default::default(),
    });
    app_state.set(AppState::InGame);
    bot.started = true;
    bot.log(&format!("booting skirmish on {}", bot.map));
}

/// What a selector resolves to.
fn select(sel: &str, bot: &Bot, units: &Query<UnitView>) -> Vec<Entity> {
    let mut out = Vec::new();
    if sel == "mine" {
        out.extend(
            units
                .iter()
                .filter(|u| u.team.0 == bot.team && u.stats.speed > 0.0)
                .map(|u| u.entity),
        );
    } else if sel == "all" {
        out.extend(
            units
                .iter()
                .filter(|u| u.stats.speed > 0.0)
                .map(|u| u.entity),
        );
    } else if sel == "last" {
        out.extend(bot.last_spawned);
    } else if let Some(kind) = sel.strip_prefix("mine:") {
        if let Some(kind) = parse_kind(kind) {
            out.extend(
                units
                    .iter()
                    .filter(|u| u.team.0 == bot.team && u.kind.0 == kind)
                    .map(|u| u.entity),
            );
        }
    } else if let Some(kind) = sel.strip_prefix("kind:") {
        if let Some(kind) = parse_kind(kind) {
            out.extend(units.iter().filter(|u| u.kind.0 == kind).map(|u| u.entity));
        }
    } else if let Some(team) = sel.strip_prefix("team:") {
        if let Ok(team) = team.parse::<u8>() {
            out.extend(
                units
                    .iter()
                    .filter(|u| u.team.0 == team && u.stats.speed > 0.0)
                    .map(|u| u.entity),
            );
        }
    } else {
        for id in sel.split(',') {
            if let Ok(id) = id.parse::<u32>()
                && let Some(u) = units.iter().find(|u| u.entity.index_u32() == id)
            {
                out.push(u.entity);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn parse_kind(name: &str) -> Option<UnitKind> {
    UnitKind::VARIANTS.iter().copied().find(|k| {
        k.unitname().eq_ignore_ascii_case(name) || format!("{k:?}").eq_ignore_ascii_case(name)
    })
}

#[derive(bevy::ecs::query::QueryData)]
struct UnitView {
    entity: Entity,
    id: &'static NetId,
    kind: &'static UnitType,
    team: &'static TeamId,
    stats: &'static UnitStats,
    transform: &'static Transform,
    health: Option<&'static Health>,
    mover: Option<&'static GroundMover>,
    path: Option<&'static MovePath>,
    target: Option<&'static MoveTarget>,
    queue: Option<&'static CommandQueue>,
    dying: Has<Dying>,
}

fn f(s: Option<&str>) -> Option<f32> {
    s.and_then(|v| v.parse().ok())
}

/// The selection's units for a command.
fn ids(targets: &[Entity], units: &Query<UnitView>) -> IdList {
    IdList::new(
        targets
            .iter()
            .filter_map(|e| units.get(*e).ok().map(|u| *u.id)),
    )
}

#[allow(clippy::too_many_arguments)]
fn bot_tick(
    mut bot: ResMut<Bot>,
    mut commands: Commands,
    mut bus: ResMut<CommandBus>,
    mut spawned: ResMut<DevSpawned>,
    registry: Res<UnitRegistry>,
    heightmap: Option<Res<Heightmap>>,
    nav: Option<Res<NavGridSet>>,
    units: Query<UnitView>,
    mut camera: Query<&mut RtsCameraState, With<RtsCamera>>,
    mut exit: MessageWriter<AppExit>,
    local: Option<Res<LocalTeam>>,
    active: Res<ActiveMatch>,
    mut ui: BotUi,
) {
    let Some(hm) = heightmap.as_deref() else {
        return;
    };
    if active.is_changed() {
        // A new match (a rematch from the room): count from its start.
        bot.frame = 0;
        bot.wait_until = 0;
    }
    bot.frame += 1;
    bot.played = true;
    bot.team = local.map_or(0, |t| t.0);
    if bot.frame == 1 {
        bot.log(&format!("team {}", bot.team));
    }
    if !bot.god {
        // The bot commands every team (`team:1`, spawned enemies).
        bus.issue(SimCommand::Dev(DevCommand::GodMode { on: true }));
        bot.god = true;
    }
    // The units the last `spawn` made.
    if !spawned.0.is_empty() {
        let new: Vec<u32> = spawned.0.iter().map(|e| e.index_u32()).collect();
        bot.last_spawned = spawned.0.last().copied();
        spawned.0.clear();
        bot.log(&format!("spawned: {new:?}"));
    }
    bot.read_new_commands();
    if bot.dump_every > 0 && bot.frame.is_multiple_of(bot.dump_every) {
        dump_state(&bot, &units);
    }
    while bot.frame >= bot.wait_until && ui.input.settled() {
        let Some(line) = bot.pending.pop_front() else {
            break;
        };
        let mut it = line.split_whitespace();
        let Some(cmd) = it.next() else { continue };
        match cmd {
            "wait" => {
                let n = it.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(1);
                bot.wait_until = bot.frame + n;
            }
            "spawn" => {
                let kind = it.next().and_then(parse_kind);
                let team = it.next().and_then(|v| match v {
                    "me" => Some(bot.team),
                    v => v.parse::<u8>().ok(),
                });
                let (x, z) = (f(it.next()), f(it.next()));
                let count = it.next().and_then(|v| v.parse::<usize>().ok()).unwrap_or(1);
                let spacing = f(it.next()).unwrap_or(24.0);
                let (Some(kind), Some(team), Some(x), Some(z)) = (kind, team, x, z) else {
                    bot.log(&format!("bad spawn: {line}"));
                    continue;
                };
                bus.issue(SimCommand::Dev(DevCommand::Spawn {
                    kind,
                    team,
                    at: NetPos::quantize(hm.place(x, z)),
                    count: count.clamp(1, 100) as u16,
                    spacing: spacing.det_clamp(0.0, 1000.0) as u16,
                }));
                bot.log(&format!("spawn {count}x {kind:?} team {team} at {x},{z}"));
                // The units exist after this tick's apply.
                bot.wait_until = bot.frame + 1;
            }
            "move" | "queue" | "attackmove" => {
                let sel = it.next().unwrap_or("");
                let (Some(x), Some(z)) = (f(it.next()), f(it.next())) else {
                    bot.log(&format!("bad {cmd}: {line}"));
                    continue;
                };
                let pos = NetPos::quantize(hm.place(x, z));
                let targets = select(sel, &bot, &units);
                bus.issue(SimCommand::Order {
                    units: Units::List(ids(&targets, &units)),
                    order: if cmd == "attackmove" {
                        Order::AttackMove(pos)
                    } else {
                        Order::Move(pos)
                    },
                    // Appends behind a busy unit's orders, else replaces.
                    queue: cmd == "queue",
                });
                bot.log(&format!("{cmd} {} units -> {x},{z}", targets.len()));
            }
            "attack" => {
                let sel = it.next().unwrap_or("");
                let target = it
                    .next()
                    .and_then(|v| v.parse::<u32>().ok())
                    .and_then(|id| units.iter().find(|u| u.entity.index_u32() == id));
                let Some(target) = target else {
                    bot.log(&format!("bad attack: {line}"));
                    continue;
                };
                let (target_entity, target_id) = (target.entity, *target.id);
                let targets = select(sel, &bot, &units);
                bus.issue(SimCommand::Order {
                    units: Units::List(ids(&targets, &units)),
                    order: Order::AttackUnit(target_id),
                    queue: false,
                });
                bot.log(&format!(
                    "attack {} units -> {}",
                    targets.len(),
                    target_entity.index_u32()
                ));
            }
            "build" => {
                let sel = it.next().unwrap_or("");
                let kind = it.next().and_then(parse_kind);
                let count = it.next().and_then(|v| v.parse::<usize>().ok()).unwrap_or(1);
                let Some(kind) = kind else {
                    bot.log(&format!("bad build: {line}"));
                    continue;
                };
                let targets = select(sel, &bot, &units);
                bus.issue(SimCommand::Factory {
                    factories: Units::List(ids(&targets, &units)),
                    kind,
                    op: FactoryOp::Add(count.min(100) as u16),
                });
                bot.log(&format!(
                    "build {count}x {kind:?} at {} factories",
                    targets.len()
                ));
            }
            "nx" => {
                let sel = it.next().unwrap_or("");
                let (Some(x), Some(z)) = (f(it.next()), f(it.next())) else {
                    bot.log(&format!("bad nx: {line}"));
                    continue;
                };
                let target = NetPos::quantize(hm.place(x, z));
                let targets = select(sel, &bot, &units);
                bus.issue(SimCommand::Cast {
                    units: Units::List(ids(&targets, &units)),
                    target,
                    auto: false,
                });
                bot.log(&format!("nx {} units -> {x},{z}", targets.len()));
            }
            "dispatch" => {
                let sel = it.next().unwrap_or("");
                let (Some(x), Some(z)) = (f(it.next()), f(it.next())) else {
                    bot.log(&format!("bad dispatch: {line}"));
                    continue;
                };
                let auto = it.next() == Some("alt");
                let targets = select(sel, &bot, &units);
                bus.issue(SimCommand::Cast {
                    units: Units::List(ids(&targets, &units)),
                    target: NetPos::quantize(hm.place(x, z)),
                    auto,
                });
                bot.log(&format!(
                    "dispatch from {} teleporters -> {x},{z}{}",
                    targets.len(),
                    if auto { " (alt)" } else { "" }
                ));
            }
            "enter" => {
                let targets = select(it.next().unwrap_or(""), &bot, &units);
                let units_list = Units::List(ids(&targets, &units));
                let teleporter = it
                    .next()
                    .and_then(|v| v.parse::<u32>().ok())
                    .and_then(|id| units.iter().find(|u| u.entity.index_u32() == id));
                match teleporter {
                    Some(t) => {
                        bus.issue(SimCommand::Order {
                            units: units_list,
                            order: Order::Enter(*t.id),
                            queue: false,
                        });
                        bot.log(&format!(
                            "enter {} units -> {}",
                            targets.len(),
                            t.entity.index_u32()
                        ));
                    }
                    None => {
                        bus.issue(SimCommand::Enter { units: units_list });
                        bot.log(&format!("enter {} units (nearest)", targets.len()));
                    }
                }
            }
            "select" => {
                let targets = select(it.next().unwrap_or(""), &bot, &units);
                for e in &ui.selected {
                    if !targets.contains(&e) {
                        commands.entity(e).remove::<Selected>();
                    }
                }
                for &e in &targets {
                    commands.entity(e).insert(Selected);
                }
                bot.log(&format!("select {} units", targets.len()));
            }
            "mouse" => match (f(it.next()), f(it.next())) {
                (Some(x), Some(y)) => ui.input.steps.push_back(InputStep::Cursor(Vec2::new(x, y))),
                _ => bot.log(&format!("bad mouse: {line}")),
            },
            "point" => {
                let (a, b) = (it.next(), f(it.next()));
                // `point <id>`: where that unit stands now.
                let unit = b
                    .is_none()
                    .then(|| a.and_then(|v| v.parse::<u32>().ok()))
                    .flatten()
                    .and_then(|id| units.iter().find(|u| u.entity.index_u32() == id));
                let (x, z) = match (unit, f(a), b) {
                    (Some(u), ..) => (u.transform.translation.x, u.transform.translation.z),
                    (None, Some(x), Some(z)) => (x, z),
                    _ => {
                        bot.log(&format!("bad point: {line}"));
                        continue;
                    }
                };
                let at = ui
                    .view
                    .single()
                    .ok()
                    .and_then(|(cam, tf)| cam.world_to_viewport(tf, hm.place(x, z)).ok());
                match at {
                    Some(at) => {
                        ui.input.steps.push_back(InputStep::Cursor(at));
                        bot.log(&format!("point {x},{z}: cursor at {:.0},{:.0}", at.x, at.y));
                    }
                    None => bot.log(&format!("point {x},{z}: not on screen")),
                }
            }
            "button" => {
                let name = it.collect::<Vec<_>>().join(" ");
                let view = ui.windows.single().map(|w| w.size()).ok();
                let at = ui.panel.as_deref().zip(view).and_then(|(panel, view)| {
                    button_centre(panel, ui.page.as_ref().map_or(0, |p| p.0), &name, view)
                });
                match at {
                    Some(at) => {
                        ui.input.steps.push_back(InputStep::Cursor(at));
                        bot.log(&format!("button {name}: cursor at {:.0},{:.0}", at.x, at.y));
                    }
                    None => bot.log(&format!("button {name}: not on the panel ({})", ui.line())),
                }
            }
            "click" | "press" | "release" => {
                let Some(button) = parse_button(it.next()) else {
                    bot.log(&format!("bad {cmd}: {line}"));
                    continue;
                };
                if cmd != "release" {
                    ui.input.steps.push_back(InputStep::Mouse(button, true));
                }
                if cmd != "press" {
                    ui.input.steps.push_back(InputStep::Mouse(button, false));
                }
            }
            "key" | "hold" | "unhold" => {
                let Some(key) = it.next().and_then(parse_key) else {
                    bot.log(&format!("bad {cmd}: {line}"));
                    continue;
                };
                if cmd != "unhold" {
                    ui.input.steps.push_back(InputStep::Key(key, true));
                }
                if cmd != "hold" {
                    ui.input.steps.push_back(InputStep::Key(key, false));
                }
            }
            "vents" => {
                let (Some(x), Some(z)) = (f(it.next()), f(it.next())) else {
                    bot.log(&format!("bad vents: {line}"));
                    continue;
                };
                let count = it.next().and_then(|v| v.parse().ok()).unwrap_or(8);
                let mut vents: Vec<_> = ui
                    .vents
                    .iter()
                    .map(|(v, taken)| (v.pos.xz().distance(Vec2::new(x, z)), v.pos, taken))
                    .collect();
                vents.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut s = format!("vents near {x},{z}:");
                for (d, pos, taken) in vents.into_iter().take(count) {
                    let _ = write!(
                        s,
                        " ({:.0},{:.0} d={d:.0}{})",
                        pos.x,
                        pos.z,
                        if taken { " taken" } else { "" }
                    );
                }
                bot.log(&s);
            }
            "ui" => {
                let mut s = ui.line();
                if let Some(buffer) = ui.buffer.as_deref() {
                    let _ = write!(s, "; buffer {}", buffer.peek(bot.team));
                }
                bot.log(&format!("ui: {s}"));
            }
            "stop" => {
                let targets = select(it.next().unwrap_or(""), &bot, &units);
                bus.issue(SimCommand::Stop {
                    units: Units::List(ids(&targets, &units)),
                });
                bot.log(&format!("stop {} units", targets.len()));
            }
            "hp" => {
                let sel = it.next().unwrap_or("");
                let Some(value) = f(it.next()) else {
                    bot.log(&format!("bad hp: {line}"));
                    continue;
                };
                let targets = select(sel, &bot, &units);
                bus.issue(SimCommand::Dev(DevCommand::SetHealth {
                    units: ids(&targets, &units),
                    hp: value.det_max(0.0).round() as u32,
                }));
                bot.log(&format!("hp {value} on {} units", targets.len()));
            }
            "kill" => {
                let targets = select(it.next().unwrap_or(""), &bot, &units);
                bus.issue(SimCommand::Dev(DevCommand::Kill {
                    units: ids(&targets, &units),
                }));
                bot.log(&format!("kill {} units", targets.len()));
            }
            "cam" => {
                let (Some(x), Some(z)) = (f(it.next()), f(it.next())) else {
                    continue;
                };
                let dist = f(it.next()).unwrap_or(500.0);
                let pitch = f(it.next()).unwrap_or(70.0).to_radians();
                let yaw = f(it.next()).unwrap_or(0.0).to_radians();
                for mut cam in &mut camera {
                    cam.snap_to(hm.place(x, z), dist);
                    cam.pitch = pitch;
                    cam.yaw = yaw;
                }
            }
            "shot" => {
                let name = it.next().unwrap_or("shot");
                let path = bot.dir.join(format!("{name}.png"));
                crate::ui::save_screenshot(&mut commands, path.clone());
                bot.log(&format!("shot {}", path.display()));
            }
            "speed" => {
                if let Some(s) = f(it.next()).filter(|s| s.is_finite() && *s > 0.0) {
                    let percent = ((s * 100.0).round() as u32)
                        .clamp(MIN_SPEED_PERCENT.into(), MAX_SPEED_PERCENT.into());
                    bus.issue(SimCommand::SetSpeed {
                        percent: percent as u16,
                    });
                    bot.log(&format!("speed {percent}%"));
                }
            }
            "pause" => {
                bus.issue(SimCommand::Pause { on: true });
                bot.log("pause");
            }
            "resume" => {
                bus.issue(SimCommand::Pause { on: false });
                bot.log("resume");
            }
            "resign" => bus.issue(SimCommand::Resign),
            "take" => match it.next().and_then(|v| v.parse::<u8>().ok()) {
                Some(from) => bus.issue(SimCommand::Take { from }),
                None => bot.log(&format!("bad take: {line}")),
            },
            "god" => bus.issue(SimCommand::Dev(DevCommand::GodMode {
                on: it.next() != Some("off"),
            })),
            "dump" => dump_state(&bot, &units),
            "every" => {
                bot.dump_every = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            }
            "trace" => {
                let targets = select(it.next().unwrap_or(""), &bot, &units);
                let on = it.next() != Some("off");
                for e in targets {
                    bot.traced.retain(|t| *t != e);
                    if on {
                        bot.traced.push(e);
                        let path = bot.dir.join(format!("trace_{}.csv", e.index_u32()));
                        let _ = fs::write(
                            &path,
                            "frame,x,y,z,heading_deg,speed,turn_speed,wp_dist,at_goal,at_end,idling,idle_updates,stuck,path_cur,path_len,progress,goal_x,goal_z\n",
                        );
                    }
                }
            }
            "path" => {
                let targets = select(it.next().unwrap_or(""), &bot, &units);
                for e in targets {
                    let Ok(u) = units.get(e) else { continue };
                    let mut s = format!("path {}:", e.index_u32());
                    match u.path {
                        Some(p) => {
                            let _ = write!(
                                s,
                                " cur={} goal=({:.0},{:.0}) reached_goal={}",
                                p.current, p.goal.x, p.goal.z, p.reached_goal
                            );
                            for w in &p.waypoints[p.current.min(p.waypoints.len())..] {
                                let _ = write!(s, " ({:.0},{:.0})", w.x, w.z);
                            }
                        }
                        None => s.push_str(" none"),
                    }
                    bot.log(&s);
                }
            }
            "nav" => {
                let (Some(x), Some(z)) = (f(it.next()), f(it.next())) else {
                    continue;
                };
                let mut s = format!("nav {x},{z}: h={:.1}", hm.sample(x, z));
                if let Some(nav) = nav.as_deref() {
                    for b in &nav.buckets {
                        let sq = ((x / 8.0) as u32, (z / 8.0) as u32);
                        let _ = write!(
                            s,
                            " [cap {:.3}: speed {:.3} slope {:?}]",
                            b.max_slope,
                            b.speed_map.get(sq.0, sq.1),
                            nav.square_slope(b.max_slope, Vec2::new(x, z))
                        );
                    }
                    let _ = write!(
                        s,
                        " structure(light)={} structure(heavy)={}",
                        nav.footprint_blocked(x, z, 1, 40.0),
                        nav.footprint_blocked(x, z, 3, 300.0)
                    );
                }
                bot.log(&s);
            }
            "scan" => {
                // ASCII map of the LIGHT class's view: `#` terrain
                // blocked, `S` structure, `.` slow (<0.5), ` ` open.
                let (Some(x0), Some(z0), Some(x1), Some(z1)) =
                    (f(it.next()), f(it.next()), f(it.next()), f(it.next()))
                else {
                    continue;
                };
                let step = it
                    .next()
                    .and_then(|v| v.parse::<i32>().ok())
                    .unwrap_or(1)
                    .max(1);
                let Some(nav) = nav.as_deref() else { continue };
                let cap = registry.max_slope_ratio(UnitKind::Bit);
                let Some(b) = nav.bucket(cap) else { continue };
                let (sx0, sz0) = ((x0 / 8.0) as i32, (z0 / 8.0) as i32);
                let (sx1, sz1) = ((x1 / 8.0) as i32, (z1 / 8.0) as i32);
                let mut s = format!(
                    "scan squares x {sx0}..{sx1} z {sz0}..{sz1} step {step} (cap {cap:.3})\n"
                );
                for sz in (sz0..=sz1.min(sz0 + 80 * step)).step_by(step as usize) {
                    for sx in (sx0..=sx1.min(sx0 + 120 * step)).step_by(step as usize) {
                        let sp = b.speed_map.get(sx.max(0) as u32, sz.max(0) as u32);
                        let c = if nav.structure_square(sx, sz, 40.0) {
                            'S'
                        } else if sp <= 0.0 {
                            '#'
                        } else if sp < 0.5 {
                            '.'
                        } else {
                            ' '
                        };
                        s.push(c);
                    }
                    let _ = writeln!(s, "| z={}", sz * 8);
                }
                let _ = fs::write(bot.dir.join("scan.txt"), &s);
                bot.log("scan written to scan.txt");
            }
            "line" => {
                let (Some(x1), Some(z1), Some(x2), Some(z2)) =
                    (f(it.next()), f(it.next()), f(it.next()), f(it.next()))
                else {
                    continue;
                };
                let registry: &UnitRegistry = &registry;
                let cap = registry.max_slope_ratio(UnitKind::Bit);
                let clear = nav.as_deref().is_none_or(|n| {
                    n.line_clear(cap, 1, 40.0, Vec2::new(x1, z1), Vec2::new(x2, z2))
                });
                bot.log(&format!("line {x1},{z1} -> {x2},{z2}: clear={clear}"));
            }
            "stats" => {
                use crate::stats::Stat;
                let stat = match it.next() {
                    Some("units") => Some(Stat::Units),
                    Some("production") => Some(Stat::Production),
                    Some("apm") => Some(Stat::Apm),
                    Some("army") => Some(Stat::ArmyValue),
                    _ => None,
                };
                commands.queue(move |world: &mut World| crate::ui::menu::show_stats(world, stat));
                bot.log(&format!("stats {stat:?}"));
            }
            "ping" => {
                let token = it.next().unwrap_or("");
                bot.log(&format!("ack {token}"));
            }
            "quit" | "room" | "menu" => press(&bot, cmd, &mut commands, &mut exit),
            _ => bot.log(&format!("unknown command: {line}")),
        }
    }
}

/// The player's side the bot reads and works: the selection, the
/// cursor, the HUD.
#[derive(SystemParam)]
struct BotUi<'w, 's> {
    input: ResMut<'w, BotInput>,
    selected: Query<'w, 's, Entity, With<Selected>>,
    view: Query<'w, 's, (&'static Camera, &'static GlobalTransform), With<RtsCamera>>,
    windows: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    panel: Option<Res<'w, PanelCommands>>,
    page: Option<Res<'w, PanelPage>>,
    cursor: Option<Res<'w, CursorState>>,
    modes: Option<Res<'w, OrderCursorModes>>,
    buffer: Option<Res<'w, PacketBuffer>>,
    vents: Query<'w, 's, (&'static GeoventSmoker, Has<VentClaim>)>,
}

impl BotUi<'_, '_> {
    /// The cursor, the armed order, the selection and the command
    /// panel's page, as the player sees them.
    fn line(&self) -> String {
        let mut s = format!(
            "cursor {:?}, armed {:?}, {} selected",
            self.cursor.as_deref().and_then(CursorState::shown),
            self.modes.as_deref().and_then(|m| m.mode),
            self.selected.iter().count(),
        );
        if let Some(panel) = self.panel.as_deref() {
            let page = self.page.as_ref().map_or(0, |p| p.0);
            let _ = write!(s, ", panel page {}/{}:", page + 1, panel.pages.len());
            for d in &panel.list {
                let _ = write!(
                    s,
                    " [{}{}]",
                    d.label(),
                    if d.disabled { " (disabled)" } else { "" }
                );
            }
        }
        s
    }
}

/// The player's mouse and keyboard as the bot works them, applied by
/// [`bot_input`].
#[derive(Resource, Default)]
struct BotInput {
    steps: VecDeque<InputStep>,
    /// Frames until the HUD has run on the last step applied.
    settling: u8,
}

impl BotInput {
    /// Every step applied, and the HUD has seen the last one.
    fn settled(&self) -> bool {
        self.steps.is_empty() && self.settling == 0
    }
}

#[derive(Clone, Copy, Debug)]
enum InputStep {
    /// The cursor to this window position (logical pixels).
    Cursor(Vec2),
    Mouse(MouseButton, bool),
    Key(KeyCode, bool),
}

/// The bot's input steps, after the window's: cursor moves at once, then
/// at most one press or release a frame, so the HUD sees each as a
/// `just_pressed` / `just_released`. The cursor stays where the bot
/// put it until the real one moves over the window.
fn bot_input(
    mut input: ResMut<BotInput>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
) {
    input.settling = input.settling.saturating_sub(1);
    while let Some(step) = input.steps.pop_front() {
        // This frame's `Update` reads it; the bot goes on after the next.
        input.settling = 2;
        match step {
            InputStep::Cursor(at) => {
                if let Ok(mut window) = windows.single_mut() {
                    window.set_cursor_position(Some(at));
                }
                continue;
            }
            InputStep::Mouse(button, true) => mouse.press(button),
            InputStep::Mouse(button, false) => mouse.release(button),
            InputStep::Key(key, true) => keys.press(key),
            InputStep::Key(key, false) => keys.release(key),
        }
        break;
    }
}

fn parse_button(name: Option<&str>) -> Option<MouseButton> {
    match name.unwrap_or("left") {
        "left" => Some(MouseButton::Left),
        "right" => Some(MouseButton::Right),
        "middle" => Some(MouseButton::Middle),
        _ => None,
    }
}

/// A key by name: a letter or digit, `f1`…`f12`, or `esc`, `enter`,
/// `space`, `tab`, `shift`, `ctrl`, `alt`, `backspace`, `delete`, `up`,
/// `down`, `left`, `right`.
fn parse_key(name: &str) -> Option<KeyCode> {
    use KeyCode::*;
    const LETTERS: [KeyCode; 26] = [
        KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO,
        KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ,
    ];
    const DIGITS: [KeyCode; 10] = [
        Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
    ];
    const FKEYS: [KeyCode; 12] = [F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12];
    let name = name.to_ascii_lowercase();
    Some(match name.as_str() {
        "esc" | "escape" => Escape,
        "enter" | "return" => Enter,
        "space" => Space,
        "tab" => Tab,
        "shift" => ShiftLeft,
        "ctrl" | "control" => ControlLeft,
        "alt" => AltLeft,
        "backspace" => Backspace,
        "delete" => Delete,
        "up" => ArrowUp,
        "down" => ArrowDown,
        "left" => ArrowLeft,
        "right" => ArrowRight,
        _ => match name.as_bytes() {
            [c @ b'a'..=b'z'] => LETTERS[usize::from(c - b'a')],
            [c @ b'0'..=b'9'] => DIGITS[usize::from(c - b'0')],
            [b'f', n @ ..] => {
                let n: usize = std::str::from_utf8(n).ok()?.parse().ok()?;
                *FKEYS.get(n.checked_sub(1)?)?
            }
            _ => return None,
        },
    })
}

/// `Update`s without a tick before the bot counts its match as stalled
/// (half a second at 60 fps; a paused match is at once).
const STALLED: u32 = 30;

/// When no tick runs `bot_tick` (paused, ended, in the menu): the first
/// `resume`, `quit`, `room` or `menu` among the commands, out of turn.
fn bot_stalled(
    mut bot: ResMut<Bot>,
    control: Option<Res<crate::net::MatchControl>>,
    state: Res<State<AppState>>,
    mut bus: ResMut<CommandBus>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
    if !bot.started {
        return;
    }
    if bot.frame != bot.seen_frame {
        bot.seen_frame = bot.frame;
        bot.stalled_for = 0;
    } else {
        bot.stalled_for = bot.stalled_for.saturating_add(1);
    }
    let paused = *state.get() == AppState::InGame && control.is_some_and(|c| c.paused);
    if !paused && bot.stalled_for < STALLED {
        return;
    }
    bot.read_new_commands();
    let played = bot.played;
    let Some(i) = bot.pending.iter().position(|l| match l.as_str() {
        "resume" => paused,
        "quit" => true,
        "room" | "menu" => played,
        _ => false,
    }) else {
        return;
    };
    let line = bot.pending.remove(i).expect("found above");
    if line == "resume" {
        bus.issue(SimCommand::Pause { on: false });
        bot.log("resume");
    } else {
        press(&bot, &line, &mut commands, &mut exit);
    }
}

/// `quit`, `room`, `menu`: what the window's close button and the Esc
/// overlay's plates do.
fn press(bot: &Bot, cmd: &str, commands: &mut Commands, exit: &mut MessageWriter<AppExit>) {
    bot.log(cmd);
    match cmd {
        "quit" => {
            exit.write(AppExit::Success);
        }
        "room" => commands.queue(crate::ui::menu::press_back_to_room),
        _ => commands.queue(crate::ui::menu::press_go_to_menu),
    }
}

fn dump_state(bot: &Bot, units: &Query<UnitView>) {
    let mut s = format!(
        "frame {} t={:.2}s\n",
        bot.frame,
        bot.frame as f32 / GAME_SPEED
    );
    let mut rows: Vec<_> = units.iter().collect();
    rows.sort_by_key(|u| u.entity.index_u32());
    for u in rows {
        let p = u.transform.translation;
        let fwd = u.transform.forward().as_vec3();
        let heading = ops::atan2(fwd.x, fwd.z).to_degrees();
        let _ = write!(
            s,
            "id={} {:?} team={} pos=({:.1},{:.1},{:.1}) hdg={:.0} hp={:.0}",
            u.entity.index_u32(),
            u.kind.0,
            u.team.0,
            p.x,
            p.y,
            p.z,
            heading,
            u.health.map_or(0.0, |h| h.current),
        );
        if u.dying {
            s.push_str(" DYING");
        }
        if let Some(m) = u.mover
            && u.stats.speed > 0.0
        {
            let _ = write!(
                s,
                " speed={:.1} prog={:?} goal={:?} wp_dist={:.1} at_goal={} at_end={} idling={} idle_upd={} idle_slow={} stuck={} want_repath={}",
                m.current_speed * GAME_SPEED,
                m.progress,
                m.goal.map(|g| (g.x.round(), g.y.round())),
                m.curr_wp_dist,
                m.at_goal,
                m.at_end_of_path,
                m.idling,
                m.num_idling_updates,
                m.num_idling_slow_updates,
                m.position_stuck,
                m.want_repath,
            );
        }
        if let Some(p) = u.path {
            let _ = write!(s, " path={}/{}", p.current, p.waypoints.len());
        }
        if let Some(t) = u.target {
            let _ = write!(s, " target=({:.0},{:.0})", t.0.x, t.0.z);
        }
        if let Some(q) = u.queue {
            let _ = write!(s, " queued={}", q.commands.len());
        }
        s.push('\n');
    }
    let tmp = bot.dir.join("state.tmp");
    if fs::write(&tmp, s).is_ok() {
        let _ = fs::rename(tmp, bot.dir.join("state.txt"));
    }
}

fn bot_trace(bot: Res<Bot>, units: Query<UnitView>) {
    for e in &bot.traced {
        let Ok(u) = units.get(*e) else { continue };
        let Some(m) = u.mover else { continue };
        let p = u.transform.translation;
        let (cur, len) = u.path.map_or((0, 0), |p| (p.current, p.waypoints.len()));
        let goal = m.goal.unwrap_or(Vec2::NAN);
        let line = format!(
            "{},{:.2},{:.2},{:.2},{:.1},{:.2},{:.4},{:.1},{},{},{},{},{},{},{},{:?},{:.0},{:.0}\n",
            bot.frame,
            p.x,
            p.y,
            p.z,
            m.heading.to_radians().to_degrees(),
            m.current_speed * GAME_SPEED,
            m.turn_speed,
            m.curr_wp_dist,
            m.at_goal as u8,
            m.at_end_of_path as u8,
            m.idling as u8,
            m.num_idling_updates,
            m.position_stuck as u8,
            cur,
            len,
            m.progress,
            goal.x,
            goal.y,
        );
        if let Ok(mut f) = fs::OpenOptions::new()
            .append(true)
            .open(bot.dir.join(format!("trace_{}.csv", e.index_u32())))
        {
            let _ = f.write_all(line.as_bytes());
        }
    }
}
