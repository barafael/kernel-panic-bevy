//! The original Kernel Panic sound effects, played the way the Spring
//! engine played them.
//!
//! Upstream names *which* file each event uses (`soundstart` /
//! `soundhit` in `weapons/*.tdf`, `gamedata/SOUND.TDF` for the unit
//! replies, `gamedata/sounds.lua` for the GUI beeps) and the engine
//! decides *when* and *how loud* (see `docs/audio-plan.md` for every
//! trigger with its engine source line). This module is the engine
//! half:
//!
//! - [`SoundBank`] holds a handle per original effect under
//!   `assets/sounds/` (Vorbis, converted once from the original
//!   `.wav`s), keyed by the lower-cased TDF name.
//! - The sim pushes [`SoundRequest`]s onto the [`SoundQueue`] resource
//!   and never touches `bevy_audio`, so headless tests and the bot run
//!   silent without registering anything (systems take the queue as
//!   `Option<ResMut<_>>`).
//! - [`play_queued_sounds`] drains the queue once per frame, attenuates
//!   each request with the engine's OpenAL model from the camera (the
//!   listener, `Game.cpp:1371`) and spawns one despawn-on-finish
//!   [`AudioPlayer`] per sound.
//!
//! Stereo panning is not reproduced: rodio's spatial sink brings its
//! own distance model, which would fight the engine's. Everything is
//! mixed as mono at the engine's gain.

use bevy::audio::{AddAudioSource, AudioPlayer, PlaybackSettings, Volume};
use bevy::prelude::*;

pub mod music;

use crate::interaction::selection::Selected;
use crate::rendering::camera::RtsCamera;
use crate::rendering::settings::{RenderSettings, VolumeKnob};
use crate::sim::DetFloat;
use crate::terrain::heightmap::Heightmap;
use crate::units::components::UnitType;
use crate::units::content::definitions::UnitKind;

/// Every effect the original ships (`upstream/Kernel-Panic/sounds/*.wav`),
/// lower-cased: Spring's VFS is case-insensitive (`soundhit=Explosion5`,
/// `SIGTERMhit`) but Bevy asset paths are not. The web build cannot list
/// a directory, so the bank is this static list. [`SoundId`] indexes it.
pub const SOUND_FILES: &[&str] = &[
    "assemblerdeath",
    "beep4",
    "beep6",
    "bitdeath",
    "bitfire",
    "bugartifire",
    "bugartihit",
    "built",
    "button9",
    "bytedeath",
    "bytefire",
    "bytehit",
    "commandfailed",
    "commandgiven",
    "explosion5",
    "flowfire",
    "flowhit",
    "netelectricarc",
    "obeliskfire",
    "obeliskhit",
    "packetfire",
    "pointerdeath",
    "pointerfire",
    "pointerhit",
    "select",
    "sigtermhit",
];

/// Index into [`SOUND_FILES`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SoundId(u8);

impl SoundId {
    /// `SOUND.TDF [COMPUTER] select1=select`.
    pub const SELECT: SoundId = SoundId::named("select");
    /// `SOUND.TDF [COMPUTER] ok1=commandgiven`, `arrived1=commandgiven`.
    pub const COMMAND_GIVEN: SoundId = SoundId::named("commandgiven");
    /// `sounds.lua` `MultiSelect` (`in3d=false`).
    pub const MULTI_SELECT: SoundId = SoundId::named("button9");
    /// `network_reflectorshield.lua`: the Firewall discharge.
    pub const BUILT: SoundId = SoundId::named("built");

    const fn named(name: &str) -> SoundId {
        let mut i = 0;
        while i < SOUND_FILES.len() {
            if str_eq(SOUND_FILES[i], name) {
                return SoundId(i as u8);
            }
            i += 1;
        }
        panic!("sound name not in SOUND_FILES");
    }

