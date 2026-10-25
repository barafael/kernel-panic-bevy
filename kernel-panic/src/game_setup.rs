//! Game setup: the bridge between the menus and the simulation.
//!
//! The menu writes a [`GameSetup`] (map + players + difficulty) and flips
//! [`AppState`] to `InGame`; `map_loading` consumes it on entry. This
//! mirrors the original Kernel Panic flow, where the launch menu
//! generated a start script (`GenerateSkirmish` → `RunGame` → engine
//! restart) — our "restart" is preparing the next map and then swapping
//! worlds: the old match's entities go and every match-level resource
//! starts over (`map_loading::match_scope`).

use bevy::prelude::*;

pub use crate::net::MatchRules;
use crate::rng::clock_f64;
use crate::units::components::Faction;

/// Top-level app state: the menu system owns `Menu`; the match runs in
/// `InGame` (where the result panel's `GameState` applies; the sim also
/// runs the attract demo behind the menu).
#[derive(States, Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppState {
    #[default]
    Menu,
    InGame,
}

/// One seat in the match. Seat `i` is owner `i` (Spring's *team*): its
/// units carry `TeamId(i)`, it takes the map's `i`-th start position,
/// and it holds its own unit cap, packet buffer and (for an AI seat)
/// its own AI brain (`units::teams`).
#[derive(Debug, Clone)]
pub struct PlayerSpec {
    pub faction: Faction,
    /// Ally team (Spring's *allyteam*): seats sharing one are friendly,
    /// share cloak detection and sight, and win or lose together.
    pub ally_team: u8,
    /// AI seats never run the local input path. A setup without any
    /// human seat is spectated (see [`SPECTATOR_TEAM`]).
    pub ai: bool,
}

/// Everything needed to start a match.
#[derive(Debug, Clone, Resource)]
pub struct GameSetup {
    /// Map file stem, resolved against the map catalog (`assets/maps/`).
    pub map: String,
    /// The seats, in owner order. The first human seat is the local
    /// player; the rest are AI seats (or, in a networked match, other
    /// peers' seats).
    pub players: Vec<PlayerSpec>,
    /// 1 Easy … 4 Extreme. Drives the AI fairness slack (`AiDifficulty`
    /// mirrors it at match start); enemy count comes from the grouping.
    pub difficulty: u8,
    /// Menu attract-mode demo: an all-AI skirmish with no win/lose
    /// screen — the menu's demo director (`ui::menu::demo_director`)
    /// restarts it once decided.
    pub demo: bool,
    /// Showcase mode: spawn only the given faction's homebase on
    /// Data_Cache_L1, then instruct its factory to produce one of each
    /// unit and its builder to erect one of each building.  The player
    /// controls all units; no AI enemy is present.
    pub showcase: Option<Faction>,
    /// Sandbox: no win / lose check at all (the `bot` dev tool's match,
    /// which may have a single seat).
    pub sandbox: bool,
    /// The match seed. Every random decision the simulation makes
    /// derives from it: the AI's dice, Hex Farm's layout, the weapon
    /// and movement random streams ([`crate::sim::SimRng`]). The host
    /// picks it and every peer (or a replay) plays with the same one;
    /// single player draws a fresh one when the setup is made
    /// ([`random_seed`]). `begin_match` copies it into the match-scoped
    /// [`crate::sim::MatchSeed`] the sim reads.
    pub seed: u64,
    /// What the host allows: pausing, the speed range, cheats (the
    /// lobby's options; the default is single player's).
    pub rules: MatchRules,
}

impl Default for GameSetup {
    fn default() -> Self {
        Self {
            map: random_weighted_map(2),
            players: vec![
                PlayerSpec {
                    faction: Faction::System,
                    ally_team: 0,
                    ai: false,
                },
                PlayerSpec {
                    faction: Faction::Hacker,
                    ally_team: 1,
                    ai: true,
                },
            ],
            difficulty: 2,
            demo: false,
            showcase: None,
            sandbox: false,
            seed: random_seed(),
            rules: MatchRules::default(),
        }
    }
}

/// Local owner when no seat is human (the menu's attract-mode demo):
/// no unit ever carries it and it has no seat, so the player only
/// watches.
pub const SPECTATOR_TEAM: u8 = u8::MAX;

