//! A matchbox signaling server on localhost, for as long as a [`Signaling`]
//! lives: the WebRTC tests' (`kernel-panic/src/net/matchbox_tests.rs`) and
//! `cargo testbox`'s (`xtask`). Rooms by URL path, as on the deployed
//! `matchbox_server`, so a game reaches `<url>/kp-<room>` here as it would
//! there.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::extract::ws::Message;
use futures_lite::StreamExt;
use matchbox_protocol::{JsonPeerEvent, PeerId, PeerRequest};
use matchbox_signaling::common_logic::{SignalingChannel, parse_request, try_send};
use matchbox_signaling::{
    ClientRequestError, NoCallbacks, SignalingServerBuilder, SignalingState, SignalingTopology,
    WsStateMeta,
};

/// Rooms by URL path, each a full mesh: who joins is introduced to
/// everyone already there. `matchbox_server`'s semantics (the deployed
/// server); `matchbox_signaling`'s own full-mesh topology reads the room
/// and adds the newcomer under separate locks, so two peers joining at
/// the same moment may never meet (`x-transport.md` §2). Here one lock
/// covers both.
#[derive(Clone, Debug, Default)]
struct Rooms(Arc<Mutex<BTreeMap<String, BTreeMap<PeerId, SignalingChannel>>>>);

impl SignalingState for Rooms {}

struct RoomMesh;

fn text(event: JsonPeerEvent) -> Message {
    Message::Text(event.to_string().into())
}

#[async_trait::async_trait]
impl SignalingTopology<NoCallbacks, Rooms> for RoomMesh {
    async fn state_machine(upgrade: WsStateMeta<NoCallbacks, Rooms>) {
        let WsStateMeta {
            room,
            peer_id,
            sender,
            mut receiver,
            state,
            ..
        } = upgrade;
        {
            let mut rooms = state.0.lock().unwrap();
            let peers = rooms.entry(room.clone()).or_default();
            for ch in peers.values() {
                let _ = try_send(ch, text(JsonPeerEvent::NewPeer(peer_id)));
            }
            peers.insert(peer_id, sender);
        }
        while let Some(request) = receiver.next().await {
            match parse_request(request) {
                Ok(PeerRequest::Signal { receiver, data }) => {
                    let to = state
                        .0
                        .lock()
                        .unwrap()
                        .get(&room)
                        .and_then(|r| r.get(&receiver).cloned());
                    if let Some(ch) = to {
                        let event = JsonPeerEvent::Signal {
                            sender: peer_id,
                            data,
                        };
                        let _ = try_send(&ch, text(event));
                    }
                }
                Ok(PeerRequest::KeepAlive) => {}
                Err(ClientRequestError::Json(_) | ClientRequestError::UnsupportedType(_)) => {}
                Err(_) => break,
            }
        }
        let mut rooms = state.0.lock().unwrap();
        if let Some(peers) = rooms.get_mut(&room) {
            peers.remove(&peer_id);
            for ch in peers.values() {
                let _ = try_send(ch, text(JsonPeerEvent::PeerLeft(peer_id)));
            }
            if peers.is_empty() {
                rooms.remove(&room);
            }
        }
    }
}

/// A signaling server, for as long as it lives.
pub struct Signaling {
    rt: Option<tokio::runtime::Runtime>,
    rooms: Rooms,
    /// `ws://<addr>`.
    pub url: String,
}

impl Signaling {
    /// On a free localhost port.
    pub fn start() -> Self {
        Self::start_at((Ipv4Addr::LOCALHOST, 0).into()).expect("bind the signaling server")
    }

    /// On `addr` (port 0: a free one).
    pub fn start_at(addr: SocketAddr) -> std::io::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let rooms = Rooms::default();
        let mut server = SignalingServerBuilder::new(addr, RoomMesh, rooms.clone()).build();
        let addr = server.bind().map_err(std::io::Error::other)?;
        rt.spawn(server.serve());
        Ok(Self {
            rt: Some(rt),
            rooms,
            url: format!("ws://{addr}"),
        })
    }

    /// Tell everyone else that `peer` left, without it leaving: what the
    /// others hear when its signaling websocket drops (a server restart
    /// does not, it just goes quiet).
    pub fn announce_left(&self, peer: uuid::Uuid) {
        let id = PeerId(peer);
        for peers in self.rooms.0.lock().unwrap().values() {
            for (_, ch) in peers.iter().filter(|(p, _)| **p != id) {
                let _ = try_send(ch, text(JsonPeerEvent::PeerLeft(id)));
            }
        }
    }

    /// Shut the server down (its connections drop).
    pub fn stop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }

    /// How many peers are in a room (signaling, not WebRTC: connected to
    /// the server, whether or not anyone has a data channel to them).
    pub fn peers_in_rooms(&self) -> usize {
        self.rooms.0.lock().unwrap().values().map(|r| r.len()).sum()
    }

    /// The rooms by path, with how many peers each has.
    pub fn rooms(&self) -> Vec<(String, usize)> {
        let rooms = self.rooms.0.lock().unwrap();
        rooms.iter().map(|(r, p)| (r.clone(), p.len())).collect()
    }
}