    /// Resolve a TDF sound name (`soundstart=` / `soundhit=` value) the
    /// way the engine's VFS does: case-insensitively, extension implied.
    /// `None` for the empty string (the TDF default) or an unknown name.
    pub fn from_name(name: &str) -> Option<SoundId> {
        if name.is_empty() {
            return None;
        }
        SOUND_FILES
            .iter()
            .position(|file| file.eq_ignore_ascii_case(name))
            .map(|i| SoundId(i as u8))
    }

    pub fn name(self) -> &'static str {
        SOUND_FILES[self.0 as usize]
    }

    fn asset_path(self) -> String {
        format!("sounds/{}.ogg", self.name())
    }
}

const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// The engine's mixer channels (`ISound.cpp:59`), each with its own
/// volume under the master. Upstream defaults every channel to 100 %.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Channel {
    /// Unit arrivals, activation; `Channels::General`.
    General,
    /// Weapon fire and explosions; `Channels::Battle`.
    Battle,
    /// Selection and order acknowledgements; `Channels::UnitReply`.
    UnitReply,
    /// GUI beeps; `Channels::UserInterface`. Always 2D.
    UserInterface,
}

impl Channel {
    /// The Audio page knob that scales this channel (`snd_volbattle`,
    /// `snd_volunitreply`, `snd_volui`; General follows the master).
    fn knob(self) -> VolumeKnob {
        match self {
            Channel::General => VolumeKnob::Master,
            Channel::Battle => VolumeKnob::Battle,
            Channel::UnitReply => VolumeKnob::UnitReply,
            Channel::UserInterface => VolumeKnob::Interface,
        }
    }
}

/// One sound to play this frame. `pos` is the world position of a 3D
/// source; `None` plays it at the listener (`in3d=false`, or a Lua
/// `PlaySoundFile` without coordinates).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoundRequest {
    pub sound: SoundId,
    pub channel: Channel,
    pub pos: Option<Vec3>,
}

/// Sounds the sim asked for since the last drain, plus the once-per-
/// frame order acknowledgement.
#[derive(Resource, Default, Debug)]
pub struct SoundQueue {
    requests: Vec<SoundRequest>,
    /// `CSelectedUnitsHandler::GiveCommand` plays the first selected
    /// unit's `ok` sound once per command. The order sites only raise
    /// this; [`play_queued_sounds`] resolves the unit.
    ok: bool,
}

impl SoundQueue {
    pub fn play(&mut self, sound: SoundId, channel: Channel, pos: Option<Vec3>) {
        self.requests.push(SoundRequest {
            sound,
            channel,
            pos,
        });
    }

    /// Play a TDF-named sound; silently nothing for the empty default or
    /// a name the original never shipped a file for.
    pub fn play_named(&mut self, name: &str, channel: Channel, pos: Option<Vec3>) {
        if let Some(sound) = SoundId::from_name(name) {
            self.play(sound, channel, pos);
        }
    }

    /// `CWeapon::Fire` (`Weapon.cpp:1269`): the weapon's `soundstart`
    /// at the firing unit, once per salvo shot (KP leaves `soundTrigger`
    /// at its default, so every shot of a burst sounds).
    pub fn weapon_fired(&mut self, def: &spring_tdf::WeaponDef, muzzle: Vec3) {
        self.play_named(&def.sound_start, Channel::Battle, Some(muzzle));
    }

    /// `CGameHelper::Explosion` (`GameHelper.cpp:340`): the weapon's
    /// `soundhit` where it went off — projectile impacts, beam hits and
    /// the `ExplodeAs` death blast alike.
    pub fn explosion(&mut self, def: &spring_tdf::WeaponDef, pos: Vec3) {
        self.play_named(&def.sound_hit, Channel::Battle, Some(pos));
    }

    /// A player command was given to the selection
    /// (`SelectedUnitsHandler.cpp:264`).
    pub fn ok(&mut self) {
        self.ok = true;
    }

