//! The network overlay of a networked match, from the driver's
//! [`NetStatus`] (never the sim):
//!
//! - **Player list**, top right: seat, ally / enemy / spectator, name,
//!   round trip to the host — Spring's player roster
//!   (`Game/UI/PlayerRosterDrawer.cpp`: "Num, User name, CPU, Ping",
//!   `A` / `E` / `S` prefixes, the ping coloured green → red between
//!   250 and 1000 ms). Shown by default as Spring's (`ShowPlayerInfo`
//!   1, `Game/Game.cpp:148`); [`TOGGLE_KEY`] hides and shows it (Spring
//!   binds no key to its `info` action). The input delay in force heads
//!   it.
//! - **Banner**, top left: a desync (and where the dump went), the end
//!   of the match for this peer (the host left or was lost, the host
//!   dropped us), leaving, a stall ("Waiting for …"), catching up with
//!   a match joined late, and for a few seconds who left, timed out,
//!   came back or joined.

use std::time::Duration;

use bevy::picking::Pickable;
use bevy::prelude::*;

use crate::game_setup::{AppState, SPECTATOR_TEAM};
use crate::net::MatchControl;
use crate::net::driver::{NetStatus, Notice, PeerNames};
use crate::net::session::Ended;
use crate::sim::DetFloat;
use crate::units::player::LocalTeam;
use crate::units::teams::Alliances;

/// Shows / hides the player list.
pub const TOGGLE_KEY: KeyCode = KeyCode::F9;

/// The player list is shown.
#[derive(Resource)]
struct PlayerListShown(bool);

impl Default for PlayerListShown {
    fn default() -> Self {
        Self(true)
    }
}

#[derive(Component)]
struct PlayerList;

#[derive(Component)]
struct NetBanner;

pub(super) struct NetOverlayPlugin;

impl Plugin for NetOverlayPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlayerListShown>().add_systems(
            Update,
            (toggle_player_list, show_player_list, show_banner)
                .chain()
                .run_if(in_state(AppState::InGame)),
        );
    }
}

fn toggle_player_list(keys: Res<ButtonInput<KeyCode>>, mut shown: ResMut<PlayerListShown>) {
    if keys.just_pressed(TOGGLE_KEY) {
        shown.0 = !shown.0;
    }
}

/// One line of the player list.
#[derive(Clone, Debug, PartialEq)]
struct Row {
    text: String,
    color: Color,
}

const HEAD: Color = Color::srgb(1.0, 1.0, 0.25);
const GONE: Color = Color::srgb(0.5, 0.5, 0.5);
const WAITING: Color = Color::srgb(1.0, 0.85, 0.3);

/// Spring's ping colour (`PlayerRosterDrawer.cpp`): green up to 250 ms,
/// red from 1000 ms.
fn ping_color(ms: u32) -> Color {
    let ms = ms as f32;
    Color::srgb(
        ((ms - 250.0) / 375.0).det_clamp(0.01, 1.0),
        ((1000.0 - ms) / 375.0).det_clamp(0.01, 1.0),
        0.01,
    )
}

/// What the list says: a heading with the input delay, then a row per
/// peer in roster order.
fn player_rows(
    status: &NetStatus,
    names: &PeerNames,
    local: u8,
    alliances: Option<&Alliances>,
    control: Option<&MatchControl>,
) -> Vec<Row> {
    let mut rows = vec![Row {
        text: format!(
            "Players · delay {} ({} ms)",
            status.delay,
            status.delay * 1000 / 30
        ),
        color: HEAD,
    }];
    let waiting: &[_] = status.waiting.as_ref().map_or(&[], |(who, _)| who);
    for p in &status.peers {
        let (seat, side) = match p.player {
            Some(seat) => {
                let ally = local != SPECTATOR_TEAM
                    && alliances.map_or(seat == local, |a| a.friendly(seat, local));
                (format!("{seat:>2}"), if ally { "A" } else { "E" })
            }
            None => (" -".to_string(), "S"),
        };
        let mut name = names.name(p.peer);
        if p.host {
            name.push_str(" (host)");
        }
        let out = p
            .player
            .and_then(|s| control.map(|c| (c.dropped.contains(&s), c.resigned.contains(&s))));
        let (state, color) = if p.gone {
            ("left".to_string(), GONE)
        } else if waiting.contains(&p.peer) && !p.me {
            ("waiting".to_string(), WAITING)
        } else {
            match p.ping_ms {
                Some(ms) => (format!("{ms} ms"), ping_color(ms)),
                None => ("…".to_string(), GONE),
            }
        };
        let note = match out {
            Some((true, _)) => "  dropped",
            Some((false, true)) => "  resigned",
            _ => "",
        };
        rows.push(Row {
            text: format!("{seat} {side:<2} {name:<16} {state}{note}"),
            color: if p.me && !p.gone { Color::WHITE } else { color },
        });
    }
    rows
}

