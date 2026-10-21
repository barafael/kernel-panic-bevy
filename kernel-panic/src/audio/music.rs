//! Background music, the way `LuaUI/Widgets/music.lua` did it.
//!
//! The upstream widget plays whatever `.ogg` files sit in `music/` (and
//! `music/<mod>/`), one random track after another with a two-second
//! gap, and removes itself with "No music found! Copy some .ogg into
//! /music/" when there are none. It runs only during a match (a LuaUI
//! widget) and `StopSoundStream`s on shutdown. The KP 4.4 installer
//! filled that folder with 72 tracks; they were copied once into
//! `kernel-panic/assets/music/` (Git LFS, README.md), renamed to
//! `<artist-slug>--<title-slug>.ogg`, with `catalog.json` and
//! `credits.txt` for the web player (`kernel-panic/web/music.html`).
//!
//! Unlike the widget, the port's player also runs on the menu (the demo
//! match plays behind it) and keeps its track across menu ↔ match
//! transitions, like a desktop player; the menu's bottom-left
//! `|<  >/||  >|` cluster sends [`MusicControl`]s. Previous walks a
//! short history; the tracks stepped back over play again on Next.
//! Pause keeps the track entity alive (a paused sink never ends), and a
//! track started while paused spawns paused, so Next while paused stays
//! paused.
//! Native builds scan the folder at startup; the web build cannot list a
//! directory, so `build.rs` bakes the list into [`WEB_MUSIC_CATALOG`].
//! Bevy's audio does not stream: a track is fetched whole, then decoded.
//! So the next track is picked and its load started as soon as the gap
//! opens (and, once something plays, right away for the one after), so
//! on the web the download happens while the current track plays.
//! Volume is the master × music knob (`snd_volmusic`) ×
//! [`UPSTREAM_MUSIC_VOLUME`], applied live.

use bevy::audio::{
    AudioPlayer, AudioSink, AudioSinkPlayback, AudioSource, PlaybackSettings, Volume,
};
use bevy::prelude::*;

use crate::game_setup::AppState;
use crate::rendering::settings::{RenderSettings, VolumeKnob};

include!(concat!(env!("OUT_DIR"), "/web_music_catalog.rs"));

/// Asset paths (`music/<name>.ogg`) of every track found at startup.
#[derive(Resource, Debug, Default)]
pub struct MusicCatalog {
    pub tracks: Vec<String>,
}

/// The one playing track entity.
#[derive(Component)]
struct MusicTrack;

/// `music.lua` waits for the stream to end and then more than two
/// seconds of its timer before starting the next one.
const TRACK_GAP_SECS: f32 = 2.0;

/// `Spring.PlaySoundStream(CurrentTrack, 0.3)` (`music.lua:75`): the
/// widget streams every track at 30 % under the music knob, which is
/// what keeps the battle audible over it.
pub const UPSTREAM_MUSIC_VOLUME: f32 = 0.3;

/// Tracks remembered for Previous.
const HISTORY_LEN: usize = 32;

/// What the menu's `|<  >/||  >|` cluster sends.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub enum MusicControl {
    Previous,
    TogglePause,
    Next,
}

/// The playlist: what plays, what played, what is loading for after.
#[derive(Resource, Debug, Default)]
pub struct MusicPlayer {
    /// Catalog index of the track playing (or last started).
    pub current: Option<usize>,
    /// Playback is paused (the sink is, or the next spawn will be).
    pub paused: bool,
    /// Tracks before `current`, oldest first, at most [`HISTORY_LEN`].
    history: Vec<usize>,
    /// Tracks stepped back over with Previous, nearest last; Next (and
    /// the natural end of a track) replays them before picking afresh.
    forward: Vec<usize>,
    /// The track queued to play next: its catalog index and the handle
    /// whose load is already under way.
    queued: Option<(usize, Handle<AudioSource>)>,
    /// Seconds of silence since the last track ended.
    gap: f32,
    rng: u32,
}

impl MusicPlayer {
    fn seed(&mut self) {
        if self.rng == 0 {
            // `music.lua` seeds from the mouse, the clock and the draw frame.
            self.rng = (crate::rng::clock_f64().fract() * u32::MAX as f64) as u32 | 1;
        }
    }

    /// Record that `index` started after `current`.
    fn advance(&mut self, index: usize) {
        if let Some(current) = self.current.replace(index) {
            self.history.push(current);
            if self.history.len() > HISTORY_LEN {
                self.history.remove(0);
            }
        }
    }

