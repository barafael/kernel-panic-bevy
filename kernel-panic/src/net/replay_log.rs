//! Replay files (`.kpreplay`): what a lockstep match is made of — the
//! match setup (map, seats, difficulty, seed, rules), a compatibility
//! stamp (sim version, unit bundle and map content hashes) and the
//! command log, every human seat's commands by tick, as the turns
//! carried them. The in-sim AI is part of the simulation, so its
//! commands are not in the log: a replay re-derives them, as every
//! lockstep peer does.
//!
//! The golden replays of the determinism matrix (M0.4,
//! `headless::replay`) are such files; since M3 every match records one
//! (`net-experiments/m3-turn-log.md`):
//!
//! - **Recording**: every peer keeps the commands of the turns it applied
//!   ([`Session::log`](super::session::Session::log)), single player
//!   (a solo host) included. When the match is replaced
//!   (`match_scope::begin_match`) or the game quits, [`finish_match`]
//!   makes a [`Replay`] of it: natively saved to `replays/`
//!   ([`replay_dir`]), on the web kept for the session's Replays page,
//!   which offers it as a download.
//! - **Playing**: the Replays page opens a file ([`open`]) and starts
//!   the match it describes with [`NextReplay`]; its session plays the
//!   log back one turn per tick ([`Session::replay`](super::session::Session::replay)),
//!   the viewer watching as a spectator with pause and speed controls
//!   (`ui::hud::replay_bar`). Spring records its demos the same way:
//!   the setup script and every packet the server broadcast
//!   (`CDemoRecorder`, `Net/GameServer.cpp:250-253`, `:559-560`), and
//!   plays them back through the normal client.
//!
//! The file: [`MAGIC`], the format version as u16 LE, then the postcard
//! encoding of [`Replay`].

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::compat::{Build, Compat};
use super::driver::{NetSession, PeerNames};
use super::{CommandFrame, MatchRules};
use crate::game_setup::{ActiveMatch, AppState, GameSetup, PlayerSpec};
use crate::units::components::Faction;

/// File magic of a `.kpreplay`.
pub const MAGIC: &[u8; 8] = b"kpreplay";
/// Layout of the file itself; bumped whenever [`Replay`] changes shape
/// or meaning. 2 (M1): seat `i` is owner `i` and [`Seat::ally_team`] its
/// ally team, where 1 gave every seat of an ally team one shared owner.
/// 3 (M2): the setup carries the host's [`MatchRules`]. 4 (M3):
/// [`ReplayMeta`] (names, networked, when).
pub const FORMAT_VERSION: u16 = 4;

/// One seat of the match: seat `i` is owner `i` (`TeamId(i)`,
/// `units::teams`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seat {
    /// Index into [`Faction::ALL`].
    pub faction: u8,
    /// Ally team (Spring's allyteam).
    pub ally_team: u8,
    pub ai: bool,
}

impl From<&PlayerSpec> for Seat {
    fn from(p: &PlayerSpec) -> Self {
        Self {
            faction: p.faction.index(),
            ally_team: p.ally_team,
            ai: p.ai,
        }
    }
}

impl From<&Seat> for PlayerSpec {
    fn from(s: &Seat) -> Self {
        Self {
            faction: Faction::from_team_id(s.faction),
            ally_team: s.ally_team,
            ai: s.ai,
        }
    }
}

/// Everything that starts a match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchSetup {
    /// Map file stem (`assets/maps/<map>.kpmap`).
    pub map: String,
    pub seats: Vec<Seat>,
    /// AI difficulty, 1 Easy … 4 Extreme.
    pub difficulty: u8,
    /// The match seed (AI dice, procedural map content).
    pub seed: u64,
    /// No win / lose check.
    pub sandbox: bool,
    /// The host's rules (pause, speed range, cheats).
    pub rules: MatchRules,
}

impl MatchSetup {
    /// The replay's form of a match's setup.
    pub fn of(setup: &GameSetup) -> Self {
        Self {
            map: setup.map.clone(),
            seats: setup.players.iter().map(Seat::from).collect(),
            difficulty: setup.difficulty,
            seed: setup.seed,
            sandbox: setup.sandbox,
            rules: setup.rules,
        }
    }

