//! The lobby with the whole game: two headless apps (the M0 harness's
//! app, menu and all) meet in a room through the Multiplayer page's
//! actions, the host starts, and both leave the menu for the match
//! through the normal match swap — with the attract demo behind the
//! menu still loading or playing — into a networked session whose
//! per-tick state hashes agree. Then both go back to the room (M3):
//! same code, same seats, chat, ready for a rematch.
//!
//! The room is an in-process network ([`RoomConnector::sim`]; the game
//! opens matchbox); the protocol over a lossy network is
//! `lobby::tests`.

use std::time::{Duration, Instant};

use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;

use super::driver::{NetSession, NetStatus};
use super::lobby::{LobbyAction, LobbyRoom, Occupant, RoomConnector};
use crate::game_setup::{ActiveMatch, AppState};
use crate::headless::{Executor, HashLog, build_app};
use crate::units::components::Homebase;
use crate::units::player::LocalTeam;

const TICK: Duration = Duration::from_nanos(1_000_000_000 / 30);
const MAP: &str = "Marble_Madness_Map";

fn act(app: &mut App, action: LobbyAction) {
    app.world_mut()
        .resource_mut::<Messages<LobbyAction>>()
        .write(action);
}

fn frames(apps: &mut [App], n: u32) {
    for _ in 0..n {
        for app in apps.iter_mut() {
            app.update();
        }
    }
}

fn lobby<R>(app: &mut App, f: impl FnOnce(&mut super::lobby::Lobby) -> R) -> R {
    f(app
        .world_mut()
        .resource_mut::<LobbyRoom>()
        .lobby
        .as_mut()
        .expect("in a room"))
}

/// The match's ticks of an app: the last `applied` of its hash log
/// (the demo's ticks before the swap come first).
fn trail(app: &mut App) -> Vec<u64> {
    let applied = app.world().resource::<NetSession>().session.applied() as usize;
    let log = &app.world().resource::<HashLog>().ticks;
    log[log.len() - applied.min(log.len())..]
        .iter()
        .map(|t| t.sim)
        .collect()
}