    /// The track to start next, and its handle when it is already
    /// loading: a track stepped back over, else the queued one, else a
    /// fresh pick. Recorded as the new `current`.
    fn take_next(&mut self, count: usize) -> (usize, Option<Handle<AudioSource>>) {
        let (index, handle) = match self.forward.pop().filter(|&i| i < count) {
            Some(index) => (index, None),
            None => match self.queued.take() {
                Some((index, handle)) if index < count => (index, Some(handle)),
                _ => (pick_track(&mut self.rng, count, self.current), None),
            },
        };
        self.advance(index);
        (index, handle)
    }

    /// The track to start on Previous: the one before `current`, which
    /// itself goes on the forward stack; with no history, `current`
    /// again from the top; `None` when nothing has played.
    fn take_previous(&mut self) -> Option<usize> {
        match self.history.pop() {
            Some(index) => {
                if let Some(current) = self.current.replace(index) {
                    self.forward.push(current);
                }
                Some(index)
            }
            None => self.current,
        }
    }

    /// `current` for the menu: `music/mister-beep--the-title.ogg` →
    /// "Mister Beep - The Title"; empty before the first track.
    pub fn track_label(&self, catalog: &MusicCatalog) -> String {
        let Some(path) = self.current.and_then(|i| catalog.tracks.get(i)) else {
            return String::new();
        };
        track_label(path)
    }
}

