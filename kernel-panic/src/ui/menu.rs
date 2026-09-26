//! The Kernel Panic menu system, rebuilt after the original
//! "Spring Direct Launch 2" widget (`kp_spring_direct_launch.lua`).
//!
//! Pages: main menu (zigzag layout, alternating left/right anchors),
//! quick skirmish (Easy/Medium/Hard/Very Hard), advanced skirmish (map
//! list, faction cycles, grouping, difficulty, live description line),
//! map list, credits, scrollable readme, an in-game Esc overlay
//! (Resume/Restart/Menu — the simulation keeps running behind it, like
//! the original), and the game-over panel ("You won!"/"You lost!").
//!
//! Visual language copied from the original: green-on-black terminal
//! look, cyan `Kernel Panic!` title, blue navigation buttons, the
//! difficulty colour ladder (light-cyan → green → yellow → orange →
//! red), olive-tinted backdrop. The original draws every panel as a
//! skewed parallelogram; `bevy_ui` has no transforms on nodes, so panels
//! are bevelled rectangles with the same colour system instead.
//!
//! Input follows the original's model: clicks go through `bevy_picking`
//! observers (`Pointer<Click>`), and actions are funnelled through one
//! [`MenuAction`] message so all state changes live in
//! [`handle_menu_actions`]. The game is never paused by the Esc menu.

use bevy::ecs::observer::On;
use bevy::picking::events::{Click, Out, Over};
use bevy::picking::Pickable;
use bevy::prelude::*;

use crate::game_setup::{
    build_setup, describe_setup, demo_setup, showcase_setup, AppState, GameOverDismissed,
    Grouping, RunGame, SkirmishConfig,
};
use crate::map_loading::MapCatalog;
use crate::rendering::camera::{MapBounds, RtsCamera, RtsCameraState};
use crate::units::combat::AimTarget;
use crate::units::components::{Faction, Homebase, TeamId, UnitType};
use crate::units::lifecycle::game_over::GameState;

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MenuPage>()
            .init_resource::<EscMenuOpen>()
            .init_resource::<GameOverOpen>()
            .init_resource::<ReadmeScroll>()
            .init_resource::<DemoDirector>()
            .init_resource::<AttractCamera>()
            .init_resource::<MenuFocus>()
            .add_message::<MenuActionMessage>()
            .add_systems(OnEnter(AppState::InGame), (close_all_overlays, despawn_launch_menu))
            .add_systems(OnExit(AppState::InGame), close_all_overlays)
            .add_systems(
                Update,
                (
                    handle_menu_actions,
                    keyboard_menu_nav,
                    mouse_menu_input
                        .run_if(in_state(AppState::Menu).or(in_state(AppState::InGame))),
                    esc_in_menu.run_if(in_state(AppState::Menu)),
                    boot_demo
                        .run_if(in_state(AppState::Menu).and(resource_exists::<MapCatalog>)),
                    demo_director.run_if(
                        in_state(AppState::Menu)
                            .and(resource_exists::<crate::game_setup::GameSetup>),
                    ),
                    attract_camera
                        .run_if(in_state(AppState::Menu))
                        .before(crate::rendering::camera::camera_smoothing),
                    maintain_launch_menu.run_if(in_state(AppState::Menu)),
                    esc_toggle.run_if(in_state(AppState::InGame)),
                    maintain_esc_menu.run_if(in_state(AppState::InGame)),
                    game_over_watch.run_if(in_state(AppState::InGame)),
                    maintain_game_over.run_if(in_state(AppState::InGame)),
                ),
            );
    }
}

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
}

#[derive(Debug, Default, Deref, DerefMut, Resource)]
struct EscMenuOpen(bool);

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
    /// button bright; mouse mode lets the `Over`/`Out` observers paint.
    mode: InputMode,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    #[default]
    Mouse,
    Keyboard,
}

impl MenuFocus {
    fn on_keyboard(&mut self) {
        self.mode = InputMode::Keyboard;
    }
    fn on_mouse(&mut self) {
        self.mode = InputMode::Mouse;
    }
}

/// Marker on every menu root (launch menu, Esc overlay, game-over panel).
#[derive(Component)]
struct MenuRoot;

