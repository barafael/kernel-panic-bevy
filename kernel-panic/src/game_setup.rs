//! Game setup: the bridge between the menus and the simulation.
//!
//! The menu writes a [`GameSetup`] (map + players + difficulty) and flips
//! [`AppState`] to `InGame`; `map_loading` consumes it on entry. This
//! mirrors the original Kernel Panic flow, where the launch menu
//! generated a start script (`GenerateSkirmish` → `RunGame` → engine
//! restart) — our "restart" is despawning the game world and re-entering
//! the `InGame` state.

use bevy::prelude::*;

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
    #[allow(dead_code)]
    pub faction: Faction,
    /// Ally team. Players sharing a team are friendly (the local player
    /// is always team 0).
    #[allow(dead_code)]
    pub team: u8,
    /// AI seats never run the local input path; reserved for future
    /// ally support (an AI seat on the local team).
    #[allow(dead_code)]
    pub ai: bool,
}

/// Everything needed to start a match.
#[derive(Debug, Clone, Resource)]
pub struct GameSetup {
    /// Map file stem, resolved against the map catalog (`assets/maps/`).
    #[allow(dead_code)]
    pub map: String,
    /// Player 0 is the local player (team 0); the rest are AI seats.
    #[allow(dead_code)]
    pub players: Vec<PlayerSpec>,
    /// 1 Easy … 4 Extreme. Drives the AI fairness slack (`AiDifficulty`
    /// mirrors it at match start); enemy count comes from the grouping.
    /// (Not yet consumed at runtime — kept as part of the match spec.)
    #[allow(dead_code)]
    pub difficulty: u8,
    /// Menu attract-mode demo: no homebases, no win/lose — the menu's
    /// demo director spawns the cast instead (`ui::menu::demo_director`).
    #[allow(dead_code)]
    pub demo: bool,
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
        }
    }
}

/// The main-menu attract-mode setup: a live skirmish map with no
/// homebases. The demo director keeps Flows spawning into Pointer fire;
/// anything that dies is replaced, so the battle never ends.
pub fn demo_setup() -> GameSetup {
    GameSetup {
        map: random_weighted_map(),
        players: vec![
            PlayerSpec {
                faction: Faction::System,
                team: 0,
                ai: false,
            },
            PlayerSpec {
                faction: Faction::Network,
                team: 1,
                ai: true,
            },
        ],
        difficulty: 2,
        demo: true,
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

/// How much the AI may out-produce the strongest enemy army before its
/// fairness cap kicks in. Difficulty 1 mirrors upstream "Fair KPAI"
/// (never outnumbers you); higher settings loosen the cap.
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
/// `map_count` is the map-catalog length so `None` (random) can pick a
/// weighted map without the menu knowing the list.
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
    }
}

/// The live description line shown under the advanced page, in the
/// original's `"<setup>: <allies>v<enemies> - <faction>"` format.
pub fn describe_setup(config: &SkirmishConfig) -> String {
    let enemies = config.grouping.enemies(config.difficulty);
    format!(
        "{}: 1v{} - Player is nº0 in [0..{}] and {:?}",
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
    let mut d = (total as f64 * rand_f64()) as u32;
    for (name, w) in WEIGHTS {
        if d < *w {
            return (*name).to_string();
        }
        d -= *w;
    }
    "Marble_Madness_Map".to_string()
}

/// Tiny XOR-shift PRNG so we don't need a rand dependency. Seeded from
/// the clock once per call site chain.
fn rand_f64() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    thread_local! {
        static STATE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    STATE.with(|s| {
        let mut x = s.get();
        if x == 0 {
            x = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64 ^ (d.as_secs() << 20))
                .unwrap_or(0x9E3779B97F4A7C15)
                | 1;
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        (x >> 11) as f64 / (1u64 << 53) as f64
    })
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