    /// Requests waiting for the next drain (tests).
    #[cfg(test)]
    pub fn pending(&self) -> &[SoundRequest] {
        &self.requests
    }
}

/// Raise the once-per-frame order acknowledgement from code that only
/// has `Commands` (the player order helpers). A world without the queue
/// (tests) ignores it.
pub fn ack_order(commands: &mut Commands) {
    commands.queue(|world: &mut World| {
        if let Some(mut queue) = world.get_resource_mut::<SoundQueue>() {
            queue.ok();
        }
    });
}

/// Whether a unit kind answers to selection and orders. Every KP unit
/// is `SoundCategory=COMPUTER` (`select1`, `ok1`, `arrived1` in
/// `SOUND.TDF`) except `mineblaster.fbi`, whose category is empty.
pub fn has_reply_sounds(kind: UnitKind) -> bool {
    kind != UnitKind::Debug
}

/// One sound effect, decoded to PCM when it loads.
///
/// Bevy's `AudioSource` keeps the file bytes and builds a fresh rodio
/// `Decoder` for every sound it starts — on the main thread, inside
/// `play_queued_audio_system`: the Vorbis headers (codebooks included)
/// parsed again and the first two packets decoded, 2–6 ms per sound,
/// several sounds a frame in a battle. Decoded once in [`SfxLoader`] (an
/// asset task), starting a sound is an `Arc` clone. The effects are
/// short, so the PCM is a few MB for all of them; the music streams
/// from its files as before.
#[derive(Asset, TypePath)]
pub struct Sfx(rodio::buffer::SamplesBuffer);

impl bevy::audio::Decodable for Sfx {
    type Decoder = rodio::buffer::SamplesBuffer;

    fn decoder(&self) -> Self::Decoder {
        self.0.clone()
    }
}

/// Loads an `.ogg` as an [`Sfx`]. No extensions of its own: only loads
/// typed as `Sfx` reach it, so `.ogg` still means `AudioSource` to
/// everything else (the music).
#[derive(Default, TypePath)]
struct SfxLoader;

impl bevy::asset::AssetLoader for SfxLoader {
    type Asset = Sfx;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(
        &self,
        reader: &mut dyn bevy::asset::io::Reader,
        _settings: &(),
        _context: &mut bevy::asset::LoadContext<'_>,
    ) -> Result<Sfx, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        decode_sfx(bytes)
    }

    fn extensions(&self) -> &[&str] {
        &[]
    }
}

/// The whole file as PCM, in its own channel count and rate.
fn decode_sfx(bytes: Vec<u8>) -> std::io::Result<Sfx> {
    use rodio::Source;
    let decoder = rodio::Decoder::new(std::io::Cursor::new(bytes))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let (channels, rate) = (decoder.channels(), decoder.sample_rate());
    let samples: Vec<rodio::Sample> = decoder.collect();
    Ok(Sfx(rodio::buffer::SamplesBuffer::new(
        channels, rate, samples,
    )))
}

/// Handles for every sound in [`SOUND_FILES`], indexed by [`SoundId`].
#[derive(Resource)]
pub struct SoundBank {
    handles: Vec<Handle<Sfx>>,
}

impl SoundBank {
    fn handle(&self, sound: SoundId) -> &Handle<Sfx> {
        &self.handles[sound.0 as usize]
    }
}

fn load_sound_bank(mut commands: Commands, asset_server: Res<AssetServer>) {
    let handles = (0..SOUND_FILES.len())
        .map(|i| asset_server.load(SoundId(i as u8).asset_path()))
        .collect();
    info!(
        "audio: loading {} original sound effects from assets/sounds",
        SOUND_FILES.len()
    );
    commands.insert_resource(SoundBank { handles });
}

/// Marker on the one-shot sound entities, to count the live sources.
/// Match content: a sound still playing when the match ends goes with
/// it (it has no `Transform` to mark it otherwise).
#[derive(Component)]
#[require(crate::map_loading::MatchEntity)]
struct GameSound;

