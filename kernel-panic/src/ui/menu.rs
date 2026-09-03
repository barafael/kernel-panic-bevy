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
    build_setup, describe_setup, demo_setup, AppState, GameOverDismissed, Grouping, RunGame,
    SkirmishConfig,
};
use crate::map_loading::MapCatalog;
use crate::terrain::heightmap::Heightmap;
use crate::units::components::{Faction, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::lifecycle::game_over::GameState;
use crate::units::lifecycle::spawning::{spawn_unit, SpawnContext};

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MenuPage>()
            .init_resource::<EscMenuOpen>()
            .init_resource::<GameOverOpen>()
            .init_resource::<ReadmeScroll>()
            .init_resource::<DemoDirector>()
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
                    demo_director
                        .run_if(in_state(AppState::Menu).and(resource_exists::<Heightmap>)),
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
pub enum MenuPage {
    #[default]
    Main,
    QuickSkirmish,
    AdvancedSkirmish,
    MapList,
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

/// One skewed-plate stand-in: bordered, tinted, white text, absolute at
/// screen-relative coordinates. Clicks go to the central action router.
#[allow(clippy::too_many_arguments)]
fn button(
    commands: &mut Commands,
    parent: Entity,
    label: &str,
    color: Color,
    font_size: f32,
    x_pct: f32,
    y_pct: f32,
    action: MenuAction,
    // None = left-anchored (box starts at x); Some(r) = right-anchored
    // (box ends at x, i.e. right edge at 100-r).
    right_anchor: Option<f32>,
) -> Entity {
    let text = commands
        .spawn((
            Text::new(label),
            TextColor(TEXT_WHITE),
            TextFont {
                font_size,
                ..default()
            },
            Pickable::IGNORE,
        ))
        .id();
    let entity = commands
        .spawn((
            MenuButton { action, base: color },
            Node {
                position_type: PositionType::Absolute,
                left: if right_anchor.is_none() {
                    Val::Percent(x_pct)
                } else {
                    Val::Auto
                },
                right: right_anchor.map(Val::Percent).unwrap_or(Val::Auto),
                top: Val::Percent(y_pct),
                padding: UiRect::all(Val::Px(font_size * 0.25)),
                border: UiRect::all(Val::Px((font_size * 0.09).max(1.0))),
                ..default()
            },
            fill(color),
            border(color),
        ))
        .id();
    commands.entity(entity).add_child(text);

    // Click + hover through bevy_picking, same as the HUD build icons.
    // Click + hover through bevy_picking, same as the HUD build icons.
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

/// Non-interactive text panel (titles, description lines).
#[allow(clippy::too_many_arguments)]
fn label(
    commands: &mut Commands,
    parent: Entity,
    text: &str,
    color: Color,
    font_size: f32,
    x_pct: f32,
    y_pct: f32,
    centered: bool,
) -> Entity {
    let text = commands
        .spawn((
            Text::new(text),
            TextColor(TEXT_WHITE),
            TextFont {
                font_size,
                ..default()
            },
            Pickable::IGNORE,
            TextLayout::new_with_justify(if centered {
                Justify::Center
            } else {
                Justify::Left
            }),
        ))
        .id();
    let entity = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                // Centered labels span the full window so the text's
                // Justify::Center has something to center within; a
                // shrink-to-fit node would just sit at `left` with the
                // text left-aligned inside it.
                left: if centered {
                    Val::Percent(0.0)
                } else {
                    Val::Percent(x_pct)
                },
                top: Val::Percent(y_pct),
                width: if centered {
                    Val::Percent(100.0)
                } else {
                    Val::Auto
                },
                padding: UiRect::all(Val::Px(font_size * 0.15)),
                ..default()
            },
            Pickable::IGNORE,
        ))
        .id();
    commands.entity(entity).add_child(text);
    let _ = color; // kept for call-site readability; text stays white like the original
    commands.entity(parent).add_child(entity);
    entity
}