/// Developer hooks from environment variables, read once at startup
/// ([`DevOptions::from_env`]; all off on wasm, which has no env).
#[derive(Resource, Debug, Clone, Default)]
pub struct DevOptions {
    /// `KP_DEMO_FACTIONS=Network,System`: the attract-mode seats'
    /// factions, one seat per name (two or more) — e.g. to watch a
    /// specific unit's weapons in `KP_MENU_SHOTS` visual checks.
    pub demo_factions: Option<Vec<Faction>>,
    /// `KP_DEMO_MAP=<stem>`: the attract-mode map (for `KP_MENU_SHOTS`
    /// visual checks of one map). With commas, a rotation of maps: each
    /// demo restart takes the next one.
    pub demo_map: Option<String>,
    /// `KP_DEMO_CYCLE=<secs>`: restart the attract demo (next map of the
    /// rotation, fresh seats) that often, decided or not — for a
    /// recording that tours several maps.
    pub demo_cycle: Option<f32>,
    /// `KP_MENU_SHOTS=<dir>`: screenshot every launch-menu page there,
    /// then quit (`ui::menu_shots`).
    #[cfg(not(target_arch = "wasm32"))]
    pub menu_shots: Option<std::path::PathBuf>,
    /// `KP_MENU_SHOTS_WARMUP=<frames>` of demo before the first shot.
    #[cfg(not(target_arch = "wasm32"))]
    pub menu_shots_warmup: Option<u32>,
    /// `KP_GAME_SHOTS=<dir>`: screenshot the in-game HUD there, then
    /// quit (`ui::game_shots`).
    #[cfg(not(target_arch = "wasm32"))]
    pub game_shots: Option<std::path::PathBuf>,
    /// `KP_GAME_SHOTS_MAP=<stem>` for the HUD shots' skirmish.
    #[cfg(not(target_arch = "wasm32"))]
    pub game_shots_map: Option<String>,
    /// `KP_EXIT_AFTER=<frames>`: quit after that many rendered frames
    /// (profiling runs: a bounded trace of the attract-mode demo).
    #[cfg(not(target_arch = "wasm32"))]
    pub exit_after: Option<u32>,
    /// `KP_TIME_SCALE=<factor>`: run the game clock that much faster
    /// (profiling runs reach a late-game army count sooner).
    #[cfg(not(target_arch = "wasm32"))]
    pub time_scale: Option<f32>,
    /// `KP_PROFILE=1`: print frame / sim-tick time percentiles at exit
    /// (`profile`).
    #[cfg(not(target_arch = "wasm32"))]
    pub profile: bool,
    /// `KP_MSAA=0|2|4`: override the camera's MSAA sample count (A/B
    /// runs of the GPU cost).
    pub msaa: Option<u8>,
    /// `KP_BLOOM=0`: no bloom pass (A/B runs of the GPU cost).
    pub bloom: Option<bool>,
    /// `KP_WINDOW=<w>x<h>`: start windowed at that size instead of
    /// borderless fullscreen (A/B runs of the fill-rate cost).
    #[cfg(not(target_arch = "wasm32"))]
    pub window: Option<(u32, u32)>,
    /// `KP_WINDOW_POS=<x>,<y>`: put the window's top-left corner there
    /// (`cargo testbox` tiles its games; window managers that place
    /// windows themselves may ignore it).
    #[cfg(not(target_arch = "wasm32"))]
    pub window_pos: Option<(i32, i32)>,
    /// `KP_SHOT_EVERY=<frames>` + `KP_SHOT_DIR=<dir>`: save a screenshot
    /// every N frames while running (watching a run without a screen).
    #[cfg(not(target_arch = "wasm32"))]
    pub shot_every: Option<u32>,
    #[cfg(not(target_arch = "wasm32"))]
    pub shot_dir: Option<std::path::PathBuf>,
    /// `KP_ATTRACT_DISTANCE=<elmos>`: orbit the attract camera this close
    /// (unit close-ups in `KP_SHOT_EVERY` runs).
    pub attract_distance: Option<f32>,
    /// `KP_RECORD=<file.mp4>`: record a camera fly-over of an AI match
    /// on `KP_DEMO_MAP`, then quit (`ui::record`).
    #[cfg(not(target_arch = "wasm32"))]
    pub record: Option<std::path::PathBuf>,
    /// `KP_RECORD_WARMUP=<game seconds>` played before recording.
    #[cfg(not(target_arch = "wasm32"))]
    pub record_warmup: Option<f32>,
    /// `KP_RECORD_SECONDS=<seconds>` of video.
    #[cfg(not(target_arch = "wasm32"))]
    pub record_seconds: Option<f32>,
    /// `KP_RECORD_ATTRACT=1`: the recording's camera is the menu demo's
    /// director (`ui::menu::attract`) instead of the recorder's own path.
    #[cfg(not(target_arch = "wasm32"))]
    pub record_attract: bool,
    /// `KP_BOT=<dir>`: the file-driven bot (`bot`) — a skirmish played
    /// from `<dir>/cmd` without a mouse.
    #[cfg(not(target_arch = "wasm32"))]
    pub bot: Option<std::path::PathBuf>,
    /// `KP_BOT_MAP=<stem>` for the bot's skirmish.
    #[cfg(not(target_arch = "wasm32"))]
    pub bot_map: Option<String>,
    /// `KP_BOT_ENEMY=1`: seat an AI opponent in the bot's skirmish.
    #[cfg(not(target_arch = "wasm32"))]
    pub bot_enemy: bool,
    /// `KP_MAP_EDIT=<stem>`: the datavent editor (`map_editor`) — a
    /// sandbox match on that map where each datavent's factory facing
    /// can be turned and saved back into its `.kpmap`.
    #[cfg(not(target_arch = "wasm32"))]
    pub map_edit: Option<String>,
    /// `KP_NO_BINDLESS=1`: run the renderer without bindless materials
    /// (the wgpu binding-array features are left off), to tell a
    /// material-slot problem from a driver one when pieces flicker.
    /// Native only: web picks its own backend and features.
    #[cfg(not(target_arch = "wasm32"))]
    pub no_bindless: bool,
    /// `KP_NO_INDIRECT=1`: draw every mesh with a direct draw call
    /// (Bevy's `NoIndirectDrawing` on the camera) instead of the
    /// GPU-driven indirect path; the other half of that bisect.
    pub no_indirect: bool,
    /// `KP_NO_FOG=1`: a seated player sees the whole map (HUD
    /// screenshots, profiling runs); see `cloak::sync_fog_enabled`.
    pub no_fog: bool,
}

