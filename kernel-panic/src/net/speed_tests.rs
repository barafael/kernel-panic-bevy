//! Speed control with the whole game (`net::speed`): the full-sim room
//! of [`super::multipeer_tests`], one guest on a machine too slow for
//! the game at 1× ([`Pacer::cost`](super::driver::Pacer::cost): a frame
//! runs only the ticks that fit in it). The host slows the game until
//! that guest keeps up, every peer changes speed on the same tick, and
//! the speed returns to 1× once the guest is fast again.
//!
//! `--nocapture` prints the speed steps and the slow guest's lag.

use std::time::Duration;

use super::multipeer_tests::{Room, SERIAL, scenario, start};
use super::sim_net::LinkParams;
use super::speed::{SERVER_PLAYER, SpeedPolicy};
use super::{CommandBus, MatchControl, SimCommand};
use crate::net::driver::{NetSession, NetStatus};

const FRAME: Duration = Duration::from_micros(33_333);

/// 50 ms a tick: 20 ticks a second, two thirds of 1× (a load of 1.5).
const SLOW: Duration = Duration::from_millis(50);

fn frames(secs: u32) -> u32 {
    secs * 30
}

/// Three players and an AI; peer 2 (a guest) on the slow machine. Every
/// peer logs what it applies.
fn slow_room(seed: u64) -> Room {
    let scn = scenario("Marble_Madness_Map", 3, 1, seed);
    let mut r = start(&scn, seed, LinkParams::latency_ms(30));
    for p in &mut r.peers {
        p.h.world_mut().resource_mut::<CommandBus>().log = Some(Vec::new());
    }
    set_cost(&mut r, 2, SLOW);
    r
}

fn set_cost(r: &mut Room, peer: usize, cost: Duration) {
    r.peers[peer]
        .h
        .world_mut()
        .resource_mut::<NetSession>()
        .pacer
        .cost = Some(cost);
}

/// The speed control's commands a peer applied: (tick, percent).
fn speed_log(r: &mut Room, peer: usize) -> Vec<(u32, u16)> {
    let w = r.peers[peer].h.world_mut();
    let bus = w.resource::<CommandBus>();
    bus.log
        .iter()
        .flatten()
        .filter(|f| f.player == SERVER_PLAYER)
        .flat_map(|f| {
            f.commands.iter().map(move |c| match c {
                SimCommand::InternalSpeed { percent } => (f.tick, *percent),
                other => panic!("the server sent {other:?}"),
            })
        })
        .collect()
}

fn control(r: &mut Room, peer: usize) -> MatchControl {
    r.peers[peer]
        .h
        .world_mut()
        .resource::<MatchControl>()
        .clone()
}

/// Ticks the slow guest is behind the host.
fn lag(r: &mut Room) -> usize {
    r.trail(0).len().saturating_sub(r.trail(2).len())
}

/// The host slows the game to about half speed for the slow guest
/// (Spring's maximum rule: its load ends near 75 %), the guest keeps
/// up, every peer applies each speed step on the same tick; the guest
/// gets fast again and the game returns to 1×.
#[test]
fn a_slow_guest_slows_the_game_full_sim() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut r = slow_room(0x51);
    let mut worst = 0;
    let mut guest_saw = None;
    r.run(frames(30), FRAME, |r, f| {
        if f > frames(20) {
            worst = worst.max(lag(r));
        }
        if f == frames(25) {
            // Another guest's player list: everyone's load, from the host.
            let st: NetStatus = r.status(1);
            guest_saw = st.peers[2].load_permille;
        }
    });
    let slowed = speed_log(&mut r, 0);
    eprintln!("slow phase: speed steps (tick, %) {slowed:?}");
    let lowest = slowed
        .iter()
        .map(|s| s.1)
        .min()
        .expect("the host slowed the game");
    assert!(
        (45..=56).contains(&lowest),
        "settles near 50 % (a load of 1.5 at 1×, aiming at 75 %): {lowest}"
    );
    let at_end = control(&mut r, 0).internal_percent;
    assert!((45..=56).contains(&at_end), "still slowed: {at_end} %");
    eprintln!(
        "lag of the slow guest after settling: at most {worst} ticks, now {}",
        lag(&mut r)
    );
    assert!(
        worst < 45,
        "the slow guest keeps up once slowed: {worst} ticks behind"
    );
    let seen = guest_saw.expect("the guest's player list shows the slow guest's load");
    assert!(seen > 600, "its load near 75 %: {seen} permille");

    // Fast again: back to the players' speed.
    set_cost(&mut r, 2, Duration::from_millis(1));
    r.run(frames(25), FRAME, |_, _| {});
    let all = speed_log(&mut r, 0);
    eprintln!("recovery: speed steps {:?}", &all[slowed.len()..]);
    for i in 0..r.peers.len() {
        let c = control(&mut r, i);
        assert_eq!(c.internal_percent, 100, "peer {i} back at 1×");
        assert_eq!(c.speed_percent, 100, "the players' speed never changed");
        // Same steps on the same ticks everywhere (as far as each got).
        let log = speed_log(&mut r, i);
        assert_eq!(log[..], all[..log.len()], "peer {i} applied the same steps");
        assert!(log.len() + 1 >= all.len(), "peer {i} applied the steps");
    }
    r.assert_agree(900);
    r.report("slow guest");
}

/// Spring's default rule, the median: one slow player of three is not
/// waited for and falls further and further behind (why the maximum is
/// the default here).
#[test]
fn the_median_leaves_a_slow_guest_behind_full_sim() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut r = slow_room(0x52);
    r.peers[0]
        .h
        .world_mut()
        .resource_mut::<NetSession>()
        .session
        .set_speed_policy(SpeedPolicy::Median);
    r.run(frames(12), FRAME, |_, _| {});
    assert!(speed_log(&mut r, 0).is_empty(), "never slowed");
    let behind = lag(&mut r);
    eprintln!("median: the slow guest is {behind} ticks behind after 12 s");
    // 10 ticks a second short.
    assert!(behind > 80, "{behind} ticks behind");
    r.assert_agree(200);
}

/// A hidden browser tab (agent I's `Msg::Away`) does not drag the game
/// down: the slow guest's tab is hidden, so the host leaves it out of
/// the speed control and plays on at 1×.
#[test]
fn a_hidden_tab_does_not_slow_the_game_full_sim() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut r = slow_room(0x53);
    r.peers[2]
        .h
        .world_mut()
        .insert_resource(crate::net::driver::TabHidden(true));
    r.run(frames(12), FRAME, |_, _| {});
    assert!(r.status(0).peers[2].away, "the host sees the tab hidden");
    assert!(speed_log(&mut r, 0).is_empty(), "never slowed");
    assert_eq!(control(&mut r, 0).internal_percent, 100);
    r.assert_agree(200);
}