/// The `Kernel Panic!` title, cyan, vsy/14 — per the original.
fn title(commands: &mut Commands, parent: Entity, font_size: f32, suffix: &str) {
    let text = if suffix.is_empty() {
        "Kernel Panic!".to_string()
    } else {
        format!("Kernel Panic!\n{suffix}")
    };
    let t = commands
        .spawn((
            Text::new(text),
            TextColor(TITLE_CYAN),
            TextFont {
                font_size,
                ..default()
            },
            Pickable::IGNORE,
            TextLayout::new_with_justify(Justify::Center),
        ))
        .id();
    let entity = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(0.0),
                top: Val::Percent(2.0),
                width: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                ..default()
            },
            Pickable::IGNORE,
        ))
        .id();
    commands.entity(entity).add_child(t);
    commands.entity(parent).add_child(entity);
}

/// Window-height-derived font sizes, matching the original's vsy ratios.
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
    let (_title_size, menu_size, page_size, list_size) = vsizes(window.height());

    let root = spawn_backdrop(&mut commands, MENU_GLASS);
    match *page {
        MenuPage::Main => main_menu_page(&mut commands, root, menu_size),
        MenuPage::QuickSkirmish => quick_skirmish_page(&mut commands, root, page_size),
        MenuPage::AdvancedSkirmish => advanced_skirmish_page(
            &mut commands,
            root,
            page_size,
            &config,
            &catalog.names(),
        ),
        MenuPage::MapList => map_list_page(&mut commands, root, list_size, &catalog),
        MenuPage::Credits => credits_page(&mut commands, root, page_size),
        MenuPage::Readme => readme_page(&mut commands, root, window.height(), readme.0),
    }
}

/// The original's zigzag main menu: buttons alternate between ending at
/// x=46% and starting at x=54%, marching down the screen.
fn main_menu_page(commands: &mut Commands, root: Entity, menu_size: f32) {
    title(commands, root, menu_size * 20.0 / 14.0, "");
    // Zigzag layout, alternating left/right anchors like the original.
    // All five buttons fit between the title and the bottom edge: at
    // vsy/20 fonts each button is ~7% of the window tall, so 10% steps
    // ending at 66% + 7% clear the bottom with room to spare.
    button(
        commands,
        root,
        "Skirmish",
        BUTTON_GREEN,
        menu_size,
        54.0,
        28.0,
        MenuAction::Goto(MenuPage::AdvancedSkirmish),
        None,
    );
    button(
        commands,
        root,
        "Quick Battle",
        BUTTON_GREEN,
        menu_size,
        46.0,
        38.0,
        MenuAction::Goto(MenuPage::QuickSkirmish),
        Some(54.0),
    );
    button(
        commands,
        root,
        "Credits",
        BUTTON_GREEN,
        menu_size,
        54.0,
        48.0,
        MenuAction::Goto(MenuPage::Credits),
        None,
    );
    button(
        commands,
        root,
        "Readme",
        BUTTON_GREEN,
        menu_size,
        46.0,
        58.0,
        MenuAction::Goto(MenuPage::Readme),
        Some(54.0),
    );
    button(
        commands,
        root,
        "Quit",
        BUTTON_GREEN,
        menu_size,
        54.0,
        68.0,
        MenuAction::Quit,
        None,
    );
}

/// The original's `SimplerSinglePlayer`: title doubles as the toggle to
/// the advanced page; every difficulty button starts a random-map Duel
/// immediately.
fn quick_skirmish_page(commands: &mut Commands, root: Entity, page_size: f32) {
    title(commands, root, page_size, "Single Player");
    button(
        commands,
        root,
        "Advanced setup…",
        NAV_BLUE,
        page_size * 0.8,
        50.0,
        12.0,
        MenuAction::Goto(MenuPage::AdvancedSkirmish),
        Some(50.0),
    );

    button(
        commands,
        root,
        "Easy",
        EASY_CYAN,
        page_size,
        50.0,
        32.0,
        MenuAction::QuickStart(1),
        Some(50.0),
    );
    button(
        commands,
        root,
        "Medium",
        MEDIUM_GREEN,
        page_size,
        50.0,
        42.0,
        MenuAction::QuickStart(2),
        Some(50.0),
    );
    button(
        commands,
        root,
        "Hard",
        HARD_YELLOW,
        page_size,
        50.0,
        52.0,
        MenuAction::QuickStart(3),
        Some(50.0),
    );
    button(
        commands,
        root,
        "Very Hard",
        VERY_HARD_RED,
        page_size,
        50.0,
        62.0,
        MenuAction::QuickStart(4),
        Some(50.0),
    );
    button(
        commands,
        root,
        "Back",
        NAV_BLUE,
        page_size,
        50.0,
        75.0,
        MenuAction::Goto(MenuPage::Main),
        Some(50.0),
    );
}