#[test]
fn two_apps_meet_start_and_agree() {
    let _serial = super::multipeer_tests::SERIAL
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut apps = [build_app(Executor::Single), build_app(Executor::Single)];
    let rooms = RoomConnector::sim();
    for app in &mut apps {
        app.insert_resource(TimeUpdateStrategy::ManualDuration(TICK));
        app.insert_resource(rooms.clone());
        app.world_mut().resource_mut::<HashLog>().enabled = true;
        // Startup: the menu's attract demo starts loading.
        app.update();
    }

    // The creator opens a room, the other joins it by its code.
    act(&mut apps[0], LobbyAction::Create);
    frames(&mut apps, 2);
    let code = apps[0]
        .world()
        .resource::<LobbyRoom>()
        .room
        .clone()
        .expect("a room")
        .to_string();
    apps[1].world_mut().resource_mut::<LobbyRoom>().code = code.clone();
    act(&mut apps[1], LobbyAction::Join);
    frames(&mut apps, 5);

    // Either may host (the lower id); the creator keeps seat 0 and the
    // joiner takes the open one.
    let host = usize::from(lobby(&mut apps[1], |l| l.is_host()));
    let guest = 1 - host;
    let room = lobby(&mut apps[guest], |l| l.room().cloned()).expect("the host's room");
    let ids: Vec<_> = (0..2)
        .map(|i| lobby(&mut apps[i], |l| l.me().unwrap()))
        .collect();
    assert_eq!(room.seats[0].who, Occupant::Human(ids[0]));
    assert_eq!(room.seats[1].who, Occupant::Human(ids[1]));
    assert_eq!(room.members.len(), 2);

    // The host picks the map and adds an AI on the creator's team; the
    // guest gets ready.
    lobby(&mut apps[host], |l| l.set_map(MAP)).unwrap();
    act(&mut apps[host], LobbyAction::AddSeat);
    act(&mut apps[host], LobbyAction::Occupant(2));
    frames(&mut apps, 3);
    act(&mut apps[guest], LobbyAction::Ready(true));
    frames(&mut apps, 3);
    assert_eq!(lobby(&mut apps[host], |l| l.start_blocker()), None);
    let room_at_start = lobby(&mut apps[host], |l| l.room().cloned()).expect("a room");
    act(&mut apps[host], LobbyAction::Start);

    // Both leave the menu and load the map.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        frames(&mut apps, 1);
        let loaded = apps.iter_mut().all(|app| {
            let world = app.world_mut();
            let bases = world
                .query_filtered::<(), With<Homebase>>()
                .iter(world)
                .count();
            world
                .get_resource::<ActiveMatch>()
                .is_some_and(|m| m.0.map == MAP && !m.0.demo)
                && bases == 3
        });
        if loaded {
            break;
        }
        assert!(Instant::now() < deadline, "the match never loaded");
        std::thread::sleep(Duration::from_millis(2));
    }
    for (i, app) in apps.iter_mut().enumerate() {
        let world = app.world();
        assert_eq!(*world.resource::<State<AppState>>().get(), AppState::InGame);
        assert!(
            world.resource::<LobbyRoom>().lobby.is_none(),
            "the room is the match's now"
        );
        let session = &world.resource::<NetSession>().session;
        assert!(session.is_networked(), "app {i} plays the networked match");
        assert_eq!(session.is_host(), i == host);
        assert_eq!(session.player(), Some(i as u8));
        assert_eq!(
            world.resource::<LocalTeam>().0,
            i as u8,
            "the seat is the local team"
        );
        let setup = &world.resource::<ActiveMatch>().0;
        assert_eq!(setup.players.len(), 3);
        assert!(setup.players[2].ai && !setup.players[0].ai && !setup.players[1].ai);
    }

    // Play: the hash trails agree tick by tick.
    let deadline = Instant::now() + Duration::from_secs(300);
    while apps
        .iter()
        .any(|a| a.world().resource::<NetSession>().session.applied() < 300)
    {
        frames(&mut apps, 1);
        assert!(Instant::now() < deadline, "the match stalled");
    }
    let (a, b) = (trail(&mut apps[0]), trail(&mut apps[1]));
    let n = a.len().min(b.len());
    assert!(n >= 300, "{n} ticks in common");
    if let Some(k) = (0..n).find(|&k| a[k] != b[k]) {
        panic!("the trails differ at tick {}", k + 1);
    }
    for app in &apps {
        let status = app.world().resource::<NetStatus>();
        assert!(status.networked && status.desync.is_none() && status.ended.is_none());
    }

    // Each match is still recorded when it is left for the room (the
    // replay is written at the next match swap: the menu's demo, or the
    // rematch).
    let replay_dirs: Vec<std::path::PathBuf> = (0..apps.len())
        .map(|i| std::env::temp_dir().join(format!("kp-back-to-room-{}-{i}", std::process::id())))
        .collect();
    for (app, dir) in apps.iter_mut().zip(&replay_dirs) {
        let _ = std::fs::remove_dir_all(dir);
        app.insert_resource(crate::net::replay_log::ReplayDir(Some(dir.clone())));
    }

    // Back to the room: the guest first (the host plays on alone), then
    // the host, each with the Esc overlay's Back to room.
    crate::ui::menu::press_back_to_room(apps[guest].world_mut());
    frames(&mut apps, 30);
    crate::ui::menu::press_back_to_room(apps[host].world_mut());
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        frames(&mut apps, 1);
        let back = apps.iter().all(|app| {
            let world = app.world();
            *world.resource::<State<AppState>>().get() == AppState::Menu
                && world.resource::<LobbyRoom>().lobby.is_some()
        });
        if back {
            break;
        }
        assert!(Instant::now() < deadline, "never got back to the room");
    }
    // Long enough for both to say hello and settle (3 s), and the host
    // to take its room back.
    frames(&mut apps, 150);
    for app in apps.iter_mut() {
        let world = app.world();
        assert_eq!(
            world
                .resource::<LobbyRoom>()
                .room
                .as_ref()
                .map(|r| r.to_string()),
            Some(code.clone()),
            "the same room"
        );
        assert_eq!(
            *world.resource::<crate::ui::menu::MenuPage>(),
            crate::ui::menu::MenuPage::Multiplayer
        );
    }
    for (app, dir) in apps.iter_mut().zip(&replay_dirs) {
        let count = |dir: &std::path::Path| std::fs::read_dir(dir).map_or(0, |d| d.count());
        if count(dir) == 0 {
            // No swap yet (the menu's demo had not loaded): the swap's
            // call. After it, the session is the demo's (which the
            // harness does not mark as one), so no second call.
            crate::net::replay_log::finish_match(app.world_mut());
        }
        let files: Vec<String> = std::fs::read_dir(dir).map_or(Vec::new(), |d| {
            d.filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
                .collect()
        });
        let _ = std::fs::remove_dir_all(dir);
        assert_eq!(
            files.len(),
            1,
            "the match left for the room was recorded: {files:?}"
        );
        app.insert_resource(crate::net::replay_log::ReplayDir(None));
    }
    assert!(lobby(&mut apps[host], |l| l.is_host()));
    let back = lobby(&mut apps[guest], |l| l.room().cloned()).expect("the host's room");
    assert_eq!(back.seats, room_at_start.seats, "the seats as they were");
    assert_eq!(back.members.len(), 2);
    assert_eq!(
        lobby(&mut apps[host], |l| l.start_blocker()),
        Some(format!(
            "waiting for {} to be ready",
            back.member(ids[guest]).unwrap().name
        ))
    );

    // Talk, then ready for the rematch.
    lobby(&mut apps[guest], |l| l.say("gg")).unwrap();
    act(&mut apps[guest], LobbyAction::Ready(true));
    frames(&mut apps, 10);
    for app in &apps {
        let log = app.world().resource::<super::chat::ChatLog>();
        let last = log.lines.back().map(|(_, l)| l.text.as_str());
        assert_eq!(last, Some("gg"));
    }
    assert_eq!(lobby(&mut apps[host], |l| l.start_blocker()), None);
}
