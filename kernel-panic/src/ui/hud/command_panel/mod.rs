//! The unit command panel — a port of Spring's built-in control panel
//! (`CGuiHandler`) as Kernel Panic configures it (`KP_CtrlPanel.txt`).
//!
//! One 3×9 grid at the left edge lists every order the selection
//! supports, then its build options ([`commands`]). It stays up as long
//! as something with commands is selected — clicking a button never
//! closes it. Geometry, hit testing and paging follow `LoadConfig` /
//! `LayoutIcons` / `IconAtPos` ([`layout`]); drawing follows
//! `DrawButtons`: build options and textured commands show their
//! picture, the rest their name scaled to fill the button inside a faint
//! frame; state buttons carry option LEDs; the button under the cursor
//! gets a blue wash and a white outline, the armed command a red wash and
//! a yellow outline, disabled commands are darkened.
//!
//! Clicks follow `MousePress` / `MouseRelease`: the press on a button is
//! the panel's (it never reaches the map), and the command runs on the
//! release over the *same* button. A right press anywhere cancels the
//! armed command, as does Escape. Everything funnels into
//! [`activation::ActivateCommand`], shared with the hotkeys and the build
//! bar.

pub(crate) mod activation;
pub(crate) mod commands;
pub(crate) mod layout;

use bevy::input::InputSystems;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::game_setup::AppState;
use crate::interaction::ability::OrderCursorModes;
use crate::interaction::selection::{Selected, SelectionSet};
use crate::units::combat::Dying;
use crate::units::components::{TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::lifecycle::bookkeeping::team_kind_count;
use crate::units::lifecycle::production::Producer;
use crate::units::mechanics::command_fire::CommandFireCooldown;
use crate::units::mechanics::worm::AutoHold;

use super::placement::PlacementMode;
use super::previews::UnitPreviews;
use super::tooltip::HoverTip;
use activation::{ActivateCommand, apply_activations, disarm, order_hotkeys, sync_active};
use commands::{CmdDesc, CmdId, CmdType, Texture, UnitCmdState, available_commands};
use layout::{KP_CTRL_PANEL, SlotCmd, fit_font_size};

pub(super) struct CommandPanelPlugin;

impl Plugin for CommandPanelPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<ActivateCommand>()
            .init_resource::<PanelCommands>()
            .init_resource::<PanelPage>()
            .init_resource::<ActiveCommand>()
            .init_resource::<PanelInput>()
            .init_resource::<PanelPics>()
            // Mouse input runs before every Update system so the presses
            // and releases the panel owns are gone before selection,
            // placement and the order click handlers look at them.
            .add_systems(
                PreUpdate,
                panel_mouse_input
                    .after(InputSystems)
                    .run_if(in_state(AppState::InGame)),
            )
            .add_systems(
                Update,
                (
                    collect_commands.run_if(in_state(AppState::InGame)),
                    order_hotkeys.run_if(in_state(AppState::InGame)),
                    apply_activations,
                    sync_active,
                    render_panel,
                )
                    .chain()
                    .in_set(CommandPanelSet)
                    .before(SelectionSet::Hover)
                    .after(crate::map_loading::GameWorldRebuild),
            );
    }
}

/// The panel's Update systems (command collection, hotkeys, activation,
/// drawing); the tooltip reads what they report.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct CommandPanelSet;

/// The selection's commands and their page layout (Spring's `commands`
/// and `icons`), rebuilt whenever the selection's command state changes.
#[derive(Resource, Default)]
pub(crate) struct PanelCommands {
    pub list: Vec<CmdDesc>,
    pub pages: Vec<Vec<Option<SlotCmd>>>,
    /// Identity of the selection the list was built for.
    selection: Vec<Entity>,
    /// The per-unit states and capped kinds `list` was built from;
    /// `collect_commands` skips the rebuild while they are unchanged.
    states: Vec<UnitCmdState>,
    capped: Vec<UnitKind>,
}