/// The original's `SinglePlayer` advanced page.
fn advanced_skirmish_page(
    commands: &mut Commands,
    root: Entity,
    page_size: f32,
    config: &SkirmishConfig,
    map_names: &[String],
) {
    title(commands, root, page_size, "Single Player");
    let map_name = match config.map {
        Some(i) => map_names.get(i).map(String::as_str).unwrap_or("random"),
        None => "random",
    };
    label(
        commands,
        root,
        &format!("Map: {map_name}"),
        MAP_GREEN,
        page_size,
        42.0,
        12.0,
        false,
    );
    button(
        commands,
        root,
        "Choose map…",
        MAP_GREEN,
        page_size * 0.85,
        58.0,
        12.4,
        MenuAction::Goto(MenuPage::MapList),
        None,
    );
    button(
        commands,
        root,
        "Random map",
        MAP_GREEN,
        page_size * 0.85,
        42.0,
        19.0,
        MenuAction::PickRandomMap,
        None,
    );

    button(
        commands,
        root,
        &format!("You:\n{:?}", config.your_faction),
        EASY_CYAN,
        page_size,
        50.0,
        25.0,
        MenuAction::CycleYourFaction,
        Some(50.0),
    );
    button(
        commands,
        root,
        &format!("Enemy:\n{:?}", config.enemy_faction),
        EASY_CYAN,
        page_size,
        50.0,
        40.0,
        MenuAction::CycleEnemyFaction,
        Some(50.0),
    );

    // Grouping presets — left column at x=10%.
    for (i, (g, color)) in [
        (Grouping::Duel, MEDIUM_GREEN),
        (Grouping::Outgunned, EXTREME_ORANGE),
    ]
    .into_iter()
    .enumerate()
    {
        button(
            commands,
            root,
            g.label(),
            color,
            page_size,
            10.0,
            30.0 + 8.0 * i as f32,
            MenuAction::SetGrouping(g),
            None,
        );
    }

    // Difficulty — right column ending at x=90%.
    for (i, (name, color)) in [
        ("Easy", EASY_CYAN),
        ("Medium", MEDIUM_GREEN),
        ("Hard", HARD_YELLOW),
        ("Extreme", EXTREME_ORANGE),
    ]
    .into_iter()
    .enumerate()
    {
        button(
            commands,
            root,
            name,
            color,
            page_size,
            90.0,
            30.0 + 8.0 * i as f32,
            MenuAction::SetDifficulty(1 + i as u8),
            Some(10.0),
        );
    }

    button(
        commands,
        root,
        "Run!",
        NAV_BLUE,
        page_size,
        40.0,
        68.0,
        MenuAction::StartSkirmish,
        None,
    );
    button(
        commands,
        root,
        "Back",
        NAV_BLUE,
        page_size,
        60.0,
        68.0,
        MenuAction::Goto(MenuPage::Main),
        None,
    );

    // Live description line, in the original's format.
    label(
        commands,
        root,
        &describe_setup(config),
        DESC_BLUE,
        page_size * 0.9,
        50.0,
        82.0,
        true,
    );
    let _ = page_size;
}

/// Two-column map list; "Random" first. (Minimap previews are a noted
/// future improvement — they need an off-thread map parse.)
fn map_list_page(commands: &mut Commands, root: Entity, list_size: f32, catalog: &MapCatalog) {
    title(commands, root, list_size * 24.0 / 28.0, "");
    label(commands, root, "Choose a map:", NAV_BLUE, list_size, 46.0, 8.0, false);

    button(
        commands,
        root,
        "Random (weighted)",
        TEAL,
        list_size,
        10.0,
        16.0,
        MenuAction::PickRandomMap,
        None,
    );

    let names = catalog.names();
    let half = names.len().div_ceil(2);
    for (i, name) in names.iter().enumerate() {
        let (x, anchor) = if i < half {
            (10.0, None)
        } else {
            (90.0, Some(10.0))
        };
        let row = if i < half { i } else { i - half };
        button(
            commands,
            root,
            name,
            BUTTON_GREEN,
            list_size,
            x,
            24.0 + row as f32 * 7.0,
            MenuAction::PickMap(i),
            anchor,
        );
    }

    button(
        commands,
        root,
        "Back",
        NAV_BLUE,
        list_size,
        50.0,
        90.0,
        MenuAction::Goto(MenuPage::AdvancedSkirmish),
        Some(50.0),
    );
}

