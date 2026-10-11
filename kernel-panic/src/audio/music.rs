//! Background music, the way `LuaUI/Widgets/music.lua` did it.
//!
//! The original ships no tracks: the widget plays whatever `.ogg` files
//! sit in `music/` (and `music/<mod>/`), one random track after another
//! with a two-second gap, and removes itself with "No music found! Copy
//! some .ogg into /music/" when there are none. It runs only during a
//! match (a LuaUI widget) and `StopSoundStream`s on shutdown.
//!
//! Here the folder is `kernel-panic/assets/music/`. Native builds scan it
//! at startup; the web build cannot list a directory, so `build.rs` bakes
//! the list into [`WEB_MUSIC_CATALOG`]. Volume is the master × music
//! knob (`snd_volmusic`), applied live to the playing track.

use bevy::audio::{AudioPlayer, AudioSink, AudioSinkPlayback, PlaybackSettings, Volume};
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

fn scan_music(mut commands: Commands) {
    let mut tracks = Vec::new();
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dir = crate::paths::from_project_root("kernel-panic/assets/music");
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

/// Pick the next track once the previous one has ended and the gap has
/// passed; keep the playing track's volume on the knobs.
fn play_music(
    time: Res<Time>,
    catalog: Res<MusicCatalog>,
    settings: Res<RenderSettings>,
    asset_server: Res<AssetServer>,
    mut playing: Query<Option<&mut AudioSink>, With<MusicTrack>>,
    mut gap: Local<f32>,
    mut rng: Local<u32>,
    mut commands: Commands,
) {
    let volume = settings.audio.linear(VolumeKnob::Music);
    if let Ok(sink) = playing.single_mut() {
        if let Some(mut sink) = sink
            && (sink.volume().to_linear() - volume).abs() > 1e-3
        {
            sink.set_volume(Volume::Linear(volume));
        }
        *gap = 0.0;
        return;
    }
    if catalog.tracks.is_empty() {
        return;
    }
    *gap += time.delta_secs();
    if *gap < TRACK_GAP_SECS {
        return;
    }
    *gap = 0.0;
    if *rng == 0 {
        // `music.lua` seeds from the mouse, the clock and the draw frame.
        *rng = (crate::rng::clock_f64().fract() * u32::MAX as f64) as u32 | 1;
    }
    let index = crate::rng::xorshift32(&mut rng) as usize % catalog.tracks.len();
    let path = &catalog.tracks[index];
    info!("Music: {}", path.rsplit('/').next().unwrap_or(path));
    commands.spawn((
        AudioPlayer::new(asset_server.load(path.clone())),
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