impl PanelCommands {
    fn slot_cmd(&self, page: usize, slot: usize) -> Option<SlotCmd> {
        self.pages.get(page)?.get(slot).copied().flatten()
    }

    /// The command description behind `slot` of `page` (page arrows get
    /// synthesised descriptions).
    fn desc_at(&self, page: usize, slot: usize) -> Option<CmdDesc> {
        match self.slot_cmd(page, slot)? {
            SlotCmd::Command(i) => self.list.get(i).cloned(),
            SlotCmd::Prev => Some(CmdDesc::page(false)),
            SlotCmd::Next => Some(CmdDesc::page(true)),
        }
    }
}

/// `activePage`.
#[derive(Resource, Default)]
pub(crate) struct PanelPage(pub usize);

/// The armed command (`inCommand`), highlighted on its button.
#[derive(Resource, Default, Debug)]
pub(crate) struct ActiveCommand(pub Option<CmdId>);

/// Press tracking (`activeMousePress` / `curIconCommand`).
#[derive(Resource, Default)]
struct PanelInput {
    /// Button + slot of a press that landed on a panel button.
    pressed: Option<(MouseButton, usize)>,
    /// A right press the panel swallowed to cancel the armed command —
    /// its release is swallowed too.
    swallowed_right: bool,
}