/// `kp_settings.lua` sets `MaxSounds` to 96 (engine default 128).
const MAX_SOUNDS: usize = 96;

/// `SoundSource.cpp:25`: OpenAL reference distance, in elmos.
const REFERENCE_DIST: f32 = 200.0;
/// `SoundSource.cpp:24`: the base rolloff factor; every KP sound item
/// keeps `rolloff = 1`.
const ROLLOFF_FACTOR: f32 = 5.0;
/// `Sound.cpp:845`: "for altitudes up to and including 600 elmos, the
/// rolloff is always clamped to 1".
const FULL_ROLLOFF_HEIGHT: f32 = 600.0;

/// The engine's gain for a 3D source `distance` elmos from a listener
/// `camera_height` elmos above the ground.
///
/// `AL_INVERSE_DISTANCE_CLAMPED` (`Sound.cpp:729`) with the reference
/// distance and rolloff above, the rolloff scaled down as the camera
/// rises (`CSound::UpdateListenerReal`) so a zoomed-out player still
/// hears the battle.
pub fn spatial_gain(distance: f32, camera_height: f32) -> f32 {
    let height_mod = (FULL_ROLLOFF_HEIGHT / camera_height.det_max(1.0)).det_min(1.0);
    let rolloff = ROLLOFF_FACTOR * height_mod;
    REFERENCE_DIST
        / (REFERENCE_DIST + rolloff * (distance.det_max(REFERENCE_DIST) - REFERENCE_DIST))
}

/// Port tuning on top of the engine's falloff: OpenAL's inverse
/// distance model with the engine's rolloff of 5 leaves a unit at the
/// camera's focus at a gain of 0.06 from the default camera (800 elmos
/// out), inaudible under the music. Every positioned sound is boosted
/// by this much and capped at 1, so the battle under the camera is
/// heard at normal zoom and nothing clips when zoomed right in.
const SPATIAL_BOOST: f32 = 6.0;

/// Below this linear gain a sound is dropped rather than spawned: it
/// would be inaudible and the mixer has `MAX_SOUNDS` slots.
const MIN_GAIN: f32 = 0.005;

/// Drain the [`SoundQueue`]: the per-frame `ok`, then every request,
/// attenuated from the camera and capped at [`MAX_SOUNDS`] live sources.
#[allow(clippy::too_many_arguments)]
fn play_queued_sounds(
    mut queue: ResMut<SoundQueue>,
    bank: Option<Res<SoundBank>>,
    settings: Res<RenderSettings>,
    camera: Query<&GlobalTransform, With<RtsCamera>>,
    heightmap: Option<Res<Heightmap>>,
    selected: Query<(&GlobalTransform, &UnitType), With<Selected>>,
    live: Query<(), With<GameSound>>,
    mut commands: Commands,
    mut announced: Local<bool>,
) {
    if std::mem::take(&mut queue.ok)
        && let Some((gtf, kind)) = selected.iter().next()
        && has_reply_sounds(kind.0)
    {
        queue.play(
            SoundId::COMMAND_GIVEN,
            Channel::UnitReply,
            Some(gtf.translation()),
        );
    }
    let master = settings.audio.linear(VolumeKnob::Master);
    let Some(bank) = bank else {
        queue.requests.clear();
        return;
    };
    if master <= 0.0 || queue.requests.is_empty() {
        queue.requests.clear();
        return;
    }
    let listener = camera.iter().next().map(|gtf| gtf.translation());
    let camera_height = listener.map_or(FULL_ROLLOFF_HEIGHT, |cam| {
        let ground = heightmap
            .as_deref()
            .map_or(0.0, |hm| hm.sample(cam.x, cam.z));
        cam.y - ground
    });
    let mut slots = MAX_SOUNDS.saturating_sub(live.iter().count());
    let mut last: Option<SoundRequest> = None;
    for request in queue.requests.drain(..) {
        // A multi-projectile shot pushes one request per projectile from
        // the same muzzle; the engine plays one sample per salvo shot.
        if last == Some(request) {
            continue;
        }
        last = Some(request);
        if slots == 0 {
            break;
        }
        let gain = match (request.pos, listener) {
            (Some(pos), Some(cam)) => {
                (spatial_gain(pos.distance(cam), camera_height) * SPATIAL_BOOST).det_min(1.0)
            }
            _ => 1.0,
        };
        let volume = gain * settings.audio.linear(request.channel.knob());
        // `RUST_LOG=kernel_panic::audio=debug` lists every effect with
        // the gain it plays at (or is dropped at), for tuning the mix.
        debug!(
            "audio: {} on {:?} gain {volume:.3}{}",
            request.sound.name(),
            request.channel,
            if volume < MIN_GAIN { " (dropped)" } else { "" }
        );
        if volume < MIN_GAIN {
            continue;
        }
        slots -= 1;
        if !std::mem::replace(&mut *announced, true) {
            info!(
                "audio: first sound effect `{}` at gain {volume:.3}",
                request.sound.name()
            );
        }
        commands.spawn((
            AudioPlayer(bank.handle(request.sound).clone()),
            PlaybackSettings::DESPAWN.with_volume(Volume::Linear(volume)),
            GameSound,
        ));
    }
}