#[allow(clippy::too_many_arguments)]
fn show_player_list(
    mut commands: Commands,
    shown: Res<PlayerListShown>,
    status: Option<Res<NetStatus>>,
    names: Option<Res<PeerNames>>,
    local: Option<Res<LocalTeam>>,
    alliances: Option<Res<Alliances>>,
    control: Option<Res<MatchControl>>,
    list: Query<Entity, With<PlayerList>>,
    mut last: Local<Option<Vec<Row>>>,
) {
    let rows = match status.filter(|s| s.networked && shown.0) {
        Some(status) => player_rows(
            &status,
            &names.map(|n| n.clone()).unwrap_or_default(),
            local.map_or(SPECTATOR_TEAM, |l| l.0),
            alliances.as_deref(),
            control.as_deref(),
        ),
        None => Vec::new(),
    };
    if last.as_ref() == Some(&rows) && (rows.is_empty() || !list.is_empty()) {
        return;
    }
    for e in &list {
        commands.entity(e).despawn();
    }
    *last = Some(rows.clone());
    if rows.is_empty() {
        return;
    }
    let root = commands
        .spawn((
            PlayerList,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(40.0),
                right: Val::Px(12.0),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(6.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
            Pickable::IGNORE,
            DespawnOnExit(AppState::InGame),
        ))
        .id();
    for row in rows {
        commands.spawn((
            Text::new(row.text),
            TextFont::from_font_size(15.0),
            TextColor(row.color),
            TextLayout::new(Justify::Left, LineBreak::NoWrap),
            Pickable::IGNORE,
            ChildOf(root),
        ));
    }
}

/// The banner's text: the most important thing first.
fn banner_text(status: &NetStatus, names: &PeerNames) -> Option<String> {
    if !status.networked {
        return None;
    }
    if let Some(d) = &status.desync {
        return Some(format!(
            "Desync at frame {} — the game is stopped.\nState dump: {}",
            d.frame,
            status.dump.as_deref().unwrap_or("pending")
        ));
    }
    let over = match status.ended {
        Some(Ended::HostLeft) => Some("The host left — the match is over."),
        Some(Ended::HostLost) => Some("Lost the connection to the host — the match is over."),
        Some(Ended::Dropped) => Some("The host dropped you from the match (no contact)."),
        Some(Ended::Left) | None => None,
    };
    if let Some(over) = over {
        return Some(format!("{over}\nEsc: Leave match"));
    }
    if status.leaving {
        return Some("Leaving the match…".to_string());
    }
    let mut lines = Vec::new();
    if let Some((tick, newest)) = status.catching_up {
        lines.push(format!(
            "Catching up with the match: {} / {} s",
            tick / 30,
            newest.max(tick) / 30
        ));
    }
    if let Some((who, since)) = &status.waiting {
        // Brief stalls are just jitter.
        if status.now.saturating_sub(*since) > Duration::from_millis(300) {
            let who: Vec<String> = who.iter().map(|p| names.name(*p)).collect();
            lines.push(format!("Waiting for {}…", who.join(", ")));
        }
    }
    for n in &status.notices {
        lines.push(match n {
            Notice::Left(p) => format!("{} left the match.", names.name(*p)),
            Notice::TimedOut(p) => format!("{} timed out and was dropped.", names.name(*p)),
            Notice::Returned(p) => format!("{} is back.", names.name(*p)),
            Notice::Joined(p) => format!("{} joined the match.", names.name(*p)),
        });
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn show_banner(
    mut commands: Commands,
    status: Option<Res<NetStatus>>,
    names: Option<Res<PeerNames>>,
    mut banner: Query<(Entity, &mut Text), With<NetBanner>>,
) {
    let names = names.map(|n| n.clone()).unwrap_or_default();
    let message = status.and_then(|s| banner_text(&s, &names));
    match (message, banner.single_mut()) {
        (Some(m), Ok((_, mut text))) => {
            if text.0 != m {
                text.0 = m;
            }
        }
        (Some(m), Err(_)) => {
            commands.spawn((
                NetBanner,
                Text::new(m),
                TextFont::from_font_size(22.0),
                TextColor(Color::srgb(1.0, 0.85, 0.3)),
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(72.0),
                    left: Val::Px(16.0),
                    ..default()
                },
                GlobalZIndex(50),
                Pickable::IGNORE,
                DespawnOnExit(AppState::InGame),
            ));
        }
        (None, Ok((e, _))) => commands.entity(e).despawn(),
        (None, Err(_)) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::session::PeerInfo;
    use crate::net::transport::PeerId;

    fn peer(n: u128, player: Option<u8>, ping: Option<u32>) -> PeerInfo {
        PeerInfo {
            peer: PeerId(n),
            player,
            host: n == 1,
            me: n == 2,
            ping_ms: ping,
            gone: false,
        }
    }

    fn status() -> NetStatus {
        NetStatus {
            networked: true,
            me: PeerId(2),
            delay: 4,
            peers: vec![
                peer(1, Some(0), Some(0)),
                peer(2, Some(1), Some(84)),
                peer(3, Some(2), Some(950)),
                peer(4, None, None),
            ],
            ..Default::default()
        }
    }

    /// Seat, side, name, ping; the host and spectators marked; the
    /// player who left greyed out, a resigned one noted.
    #[test]
    fn player_list_rows() {
        let mut st = status();
        st.peers[2].gone = true;
        let mut names = PeerNames::default();
        names.0.insert(PeerId(1), "ada".into());
        let al = Alliances::from_seats([(0, 0), (1, 0), (2, 1)]);
        let mut control = MatchControl::default();
        control.resigned.insert(2);
        control.dropped.insert(2);
        let rows = player_rows(&st, &names, 1, Some(&al), Some(&control));
        let text: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(text[0], "Players · delay 4 (133 ms)");
        assert_eq!(text[1], " 0 A  ada (host)       0 ms");
        assert_eq!(text[2], " 1 A  peer-0002        84 ms");
        assert_eq!(text[3], " 2 E  peer-0003        left  dropped");
        assert_eq!(text[4], " - S  peer-0004        …");
        assert_eq!(rows[3].color, GONE);
        assert_eq!(rows[2].color, Color::WHITE, "this peer");
    }

    #[test]
    fn ping_turns_red_past_a_second() {
        let g = |ms| ping_color(ms).to_srgba();
        assert!(g(50).green > 0.99 && g(50).red < 0.02);
        assert!(g(1000).red > 0.99 && g(1000).green < 0.02);
    }

    /// The match ending beats waiting; notices name who left.
    #[test]
    fn banner_says_what_matters_most() {
        let names = PeerNames::default();
        let mut st = status();
        assert_eq!(banner_text(&st, &names), None);
        st.waiting = Some((vec![PeerId(1)], Duration::ZERO));
        st.now = Duration::from_secs(1);
        st.notices = vec![Notice::Left(PeerId(3))];
        assert_eq!(
            banner_text(&st, &names).as_deref(),
            Some("Waiting for peer-0001…\npeer-0003 left the match.")
        );
        st.ended = Some(Ended::HostLeft);
        assert_eq!(
            banner_text(&st, &names).as_deref(),
            Some("The host left — the match is over.\nEsc: Leave match")
        );
        st.networked = false;
        assert_eq!(banner_text(&st, &names), None);
    }
}
