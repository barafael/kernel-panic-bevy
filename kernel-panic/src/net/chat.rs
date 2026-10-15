//! Text chat in the room and in the match (`docs/multiplayer-plan.md`
//! M3, `net-experiments/m3-rooms.md`).
//!
//! Spring's chat is not part of the sync: a `NETMSG_CHAT` goes to the
//! server, which drops it if it claims another sender or is empty and
//! broadcasts it to everyone (`CGameServer::ProcessPacket` and
//! `GotChatMessage`, `rts/Net/GameServer.cpp:1147-1162, 3057-3063`);
//! the clients print it (`CGame::HandleChatMsg`, `Game/Game.cpp:1923`).
//! The same here, on the reliable channel: a peer sends its line to the
//! host, the host checks the sender and relays it to everyone, the
//! sender included, so every peer sees the lines in the host's order.
//! In the room the lobby carries it ([`super::lobby::LobbyMsg::Chat`]),
//! in a match the session ([`super::wire::Msg::Chat`]); neither touches
//! the sim. Both feed one [`ChatLog`], so the room's talk is still on
//! screen when the match starts.

use std::collections::VecDeque;
use std::time::Duration;

use bevy::prelude::*;

use super::transport::PeerId;

/// Characters of one chat line, at most (Spring cuts a message at 127
/// bytes, `MAX_MSG_SIZE`, `Game/ChatMessage.cpp:16`).
pub const CHAT_MAX: usize = 127;

/// Lines the log keeps.
const LOG_KEEP: usize = 100;

/// `text` as it may travel: no control characters, at most
/// [`CHAT_MAX`] characters, trimmed. Empty: nothing to send.
pub fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(CHAT_MAX)
        .collect::<String>()
        .trim()
        .to_string()
}

/// One line said by someone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatLine {
    pub from: PeerId,
    /// The sender's name when the line arrived.
    pub name: String,
    pub text: String,
}

/// Everything said in the room and the match this game took part in,
/// oldest first, with when it arrived (`Time<Real>`).
#[derive(Resource, Default, Debug)]
pub struct ChatLog {
    pub lines: VecDeque<(Duration, ChatLine)>,
    /// Counts every line ever pushed (views redraw when it moves).
    pub count: u64,
}

impl ChatLog {
    pub fn push(&mut self, at: Duration, line: ChatLine) {
        self.lines.push_back((at, line));
        while self.lines.len() > LOG_KEEP {
            self.lines.pop_front();
        }
        self.count += 1;
    }

    /// The newest `n` lines, oldest first.
    pub fn last(&self, n: usize) -> impl Iterator<Item = &(Duration, ChatLine)> {
        self.lines.iter().skip(self.lines.len().saturating_sub(n))
    }
}

/// The in-match chat line being typed (Enter opens it, Enter sends,
/// Esc drops it — Spring's chat input). While it is open the game's
/// hotkeys see no keys ([`ChatInput::swallow_keys`]).
#[derive(Resource, Default, Debug)]
pub struct ChatInput {
    pub open: bool,
    pub text: String,
}

impl ChatInput {
    /// `PreUpdate`, after Bevy's input: with the chat line open, no key
    /// reaches the game's hotkeys (the line reads `KeyboardInput`).
    pub fn swallow_keys(input: Res<ChatInput>, mut keys: ResMut<ButtonInput<KeyCode>>) {
        if input.open {
            keys.reset_all();
        }
    }
}

/// Type into `text` from one `KeyboardInput`: characters, Backspace.
/// `Some(key)` for the keys a field acts on itself (Enter, Escape).
pub fn type_into(
    text: &mut String,
    key: &bevy::input::keyboard::KeyboardInput,
    max: usize,
    accept: impl Fn(char) -> bool,
) -> Option<bevy::input::keyboard::Key> {
    use bevy::input::keyboard::Key;
    if !key.state.is_pressed() {
        return None;
    }
    match &key.logical_key {
        Key::Character(s) => {
            for c in s.chars() {
                if accept(c) && !c.is_control() && text.chars().count() < max {
                    text.push(c);
                }
            }
            None
        }
        Key::Space => {
            if accept(' ') && text.chars().count() < max {
                text.push(' ');
            }
            None
        }
        Key::Backspace => {
            text.pop();
            None
        }
        k @ (Key::Enter | Key::Escape) => Some(k.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_cleaned_and_the_log_bounded() {
        assert_eq!(clean("  gg\u{7}\n wp "), "gg wp");
        assert_eq!(clean(&"x".repeat(500)).len(), CHAT_MAX);
        assert_eq!(clean(" \t "), "");
        let mut log = ChatLog::default();
        for i in 0..150 {
            log.push(
                Duration::from_secs(i),
                ChatLine {
                    from: PeerId(1),
                    name: "a".into(),
                    text: i.to_string(),
                },
            );
        }
        assert_eq!(log.lines.len(), LOG_KEEP);
        assert_eq!(log.count, 150);
        let last: Vec<&str> = log.last(2).map(|(_, l)| l.text.as_str()).collect();
        assert_eq!(last, ["148", "149"]);
    }
}