impl DevOptions {
    #[cfg(target_arch = "wasm32")]
    pub fn from_env() -> Self {
        Self::default()
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok();
        let demo_factions = var("KP_DEMO_FACTIONS").and_then(|list| {
            let pinned: Vec<Faction> = list
                .split(',')
                .filter_map(|name| {
                    Faction::ALL
                        .into_iter()
                        .find(|f| format!("{f:?}").eq_ignore_ascii_case(name.trim()))
                })
                .collect();
            (pinned.len() >= 2).then_some(pinned)
        });
        Self {
            demo_factions,
            demo_map: var("KP_DEMO_MAP"),
            demo_cycle: var("KP_DEMO_CYCLE").and_then(|n| n.parse().ok()),
            menu_shots: var("KP_MENU_SHOTS").map(Into::into),
            menu_shots_warmup: var("KP_MENU_SHOTS_WARMUP").and_then(|w| w.parse().ok()),
            game_shots: var("KP_GAME_SHOTS").map(Into::into),
            game_shots_map: var("KP_GAME_SHOTS_MAP"),
            exit_after: var("KP_EXIT_AFTER").and_then(|n| n.parse().ok()),
            time_scale: var("KP_TIME_SCALE")
                .and_then(|n| n.parse::<f32>().ok())
                // Zero, negative, inf or NaN would trip the clock's asserts.
                .filter(|s| s.is_finite() && *s > 0.0),
            profile: var("KP_PROFILE").is_some_and(|v| v != "0"),
            msaa: var("KP_MSAA")
                .and_then(|n| n.parse().ok())
                .filter(|n| matches!(n, 0 | 1 | 2 | 4)),
            bloom: var("KP_BLOOM").map(|v| v != "0"),
            window: var("KP_WINDOW")
                .and_then(|s| {
                    let (w, h) = s.split_once('x')?;
                    Some((w.parse().ok()?, h.parse().ok()?))
                })
                // The window's resize floor: a 0x0 swapchain panics.
                .filter(|&(w, h)| w >= 320 && h >= 240),
            window_pos: var("KP_WINDOW_POS").and_then(|s| {
                let (x, y) = s.split_once(',')?;
                Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
            }),
            shot_every: var("KP_SHOT_EVERY").and_then(|n| n.parse().ok()),
            shot_dir: var("KP_SHOT_DIR").map(Into::into),
            attract_distance: var("KP_ATTRACT_DISTANCE").and_then(|n| n.parse().ok()),
            record: var("KP_RECORD").map(Into::into),
            record_warmup: var("KP_RECORD_WARMUP").and_then(|n| n.parse().ok()),
            record_seconds: var("KP_RECORD_SECONDS").and_then(|n| n.parse().ok()),
            #[cfg(not(target_arch = "wasm32"))]
            record_attract: var("KP_RECORD_ATTRACT").is_some_and(|v| v == "1"),
            bot: var("KP_BOT").map(Into::into),
            bot_map: var("KP_BOT_MAP"),
            map_edit: var("KP_MAP_EDIT"),
            bot_enemy: var("KP_BOT_ENEMY").is_some_and(|v| v != "0"),
            no_bindless: var("KP_NO_BINDLESS").is_some_and(|v| v != "0"),
            no_indirect: var("KP_NO_INDIRECT").is_some_and(|v| v != "0"),
            no_fog: var("KP_NO_FOG").is_some_and(|v| v != "0"),
        }
    }
}