    /// The game's form of this setup.
    pub fn game_setup(&self) -> GameSetup {
        GameSetup {
            map: self.map.clone(),
            players: self.seats.iter().map(PlayerSpec::from).collect(),
            difficulty: self.difficulty,
            demo: false,
            showcase: None,
            sandbox: self.sandbox,
            seed: self.seed,
            rules: self.rules,
        }
    }
}

/// What a replay says about its match beyond the sim.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayMeta {
    /// The players' names by seat (empty: an AI seat, or not known).
    pub names: Vec<String>,
    /// Recorded in a networked match.
    pub networked: bool,
    /// When the match was saved, Unix seconds (0: unknown).
    pub saved_at: u64,
}

/// A recorded match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replay {
    pub setup: MatchSetup,
    pub compat: Compat,
    /// Ticks the recording ran: the replay's length.
    pub ticks: u32,
    /// The human seats' frames, by tick then player.
    pub frames: Vec<CommandFrame>,
    pub meta: ReplayMeta,
}

/// Why a replay can't be read or played (the golden player, `headless`,
/// meets the last four, natively).
#[derive(Debug, thiserror::Error)]
#[cfg_attr(
    not(all(any(test, feature = "replay"), not(target_arch = "wasm32"))),
    allow(dead_code)
)]
pub enum ReplayError {
    #[error("not a .kpreplay file")]
    Magic,
    #[error("replay format version {0}, this build reads {FORMAT_VERSION}")]
    Version(u16),
    #[error("corrupt replay: {0}")]
    Decode(#[from] postcard::Error),
    #[error(
        "replay recorded with sim version {}, unit bundle {:016x}, map {:016x}; \
         this build has sim version {}, unit bundle {:016x}, map {:016x}: re-record it",
        recorded.sim_version, recorded.units_hash, recorded.map_hash,
        current.sim_version, current.units_hash, current.map_hash
    )]
    Compat { recorded: Compat, current: Compat },
    #[error("command frame for tick {frame} found at tick {at}: the log is out of order")]
    Order { frame: u32, at: u32 },
    #[error("map {0}: {1}")]
    Map(String, String),
    #[error("{0}")]
    Check(String),
}

impl Replay {
    /// A replay of the match `setup` played on `map_bytes` for `ticks`
    /// ticks, from the frames the `CommandBus` applied (its `log`):
    /// every frame of an owner without AI, i.e. a human player's.
    #[cfg_attr(
        not(all(any(test, feature = "replay"), not(target_arch = "wasm32"))),
        allow(dead_code)
    )]
    pub fn record(
        setup: MatchSetup,
        map_bytes: &[u8],
        ticks: u32,
        applied: impl IntoIterator<Item = CommandFrame>,
    ) -> Self {
        let ai = crate::units::ai::AiSeats::from_setup(&setup.game_setup());
        let frames: Vec<CommandFrame> = applied
            .into_iter()
            .filter(|f| !ai.contains(f.player))
            .collect();
        Self::of_log(
            setup,
            Compat::current(map_bytes),
            ticks,
            frames,
            ReplayMeta::default(),
        )
    }

    /// A replay of a match's turn log: `frames` as the turns carried them
    /// (`Session::log`), the empty ones left out, in tick then player
    /// order.
    pub fn of_log(
        setup: MatchSetup,
        compat: Compat,
        ticks: u32,
        mut frames: Vec<CommandFrame>,
        meta: ReplayMeta,
    ) -> Self {
        frames.retain(|f| !f.commands.is_empty() && f.tick < ticks);
        frames.sort_by_key(|f| (f.tick, f.player));
        Self {
            setup,
            compat,
            ticks,
            frames,
            meta,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend(FORMAT_VERSION.to_le_bytes());
        postcard::to_extend(self, out).expect("replays always serialize")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ReplayError> {
        let rest = bytes.strip_prefix(MAGIC).ok_or(ReplayError::Magic)?;
        let (version, body) = rest.split_at_checked(2).ok_or(ReplayError::Magic)?;
        let version = u16::from_le_bytes([version[0], version[1]]);
        if version != FORMAT_VERSION {
            return Err(ReplayError::Version(version));
        }
        Ok(postcard::from_bytes(body)?)
    }

    /// Commands in the log (all frames).
    #[cfg_attr(not(any(test, feature = "replay")), allow(dead_code))]
    pub fn commands(&self) -> usize {
        self.frames.iter().map(|f| f.commands.len()).sum()
    }

    /// One line about the match, for the Replays page: when, the map,
    /// the seats, how long.
    pub fn label(&self) -> String {
        let when = match self.meta.saved_at {
            0 => String::new(),
            t => {
                let (y, mo, d, h, mi, _) = civil(t);
                format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}  ")
            }
        };
        let names: Vec<&str> = self
            .meta
            .names
            .iter()
            .map(String::as_str)
            .filter(|n| !n.is_empty())
            .collect();
        let who = if names.is_empty() {
            format!("{} seats", self.setup.seats.len())
        } else {
            names.join(", ")
        };
        let clock = crate::ui::game_clock(self.ticks);
        format!(
            "{when}{} - {who} - {clock}",
            crate::map_loading::display_name(&self.setup.map)
        )
    }
}