/// Named button pictures (`unitpics/<name>.png`).
#[derive(Resource, Default)]
struct PanelPics(Vec<(&'static str, Handle<Image>)>);

fn cursor(windows: &Query<&Window, With<PrimaryWindow>>) -> Option<(Vec2, Vec2)> {
    let w = windows.single().ok()?;
    Some((w.cursor_position()?, Vec2::new(w.width(), w.height())))
}

/// The page slot under the cursor that holds a command (`IconAtPos` with
/// `selectThrough`: empty slots let clicks through to the map).
fn command_slot(
    windows: &Query<&Window, With<PrimaryWindow>>,
    panel: &PanelCommands,
    page: usize,
) -> Option<usize> {
    let (pos, view) = cursor(windows)?;
    let slot = KP_CTRL_PANEL.slot_at(pos, view)?;
    panel.slot_cmd(page, slot).map(|_| slot)
}

#[allow(clippy::too_many_arguments)]
fn panel_mouse_input(
    windows: Query<&Window, With<PrimaryWindow>>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    panel: Res<PanelCommands>,
    page: Res<PanelPage>,
    mut input: ResMut<PanelInput>,
    mut modes: ResMut<OrderCursorModes>,
    mut placement: ResMut<PlacementMode>,
    mut active: ResMut<ActiveCommand>,
    mut out: MessageWriter<ActivateCommand>,
) {
    let armed = modes.any_active() || placement.kind.is_some();
    if armed && keys.just_pressed(KeyCode::Escape) {
        keys.clear_just_pressed(KeyCode::Escape);
        disarm(&mut modes, &mut placement, &mut active);
    }

    let slot = command_slot(&windows, &panel, page.0);
    for button in [MouseButton::Left, MouseButton::Right] {
        if mouse.just_pressed(button) {
            if let Some(slot) = slot {
                mouse.clear_just_pressed(button);
                input.pressed = Some((button, slot));
                if button == MouseButton::Right {
                    disarm(&mut modes, &mut placement, &mut active);
                }
            } else if button == MouseButton::Right && armed {
                // Right-click on the map with a command armed: cancel it
                // and issue nothing (`MouseRelease` finds no default
                // command to run).
                mouse.clear_just_pressed(button);
                input.swallowed_right = true;
                disarm(&mut modes, &mut placement, &mut active);
            }
        }
        if mouse.just_released(button) {
            if button == MouseButton::Right && input.swallowed_right {
                mouse.clear_just_released(button);
                input.swallowed_right = false;
            }
            if let Some((pressed_button, pressed_slot)) = input.pressed
                && pressed_button == button
            {
                mouse.clear_just_released(button);
                input.pressed = None;
                // Released over the button it was pressed on → run it.
                if slot == Some(pressed_slot)
                    && let Some(desc) = panel.desc_at(page.0, pressed_slot)
                {
                    out.write(ActivateCommand::click(
                        desc.id,
                        button == MouseButton::Right,
                        &keys,
                    ));
                }
            }
        }
    }
}

type SelectionQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static UnitType,
        Option<&'static TeamId>,
        Option<&'static Producer>,
        Option<&'static AutoHold>,
        Option<&'static CommandFireCooldown>,
    ),
    With<Selected>,
>;

fn collect_commands(
    selected: SelectionQuery,
    team_units: Query<(&UnitType, &TeamId), Without<Dying>>,
    mut panel: ResMut<PanelCommands>,
    mut page: ResMut<PanelPage>,
) {
    let mut rows: Vec<_> = selected.iter().collect();
    rows.sort_by_key(|r| r.0);
    let mut capped: Vec<UnitKind> = Vec::new();
    // Teams whose limits were already checked: the count is a scan over
    // every unit of the team, so do it once per team, not per selected
    // unit.
    let mut checked_teams: Vec<u8> = Vec::new();
    let states: Vec<UnitCmdState> = rows
        .iter()
        .map(|(_, ut, team, producer, autohold, cooldown)| {
            if let Some(team) = team
                && !checked_teams.contains(&team.0)
            {
                checked_teams.push(team.0);
                let kind = UnitKind::LogicBomb;
                if let Some(limit) = kind.team_limit()
                    && !capped.contains(&kind)
                    && team_kind_count(kind, team.0, &team_units) >= limit
                {
                    capped.push(kind);
                }
            }
            let mut queued: Vec<(UnitKind, u32)> = Vec::new();
            if let Some(p) = producer {
                for k in p.queue() {
                    match queued.iter_mut().find(|(q, _)| q == k) {
                        Some((_, n)) => *n += 1,
                        None => queued.push((*k, 1)),
                    }
                }
            }
            UnitCmdState {
                kind: Some(ut.0),
                repeat: producer.map(|p| p.repeat()),
                queued,
                autohold: autohold.map(|a| a.0),
                // The button only ever shows whole seconds
                // (`recharge_label` ceils), so round here: the state
                // then changes once a second instead of every frame.
                recharge: cooldown.map(|c| c.remaining.ceil()),
            }
        })
        .collect();
    let ids: Vec<Entity> = rows.iter().map(|r| r.0).collect();
    if ids != panel.selection {
        // A new selection starts on its first page.
        panel.selection = ids;
        page.0 = 0;
    }
    if states != panel.states || capped != panel.capped {
        panel.list = available_commands(&states, &capped);
        panel.pages = KP_CTRL_PANEL.layout(panel.list.len());
        panel.states = states;
        panel.capped = capped;
    }
    let clamped = page.0.min(panel.pages.len().saturating_sub(1));
    if clamped != page.0 {
        page.0 = clamped;
    }
}

#[derive(Component)]
struct CommandPanelRoot;

/// White at `a` alpha.
fn white(a: f32) -> Color {
    Color::srgba(1.0, 1.0, 1.0, a)
}

/// Everything a rebuild depends on, hashed to skip unchanged frames.
fn render_signature(
    panel: &PanelCommands,
    page: usize,
    view: Vec2,
    hovered: Option<usize>,
    pressed: Option<(MouseButton, usize)>,
    active: Option<CmdId>,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (view.x as u32, view.y as u32, page, hovered, active).hash(&mut h);
    pressed
        .map(|(b, s)| (b == MouseButton::Left, s))
        .hash(&mut h);
    if let Some(slots) = panel.pages.get(page) {
        for (slot, cmd) in slots.iter().enumerate() {
            let Some(cmd) = cmd else {
                continue;
            };
            slot.hash(&mut h);
            cmd.hash(&mut h);
            if let Some(d) = panel.desc_at(page, slot) {
                (
                    d.id,
                    d.label().to_string(),
                    d.disabled,
                    d.texture,
                    d.only_texture,
                    d.count,
                )
                    .hash(&mut h);
            }
        }
    }
    h.finish() | 1
}

#[allow(clippy::too_many_arguments)]
fn render_panel(
    mut commands: Commands,
    windows: Query<&Window, With<PrimaryWindow>>,
    mouse: Res<ButtonInput<MouseButton>>,
    panel: Res<PanelCommands>,
    page: Res<PanelPage>,
    input: Res<PanelInput>,
    active: Res<ActiveCommand>,
    previews: Res<UnitPreviews>,
    mut pics: ResMut<PanelPics>,
    asset_server: Res<AssetServer>,
    mut tip: ResMut<HoverTip>,
    roots: Query<Entity, With<CommandPanelRoot>>,
    mut last: Local<u64>,
    in_game: Option<Res<State<AppState>>>,
) {
    let playing = in_game.is_some_and(|s| *s.get() == AppState::InGame);
    let Some((cursor_pos, view)) = cursor(&windows).or_else(|| {
        windows
            .single()
            .ok()
            .map(|w| (Vec2::splat(-1.0), Vec2::new(w.width(), w.height())))
    }) else {
        return;
    };
    let hovered = if playing {
        KP_CTRL_PANEL
            .slot_at(cursor_pos, view)
            .filter(|s| panel.slot_cmd(page.0, *s).is_some())
    } else {
        None
    };
    let hover_desc = hovered.and_then(|s| panel.desc_at(page.0, s));
    if tip.panel != hover_desc {
        tip.panel = hover_desc;
    }

    let sig = if playing && !panel.list.is_empty() {
        render_signature(&panel, page.0, view, hovered, input.pressed, active.0)
    } else {
        0
    };
    if sig == *last && (sig == 0 || !roots.is_empty()) {
        return;
    }
    *last = sig;
    for e in &roots {
        commands.entity(e).despawn();
    }
    if sig == 0 {
        return;
    }

    let cfg = KP_CTRL_PANEL;
    let text_border = Vec2::new(cfg.text_border * view.x, cfg.text_border * view.y);
    let mouse_down = mouse.pressed(MouseButton::Left) || mouse.pressed(MouseButton::Right);

    let root = commands
        .spawn((
            CommandPanelRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Px(0.0),
                height: Val::Px(0.0),
                ..default()
            },
            GlobalZIndex(-1),
        ))
        .id();

    let Some(slots) = panel.pages.get(page.0) else {
        return;
    };
    for (slot, cmd) in slots.iter().enumerate() {
        if cmd.is_none() {
            continue;
        }
        let Some(desc) = panel.desc_at(page.0, slot) else {
            continue;
        };
        let rect = cfg.icon_px(slot, view);
        let size = rect.size();
        let is_active = active.0.is_some_and(|a| a == desc.id);
        let is_hovered = hovered == Some(slot);
        let highlight = is_hovered || is_active;
        let pressed_here = input.pressed.is_some_and(|(_, s)| s == slot) && mouse_down;

        let texture = desc.texture.and_then(|t| match t {
            Texture::Buildpic(kind) => previews.get(kind).cloned(),
            Texture::Pic(name) => Some(pic(&mut pics, &asset_server, name)),
        });
        let used_texture = texture.is_some();

        let mut icon = commands.spawn((
            ChildOf(root),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(rect.min.x),
                top: Val::Px(rect.min.y),
                width: Val::Px(size.x),
                height: Val::Px(size.y),
                ..default()
            },
        ));
        // Picture (textureAlpha 1, stretched over the whole button).
        if let Some(handle) = texture {
            icon.insert(ImageNode::new(handle));
        }
        let icon = icon.id();

        let overlay = |commands: &mut Commands, color: Color| {
            commands.spawn((
                ChildOf(icon),
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    top: Val::Px(0.0),
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                BackgroundColor(color),
            ));
        };
        // `DrawHilightQuad` (additive in Spring; a translucent wash here).
        if highlight {
            let wash = if is_active {
                Color::srgba(1.0, 0.0, 0.0, 0.3)
            } else if pressed_here {
                Color::srgba(1.0, 0.0, 0.0, 0.2)
            } else {
                Color::srgba(0.0, 0.0, 1.0, 0.2)
            };
            overlay(&mut commands, wash);
        }

        // Queue count, bottom-left, a fifth of the button high.
        if let Some(n) = desc.count {
            let fs = size.y * 0.2 / 0.72;
            commands.spawn((
                ChildOf(icon),
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(text_border.x + 0.002 * view.x),
                    bottom: Val::Px(text_border.y + 0.002 * view.y),
                    ..default()
                },
                Text::new(n.to_string()),
                TextFont::from_font_size(fs),
                TextColor(Color::WHITE),
            ));
        }

        let is_arrow = matches!(desc.ty, CmdType::Prev | CmdType::Next);
        let draw_name = !used_texture || !desc.only_texture;
        let leds = desc.ty == CmdType::Mode && desc.options.len() >= 2;
        let led_shrink = if leds { 0.125 * size.y } else { 0.0 };
        if is_arrow {
            let glyph = if desc.ty == CmdType::Prev { "<" } else { ">" };
            let color = if highlight {
                Color::srgb(1.0, 1.0, 0.0)
            } else {
                Color::srgb(0.7, 0.7, 0.7)
            };
            spawn_label(&mut commands, icon, glyph, size.y * 0.8, color, 0.0);
        } else if draw_name {
            if !used_texture {
                // Faint frame around text buttons.
                commands.spawn((
                    ChildOf(icon),
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(1.0),
                        top: Val::Px(1.0),
                        right: Val::Px(1.0),
                        bottom: Val::Px(1.0),
                        border: UiRect::all(Val::Px(1.0)),
                        ..default()
                    },
                    BorderColor::all(white(0.1)),
                ));
            }
            let label = desc.label().to_string();
            let fs = fit_font_size(&label, size, text_border, led_shrink);
            spawn_label(&mut commands, icon, &label, fs, Color::WHITE, led_shrink);
        }

        // Option LEDs (`DrawOptionLEDs`).
        if leds {
            let p_count = desc.options.len();
            let xs = size.x / (1 + 2 * p_count) as f32;
            let ys = size.y * 0.125;
            for x in 0..p_count {
                let color = if x != desc.state {
                    Color::srgba(0.25, 0.25, 0.25, 0.5)
                } else if p_count == 2 {
                    if desc.state == 0 {
                        Color::srgba(1.0, 0.0, 0.0, 0.75)
                    } else {
                        Color::srgba(0.0, 1.0, 0.0, 0.75)
                    }
                } else if p_count == 3 {
                    [
                        Color::srgba(1.0, 0.0, 0.0, 0.75),
                        Color::srgba(1.0, 1.0, 0.0, 0.75),
                        Color::srgba(0.0, 1.0, 0.0, 0.75),
                    ][desc.state.min(2)]
                } else {
                    Color::srgba(0.75, 0.75, 0.75, 0.75)
                };
                commands.spawn((
                    ChildOf(icon),
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(xs * (1 + 2 * x) as f32),
                        bottom: Val::Px(3.0 + text_border.y),
                        width: Val::Px(xs),
                        height: Val::Px(ys),
                        border: UiRect::all(Val::Px(1.0)),
                        ..default()
                    },
                    BackgroundColor(color),
                    BorderColor::all(white(0.5)),
                ));
            }
        }

        // Darken disabled commands.
        if desc.disabled {
            overlay(&mut commands, Color::srgba(0.0, 0.0, 0.0, 0.5));
        }

        // Highlight outline.
        if highlight {
            let color = if is_active {
                Color::srgba(1.0, 1.0, 0.0, 0.75)
            } else if mouse_down {
                Color::srgba(1.0, 0.0, 0.0, 0.5)
            } else {
                white(0.5)
            };
            commands.spawn((
                ChildOf(icon),
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    top: Val::Px(0.0),
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    border: UiRect::all(Val::Px(1.5)),
                    ..default()
                },
                BorderColor::all(color),
            ));
        }
    }
}

