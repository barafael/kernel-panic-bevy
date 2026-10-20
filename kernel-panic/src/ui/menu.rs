//! The Kernel Panic menu system, rebuilt after the original
//! "Spring Direct Launch 2" widget (`kp_spring_direct_launch.lua`).
//!
//! Pages: main menu (zigzag layout, alternating left/right anchors),
//! quick skirmish (Easy/Medium/Hard/Very Hard), advanced skirmish (map
//! list, faction cycles, grouping, difficulty, live description line),
//! map list, credits, scrollable readme, multiplayer (rooms and their
//! seats, [`lobby_page`]), an in-game Esc overlay
//! (Resume/Restart/Menu — the simulation keeps running behind it, like
//! the original; in a networked match Leave match instead of Restart),
//! and the game-over panel ("You won!"/"You lost!").
//!
//! Visual language copied from the original: green-on-black terminal
//! look, cyan `Kernel Panic!` title, blue navigation buttons, the
//! difficulty colour ladder (light-cyan → green → yellow → orange →
//! red), olive-tinted backdrop. The original draws every panel as a
//! skewed parallelogram; `bevy_ui` has no transforms on nodes, so panels
//! are bevelled rectangles with the same colour system instead.
//!
//! Input: buttons are hit-tested against the cursor in
//! [`mouse_menu_input`] and driven from the keyboard by
//! [`keyboard_menu_nav`]; both funnel into one [`MenuAction`] message so
//! all state changes live in [`handle_menu_actions`]. The game is never
//! paused by the Esc menu, but the overlay is modal for the mouse: while
//! it is open no press reaches the HUD or the world.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::ecs::system::SystemParam;
use bevy::input::InputSystems;
use bevy::picking::Pickable;
use bevy::prelude::*;
use bevy::text::FontSize;
use bevy::ui::ComputedStackIndex;

use crate::audio::music::{MusicCatalog, MusicControl, MusicPlayer};
use crate::game_setup::{
    AppState, DevOptions, GameOverDismissed, Grouping, RunGame, SkirmishConfig, build_setup,
    demo_setup, describe_setup, showcase_setup,
};
use crate::map_loading::MapCatalog;
use crate::rendering::settings::{MsaaSupport, RenderSettings, VolumeKnob, WINDOW_SIZES};
use crate::sim::DetFloat;
use crate::units::components::{Faction, Homebase, TeamId};
use crate::units::lifecycle::game_over::GameState;

mod attract;
mod lobby_page;
mod map_preview;
mod replays_page;
mod stats_page;

use attract::{AttractCamera, attract_camera};

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MenuPage>()
            .init_resource::<EscMenuOpen>()
            .init_resource::<EscSettingsOpen>()
            .init_resource::<EscAudioOpen>()
            .init_resource::<EscResignConfirm>()
            .init_resource::<GameOverOpen>()
            .init_resource::<stats_page::StatsView>()
            .init_resource::<ReadmeScroll>()
            .init_resource::<DemoDirector>()
            .init_resource::<AttractCamera>()
            .init_resource::<MenuFocus>()
            .add_plugins(replays_page::ReplaysPagePlugin)
            .add_plugins(map_preview::MapPreviewPlugin)
            // The Settings page's frame-rate readout.
            .add_plugins(FrameTimeDiagnosticsPlugin::default())
            .add_message::<MenuActionMessage>()
            .add_systems(
                OnEnter(AppState::InGame),
                (close_all_overlays, despawn_launch_menu),
            )
            .add_systems(OnExit(AppState::InGame), close_all_overlays)
            // A click on a menu button is the menu's, and the Esc overlay
            // swallows every press: consumed before the HUD's own mouse
            // input (build bar, command panel, also `PreUpdate`) and
            // everything in `Update` (placement, order clicks,
            // selection, right-click orders, camera drag) sees it.
            .add_systems(
                PreUpdate,
                mouse_menu_input
                    .after(InputSystems)
                    .in_set(MenuMouseInput)
                    .run_if(in_state(AppState::Menu).or_else(in_state(AppState::InGame))),
            )
            .add_systems(
                Update,
                (
                    handle_menu_actions,
                    keyboard_menu_nav,
                    esc_in_menu.run_if(in_state(AppState::Menu)),
                    boot_demo
                        .run_if(in_state(AppState::Menu).and_then(resource_exists::<MapCatalog>)),
                    demo_director.run_if(
                        in_state(AppState::Menu)
                            .and_then(resource_exists::<crate::game_setup::GameSetup>),
                    ),
                    attract_camera
                        .run_if(in_state(AppState::Menu).or_else(attract_recording))
                        .before(crate::rendering::camera::camera_smoothing),
                    maintain_launch_menu.run_if(in_state(AppState::Menu)),
                    map_preview_info
                        .run_if(in_state(AppState::Menu))
                        .after(maintain_launch_menu),
                    lobby_page::type_into_fields
                        .run_if(in_state(AppState::Menu))
                        .before(keyboard_menu_nav),
                    esc_toggle.run_if(in_state(AppState::InGame)),
                    maintain_esc_menu.run_if(in_state(AppState::InGame)),
                    // After the page builders: a freshly drawn Settings
                    // page shows the rate this same frame.
                    (fps_readout, music_readout)
                        .after(maintain_launch_menu)
                        .after(maintain_esc_menu),
                    game_over_watch.run_if(in_state(AppState::InGame)),
                    maintain_game_over.run_if(in_state(AppState::InGame)),
                    finish_leaving.run_if(in_state(AppState::InGame)),
                ),
            );
    }
}

/// [`mouse_menu_input`]'s `PreUpdate` slot: the HUD's mouse input runs
/// after it.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MenuMouseInput;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Which page the launch menu (or an overlay) is showing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Resource)]
pub(crate) enum MenuPage {
    #[default]
    Main,
    QuickSkirmish,
    AdvancedSkirmish,
    MapList,
    Showcase,
    Credits,
    Readme,
    Settings,
    /// The volume knobs, reached from Settings.
    Audio,
    /// Open or join a room, then the room itself (`net::lobby`).
    Multiplayer,
    /// The recorded matches (`net::replay_log`).
    Replays,
}

#[derive(Debug, Default, Deref, DerefMut, Resource)]
pub(crate) struct EscMenuOpen(pub(crate) bool);

/// The Settings page drawn inside the Esc overlay (in a match).
#[derive(Debug, Default, Deref, DerefMut, Resource)]
pub(crate) struct EscSettingsOpen(pub(crate) bool);

/// The Audio page drawn inside the Esc overlay's Settings page.
#[derive(Debug, Default, Deref, DerefMut, Resource)]
pub(crate) struct EscAudioOpen(pub(crate) bool);

/// The Esc overlay's Resign plate was clicked once: the next click
/// resigns (Spring's quit box asks the same way).
#[derive(Debug, Default, Deref, DerefMut, Resource)]
pub(crate) struct EscResignConfirm(pub(crate) bool);

#[derive(Debug, Default, Deref, DerefMut, Resource)]
struct GameOverOpen(bool);

#[derive(Debug, Default, Deref, DerefMut, Resource)]
struct ReadmeScroll(usize);

/// Keyboard-navigation focus: the menu button currently highlighted by
/// arrow/Tab navigation and activated with Enter/Space. Validated against
/// the live world each frame (buttons respawn on page changes).
#[derive(Debug, Default, Resource)]
struct MenuFocus {
    entity: Option<Entity>,
    /// Which input drives the highlight. Keyboard mode paints the focused
    /// button bright; mouse mode leaves it to [`mouse_menu_input`]'s hover.
    mode: InputMode,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    #[default]
    Mouse,
    Keyboard,
}

/// Marker on every menu root (launch menu, Esc overlay, game-over panel).
#[derive(Component)]
pub(super) struct MenuRoot;

/// Marker on every placed menu frame that is not a button (labels,
/// headings, readouts): with [`MenuButton`], the set `menu_shots`
/// audits for overlaps and off-window rects.
#[derive(Component)]
pub(super) struct MenuFrame;

/// The Settings page's frame-rate label; [`fps_readout`] keeps its text
/// current while the page is up.
#[derive(Component)]
struct FpsReadout;

/// One menu button: carries its action and base colour (hover restores
/// exactly this).
#[derive(Component)]
pub(super) struct MenuButton {
    action: MenuAction,
    base: Color,
}

/// Everything a menu button can do. Handled centrally in
/// [`handle_menu_actions`].
#[derive(Debug, Clone, Copy, PartialEq)]
enum MenuAction {
    Goto(MenuPage),
    /// Quick skirmish with the given difficulty.
    QuickStart(u8),
    StartSkirmish,
    Restart,
    GoToMenu,
    Resume,
    /// Victory: dismiss the panel and keep simulating.
    KeepPlaying,
    Quit,
    CycleYourFaction,
    CycleEnemyFaction,
    SetGrouping(Grouping),
    SetDifficulty(u8),
    PickMap(usize),
    PickRandomMap,
    /// Main page: step the attract-mode demo's map through the catalog
    /// (`-1` back, `+1` forward) and restart the demo on it.
    CycleDemoMap(i32),
    ScrollReadme(i32),
    /// Start showcase mode for the given faction.
    Showcase(Faction),
    /// Esc overlay: show / hide the Settings page.
    EscSettings(bool),
    SetMsaa(u8),
    SetBloom(bool),
    SetVsync(bool),
    SetFullscreen(bool),
    SetWindowSize(u32, u32),
    /// Show the automatic tip dispenser in a match.
    SetTips(bool),
    /// One Audio page knob, in percent.
    SetVolume(VolumeKnob, u8),
    /// Esc overlay: show / hide the Audio page inside Settings.
    EscAudio(bool),
    /// Esc overlay: Resign — `false` asks, `true` (the second click)
    /// resigns (`SimCommand::Resign`).
    Resign(bool),
    /// Networked match: resign, leave the match, back to its room with
    /// the same connection ([`NetLeave::leave`], `to_room`). The menu
    /// button leaves the room too.
    BackToRoom,
    /// The music cluster (main page, Esc overlay): previous / pause / next.
    Music(MusicControl),
    /// The Multiplayer page: handed to the lobby (`net::lobby`).
    Lobby(crate::net::lobby::LobbyAction),
    /// The Replays page.
    Replays(replays_page::ReplaysAction),
    /// The game-over panel's Statistics page.
    Stats(stats_page::StatsAction),
}

// ---------------------------------------------------------------------------
// Palette (lifted from the original's AddFrame colour table)
// ---------------------------------------------------------------------------

const TITLE_CYAN: Color = Color::srgb(0.0, 1.0, 1.0);
const BUTTON_GREEN: Color = Color::srgb(0.0, 1.0, 0.0);
const NAV_BLUE: Color = Color::srgb(0.0, 0.0, 1.0);
const EASY_CYAN: Color = Color::srgb(0.33, 0.88, 1.0);
const MEDIUM_GREEN: Color = Color::srgb(0.33, 0.88, 0.0);
const HARD_YELLOW: Color = Color::srgb(0.88, 0.79, 0.0);
const EXTREME_ORANGE: Color = Color::srgb(0.88, 0.40, 0.0);
const VERY_HARD_RED: Color = Color::srgb(0.88, 0.02, 0.0);
const MAP_GREEN: Color = Color::srgb(0.1, 1.0, 0.0);
const TEAL: Color = Color::srgb(0.0, 1.0, 0.5);
const DESC_BLUE: Color = Color::srgb(0.2, 0.5, 0.9);
const WON_GREEN: Color = Color::srgb(0.2, 1.0, 0.3);
const LOST_RED: Color = Color::srgb(1.0, 0.2, 0.2);
const README_UPDOWN: Color = Color::srgb(0.9, 0.6, 0.0);
const TEXT_WHITE: Color = Color::srgb(1.0, 1.0, 1.0);

/// The AI difficulties (`1 +` index) as the skirmish page and a room
/// offer them.
const DIFFICULTIES: [(&str, Color); 4] = [
    ("Easy", EASY_CYAN),
    ("Medium", MEDIUM_GREEN),
    ("Hard", HARD_YELLOW),
    ("Extreme", EXTREME_ORANGE),
];

/// A faction's colour (the showcase page, a room's seats).
fn faction_color(faction: Faction) -> Color {
    match faction {
        Faction::System => EASY_CYAN,
        Faction::Hacker => VERY_HARD_RED,
        Faction::Network => DESC_BLUE,
    }
}

/// The original renders fills at half the listed alpha; borders near
/// full. We mirror that so colours read as tinted glass over black.
fn fill(color: Color) -> BackgroundColor {
    let a = color.to_srgba();
    BackgroundColor(Color::srgba(a.red, a.green, a.blue, 0.12))
}

fn border(color: Color) -> BorderColor {
    BorderColor::all(color)
}

/// Hover brightening: `c → 1-(1-c)/2` per channel, straight from the
/// original's `DrawFrame` selected state.
fn brighten(color: Color) -> Color {
    let c = color.to_srgba();
    Color::srgb(
        1.0 - (1.0 - c.red) / 2.0,
        1.0 - (1.0 - c.green) / 2.0,
        1.0 - (1.0 - c.blue) / 2.0,
    )
}

// ---------------------------------------------------------------------------
// Shared UI construction
// ---------------------------------------------------------------------------

/// Fullscreen menu backdrop. Translucent since the attract-mode demo
/// runs live behind the launch menu (the original showed the 3D view
/// behind the in-game menu the same way).
fn spawn_backdrop(commands: &mut Commands, tint: Color) -> Entity {
    commands
        .spawn((
            MenuRoot,
            crate::map_loading::PersistentEntity,
            // Pure background — never captures the pointer. The buttons
            // are its children and get all hover/click; with the live
            // 3D demo rendering behind this node, an absorbable backdrop
            // would swallow events meant for the buttons.
            Pickable::IGNORE,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            BackgroundColor(tint),
        ))
        .id()
}

/// Olive glass over the live demo — the original's tiled-backdrop tint.
const MENU_GLASS: Color = Color::srgba(0.13, 0.13, 0.0, 0.55);
/// Darker glass for the in-game overlays.
const OVERLAY_GLASS: Color = Color::srgba(0.0, 0.0, 0.0, 0.55);

/// Where a frame hangs off its position: the original `AddFrame`'s
/// two-letter `FramePosition` code — horizontal `l`/`c`/`r`, vertical
/// `t`/`c`/`b`. `Cc` centres the frame on the point, `Rb` puts its
/// bottom-right corner there, and so on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    Lt,
    Ct,
    Rt,
    Lc,
    Cc,
    Rc,
    Lb,
    Cb,
    Rb,
}