/// UTC date and time of Unix second `t`: (year, month, day, hour,
/// minute, second) — Howard Hinnant's `civil_from_days`.
fn civil(t: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (t / 86_400) as i64;
    let secs = (t % 86_400) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, secs / 3600, secs / 60 % 60, secs % 60)
}

/// Now, Unix seconds (the file's name and [`ReplayMeta::saved_at`];
/// never the sim's).
fn unix_now() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }
    #[cfg(target_arch = "wasm32")]
    {
        (js_sys::Date::now() / 1000.0) as u64
    }
}

// --- recording -----------------------------------------------------------

/// The content hash of the map the running match loaded (the replay's
/// [`Compat::map_hash`]), set at the swap.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActiveMapHash(pub u64);

/// The shortest match worth a replay file: a second.
const MIN_TICKS: u32 = 30;

/// The replay of the match in `world` with its session's command `log`,
/// if it is one worth keeping: a real match (not the menu's demo, a
/// showcase or the dev tools' sandbox), not itself a replay, at least
/// [`MIN_TICKS`] long.
pub fn replay_of(world: &World, log: Vec<CommandFrame>) -> Option<Replay> {
    let net = world.get_resource::<NetSession>()?;
    let session = &net.session;
    let setup = &world.get_resource::<ActiveMatch>()?.0;
    if session.is_replay()
        || setup.demo
        || setup.showcase.is_some()
        || setup.sandbox
        || session.applied() < MIN_TICKS
    {
        return None;
    }
    let map_hash = world.get_resource::<ActiveMapHash>()?.0;
    let names = world.get_resource::<PeerNames>();
    let roster = session.peers();
    let meta = ReplayMeta {
        names: (0..setup.players.len() as u8)
            .map(|seat| {
                roster
                    .iter()
                    .find(|p| p.player == Some(seat))
                    .zip(names)
                    .map(|(p, n)| n.name(p.peer))
                    .unwrap_or_default()
            })
            .collect(),
        networked: session.is_networked(),
        saved_at: unix_now(),
    };
    Some(Replay::of_log(
        MatchSetup::of(setup),
        Compat::of(Build::current(), map_hash),
        session.applied(),
        log,
        meta,
    ))
}

