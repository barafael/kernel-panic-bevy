//! `cargo testbox`, a Cargo alias of this xtask (`.cargo/config.toml`):
//! several games on this machine in one multiplayer room, introduced by
//! a signaling server in this process ([`kp_signaling`]), talking WebRTC
//! over host candidates only. Nothing leaves the machine.
//!
//! Each game gets a directory `target/testbox/peer<n>/`: its log
//! (`game.log`), its settings (`config/`, as `XDG_CONFIG_HOME`) and its
//! file-driven bot (`bot/`, `KP_BOT`: `cmd` in, `log` and `state.txt`
//! out; the command list is in `kernel-panic/src/bot.rs`). The lobby's
//! autopilot (`KP_AUTO_START`, `kernel-panic/src/net/autopilot.rs`)
//! seats everyone and starts the match, unless `--manual`.

mod peers;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use peers::{Launch, Peers};

const USAGE: &str = "\
cargo testbox: several Kernel Panic games in one multiplayer room, on this machine.

USAGE:
    cargo testbox up [OPTIONS]          play: launch the games, wait until they close (Ctrl-C ends all)
    cargo testbox test [OPTIONS]        check: a match starts, runs --secs in lockstep, no desync
    cargo testbox send <peer> <cmd>…    append a bot command to a running game's bot/cmd
                                        (<peer>: 1, peer1 or all; e.g. `send all shot now`)
    cargo testbox signaling [--port P]  only the signaling server, for games started by hand
                                        (`cargo run -- --room <code> --signaling <url>`, KP_ICE=none)

OPTIONS (up, test):
    -n, --peers <N>       games in the room [default: 2]
    --release             build and run the release profile
    --map <stem>          the room's map [default: the host's random pick; test: Data_Cache_L1]
    --cheats              cheats on in the room (the bot's spawn, kill, hp)
    --room <code>         the room code [default: testbox]
    --size <W>x<H>        each window's size [default: 960x600]
    --manual              no autopilot: seat, ready and start by hand
    --rejoin              up: a game that closes while another plays starts again as the
                          same player (`send 2 quit` leaves, and it comes back)
    --sound               leave peer1's sound on (all games are muted otherwise)
    -v, --verbose         echo every log line, not only warnings, errors and the lobby's
    --secs <S>            test: how long the match must run [default: 30]
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let outcome = match args.first().map(String::as_str) {
        Some("up") => Opts::parse(&args[1..]).and_then(|o| up(&o)),
        Some("test") => Opts::parse(&args[1..]).and_then(|o| test(&o)),
        Some("send") => send(&args[1..]),
        Some("signaling") => signaling(&args[1..]),
        Some("-h" | "--help" | "help") | None => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("testbox: {why}");
            ExitCode::FAILURE
        }
    }
}

pub struct Opts {
    pub peers: usize,
    pub release: bool,
    pub map: Option<String>,
    pub cheats: bool,
    pub room: String,
    pub size: (u32, u32),
    pub manual: bool,
    pub rejoin: bool,
    pub sound: bool,
    pub verbose: bool,
    pub secs: u64,
}

impl Opts {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut o = Self {
            peers: 2,
            release: false,
            map: None,
            cheats: false,
            room: "testbox".into(),
            size: (960, 600),
            manual: false,
            rejoin: false,
            sound: false,
            verbose: false,
            secs: 30,
        };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut value = || it.next().ok_or_else(|| format!("{a} needs a value"));
            match a.as_str() {
                "-n" | "--peers" => {
                    o.peers = value()?.parse().map_err(|_| "--peers: a number")?;
                }
                "--release" => o.release = true,
                "--map" => o.map = Some(value()?.clone()),
                "--cheats" => o.cheats = true,
                "--room" => o.room = value()?.clone(),
                "--size" => {
                    let v = value()?;
                    o.size = v
                        .split_once('x')
                        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                        .filter(|&(w, h)| w >= 320 && h >= 240)
                        .ok_or("--size: <W>x<H>, at least 320x240")?;
                }
                "--manual" => o.manual = true,
                "--rejoin" => o.rejoin = true,
                "--sound" => o.sound = true,
                "-v" | "--verbose" => o.verbose = true,
                "--secs" => o.secs = value()?.parse().map_err(|_| "--secs: a number")?,
                _ => return Err(format!("unknown option {a:?}\n\n{USAGE}")),
            }
        }
        if !(2..=8).contains(&o.peers) {
            return Err("--peers: 2 to 8 (a match seats at most 8)".into());
        }
        Ok(o)
    }
}

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace root")
        .to_owned()
}