/// `CSelectedUnitsHandler::SelectGroup` / `SelectUnits`
/// (`SelectedUnitsHandler.cpp:333-340`): exactly one unit newly selected
/// plays its `select` reply at the unit; more play the 2D `MultiSelect`
/// beep. Units of a category without replies (the mine blaster) count
/// but stay silent on their own.
fn selection_sounds(
    added: Query<(&GlobalTransform, &UnitType), Added<Selected>>,
    mut queue: ResMut<SoundQueue>,
) {
    let mut iter = added.iter();
    let Some((first_gtf, first_kind)) = iter.next() else {
        return;
    };
    if iter.next().is_some() {
        queue.play(SoundId::MULTI_SELECT, Channel::UserInterface, None);
    } else if has_reply_sounds(first_kind.0) {
        queue.play(
            SoundId::SELECT,
            Channel::UnitReply,
            Some(first_gtf.translation()),
        );
    }
}

pub struct GameAudioPlugin;

impl Plugin for GameAudioPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(music::MusicPlugin)
            .add_audio_source::<Sfx>()
            .init_asset_loader::<SfxLoader>()
            .init_resource::<SoundQueue>()
            .add_systems(Startup, load_sound_bank)
            .add_systems(Update, selection_sounds)
            // After every Update producer, so a shot sounds the frame it
            // is fired.
            .add_systems(PostUpdate, play_queued_sounds);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every effect decodes whole at load, and playing it hands out the
    /// same samples from the start each time.
    #[test]
    fn effects_decode_to_pcm_once() {
        use rodio::Source;
        let dir = crate::paths::assets_dir();
        for i in 0..SOUND_FILES.len() {
            let path = dir.join(SoundId(i as u8).asset_path());
            let bytes = std::fs::read(&path).unwrap();
            let sfx = decode_sfx(bytes.clone()).unwrap();
            let reference = rodio::Decoder::new(std::io::Cursor::new(bytes)).unwrap();
            let (channels, rate) = (reference.channels(), reference.sample_rate());
            let expected: Vec<rodio::Sample> = reference.collect();
            assert!(!expected.is_empty(), "{path:?} is silent");
            for _ in 0..2 {
                let decoder = bevy::audio::Decodable::decoder(&sfx);
                assert_eq!(
                    (decoder.channels(), decoder.sample_rate()),
                    (channels, rate)
                );
                assert_eq!(decoder.collect::<Vec<_>>(), expected, "{path:?}");
            }
        }
    }

    #[test]
    fn tdf_names_resolve_case_insensitively() {
        assert_eq!(
            SoundId::from_name("Explosion5"),
            SoundId::from_name("explosion5")
        );
        assert_eq!(
            SoundId::from_name("SIGTERMhit").map(|s| s.name()),
            Some("sigtermhit")
        );
        assert_eq!(SoundId::from_name(""), None);
        assert_eq!(SoundId::from_name("nosuchsound"), None);
    }

    #[test]
    fn every_bundled_weapon_sound_has_a_file() {
        // Every `soundstart` / `soundhit` the weapon TDFs name must be
        // one of the originals we ship; anything else would be silent.
        let defs = crate::units::content::defs::defs();
        for (name, def) in &defs.weapons.weapons {
            for sound in [&def.sound_start, &def.sound_hit] {
                if !sound.is_empty() {
                    assert!(
                        SoundId::from_name(sound).is_some(),
                        "weapon {name}: sound {sound:?} has no file in SOUND_FILES"
                    );
                }
            }
        }
    }

    #[test]
    fn sound_files_exist_on_disk() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/sounds");
        for id in 0..SOUND_FILES.len() {
            let path = dir.join(format!("{}.ogg", SoundId(id as u8).name()));
            assert!(path.is_file(), "missing {}", path.display());
        }
    }

    #[test]
    fn gain_follows_the_engine_model() {
        // Inside the reference distance: full gain regardless of height.
        assert_eq!(spatial_gain(0.0, 300.0), 1.0);
        assert_eq!(spatial_gain(200.0, 5000.0), 1.0);
        // Low camera: rolloff 5. 600 elmos away -> 200 / (200 + 5 * 400).
        let low = spatial_gain(600.0, 400.0);
        assert!((low - 200.0 / 2200.0).abs() < 1e-6, "{low}");
        // High camera (1200): rolloff halves, the same distance is louder.
        let high = spatial_gain(600.0, 1200.0);
        assert!((high - 200.0 / 1200.0).abs() < 1e-6, "{high}");
        assert!(high > low);
    }

    #[test]
    fn single_and_multi_selection_pick_different_sounds() {
        let mut app = App::new();
        app.init_resource::<SoundQueue>()
            .add_systems(Update, selection_sounds);
        let unit = |app: &mut App, kind: UnitKind, x: f32| {
            app.world_mut()
                .spawn((
                    UnitType(kind),
                    Transform::from_xyz(x, 0.0, 0.0),
                    GlobalTransform::from_xyz(x, 0.0, 0.0),
                ))
                .id()
        };
        let a = unit(&mut app, UnitKind::Bit, 10.0);
        let b = unit(&mut app, UnitKind::Byte, 20.0);
        let mine = unit(&mut app, UnitKind::Debug, 30.0);

        app.world_mut().entity_mut(a).insert(Selected);
        app.update();
        assert_eq!(
            app.world().resource::<SoundQueue>().pending(),
            &[SoundRequest {
                sound: SoundId::SELECT,
                channel: Channel::UnitReply,
                pos: Some(Vec3::new(10.0, 0.0, 0.0)),
            }]
        );

        app.world_mut()
            .resource_mut::<SoundQueue>()
            .requests
            .clear();
        app.world_mut().entity_mut(a).remove::<Selected>();
        app.update();
        app.world_mut().entity_mut(a).insert(Selected);
        app.world_mut().entity_mut(b).insert(Selected);
        app.update();
        assert_eq!(
            app.world().resource::<SoundQueue>().pending(),
            &[SoundRequest {
                sound: SoundId::MULTI_SELECT,
                channel: Channel::UserInterface,
                pos: None,
            }]
        );

        app.world_mut()
            .resource_mut::<SoundQueue>()
            .requests
            .clear();
        app.world_mut().entity_mut(mine).insert(Selected);
        app.update();
        assert!(
            app.world().resource::<SoundQueue>().pending().is_empty(),
            "the mine blaster has no SoundCategory"
        );
    }
}