/// The match in `world` is over (replaced by the next, or the game
/// quits): keep its replay ([`replay_of`]) — natively in [`replay_dir`],
/// on the web in [`RecentReplays`]. Once per match.
pub fn finish_match(world: &mut World) {
    if world
        .get_resource::<NetSession>()
        .is_none_or(|n| n.replay_kept)
    {
        return;
    }
    let Some(log) = world.get_resource_mut::<NetSession>().map(|mut net| {
        net.replay_kept = true;
        net.session.take_log()
    }) else {
        return;
    };
    let Some(replay) = replay_of(world, log) else {
        return;
    };
    let name = file_name(&replay);
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(dir) = world.get_resource::<ReplayDir>().and_then(|d| d.0.clone()) {
        save(&dir, &name, &replay.encode());
    }
    #[cfg(target_arch = "wasm32")]
    {
        let mut recent = world.get_resource_or_init::<RecentReplays>();
        recent.0.insert(0, (name, replay.encode()));
        recent.0.truncate(RECENT_KEPT);
    }
}

/// `2026-10-15_14-03-27_Marble_Madness_Map.kpreplay` (UTC).
fn file_name(replay: &Replay) -> String {
    let (y, mo, d, h, mi, s) = civil(replay.meta.saved_at);
    let map: String = replay
        .setup
        .map
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("{y:04}-{mo:02}-{d:02}_{h:02}-{mi:02}-{s:02}_{map}.kpreplay")
}

/// Where native replays go: `replays/` in the project root (next to the
/// executable in a release package, as `desync-dumps/`), or
/// `KP_REPLAY_DIR` (`cargo testbox` keeps each game's in its directory).
#[cfg(not(target_arch = "wasm32"))]
pub fn replay_dir() -> std::path::PathBuf {
    std::env::var_os("KP_REPLAY_DIR").map_or_else(
        || crate::paths::project_root().join("replays"),
        std::path::PathBuf::from,
    )
}

/// Native: where [`finish_match`] saves (`None`: nowhere — the headless
/// harness, whose tests and replays must not write files).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Resource, Clone, Debug)]
pub struct ReplayDir(pub Option<std::path::PathBuf>);

/// Replay files kept in [`replay_dir`]: the oldest go beyond this.
#[cfg(not(target_arch = "wasm32"))]
const FILES_KEPT: usize = 100;

#[cfg(not(target_arch = "wasm32"))]
fn save(dir: &std::path::Path, name: &str, bytes: &[u8]) {
    let path = dir.join(name);
    match std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&path, bytes)) {
        Ok(()) => info!("replay saved: {} ({} bytes)", path.display(), bytes.len()),
        Err(e) => {
            warn!("cannot save the replay to {}: {e}", path.display());
            return;
        }
    }
    // The names sort by date: drop the oldest beyond the limit.
    let files = list_files(dir);
    for old in files.iter().skip(FILES_KEPT) {
        let _ = std::fs::remove_file(old);
    }
}

/// The `.kpreplay` files in `dir`, newest first (by name: they start
/// with the date).
#[cfg(not(target_arch = "wasm32"))]
pub fn list_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "kpreplay"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files.reverse();
    files
}

/// Web: the replays of this page session (newest first), and files the
/// player opened: `(name, bytes)`.
#[derive(Resource, Default)]
pub struct RecentReplays(pub Vec<(String, Vec<u8>)>);

/// Web: replays kept in memory.
#[cfg(target_arch = "wasm32")]
const RECENT_KEPT: usize = 10;

/// Web: offer `bytes` as a download named `name`; `false` if the page
/// would not.
#[cfg(target_arch = "wasm32")]
pub fn download(name: &str, bytes: &[u8]) -> bool {
    use wasm_bindgen::JsCast;
    let done = (|| {
        let array = js_sys::Uint8Array::from(bytes);
        let parts = js_sys::Array::of1(&array);
        let blob = web_sys::Blob::new_with_u8_array_sequence(&parts).ok()?;
        let url = web_sys::Url::create_object_url_with_blob(&blob).ok()?;
        let a = web_sys::window()?
            .document()?
            .create_element("a")
            .ok()?
            .dyn_into::<web_sys::HtmlAnchorElement>()
            .ok()?;
        a.set_href(&url);
        a.set_download(name);
        a.click();
        web_sys::Url::revoke_object_url(&url).ok()
    })();
    if done.is_none() {
        warn!("could not offer {name} for download");
    }
    done.is_some()
}