/// `KP_EXIT_AFTER` / `KP_TIME_SCALE`: quit once that many frames have
/// rendered; run the clock scaled.
#[cfg(not(target_arch = "wasm32"))]
pub fn dev_run_control(
    dev: Res<DevOptions>,
    mut frames: Local<u32>,
    mut time: ResMut<Time<Virtual>>,
    mut exit: MessageWriter<AppExit>,
) {
    *frames += 1;
    if let Some(scale) = dev.time_scale
        && time.relative_speed() != scale
    {
        time.set_relative_speed(scale);
        // Keep the catch-up cap at three sim ticks per frame in game
        // time, so a scaled run profiles frames like real ones. The
        // cap is derived from the unscaled default once, not from the
        // live value: another clock writer (the shot / record tools)
        // would otherwise have it divided again on every re-apply
        // until `set_max_delta(ZERO)` asserts.
        let cap = Time::<Virtual>::default().max_delta().div_f32(scale);
        time.set_max_delta(cap);
    }
    if dev.exit_after.is_some_and(|n| *frames >= n) {
        exit.write(AppExit::Success);
    }
}

/// The main-menu attract-mode setup: a random all-AI skirmish on a
/// weighted-random map, seated as that map is meant to be played
/// ([`demo_seats`]) with random factions, each its own ally team
/// (free-for-all), played by the AI while the player spectates. The menu's
/// demo director restarts it with a fresh roll once it's decided.
/// [`DevOptions`] can pin the factions and the map.
pub fn demo_setup(dev: &DevOptions) -> GameSetup {
    let map = dev.demo_map.as_deref().map_or_else(
        || random_weighted_map(2),
        |list| {
            // A comma list rotates: one map per demo restart.
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let maps: Vec<&str> = list
                .split(',')
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .collect();
            let i = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            maps.get(i % maps.len().max(1))
                .map_or_else(|| random_weighted_map(2), |m| m.to_string())
        },
    );
    let seats = demo_seats(&map, clock_f64());
    let mut players: Vec<PlayerSpec> = (0..seats)
        .map(|i| PlayerSpec {
            faction: Faction::ALL[(clock_f64() * 3.0) as usize % 3],
            ally_team: i,
            ai: true,
        })
        .collect();
    if let Some(pinned) = &dev.demo_factions {
        players = pinned
            .iter()
            .enumerate()
            .map(|(i, &faction)| PlayerSpec {
                faction,
                ally_team: i as u8,
                ai: true,
            })
            .collect();
    }
    GameSetup {
        map,
        players,
        difficulty: 2,
        demo: true,
        showcase: None,
        sandbox: false,
        seed: random_seed(),
        rules: MatchRules::default(),
    }
}

/// Showcase setup: one faction, Data_Cache_L1, no AI enemies.
/// The [`ShowcaseDirector`](crate::showcase::ShowcaseDirector) queues
/// factory production and builder construction after the world spawns.
pub fn showcase_setup(faction: Faction) -> GameSetup {
    GameSetup {
        map: "Data_Cache_L1".to_string(),
        players: vec![PlayerSpec {
            faction,
            ally_team: 0,
            ai: false,
        }],
        difficulty: 1,
        demo: false,
        showcase: Some(faction),
        sandbox: false,
        seed: random_seed(),
        rules: MatchRules::default(),
    }
}

/// Skirmish settings being edited on the advanced-skirmish menu page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Resource)]
pub struct SkirmishConfig {
    pub map: Option<usize>, // index into the map catalog; None = weighted random
    pub your_faction: Faction,
    pub enemy_faction: Faction,
    pub grouping: Grouping,
    pub difficulty: u8,
    /// The Team Game roster, rolled when the grouping or difficulty
    /// changes ([`SkirmishConfig::set_grouping`]), so the description
    /// line names the match Run! starts, as the original's page showed
    /// the skirmish it had already generated.
    pub team_roll: TeamRoll,
}

impl Default for SkirmishConfig {
    fn default() -> Self {
        Self {
            map: None,
            your_faction: Faction::System,
            enemy_faction: Faction::Hacker,
            grouping: Grouping::Duel,
            difficulty: 2,
            team_roll: TeamRoll::roll(2, &mut clock_f64),
        }
    }
}

impl SkirmishConfig {
    pub fn set_grouping(&mut self, grouping: Grouping) {
        self.grouping = grouping;
        self.team_roll = TeamRoll::roll(self.difficulty, &mut clock_f64);
    }

    pub fn set_difficulty(&mut self, difficulty: u8) {
        self.difficulty = difficulty;
        self.team_roll = TeamRoll::roll(difficulty, &mut clock_f64);
    }
}