impl Anchor {
    /// Translation, in percent of the frame's own size, that moves the
    /// frame's top-left corner (where layout puts it) onto the anchor.
    fn shift(self) -> (f32, f32) {
        use Anchor::*;
        let x = match self {
            Lt | Lc | Lb => 0.0,
            Ct | Cc | Cb => -50.0,
            Rt | Rc | Rb => -100.0,
        };
        let y = match self {
            Lt | Ct | Rt => 0.0,
            Lc | Cc | Rc => -50.0,
            Lb | Cb | Rb => -100.0,
        };
        (x, y)
    }
}

/// Absolute placement at the original's screen coordinates: `x` runs
/// 0..1 from the left, `y` 0..1 from the *bottom* (Spring's convention,
/// so page layouts transcribe `AddFrame(…, {x=vsx*X, y=vsy*Y}, …)`
/// literally). `UiTransform` percentages resolve against the node's own
/// size, which is what makes the anchor exact for any text width.
fn anchored(x: f32, y: f32, anchor: Anchor) -> (Node, UiTransform) {
    let (sx, sy) = anchor.shift();
    (
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(x * 100.0),
            top: Val::Percent((1.0 - y) * 100.0),
            ..default()
        },
        UiTransform::from_translation(Val2::percent(sx, sy)),
    )
}

/// Frame padding: the original sizes a frame `FontSize·(1+width)` by
/// `FontSize·(1+lines)`, i.e. half a font size of margin per side.
fn frame_padding(font_size: f32) -> UiRect {
    UiRect::axes(Val::Px(font_size * 0.5), Val::Px(font_size * 0.25))
}

fn frame_border_px(font_size: f32) -> f32 {
    (font_size * 0.09).det_max(1.0)
}

fn frame_border(font_size: f32) -> UiRect {
    UiRect::all(Val::Px(frame_border_px(font_size)))
}

/// Where a frame goes.
#[derive(Clone, Copy)]
enum Place {
    /// At original coordinates (see [`anchored`]).
    At((f32, f32), Anchor),
    /// The next cell of a flex row or column (a room's seat table).
    Flow,
}

/// The end of a label that stays in view when the label is wider than
/// its cell (the rest is clipped); a label that fits stays centred.
#[derive(Clone, Copy)]
enum Keep {
    Start,
    /// A line being typed: its newest characters.
    End,
}

/// The node of a frame at `place` around a label of `font_size`:
/// padding, `min_width`, the label centred, or clipped to keep one end
/// in view ([`Keep`]). A cell keeps its size in its row; only a clipped
/// one gives way when the row is too narrow.
fn frame_node(
    place: Place,
    font_size: f32,
    min_width: Val,
    clip: Option<Keep>,
) -> (Node, UiTransform) {
    let (mut node, transform) = match place {
        Place::At((x, y), anchor) => anchored(x, y, anchor),
        Place::Flow => (
            Node {
                flex_shrink: 0.0,
                ..default()
            },
            UiTransform::default(),
        ),
    };
    node.padding = frame_padding(font_size);
    node.min_width = min_width;
    node.justify_content = JustifyContent::Center;
    if let Some(keep) = clip {
        // The label's auto margins centre it while it fits; past that
        // they are zero and the justification picks the end in view.
        node.overflow = Overflow::clip();
        node.flex_shrink = 1.0;
        node.justify_content = match keep {
            Keep::Start => JustifyContent::FlexStart,
            Keep::End => JustifyContent::FlexEnd,
        };
    }
    (node, transform)
}

/// A clickable frame at original coordinates (see [`anchored`]) or in
/// a flex row ([`ButtonSpec::cell`]); clicks go to the central action
/// router. Built with [`ButtonSpec::spawn`].
struct ButtonSpec<'a> {
    label: &'a str,
    color: Color,
    font_size: f32,
    place: Place,
    action: MenuAction,
    /// Minimum width (in percent of the window for a frame at original
    /// coordinates), so a column of frames lines up as even boxes
    /// instead of a ragged edge.
    min_width: Val,
    /// The current pick of a choice group (grouping, difficulty): drawn
    /// brightened with a heavier border, so the page shows the
    /// configuration at a glance.
    chosen: bool,
    clip: Option<Keep>,
}

impl<'a> ButtonSpec<'a> {
    fn new(
        label: &'a str,
        color: Color,
        font_size: f32,
        pos: (f32, f32),
        anchor: Anchor,
        action: MenuAction,
    ) -> Self {
        Self {
            label,
            color,
            font_size,
            place: Place::At(pos, anchor),
            action,
            min_width: Val::Auto,
            chosen: false,
            clip: None,
        }
    }

    /// A button in a flex row or column.
    fn cell(label: &'a str, color: Color, font_size: f32, action: MenuAction) -> Self {
        Self {
            place: Place::Flow,
            ..Self::new(label, color, font_size, (0.0, 0.0), Anchor::Cc, action)
        }
    }

    fn min_width(self, pct: f32) -> Self {
        Self {
            min_width: Val::Percent(pct),
            ..self
        }
    }

    fn min_width_px(self, px: f32) -> Self {
        Self {
            min_width: Val::Px(px),
            ..self
        }
    }

    fn chosen(self, chosen: bool) -> Self {
        Self { chosen, ..self }
    }

    fn clip(self, keep: Keep) -> Self {
        Self {
            clip: Some(keep),
            ..self
        }
    }

    fn spawn(self, commands: &mut Commands, parent: Entity) -> Entity {
        let font_size = readable(self.font_size);
        let base = if self.chosen {
            brighten(self.color)
        } else {
            self.color
        };
        let text = spawn_frame_text(commands, self.label, font_size, Justify::Center, self.clip);
        let (mut node, transform) = frame_node(self.place, font_size, self.min_width, self.clip);
        node.border = if self.chosen {
            UiRect::all(Val::Px((font_size * 0.18).det_max(2.0)))
        } else {
            frame_border(font_size)
        };
        let entity = commands
            .spawn((
                MenuButton {
                    action: self.action,
                    base,
                },
                node,
                transform,
                fill(base),
                border(base),
            ))
            .add_child(text)
            .id();
        commands.entity(parent).add_child(entity);
        entity
    }
}

fn spawn_button_text(commands: &mut Commands, label: &str, font_size: f32) -> Entity {
    spawn_frame_text(commands, label, font_size, Justify::Center, None)
}

/// A frame's label; `clip`: its frame clips it ([`frame_node`]).
fn spawn_frame_text(
    commands: &mut Commands,
    label: &str,
    font_size: f32,
    justify: Justify,
    clip: Option<Keep>,
) -> Entity {
    let text = commands
        .spawn((
            Text::new(label),
            TextColor(TEXT_WHITE),
            TextFont {
                font_size: FontSize::Px(font_size),
                ..default()
            },
            // Why no-wrap: an absolute node's available width is what's
            // left right of its `left` inset, so a right-anchored frame
            // near the edge would otherwise wrap its label.
            TextLayout::new(justify, LineBreak::NoWrap),
            Pickable::IGNORE,
        ))
        .id();
    if clip.is_some() {
        commands.entity(text).insert(Node {
            margin: UiRect::horizontal(Val::Auto),
            ..default()
        });
    }
    text
}

/// A centred column of `min_width` 18 % buttons stepping down from
/// y=60 % (the quick-battle and showcase pages).
fn centre_column<'a>(
    commands: &mut Commands,
    root: Entity,
    font_size: f32,
    entries: impl IntoIterator<Item = (&'a str, Color, MenuAction)>,
) {
    for (i, (name, color, action)) in entries.into_iter().enumerate() {
        ButtonSpec::new(
            name,
            color,
            font_size,
            (0.5, 0.6 - 0.1 * i as f32),
            Anchor::Cc,
            action,
        )
        .min_width(18.0)
        .spawn(commands, root);
    }
}

/// Non-interactive frame (headings, description lines, text blocks),
/// the counterpart of [`ButtonSpec`]: bare centred text by default,
/// `frame` draws the original's coloured plate behind it, `justify`
/// aligns multi-line text inside the frame.
struct LabelSpec<'a> {
    text: &'a str,
    font_size: f32,
    place: Place,
    frame: Option<Color>,
    justify: Justify,
    min_width: Val,
    clip: Option<Keep>,
}

impl<'a> LabelSpec<'a> {
    fn new(text: &'a str, font_size: f32, pos: (f32, f32), anchor: Anchor) -> Self {
        Self {
            text,
            font_size,
            place: Place::At(pos, anchor),
            frame: None,
            justify: Justify::Center,
            min_width: Val::Auto,
            clip: None,
        }
    }

    /// A label in a flex row or column.
    fn cell(text: &'a str, font_size: f32) -> Self {
        Self {
            place: Place::Flow,
            ..Self::new(text, font_size, (0.0, 0.0), Anchor::Cc)
        }
    }

    fn min_width_px(self, px: f32) -> Self {
        Self {
            min_width: Val::Px(px),
            ..self
        }
    }

    fn clip(self, keep: Keep) -> Self {
        Self {
            clip: Some(keep),
            ..self
        }
    }

    fn frame(self, color: Color) -> Self {
        Self {
            frame: Some(color),
            ..self
        }
    }

    fn justify(self, justify: Justify) -> Self {
        Self { justify, ..self }
    }

    fn spawn(self, commands: &mut Commands, parent: Entity) -> Entity {
        let font_size = readable(self.font_size);
        let text = spawn_frame_text(commands, self.text, font_size, self.justify, self.clip);
        let (mut node, transform) = frame_node(self.place, font_size, self.min_width, self.clip);
        if self.frame.is_some() {
            node.border = frame_border(font_size);
        }
        let entity = commands
            .spawn((MenuFrame, node, transform, Pickable::IGNORE))
            .id();
        if let Some(color) = self.frame {
            commands.entity(entity).insert((fill(color), border(color)));
        }
        commands.entity(entity).add_child(text);
        commands.entity(parent).add_child(entity);
        entity
    }
}

/// The `Kernel Panic!` title: cyan, top-centre, like the original main
/// menu's `AddFrame("Kernel Panic!", {x=vsx*0.5, y=vsy*0.98}, vsy/14, …, "ct")`.
fn title(commands: &mut Commands, parent: Entity, font_size: f32) {
    let t = commands
        .spawn((
            Text::new("Kernel Panic!"),
            TextColor(TITLE_CYAN),
            TextFont {
                font_size: FontSize::Px(font_size),
                ..default()
            },
            TextLayout::new(Justify::Center, LineBreak::NoWrap),
            Pickable::IGNORE,
        ))
        .id();
    let entity = commands
        .spawn((MenuFrame, anchored(0.5, 0.98, Anchor::Ct), Pickable::IGNORE))
        .id();
    commands.entity(entity).add_child(t);
    commands.entity(parent).add_child(entity);
}

/// No label on any sane window goes below this: the original's ratios
/// put a hint at 9 px on an 800×600 window.
const MIN_FONT_PX: f32 = 11.5;

/// `px`, or [`MIN_FONT_PX`] when the window is too small for it.
fn readable(px: f32) -> f32 {
    px.det_max(MIN_FONT_PX)
}

/// Window-height-derived font sizes. The original's ratios are `vsy/14`
/// title, `vsy/20` main menu, `vsy/24` pages, `vsy/28` lists; the
/// title and the main menu's plates are a quarter larger here (the
/// original's were sized for 4:3 screens at 1024×768 and read small on
/// a widescreen window), the dense pages keep the original ratios.
fn vsizes(height: f32) -> (f32, f32, f32, f32) {
    (height / 11.0, height / 16.0, height / 24.0, height / 28.0)
}

// ---------------------------------------------------------------------------
// Action routing
// ---------------------------------------------------------------------------

#[derive(Message)]
struct MenuActionMessage {
    action: MenuAction,
}

/// Tests: press "Back to room" (the Esc overlay's plate in a networked
/// match).
#[cfg(test)]
pub(crate) fn press_back_to_room(world: &mut World) {
    world.write_message(MenuActionMessage {
        action: MenuAction::BackToRoom,
    });
}

/// What the menu pages show and the buttons edit: the current page,
/// the skirmish setup, the readme scroll and the render settings.
#[derive(SystemParam)]
struct MenuState<'w> {
    page: ResMut<'w, MenuPage>,
    config: ResMut<'w, SkirmishConfig>,
    readme_scroll: ResMut<'w, ReadmeScroll>,
    render: ResMut<'w, RenderSettings>,
}

/// The in-match overlays' open flags (Esc menu, its Settings page, the
/// game-over panel and its dismissal).
#[derive(SystemParam)]
struct Overlays<'w> {
    esc_open: ResMut<'w, EscMenuOpen>,
    esc_settings: ResMut<'w, EscSettingsOpen>,
    esc_audio: ResMut<'w, EscAudioOpen>,
    game_over_open: ResMut<'w, GameOverOpen>,
    dismissed: ResMut<'w, GameOverDismissed>,
    resign_confirm: ResMut<'w, EscResignConfirm>,
    stats: ResMut<'w, stats_page::StatsView>,
}

impl Overlays<'_> {
    fn close_all(&mut self) {
        self.stats.open = false;
        *self.esc_open = EscMenuOpen(false);
        *self.esc_settings = EscSettingsOpen(false);
        *self.esc_audio = EscAudioOpen(false);
        *self.game_over_open = GameOverOpen(false);
        *self.resign_confirm = EscResignConfirm(false);
    }
}

/// The transitions a button can trigger: app / game state changes, a
/// match (re)start, quitting.
#[derive(SystemParam)]
struct MenuFlow<'w> {
    app_state: ResMut<'w, NextState<AppState>>,
    game_state: ResMut<'w, NextState<GameState>>,
    run_game: MessageWriter<'w, RunGame>,
    exit: MessageWriter<'w, AppExit>,
    music: MessageWriter<'w, MusicControl>,
    bus: Option<ResMut<'w, crate::net::CommandBus>>,
    lobby: MessageWriter<'w, crate::net::lobby::LobbyAction>,
    replays: MessageWriter<'w, replays_page::ReplaysAction>,
}

