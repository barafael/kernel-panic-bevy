//! Hidden browser tabs (the web build; `net-experiments/m3-rooms.md`
//! §1 has the measurements).
//!
//! A hidden tab gets no animation frames at all, and its timers fire at
//! most once a second (Chromium 154, Firefox 157). The game's frames are
//! animation frames (winit's redraws), so a hidden tab used to stop
//! dead: no ticks, no packets; the host dropped it after a minute of
//! silence (or, the host's own tab hidden, the match stopped for all).
//!
//! What keeps running while hidden, measured: a dedicated worker's
//! `setInterval` (30/s in both engines, also past Chromium's 5-minute
//! "intensive" throttling) and the messages it posts to the page. So
//! while the tab is hidden — and the game is in a room or a networked
//! match; single player still stops in a hidden tab, as it always did
//! — a worker posts every [`HIDDEN_FRAME_MS`] and
//! each message wakes the event loop ([`EventLoopProxyWrapper`]); our
//! `bevy_winit` patch (`vendor/bevy_winit`) lets a hidden page's wake
//! run the app update — Bevy only updates on a redraw, which a hidden
//! page never gets. The lockstep driver then runs its ticks as usual
//! (by the clock), sends its packets, and the match goes on.
//!
//! The other players are told ([`TabHidden`] → `Session::set_away`):
//! their player list says "tab hidden". Should the browser stop the
//! worker too (a frozen or discarded tab), the existing rules apply:
//! a silent guest is dropped after the timeout, one far behind that
//! stopped catching up is dropped too (`session::stalled`).

use bevy::prelude::*;

pub use super::driver::TabHidden;

/// How often a hidden tab's worker wakes the game (a tick's length).
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub const HIDDEN_FRAME_MS: u32 = 33;

/// Hidden-tab handling: [`TabHidden`] everywhere (always `false`
/// natively), the waking worker on the web.
pub struct WebTabPlugin;