/// Battle shape presets from the original's advanced page
/// (`GenerateGame.lua`'s groupings 1–3; Spectate and Heroic are not
/// implemented in the remake yet — see the menu module).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grouping {
    /// 1 human vs 1 AI.
    Duel,
    /// The human and AI allies against AI enemies, sides of a size
    /// rolled per difficulty ([`TeamRoll`]).
    Team,
    /// 1 human vs N AI, N growing with difficulty (upstream's
    /// "Challenge").
    Outgunned,
}

/// Half the seats a map offers, upstream's `HalfMaxMapTeam`
/// (`floor(MaxPlayers / 2)`). The remake rolls the roster before the
/// map is read and seats beyond a map's start positions on a ring
/// (`spawning::spawn_homebases`), so it takes the 4v4 the multiplayer
/// plan supports: at most 8 seats.
const HALF_MAX_TEAM: u8 = 4;

/// A Team Game roster: `GenerateSkirmish`, grouping 2
/// (`GenerateGame.lua:123-143` sizes, `:229-237` the player's seat,
/// `:270-290` the sides).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeamRoll {
    /// Seats on the player's side, the player included.
    pub allies: u8,
    pub enemies: u8,
    /// The player's seat (`PlayerTeamNum`).
    pub player: u8,
}

impl TeamRoll {
    /// Upstream's sizes per difficulty (`GenerateRandInt(a, b)` is
    /// inclusive) and the player's seat: an even seat when the player's
    /// side outnumbers the other, an odd one when outnumbered, so the
    /// interleaved start positions of [`TeamRoll::enemy`] work out.
    /// `rand` yields uniform [0, 1).
    pub fn roll(difficulty: u8, rand: &mut impl FnMut() -> f64) -> Self {
        let mut int = |lo: u8, hi: u8| {
            let (lo, hi) = (lo.min(hi), lo.max(hi));
            lo + ((rand() * f64::from(hi - lo + 1)) as u8).min(hi - lo)
        };
        let half = HALF_MAX_TEAM;
        let (allies, enemies) = match difficulty {
            1 => {
                let a = int(2, half);
                (a, a - 1)
            }
            2 => {
                let a = int(2, half);
                (a, a)
            }
            3 => {
                let a = int(2, half - 1);
                (a, int(a + 1, half + 1))
            }
            _ => {
                let e = int(3, half + 1);
                (int(1, e - 2), e)
            }
        };
        let mut player = int(0, allies + enemies - 1);
        if allies > 1 {
            if enemies < allies {
                player = 2 * int(0, allies - 1);
            }
            if enemies > allies {
                player = 1 + 2 * int(0, allies - 1);
            }
        }
        Self {
            allies,
            enemies,
            player,
        }
    }

    pub fn seats(self) -> u8 {
        self.allies + self.enemies
    }

    /// Is seat `p` on the other side? Upstream alternates the sides
    /// over the seats (so over the start positions) and gives the
    /// bigger side the trailing seats.
    pub fn enemy(self, p: u8) -> bool {
        let (allies, enemies) = (self.allies, self.enemies);
        if allies <= 1 {
            return p != self.player;
        }
        let mut enemy = !(p + self.player).is_multiple_of(2);
        if enemies < allies && p > 2 * enemies {
            enemy = false;
        }
        if allies < enemies && p > 2 * allies {
            enemy = true;
        }
        enemy
    }
}

impl Grouping {
    /// Label shown in the original menu.
    pub fn label(self) -> &'static str {
        match self {
            Grouping::Duel => "Duel",
            Grouping::Team => "Team Game",
            Grouping::Outgunned => "Outgunned",
        }
    }

    /// The setup-name the original's `GenerateSkirmish` assigned, reused
    /// for the live description line.
    pub fn setup_name(self, difficulty: u8) -> &'static str {
        match (self, difficulty) {
            (Grouping::Duel, 1) => "Beginner's Duel",
            (Grouping::Duel, 2) => "Standard Duel",
            (Grouping::Duel, 3) => "Experienced Duel",
            (Grouping::Duel, _) => "Veteran's Duel",
            (Grouping::Team, 1) => "Easy Team Game",
            (Grouping::Team, 2) => "Medium Team Game",
            (Grouping::Team, 3) => "Hard Team Game",
            (Grouping::Team, _) => "Very Hard Team Game",
            (Grouping::Outgunned, 1) => "Tough Challenge",
            (Grouping::Outgunned, 2) => "Difficult Challenge",
            (Grouping::Outgunned, 3) => "Insane Challenge",
            (Grouping::Outgunned, _) => "Impossible Challenge",
        }
    }

    /// `(allies, enemies)`: seats on the player's side (the player
    /// included) and on the other. Team Game's come from the `roll`.
    pub fn sides(self, difficulty: u8, roll: TeamRoll) -> (usize, usize) {
        match self {
            Grouping::Duel => (1, 1),
            Grouping::Team => (roll.allies as usize, roll.enemies as usize),
            Grouping::Outgunned => (1, 1 + (difficulty as usize).min(4)),
        }
    }
}