/// Leaving a networked match: what "Leave match" (and the menu, from a
/// networked match) needs.
#[derive(SystemParam)]
struct NetLeave<'w> {
    net: Option<ResMut<'w, crate::net::driver::NetSession>>,
    local: Option<Res<'w, crate::units::player::LocalTeam>>,
    control: Option<Res<'w, crate::net::MatchControl>>,
    fates: Option<Res<'w, crate::units::teams::TeamFates>>,
    /// The connection the match handed back ("Back to room").
    back: Option<Res<'w, crate::net::driver::BackToRoom>>,
}

impl NetLeave<'_> {
    /// In a networked match that is still connected.
    fn connected(&self) -> bool {
        self.net.as_ref().is_some_and(|n| n.session.connected())
    }

    /// Resign (if still in play), then let the driver disconnect
    /// (`NetSession::request_leave`) or, `to_room`, hand the connection
    /// back to the room (`NetSession::request_back_to_room`);
    /// [`finish_leaving`] goes to the menu once the transport is closed.
    /// Spring's quit box does the same: resign, then leave
    /// (`Game/UI/QuitBox.cpp`).
    fn leave(&mut self, bus: Option<&mut crate::net::CommandBus>, to_room: bool) {
        let active = self.local.as_ref().and_then(|l| l.seat()).filter(|&p| {
            !self
                .control
                .as_ref()
                .is_some_and(|c| c.resigned.contains(&p))
                && !self.fates.as_ref().is_some_and(|f| f.is_dead(p))
        });
        if let (Some(_), Some(bus)) = (active, bus) {
            bus.issue(crate::net::SimCommand::Resign);
        }
        if let Some(net) = self.net.as_mut() {
            if to_room {
                net.request_back_to_room();
            } else {
                net.request_leave();
            }
        }
    }
}

/// What a match (or the demo behind the menu) is built from: the map
/// catalog, the dev overrides and the setup currently loaded, if any.
#[derive(SystemParam)]
struct MenuWorld<'w> {
    catalog: Res<'w, MapCatalog>,
    dev: Res<'w, DevOptions>,
    demo: Option<Res<'w, crate::game_setup::GameSetup>>,
}

/// The render settings as the Settings page draws them: the values and
/// the sample counts this GPU offers.
#[derive(SystemParam)]
struct SettingsView<'w> {
    render: Res<'w, RenderSettings>,
    msaa_support: Res<'w, MsaaSupport>,
}

fn handle_menu_actions(
    mut ev: MessageReader<MenuActionMessage>,
    mut state: MenuState,
    mut overlays: Overlays,
    mut flow: MenuFlow,
    mut net: NetLeave,
    world: MenuWorld,
    mut commands: Commands,
) {
    let MenuWorld { catalog, dev, demo } = &world;
    let MenuState {
        page,
        config,
        readme_scroll,
        render,
    } = &mut state;
    for msg in ev.read() {
        let action = msg.action;
        // Any config-affecting action invalidates the current page; the
        // maintain systems redraw it.
        match action {
            MenuAction::Goto(p) => {
                if p == MenuPage::Readme {
                    **readme_scroll = ReadmeScroll(0);
                }
                **page = p;
            }
            MenuAction::QuickStart(difficulty) => {
                config.difficulty = difficulty;
                config.grouping = Grouping::Duel;
                config.map = None; // weighted random, like RunRandomGame
                commands.insert_resource(build_setup(config, &catalog.names()));
                // No RunGame here — OnEnter(InGame) performs the single
                // prepare+load pass.
                flow.app_state.set(AppState::InGame);
            }
            MenuAction::StartSkirmish => {
                commands.insert_resource(build_setup(config, &catalog.names()));
                flow.app_state.set(AppState::InGame);
            }
            MenuAction::Restart => {
                // A replay starts over (`net::replay_log`).
                if let Some(view) = net.net.as_ref().and_then(|n| n.viewing.clone()) {
                    commands.insert_resource(crate::net::replay_log::NextReplay {
                        replay: view.replay,
                        name: view.name,
                    });
                }
                flow.run_game.write(RunGame);
                overlays.close_all();
            }
            // From a networked match the menu is leaving it: resign and
            // disconnect first, `finish_leaving` comes back here.
            MenuAction::GoToMenu | MenuAction::BackToRoom if net.connected() => {
                let to_room = action == MenuAction::BackToRoom;
                net.leave(flow.bus.as_deref_mut(), to_room);
                overlays.close_all();
            }
            MenuAction::GoToMenu | MenuAction::BackToRoom => {
                flow.app_state.set(AppState::Menu);
                overlays.close_all();
                // A match that handed its connection back opens its
                // room (`net::lobby` picks the connection up).
                **page = if net.back.is_some() {
                    MenuPage::Multiplayer
                } else {
                    MenuPage::Main
                };
                // Reload the attract-mode demo behind the menu (the real
                // match's world is torn down by the RunGame handler).
                commands.insert_resource(demo_setup(dev));
                flow.run_game.write(RunGame);
            }
            MenuAction::Resume => {
                *overlays.esc_open = EscMenuOpen(false);
                *overlays.esc_settings = EscSettingsOpen(false);
                *overlays.esc_audio = EscAudioOpen(false);
                *overlays.resign_confirm = EscResignConfirm(false);
            }
            MenuAction::Resign(false) => *overlays.resign_confirm = EscResignConfirm(true),
            MenuAction::Resign(true) => {
                if let Some(bus) = flow.bus.as_mut() {
                    bus.issue(crate::net::SimCommand::Resign);
                }
                overlays.close_all();
            }
            MenuAction::KeepPlaying => {
                overlays.dismissed.0 = true;
                flow.game_state.set(GameState::Playing);
                *overlays.game_over_open = GameOverOpen(false);
            }
            MenuAction::Quit => {
                // Through the app, not `process::exit`: on the web that
                // is `abort` (a trapped module, frozen canvas), and
                // natively it would skip destructors such as the
                // recorder's child process.
                flow.exit.write(AppExit::Success);
            }
            MenuAction::CycleYourFaction => {
                config.your_faction = next_faction(config.your_faction);
            }
            MenuAction::CycleEnemyFaction => {
                config.enemy_faction = next_faction(config.enemy_faction);
            }
            MenuAction::SetGrouping(g) => config.set_grouping(g),
            MenuAction::SetDifficulty(d) => config.set_difficulty(d),
            MenuAction::PickMap(i) => {
                config.map = Some(i);
                **page = MenuPage::AdvancedSkirmish;
            }
            MenuAction::PickRandomMap => {
                config.map = None;
                **page = MenuPage::AdvancedSkirmish;
            }
            MenuAction::CycleDemoMap(step) => {
                let names = catalog.names();
                if names.is_empty() {
                    continue;
                }
                // Step from the map the demo is on now; an unknown (or
                // absent) map starts the cycle at the first entry.
                let current = demo
                    .as_deref()
                    .and_then(|s| names.iter().position(|n| *n == s.map));
                let next = step_catalog_index(current, step, names.len());
                let mut setup = demo_setup(dev);
                setup.map = names[next].clone();
                commands.insert_resource(setup);
                flow.run_game.write(RunGame);
            }
            MenuAction::ScrollReadme(lines) => {
                readme_scroll.0 = (readme_scroll.0 as isize + lines as isize).max(0) as usize;
            }
            MenuAction::Showcase(faction) => {
                commands.insert_resource(showcase_setup(faction));
                flow.app_state.set(AppState::InGame);
            }
            MenuAction::EscSettings(open) => overlays.esc_settings.0 = open,
            MenuAction::EscAudio(open) => overlays.esc_audio.0 = open,
            // The render settings are applied to the camera and window
            // (and saved) by `rendering::settings::apply_render_settings`
            // on the next frame.
            MenuAction::SetMsaa(n) => render.msaa = n,
            MenuAction::SetBloom(on) => render.bloom = on,
            MenuAction::SetVsync(on) => render.vsync = on,
            MenuAction::SetFullscreen(on) => render.fullscreen = on,
            MenuAction::SetWindowSize(w, h) => {
                render.window_size = (w, h);
                render.fullscreen = false;
            }
            MenuAction::SetVolume(knob, v) => render.audio.set(knob, v),
            MenuAction::SetTips(on) => render.tips = on,
            MenuAction::Music(control) => {
                flow.music.write(control);
            }
            MenuAction::Lobby(action) => {
                flow.lobby.write(action);
            }
            MenuAction::Replays(action) => {
                flow.replays.write(action);
            }
            MenuAction::Stats(action) => overlays.stats.apply(action),
        }
    }
}

/// The catalog index `step` entries on from `current`, wrapping at both
/// ends. With no current index (the demo is on a map the catalog no
/// longer lists, or there is no demo), stepping forward starts at the
/// first map and stepping back at the last.
fn step_catalog_index(current: Option<usize>, step: i32, len: usize) -> usize {
    debug_assert!(len > 0);
    match current {
        Some(i) => (i as i32 + step).rem_euclid(len as i32) as usize,
        None if step < 0 => len - 1,
        None => 0,
    }
}

/// Faction cycle order from the original: System → Hacker → Network.
fn next_faction(f: Faction) -> Faction {
    Faction::from_team_id(f.index() + 1)
}

/// Read a `Val` percent, treating `Auto`/`Px` as 0. Used to order buttons
/// spatially for arrow-key navigation.
fn val_percent(v: &Val) -> f32 {
    match v {
        Val::Percent(p) => *p,
        _ => 0.0,
    }
}

/// Full keyboard navigation over every live menu button (launch menu,
/// Esc overlay, and game-over panel all spawn `MenuButton`s). Arrow keys
/// and Tab move focus, Shift+Tab/opposite arrows move it back, and Enter
/// or Space activates the focused button. In keyboard mode the focused
/// button is drawn brightened and every other one at its base colour,
/// overriding transient mouse hover, so keyboard users always see where
/// they are.
///
/// The spatially sorted button list is only built when a key moves the
/// focus or the focused button is gone (buttons respawn on page
/// changes), and the buttons are repainted only when the focus or the
/// input mode changed — not every frame.
fn keyboard_menu_nav(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Query<(Entity, &MenuButton, &Node)>,
    mut focus: ResMut<MenuFocus>,
    mut colors: Query<(Entity, &MenuButton, &mut BorderColor, &mut BackgroundColor)>,
    mut ev: MessageWriter<MenuActionMessage>,
    mut painted: Local<Option<(Entity, InputMode)>>,
) {
    if buttons.is_empty() {
        if focus.entity.is_some() {
            focus.entity = None;
        }
        *painted = None;
        return;
    }

    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let nav_key = keys.just_pressed(KeyCode::Tab)
        || keys.just_pressed(KeyCode::ArrowUp)
        || keys.just_pressed(KeyCode::ArrowDown)
        || keys.just_pressed(KeyCode::ArrowLeft)
        || keys.just_pressed(KeyCode::ArrowRight)
        || keys.just_pressed(KeyCode::Enter)
        || keys.just_pressed(KeyCode::Space);
    let confirm = keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space);

    let focus_live = focus.entity.is_some_and(|e| buttons.contains(e));
    if nav_key || !focus_live {
        // Build the spatially-ordered list of buttons currently on screen.
        let mut list: Vec<(Entity, &MenuButton, f32, f32)> = Vec::new();
        for (e, b, node) in &buttons {
            let x = if let Val::Auto = node.left {
                100.0 - val_percent(&node.right)
            } else {
                val_percent(&node.left)
            };
            list.push((e, b, val_percent(&node.top), x));
        }
        // Top-to-bottom, then left-to-right.
        list.sort_by(|a, b| {
            a.2.partial_cmp(&b.2)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal))
        });

        // Validate/resolve the focused index (defaults to the first button).
        let mut idx = list
            .iter()
            .position(|(e, _, _, _)| Some(*e) == focus.entity)
            .unwrap_or(0);

        if nav_key {
            focus.mode = InputMode::Keyboard;
            // Movement keys: Shift+Tab or Up/Left go back, Tab/Down/Right go on.
            if keys.just_pressed(KeyCode::Tab) && shift {
                idx = (idx + list.len() - 1) % list.len();
            } else if keys.just_pressed(KeyCode::Tab) {
                idx = (idx + 1) % list.len();
            } else if keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::ArrowLeft) {
                idx = (idx + list.len() - 1) % list.len();
            } else if keys.just_pressed(KeyCode::ArrowDown)
                || keys.just_pressed(KeyCode::ArrowRight)
            {
                idx = (idx + 1) % list.len();
            }
        }

        if focus.entity != Some(list[idx].0) {
            focus.entity = Some(list[idx].0);
        }

        // Confirm the focused button.
        if confirm {
            ev.write(MenuActionMessage {
                action: list[idx].1.action,
            });
        }
    }

    // Paint only in keyboard mode, so mouse hover keeps working untouched,
    // and only when the focus or the mode changed since the last paint.
    if focus.mode == InputMode::Keyboard
        && let Some(fidx) = focus.entity
        && *painted != Some((fidx, InputMode::Keyboard))
    {
        *painted = Some((fidx, InputMode::Keyboard));
        for (e, b, mut bc, mut bg) in &mut colors {
            let c = if e == fidx { brighten(b.base) } else { b.base };
            *bc = border(c);
            *bg = fill(c);
        }
    } else if focus.mode != InputMode::Keyboard {
        *painted = None;
    }
}

// ---------------------------------------------------------------------------
// Launch menu (AppState::Menu)
// ---------------------------------------------------------------------------