/// One menu button: carries its action and base colour (hover restores
/// exactly this).
#[derive(Component)]
struct MenuButton {
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
    ScrollReadme(i32),
    /// Start showcase mode for the given faction.
    Showcase(Faction),
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
    Color::srgb(1.0 - (1.0 - c.red) / 2.0, 1.0 - (1.0 - c.green) / 2.0, 1.0 - (1.0 - c.blue) / 2.0)
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

fn frame_border(font_size: f32) -> UiRect {
    UiRect::all(Val::Px((font_size * 0.09).max(1.0)))
}

/// A clickable frame at original coordinates (see [`anchored`]).
/// Clicks go to the central action router.
#[allow(clippy::too_many_arguments)]
fn button(
    commands: &mut Commands,
    parent: Entity,
    label: &str,
    color: Color,
    font_size: f32,
    (x, y): (f32, f32),
    anchor: Anchor,
    action: MenuAction,
) -> Entity {
    let text = spawn_button_text(commands, label, font_size);
    let (mut node, transform) = anchored(x, y, anchor);
    node.padding = frame_padding(font_size);
    node.border = frame_border(font_size);
    node.justify_content = JustifyContent::Center;
    let entity = commands
        .spawn((
            MenuButton { action, base: color },
            node,
            transform,
            fill(color),
            border(color),
        ))
        .id();
    finish_button(commands, parent, entity, text)
}

/// A [`button`] for one option of a choice group (grouping,
/// difficulty): the current pick is drawn brightened with a heavier
/// border, so the page shows the configuration at a glance.
#[allow(clippy::too_many_arguments)]
fn choice_button(
    commands: &mut Commands,
    parent: Entity,
    label: &str,
    color: Color,
    font_size: f32,
    pos: (f32, f32),
    anchor: Anchor,
    action: MenuAction,
    chosen: bool,
) -> Entity {
    let base = if chosen { brighten(color) } else { color };
    let e = button(commands, parent, label, base, font_size, pos, anchor, action);
    if chosen {
        let heavy = UiRect::all(Val::Px((font_size * 0.18).max(2.0)));
        commands
            .entity(e)
            .entry::<Node>()
            .and_modify(move |mut n| n.border = heavy);
    }
    e
}

/// Give a frame a minimum width (percent of the window) so a column of
/// frames lines up as even boxes instead of a ragged edge.
fn min_width(commands: &mut Commands, entity: Entity, pct: f32) {
    commands
        .entity(entity)
        .entry::<Node>()
        .and_modify(move |mut n| n.min_width = Val::Percent(pct));
}

fn spawn_button_text(commands: &mut Commands, label: &str, font_size: f32) -> Entity {
    commands
        .spawn((
            Text::new(label),
            TextColor(TEXT_WHITE),
            TextFont {
                font_size,
                ..default()
            },
            // Why no-wrap: an absolute node's available width is what's
            // left right of its `left` inset, so a right-anchored frame
            // near the edge would otherwise wrap its label.
            TextLayout::new(Justify::Center, LineBreak::NoWrap),
            Pickable::IGNORE,
        ))
        .id()
}

/// Wire click/hover observers, attach the text, and parent to `parent`.
fn finish_button(
    commands: &mut Commands,
    parent: Entity,
    entity: Entity,
    text: Entity,
) -> Entity {
    commands.entity(entity).add_child(text);

    // Click + hover hit-tested manually in `mouse_menu_input` —
    // bevy_picking's pointer pipeline does not populate in this app's
    // runtime, so the observers only exist for parity with the HUD.
    commands.entity(entity).observe(
        |click: On<Pointer<Click>>,
         buttons: Query<&MenuButton>,
         mut ev: MessageWriter<MenuActionMessage>| {
            if let Ok(b) = buttons.get(click.entity) {
                ev.write(MenuActionMessage { action: b.action });
            }
        },
    );
    commands.entity(entity).observe(
        |over: On<Pointer<Over>>,
         mut buttons: Query<(&MenuButton, &mut BorderColor, &mut BackgroundColor)>,
         mut focus: ResMut<MenuFocus>| {
            if let Ok((b, mut bc, mut bg)) = buttons.get_mut(over.entity) {
                // A pointer is over a button: the mouse owns the current
                // highlight from here on.
                focus.on_mouse();
                *bc = border(brighten(b.base));
                *bg = fill(brighten(b.base));
            }
        },
    );
    commands.entity(entity).observe(
        |out: On<Pointer<Out>>,
         mut buttons: Query<(&MenuButton, &mut BorderColor, &mut BackgroundColor)>| {
            if let Ok((b, mut bc, mut bg)) = buttons.get_mut(out.entity) {
                *bc = border(b.base);
                *bg = fill(b.base);
            }
        },
    );

    commands.entity(parent).add_child(entity);
    entity
}

/// Non-interactive frame (headings, description lines, text blocks).
/// `frame` draws the original's coloured plate behind the text; `None`
/// leaves bare text. `justify` aligns multi-line text inside the frame.
#[allow(clippy::too_many_arguments)]
fn label(
    commands: &mut Commands,
    parent: Entity,
    text: &str,
    frame: Option<Color>,
    font_size: f32,
    (x, y): (f32, f32),
    anchor: Anchor,
    justify: Justify,
) -> Entity {
    let text = commands
        .spawn((
            Text::new(text),
            TextColor(TEXT_WHITE),
            TextFont {
                font_size,
                ..default()
            },
            TextLayout::new(justify, LineBreak::NoWrap),
            Pickable::IGNORE,
        ))
        .id();
    let (mut node, transform) = anchored(x, y, anchor);
    node.padding = frame_padding(font_size);
    if frame.is_some() {
        node.border = frame_border(font_size);
    }
    let entity = commands.spawn((node, transform, Pickable::IGNORE)).id();
    if let Some(color) = frame {
        commands.entity(entity).insert((fill(color), border(color)));
    }
    commands.entity(entity).add_child(text);
    commands.entity(parent).add_child(entity);
    entity
}

/// The `Kernel Panic!` title: cyan, top-centre, like the original main
/// menu's `AddFrame("Kernel Panic!", {x=vsx*0.5, y=vsy*0.98}, vsy/14, …, "ct")`.
fn title(commands: &mut Commands, parent: Entity, font_size: f32) {
    let t = commands
        .spawn((
            Text::new("Kernel Panic!"),
            TextColor(TITLE_CYAN),
            TextFont {
                font_size,
                ..default()
            },
            TextLayout::new(Justify::Center, LineBreak::NoWrap),
            Pickable::IGNORE,
        ))
        .id();
    let entity = commands
        .spawn((anchored(0.5, 0.98, Anchor::Ct), Pickable::IGNORE))
        .id();
    commands.entity(entity).add_child(t);
    commands.entity(parent).add_child(entity);
}

/// Window-height-derived font sizes, matching the original's vsy ratios:
/// `vsy/14` title, `vsy/20` main menu, `vsy/24` pages, `vsy/28` lists.
fn vsizes(height: f32) -> (f32, f32, f32, f32) {
    (height / 14.0, height / 20.0, height / 24.0, height / 28.0)
}

// ---------------------------------------------------------------------------
// Action routing
// ---------------------------------------------------------------------------

#[derive(Message)]
struct MenuActionMessage {
    action: MenuAction,
}

#[allow(clippy::too_many_arguments)]
fn handle_menu_actions(
    mut ev: MessageReader<MenuActionMessage>,
    mut page: ResMut<MenuPage>,
    mut config: ResMut<SkirmishConfig>,
    mut esc_open: ResMut<EscMenuOpen>,
    mut game_over_open: ResMut<GameOverOpen>,
    mut dismissed: ResMut<GameOverDismissed>,
    mut readme_scroll: ResMut<ReadmeScroll>,
    mut app_state: ResMut<NextState<AppState>>,
    mut game_state: ResMut<NextState<GameState>>,
    mut run_game: MessageWriter<RunGame>,
    catalog: Res<MapCatalog>,
    mut commands: Commands,
) {
    for msg in ev.read() {
        let action = msg.action;
        // Any config-affecting action invalidates the current page; the
        // maintain systems redraw it.
        match action {
            MenuAction::Goto(p) => {
                if p == MenuPage::Readme {
                    *readme_scroll = ReadmeScroll(0);
                }
                *page = p;
            }
            MenuAction::QuickStart(difficulty) => {
                config.difficulty = difficulty;
                config.grouping = Grouping::Duel;
                config.map = None; // weighted random, like RunRandomGame
                commands.insert_resource(build_setup(&config, &catalog.names()));
                // No RunGame here — OnEnter(InGame) performs the single
                // prepare+load pass.
                app_state.set(AppState::InGame);
            }
            MenuAction::StartSkirmish => {
                commands.insert_resource(build_setup(&config, &catalog.names()));
                app_state.set(AppState::InGame);
            }
            MenuAction::Restart => {
                run_game.write(RunGame);
                *esc_open = EscMenuOpen(false);
                *game_over_open = GameOverOpen(false);
            }
            MenuAction::GoToMenu => {
                app_state.set(AppState::Menu);
                *esc_open = EscMenuOpen(false);
                *game_over_open = GameOverOpen(false);
                *page = MenuPage::Main;
                // Reload the attract-mode demo behind the menu (the real
                // match's world is torn down by the RunGame handler).
                commands.insert_resource(demo_setup());
                run_game.write(RunGame);
            }
            MenuAction::Resume => {
                *esc_open = EscMenuOpen(false);
            }
            MenuAction::KeepPlaying => {
                dismissed.0 = true;
                game_state.set(GameState::Playing);
                *game_over_open = GameOverOpen(false);
            }
            MenuAction::Quit => {
                std::process::exit(0);
            }
            MenuAction::CycleYourFaction => {
                config.your_faction = next_faction(config.your_faction);
            }
            MenuAction::CycleEnemyFaction => {
                config.enemy_faction = next_faction(config.enemy_faction);
            }
            MenuAction::SetGrouping(g) => config.grouping = g,
            MenuAction::SetDifficulty(d) => config.difficulty = d,
            MenuAction::PickMap(i) => {
                config.map = Some(i);
                *page = MenuPage::AdvancedSkirmish;
            }
            MenuAction::PickRandomMap => {
                config.map = None;
                *page = MenuPage::AdvancedSkirmish;
            }
            MenuAction::ScrollReadme(lines) => {
                readme_scroll.0 = (readme_scroll.0 as isize + lines as isize).max(0) as usize;
            }
            MenuAction::Showcase(faction) => {
                commands.insert_resource(showcase_setup(faction));
                app_state.set(AppState::InGame);
            }
        }
    }
}

/// Faction cycle order from the original: System → Hacker → Network.
fn next_faction(f: Faction) -> Faction {
    match f {
        Faction::System => Faction::Hacker,
        Faction::Hacker => Faction::Network,
        Faction::Network => Faction::System,
    }
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
/// or Space activates the focused button. The focused button is drawn
/// brightened every frame, overriding transient mouse hover, so keyboard
/// users always see where they are.
fn keyboard_menu_nav(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Query<(Entity, &MenuButton, &Node)>,
    mut focus: ResMut<MenuFocus>,
    mut colors: Query<(&MenuButton, &mut BorderColor, &mut BackgroundColor)>,
    mut ev: MessageWriter<MenuActionMessage>,
) {
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
    if list.is_empty() {
        focus.entity = None;
        return;
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

    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let nav_key = keys.just_pressed(KeyCode::Tab)
        || keys.just_pressed(KeyCode::ArrowUp)
        || keys.just_pressed(KeyCode::ArrowDown)
        || keys.just_pressed(KeyCode::ArrowLeft)
        || keys.just_pressed(KeyCode::ArrowRight)
        || keys.just_pressed(KeyCode::Enter)
        || keys.just_pressed(KeyCode::Space);
    let confirm = keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space);

    if nav_key {
        focus.on_keyboard();
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

    focus.entity = Some(list[idx].0);

    // Confirm the focused button.
    if confirm {
        ev.write(MenuActionMessage {
            action: list[idx].1.action,
        });
    }

    // Paint only in keyboard mode, so mouse hover keeps working untouched.
    if focus.mode == InputMode::Keyboard {
        let (fidx, _, _, _) = list[idx];
        for (e, b, _, _) in &list {
            if let Ok((_, mut bc, mut bg)) = colors.get_mut(*e) {
                let c = if *e == fidx { brighten(b.base) } else { b.base };
                *bc = border(c);
                *bg = fill(c);
            }
        }
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
/// primary click on a button dispatches its [`MenuAction`].
fn mouse_menu_input(
    buttons: Query<(Entity, &MenuButton, &ComputedNode, &UiGlobalTransform)>,
    windows: Query<&Window>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut focus: ResMut<MenuFocus>,
    mut hovered: Local<Option<Entity>>,
    mut colors: Query<(&MenuButton, &mut BorderColor, &mut BackgroundColor)>,
    mut ev: MessageWriter<MenuActionMessage>,
) {
    let Ok(window) = windows.single() else { return };
    let Some(cursor) = window.cursor_position() else { return };
    // `ComputedNode::size`/`UiGlobalTransform` are in physical pixels, whereas
    // `cursor_position()` is logical — convert so the hit test is in one space.
    let phys = cursor * window.scale_factor();

    // Find the topmost button under the cursor using the UI node's canonical
    // hit test. Prefer the deepest node (larger `stack_index`).
    let mut hit: Option<Entity> = None;
    let mut hit_stack = 0u32;
    for (e, _b, cnode, gtf) in &buttons {
        if cnode.contains_point(*gtf, phys) {
            let idx = cnode.stack_index();
            if idx >= hit_stack {
                hit_stack = idx;
                hit = Some(e);
            }
        }
    }

    // If the hovered button changed, repaint the visual state.
    if *hovered != hit {
        if let Some(prev) = *hovered && let Ok((b, mut bc, mut bg)) = colors.get_mut(prev) {
            *bc = border(b.base);
            *bg = fill(b.base);
        }
        *hovered = hit;
        if let Some(cur) = hit && let Ok((b, mut bc, mut bg)) = colors.get_mut(cur) {
            *bc = border(brighten(b.base));
            *bg = fill(brighten(b.base));
        }
    }

    if let Some(ent) = hit {
        // Entering/keeping mouse mode: the keyboard focus no longer paints.
        if focus.entity != Some(ent) {
            focus.entity = Some(ent);
            focus.on_mouse();
        }
        if mouse.just_pressed(MouseButton::Left)
            && let Ok((_, b, ..)) = buttons.get(ent)
        {
            ev.write(MenuActionMessage { action: b.action });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn maintain_launch_menu(
    page: Res<MenuPage>,
    mut commands: Commands,
    existing_root: Query<Entity, With<MenuRoot>>,
    windows: Query<&Window>,
    mut last_page: Local<Option<MenuPage>>,
    catalog: Res<MapCatalog>,
    config: Res<SkirmishConfig>,
    readme: Res<ReadmeScroll>,
) {
    if last_page.is_some() && *last_page == Some(*page) && !existing_root.is_empty() {
        return;
    }
    *last_page = Some(*page);
    for e in &existing_root {
        commands.entity(e).despawn();
    }

    let Ok(window) = windows.single() else {
        return;
    };
    let (title_size, menu_size, page_size, list_size) = vsizes(window.height());

    let root = spawn_backdrop(&mut commands, MENU_GLASS);
    match *page {
        MenuPage::Main => main_menu_page(&mut commands, root, title_size, menu_size),
        MenuPage::QuickSkirmish => quick_skirmish_page(&mut commands, root, page_size),
        MenuPage::AdvancedSkirmish => advanced_skirmish_page(
            &mut commands,
            root,
            page_size,
            &config,
            &catalog.names(),
        ),
        MenuPage::MapList => map_list_page(&mut commands, root, list_size, &catalog),
        MenuPage::Showcase => showcase_page(&mut commands, root, page_size),
        MenuPage::Credits => credits_page(&mut commands, root, page_size),
        MenuPage::Readme => readme_page(&mut commands, root, window.height(), readme.0),
    }
}

/// The original's zigzag main menu (`MainMenu`): `vsy/20` buttons whose
/// bottom corners alternate between ending at x=46% (`rb`) and starting
/// at x=54% (`lb`), stepping down 10% of the screen per button.
fn main_menu_page(commands: &mut Commands, root: Entity, title_size: f32, menu_size: f32) {
    title(commands, root, title_size);
    let entries: [(&str, Color, MenuAction); 6] = [
        ("Skirmish", BUTTON_GREEN, MenuAction::Goto(MenuPage::AdvancedSkirmish)),
        ("Quick Battle", BUTTON_GREEN, MenuAction::Goto(MenuPage::QuickSkirmish)),
        ("Showcase", EASY_CYAN, MenuAction::Goto(MenuPage::Showcase)),
        ("Credits", BUTTON_GREEN, MenuAction::Goto(MenuPage::Credits)),
        ("Readme", BUTTON_GREEN, MenuAction::Goto(MenuPage::Readme)),
        ("Quit", BUTTON_GREEN, MenuAction::Quit),
    ];
    for (i, (name, color, action)) in entries.into_iter().enumerate() {
        let y = 0.7 - 0.1 * i as f32;
        let (x, anchor) = if i % 2 == 0 {
            (0.46, Anchor::Rb)
        } else {
            (0.54, Anchor::Lb)
        };
        button(commands, root, name, color, menu_size, (x, y), anchor, action);
    }
}

/// Page heading in the original's style: a blue plate at the top
/// centre (`AddFrame("Kernel Panic!\n<page>", {x=0.5, y=0.9}, vsy/24, {0,0,1}, "cc")`).
fn page_heading(commands: &mut Commands, root: Entity, page_size: f32, text: &str) {
    label(
        commands,
        root,
        &format!("Kernel Panic!\n{text}"),
        Some(NAV_BLUE),
        page_size,
        (0.5, 0.9),
        Anchor::Cc,
        Justify::Center,
    );
}

/// Pick a faction to see one of each of its units and buildings built
/// live on Data_Cache_L1. Laid out like the quick-battle column.
fn showcase_page(commands: &mut Commands, root: Entity, page_size: f32) {
    page_heading(commands, root, page_size, "Showcase");
    label(
        commands,
        root,
        "Pick a faction to see its full unit tree built live",
        None,
        page_size * 0.7,
        (0.5, 0.72),
        Anchor::Cc,
        Justify::Center,
    );
    for (i, (name, color, faction)) in [
        ("System", EASY_CYAN, Faction::System),
        ("Hacker", VERY_HARD_RED, Faction::Hacker),
        ("Network", DESC_BLUE, Faction::Network),
    ]
    .into_iter()
    .enumerate()
    {
        let e = button(
            commands,
            root,
            name,
            color,
            page_size,
            (0.5, 0.6 - 0.1 * i as f32),
            Anchor::Cc,
            MenuAction::Showcase(faction),
        );
        min_width(commands, e, 18.0);
    }
    back_button(commands, root, page_size, (0.5, 0.2), MenuPage::Main);
}

/// Blue `Back` plate, centred on `pos`.
fn back_button(
    commands: &mut Commands,
    root: Entity,
    font_size: f32,
    pos: (f32, f32),
    to: MenuPage,
) -> Entity {
    button(
        commands,
        root,
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
    let heading = button(
        commands,
        root,
        "Kernel Panic!\nSingle Player",
        NAV_BLUE,
        page_size,
        (0.5, 0.8),
        Anchor::Cc,
        MenuAction::Goto(MenuPage::AdvancedSkirmish),
    );
    min_width(commands, heading, 24.0);
    for (i, (name, color, difficulty)) in [
        ("Easy", MEDIUM_GREEN, 1),
        ("Medium", HARD_YELLOW, 2),
        ("Hard", EXTREME_ORANGE, 3),
        ("Very Hard", VERY_HARD_RED, 4),
    ]
    .into_iter()
    .enumerate()
    {
        let e = button(
            commands,
            root,
            name,
            color,
            page_size,
            (0.5, 0.6 - 0.1 * i as f32),
            Anchor::Cc,
            MenuAction::QuickStart(difficulty),
        );
        min_width(commands, e, 18.0);
    }
    back_button(commands, root, page_size, (0.5, 0.2), MenuPage::Main);
    label(
        commands,
        root,
        "Click the heading for the advanced setup",
        None,
        page_size * 0.6,
        (0.5, 0.1),
        Anchor::Cc,
        Justify::Center,
    );
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
    let heading = button(
        commands,
        root,
        "Kernel Panic!\nSingle Player",
        NAV_BLUE,
        page_size,
        (0.5, 0.9),
        Anchor::Cc,
        MenuAction::Goto(MenuPage::QuickSkirmish),
    );
    min_width(commands, heading, 24.0);

    let map_name = match config.map {
        Some(i) => map_names.get(i).map(String::as_str).unwrap_or("random"),
        None => "random",
    };
    let map = button(
        commands,
        root,
        &format!("Map: {map_name}"),
        MAP_GREEN,
        page_size,
        (0.5, 0.75),
        Anchor::Cc,
        MenuAction::Goto(MenuPage::MapList),
    );
    min_width(commands, map, 24.0);

    for (text, y, action) in [
        (format!("You:\n{:?}", config.your_faction), 0.6, MenuAction::CycleYourFaction),
        (format!("Enemy:\n{:?}", config.enemy_faction), 0.45, MenuAction::CycleEnemyFaction),
    ] {
        let e = button(commands, root, &text, EASY_CYAN, page_size, (0.5, y), Anchor::Cc, action);
        min_width(commands, e, 24.0);
    }

    // Grouping presets: left column. (Spectate / Team Game / Heroic are
    // not implemented yet — see `Grouping`.)
    for (i, (g, color)) in [
        (Grouping::Duel, MEDIUM_GREEN),
        (Grouping::Outgunned, EXTREME_ORANGE),
    ]
    .into_iter()
    .enumerate()
    {
        let e = choice_button(
            commands,
            root,
            g.label(),
            color,
            page_size,
            (0.1, 0.55 - 0.1 * i as f32),
            Anchor::Lc,
            MenuAction::SetGrouping(g),
            config.grouping == g,
        );
        min_width(commands, e, 16.0);
    }

    // Difficulty: right column.
    for (i, (name, color)) in [
        ("Easy", EASY_CYAN),
        ("Medium", MEDIUM_GREEN),
        ("Hard", HARD_YELLOW),
        ("Extreme", EXTREME_ORANGE),
    ]
    .into_iter()
    .enumerate()
    {
        let difficulty = 1 + i as u8;
        let e = choice_button(
            commands,
            root,
            name,
            color,
            page_size,
            (0.9, 0.6 - 0.1 * i as f32),
            Anchor::Rc,
            MenuAction::SetDifficulty(difficulty),
            config.difficulty == difficulty,
        );
        min_width(commands, e, 16.0);
    }

    let run = button(
        commands,
        root,
        "Run!",
        NAV_BLUE,
        page_size,
        (0.4, 0.2),
        Anchor::Cc,
        MenuAction::StartSkirmish,
    );
    min_width(commands, run, 12.0);
    let back = back_button(commands, root, page_size, (0.6, 0.2), MenuPage::Main);
    min_width(commands, back, 12.0);

    label(
        commands,
        root,
        &describe_setup(config),
        Some(DESC_BLUE),
        page_size * 0.8,
        (0.5, 0.05),
        Anchor::Cc,
        Justify::Center,
    );
}

/// The original's `ListMap`: two columns of map plates (`lc` at x=10%,
/// `rc` at x=90%) under a "Choose a map:" heading, Back at the bottom
/// — paired with the weighted-random pick like Run!/Back, since long
/// map names reach into the centre column.
fn map_list_page(commands: &mut Commands, root: Entity, list_size: f32, catalog: &MapCatalog) {
    label(
        commands,
        root,
        "Choose a map:",
        Some(NAV_BLUE),
        list_size * 28.0 / 24.0,
        (0.5, 0.95),
        Anchor::Cc,
        Justify::Center,
    );
    let nav_size = list_size * 28.0 / 24.0;
    let random = button(
        commands,
        root,
        "Random map",
        TEAL,
        nav_size,
        (0.4, 0.05),
        Anchor::Cc,
        MenuAction::PickRandomMap,
    );
    min_width(commands, random, 16.0);

    let names = catalog.names();
    let rows = names.len().div_ceil(2).max(1);
    // The original steps 10% per row from y=85%; longer catalogs
    // squeeze to keep the last row clear of Back.
    let step = (0.72 / rows as f32).min(0.1);
    for (i, name) in names.iter().enumerate() {
        let (col, row) = (i / rows, i % rows);
        let (x, anchor) = if col == 0 {
            (0.1, Anchor::Lc)
        } else {
            (0.9, Anchor::Rc)
        };
        let e = button(
            commands,
            root,
            name,
            BUTTON_GREEN,
            list_size,
            (x, 0.85 - step * row as f32),
            anchor,
            MenuAction::PickMap(i),
        );
        min_width(commands, e, 30.0);
    }
    let back = back_button(commands, root, nav_size, (0.6, 0.05), MenuPage::AdvancedSkirmish);
    min_width(commands, back, 16.0);
}

/// The original's `Credits`: heading plate sitting on y=80%, the credit
/// lines hanging below it, the engine credit around y=30%, Back at 10%.
fn credits_page(commands: &mut Commands, root: Entity, page_size: f32) {
    label(
        commands,
        root,
        "Kernel Panic!\nCredits:",
        Some(MEDIUM_GREEN),
        page_size,
        (0.5, 0.8),
        Anchor::Cb,
        Justify::Center,
    );
    const CREDITS: &str = "\
- Original concept by Boirunner
- About all the work done by KDR_11k
- Maintenance and silly mod options by zwzsg
- Sounds by Noruas and Pendrokar
- Voices by Eva and Panda
- Maps by Boirunner, Runecrafter, zwzsg, TradeMark, KDR_11k and FireStorm
- Some LUA interface upgrade based of jK and trepan code
- The Touhou faction characters were inspired by ZUN's works
- Many thanks to lurker, Quantum, and the rest of #lua crew
- Reimplementation in Rust + Bevy, from the original Spring mod";
    label(
        commands,
        root,
        CREDITS,
        Some(TEAL),
        page_size * 24.0 / 30.0,
        (0.5, 0.78),
        Anchor::Ct,
        Justify::Left,
    );
    label(
        commands,
        root,
        "Spring Engine by:",
        Some(EXTREME_ORANGE),
        page_size,
        (0.5, 0.28),
        Anchor::Cb,
        Justify::Center,
    );
    label(
        commands,
        root,
        "Swedish Yankspankers",
        Some(README_UPDOWN),
        page_size * 24.0 / 30.0,
        (0.5, 0.28),
        Anchor::Ct,
        Justify::Center,
    );
    back_button(commands, root, page_size * 24.0 / 30.0, (0.5, 0.1), MenuPage::Main);
}

/// The original's `PrintReadMe`: file name plate at the top centre, the
/// text panel filling the screen below it, Up in both top corners, Down
/// at 30% / 70% along the bottom edge and Back between them.
fn readme_page(commands: &mut Commands, root: Entity, window_h: f32, scroll: usize) {
    let page_size = window_h / 24.0;
    label(
        commands,
        root,
        "Kernel_Panic_readme.txt",
        Some(Color::srgb(1.0, 1.0, 0.5)),
        window_h / 32.0,
        (0.5, 1.0),
        Anchor::Ct,
        Justify::Center,
    );

    // The original prints 16 px lines at ~1080p; scale with the window.
    let line_px = (window_h / 64.0).max(12.0);
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
                font_size: line_px,
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
            button(
                commands,
                root,
                "Up",
                README_UPDOWN,
                page_size,
                (x, 1.0),
                anchor,
                MenuAction::ScrollReadme(-page),
            );
        }
    }
    if scroll < max_scroll {
        for x in [0.3, 0.7] {
            button(
                commands,
                root,
                "Down",
                README_UPDOWN,
                page_size,
                (x, 0.0),
                Anchor::Cb,
                MenuAction::ScrollReadme(page),
            );
        }
    }
    button(
        commands,
        root,
        "Back",
        NAV_BLUE,
        page_size,
        (0.5, 0.0),
        Anchor::Cb,
        MenuAction::Goto(MenuPage::Main),
    );
}

/// Readme content, cached; falls back to a friendly note if the asset is
/// missing. CRLF is normalised and long runs of blank lines collapse to
/// a single spacer, mirroring the original's parse.
fn readme_lines() -> Vec<String> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Vec<String>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let path = crate::paths::from_project_root("kernel-panic/assets/readme.txt");
            match std::fs::read(&path) {
                Ok(bytes) => {
                    // The upstream readme is ISO-8859-1, which
                    // `read_to_string` rejects as invalid UTF-8. Latin-1
                    // maps every byte 1:1 onto U+0000..=U+00FF, so decode
                    // non-UTF-8 bytes by widening them — umlauts and
                    // friends survive intact.
                    let text = String::from_utf8(bytes).unwrap_or_else(|error| {
                        error
                            .into_bytes()
                            .into_iter()
                            .map(|b| b as char)
                            .collect()
                    });
                    let mut out: Vec<String> = Vec::new();
                    for line in text.replace("\r\n", "\n").split('\n') {
                        let line = line.trim_end();
                        if line.is_empty()
                            && out.last().is_some_and(|last: &String| last.is_empty())
                        {
                            continue;
                        }
                        out.push(line.to_string());
                    }
                    out
                }
                Err(_) => vec![
                    "File Kernel_Panic_readme.txt not found!".to_string(),
                    "(expected at kernel-panic/assets/readme.txt)".to_string(),
                ],
            }
        })
        .clone()
}

// ---------------------------------------------------------------------------
// Esc overlay + game over (AppState::InGame) — the game keeps running
// ---------------------------------------------------------------------------

fn esc_toggle(
    keys: Res<ButtonInput<KeyCode>>,
    mut esc_open: ResMut<EscMenuOpen>,
    game_over_open: Res<GameOverOpen>,
) {
    if keys.just_pressed(KeyCode::Escape) && !game_over_open.0 {
        esc_open.0 = !esc_open.0;
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

fn close_all_overlays(mut esc_open: ResMut<EscMenuOpen>, mut game_over: ResMut<GameOverOpen>) {
    *esc_open = EscMenuOpen(false);
    *game_over = GameOverOpen(false);
}

fn maintain_esc_menu(
    esc_open: Res<EscMenuOpen>,
    mut commands: Commands,
    existing_root: Query<Entity, (With<MenuRoot>, Without<GameOverPanel>)>,
    windows: Query<&Window>,
    mut last: Local<bool>,
) {
    if *last == esc_open.0 {
        return;
    }
    *last = esc_open.0;
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

    // The original's `SaveLoadMenu`: four big plates meeting around the
    // screen centre — Save/Load on top (here Resume/Restart; there is no
    // save system), Menu/Restart below (here Menu/Quit).
    let root = spawn_backdrop(&mut commands, OVERLAY_GLASS);
    for (text, pos, anchor, action) in [
        ("Resume", (0.45, 0.505), Anchor::Rb, MenuAction::Resume),
        ("Restart", (0.55, 0.505), Anchor::Lb, MenuAction::Restart),
        ("Menu", (0.45, 0.5), Anchor::Rt, MenuAction::GoToMenu),
        ("Quit", (0.55, 0.5), Anchor::Lt, MenuAction::Quit),
    ] {
        let e = button(&mut commands, root, text, BUTTON_GREEN, size, pos, anchor, action);
        min_width(&mut commands, e, 22.0);
    }
}

#[derive(Component)]
struct GameOverPanel;

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

fn maintain_game_over(
    game_over_open: Res<GameOverOpen>,
    game_state: Res<State<GameState>>,
    mut commands: Commands,
    existing_root: Query<Entity, With<GameOverPanel>>,
    windows: Query<&Window>,
    mut last: Local<bool>,
) {
    if *last == game_over_open.0 {
        return;
    }
    *last = game_over_open.0;
    for e in &existing_root {
        commands.entity(e).despawn();
    }
    if !game_over_open.0 {
        return;
    }
    let Ok(window) = windows.single() else {
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

    // The original's `GameOverMenu`: result plate at y=70%, then
    // "Keep on playing/watching" ending at x=48% and "Go to Menu"
    // starting at x=52% on the y=25% line; a loss adds Restart above.
    label(
        &mut commands,
        root,
        if won { "You won!" } else { "You lost!" },
        Some(if won { WON_GREEN } else { LOST_RED }),
        title_size,
        (0.5, 0.7),
        Anchor::Cc,
        Justify::Center,
    );
    let keep = if won { "Keep on playing" } else { "Keep on watching" };
    button(
        &mut commands,
        root,
        keep,
        BUTTON_GREEN,
        menu_size,
        (0.48, 0.25),
        Anchor::Rc,
        MenuAction::KeepPlaying,
    );
    button(
        &mut commands,
        root,
        "Go to Menu",
        BUTTON_GREEN,
        menu_size,
        (0.52, 0.25),
        Anchor::Lc,
        MenuAction::GoToMenu,
    );
    if !won {
        button(
            &mut commands,
            root,
            "Restart",
            BUTTON_GREEN,
            menu_size,
            (0.5, 0.35),
            Anchor::Cc,
            MenuAction::Restart,
        );
    }
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
    if director.decided_for > DEMO_DECIDED_GRACE || director.age > DEMO_MAX_AGE {
        commands.insert_resource(demo_setup());
        run_game.write(RunGame);
        *director = DemoDirector::default();
    }
}

/// The attract-mode camera: glides between points of interest — a unit
/// that is currently aiming at something (a fight), else a homebase —
/// while slowly orbiting, like a spectator director.
#[derive(Resource, Default)]
struct AttractCamera {
    /// Seconds until the next point of interest is picked.
    retarget_in: f32,
    target: Option<Vec3>,
    clock: f32,
}

/// Seconds between points of interest.
const ATTRACT_DWELL: f32 = 14.0;
/// Max glide speed of the focus between points (elmos/s).
const ATTRACT_GLIDE_SPEED: f32 = 260.0;
/// Orbit speed (rad/s).
const ATTRACT_ORBIT_SPEED: f32 = 0.06;

#[allow(clippy::too_many_arguments)]
fn attract_camera(
    time: Res<Time>,
    mut director: ResMut<AttractCamera>,
    bounds: Res<MapBounds>,
    fighters: Query<&GlobalTransform, (With<AimTarget>, With<UnitType>)>,
    bases: Query<&GlobalTransform, With<Homebase>>,
    mut cam: Query<&mut RtsCameraState, With<RtsCamera>>,
) {
    let Ok(mut state) = cam.single_mut() else {
        return;
    };
    let dt = time.delta_secs();
    director.clock += dt;
    // A freshly loaded map invalidates the old point of interest.
    if bounds.is_changed() {
        director.target = None;
        director.retarget_in = 0.0;
    }
    director.retarget_in -= dt;
    if director.retarget_in <= 0.0 || director.target.is_none() {
        director.retarget_in = ATTRACT_DWELL;
        let pick = |n: usize| ((rand_01() * n as f32) as usize).min(n.saturating_sub(1));
        let fighting: Vec<Vec3> = fighters.iter().map(|g| g.translation()).collect();
        let homes: Vec<Vec3> = bases.iter().map(|g| g.translation()).collect();
        director.target = if !fighting.is_empty() {
            Some(fighting[pick(fighting.len())])
        } else if !homes.is_empty() {
            Some(homes[pick(homes.len())])
        } else {
            Some((bounds.min + bounds.max) * 0.5)
        };
    }
    if let Some(target) = director.target {
        let to = target - state.focus;
        let step = ATTRACT_GLIDE_SPEED * dt;
        state.focus = if to.length() <= step {
            target
        } else {
            state.focus + to.normalize() * step
        };
    }
    state.yaw += ATTRACT_ORBIT_SPEED * dt;
    state.pitch = 0.62;
    // Slow breathing zoom so the shot doesn't feel static.
    state.distance = 1250.0 + 250.0 * (director.clock * 0.07).sin();
}

/// Deterministic-enough per-call jitter (menu demo only; gameplay uses
/// no randomness).
///
/// Uses Bevy's `Instant`, not `std::time`: `SystemTime`/`Instant` panic
/// with "time not supported on this platform" on wasm32, where Bevy's
/// is `performance.now()`-backed instead.
fn rand_01() -> f32 {
    use bevy::platform::time::Instant;
    thread_local! {
        static STATE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
        static ANCHOR: Instant = Instant::now();
    }
    STATE.with(|s| {
        let mut x = s.get();
        if x == 0 {
            // No epoch on Instant — seed from nanos elapsed since this
            // thread's first call, mixed with a fixed odd constant.
            let elapsed = ANCHOR.with(|a| a.elapsed());
            x = ((elapsed.subsec_nanos() as u64) ^ (elapsed.as_secs() << 17) ^ 0x853C49E6748FEA9B)
                | 1;
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        (x >> 40) as f32 / (1u64 << 24) as f32
    })
}

/// On first boot: once the map catalog exists, load the demo world
/// behind the launch menu.
/// First-frame boot: seed the attract-mode demo. Public so
/// `map_loading` can order its `RunGame` reload after this system —
/// the reload must observe the `demo_setup` insert this writes.
pub fn boot_demo(
    mut done: Local<bool>,
    mut commands: Commands,
    mut run_game: MessageWriter<RunGame>,
) {
    if *done {
        return;
    }
    *done = true;
    commands.insert_resource(demo_setup());
    run_game.write(RunGame);
}