/// How far the AI may out-produce its enemies before its fairness cap
/// kicks in: the slack widens Fair KPAI's `Lack` head start
/// (`units::ai::build_orders::Lack::compute`). Difficulty 1 is exactly
/// upstream "Fair KPAI"; higher settings loosen the cap.
#[derive(Debug, Clone, Copy, Resource)]
pub struct AiDifficulty(pub u8);

impl AiDifficulty {
    pub fn fairness_slack(self) -> usize {
        match self.0.min(4) {
            1 => 0,
            2 => 2,
            3 => 4,
            _ => 8,
        }
    }
}

/// Set once the player dismisses the game-over panel ("Keep on
/// playing / watching"); cleared when a new outcome is announced
/// (`game_over::announce_outcome`). UI only. Match-scoped: re-armed at
/// every match swap.
#[derive(Debug, Clone, Copy, Resource, Default)]
pub struct GameOverDismissed(pub bool);

/// The setup of the match currently in the world. [`GameSetup`] is the
/// *next* match the menu asked for — it changes the moment Restart or a
/// demo reroll is requested, while the old world keeps running until
/// the new map is prepared. Whatever judges the running match (the
/// game-over census: seats, demo / showcase / sandbox rules) reads this
/// instead; `map_loading` sets it at the swap. Absent before the first
/// match.
#[derive(Debug, Clone, Resource)]
pub struct ActiveMatch(pub GameSetup);

/// Message written by menu buttons: start/restart the configured match.
#[derive(Message)]
pub struct RunGame;

/// Turn the menu's skirmish config into a concrete [`GameSetup`].
///
/// `map_names` is the map catalog, so `None` (random) can pick a weighted
/// map without the menu knowing the list.
pub fn build_setup(config: &SkirmishConfig, map_names: &[String]) -> GameSetup {
    let seat = |enemy: bool, ai: bool| PlayerSpec {
        faction: if enemy {
            config.enemy_faction
        } else {
            config.your_faction
        },
        ally_team: u8::from(enemy),
        ai,
    };
    let players: Vec<PlayerSpec> = match config.grouping {
        // Upstream seats the sides interleaved, the player at its
        // rolled seat; allies take the player's faction.
        Grouping::Team => {
            let roll = config.team_roll;
            (0..roll.seats())
                .map(|p| seat(roll.enemy(p), p != roll.player))
                .collect()
        }
        // The player first, then one seat (one owner, one AI) per enemy.
        Grouping::Duel | Grouping::Outgunned => {
            let (_, enemies) = config.grouping.sides(config.difficulty, config.team_roll);
            std::iter::once(seat(false, false))
                .chain((0..enemies).map(|_| seat(true, true)))
                .collect()
        }
    };
    let random = || random_weighted_map(players.len());
    let map = match config.map {
        Some(i) => map_names.get(i).cloned().unwrap_or_else(random),
        None => random(),
    };
    GameSetup {
        map,
        players,
        difficulty: config.difficulty,
        demo: false,
        showcase: None,
        sandbox: false,
        seed: random_seed(),
        rules: MatchRules::default(),
    }
}

/// The live description line shown under the advanced page, in the
/// original's `"<setup>: <allies>v<enemies> - <faction>"` format.
pub fn describe_setup(config: &SkirmishConfig) -> String {
    let (allies, enemies) = config.grouping.sides(config.difficulty, config.team_roll);
    let player = match config.grouping {
        Grouping::Team => config.team_roll.player,
        Grouping::Duel | Grouping::Outgunned => 0,
    };
    format!(
        "{}: {}v{} - Player is #{} in [0..{}] and {:?}",
        config.grouping.setup_name(config.difficulty),
        allies,
        enemies,
        player,
        allies + enemies - 1,
        config.your_faction,
    )
}

/// Most seats an attract demo fills (the menu runs it behind the UI).
const DEMO_MAX_SEATS: u32 = 6;

/// How many seats an attract demo on `map` fills, from the players the
/// map is for (`map_meta`) — a Circular Buffer for n gets n, Three-Way
/// Handshake three — at most [`DEMO_MAX_SEATS`]. `roll` in `0..1` picks
/// within the range. A map the build doesn't know (a dev's own file)
/// gets two to four.
pub fn demo_seats(map: &str, roll: f64) -> u8 {
    let (low, high) = crate::map_loading::map_meta(map).map_or((2, 4), |m| m.players);
    let high = high.clamp(2, DEMO_MAX_SEATS);
    let low = low.clamp(2, high);
    let span = high - low + 1;
    (low + ((roll * span as f64) as u32).min(span - 1)) as u8
}