/// `artist-slug--title-slug.ogg` → "Artist
/// Slug - Title Slug". A name without the `--` separator is shown as is,
/// minus the extension.
fn track_label(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let name = name.strip_suffix(".ogg").unwrap_or(name);
    let words = |slug: &str| {
        slug.split('-')
            .filter(|w| !w.is_empty())
            .map(|w| {
                let mut c = w.chars();
                match c.next() {
                    Some(first) => first.to_uppercase().chain(c).collect::<String>(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    match name.split_once("--") {
        Some((artist, title)) => format!("{} - {}", words(artist), words(title)),
        None => name.to_string(),
    }
}

/// The file starts with the Ogg capture pattern. A track that is still a
/// Git LFS pointer (130 bytes of text) would otherwise reach the
/// decoder, and `bevy_audio` panics on a source it cannot decode.
#[cfg(not(target_arch = "wasm32"))]
fn is_ogg(path: &std::path::Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    std::fs::File::open(path).is_ok_and(|mut f| f.read_exact(&mut magic).is_ok())
        && &magic == b"OggS"
}

fn scan_music(mut commands: Commands) {
    let mut tracks = Vec::new();
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dir = crate::paths::assets_dir().join("music");
        let (oggs, pointers): (Vec<_>, Vec<_>) = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ogg")))
            .partition(|p| is_ogg(p));
        if !pointers.is_empty() {
            warn!(
                "music: skipping {} file(s) in {} that are not Ogg streams (Git LFS pointers? run `git lfs pull`)",
                pointers.len(),
                dir.display()
            );
        }
        let mut names: Vec<String> = oggs
            .into_iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        names.sort();
        tracks.extend(names.into_iter().map(|n| format!("music/{n}")));
    }
    #[cfg(target_arch = "wasm32")]
    {
        tracks.extend(WEB_MUSIC_CATALOG.iter().map(|n| format!("music/{n}.ogg")));
    }
    if tracks.is_empty() {
        info!("No music found! Copy some .ogg into kernel-panic/assets/music/");
    } else {
        info!("music: {} track(s) in assets/music", tracks.len());
    }
    commands.insert_resource(MusicCatalog { tracks });
}

/// Pick a random track index, avoiding `previous` when there is a choice
/// (`music.lua` draws uniformly; a repeat right after itself is the one
/// draw nobody wants).
fn pick_track(rng: &mut u32, count: usize, previous: Option<usize>) -> usize {
    let index = crate::rng::xorshift32(rng) as usize % count;
    match previous {
        Some(p) if p == index && count > 1 => (index + 1) % count,
        _ => index,
    }
}

/// Queue the next track and start its load, keep the playing track's
/// volume on the knobs, apply the menu's controls, and start the next
/// track once the gap passed.
#[allow(clippy::too_many_arguments)]
fn play_music(
    time: Res<Time>,
    catalog: Res<MusicCatalog>,
    settings: Res<RenderSettings>,
    asset_server: Res<AssetServer>,
    mut controls: MessageReader<MusicControl>,
    mut playing: Query<(Entity, Option<&mut AudioSink>), With<MusicTrack>>,
    mut player: ResMut<MusicPlayer>,
    mut commands: Commands,
) {
    let count = catalog.tracks.len();
    if count == 0 {
        controls.clear();
        return;
    }
    // Only the bookkeeping below marks the resource changed: the menu
    // readout compares its text anyway, this just keeps it quiet.
    let player = player.bypass_change_detection();
    player.seed();
    let volume = settings.audio.linear(VolumeKnob::Music) * UPSTREAM_MUSIC_VOLUME;
    // Something to play after the current one, loading already.
    if player.queued.is_none() {
        let index = pick_track(&mut player.rng, count, player.current);
        let handle = asset_server.load(catalog.tracks[index].clone());
        player.queued = Some((index, handle));
    }
    let mut start: Option<(usize, Option<Handle<AudioSource>>)> = None;
    for control in controls.read() {
        match control {
            MusicControl::TogglePause => {
                player.paused = !player.paused;
                if let Ok((_, Some(sink))) = playing.single_mut() {
                    if player.paused {
                        sink.pause();
                    } else {
                        sink.play();
                    }
                }
            }
            MusicControl::Next => start = Some(player.take_next(count)),
            MusicControl::Previous => {
                if let Some(index) = player.take_previous() {
                    start = Some((index, None));
                }
            }
        }
    }
    if let Some((index, handle)) = start {
        // Skip straight to it: no gap, the playing track goes.
        for (entity, _) in &playing {
            commands.entity(entity).despawn();
        }
        player.gap = 0.0;
        start_track(
            player,
            index,
            handle,
            &catalog,
            &asset_server,
            volume,
            &mut commands,
        );
        return;
    }
    if let Ok((_, sink)) = playing.single_mut() {
        if let Some(mut sink) = sink
            && (sink.volume().to_linear() - volume).abs() > 1e-3
        {
            sink.set_volume(Volume::Linear(volume));
        }
        player.gap = 0.0;
        return;
    }
    if player.paused {
        return;
    }
    player.gap += time.delta_secs();
    if player.gap < TRACK_GAP_SECS {
        return;
    }
    player.gap = 0.0;
    let (index, handle) = player.take_next(count);
    start_track(
        player,
        index,
        handle,
        &catalog,
        &asset_server,
        volume,
        &mut commands,
    );
}

/// Spawn catalog track `index` (its load may still be under way:
/// `AudioPlayer` starts once the asset has loaded, so that only delays
/// the start, never skips the track), paused if the player is.
fn start_track(
    player: &MusicPlayer,
    index: usize,
    handle: Option<Handle<AudioSource>>,
    catalog: &MusicCatalog,
    asset_server: &AssetServer,
    volume: f32,
    commands: &mut Commands,
) {
    let path = &catalog.tracks[index];
    let handle = handle.unwrap_or_else(|| asset_server.load(path.clone()));
    info!("Music: {}", path.rsplit('/').next().unwrap_or(path));
    commands.spawn((
        AudioPlayer::new(handle),
        PlaybackSettings {
            paused: player.paused,
            ..PlaybackSettings::DESPAWN.with_volume(Volume::Linear(volume))
        },
        MusicTrack,
    ));
}

pub struct MusicPlugin;

impl Plugin for MusicPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MusicCatalog>()
            .init_resource::<MusicPlayer>()
            .add_message::<MusicControl>()
            .add_systems(Startup, scan_music)
            .add_systems(
                Update,
                play_music.run_if(in_state(AppState::Menu).or_else(in_state(AppState::InGame))),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn manifest() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn pick_track_never_repeats_when_it_has_a_choice() {
        let mut rng = 0xBADC0FFE;
        for _ in 0..200 {
            let i = pick_track(&mut rng, 5, Some(3));
            assert!(i < 5 && i != 3);
        }
        // A single track is the only choice.
        assert_eq!(pick_track(&mut rng, 1, Some(0)), 0);
    }

    fn player_at(indices: &[usize]) -> MusicPlayer {
        let mut player = MusicPlayer {
            rng: 0xBADC0FFE,
            ..default()
        };
        for &i in indices {
            player.advance(i);
        }
        player
    }

    /// Previous walks back through what played; Next then replays the
    /// tracks stepped over before picking anything new.
    #[test]
    fn previous_walks_history_then_next_replays_it() {
        let mut player = player_at(&[2, 4, 7]);
        assert_eq!(player.take_previous(), Some(4));
        assert_eq!(player.take_previous(), Some(2));
        assert_eq!(player.current, Some(2));
        assert_eq!(player.take_next(10).0, 4);
        assert_eq!(player.take_next(10).0, 7);
        assert_eq!(player.current, Some(7));
        // The forward stack is spent: a fresh pick, never the current one.
        let (fresh, handle) = player.take_next(10);
        assert_ne!(fresh, 7);
        assert!(handle.is_none());
        // And going back from there retraces the whole path.
        assert_eq!(player.take_previous(), Some(7));
        assert_eq!(player.take_previous(), Some(4));
    }

    /// With nothing before it, Previous restarts the current track; with
    /// nothing played at all it does nothing.
    #[test]
    fn previous_with_no_history_restarts_or_does_nothing() {
        let mut player = player_at(&[]);
        assert_eq!(player.take_previous(), None);
        let mut player = player_at(&[3]);
        assert_eq!(player.take_previous(), Some(3));
        assert_eq!(player.current, Some(3));
        assert!(player.forward.is_empty());
    }

    /// Next prefers the queued (already loading) track over a fresh pick.
    #[test]
    fn next_takes_the_queued_track_first() {
        let mut player = player_at(&[1]);
        player.queued = Some((5, Handle::default()));
        let (index, handle) = player.take_next(10);
        assert_eq!(index, 5);
        assert!(handle.is_some());
        assert!(player.queued.is_none());
        assert_eq!(player.history, vec![1]);
    }

    #[test]
    fn history_is_capped() {
        let mut player = player_at(&[]);
        for i in 0..(HISTORY_LEN + 10) {
            player.advance(i);
        }
        assert_eq!(player.history.len(), HISTORY_LEN);
        assert_eq!(player.history[0], 9);
    }

    #[test]
    fn track_label_reads_artist_and_title_from_the_slug() {
        assert_eq!(
            track_label("music/mister-beep--the-c64-song.ogg"),
            "Mister Beep - The C64 Song"
        );
        assert_eq!(track_label("music/untagged.ogg"), "untagged");
        let catalog = MusicCatalog {
            tracks: vec!["music/lo-bat--hello.ogg".into()],
        };
        assert_eq!(player_at(&[]).track_label(&catalog), "");
        assert_eq!(player_at(&[0]).track_label(&catalog), "Lo Bat - Hello");
    }

    /// The web player's Sound-effects tab lists exactly the bank
    /// (`ALL_SOUND_IDS`); both ways, so a typo on either side fails.
    #[test]
    fn web_player_lists_every_sound_effect() {
        let html =
            std::fs::read_to_string(manifest().join("web/music.html")).expect("web/music.html");
        for sound in crate::audio::ALL_SOUND_IDS {
            let name = sound.name();
            assert!(
                html.contains(&format!("['{name}', ")),
                "music.html does not list the effect {name:?}"
            );
        }
        for line in html.lines() {
            let Some(rest) = line.trim_start().strip_prefix("['") else {
                continue;
            };
            let Some(name) = rest.split('\'').next() else {
                continue;
            };
            assert!(
                crate::audio::SoundId::from_name(name).is_some(),
                "music.html lists {name:?}, which is not a `SoundId`"
            );
        }
    }

    /// `catalog.json` describes exactly
    /// the `.ogg` files in the folder, and they are real tracks, not LFS
    /// pointers. Skips when there is no catalog (an empty folder is a
    /// valid build).
    #[test]
    fn music_catalog_matches_the_folder() {
        let dir = manifest().join("assets/music");
        let Ok(text) = std::fs::read_to_string(dir.join("catalog.json")) else {
            return;
        };
        let catalog: serde_json::Value = serde_json::from_str(&text).expect("catalog.json parses");
        let tracks = catalog["tracks"].as_array().expect("tracks array");
        let mut listed = std::collections::BTreeSet::new();
        for t in tracks {
            let file = t["file"].as_str().expect("file");
            assert!(
                file.ends_with(".ogg")
                    && file
                        .trim_end_matches(".ogg")
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{file}: not a slug"
            );
            assert!(
                t["artist"].is_string() && t["title"].is_string(),
                "{file}: names"
            );
            let meta = std::fs::metadata(dir.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
            assert!(
                meta.len() > 1024,
                "{file} is {} bytes: an LFS pointer — run `git lfs pull`",
                meta.len()
            );
            assert_eq!(
                meta.len(),
                t["bytes"].as_u64().expect("bytes"),
                "{file}: size"
            );
            assert!(
                t["seconds"].as_u64().is_some_and(|s| s > 0),
                "{file}: seconds"
            );
            listed.insert(file.to_string());
        }
        let credits = catalog["credits"].as_str().unwrap_or("credits.txt");
        assert!(
            dir.join(credits).is_file(),
            "credits file {credits} missing"
        );
        let on_disk: std::collections::BTreeSet<String> = std::fs::read_dir(&dir)
            .expect("assets/music")
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".ogg"))
            .collect();
        assert_eq!(on_disk, listed, "folder and catalog.json disagree");
    }
}