/// A label centred on its button (`FONT_CENTER | FONT_VCENTER`), lifted
/// by `lift` px above the LED strip.
fn spawn_label(
    commands: &mut Commands,
    icon: Entity,
    text: &str,
    size: f32,
    color: Color,
    lift: f32,
) {
    commands
        .spawn((
            ChildOf(icon),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                padding: UiRect::bottom(Val::Px(lift)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
        ))
        .with_child((
            Text::new(text),
            TextFont::from_font_size(size.max(1.0)),
            TextColor(color),
            TextLayout::new(Justify::Center, LineBreak::NoWrap),
        ));
}

fn pic(pics: &mut PanelPics, asset_server: &AssetServer, name: &'static str) -> Handle<Image> {
    if let Some((_, h)) = pics.0.iter().find(|(n, _)| *n == name) {
        return h.clone();
    }
    let h: Handle<Image> = asset_server.load(format!("unitpics/{name}.png"));
    pics.0.push((name, h.clone()));
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use bevy::window::WindowResolution;

    fn world_with_window(cursor: Option<Vec2>) -> World {
        let mut world = World::new();
        let mut window = Window {
            resolution: WindowResolution::new(1920, 1080),
            ..default()
        };
        window.set_cursor_position(cursor);
        world.spawn((window, PrimaryWindow));
        world.init_resource::<ButtonInput<MouseButton>>();
        world.init_resource::<ButtonInput<KeyCode>>();
        world.init_resource::<PanelCommands>();
        world.init_resource::<PanelPage>();
        world.init_resource::<PanelInput>();
        world.init_resource::<OrderCursorModes>();
        world.init_resource::<PlacementMode>();
        world.init_resource::<ActiveCommand>();
        world.init_resource::<Messages<ActivateCommand>>();
        world
    }

    fn offer_kernel(world: &mut World) {
        let mut panel = world.resource_mut::<PanelCommands>();
        panel.list = available_commands(
            &[UnitCmdState {
                kind: Some(UnitKind::Kernel),
                repeat: Some(false),
                ..Default::default()
            }],
            &[],
        );
        panel.pages = KP_CTRL_PANEL.layout(panel.list.len());
    }

    fn slot_centre(slot: usize) -> Vec2 {
        KP_CTRL_PANEL
            .icon_px(slot, Vec2::new(1920.0, 1080.0))
            .center()
    }

    fn sent(world: &mut World) -> Vec<ActivateCommand> {
        world
            .resource_mut::<Messages<ActivateCommand>>()
            .drain()
            .collect()
    }

    /// Press + release on the same button activates it on the release;
    /// both events are consumed so nothing behind the panel sees them.
    #[test]
    fn click_activates_on_release() {
        // Slot 2: Kernel → Stop, Repeat, Bit …
        let mut world = world_with_window(Some(slot_centre(2)));
        offer_kernel(&mut world);
        world
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);
        world.run_system_once(panel_mouse_input).unwrap();
        assert!(
            !world
                .resource::<ButtonInput<MouseButton>>()
                .just_pressed(MouseButton::Left)
        );
        assert!(sent(&mut world).is_empty(), "nothing runs on the press");
        {
            let mut mouse = world.resource_mut::<ButtonInput<MouseButton>>();
            mouse.clear();
            mouse.release(MouseButton::Left);
        }
        world.run_system_once(panel_mouse_input).unwrap();
        assert!(
            !world
                .resource::<ButtonInput<MouseButton>>()
                .just_released(MouseButton::Left)
        );
        let msgs = sent(&mut world);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].id, CmdId::Produce(UnitKind::Bit));
        assert!(!msgs[0].right);
    }

    /// Releasing over a different button runs nothing.
    #[test]
    fn release_elsewhere_cancels_the_click() {
        let mut world = world_with_window(Some(slot_centre(2)));
        offer_kernel(&mut world);
        world
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Right);
        world.run_system_once(panel_mouse_input).unwrap();
        world
            .query::<&mut Window>()
            .single_mut(&mut world)
            .unwrap()
            .set_cursor_position(Some(slot_centre(3)));
        {
            let mut mouse = world.resource_mut::<ButtonInput<MouseButton>>();
            mouse.clear();
            mouse.release(MouseButton::Right);
        }
        world.run_system_once(panel_mouse_input).unwrap();
        assert!(sent(&mut world).is_empty());
    }

    /// Right-click on the map with a command armed cancels it and the
    /// click never reaches the right-click move order.
    #[test]
    fn right_click_on_map_cancels_armed_command() {
        let mut world = world_with_window(Some(Vec2::new(1000.0, 500.0)));
        offer_kernel(&mut world);
        world.resource_mut::<PlacementMode>().kind = Some(UnitKind::Socket);
        world
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Right);
        world.run_system_once(panel_mouse_input).unwrap();
        assert_eq!(world.resource::<PlacementMode>().kind, None);
        assert!(
            !world
                .resource::<ButtonInput<MouseButton>>()
                .just_pressed(MouseButton::Right)
        );
        {
            let mut mouse = world.resource_mut::<ButtonInput<MouseButton>>();
            mouse.clear();
            mouse.release(MouseButton::Right);
        }
        world.run_system_once(panel_mouse_input).unwrap();
        assert!(
            !world
                .resource::<ButtonInput<MouseButton>>()
                .just_released(MouseButton::Right)
        );
    }

    /// Escape disarms and is consumed (so it doesn't also open the menu).
    #[test]
    fn escape_disarms() {
        let mut world = world_with_window(None);
        world.resource_mut::<OrderCursorModes>().move_order = true;
        world
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::Escape);
        world.run_system_once(panel_mouse_input).unwrap();
        assert!(!world.resource::<OrderCursorModes>().any_active());
        assert!(
            !world
                .resource::<ButtonInput<KeyCode>>()
                .just_pressed(KeyCode::Escape)
        );
    }

    /// The panel is built from the selection: selecting a unit shows its
    /// commands, the list survives clicks, and a deselect empties it.
    #[test]
    fn panel_follows_selection() {
        let mut world = World::new();
        world.init_resource::<PanelCommands>();
        world.init_resource::<PanelPage>();
        let kernel = world
            .spawn((
                UnitType(UnitKind::Kernel),
                Producer::new(),
                TeamId(0),
                Selected,
            ))
            .id();
        world.run_system_once(collect_commands).unwrap();
        assert_eq!(world.resource::<PanelCommands>().list.len(), 6);
        world.entity_mut(kernel).remove::<Selected>();
        world.run_system_once(collect_commands).unwrap();
        assert!(world.resource::<PanelCommands>().list.is_empty());
    }

    /// A recharging Terminal's state is kept in whole seconds, so the
    /// command list is only rebuilt when the shown countdown changes.
    #[test]
    fn recharge_state_changes_once_a_second() {
        let mut world = World::new();
        world.init_resource::<PanelCommands>();
        world.init_resource::<PanelPage>();
        let terminal = world
            .spawn((
                UnitType(UnitKind::Terminal),
                TeamId(0),
                CommandFireCooldown { remaining: 41.2 },
                Selected,
            ))
            .id();
        world.run_system_once(collect_commands).unwrap();
        let first = world.resource::<PanelCommands>().states.clone();
        assert_eq!(first[0].recharge, Some(42.0));
        assert_eq!(world.resource::<PanelCommands>().list[0].label(), "42s");

        world
            .entity_mut(terminal)
            .insert(CommandFireCooldown { remaining: 41.7 });
        world.run_system_once(collect_commands).unwrap();
        assert_eq!(world.resource::<PanelCommands>().states, first);

        world
            .entity_mut(terminal)
            .insert(CommandFireCooldown { remaining: 40.9 });
        world.run_system_once(collect_commands).unwrap();
        assert_eq!(world.resource::<PanelCommands>().list[0].label(), "41s");
    }
}
