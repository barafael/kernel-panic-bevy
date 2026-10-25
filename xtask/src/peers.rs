//! The games of a testbox run: built once, launched into one room, their
//! output collected, their bots driven through `bot/cmd`.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kp_signaling::Signaling;

use crate::{Opts, box_dir, repo_root};

/// What a run adds to the options.
#[derive(Default)]
pub struct Launch {
    /// The room's map, over `--map`.
    pub map: Option<String>,
    /// Written to every bot's `cmd` before the game starts.
    pub first_commands: Vec<String>,
}

pub struct Peers {
    peers: Vec<Peer>,
    /// Lives as long as the games: they meet through it.
    _server: Signaling,
    stop: Arc<AtomicBool>,
    /// Log lines that fail a check: a desync, a panic.
    fatal: Arc<Mutex<Vec<String>>>,
}

struct Peer {
    name: String,
    dir: PathBuf,
    child: Child,
    exited: Option<std::process::ExitStatus>,
}

impl Peers {
    pub fn launch(o: &Opts, launch: &Launch) -> Result<Self, String> {
        let exe = build(o.release)?;
        let stop = on_ctrl_c()?;
        let server = Signaling::start();
        println!("testbox: signaling on {}", server.url);
        let fatal = Arc::new(Mutex::new(Vec::new()));
        let root = box_dir();
        // Last run's games go: a bot reads its `cmd` from the start.
        if root.exists() {
            fs::remove_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
        }
        let map = launch.map.as_ref().or(o.map.as_ref());
        let mut peers = Vec::new();
        for i in 0..o.peers {
            let name = format!("peer{}", i + 1);
            let dir = root.join(&name);
            let config = dir.join("config/kernel-panic");
            fs::create_dir_all(&config).map_err(|e| format!("{}: {e}", config.display()))?;
            fs::create_dir_all(dir.join("bot")).map_err(|e| format!("{}: {e}", dir.display()))?;
            // Windowed (KP_WINDOW too), no tips voice, muted but for one.
            let muted = !(o.sound && i == 0);
            let settings = format!(
                "fullscreen=0\nwindow={}x{}\ntips=0\nvolume={}\n",
                o.size.0,
                o.size.1,
                if muted { 0 } else { 100 }
            );
            write(&config.join("settings.txt"), &settings)?;
            let mut cmd = String::new();
            for line in &launch.first_commands {
                cmd.push_str(line);
                cmd.push('\n');
            }
            write(&dir.join("bot/cmd"), &cmd)?;

            // Two columns, as many rows as it takes.
            let (col, row) = ((i % 2) as u32, (i / 2) as u32);
            let mut command = Command::new(&exe);
            command
                .current_dir(repo_root())
                .env("KP_ROOM", &o.room)
                .env("KP_SIGNALING", &server.url)
                .env("KP_ICE", "none")
                .env("KP_NAME", &name)
                .env("KP_BOT", dir.join("bot"))
                .env("XDG_CONFIG_HOME", dir.join("config"))
                .env("KP_WINDOW", format!("{}x{}", o.size.0, o.size.1))
                .env(
                    "KP_WINDOW_POS",
                    format!("{},{}", col * o.size.0, row * o.size.1),
                )
                .env("NO_COLOR", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if !o.manual {
                command.env("KP_AUTO_START", o.peers.to_string());
                if let Some(map) = map {
                    command.env("KP_AUTO_MAP", map);
                }
                if o.cheats {
                    command.env("KP_AUTO_CHEATS", "1");
                }
            }
            let mut child = command
                .spawn()
                .map_err(|e| format!("{}: {e}", exe.display()))?;
            let log = fs::File::create(dir.join("game.log"))
                .map_err(|e| format!("{}: {e}", dir.display()))?;
            let log = Arc::new(Mutex::new(log));
            let out = child.stdout.take().expect("piped");
            let err = child.stderr.take().expect("piped");
            for stream in [Box::new(out) as Box<dyn Read + Send>, Box::new(err)] {
                let (name, log, fatal, verbose) =
                    (name.clone(), log.clone(), fatal.clone(), o.verbose);
                std::thread::spawn(move || pump(&name, stream, &log, &fatal, verbose));
            }
            println!("testbox: {name} started ({})", dir.display());
            peers.push(Peer {
                name,
                dir,
                child,
                exited: None,
            });
        }
        Ok(Self {
            peers,
            _server: server,
            stop,
            fatal,
        })
    }

    /// Until every game has closed, or Ctrl-C closes them.
    pub fn wait_all(&mut self) {
        while self.peers.iter().any(|p| p.exited.is_none()) {
            if self.stop.load(Ordering::Relaxed) {
                println!("testbox: closing the games");
                self.close(Duration::from_secs(5));
                return;
            }
            self.reap();
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Note the games that exited.
    fn reap(&mut self) {
        for p in &mut self.peers {
            if p.exited.is_none()
                && let Ok(Some(status)) = p.child.try_wait()
            {
                println!("testbox: {} exited: {status}", p.name);
                p.exited = Some(status);
            }
        }
    }

    /// Every game still running, no Ctrl-C and no fatal log line.
    fn poll(&mut self) -> Result<(), String> {
        self.reap();
        if let Some(p) = self.peers.iter().find(|p| p.exited.is_some()) {
            return Err(format!("{} exited early (see its game.log)", p.name));
        }
        if self.stop.load(Ordering::Relaxed) {
            return Err("interrupted".into());
        }
        if let Some(line) = self.fatal.lock().unwrap().first() {
            return Err(line.clone());
        }
        Ok(())
    }

    /// Wait until every bot logged `ack <token>`: the bot frame of each.
    pub fn wait_ack(&mut self, token: &str, limit: Duration) -> Result<Vec<u64>, String> {
        let t0 = Instant::now();
        loop {
            self.poll()?;
            let frames: Vec<Option<u64>> = self.peers.iter().map(|p| ack(&p.dir, token)).collect();
            if let Some(frames) = frames.iter().copied().collect::<Option<Vec<u64>>>() {
                return Ok(frames);
            }
            if t0.elapsed() > limit {
                let missing: Vec<&str> = self
                    .peers
                    .iter()
                    .zip(&frames)
                    .filter(|(_, f)| f.is_none())
                    .map(|(p, _)| p.name.as_str())
                    .collect();
                return Err(format!(
                    "no `ack {token}` from {missing:?} within {limit:?} (see bot/log, game.log)"
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// The team each bot logged at its first tick (`team <n>`).
    pub fn teams(&self) -> Vec<Option<u8>> {
        self.peers
            .iter()
            .map(|p| {
                let log = fs::read_to_string(p.dir.join("bot/log")).ok()?;
                log.lines()
                    .find_map(|l| l.split_once("] team ")?.1.parse().ok())
            })
            .collect()
    }

    /// Every game running for `span`, without a fatal line.
    pub fn watch(&mut self, span: Duration) -> Result<(), String> {
        let t0 = Instant::now();
        while t0.elapsed() < span {
            self.poll()?;
            std::thread::sleep(Duration::from_millis(200));
        }
        self.poll()
    }

    pub fn send_all(&self, line: &str) {
        for p in self.peers.iter().filter(|p| p.exited.is_none()) {
            if let Err(why) = append_line(&p.dir.join("bot/cmd"), line) {
                eprintln!("testbox: {why}");
            }
        }
    }

    /// Wait up to `grace` for the games to close, then kill the rest.
    pub fn close(&mut self, grace: Duration) {
        let t0 = Instant::now();
        for p in &mut self.peers {
            while p.exited.is_none() {
                match p.child.try_wait() {
                    Ok(Some(status)) => p.exited = Some(status),
                    Ok(None) if t0.elapsed() < grace => {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    _ => {
                        let _ = p.child.kill();
                        p.exited = p.child.wait().ok();
                    }
                }
            }
        }
    }
}

impl Drop for Peers {
    fn drop(&mut self) {
        for p in self.peers.iter_mut().filter(|p| p.exited.is_none()) {
            let _ = p.child.kill();
            let _ = p.child.wait();
        }
    }
}

/// The game's binary, built for `release` or the dev profile.
fn build(release: bool) -> Result<PathBuf, String> {
    let mut cargo = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cargo
        .current_dir(repo_root())
        .args(["build", "-p", "kernel-panic", "--bin", "kernel-panic"])
        .arg("--message-format=json-render-diagnostics")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if release {
        cargo.arg("--release");
    }
    println!(
        "testbox: building kernel-panic{}",
        if release { " (release)" } else { "" }
    );
    let out = cargo.output().map_err(|e| format!("cargo build: {e}"))?;
    if !out.status.success() {
        return Err(format!("cargo build: {}", out.status));
    }
    // The artifact line of the binary: `"executable":"<path>"`.
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(r#""reason":"compiler-artifact""#))
        .find_map(|l| {
            let rest = &l[l.find(r#""executable":""#)? + 14..];
            Some(PathBuf::from(&rest[..rest.find('"')?]))
        })
        .ok_or_else(|| "cargo build named no kernel-panic executable".into())
}

/// Copy a game's output into its `game.log`, echoing what matters (or
/// everything, `verbose`) with the game's name in front.
fn pump(
    name: &str,
    stream: Box<dyn Read + Send>,
    log: &Mutex<fs::File>,
    fatal: &Mutex<Vec<String>>,
    verbose: bool,
) {
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        let line = strip_ansi(&line);
        let _ = writeln!(log.lock().unwrap(), "{line}");
        let lower = line.to_lowercase();
        if lower.contains("desync") || lower.contains("panicked at") {
            fatal.lock().unwrap().push(format!("{name}: {line}"));
        }
        // WebRTC's own warnings (candidates it cannot use, a closing
        // connection's queues) are routine on a loopback mesh.
        let routine = line.contains(" webrtc") || line.contains(" matchbox_socket");
        let shown = verbose
            || !routine
                && [
                    " WARN ",
                    " ERROR ",
                    "autopilot",
                    "KP_BOT",
                    " room ",
                    "desync",
                    "panicked",
                ]
                .iter()
                .any(|w| line.contains(w));
        if shown {
            println!("[{name}] {line}");
        }
    }
}

/// The bot frame of the first `ack <token>` in `dir`'s bot log.
fn ack(dir: &Path, token: &str) -> Option<u64> {
    let log = fs::read_to_string(dir.join("bot/log")).ok()?;
    let want = format!("] ack {token}");
    log.lines()
        .find(|l| l.ends_with(&want))
        .and_then(|l| l.strip_prefix('[')?.split(']').next()?.parse().ok())
}

pub fn append_line(path: &Path, line: &str) -> Result<(), String> {
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .and_then(|mut f| writeln!(f, "{line}"))
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Set on Ctrl-C (the games, in the same process group, get it too).
pub fn on_ctrl_c() -> Result<Arc<AtomicBool>, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let set = stop.clone();
    ctrlc::set_handler(move || set.store(true, Ordering::Relaxed))
        .map_err(|e| format!("Ctrl-C handler: {e}"))?;
    Ok(stop)
}

/// Without terminal colour codes (`NO_COLOR` is a request, not a promise).
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // CSI: ESC [ parameters, then a final letter.
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