/// Weighted-random map choice, weights lifted from the original
/// launcher's `AddMap(Weight, …)` table (73 total), plus the port's own
/// maps. Only maps with a start position for each of `seats` are drawn.
pub fn random_weighted_map(seats: usize) -> String {
    const WEIGHTS: &[(&str, u32)] = &[
        ("Marble_Madness_Map", 7),
        ("Major_Madness3.0", 5),
        ("Data_Cache_L1", 6),
        ("Spooler_Buffer_0.5_beta", 4),
        ("DigitalDivide_PT2", 4),
        ("Speed_Balls_16_Way", 3),
        ("Direct_Memory_Access_0.5c_beta", 3),
        ("Direct_Memory_Access_0.5e_beta", 3),
        ("Hex_Farm_8", 11),
        ("Central_Hub", 7),
        ("Corrupted_Core", 5),
        ("Dual_Core", 3),
        ("Quad_Core", 2),
        ("Memory_Bank_v3", 4),
        ("pacman", 3),
        ("Palladium_0.5_(beta)", 3),
        ("deadlock_v1", 3),
        ("token_ring_v1", 2),
        ("heap_fragmentation_v1", 2),
        ("three_way_handshake_v1", 2),
        ("pipeline_v1", 2),
        ("circular_buffer_3_v1", 1),
        ("circular_buffer_4_v1", 1),
        ("circular_buffer_5_v1", 1),
        ("circular_buffer_6_v1", 1),
    ];
    let fits =
        |name: &str| crate::map_loading::map_meta(name).is_none_or(|m| m.starts as usize >= seats);
    let weights: Vec<(&str, u32)> = WEIGHTS.iter().copied().filter(|(n, _)| fits(n)).collect();
    let total: u32 = weights.iter().map(|(_, w)| w).sum();
    let mut d = (total as f64 * clock_f64()) as u32;
    for (name, w) in weights {
        if d < w {
            return name.to_string();
        }
        d -= w;
    }
    // Nothing seats that many: the map with the most starts.
    "Quad_Core".to_string()
}