impl Plugin for WebTabPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TabHidden>();
        #[cfg(target_arch = "wasm32")]
        app.add_systems(Startup, web::install)
            .add_systems(First, web::track);
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use std::cell::{Cell, RefCell};

    use bevy::platform::time::Instant;
    use bevy::prelude::*;
    use bevy::winit::{EventLoopProxyWrapper, WinitUserEvent};
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::*;

    use super::{HIDDEN_FRAME_MS, TabHidden};
    use crate::sim::DetFloat;

    /// The worker: given a period in ms, posts a message that often;
    /// given 0, stops.
    const WORKER: &str = "let t = null;\n\
        onmessage = (e) => {\n\
          if (e.data > 0) { if (t === null) t = setInterval(() => postMessage(0), e.data); }\n\
          else if (t !== null) { clearInterval(t); t = null; }\n\
        };";

    /// The page side, kept alive for the app's lifetime (wasm is
    /// single-threaded: thread-locals are the page's).
    struct Hooks {
        worker: web_sys::Worker,
        _on_tick: Closure<dyn FnMut(web_sys::MessageEvent)>,
        _on_visibility: Closure<dyn FnMut(web_sys::Event)>,
    }

    thread_local! {
        static HOOKS: RefCell<Option<Hooks>> = const { RefCell::new(None) };
        /// The game is in a room or a networked match: worth waking
        /// while hidden.
        static WANTED: Cell<bool> = const { Cell::new(false) };
    }

    /// What the worker should do now: wake every [`HIDDEN_FRAME_MS`]
    /// (hidden and wanted) or rest (0).
    fn period() -> u32 {
        if hidden() && WANTED.get() {
            HIDDEN_FRAME_MS
        } else {
            0
        }
    }

    fn tell_worker() {
        HOOKS.with(|h| {
            if let Some(h) = h.borrow().as_ref() {
                let _ = h.worker.post_message(&JsValue::from(period()));
            }
        });
    }

    fn document() -> Option<web_sys::Document> {
        web_sys::window()?.document()
    }

    fn hidden() -> bool {
        document().is_some_and(|d| d.hidden())
    }

    /// Startup: the worker, its wake-ups, and the `visibilitychange`
    /// listener that starts and stops it (an event: it reaches a hidden
    /// page at once, unlike a frame).
    pub(super) fn install(proxy: Option<Res<EventLoopProxyWrapper>>) {
        let Some(proxy) = proxy else {
            return;
        };
        let proxy = (**proxy).clone();
        let worker = (|| {
            let parts = js_sys::Array::of1(&JsValue::from_str(WORKER));
            let options = web_sys::BlobPropertyBag::new();
            options.set_type("text/javascript");
            let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &options).ok()?;
            let url = web_sys::Url::create_object_url_with_blob(&blob).ok()?;
            web_sys::Worker::new(&url).ok()
        })();
        let (Some(worker), Some(document)) = (worker, document()) else {
            warn!("no worker: a hidden tab will stop the game");
            return;
        };
        let on_tick = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |_| {
            let _ = proxy.send_event(WinitUserEvent::WakeUp);
        });
        worker.set_onmessage(Some(on_tick.as_ref().unchecked_ref()));
        let w = worker.clone();
        let on_visibility = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            let _ = w.post_message(&JsValue::from(period()));
        });
        let _ = document.add_event_listener_with_callback(
            "visibilitychange",
            on_visibility.as_ref().unchecked_ref(),
        );
        HOOKS.with(|h| {
            *h.borrow_mut() = Some(Hooks {
                worker,
                _on_tick: on_tick,
                _on_visibility: on_visibility,
            });
        });
    }

    /// What a hidden spell did, for the log.
    #[derive(Default)]
    pub(super) struct Spell {
        since: Option<Instant>,
        frames: u32,
        ticks: u32,
        last_report: Option<Instant>,
    }

    /// Every frame: whether the worker is wanted (in a room or a
    /// networked match), [`TabHidden`] from the document, and a log line
    /// every 10 s while hidden (frames and sim ticks), then a summary.
    pub(super) fn track(
        mut tab: ResMut<TabHidden>,
        net: Option<Res<crate::net::driver::NetSession>>,
        lobby: Option<Res<crate::net::lobby::LobbyRoom>>,
        mut spell: Local<Spell>,
        mut last_applied: Local<u32>,
    ) {
        let wanted = lobby.is_some_and(|l| l.lobby.is_some())
            || net.as_ref().is_some_and(|n| n.session.connected());
        if wanted != WANTED.get() {
            WANTED.set(wanted);
            tell_worker();
        }
        let now_hidden = hidden();
        let applied = net.map_or(0, |n| n.session.applied());
        let ticks = applied.saturating_sub(*last_applied);
        *last_applied = applied;
        if now_hidden != tab.0 {
            tab.0 = now_hidden;
            if now_hidden {
                info!("tab hidden: a worker wakes the game every {HIDDEN_FRAME_MS} ms");
                *spell = Spell {
                    since: Some(Instant::now()),
                    ..default()
                };
            } else if let Some(since) = spell.since {
                let secs = since.elapsed().as_secs_f32();
                info!(
                    "tab shown again after {secs:.1} s hidden: {} frames, {} ticks ({:.1} ticks/s)",
                    spell.frames,
                    spell.ticks,
                    spell.ticks as f32 / secs.det_max(0.001)
                );
            }
            tell_worker();
            return;
        }
        if !now_hidden {
            return;
        }
        spell.frames += 1;
        spell.ticks += ticks;
        let report = spell
            .last_report
            .or(spell.since)
            .is_some_and(|t| t.elapsed().as_secs() >= 10);
        if report {
            spell.last_report = Some(Instant::now());
            let secs = spell.since.map_or(0.0, |t| t.elapsed().as_secs_f32());
            info!(
                "tab hidden {secs:.0} s: {} frames, {} ticks ({:.1} ticks/s)",
                spell.frames,
                spell.ticks,
                spell.ticks as f32 / secs.det_max(0.001)
            );
        }
    }
}