/// Mouse interaction for every live menu button (launch menu, Esc overlay,
/// game-over panel). `bevy_picking` is not relied on here: it needs the
/// `bevy_picking` feature AND its pointer pipeline does not populate in this
/// app's runtime, so we hit-test directly against `window.cursor_position()`
/// (the same source the RTS selection/placement systems already use) and the
/// node's computed rect. Hover brightens the button under the cursor; a
/// primary click on a button dispatches its [`MenuAction`]. Any press on
/// a button is consumed, so it can't also deselect or order the units
/// behind the game-over panel. The Esc overlay (and its Settings page)
/// is modal: while it is open every mouse press is consumed, wherever
/// it lands — no deselect, no order, no HUD button, no camera drag.
/// Releases pass through, so a drag begun before the overlay opened
/// still ends cleanly.
#[allow(clippy::too_many_arguments)]
fn mouse_menu_input(
    buttons: Query<(
        Entity,
        &MenuButton,
        &ComputedNode,
        &ComputedStackIndex,
        &UiGlobalTransform,
    )>,
    windows: Query<&Window>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    esc: (
        Res<EscMenuOpen>,
        Res<EscSettingsOpen>,
        Option<Res<stats_page::StatsView>>,
    ),
    mut focus: ResMut<MenuFocus>,
    mut hovered: Local<Option<Entity>>,
    mut colors: Query<(&MenuButton, &mut BorderColor, &mut BackgroundColor)>,
    mut ev: MessageWriter<MenuActionMessage>,
) {
    const BUTTONS: [MouseButton; 3] = [MouseButton::Left, MouseButton::Right, MouseButton::Middle];
    let consume = |mouse: &mut ButtonInput<MouseButton>| {
        for button in BUTTONS {
            if mouse.just_pressed(button) {
                mouse.clear_just_pressed(button);
            }
        }
    };
    let modal = esc.0.0 || esc.1.0 || esc.2.is_some_and(|v| v.open);
    let cursor = windows
        .single()
        .ok()
        .and_then(|w| Some((w.cursor_position()?, w)));
    let Some((cursor, window)) = cursor else {
        if modal {
            consume(&mut mouse);
        }
        return;
    };
    // `ComputedNode::size`/`UiGlobalTransform` are in physical pixels, whereas
    // `cursor_position()` is logical — convert so the hit test is in one space.
    let phys = cursor * window.scale_factor();

    // Find the topmost button under the cursor using the UI node's canonical
    // hit test. Prefer the deepest node (larger stack index).
    let mut hit: Option<Entity> = None;
    let mut hit_stack = 0u32;
    for (e, _b, cnode, stack, gtf) in &buttons {
        if cnode.contains_point(*gtf, phys) {
            let idx = stack.0;
            if idx >= hit_stack {
                hit_stack = idx;
                hit = Some(e);
            }
        }
    }

    // If the hovered button changed, repaint the visual state.
    if *hovered != hit {
        if let Some(prev) = *hovered
            && let Ok((b, mut bc, mut bg)) = colors.get_mut(prev)
        {
            *bc = border(b.base);
            *bg = fill(b.base);
        }
        *hovered = hit;
        if let Some(cur) = hit
            && let Ok((b, mut bc, mut bg)) = colors.get_mut(cur)
        {
            *bc = border(brighten(b.base));
            *bg = fill(brighten(b.base));
        }
    }

    if let Some(ent) = hit {
        // Entering/keeping mouse mode: the keyboard focus no longer paints.
        if focus.entity != Some(ent) {
            focus.entity = Some(ent);
            focus.mode = InputMode::Mouse;
        }
        if mouse.just_pressed(MouseButton::Left)
            && let Ok((_, b, ..)) = buttons.get(ent)
        {
            ev.write(MenuActionMessage { action: b.action });
        }
    }
    if hit.is_some() || modal {
        consume(&mut mouse);
    }
}

/// What the launch-menu pages draw from.
#[derive(SystemParam)]
struct LaunchMenuInputs<'w> {
    catalog: Res<'w, MapCatalog>,
    config: Res<'w, SkirmishConfig>,
    readme: Res<'w, ReadmeScroll>,
    settings: SettingsView<'w>,
    /// The attract-mode demo behind the menu; the main page names its map.
    demo: Option<Res<'w, crate::game_setup::GameSetup>>,
    /// The music player; the main page's bottom-left cluster drives it.
    music: Res<'w, MusicPlayer>,
    music_catalog: Res<'w, MusicCatalog>,
    /// The Multiplayer page's room.
    lobby: Res<'w, crate::net::lobby::LobbyRoom>,
    /// The Replays page's list.
    shelf: Res<'w, replays_page::ReplayShelf>,
    /// The room's chat, beside the seats.
    chat: Res<'w, crate::net::chat::ChatLog>,
    /// The map selector's turntable.
    preview: Res<'w, map_preview::MapPreview>,
}

fn maintain_launch_menu(
    page: Res<MenuPage>,
    mut commands: Commands,
    existing_root: Query<Entity, With<MenuRoot>>,
    windows: Query<&Window>,
    mut last_page: Local<Option<MenuPage>>,
    inputs: LaunchMenuInputs,
) {
    let LaunchMenuInputs {
        catalog,
        config,
        readme,
        settings,
        demo,
        music,
        music_catalog,
        lobby,
        shelf,
        chat,
        preview,
    } = &inputs;
    // A page redraws when it is entered, and when the configuration it
    // shows (the skirmish setup's choice rows, the render settings, the
    // readme scroll, the demo's map) changed under it.
    let stale = config.is_changed()
        || settings.render.is_changed()
        || readme.is_changed()
        || demo.as_ref().is_some_and(|d| d.is_changed())
        || (*page == MenuPage::Multiplayer && (lobby.is_changed() || chat.is_changed()))
        || (*page == MenuPage::Replays && shelf.is_changed());
    if last_page.is_some() && *last_page == Some(*page) && !existing_root.is_empty() && !stale {
        return;
    }
    *last_page = Some(*page);
    for e in &existing_root {
        commands.entity(e).despawn();
    }

    let Ok(window) = windows.single() else {
        return;
    };
    let (title_size, menu_size, page_size, _) = vsizes(window.height());

    let root = spawn_backdrop(&mut commands, MENU_GLASS);
    match *page {
        MenuPage::Main => {
            let demo_map = demo.as_deref().filter(|d| d.demo).map(|d| d.map.as_str());
            main_menu_page(
                &mut commands,
                root,
                title_size,
                menu_size,
                demo_map,
                (music, music_catalog),
            )
        }
        MenuPage::QuickSkirmish => quick_skirmish_page(&mut commands, root, page_size),
        MenuPage::AdvancedSkirmish => {
            advanced_skirmish_page(&mut commands, root, page_size, config, &catalog.names())
        }
        MenuPage::MapList => map_list_page(
            &mut commands,
            root,
            (window.width(), window.height()),
            catalog,
            preview,
        ),
        MenuPage::Showcase => showcase_page(&mut commands, root, page_size),
        MenuPage::Credits => credits_page(&mut commands, root, (window.width(), page_size)),
        MenuPage::Readme => readme_page(&mut commands, root, window.height(), readme.0),
        MenuPage::Settings => settings_page(
            &mut commands,
            root,
            (window.width(), page_size),
            settings,
            MenuAction::Goto(MenuPage::Audio),
            MenuAction::Goto(MenuPage::Main),
        ),
        MenuPage::Audio => audio_page(
            &mut commands,
            root,
            (window.width(), page_size),
            settings,
            MenuAction::Goto(MenuPage::Settings),
        ),
        MenuPage::Multiplayer => lobby_page::multiplayer_page(
            &mut commands,
            root,
            (window.width(), page_size),
            lobby,
            chat,
        ),
        MenuPage::Replays => {
            replays_page::replays_page(&mut commands, root, (window.width(), page_size), shelf)
        }
    }
}

/// The original's zigzag main menu (`MainMenu`): `vsy/20` buttons whose
/// bottom corners alternate between ending at x=46% (`rb`) and starting
/// at x=54% (`lb`), stepping down 10% of the screen per button.
///
/// `demo_map` is the map the attract-mode demo is playing behind the
/// menu, named in the bottom-right corner between small `<` / `>`
/// buttons that cycle it through the catalog. The bottom-left corner
/// holds the music cluster.
fn main_menu_page(
    commands: &mut Commands,
    root: Entity,
    title_size: f32,
    menu_size: f32,
    demo_map: Option<&str>,
    music: (&MusicPlayer, &MusicCatalog),
) {
    title(commands, root, title_size);
    demo_map_cycler(commands, root, menu_size * 0.5, demo_map);
    music_controls(commands, root, menu_size * 0.5, music.0, music.1);
    // No Quit on the web: a page has nothing to quit to.
    let mut entries: Vec<(&str, Color, MenuAction)> = vec![
        (
            "Skirmish",
            BUTTON_GREEN,
            MenuAction::Goto(MenuPage::AdvancedSkirmish),
        ),
        (
            "Quick Battle",
            BUTTON_GREEN,
            MenuAction::Goto(MenuPage::QuickSkirmish),
        ),
        (
            "Multiplayer",
            BUTTON_GREEN,
            MenuAction::Goto(MenuPage::Multiplayer),
        ),
        ("Replays", BUTTON_GREEN, MenuAction::Goto(MenuPage::Replays)),
        ("Showcase", EASY_CYAN, MenuAction::Goto(MenuPage::Showcase)),
        ("Credits", BUTTON_GREEN, MenuAction::Goto(MenuPage::Credits)),
        ("Readme", BUTTON_GREEN, MenuAction::Goto(MenuPage::Readme)),
        (
            "Settings",
            BUTTON_GREEN,
            MenuAction::Goto(MenuPage::Settings),
        ),
    ];
    if !cfg!(target_arch = "wasm32") {
        entries.push(("Quit", BUTTON_GREEN, MenuAction::Quit));
    }
    // 10 % apart, closer when eight or nine plates must fit above
    // y=10 %.
    let step = (0.6 / (entries.len() - 1) as f32).det_min(0.1);
    for (i, (name, color, action)) in entries.into_iter().enumerate() {
        let y = 0.7 - step * i as f32;
        let (x, anchor) = if i % 2 == 0 {
            (0.46, Anchor::Rb)
        } else {
            (0.54, Anchor::Lb)
        };
        ButtonSpec::new(name, color, menu_size, (x, y), anchor, action).spawn(commands, root);
    }
}

/// Bottom-right corner of the main page: `< MapName >`. The arrows
/// restart the demo behind the menu on the previous / next catalog map,
/// so the backdrop can be browsed without starting a match.
fn demo_map_cycler(commands: &mut Commands, root: Entity, font_size: f32, demo_map: Option<&str>) {
    let font_size = readable(font_size);
    // Right-to-left from the corner: `>` hangs off x=99%, the name off
    // the arrow's left edge, `<` off the name's. Absolute nodes don't
    // know each other's widths, so the three sit in one row node
    // anchored at the corner instead.
    let (mut node, transform) = anchored(0.99, 0.02, Anchor::Rb);
    node.flex_direction = FlexDirection::Row;
    node.align_items = AlignItems::Center;
    node.column_gap = Val::Px(font_size * 0.4);
    let row = commands.spawn((node, transform, Pickable::IGNORE)).id();
    commands.entity(root).add_child(row);

    let arrow = |commands: &mut Commands, label: &str, step: i32| {
        let text = spawn_button_text(commands, label, font_size);
        let node = Node {
            padding: frame_padding(font_size),
            border: frame_border(font_size),
            justify_content: JustifyContent::Center,
            ..default()
        };
        commands
            .spawn((
                MenuButton {
                    action: MenuAction::CycleDemoMap(step),
                    base: MAP_GREEN,
                },
                node,
                fill(MAP_GREEN),
                border(MAP_GREEN),
            ))
            .add_child(text)
            .id()
    };
    let prev = arrow(commands, "<", -1);
    let name = commands
        .spawn((
            Text::new(
                demo_map
                    .map(crate::map_loading::display_name)
                    .unwrap_or_default(),
            ),
            TextColor(TEXT_WHITE),
            TextFont {
                font_size: FontSize::Px(font_size),
                ..default()
            },
            TextLayout::new(Justify::Center, LineBreak::NoWrap),
            Pickable::IGNORE,
        ))
        .id();
    let next = arrow(commands, ">", 1);
    commands.entity(row).add_children(&[prev, name, next]);
}

/// The track name in the music cluster, rewritten in place by
/// [`music_readout`].
#[derive(Component)]
struct MusicTrackReadout;

/// The cluster's play / pause button; its label follows the player.
#[derive(Component)]
struct MusicPauseButton;

/// The play / pause button shows what a press will do.
fn pause_label(paused: bool) -> &'static str {
    if paused { ">" } else { "||" }
}

/// Bottom-left corner: `|<  >/||  >|  Artist - Title`. Previous, pause /
/// play and next for the music player, which runs behind the menu as
/// well as in a match (`audio::music`). Only the texts change
/// afterwards, in place, so the page is not redrawn for it.
fn music_controls(
    commands: &mut Commands,
    root: Entity,
    font_size: f32,
    player: &MusicPlayer,
    catalog: &MusicCatalog,
) {
    let font_size = readable(font_size);
    let (mut node, transform) = anchored(0.01, 0.02, Anchor::Lb);
    node.flex_direction = FlexDirection::Row;
    node.align_items = AlignItems::Center;
    node.column_gap = Val::Px(font_size * 0.4);
    let row = commands.spawn((node, transform, Pickable::IGNORE)).id();
    commands.entity(root).add_child(row);

    let button = |commands: &mut Commands, label: &str, control: MusicControl| {
        let text = spawn_button_text(commands, label, font_size);
        let node = Node {
            padding: frame_padding(font_size),
            border: frame_border(font_size),
            justify_content: JustifyContent::Center,
            ..default()
        };
        commands
            .spawn((
                MenuButton {
                    action: MenuAction::Music(control),
                    base: TEAL,
                },
                node,
                fill(TEAL),
                border(TEAL),
            ))
            .add_child(text)
            .id()
    };
    let prev = button(commands, "|<", MusicControl::Previous);
    let pause = button(
        commands,
        pause_label(player.paused),
        MusicControl::TogglePause,
    );
    commands.entity(pause).insert(MusicPauseButton);
    let next = button(commands, ">|", MusicControl::Next);
    let name = commands
        .spawn((
            MusicTrackReadout,
            Text::new(player.track_label(catalog)),
            TextColor(TEXT_WHITE),
            TextFont {
                font_size: FontSize::Px(font_size),
                ..default()
            },
            TextLayout::new(Justify::Left, LineBreak::NoWrap),
            Pickable::IGNORE,
        ))
        .id();
    commands
        .entity(row)
        .add_children(&[prev, pause, next, name]);
}

/// Keep the music cluster's texts current: the track name and the
/// pause button's label. Written only when they change, like
/// [`fps_readout`].
fn music_readout(
    player: Res<MusicPlayer>,
    catalog: Res<MusicCatalog>,
    mut names: Query<&mut Text, With<MusicTrackReadout>>,
    toggles: Query<&Children, With<MusicPauseButton>>,
    mut texts: Query<&mut Text, Without<MusicTrackReadout>>,
) {
    let label = player.track_label(&catalog);
    for mut text in &mut names {
        if text.0 != label {
            text.0.clone_from(&label);
        }
    }
    let wanted = pause_label(player.paused);
    for children in &toggles {
        let mut texts = texts.iter_many_mut(children).matched();
        while let Some(mut text) = texts.fetch_next() {
            if text.0 != wanted {
                text.0 = wanted.to_string();
            }
        }
    }
}