// --- playing --------------------------------------------------------------

/// A replay to play at the next match swap (the Replays page inserts
/// it with the replay's [`GameSetup`]; the session takes it).
#[derive(Resource, Clone, Debug)]
pub struct NextReplay {
    pub replay: Replay,
    /// Its file name, for the viewer.
    pub name: String,
}

/// What the viewer shows about the replay it plays (and the replay,
/// to start it over).
#[derive(Clone, Debug)]
pub struct ReplayView {
    pub name: String,
    pub replay: Replay,
    /// The recording's map hash (a different copy of the map plays a
    /// different match).
    pub map_hash: u64,
}

/// Decode `bytes` as a replay this build can play: same sim version
/// and unit data (the map is compared once it loads).
pub fn open(bytes: &[u8]) -> Result<Replay, String> {
    let replay = Replay::decode(bytes).map_err(|e| e.to_string())?;
    if let Some(why) = Build::current().mismatch(&replay.compat.build()) {
        return Err(format!("The replay {why}."));
    }
    Ok(replay)
}

/// Play `replay` (named `name`): its setup is the next match's, the
/// replay waits in [`NextReplay`] for the swap.
pub fn start(commands: &mut Commands, replay: Replay, name: String) {
    info!(
        "playing replay {name}: {} on {}",
        replay.label(),
        replay.setup.map
    );
    commands.insert_resource(replay.setup.game_setup());
    commands.insert_resource(NextReplay { replay, name });
    commands.queue(|world: &mut World| {
        world
            .resource_mut::<NextState<AppState>>()
            .set(AppState::InGame);
    });
}

/// Saves the running match's replay when the game quits (a match the
/// next one replaces is saved at the swap, `match_scope::begin_match`).
pub struct ReplayLogPlugin;

impl Plugin for ReplayLogPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ActiveMapHash>()
            .init_resource::<RecentReplays>()
            .add_systems(Last, save_on_exit);
        #[cfg(not(target_arch = "wasm32"))]
        app.insert_resource(ReplayDir(Some(replay_dir())));
    }
}

fn save_on_exit(world: &mut World) {
    let quitting = world
        .get_resource::<Messages<AppExit>>()
        .is_some_and(|m| !m.is_empty());
    if quitting {
        finish_match(world);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{SimCommand, Units};

    #[test]
    fn dates_and_names() {
        assert_eq!(civil(0), (1970, 1, 1, 0, 0, 0));
        // 2026-10-15 14:03:27 UTC.
        assert_eq!(civil(1_792_073_007), (2026, 10, 15, 14, 3, 27));
        assert_eq!(civil(951_782_400), (2000, 2, 29, 0, 0, 0));
        let replay = Replay::of_log(
            MatchSetup::of(&GameSetup::default()),
            Compat::of(Build::current(), 1),
            3000,
            vec![
                CommandFrame {
                    tick: 9,
                    player: 0,
                    commands: vec![SimCommand::Resign],
                },
                CommandFrame {
                    tick: 4,
                    player: 1,
                    commands: vec![SimCommand::Stop {
                        units: Units::of([crate::net::NetId(3)]),
                    }],
                },
                CommandFrame {
                    tick: 5,
                    player: 1,
                    commands: vec![],
                },
            ],
            ReplayMeta {
                names: vec!["alice".into(), String::new()],
                networked: true,
                saved_at: 1_792_073_007,
            },
        );
        let order: Vec<(u32, u8)> = replay.frames.iter().map(|f| (f.tick, f.player)).collect();
        assert_eq!(order, vec![(4, 1), (9, 0)], "sorted, the empty frame gone");
        assert!(file_name(&replay).starts_with("2026-10-15_14-03-27_"));
        assert!(replay.label().contains("alice"));
        assert!(replay.label().ends_with("1:40"), "{}", replay.label());
        assert_eq!(Replay::decode(&replay.encode()).unwrap(), replay);
        assert!(open(&replay.encode()).is_ok());
    }
}