/// A fresh match seed from launch entropy, for a [`GameSetup`] made on
/// this machine (single player, the attract demo). Called when the
/// setup is created, never by the sim: a networked match takes the
/// host's seed, a replay the recorded one.
pub fn random_seed() -> u64 {
    let hi = (clock_f64() * (1u64 << 32) as f64) as u64;
    let lo = (clock_f64() * (1u64 << 32) as f64) as u64;
    (hi << 32) | lo
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duel_always_one_enemy_outgunned_scales() {
        let mut config = SkirmishConfig {
            grouping: Grouping::Duel,
            ..Default::default()
        };
        assert_eq!(build_setup(&config, &[]).players.len(), 2);
        config.grouping = Grouping::Outgunned;
        config.difficulty = 1;
        assert_eq!(build_setup(&config, &[]).players.len(), 3);
        config.difficulty = 4;
        assert_eq!(build_setup(&config, &[]).players.len(), 6);
    }

    #[test]
    fn local_player_is_team_zero_seat_zero() {
        let config = SkirmishConfig {
            your_faction: Faction::Network,
            enemy_faction: Faction::System,
            ..default()
        };
        let setup = build_setup(&config, &[]);
        assert!(!setup.players[0].ai);
        assert_eq!(setup.players[0].ally_team, 0);
        assert!(setup.players[1..].iter().all(|p| p.ai && p.ally_team == 1));
    }

    /// Upstream's Team Game sizes per difficulty, with `HalfMaxMapTeam`
    /// = 4, over the whole range of the dice.
    #[test]
    fn team_roll_sizes_follow_upstream() {
        for difficulty in 1..=4 {
            for step in 0..64 {
                let mut k = 0u32;
                let mut rand = || {
                    k += 1;
                    f64::from((step * 7 + k * 13) % 64) / 64.0
                };
                let r = TeamRoll::roll(difficulty, &mut rand);
                let (a, e) = (r.allies, r.enemies);
                match difficulty {
                    1 => assert!((2..=4).contains(&a) && e == a - 1, "{r:?}"),
                    2 => assert!((2..=4).contains(&a) && e == a, "{r:?}"),
                    3 => assert!((2..=3).contains(&a) && (a + 1..=5).contains(&e), "{r:?}"),
                    _ => assert!((3..=5).contains(&e) && (1..=e - 2).contains(&a), "{r:?}"),
                }
                assert!(r.player < r.seats());
                assert!(r.seats() <= 8);
                let enemies = (0..r.seats()).filter(|p| r.enemy(*p)).count() as u8;
                assert_eq!(enemies, e, "{r:?}");
                assert!(!r.enemy(r.player), "the player is on its own side: {r:?}");
            }
        }
    }

    /// A Team Game seats every side's owners in their own ally team:
    /// the human plus AI allies on team 0, AI enemies on team 1.
    #[test]
    fn team_game_has_ai_allies() {
        let config = SkirmishConfig {
            grouping: Grouping::Team,
            difficulty: 2,
            team_roll: TeamRoll {
                allies: 3,
                enemies: 3,
                player: 1,
            },
            ..default()
        };
        let setup = build_setup(&config, &[]);
        let humans: Vec<usize> = (0..setup.players.len())
            .filter(|i| !setup.players[*i].ai)
            .collect();
        assert_eq!(humans, vec![1]);
        let sides: Vec<u8> = setup.players.iter().map(|p| p.ally_team).collect();
        assert_eq!(sides, vec![1, 0, 1, 0, 1, 0]);
        assert!(
            setup
                .players
                .iter()
                .all(|p| (p.ally_team == 0) == (p.faction == config.your_faction))
        );
        assert_eq!(
            describe_setup(&config),
            "Medium Team Game: 3v3 - Player is #1 in [0..5] and System"
        );
    }

    #[test]
    fn fairness_slack_ladder() {
        assert_eq!(AiDifficulty(1).fairness_slack(), 0);
        assert_eq!(AiDifficulty(2).fairness_slack(), 2);
        assert_eq!(AiDifficulty(3).fairness_slack(), 4);
        assert_eq!(AiDifficulty(4).fairness_slack(), 8);
    }

    #[test]
    fn weighted_map_is_from_table() {
        let names: &[(&str, u32)] = &[
            ("Marble_Madness_Map", 7),
            ("Major_Madness3.0", 5),
            ("Data_Cache_L1", 6),
            ("Spooler_Buffer_0.5_beta", 4),
            ("DigitalDivide_PT2", 4),
            ("Speed_Balls_16_Way", 3),
            ("Direct_Memory_Access_0.5c_beta", 3),
            ("Direct_Memory_Access_0.5e_beta", 3),
            ("Hex_Farm_8", 11),
            ("Central_Hub", 7),
            ("Corrupted_Core", 5),
            ("Dual_Core", 3),
            ("Quad_Core", 2),
            ("Memory_Bank_v3", 4),
            ("pacman", 3),
            ("Palladium_0.5_(beta)", 3),
            ("deadlock_v1", 3),
            ("token_ring_v1", 2),
            ("heap_fragmentation_v1", 2),
            ("three_way_handshake_v1", 2),
            ("pipeline_v1", 2),
            ("circular_buffer_3_v1", 1),
            ("circular_buffer_4_v1", 1),
            ("circular_buffer_5_v1", 1),
            ("circular_buffer_6_v1", 1),
        ];
        let valid: Vec<String> = names.iter().map(|(n, _)| n.to_string()).collect();
        for _ in 0..40 {
            assert!(valid.contains(&random_weighted_map(2)));
        }
    }

    /// A random map always has a start for every seat asked for.
    #[test]
    fn weighted_map_seats_everyone() {
        for seats in 2..=8 {
            for _ in 0..40 {
                let map = random_weighted_map(seats);
                let meta = crate::map_loading::map_meta(&map).expect("a shipped map");
                assert!(meta.starts as usize >= seats, "{map} for {seats} seats");
            }
        }
    }

    /// The demo seats a map the way it is meant to be played, within its
    /// starts and the demo's cap.
    #[test]
    fn demo_seats_follow_the_map() {
        let all =
            |map: &str| -> Vec<u8> { (0..20).map(|i| demo_seats(map, i as f64 / 20.0)).collect() };
        assert!(all("circular_buffer_3_v1").iter().all(|&n| n == 3));
        assert!(all("circular_buffer_6_v1").iter().all(|&n| n == 6));
        assert!(all("three_way_handshake_v1").iter().all(|&n| n == 3));
        let quad = all("Quad_Core");
        assert!(quad.iter().all(|&n| (4..=6).contains(&n)), "{quad:?}");
        assert!(quad.contains(&4) && quad.contains(&6));
        let token = all("token_ring_v1");
        assert!(
            token.iter().all(|&n| (2..=4).contains(&n)) && token.contains(&2) && token.contains(&4)
        );
        assert!(all("no_such_map").iter().all(|&n| (2..=4).contains(&n)));
        assert_eq!(demo_seats("Dual_Core", 0.9999), 6);
    }
}