/// Page heading in the original's style: a blue plate at the top
/// centre (`AddFrame("Kernel Panic!\n<page>", {x=0.5, y=0.9}, vsy/24, {0,0,1}, "cc")`).
fn page_heading(commands: &mut Commands, root: Entity, page_size: f32, text: &str) {
    LabelSpec::new(
        &format!("Kernel Panic!\n{text}"),
        page_size,
        (0.5, 0.9),
        Anchor::Cc,
    )
    .frame(NAV_BLUE)
    .spawn(commands, root);
}

/// Pick a faction to see one of each of its units and buildings built
/// live on Data_Cache_L1. Laid out like the quick-battle column.
fn showcase_page(commands: &mut Commands, root: Entity, page_size: f32) {
    page_heading(commands, root, page_size, "Showcase");
    LabelSpec::new(
        "Pick a faction to see its full unit tree built live",
        page_size * 0.7,
        (0.5, 0.72),
        Anchor::Cc,
    )
    .spawn(commands, root);
    centre_column(
        commands,
        root,
        page_size,
        [
            ("System", Faction::System),
            ("Hacker", Faction::Hacker),
            ("Network", Faction::Network),
        ]
        .map(|(name, f)| (name, faction_color(f), MenuAction::Showcase(f))),
    );
    back_button(page_size, (0.5, 0.2), MenuPage::Main).spawn(commands, root);
}

/// One option of a Settings row: label, action, whether it is the
/// current pick.
type Choice = (String, MenuAction, bool);

/// The volumes the Audio page offers, in percent (KP's installer
/// defaults everything to 100).
const VOLUME_CHOICES: &[u8] = &[0, 20, 40, 60, 80, 100];

/// The Settings page: one choice row per render setting, drawn like the
/// advanced page's grouping / difficulty rows (the current pick
/// brightened), each taking effect on the next frame. `back` is the
/// row's exit: the main page in the launch menu, the Esc overlay in a
/// match.
fn settings_page(
    commands: &mut Commands,
    root: Entity,
    (window_width, page_size): (f32, f32),
    settings: &SettingsView,
    audio: MenuAction,
    back: MenuAction,
) {
    let SettingsView {
        render,
        msaa_support,
    } = settings;
    page_heading(commands, root, page_size, "Settings");
    // Frame rate, top-right beside the heading, so a setting's cost
    // shows as it is toggled. Drawn blank; `fps_readout` fills it in.
    let readout = LabelSpec::new(&fps_text(None), page_size * 0.85, (0.97, 0.9), Anchor::Rc)
        .frame(NAV_BLUE)
        .spawn(commands, root);
    commands.entity(readout).insert(FpsReadout);

    let msaa_label = |n: u8| match n {
        1 => "Off".to_string(),
        n => format!("{n}x"),
    };
    let size_label = |(w, h): (u32, u32)| format!("{w}x{h}");
    let msaa: Vec<Choice> = msaa_support
        .0
        .iter()
        .map(|&n| (msaa_label(n), MenuAction::SetMsaa(n), render.msaa == n))
        .collect();
    let sizes: Vec<Choice> = WINDOW_SIZES
        .iter()
        .map(|&(w, h)| {
            (
                size_label((w, h)),
                MenuAction::SetWindowSize(w, h),
                !render.fullscreen && render.window_size == (w, h),
            )
        })
        .collect();
    let on_off = |action: fn(bool) -> MenuAction, on: bool| {
        vec![
            ("On".to_string(), action(true), on),
            ("Off".to_string(), action(false), !on),
        ]
    };
    let display = vec![
        (
            "Fullscreen".to_string(),
            MenuAction::SetFullscreen(true),
            render.fullscreen,
        ),
        (
            "Windowed".to_string(),
            MenuAction::SetFullscreen(false),
            !render.fullscreen,
        ),
    ];

    // Web: the canvas is sized by the page, so no window rows.
    let rows: Vec<(&str, Color, Vec<Choice>)> = if cfg!(target_arch = "wasm32") {
        vec![
            ("Antialiasing", EASY_CYAN, msaa),
            (
                "Bloom",
                MEDIUM_GREEN,
                on_off(MenuAction::SetBloom, render.bloom),
            ),
            (
                "VSync",
                HARD_YELLOW,
                on_off(MenuAction::SetVsync, render.vsync),
            ),
            ("Tips", TEAL, on_off(MenuAction::SetTips, render.tips)),
        ]
    } else {
        vec![
            ("Antialiasing", EASY_CYAN, msaa),
            (
                "Bloom",
                MEDIUM_GREEN,
                on_off(MenuAction::SetBloom, render.bloom),
            ),
            (
                "VSync",
                HARD_YELLOW,
                on_off(MenuAction::SetVsync, render.vsync),
            ),
            ("Display", EXTREME_ORANGE, display),
            ("Window", MAP_GREEN, sizes),
            ("Tips", TEAL, on_off(MenuAction::SetTips, render.tips)),
        ]
    };
    choice_rows(commands, root, window_width, page_size, rows);
    ButtonSpec::new("Audio", TEAL, page_size, (0.5, 0.26), Anchor::Cc, audio)
        .min_width(12.0)
        .spawn(commands, root);
    ButtonSpec::new("Back", NAV_BLUE, page_size, (0.5, 0.15), Anchor::Cc, back)
        .min_width(12.0)
        .spawn(commands, root);
    LabelSpec::new(
        "Changes apply at once and are kept for next time",
        page_size * 0.6,
        (0.5, 0.06),
        Anchor::Cc,
    )
    .spawn(commands, root);
}

/// The frame-rate label's text: the smoothed rate to the frame, or a
/// dash before the first measurement.
/// The Audio page: one row per mixer knob (Spring's `snd_vol*` settings),
/// in percent, laid out like the Settings page.
fn audio_page(
    commands: &mut Commands,
    root: Entity,
    (window_width, page_size): (f32, f32),
    settings: &SettingsView,
    back: MenuAction,
) {
    let audio = settings.render.audio;
    page_heading(commands, root, page_size, "Audio");
    let colors = [
        EASY_CYAN,
        MEDIUM_GREEN,
        HARD_YELLOW,
        EXTREME_ORANGE,
        MAP_GREEN,
        TEAL,
    ];
    let rows: Vec<(&str, Color, Vec<Choice>)> = VolumeKnob::ALL
        .iter()
        .zip(colors)
        .map(|(&knob, color)| {
            let choices = VOLUME_CHOICES
                .iter()
                .map(|&v| {
                    let label = if v == 0 {
                        "Off".to_string()
                    } else {
                        format!("{v}%")
                    };
                    (label, MenuAction::SetVolume(knob, v), audio.get(knob) == v)
                })
                .collect();
            (knob.label(), color, choices)
        })
        .collect();
    choice_rows(commands, root, window_width, page_size, rows);
    ButtonSpec::new("Back", NAV_BLUE, page_size, (0.5, 0.15), Anchor::Cc, back)
        .min_width(12.0)
        .spawn(commands, root);
    LabelSpec::new(
        "Master scales every channel;\nclick the tip box in a match to mute the tips voice",
        page_size * 0.6,
        (0.5, 0.06),
        Anchor::Cc,
    )
    .spawn(commands, root);
}

/// Where [`choice_rows`] puts its first row, and the lowest its last
/// row may sit (five rows at the natural spacing end there).
const FIRST_ROW_Y: f32 = 0.72;
const LAST_ROW_Y: f32 = 0.34;

/// The gap between [`choice_rows`] rows: 9.5 %, squeezed to fit.
fn row_spacing(rows: usize) -> f32 {
    if rows <= 1 {
        0.095
    } else {
        ((FIRST_ROW_Y - LAST_ROW_Y) / (rows - 1) as f32).det_min(0.095)
    }
}

/// The choices of a [`choice_rows`] row run from [`choices_start`] to
/// this x; the row labels sit at [`LABEL_X`] on the left.
const CHOICES_END: f32 = 0.98;
const LABEL_X: f32 = 0.02;
/// The choices start no further left than this (80 % of the width),
/// and further right only when a row label needs the room.
const CHOICES_MIN_START: f32 = 0.18;

/// Where a page's choices start: past its longest label (`glyph` wide
/// per character, plus the frame's padding and a gap).
fn choices_start(longest_label: usize, glyph: f32) -> f32 {
    (LABEL_X + (longest_label as f32 + 3.0) * glyph).det_max(CHOICES_MIN_START)
}

/// Draw labelled rows of mutually exclusive choices, the current pick
/// brightened. Row labels on the left at [`LABEL_X`], the choices
/// stepping right from [`choices_start`] in even boxes sized to the
/// row's longest choice, never wider than what fits before
/// [`CHOICES_END`] (six volume steps on a wide window used to run off
/// the screen, and a long label off the left edge of a small one);
/// rows 9.5% apart down from y=72%, closer when there are more than
/// five, so the last stays at [`LAST_ROW_Y`] and the buttons under
/// them keep their place. A monospace glyph is ~0.6 em
/// wide, plus the frame's padding.
fn choice_rows(
    commands: &mut Commands,
    root: Entity,
    window_width: f32,
    page_size: f32,
    rows: Vec<(&str, Color, Vec<Choice>)>,
) {
    let size = readable(page_size * 0.85);
    let glyph = 0.6 * size / window_width;
    let longest_label = rows
        .iter()
        .map(|(name, _, _)| name.chars().count())
        .max()
        .unwrap_or(0);
    let start = choices_start(longest_label, glyph);
    let spacing = row_spacing(rows.len());
    for (i, (name, color, choices)) in rows.into_iter().enumerate() {
        let y = FIRST_ROW_Y - spacing * i as f32;
        let longest = choices
            .iter()
            .map(|(text, _, _)| text.chars().count())
            .max()
            .unwrap_or(0);
        let step = choice_step(longest, glyph, choices.len(), start);
        LabelSpec::new(name, size, (LABEL_X, y), Anchor::Lc)
            .frame(color)
            .spawn(commands, root);
        for (j, (text, action, chosen)) in choices.into_iter().enumerate() {
            ButtonSpec::new(
                &text,
                color,
                size,
                (start + step * j as f32, y),
                Anchor::Lc,
                action,
            )
            .min_width(100.0 * (step - 0.01))
            .chosen(chosen)
            .spawn(commands, root);
        }
    }
}

/// The x step (window fraction) between a row's `count` choice boxes
/// from `start`: the longest choice plus padding at `glyph` width each,
/// at least 12 %, and never more than an even share of the span left
/// before [`CHOICES_END`].
fn choice_step(longest: usize, glyph: f32, count: usize, start: f32) -> f32 {
    let natural = ((longest as f32 + 2.5) * glyph).det_max(0.12);
    natural.det_min((CHOICES_END - start) / count.max(1) as f32)
}

fn fps_text(fps: Option<f64>) -> String {
    match fps {
        Some(fps) => format!("{} fps", fps.round() as u32),
        None => "-- fps".to_string(),
    }
}

/// Keep the Settings page's [`FpsReadout`] current. Only the whole
/// number is shown, and the text is only written when it changes, so
/// the label does not re-layout every frame.
fn fps_readout(
    diagnostics: Res<DiagnosticsStore>,
    readouts: Query<&Children, With<FpsReadout>>,
    mut texts: Query<&mut Text>,
) {
    let fps = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.smoothed());
    let wanted = fps_text(fps);
    for children in &readouts {
        let mut texts = texts.iter_many_mut(children).matched();
        while let Some(mut text) = texts.fetch_next() {
            if text.0 != wanted {
                text.0.clone_from(&wanted);
            }
        }
    }
}

/// Blue `Back` plate, centred on `pos`.
fn back_button(font_size: f32, pos: (f32, f32), to: MenuPage) -> ButtonSpec<'static> {
    ButtonSpec::new(
        "Back",
        NAV_BLUE,
        font_size,
        pos,
        Anchor::Cc,
        MenuAction::Goto(to),
    )
}

/// The original's `SimplerSinglePlayer`: a centred column of difficulty
/// buttons that each start a random-map Duel at once. The heading
/// plate toggles to the advanced page, like the original's.
fn quick_skirmish_page(commands: &mut Commands, root: Entity, page_size: f32) {
    ButtonSpec::new(
        "Kernel Panic!\nSingle Player",
        NAV_BLUE,
        page_size,
        (0.5, 0.8),
        Anchor::Cc,
        MenuAction::Goto(MenuPage::AdvancedSkirmish),
    )
    .min_width(24.0)
    .spawn(commands, root);
    centre_column(
        commands,
        root,
        page_size,
        [
            ("Easy", MEDIUM_GREEN, MenuAction::QuickStart(1)),
            ("Medium", HARD_YELLOW, MenuAction::QuickStart(2)),
            ("Hard", EXTREME_ORANGE, MenuAction::QuickStart(3)),
            ("Very Hard", VERY_HARD_RED, MenuAction::QuickStart(4)),
        ],
    );
    back_button(page_size, (0.5, 0.2), MenuPage::Main).spawn(commands, root);
    LabelSpec::new(
        "Click the heading for the advanced setup",
        page_size * 0.6,
        (0.5, 0.1),
        Anchor::Cc,
    )
    .spawn(commands, root);
}

