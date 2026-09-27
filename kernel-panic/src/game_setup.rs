//! Game setup: the bridge between the menus and the simulation.
//!
//! The menu writes a [`GameSetup`] (map + players + difficulty) and flips
//! [`AppState`] to `InGame`; `map_loading` consumes it on entry. This
//! mirrors the original Kernel Panic flow, where the launch menu
//! generated a start script (`GenerateSkirmish` → `RunGame` → engine
//! restart) — our "restart" is despawning the game world and re-entering
//! the `InGame` state.

use bevy::prelude::*;

use crate::rng::clock_f64;
use crate::units::components::Faction;

/// Top-level app state: the menu system owns `Menu`; the simulation runs
/// in `InGame` (where `GameState` Playing/Victory/Defeat applies).
#[derive(States, Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppState {
    #[default]
    Menu,
    InGame,
}

/// One seat in the match.
#[derive(Debug, Clone)]
pub struct PlayerSpec {
    pub faction: Faction,
    /// Ally team. Players sharing a team are friendly (the local player
    /// is always team 0).
    pub team: u8,
    /// AI seats never run the local input path. A setup without any
    /// human seat is spectated (see [`SPECTATOR_TEAM`]).
    pub ai: bool,
}

/// Everything needed to start a match.
#[derive(Debug, Clone, Resource)]
pub struct GameSetup {
    /// Map file stem, resolved against the map catalog (`assets/maps/`).
    pub map: String,
    /// Player 0 is the local player (team 0); the rest are AI seats.
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
}

impl Default for GameSetup {
    fn default() -> Self {
        Self {
            map: random_weighted_map(),
            players: vec![
                PlayerSpec {
                    faction: Faction::System,
                    team: 0,
                    ai: false,
                },
                PlayerSpec {
                    faction: Faction::Hacker,
                    team: 1,
                    ai: true,
                },
            ],
            difficulty: 2,
            demo: false,
            showcase: None,
        }
    }
}

/// Local "team" when no seat is human (the menu's attract-mode demo):
/// no unit ever carries it, so every seat is an opponent the AI drives
/// and the player only watches.
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
    /// visual checks of one map).
    pub demo_map: Option<String>,
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
            menu_shots: var("KP_MENU_SHOTS").map(Into::into),
            menu_shots_warmup: var("KP_MENU_SHOTS_WARMUP").and_then(|w| w.parse().ok()),
            game_shots: var("KP_GAME_SHOTS").map(Into::into),
            game_shots_map: var("KP_GAME_SHOTS_MAP"),
            exit_after: var("KP_EXIT_AFTER").and_then(|n| n.parse().ok()),
            time_scale: var("KP_TIME_SCALE").and_then(|n| n.parse().ok()),
            profile: var("KP_PROFILE").is_some_and(|v| v != "0"),
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
    }
    if dev.exit_after.is_some_and(|n| *frames >= n) {
        exit.write(AppExit::Success);
    }
}

/// The main-menu attract-mode setup: a random all-AI skirmish on a
/// weighted-random map — 2 to 4 seats of random factions, each on its
/// own team, played by the AI while the player spectates. The menu's
/// demo director restarts it with a fresh roll once it's decided.
/// [`DevOptions`] can pin the factions and the map.
pub fn demo_setup(dev: &DevOptions) -> GameSetup {
    let seats = 2 + (clock_f64() * 3.0) as u8;
    let mut players: Vec<PlayerSpec> = (0..seats)
        .map(|i| PlayerSpec {
            faction: Faction::ALL[(clock_f64() * 3.0) as usize % 3],
            team: 1 + i,
            ai: true,
        })
        .collect();
    if let Some(pinned) = &dev.demo_factions {
        players = pinned
            .iter()
            .enumerate()
            .map(|(i, &faction)| PlayerSpec {
                faction,
                team: 1 + i as u8,
                ai: true,
            })
            .collect();
    }
    let map = dev.demo_map.clone().unwrap_or_else(random_weighted_map);
    GameSetup {
        map,
        players,
        difficulty: 2,
        demo: true,
        showcase: None,
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
            team: 0,
            ai: false,
        }],
        difficulty: 1,
        demo: false,
        showcase: Some(faction),
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
}