fn credits_page(commands: &mut Commands, root: Entity, page_size: f32) {
    title(commands, root, page_size, "Credits:");
    const CREDITS: &str = "\
- Original concept by Boirunner
- About all the work done by KDR_11k
- Maintenance and silly mod options by zwzsg
- Sounds by Noruas and Pendrokar
- Voices by Eva and Panda
- Maps by Boirunner, Runecrafter, zwzsg, TradeMark, KDR_11k and FireStorm
- Some LUA interface upgrade based of jK and trepan code
- The Touhou faction characters were inspired by ZUN's works

Reimplementation: Rust + Bevy, from the original Spring mod.";
    label(commands, root, CREDITS, BUTTON_GREEN, page_size * 0.8, 30.0, 18.0, true);
    label(
        commands,
        root,
        "Original engine: Spring RTS (the mod runs on it unmodified)",
        EXTREME_ORANGE,
        page_size * 0.7,
        50.0,
        75.0,
        true,
    );
    button(
        commands,
        root,
        "Back",
        NAV_BLUE,
        page_size,
        50.0,
        88.0,
        MenuAction::Goto(MenuPage::Main),
        Some(50.0),
    );
}

/// Readme page: body slice + Up/Down (orange) + Back, per the original's
/// `PrintReadMe` (wheel scrolling is a noted TODO — bevy_picking scroll
/// events aren't wired for UI nodes here yet).
fn readme_page(commands: &mut Commands, root: Entity, window_h: f32, scroll: usize) {
    let page_size = window_h / 24.0;
    title(commands, root, page_size, "");
    label(commands, root, "Kernel_Panic_readme.txt", BUTTON_GREEN, page_size, 44.0, 6.0, false);

    const LINE_HEIGHT: f32 = 16.0;
    let lines_per_screen = ((window_h * 0.70) / LINE_HEIGHT) as usize;
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
                font_size: LINE_HEIGHT,
                ..default()
            },
            Pickable::IGNORE,
        ))
        .id();
    let body_panel = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(8.0),
                top: Val::Percent(12.0),
                width: Val::Percent(84.0),
                height: Val::Percent(70.0),
                overflow: Overflow::clip(),
                padding: UiRect::all(Val::Px(8.0)),
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
    button(
        commands,
        root,
        "Up",
        README_UPDOWN,
        page_size * 0.8,
        0.0,
        12.0,
        MenuAction::ScrollReadme(-page),
        None,
    );
    button(
        commands,
        root,
        "Up",
        README_UPDOWN,
        page_size * 0.8,
        100.0,
        12.0,
        MenuAction::ScrollReadme(-page),
        Some(0.0),
    );
    button(
        commands,
        root,
        "Down",
        README_UPDOWN,
        page_size * 0.8,
        30.0,
        84.0,
        MenuAction::ScrollReadme(page),
        None,
    );
    button(
        commands,
        root,
        "Down",
        README_UPDOWN,
        page_size * 0.8,
        70.0,
        84.0,
        MenuAction::ScrollReadme(page),
        None,
    );
    button(
        commands,
        root,
        "Back",
        NAV_BLUE,
        page_size,
        50.0,
        92.0,
        MenuAction::Goto(MenuPage::Main),
        Some(50.0),
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
    let menu_size = window.height() / 20.0;

    // The original's SaveLoadMenu cluster: big green buttons around
    // screen centre (Save/Load omitted — no save system yet).
    let root = spawn_backdrop(&mut commands, OVERLAY_GLASS);
    button(
        &mut commands,
        root,
        "Resume",
        BUTTON_GREEN,
        menu_size * 1.4,
        50.0,
        42.0,
        MenuAction::Resume,
        Some(50.0),
    );
    button(
        &mut commands,
        root,
        "Restart",
        BUTTON_GREEN,
        menu_size * 1.4,
        50.0,
        52.0,
        MenuAction::Restart,
        Some(50.0),
    );
    button(
        &mut commands,
        root,
        "Menu",
        BUTTON_GREEN,
        menu_size * 1.4,
        50.0,
        62.0,
        MenuAction::GoToMenu,
        Some(50.0),
    );
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

    label(
        &mut commands,
        root,
        if won { "You won!" } else { "You lost!" },
        if won { WON_GREEN } else { LOST_RED },
        title_size,
        50.0,
        30.0,
        true,
    );

    if won {
        button(
            &mut commands,
            root,
            "Keep on playing",
            BUTTON_GREEN,
            menu_size,
            46.0,
            70.0,
            MenuAction::KeepPlaying,
            Some(52.0),
        );
    } else {
        button(
            &mut commands,
            root,
            "Restart",
            BUTTON_GREEN,
            menu_size,
            48.0,
            70.0,
            MenuAction::Restart,
            Some(52.0),
        );
    }
    button(
        &mut commands,
        root,
        "Go to Menu",
        BUTTON_GREEN,
        menu_size,
        52.0,
        70.0,
        MenuAction::GoToMenu,
        None,
    );
}