/// The original's `SinglePlayer` advanced page, at its coordinates:
/// heading, map and the two faction plates down the centre, grouping
/// presets on the left (`lc` at x=10%), difficulty on the right (`rc`
/// at x=90%), Run!/Back side by side, description line at the bottom.
fn advanced_skirmish_page(
    commands: &mut Commands,
    root: Entity,
    page_size: f32,
    config: &SkirmishConfig,
    map_names: &[String],
) {
    ButtonSpec::new(
        "Kernel Panic!\nSingle Player",
        NAV_BLUE,
        page_size,
        (0.5, 0.9),
        Anchor::Cc,
        MenuAction::Goto(MenuPage::QuickSkirmish),
    )
    .min_width(24.0)
    .spawn(commands, root);

    let map_name = match config.map.and_then(|i| map_names.get(i)) {
        Some(stem) => crate::map_loading::display_name(stem),
        None => "random".to_string(),
    };
    ButtonSpec::new(
        &format!("Map: {map_name}"),
        MAP_GREEN,
        page_size,
        (0.5, 0.75),
        Anchor::Cc,
        MenuAction::Goto(MenuPage::MapList),
    )
    .min_width(24.0)
    .spawn(commands, root);

    for (text, y, action) in [
        (
            format!("You:\n{:?}", config.your_faction),
            0.6,
            MenuAction::CycleYourFaction,
        ),
        (
            format!("Enemy:\n{:?}", config.enemy_faction),
            0.45,
            MenuAction::CycleEnemyFaction,
        ),
    ] {
        ButtonSpec::new(&text, EASY_CYAN, page_size, (0.5, y), Anchor::Cc, action)
            .min_width(24.0)
            .spawn(commands, root);
    }

    // Grouping presets: left column, in the original's order. (Spectate
    // and Heroic are not implemented yet — see `Grouping`.)
    for (i, (g, color)) in [
        (Grouping::Duel, MEDIUM_GREEN),
        (Grouping::Team, HARD_YELLOW),
        (Grouping::Outgunned, EXTREME_ORANGE),
    ]
    .into_iter()
    .enumerate()
    {
        ButtonSpec::new(
            g.label(),
            color,
            page_size,
            (0.1, 0.55 - 0.1 * i as f32),
            Anchor::Lc,
            MenuAction::SetGrouping(g),
        )
        .min_width(16.0)
        .chosen(config.grouping == g)
        .spawn(commands, root);
    }

    // Difficulty: right column.
    for (i, (name, color)) in DIFFICULTIES.into_iter().enumerate() {
        let difficulty = 1 + i as u8;
        ButtonSpec::new(
            name,
            color,
            page_size,
            (0.9, 0.6 - 0.1 * i as f32),
            Anchor::Rc,
            MenuAction::SetDifficulty(difficulty),
        )
        .min_width(16.0)
        .chosen(config.difficulty == difficulty)
        .spawn(commands, root);
    }

    ButtonSpec::new(
        "Run!",
        NAV_BLUE,
        page_size,
        (0.4, 0.2),
        Anchor::Cc,
        MenuAction::StartSkirmish,
    )
    .min_width(12.0)
    .spawn(commands, root);
    back_button(page_size, (0.6, 0.2), MenuPage::Main)
        .min_width(12.0)
        .spawn(commands, root);

    LabelSpec::new(
        &describe_setup(config),
        page_size * 0.8,
        (0.5, 0.05),
        Anchor::Cc,
    )
    .frame(DESC_BLUE)
    .spawn(commands, root);
}

/// The map selector: the maps down a sidebar on the left (two columns
/// where the window is too short for one), the one under the pointer
/// turning in the main area ([`map_preview`]) with its name, players,
/// size and description beneath. Random map and Back close the sidebar.
fn map_list_page(
    commands: &mut Commands,
    root: Entity,
    (window_w, window_h): (f32, f32),
    catalog: &MapCatalog,
    preview: &map_preview::MapPreview,
) {
    const TOP: f32 = 0.88;
    const BOTTOM: f32 = 0.15;
    const MARGIN: f32 = 0.02;
    let mut entries: Vec<(usize, String)> = catalog
        .names()
        .iter()
        .enumerate()
        .map(|(i, stem)| (i, crate::map_loading::display_name(stem)))
        .collect();
    entries.sort_by_key(|(_, name)| name.to_lowercase());
    let longest = entries
        .iter()
        .map(|(_, e)| e.chars().count())
        .max()
        .unwrap_or(1) as f32;
    let gap = 0.01 * window_w;
    // One column or two, whichever reads larger: a plate is 1.5 em plus
    // its border (keep a fifth of each row free), and the sidebar keeps
    // to `SIDEBAR` of the width so the turntable has the rest.
    const SIDEBAR: f32 = 0.46;
    let font_for = |columns: usize| {
        let rows = entries.len().div_ceil(columns).max(1) as f32;
        let by_rows = 0.8 * (TOP - BOTTOM) * window_h / rows / 1.7;
        let column = (SIDEBAR * window_w - (columns - 1) as f32 * gap) / columns as f32;
        let by_width = column / (0.6 * longest + 1.4);
        (window_h / 28.0).det_min(by_rows).det_min(by_width)
    };
    let columns = if font_for(2) > font_for(1) { 2 } else { 1 };
    let rows = entries.len().div_ceil(columns).max(1);
    let font = readable(font_for(columns));
    let column_w = (0.6 * longest + 1.4) * font;
    let margin = MARGIN * window_w;
    let sidebar_w = columns as f32 * column_w + (columns - 1) as f32 * gap;
    let step = (TOP - BOTTOM) / rows as f32;

    let nav = window_h / 24.0;
    LabelSpec::new(
        "Choose a map:",
        nav,
        ((margin + sidebar_w * 0.5) / window_w, 0.95),
        Anchor::Cc,
    )
    .frame(NAV_BLUE)
    .spawn(commands, root);
    for (slot, (i, name)) in entries.iter().enumerate() {
        let (col, row) = (slot / rows, slot % rows);
        let left = margin + col as f32 * (column_w + gap);
        ButtonSpec::new(
            name,
            BUTTON_GREEN,
            font,
            (left / window_w, TOP - step * (row as f32 + 0.5)),
            Anchor::Lc,
            MenuAction::PickMap(*i),
        )
        .min_width_px(column_w)
        .spawn(commands, root);
    }
    // Random map and Back share the sidebar's foot.
    let foot = nav
        .det_min(font * 1.5)
        .det_min(sidebar_w / ((10.0 + 4.0) * 0.6 + 4.0));
    ButtonSpec::new(
        "Random map",
        TEAL,
        foot,
        (MARGIN, 0.06),
        Anchor::Lc,
        MenuAction::PickRandomMap,
    )
    .spawn(commands, root);
    ButtonSpec::new(
        "Back",
        NAV_BLUE,
        foot,
        ((margin + sidebar_w) / window_w, 0.06),
        Anchor::Rc,
        MenuAction::Goto(MenuPage::AdvancedSkirmish),
    )
    .spawn(commands, root);

    // The turntable, 16:10, as large as the main area allows.
    let area_left = margin + sidebar_w + 2.0 * gap;
    let area_w = window_w - area_left - margin;
    let ratio = map_preview::TARGET_SIZE.x as f32 / map_preview::TARGET_SIZE.y as f32;
    let image_w = area_w.det_min(0.68 * window_h * ratio);
    let image_h = image_w / ratio;
    let image_left = area_left + (area_w - image_w) * 0.5;
    let image_top = 0.05 * window_h;
    let border = frame_border_px(nav);
    let image = commands
        .spawn((
            MenuFrame,
            Pickable::IGNORE,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(image_left),
                top: Val::Px(image_top),
                width: Val::Px(image_w),
                height: Val::Px(image_h),
                border: UiRect::all(Val::Px(border)),
                ..default()
            },
            BorderColor::all(MAP_GREEN),
            ImageNode::new(preview.target.clone()),
        ))
        .id();
    commands.entity(root).add_child(image);
    // What the turntable shows, kept current by `map_preview_info`.
    let info_font = readable(nav * 0.6);
    let info = commands
        .spawn((
            MenuFrame,
            Pickable::IGNORE,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(image_left),
                top: Val::Px(image_top + image_h + 0.02 * window_h),
                width: Val::Px(image_w),
                max_height: Val::Px(window_h - (image_top + image_h + 0.04 * window_h)),
                overflow: Overflow::clip(),
                ..default()
            },
        ))
        .with_child((
            MapPreviewInfo,
            Text::new(map_preview_text(preview.map())),
            TextColor(TEXT_WHITE),
            TextFont {
                font_size: FontSize::Px(info_font),
                ..default()
            },
            TextLayout::new(Justify::Left, LineBreak::WordBoundary),
            Pickable::IGNORE,
        ))
        .id();
    commands.entity(root).add_child(info);
}

/// The text under the map selector's turntable.
#[derive(Component)]
struct MapPreviewInfo;

/// Name, players, size (in Spring's 512-elmo map units) and description
/// of the map `stem`.
fn map_preview_text(stem: Option<&str>) -> String {
    let Some(stem) = stem else {
        return String::new();
    };
    let mut text = crate::map_loading::display_name(stem);
    let meta = crate::map_loading::map_meta(stem);
    let mut facts = Vec::new();
    if let Some(m) = meta {
        facts.push(match m.players {
            (lo, hi) if lo == hi => format!("{lo} players"),
            (lo, hi) => format!("{lo}-{hi} players"),
        });
    }
    if let Some(r) = map_preview::relief(stem) {
        facts.push(format!(
            "size {}x{}",
            (r.width / 512.0).round(),
            (r.depth / 512.0).round()
        ));
    }
    if !facts.is_empty() {
        text += "\n";
        text += &facts.join(", ");
    }
    if let Some(m) = meta
        && !m.description.is_empty()
    {
        text += "\n";
        text += m.description;
    }
    text
}

/// Keep the turntable's caption on the map it shows.
fn map_preview_info(
    preview: Res<map_preview::MapPreview>,
    mut texts: Query<&mut Text, With<MapPreviewInfo>>,
) {
    if !preview.is_changed() {
        return;
    }
    let wanted = map_preview_text(preview.map());
    for mut text in &mut texts {
        if text.0 != wanted {
            text.0.clone_from(&wanted);
        }
    }
}

/// The original's `Credits`: heading plate sitting on y=80%, the credit
/// lines hanging below it, the engine credit around y=30%, Back at 10%.
/// The credits block hangs from y=80%; its font is whatever fits the
/// window's width and the height left above the engine plates and
/// Back (under the page's `22/30`), and the engine plates sit right
/// under it, wherever that is: a small window cannot hold fourteen
/// readable lines over a plate fixed at the original's y=28%.
fn credits_page(commands: &mut Commands, root: Entity, (window_w, page_size): (f32, f32)) {
    LabelSpec::new(
        "Kernel Panic!\nCredits:",
        page_size,
        (0.5, 0.82),
        Anchor::Cb,
    )
    .frame(MEDIUM_GREEN)
    .spawn(commands, root);
    const CREDITS: &str = "\
- Original concept by Boirunner
- About all the work done by KDR_11k
- Maintenance and silly mod options by zwzsg
- Sounds by Noruas and Pendrokar
- Voices by Eva and Panda
- Music by Mister Beep, Lo-bat. and ParagonX9
- C64/Amiga remixes by Radix, Dreamfish, Stalker,
  Warwick Gaetjens, Wire and Front 6
- Maps by Boirunner, Runecrafter, zwzsg, TradeMark,
  KDR_11k and FireStorm
- Some LUA interface upgrade based of jK and trepan code
- The Touhou faction characters were inspired by ZUN's works
- Many thanks to lurker, Quantum, and the rest of #lua crew
- Remake in Rust + Bevy by Rafael Bachmann, porting the
  Spring / Recoil engine code the game runs on
- New maps after Beyond All Reason's Hooked (Raghna), Asteroid
  Mines, Sinkhole Network, Desert Triad, Pillar of Doom, SpeedMetal
- Code: GPL-3.0-or-later. New maps: CC BY-SA 4.0. Some maps and
  the music: licence unknown, see ASSETS-LICENSES.md
- Source: github.com/barafael/kernel-panic-bevy";
    const BLOCK_TOP: f32 = 0.80;
    const BACK_Y: f32 = 0.06;
    const GAP: f32 = 0.015;
    let window_h = page_size * 24.0;
    let small_font = readable(page_size * 24.0 / 30.0);
    // A plate is its font plus half an em of padding and the border tall.
    let plate = |font: f32| font * 1.5 + 2.0 * frame_border_px(font);
    let engine_h = plate(readable(page_size)) + plate(small_font);
    let budget_h = (BLOCK_TOP - BACK_Y - 2.0 * GAP) * window_h - plate(small_font) * 0.5 - engine_h;
    let credits_font = readable(fitted_font(
        page_size * 22.0 / 30.0,
        CREDITS,
        (0.9 * window_w, budget_h),
    ));
    let block_h = (1.2 * CREDITS.lines().count() as f32 + 0.5) * credits_font
        + 2.0 * frame_border_px(credits_font);
    LabelSpec::new(CREDITS, credits_font, (0.5, BLOCK_TOP), Anchor::Ct)
        .frame(TEAL)
        .justify(Justify::Left)
        .spawn(commands, root);
    let engine_y = BLOCK_TOP - (block_h + plate(readable(page_size))) / window_h - GAP;
    LabelSpec::new("Spring Engine by:", page_size, (0.5, engine_y), Anchor::Cb)
        .frame(EXTREME_ORANGE)
        .spawn(commands, root);
    LabelSpec::new(
        "Swedish Yankspankers",
        small_font,
        (0.5, engine_y),
        Anchor::Ct,
    )
    .frame(README_UPDOWN)
    .spawn(commands, root);
    back_button(small_font, (0.5, BACK_Y), MenuPage::Main).spawn(commands, root);
}

/// The largest font up to `wanted` at which `text` (monospace glyphs
/// ~0.6 em wide, lines 1.2 em tall, half an em of frame padding per
/// side) fits in a `(width, height)` box, in pixels.
fn fitted_font(wanted: f32, text: &str, (width, height): (f32, f32)) -> f32 {
    let lines = text.lines().count().max(1) as f32;
    let longest = text
        .lines()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(1)
        .max(1) as f32;
    let by_width = width / (0.6 * longest + 1.2);
    let by_height = height / (1.2 * lines + 0.7);
    wanted.det_min(by_width).det_min(by_height)
}

