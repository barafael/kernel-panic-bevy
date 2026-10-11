//! Background music, the way `LuaUI/Widgets/music.lua` did it.
//!
//! The upstream widget plays whatever `.ogg` files sit in `music/` (and
//! `music/<mod>/`), one random track after another with a two-second
//! gap, and removes itself with "No music found! Copy some .ogg into
//! /music/" when there are none. It runs only during a match (a LuaUI
//! widget) and `StopSoundStream`s on shutdown. The KP 4.4 installer
//! filled that folder with 72 tracks; `scripts/bake-music.sh` brings
//! them into `kernel-panic/assets/music/` (Git LFS), renamed to
//! `<artist-slug>--<title-slug>.ogg`, with `catalog.json` and
//! `credits.txt` for the web player (`kernel-panic/web/music.html`).
//!
//! Native builds scan the folder at startup; the web build cannot list a
//! directory, so `build.rs` bakes the list into [`WEB_MUSIC_CATALOG`].
//! Bevy's audio does not stream: a track is fetched whole, then decoded.
//! So the next track is picked and its load started as soon as the gap
//! opens (and, once something plays, right away for the one after), so
//! on the web the download happens while the current track plays.
//! Volume is the master × music knob (`snd_volmusic`), applied live.

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

/// The track queued to play next: its catalog index and the handle whose
/// load is already under way.
#[derive(Default)]
struct NextTrack {
    queued: Option<(usize, Handle<AudioSource>)>,
    /// Index of the track playing (or last played), never picked twice
    /// in a row when there is a choice.
    previous: Option<usize>,
    /// Seconds of silence since the last track ended.
    gap: f32,
}

fn scan_music(mut commands: Commands) {
    let mut tracks = Vec::new();
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dir = crate::paths::assets_dir().join("music");
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ogg")))
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
/// volume on the knobs, and start the queued track once the gap passed.
#[allow(clippy::too_many_arguments)]
fn play_music(
    time: Res<Time>,
    catalog: Res<MusicCatalog>,
    settings: Res<RenderSettings>,
    asset_server: Res<AssetServer>,
    mut playing: Query<Option<&mut AudioSink>, With<MusicTrack>>,
    mut next: Local<NextTrack>,
    mut rng: Local<u32>,
    mut commands: Commands,
) {
    if catalog.tracks.is_empty() {
        return;
    }
    if *rng == 0 {
        // `music.lua` seeds from the mouse, the clock and the draw frame.
        *rng = (crate::rng::clock_f64().fract() * u32::MAX as f64) as u32 | 1;
    }
    let volume = settings.audio.linear(VolumeKnob::Music);
    // Something to play after the current one, loading already.
    if next.queued.is_none() {
        let index = pick_track(&mut rng, catalog.tracks.len(), next.previous);
        let handle = asset_server.load(catalog.tracks[index].clone());
        next.queued = Some((index, handle));
    }
    if let Ok(sink) = playing.single_mut() {
        if let Some(mut sink) = sink
            && (sink.volume().to_linear() - volume).abs() > 1e-3
        {
            sink.set_volume(Volume::Linear(volume));
        }
        next.gap = 0.0;
        return;
    }
    next.gap += time.delta_secs();
    if next.gap < TRACK_GAP_SECS {
        return;
    }
    next.gap = 0.0;
    let Some((index, handle)) = next.queued.take() else {
        return;
    };
    next.previous = Some(index);
    let path = &catalog.tracks[index];
    info!("Music: {}", path.rsplit('/').next().unwrap_or(path));
    // `AudioPlayer` starts once the asset has loaded, so a still-loading
    // handle only delays the start, never skips the track.
    commands.spawn((
        AudioPlayer::new(handle),
        PlaybackSettings::DESPAWN.with_volume(Volume::Linear(volume)),
        MusicTrack,
    ));
}

/// `widget:Shutdown` → `Spring.StopSoundStream()`.
fn stop_music(playing: Query<Entity, With<MusicTrack>>, mut commands: Commands) {
    for entity in &playing {
        commands.entity(entity).despawn();
    }
}

pub struct MusicPlugin;

impl Plugin for MusicPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MusicCatalog>()
            .add_systems(Startup, scan_music)
            .add_systems(Update, play_music.run_if(in_state(AppState::InGame)))
            .add_systems(OnExit(AppState::InGame), stop_music);
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

    /// The web player's Sound-effects tab lists exactly the bank
    /// (`SOUND_FILES`); both ways, so a typo on either side fails.
    #[test]
    fn web_player_lists_every_sound_effect() {
        let html =
            std::fs::read_to_string(manifest().join("web/music.html")).expect("web/music.html");
        for name in crate::audio::SOUND_FILES {
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
                "music.html lists {name:?}, which is not in SOUND_FILES"
            );
        }
    }

    /// `catalog.json` (from `scripts/bake-music.sh`) describes exactly
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