// (no trailing helpers)

// ---------------------------------------------------------------------------
// Attract-mode demo — live battle behind the main menu
// ---------------------------------------------------------------------------

/// The demo cast: a battery of Pointers on the west side that auto-fire
/// at Flows streaming in from the east. Every lost unit is replaced, so
/// the battle runs forever.
#[derive(Resource, Default)]
struct DemoDirector {
    /// Desired Pointer battery: (live entity, home position). A `None`
    /// or dead entity is respawned at its position.
    pointer_slots: Vec<(Option<Entity>, Vec3)>,
    flow_timer: f32,
}

/// Marker on demo Flows, for counting the live swarm.
#[derive(Component)]
struct DemoFlow;

const DEMO_FLOW_CAP: usize = 10;
const DEMO_FLOW_SPAWN_INTERVAL: f32 = 1.5;

#[allow(clippy::too_many_arguments)]
fn demo_director(
    time: Res<Time>,
    mut director: ResMut<DemoDirector>,
    heightmap: Option<Res<Heightmap>>,
    units: Query<(), With<UnitType>>,
    flows: Query<(), With<DemoFlow>>,
    mut ctx: SpawnContext,
) {
    // The first frame after a world (re)load may still see the previous
    // match's heightmap — spawning into a stale map self-corrects: the
    // RunGame teardown despawns those actors and the slots refill here.
    let Some(heightmap) = heightmap else {
        return;
    };
    let (w, d) = heightmap.world_size();

    // Pointer battery (west half), initialised once per director life.
    if director.pointer_slots.is_empty() {
        for i in 0..6 {
            let x = w * (0.16 + 0.05 * (i % 3) as f32);
            let z = d * (0.28 + 0.22 * (i / 3) as f32);
            director.pointer_slots.push((None, Vec3::new(x, 0.0, z)));
        }
    }

    // Replace lost Pointers at their home position — the user-visible
    // rule "if any unit dies, it is replaced".
    for slot in &mut director.pointer_slots {
        let alive = slot
            .0
            .is_some_and(|e| units.get(e).is_ok());
        if !alive {
            let pos = heightmap.place(slot.1.x, slot.1.z);
            slot.0 = Some(spawn_unit(
                UnitKind::Pointer,
                Faction::System,
                0,
                pos,
                &mut ctx,
            ));
        }
    }

    // Flow swarm (east half): spawn on a timer while under the cap.
    director.flow_timer -= time.delta_secs();
    if director.flow_timer <= 0.0 {
        director.flow_timer = DEMO_FLOW_SPAWN_INTERVAL;
        if flows.iter().count() < DEMO_FLOW_CAP {
            let x = w * (0.62 + 0.25 * rand_01());
            let z = d * (0.15 + 0.70 * rand_01());
            let ground = heightmap.place(x, z);
            let alt = ctx
                .unit_registry
                .cruise_alt(UnitKind::Flow)
                .max(60.0);
            let entity = spawn_unit(
                UnitKind::Flow,
                Faction::Network,
                1,
                ground + Vec3::Y * alt,
                &mut ctx,
            );
            ctx.commands.entity(entity).insert(DemoFlow);
        }
    }
}

/// Deterministic-enough per-call jitter (menu demo only; gameplay uses
/// no randomness).
fn rand_01() -> f32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    thread_local! {
        static STATE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    STATE.with(|s| {
        let mut x = s.get();
        if x == 0 {
            x = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64 ^ (d.as_secs() << 17))
                .unwrap_or(0x853C49E6748FEA9B)
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