impl Default for SkirmishConfig {
    fn default() -> Self {
        Self {
            map: None,
            your_faction: Faction::System,
            enemy_faction: Faction::Hacker,
            grouping: Grouping::Duel,
            difficulty: 2,
        }
    }
}

/// Battle shape presets from the original's advanced page. (Spectate and
/// Heroic are not implemented in the remake yet — see the menu module.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grouping {
    /// 1 human vs 1 AI.
    Duel,
    /// 1 human vs N AI, N growing with difficulty.
    Outgunned,
}

impl Grouping {
    /// Label shown in the original menu.
    pub fn label(self) -> &'static str {
        match self {
            Grouping::Duel => "Duel",
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
            (Grouping::Outgunned, 1) => "Tough Challenge",
            (Grouping::Outgunned, 2) => "Difficult Challenge",
            (Grouping::Outgunned, 3) => "Insane Challenge",
            (Grouping::Outgunned, _) => "Impossible Challenge",
        }
    }

    /// AI enemy count for a given difficulty.
    pub fn enemies(self, difficulty: u8) -> usize {
        match self {
            Grouping::Duel => 1,
            Grouping::Outgunned => 1 + (difficulty as usize).min(4),
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
/// playing"), so `check_game_over` doesn't immediately re-trigger while
/// the victory condition still holds.
#[derive(Debug, Clone, Copy, Resource, Default)]
pub struct GameOverDismissed(pub bool);

/// Message written by menu buttons: start/restart the configured match.
#[derive(Message)]
pub struct RunGame;

/// Turn the menu's skirmish config into a concrete [`GameSetup`].
///
/// `map_names` is the map catalog, so `None` (random) can pick a weighted
/// map without the menu knowing the list.
pub fn build_setup(config: &SkirmishConfig, map_names: &[String]) -> GameSetup {
    let map = match config.map {
        Some(i) => map_names
            .get(i)
            .cloned()
            .unwrap_or_else(random_weighted_map),
        None => random_weighted_map(),
    };
    let enemies = config.grouping.enemies(config.difficulty);
    let mut players = vec![PlayerSpec {
        faction: config.your_faction,
        team: 0,
        ai: false,
    }];
    for _ in 0..enemies {
        players.push(PlayerSpec {
            faction: config.enemy_faction,
            team: 1,
            ai: true,
        });
    }
    GameSetup {
        map,
        players,
        difficulty: config.difficulty,
        demo: false,
        showcase: None,
    }
}

/// The live description line shown under the advanced page, in the
/// original's `"<setup>: <allies>v<enemies> - <faction>"` format.
pub fn describe_setup(config: &SkirmishConfig) -> String {
    let enemies = config.grouping.enemies(config.difficulty);
    format!(
        "{}: 1v{} - Player is #0 in [0..{}] and {:?}",
        config.grouping.setup_name(config.difficulty),
        enemies,
        enemies,
        config.your_faction,
    )
}

/// Weighted-random map choice, weights lifted from the original
/// launcher's `AddMap(Weight, …)` table (73 total).
pub fn random_weighted_map() -> String {
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
    ];
    let total: u32 = WEIGHTS.iter().map(|(_, w)| w).sum();
    let mut d = (total as f64 * clock_f64()) as u32;
    for (name, w) in WEIGHTS {
        if d < *w {
            return (*name).to_string();
        }
        d -= *w;
    }
    "Marble_Madness_Map".to_string()
}

/// A fresh seed for per-match procedural content (Hex Farm's layout,
/// which the original gadget re-rolls every game).
pub fn match_seed() -> u64 {
    let hi = (clock_f64() * (1u64 << 32) as f64) as u64;
    let lo = (clock_f64() * (1u64 << 32) as f64) as u64;
    (hi << 32) | lo
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duel_always_one_enemy_outgunned_scales() {
        let mut config = SkirmishConfig::default();
        config.grouping = Grouping::Duel;
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
        assert_eq!(setup.players[0].team, 0);
        assert!(setup.players[1..].iter().all(|p| p.ai && p.team == 1));
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
        ];
        let valid: Vec<String> = names.iter().map(|(n, _)| n.to_string()).collect();
        for _ in 0..40 {
            assert!(valid.contains(&random_weighted_map()));
        }
    }
}