/// The original's `PrintReadMe`: file name plate at the top centre, the
/// text panel filling the screen below it, Up in both top corners, Down
/// at 30% / 70% along the bottom edge and Back between them.
fn readme_page(commands: &mut Commands, root: Entity, window_h: f32, scroll: usize) {
    let page_size = window_h / 24.0;
    LabelSpec::new(
        "Kernel_Panic_readme.txt",
        window_h / 32.0,
        (0.5, 1.0),
        Anchor::Ct,
    )
    .frame(Color::srgb(1.0, 1.0, 0.5))
    .spawn(commands, root);

    // The original prints 16 px lines at ~1080p; scale with the window.
    let line_px = (window_h / 64.0).det_max(12.0);
    // Leave the top ~10% for the file name and the bottom ~10% for the
    // Down / Back row.
    let lines_per_screen = ((window_h * 0.78) / (line_px * 1.2)) as usize;
    let lines = readme_lines();
    let max_scroll = lines.len().saturating_sub(lines_per_screen);
    let scroll = scroll.min(max_scroll);
    let body: String = lines
        .iter()
        .skip(scroll)
        .take(lines_per_screen)
        .map(|l| format!("{l}\n"))
        .collect();

    let body_text = commands
        .spawn((
            Text::new(body),
            TextColor(TEXT_WHITE),
            TextFont {
                font_size: FontSize::Px(line_px),
                ..default()
            },
            Pickable::IGNORE,
        ))
        .id();
    let body_panel = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(0.0),
                top: Val::Percent(10.0),
                width: Val::Percent(100.0),
                height: Val::Percent(80.0),
                overflow: Overflow::clip(),
                padding: UiRect::axes(Val::Px(line_px), Val::Px(line_px * 0.5)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.1, 0.2, 1.0)),
            Pickable::IGNORE,
        ))
        .id();
    commands.entity(body_panel).add_child(body_text);
    commands.entity(root).add_child(body_panel);

    // Page by (lines per screen - 1), like the original's Up/Down.
    let page = (lines_per_screen.saturating_sub(1)) as i32;
    if scroll > 0 {
        for (x, anchor) in [(0.0, Anchor::Lt), (1.0, Anchor::Rt)] {
            ButtonSpec::new(
                "Up",
                README_UPDOWN,
                page_size,
                (x, 1.0),
                anchor,
                MenuAction::ScrollReadme(-page),
            )
            .spawn(commands, root);
        }
    }
    if scroll < max_scroll {
        for x in [0.3, 0.7] {
            ButtonSpec::new(
                "Down",
                README_UPDOWN,
                page_size,
                (x, 0.0),
                Anchor::Cb,
                MenuAction::ScrollReadme(page),
            )
            .spawn(commands, root);
        }
    }
    ButtonSpec::new(
        "Back",
        NAV_BLUE,
        page_size,
        (0.5, 0.0),
        Anchor::Cb,
        MenuAction::Goto(MenuPage::Main),
    )
    .spawn(commands, root);
}

/// The readme, embedded so the web build (no disk) shows it too.
const README: &str = include_str!("../../assets/readme.txt");

/// Readme lines, cached. CRLF is normalised and long runs of blank
/// lines collapse to a single spacer, mirroring the original's parse.
fn readme_lines() -> Vec<String> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Vec<String>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let mut out: Vec<String> = Vec::new();
            for line in README.replace("\r\n", "\n").split('\n') {
                // The font has no tab glyph: expand to the next
                // multiple of four columns.
                let mut expanded = String::new();
                for c in line.trim_end().chars() {
                    if c == '\t' {
                        let pad = 4 - expanded.chars().count() % 4;
                        expanded.extend(std::iter::repeat_n(' ', pad));
                    } else {
                        expanded.push(c);
                    }
                }
                if expanded.is_empty() && out.last().is_some_and(String::is_empty) {
                    continue;
                }
                out.push(expanded);
            }
            out
        })
        .clone()
}

// ---------------------------------------------------------------------------
// Esc overlay + game over (AppState::InGame) — the game keeps running
// ---------------------------------------------------------------------------

fn esc_toggle(
    keys: Res<ButtonInput<KeyCode>>,
    mut esc_open: ResMut<EscMenuOpen>,
    mut esc_settings: ResMut<EscSettingsOpen>,
    mut esc_audio: ResMut<EscAudioOpen>,
    game_over_open: Res<GameOverOpen>,
    mut stats: ResMut<stats_page::StatsView>,
) {
    // Esc backs out of the Statistics page to the game-over panel.
    if keys.just_pressed(KeyCode::Escape) && stats.open {
        stats.open = false;
        return;
    }
    if keys.just_pressed(KeyCode::Escape) && !game_over_open.0 {
        if esc_audio.0 {
            esc_audio.0 = false;
        } else if esc_settings.0 {
            // Back out of the Settings page to the overlay first.
            esc_settings.0 = false;
        } else {
            esc_open.0 = !esc_open.0;
        }
    }
}

/// In the launch menu, Escape backs out to the main page — the only
/// exit the sub-pages offer besides their Back button. On Main itself
/// Esc does nothing (no accidental quit).
fn esc_in_menu(keys: Res<ButtonInput<KeyCode>>, mut page: ResMut<MenuPage>) {
    if keys.just_pressed(KeyCode::Escape) && *page != MenuPage::Main {
        *page = MenuPage::Main;
    }
}

/// The launch menu root carries `PersistentEntity` (it must survive the
/// in-game world teardown), so leaving `Menu` for `InGame` has to drop
/// it explicitly or the last page would stay frozen over the match.
fn despawn_launch_menu(
    mut commands: Commands,
    existing_root: Query<Entity, (With<MenuRoot>, Without<GameOverPanel>)>,
) {
    for e in &existing_root {
        commands.entity(e).despawn();
    }
}

fn close_all_overlays(
    mut esc_open: ResMut<EscMenuOpen>,
    mut esc_settings: ResMut<EscSettingsOpen>,
    mut esc_audio: ResMut<EscAudioOpen>,
    mut game_over: ResMut<GameOverOpen>,
    mut stats: ResMut<stats_page::StatsView>,
) {
    stats.open = false;
    *esc_open = EscMenuOpen(false);
    *esc_settings = EscSettingsOpen(false);
    *esc_audio = EscAudioOpen(false);
    *game_over = GameOverOpen(false);
}

/// Leaving a networked match, second half: once the driver closed the
/// transport, go to the menu (the same way as the Menu button).
fn finish_leaving(
    status: Option<Res<crate::net::driver::NetStatus>>,
    mut actions: MessageWriter<MenuActionMessage>,
    mut sent: Local<bool>,
) {
    let closed = status.is_some_and(|s| s.leaving && s.closed);
    if closed && !*sent {
        actions.write(MenuActionMessage {
            action: MenuAction::GoToMenu,
        });
    }
    *sent = closed;
}

/// Whether the running match is networked (the Esc overlay and the
/// game-over panel offer Leave match, never Restart).
fn networked(status: Option<&crate::net::driver::NetStatus>) -> bool {
    status.is_some_and(|s| s.networked)
}

/// The Esc overlay's four big plates: (label, position, anchor, action).
fn esc_plates(networked: bool) -> [(&'static str, (f32, f32), Anchor, MenuAction); 4] {
    // On the web there is nothing to quit to: the fourth plate goes
    // to the menu as well.
    let (quit_text, quit) = if cfg!(target_arch = "wasm32") {
        ("Menu", MenuAction::GoToMenu)
    } else {
        ("Quit", MenuAction::Quit)
    };
    // A networked match can't be restarted alone: its plate goes back
    // to the room (resign, leave the match; Menu leaves the room too).
    let second = if networked {
        ("Back to room", MenuAction::BackToRoom)
    } else {
        ("Restart", MenuAction::Restart)
    };
    [
        ("Resume", (0.45, 0.505), Anchor::Rb, MenuAction::Resume),
        (second.0, (0.55, 0.505), Anchor::Lb, second.1),
        ("Menu", (0.45, 0.5), Anchor::Rt, MenuAction::GoToMenu),
        (quit_text, (0.55, 0.5), Anchor::Lt, quit),
    ]
}

#[allow(clippy::too_many_arguments)]
/// What the Esc overlay was last drawn for: open, Settings, Audio,
/// Resign asked, networked, the render settings.
type EscMenuDrawn = (bool, bool, bool, bool, bool, RenderSettings);

#[allow(clippy::too_many_arguments)]
fn maintain_esc_menu(
    esc_open: Res<EscMenuOpen>,
    esc_settings: Res<EscSettingsOpen>,
    esc_audio: Res<EscAudioOpen>,
    mut resign_confirm: ResMut<EscResignConfirm>,
    settings: SettingsView,
    music: Res<MusicPlayer>,
    music_catalog: Res<MusicCatalog>,
    net: Option<Res<crate::net::driver::NetStatus>>,
    mut commands: Commands,
    existing_root: Query<Entity, (With<MenuRoot>, Without<GameOverPanel>)>,
    windows: Query<&Window>,
    mut last: Local<Option<EscMenuDrawn>>,
) {
    // A closed overlay forgets a half-done Resign.
    if !esc_open.0 && resign_confirm.0 {
        resign_confirm.0 = false;
    }
    // Redrawn when the overlay opens or closes, when it switches to or
    // from its Settings page, and when a setting changed (its choice
    // rows show the current pick).
    let now = (
        esc_open.0,
        esc_settings.0,
        esc_audio.0,
        resign_confirm.0,
        networked(net.as_deref()),
        *settings.render,
    );
    if *last == Some(now) {
        return;
    }
    *last = Some(now);
    for e in &existing_root {
        // Only despawn Esc-menu roots (game-over panel has its own tag on
        // the same marker set; disambiguated below by GameOverPanel).
        commands.entity(e).despawn();
    }
    if !esc_open.0 {
        return;
    }
    let Ok(window) = windows.single() else {
        return;
    };
    let size = window.height() / 14.0;
    let root = spawn_backdrop(&mut commands, OVERLAY_GLASS);

    if esc_settings.0 {
        let layout = (window.width(), window.height() / 24.0);
        if esc_audio.0 {
            audio_page(
                &mut commands,
                root,
                layout,
                &settings,
                MenuAction::EscAudio(false),
            );
        } else {
            settings_page(
                &mut commands,
                root,
                layout,
                &settings,
                MenuAction::EscAudio(true),
                MenuAction::EscSettings(false),
            );
        }
        return;
    }

    // The original's `SaveLoadMenu`: four big plates meeting around the
    // screen centre — Save/Load on top (here Resume/Restart, or Leave
    // match when networked; there is no save system), Menu/Restart below
    // (here Menu/Quit) — plus a Settings plate underneath.
    for (text, pos, anchor, action) in esc_plates(networked(net.as_deref())) {
        ButtonSpec::new(text, BUTTON_GREEN, size, pos, anchor, action)
            .min_width(22.0)
            .spawn(&mut commands, root);
    }
    ButtonSpec::new(
        "Settings",
        NAV_BLUE,
        size * 0.6,
        (0.5, 0.32),
        Anchor::Ct,
        MenuAction::EscSettings(true),
    )
    .min_width(20.0)
    .spawn(&mut commands, root);
    // Resign (the quit box's): asks once, then resigns.
    let (resign_text, resign) = if resign_confirm.0 {
        ("Really resign?", MenuAction::Resign(true))
    } else {
        ("Resign", MenuAction::Resign(false))
    };
    ButtonSpec::new(
        resign_text,
        LOST_RED,
        size * 0.6,
        (0.5, 0.7),
        Anchor::Cb,
        resign,
    )
    .min_width(20.0)
    .spawn(&mut commands, root);
    // The same music cluster as the main page: skipping a track mid-match
    // is what the overlay is for.
    music_controls(&mut commands, root, size * 0.5, &music, &music_catalog);
}

#[derive(Component)]
struct GameOverPanel;

/// The bot's `stats` command: the game-over panel, on its Statistics
/// page (on `stat`, if given).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn show_stats(world: &mut World, stat: Option<crate::stats::Stat>) {
    world.resource_mut::<GameOverOpen>().0 = true;
    let mut view = world.resource_mut::<stats_page::StatsView>();
    view.open = true;
    if let Some(stat) = stat {
        view.stat = stat;
    }
}

fn game_over_watch(
    state: Res<State<GameState>>,
    mut last: Local<Option<GameState>>,
    mut game_over_open: ResMut<GameOverOpen>,
    dismissed: Res<GameOverDismissed>,
) {
    if last.is_none() || *last != Some(*state.get()) {
        let entered = matches!(*state.get(), GameState::Victory | GameState::Defeat);
        if entered && !dismissed.0 {
            game_over_open.0 = true;
        }
        if *state.get() == GameState::Playing {
            game_over_open.0 = false;
        }
        *last = Some(*state.get());
    }
}

/// What the game-over panel's Statistics page draws from.
#[derive(SystemParam)]
struct StatsInputs<'w> {
    view: Res<'w, stats_page::StatsView>,
    stats: Res<'w, crate::stats::MatchStats>,
    local: Option<Res<'w, crate::units::player::LocalTeam>>,
    images: ResMut<'w, Assets<Image>>,
}

/// The game-over panel's redraw key: open, the result, networked, and
/// with the Statistics page up its view, the samples and the window.
type GameOverDrawn = (
    bool,
    GameState,
    bool,
    Option<(stats_page::StatsView, usize, UVec2)>,
);

#[allow(clippy::too_many_arguments)]
fn maintain_game_over(
    game_over_open: Res<GameOverOpen>,
    game_state: Res<State<GameState>>,
    net: Option<Res<crate::net::driver::NetStatus>>,
    mut inputs: StatsInputs,
    mut commands: Commands,
    existing_root: Query<Entity, With<GameOverPanel>>,
    windows: Query<&Window>,
    mut last: Local<Option<GameOverDrawn>>,
) {
    // Redrawn when the result changes under an open panel too: a
    // fallen player's allies winning turns "You lost!" into "You won!".
    let networked = networked(net.as_deref());
    let window = windows.single().ok();
    let page = (game_over_open.0 && inputs.view.open).then(|| {
        (
            *inputs.view,
            inputs.stats.ticks.len(),
            window.map_or(UVec2::ZERO, |w| w.physical_size()),
        )
    });
    let now = (game_over_open.0, *game_state.get(), networked, page);
    if *last == Some(now) {
        return;
    }
    *last = Some(now);
    for e in &existing_root {
        commands.entity(e).despawn();
    }
    if !game_over_open.0 {
        return;
    }
    let Some(window) = window else {
        return;
    };
    let (title_size, menu_size) = (window.height() / 14.0, window.height() / 28.0);
    let won = *game_state.get() == GameState::Victory;

    let root = commands
        .spawn((
            MenuRoot,
            GameOverPanel,
            crate::map_loading::PersistentEntity,
            Pickable::IGNORE,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            BackgroundColor(OVERLAY_GLASS),
        ))
        .id();

    if page.is_some() {
        let local = inputs.local.as_deref().and_then(|l| l.seat());
        let StatsInputs {
            view,
            stats,
            images,
            ..
        } = &mut inputs;
        stats_page::stats_page(&mut commands, root, window, images, stats, view, local);
        return;
    }

    // The original's `GameOverMenu`: result plate at y=70%, then
    // "Keep on playing/watching" ending at x=48% and "Go to Menu"
    // starting at x=52% on the y=25% line; a loss adds Restart above
    // (not in a networked match: "Go to Menu" leaves it, closing the
    // connection).
    LabelSpec::new(
        if won { "You won!" } else { "You lost!" },
        title_size,
        (0.5, 0.7),
        Anchor::Cc,
    )
    .frame(if won { WON_GREEN } else { LOST_RED })
    .spawn(&mut commands, root);
    let keep = if won {
        "Keep on playing"
    } else {
        "Keep on watching"
    };
    ButtonSpec::new(
        keep,
        BUTTON_GREEN,
        menu_size,
        (0.48, 0.25),
        Anchor::Rc,
        MenuAction::KeepPlaying,
    )
    .spawn(&mut commands, root);
    ButtonSpec::new(
        "Go to Menu",
        BUTTON_GREEN,
        menu_size,
        (0.52, 0.25),
        Anchor::Lc,
        MenuAction::GoToMenu,
    )
    .spawn(&mut commands, root);
    if !won && !networked {
        ButtonSpec::new(
            "Restart",
            BUTTON_GREEN,
            menu_size,
            (0.5, 0.35),
            Anchor::Cc,
            MenuAction::Restart,
        )
        .spawn(&mut commands, root);
    } else if networked {
        // Where Restart sits in single player: the room the match came
        // from, everyone's seats as they were.
        ButtonSpec::new(
            "Back to room",
            BUTTON_GREEN,
            menu_size,
            (0.5, 0.35),
            Anchor::Cc,
            MenuAction::BackToRoom,
        )
        .spawn(&mut commands, root);
    }
    // The match's graphs (`stats`), under the plates.
    ButtonSpec::new(
        "Statistics",
        NAV_BLUE,
        menu_size,
        (0.5, 0.15),
        Anchor::Cc,
        MenuAction::Stats(stats_page::StatsAction::Open(true)),
    )
    .spawn(&mut commands, root);
}