/// Where the games' directories go.
pub fn box_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| repo_root().join("target"), PathBuf::from)
        .join("testbox")
}

fn up(o: &Opts) -> Result<(), String> {
    let mut peers = Peers::launch(o, &Launch::default())?;
    println!(
        "testbox: {} games in room {:?}; Ctrl-C closes them all. Drive a bot with \
         `cargo testbox send <peer|all> <command>`.",
        o.peers, o.room
    );
    peers.wait_all();
    Ok(())
}

fn test(o: &Opts) -> Result<(), String> {
    if o.manual {
        return Err("test needs the autopilot (no --manual)".into());
    }
    let launch = Launch {
        // A small map keeps loading short; any map in the catalog will do.
        map: Some(o.map.clone().unwrap_or_else(|| "Data_Cache_L1".into())),
        // The bot reads its commands only in a match: the ack is the sign
        // that one runs.
        first_commands: vec!["ping started".into()],
    };
    let mut peers = Peers::launch(o, &launch)?;
    let outcome = check(o, &mut peers);
    peers.send_all("quit");
    peers.close(Duration::from_secs(15));
    match &outcome {
        Ok(()) => println!("testbox: PASS"),
        Err(why) => println!("testbox: FAIL: {why}"),
    }
    println!("testbox: logs in {}", box_dir().display());
    outcome
}

fn check(o: &Opts, peers: &mut Peers) -> Result<(), String> {
    // A debug build's first frames, the room and the map's load.
    let start = Duration::from_secs(if o.release { 90 } else { 240 });
    let frames = peers.wait_ack("started", start)?;
    println!(
        "testbox: every game is in the match (bot frames {frames:?}, teams {:?})",
        peers.teams()
    );
    peers.watch(Duration::from_secs(o.secs))?;
    peers.send_all("ping done");
    let frames = peers.wait_ack("done", Duration::from_secs(30))?;
    println!(
        "testbox: still in lockstep after {} s (bot frames {frames:?})",
        o.secs
    );
    Ok(())
}

fn send(args: &[String]) -> Result<(), String> {
    let [peer, command @ ..] = args else {
        return Err(format!("send needs a peer and a command\n\n{USAGE}"));
    };
    if command.is_empty() {
        return Err("send needs a command (see kernel-panic/src/bot.rs)".into());
    }
    let line = command.join(" ");
    let dirs: Vec<PathBuf> = if peer == "all" {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(box_dir())
            .map_err(|e| {
                format!(
                    "{}: {e} (is `cargo testbox up` running?)",
                    box_dir().display()
                )
            })?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.join("bot").is_dir())
            .collect();
        dirs.sort();
        dirs
    } else {
        let n = peer.strip_prefix("peer").unwrap_or(peer);
        vec![box_dir().join(format!("peer{n}"))]
    };
    for dir in dirs {
        let cmd = dir.join("bot/cmd");
        if !cmd.exists() {
            return Err(format!("{}: no such game", cmd.display()));
        }
        peers::append_line(&cmd, &line)?;
        println!(
            "{}: {line}",
            dir.file_name().unwrap_or_default().to_string_lossy()
        );
    }
    Ok(())
}

fn signaling(args: &[String]) -> Result<(), String> {
    let port: u16 = match args {
        [] => 0,
        [flag, p] if flag == "--port" => p.parse().map_err(|_| "--port: a number")?,
        _ => return Err(format!("signaling takes only --port\n\n{USAGE}")),
    };
    let server = kp_signaling::Signaling::start_at(([127, 0, 0, 1], port).into())
        .map_err(|e| format!("signaling server: {e}"))?;
    println!(
        "testbox: signaling on {}; Ctrl-C stops it. A game joins with\n  \
         KP_ICE=none cargo run -- --room <code> --signaling {}",
        server.url, server.url
    );
    let stop = peers::on_ctrl_c()?;
    let mut last = Vec::new();
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        let rooms = server.rooms();
        if rooms != last {
            println!("testbox: rooms {rooms:?}");
            last = rooms;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}