// (no trailing helpers)

// ---------------------------------------------------------------------------
// Attract-mode demo — live battle behind the main menu
// ---------------------------------------------------------------------------

/// Keeps the attract-mode demo alive: once the all-AI skirmish behind
/// the menu is decided (one seat's homebases left) or has run for
/// [`DEMO_MAX_AGE`], roll a fresh random setup and reload.
#[derive(Resource, Default)]
struct DemoDirector {
    /// Seconds since the current demo world loaded.
    age: f32,
    /// Seconds the match has been decided for (restart after a grace
    /// period so the last fight finishes on screen).
    decided_for: f32,
    /// Whether two or more sides have been seen alive — a world still
    /// loading shows no homebases at all and must not count as decided.
    contested: bool,
}

/// Longest a single demo match runs before a new roll.
const DEMO_MAX_AGE: f32 = 12.0 * 60.0;
/// Minimum demo age before "decided" is checked.
const DEMO_SETTLE: f32 = 10.0;
/// Seconds a decided match keeps playing before the restart.
const DEMO_DECIDED_GRACE: f32 = 8.0;

fn demo_director(
    time: Res<Time>,
    setup: Res<crate::game_setup::GameSetup>,
    mut director: ResMut<DemoDirector>,
    homebases: Query<&TeamId, With<Homebase>>,
    dev: Res<DevOptions>,
    mut commands: Commands,
    mut run_game: MessageWriter<RunGame>,
) {
    if setup.is_changed() {
        *director = DemoDirector::default();
    }
    if !setup.demo {
        return;
    }
    director.age += time.delta_secs();
    let mut teams: Vec<u8> = homebases.iter().map(|t| t.0).collect();
    teams.sort_unstable();
    teams.dedup();
    director.contested |= teams.len() >= 2;
    let decided = director.contested && director.age > DEMO_SETTLE && teams.len() <= 1;
    director.decided_for = if decided {
        director.decided_for + time.delta_secs()
    } else {
        0.0
    };
    let max_age = dev.demo_cycle.unwrap_or(DEMO_MAX_AGE);
    if director.decided_for > DEMO_DECIDED_GRACE || director.age > max_age {
        commands.insert_resource(demo_setup(&dev));
        run_game.write(RunGame);
        *director = DemoDirector::default();
    }
}

/// `KP_RECORD_ATTRACT`: the recorder's spectated match is filmed by the
/// attract director.
fn attract_recording(dev: Res<DevOptions>, state: Res<State<AppState>>) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    let on = dev.record_attract && dev.record.is_some();
    #[cfg(target_arch = "wasm32")]
    let on = {
        let _ = &dev;
        false
    };
    on && *state.get() == AppState::InGame
}

/// On first boot: once the map catalog exists, load the demo world
/// behind the launch menu.
/// First-frame boot: seed the attract-mode demo. Public so
/// `map_loading` can order its `RunGame` reload after this system —
/// the reload must observe the `demo_setup` insert this writes.
pub fn boot_demo(
    mut done: Local<bool>,
    dev: Res<DevOptions>,
    mut commands: Commands,
    mut run_game: MessageWriter<RunGame>,
) {
    if *done {
        return;
    }
    *done = true;
    commands.insert_resource(demo_setup(&dev));
    run_game.write(RunGame);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::diagnostic::{Diagnostic, RegisterDiagnostic};

    /// A networked match offers Back to room where single player has
    /// Restart; the rest of the overlay is the same.
    #[test]
    fn networked_esc_overlay_leaves_instead_of_restarting() {
        let labels = |net| esc_plates(net).map(|(text, _, _, action)| (text, action));
        let solo = labels(false);
        let net = labels(true);
        assert_eq!(solo[1], ("Restart", MenuAction::Restart));
        assert_eq!(net[1], ("Back to room", MenuAction::BackToRoom));
        assert!(!net.iter().any(|(_, a)| *a == MenuAction::Restart));
        assert_eq!((solo[0], solo[2], solo[3]), (net[0], net[2], net[3]));
    }

    /// Leaving a networked match: Leave match resigns and asks the driver
    /// to disconnect; once the transport is closed `finish_leaving` sends
    /// the player to the menu, once.
    #[test]
    fn leave_match_resigns_then_goes_to_the_menu_once_closed() {
        use crate::net::driver::{NetSession, NetStatus};
        use crate::net::session::{DelayBounds, Session, SessionConfig};
        use crate::net::sim_net::SimNetwork;
        use crate::net::transport::PeerId;
        use bevy::ecs::system::RunSystemOnce;

        let net = SimNetwork::new(1);
        let cfg = SessionConfig {
            match_id: 1,
            me: PeerId(2),
            roster: [(PeerId(1), Some(0)), (PeerId(2), Some(1))].into(),
            delay: DelayBounds::default(),
        };
        let mut world = World::new();
        world.insert_resource(NetSession::new(Session::networked(
            cfg,
            Box::new(net.join(PeerId(2))),
        )));
        world.insert_resource(crate::units::player::LocalTeam(1));
        world.init_resource::<crate::net::CommandBus>();
        world.init_resource::<Messages<MenuActionMessage>>();
        world
            .run_system_once(
                |mut net: NetLeave, mut bus: ResMut<crate::net::CommandBus>| {
                    assert!(net.connected());
                    net.leave(Some(&mut bus), false);
                },
            )
            .unwrap();
        // The resignation went into the bus for the driver to send.
        let mut bus = world.resource_mut::<crate::net::CommandBus>();
        bus.routed = true;
        bus.seal(Some(1));
        let sealed = std::mem::take(&mut bus.outbox);
        assert_eq!(sealed[0].commands, vec![crate::net::SimCommand::Resign]);
        assert!(world.resource::<NetSession>().leaving());

        let mut status = NetStatus {
            networked: true,
            leaving: true,
            ..Default::default()
        };
        world.insert_resource(status.clone());
        let finish = world.register_system(finish_leaving);
        world.run_system(finish).unwrap();
        let sent = |w: &mut World| {
            w.resource_mut::<Messages<MenuActionMessage>>()
                .drain()
                .count()
        };
        assert_eq!(sent(&mut world), 0, "still connected");
        status.closed = true;
        world.insert_resource(status);
        world.run_system(finish).unwrap();
        world.run_system(finish).unwrap();
        assert_eq!(sent(&mut world), 1, "to the menu, once");
    }

    /// The Esc overlay is modal: with it (or its Settings page) open,
    /// a press anywhere — no button under the cursor — is consumed
    /// before the HUD and the world see it; releases pass. Closed, a
    /// press off the buttons goes through.
    #[test]
    fn esc_overlay_swallows_every_press() {
        use bevy::ecs::system::RunSystemOnce;
        let run = |esc: bool, settings: bool| {
            let mut world = World::new();
            let mut window = Window::default();
            window.set_cursor_position(Some(Vec2::new(100.0, 100.0)));
            world.spawn(window);
            world.init_resource::<MenuFocus>();
            world.init_resource::<Messages<MenuActionMessage>>();
            world.insert_resource(EscMenuOpen(esc));
            world.insert_resource(EscSettingsOpen(settings));
            let mut mouse = ButtonInput::<MouseButton>::default();
            mouse.press(MouseButton::Left);
            mouse.press(MouseButton::Right);
            mouse.press(MouseButton::Middle);
            world.insert_resource(mouse);
            world.run_system_once(mouse_menu_input).unwrap();
            let mouse = world.resource::<ButtonInput<MouseButton>>();
            let pressed = [MouseButton::Left, MouseButton::Right, MouseButton::Middle]
                .map(|b| mouse.just_pressed(b));

            world.resource_mut::<ButtonInput<MouseButton>>().clear();
            world
                .resource_mut::<ButtonInput<MouseButton>>()
                .release(MouseButton::Left);
            world.run_system_once(mouse_menu_input).unwrap();
            let released = world
                .resource::<ButtonInput<MouseButton>>()
                .just_released(MouseButton::Left);
            (pressed, released)
        };
        assert_eq!(run(true, false), ([false; 3], true), "overlay open");
        assert_eq!(run(true, true), ([false; 3], true), "Settings page");
        assert_eq!(run(false, false), ([true; 3], true), "closed");
    }

    #[test]
    fn demo_map_cycle_wraps_both_ways() {
        assert_eq!(step_catalog_index(Some(0), 1, 5), 1);
        assert_eq!(step_catalog_index(Some(4), 1, 5), 0);
        assert_eq!(step_catalog_index(Some(0), -1, 5), 4);
        assert_eq!(step_catalog_index(None, 1, 5), 0);
        assert_eq!(step_catalog_index(None, -1, 5), 4);
        assert_eq!(step_catalog_index(Some(0), 1, 1), 0);
    }

    /// Six volume steps on a 2560-wide window fit before the right
    /// edge; four short MSAA choices keep their natural 12 % boxes.
    #[test]
    fn choice_rows_fit_the_window() {
        let glyph = 0.6 * (1440.0 / 24.0 * 0.85) / 2560.0;
        assert_eq!(
            choices_start(6, glyph),
            CHOICES_MIN_START,
            "short labels keep the 80 % span"
        );
        let start = choices_start(12, glyph);
        assert!((CHOICES_MIN_START..0.22).contains(&start), "{start}");
        let volume = choice_step(4, glyph, 6, start);
        assert!(start + volume * 6.0 <= CHOICES_END + 1e-6);
        assert!(volume > 0.1);
        assert_eq!(choice_step(3, glyph, 4, start), 0.12);
        // An 800×600 window (376 logical rows at 1.5×): "Antialiasing"
        // pushes the choices right instead of running off the left edge.
        let glyph = 0.6 * readable(376.0 / 24.0 * 0.85) / 533.0;
        let start = choices_start(12, glyph);
        assert!(start > CHOICES_MIN_START && start < 0.4, "{start}");
        let step = choice_step(4, glyph, 6, start);
        assert!(start + step * 6.0 <= CHOICES_END + 1e-6);
    }

    #[test]
    fn pause_label_shows_what_a_press_does() {
        assert_eq!(pause_label(false), "||");
        assert_eq!(pause_label(true), ">");
    }

    /// The cluster's name text and pause label follow the player.
    #[test]
    fn music_readout_writes_the_cluster_texts() {
        let mut app = App::new();
        app.insert_resource(MusicCatalog {
            tracks: vec!["music/lo-bat--hello.ogg".into()],
        })
        .init_resource::<MusicPlayer>()
        .add_systems(Update, music_readout);
        let name = app
            .world_mut()
            .spawn((MusicTrackReadout, Text::new("")))
            .id();
        let label = app.world_mut().spawn(Text::new(pause_label(false))).id();
        app.world_mut().spawn(MusicPauseButton).add_child(label);

        app.update();
        assert_eq!(app.world().get::<Text>(name).unwrap().0, "");
        assert_eq!(app.world().get::<Text>(label).unwrap().0, "||");

        let mut player = app.world_mut().resource_mut::<MusicPlayer>();
        player.current = Some(0);
        player.paused = true;
        app.update();
        assert_eq!(app.world().get::<Text>(name).unwrap().0, "Lo Bat - Hello");
        assert_eq!(app.world().get::<Text>(label).unwrap().0, ">");
    }

    #[test]
    fn fps_text_rounds_and_dashes_before_first_sample() {
        assert_eq!(fps_text(None), "-- fps");
        assert_eq!(fps_text(Some(59.6)), "60 fps");
        assert_eq!(fps_text(Some(143.2)), "143 fps");
    }

    /// The readout's child text follows the smoothed FPS diagnostic.
    #[test]
    fn fps_readout_writes_the_child_text() {
        let mut app = App::new();
        app.init_resource::<DiagnosticsStore>()
            .register_diagnostic(Diagnostic::new(FrameTimeDiagnosticsPlugin::FPS))
            .add_systems(Update, fps_readout);
        let text = app.world_mut().spawn(Text::new(fps_text(None))).id();
        app.world_mut().spawn(FpsReadout).add_child(text);

        // No measurement yet: the dash stays.
        app.update();
        assert_eq!(app.world().get::<Text>(text).unwrap().0, "-- fps");

        app.world_mut()
            .resource_mut::<DiagnosticsStore>()
            .get_mut(&FrameTimeDiagnosticsPlugin::FPS)
            .unwrap()
            .add_measurement(bevy::diagnostic::DiagnosticMeasurement {
                time: bevy::platform::time::Instant::now(),
                value: 61.4,
            });
        app.update();
        assert_eq!(app.world().get::<Text>(text).unwrap().0, "61 fps");
    }
}
