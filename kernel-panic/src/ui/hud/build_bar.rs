//! Kernel Panic's build bar (`LuaUI/Widgets/kp_buildbar.lua`, on by
//! default): a row of icons along the top edge, right-aligned, that keeps
//! the homebase and the special buildings in view whatever is selected.
//!
//! - One icon per finished homebase (factories with more than one build
//!   option — minifacs are left out) and per Terminal / Firewall, the
//!   specials first. Icons are `55 + (width − 800) / 38` px wide and
//!   three quarters of that tall, with a green border.
//! - A homebase shows the unit it is building with a progress pie, else
//!   its own buildpic; below it the next queued units (up to three, the
//!   count in the corner). A Terminal / Firewall shows its recharge pie
//!   and the seconds left, or "Ready!".
//! - Hovering a homebase opens its build options below it (they stay
//!   open until a click elsewhere or another homebase is hovered).
//!   Clicking an option edits *that* factory's queue with the control
//!   panel's rules (left +1, right −1, Shift ×5, Ctrl ×20, Alt front).
//! - Clicking an icon selects that unit (replacing selected units of the
//!   same type); for a Terminal / Firewall it also arms SIGTERM /
//!   Firewall.
//!
//! Left out: the icon-size mouse wheel, middle-click camera jump and the
//! right-click waypoint modes (the remake's factories have no rally
//! orders).

use bevy::input::InputSystems;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::game_setup::AppState;
use crate::interaction::selection::Selected;
use crate::units::combat::Dying;
use crate::units::components::{TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::lifecycle::production::{Producer, factory_roster};
use crate::units::lifecycle::spawning::Emerging;
use crate::units::mechanics::command_fire::{
    CommandFireCooldown, FIREWALL_COOLDOWN, SIGTERM_COOLDOWN,
};
use crate::units::player::LocalTeam;

use super::command_panel::activation::{ActivateCommand, edit_queue};
use super::command_panel::commands::{CmdDesc, CmdId, UnitCmdState, unit_commands};
use super::previews::UnitPreviews;
use super::tooltip::{BarTip, HoverTip};
use super::widgets::{cover, hud_root, needs_rebuild, view_and_cursor};

pub(super) struct BuildBarPlugin;

impl Plugin for BuildBarPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BarState>()
            .add_systems(
                PreUpdate,
                bar_mouse_input
                    .after(InputSystems)
                    .run_if(in_state(AppState::InGame)),
            )
            .add_systems(
                Update,
                (
                    collect_bar.run_if(in_state(AppState::InGame)),
                    render_bar.run_if(in_state(AppState::InGame)),
                )
                    .chain()
                    .before(super::command_panel::CommandPanelSet)
                    .after(crate::map_loading::GameWorldRebuild),
            );
    }
}

/// One bar icon.
#[derive(Clone, Debug, PartialEq)]
struct BarEntry {
    entity: Entity,
    kind: UnitKind,
    /// Homebase: the unit in production and how far along it is.
    building: Option<(UnitKind, f32)>,
    /// Homebase: the queue as consecutive runs `(kind, count)`.
    runs: Vec<(UnitKind, u32)>,
    /// Terminal / Firewall: seconds left and the full reload.
    recharge: Option<(f32, f32)>,
}

impl BarEntry {
    fn is_special(&self) -> bool {
        matches!(self.kind, UnitKind::Terminal | UnitKind::Firewall)
    }

    fn options(&self) -> &'static [UnitKind] {
        if self.is_special() {
            &[]
        } else {
            factory_roster(self.kind)
        }
    }
}

#[derive(Resource, Default)]
struct BarState {
    entries: Vec<BarEntry>,
    /// Entity whose build options are open (`openedMenu`).
    opened: Option<Entity>,
    /// The open menu came from hovering (`hoveredMenu`).
    hovered_menu: bool,
    /// Button and target of a press that landed on the bar.
    pressed: Option<(MouseButton, Hit)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Hit {
    Icon(usize),
    Option(usize),
}

/// `ViewResize`: icon size from the view width.
fn icon_size(view: Vec2) -> Vec2 {
    let x = (55.0 + (view.x - 800.0) / 38.0).floor().max(20.0);
    Vec2::new(x, (x * 0.75).floor())
}

/// Top-left of icon `i` of `n` (top bar, right-aligned: `bar_offset 1`,
/// `bar_align −1`).
fn icon_pos(i: usize, n: usize, view: Vec2) -> Vec2 {
    let s = icon_size(view);
    let left = (view.x - s.x * n as f32).max(0.0);
    Vec2::new(left + s.x * i as f32, 0.0)
}

fn hit_test(cursor: Vec2, view: Vec2, state: &BarState) -> Option<Hit> {
    let s = icon_size(view);
    let n = state.entries.len();
    for i in 0..n {
        let p = icon_pos(i, n, view);
        if Rect::from_corners(p, p + s).contains(cursor) {
            return Some(Hit::Icon(i));
        }
    }
    let i = state
        .entries
        .iter()
        .position(|e| Some(e.entity) == state.opened)?;
    let p = icon_pos(i, n, view);
    let opts = state.entries[i].options().len();
    for j in 0..opts {
        let q = p + Vec2::new(0.0, s.y * (j + 1) as f32);
        if Rect::from_corners(q, q + s).contains(cursor) {
            return Some(Hit::Option(j));
        }
    }
    None
}

type BarUnits<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static UnitType,
        &'static TeamId,
        Option<&'static Producer>,
        Option<&'static CommandFireCooldown>,
        Has<Emerging>,
    ),
    Without<Dying>,
>;

fn collect_bar(
    units: BarUnits,
    local: Option<Res<LocalTeam>>,
    registry: Res<UnitRegistry>,
    mut state: ResMut<BarState>,
    mut factories: Local<Vec<BarEntry>>,
) {
    let Some(team) = local.map(|l| l.0) else {
        if !state.entries.is_empty() {
            state.entries.clear();
            state.opened = None;
        }
        return;
    };
    // Specials are collected straight into the (emptied) entries Vec
    // and the homebases into a scratch Vec; both keep their capacity
    // across frames.
    let mut specials = std::mem::take(&mut state.entries);
    specials.clear();
    factories.clear();
    for (entity, ut, t, producer, cooldown, emerging) in &units {
        if t.0 != team || emerging {
            continue;
        }
        let kind = ut.0;
        if matches!(kind, UnitKind::Terminal | UnitKind::Firewall) {
            let total = if kind == UnitKind::Terminal {
                SIGTERM_COOLDOWN
            } else {
                FIREWALL_COOLDOWN
            };
            specials.push(BarEntry {
                entity,
                kind,
                building: None,
                runs: Vec::new(),
                recharge: Some((cooldown.map_or(0.0, |c| c.remaining.max(0.0)), total)),
            });
        } else if factory_roster(kind).len() > 1
            && let Some(p) = producer
        {
            let mut runs: Vec<(UnitKind, u32)> = Vec::new();
            for k in p.queue() {
                match runs.last_mut() {
                    Some((last, n)) if last == k => *n += 1,
                    _ => runs.push((*k, 1)),
                }
            }
            factories.push(BarEntry {
                entity,
                kind,
                building: p
                    .current_production()
                    .map(|k| (k, p.progress_fraction(&registry, kind).unwrap_or(0.0))),
                runs,
                recharge: None,
            });
        }
    }
    // Newest specials first (`table.insert(facs, 1, …)`), then homebases.
    specials.sort_by_key(|e| std::cmp::Reverse(e.entity));
    factories.sort_by_key(|e| e.entity);
    specials.append(&mut factories);
    state.entries = specials;
    if state
        .opened
        .is_some_and(|o| !state.entries.iter().any(|e| e.entity == o))
    {
        state.opened = None;
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn bar_mouse_input(
    windows: Query<&Window, With<PrimaryWindow>>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut state: ResMut<BarState>,
    mut producers: Query<&mut Producer>,
    selected: Query<(Entity, &UnitType), With<Selected>>,
    mut out: MessageWriter<ActivateCommand>,
    mut commands: Commands,
) {
    let Some((view, cursor)) = view_and_cursor(&windows) else {
        return;
    };
    let hit = cursor.and_then(|c| hit_test(c, view, &state));

    // `IsAbove_`: hovering a homebase icon opens its options.
    if let Some(Hit::Icon(i)) = hit
        && (state.opened.is_none() || state.hovered_menu)
    {
        let e = &state.entries[i];
        state.opened = (!e.is_special()).then_some(e.entity);
        state.hovered_menu = true;
    }

    for button in [MouseButton::Left, MouseButton::Right] {
        if mouse.just_pressed(button) {
            match hit {
                Some(h) => {
                    mouse.clear_just_pressed(button);
                    state.pressed = Some((button, h));
                    if let Hit::Icon(i) = h {
                        let e = state.entries[i].clone();
                        // Select the unit, dropping selected units of its type.
                        for (sel, ut) in &selected {
                            if ut.0 == e.kind {
                                commands.entity(sel).remove::<Selected>();
                            }
                        }
                        // `try_insert`: the entries were refreshed last
                        // frame and can name a unit the map swap just
                        // despawned.
                        commands.entity(e.entity).try_insert(Selected);
                        let arm = match e.kind {
                            UnitKind::Terminal => Some(CmdId::Sigterm),
                            UnitKind::Firewall => Some(CmdId::Firewall),
                            _ => None,
                        };
                        if let Some(id) = arm {
                            out.write(ActivateCommand::click(id, false, &keys));
                        }
                    }
                }
                None => {
                    state.opened = None;
                    state.hovered_menu = false;
                }
            }
        }
        if mouse.just_released(button)
            && let Some((b, h)) = state.pressed
            && b == button
        {
            mouse.clear_just_released(button);
            state.pressed = None;
            if hit == Some(h)
                && let Hit::Option(j) = h
                && let Some(e) = state
                    .entries
                    .iter()
                    .find(|e| Some(e.entity) == state.opened)
                && let Some(&kind) = e.options().get(j)
                && let Ok(mut producer) = producers.get_mut(e.entity)
            {
                let msg = ActivateCommand::click(
                    CmdId::Produce(kind),
                    button == MouseButton::Right,
                    &keys,
                );
                edit_queue(&mut producer, kind, &msg);
            }
        }
    }
}

#[derive(Component)]
struct BarRoot;

/// A progress pie on an icon, kept current in place between rebuilds.
#[derive(Component)]
struct BarPie {
    unit: Entity,
    key: u32,
}

/// A Terminal / Firewall recharge countdown, kept current in place.
#[derive(Component)]
struct BarCountdown {
    unit: Entity,
    secs: u32,
}

const GREEN: Color = Color::srgb(0.0, 0.8, 0.0);
const DARK_GREEN: Color = Color::srgb(0.0, 0.5, 0.0);

/// A clockwise progress pie from 12 o'clock (`DrawBuildProgress`).
fn pie(progress: f32) -> BackgroundGradient {
    let a = progress.clamp(0.0, 1.0) * std::f32::consts::TAU;
    let fill = Color::srgba(1.0, 1.0, 1.0, 0.5);
    BackgroundGradient::from(ConicGradient::new(
        UiPosition::CENTER,
        vec![
            AngularColorStop::new(fill, 0.0),
            AngularColorStop::new(fill, a),
            AngularColorStop::new(Color::NONE, a),
            AngularColorStop::new(Color::NONE, std::f32::consts::TAU),
        ],
    ))
}

impl BarEntry {
    /// The entry's pie: its progress and a quantised key that changes
    /// when the pie visibly does. `None` when no pie is drawn.
    fn pie(&self) -> Option<(f32, u32)> {
        if let Some((_, p)) = self.building {
            return Some((p, (p * 360.0) as u32));
        }
        let (left, total) = self.recharge?;
        (left > 0.0).then(|| (1.0 - left / total.max(1e-3), (left * 4.0) as u32))
    }

    /// Whole seconds left on a recharging Terminal / Firewall.
    fn countdown(&self) -> Option<u32> {
        let (left, _) = self.recharge?;
        (left > 0.0).then(|| left.ceil() as u32)
    }
}

/// Everything the bar's node tree depends on apart from the pies and
/// countdowns, which [`render_bar`] updates in place.
fn structure_signature(state: &BarState, view: Vec2, hit: Option<Hit>, mouse_down: bool) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (view.x as u32, view.y as u32, hit, mouse_down, state.opened).hash(&mut h);
    for e in &state.entries {
        (e.entity, e.kind, &e.runs).hash(&mut h);
        e.building.map(|(k, _)| k).hash(&mut h);
        e.countdown().is_some().hash(&mut h);
    }
    h.finish() | 1
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn render_bar(
    mut commands: Commands,
    windows: Query<&Window, With<PrimaryWindow>>,
    mouse: Res<ButtonInput<MouseButton>>,
    state: Res<BarState>,
    previews: Res<UnitPreviews>,
    units: Query<&UnitType>,
    mut tip: ResMut<HoverTip>,
    roots: Query<Entity, With<BarRoot>>,
    mut pies: Query<(&mut BarPie, &mut BackgroundGradient)>,
    mut countdowns: Query<(&mut BarCountdown, &mut Text)>,
    mut last: Local<u64>,
    mut last_tip: Local<Option<TipKey>>,
) {
    let Some((view, cursor)) = view_and_cursor(&windows) else {
        return;
    };
    let hit = cursor.and_then(|c| hit_test(c, view, &state));
    let mouse_down =
        mouse.any_pressed([MouseButton::Left, MouseButton::Right, MouseButton::Middle]);

    // Tooltip: the text is a fresh `String` / a whole command list, so
    // only rebuild it when what is under the cursor changes.
    let opened = state
        .entries
        .iter()
        .find(|e| Some(e.entity) == state.opened);
    let tip_key = match hit {
        Some(Hit::Icon(i)) => state
            .entries
            .get(i)
            .filter(|e| units.contains(e.entity))
            .map(|e| (hit, e.entity, e.kind, 0)),
        Some(Hit::Option(j)) => opened.and_then(|e| {
            let kind = *e.options().get(j)?;
            Some((hit, e.entity, e.kind, queued_of(e, kind)))
        }),
        None => None,
    };
    if *last_tip != tip_key {
        *last_tip = tip_key;
        tip.bar = match hit {
            Some(Hit::Icon(i)) => tip_key.map(|_| BarTip::Text(bar_name(state.entries[i].kind))),
            Some(Hit::Option(j)) => opened.and_then(|e| option_desc(e, j)).map(BarTip::Command),
            None => None,
        };
    }

    let sig = if state.entries.is_empty() {
        0
    } else {
        structure_signature(&state, view, hit, mouse_down)
    };
    if !needs_rebuild(&mut commands, sig, &mut last, &roots) {
        // Same layout: advance the pies and countdowns in place.
        let entry = |unit: Entity| state.entries.iter().find(|e| e.entity == unit);
        for (mut p, mut gradient) in &mut pies {
            if let Some((progress, key)) = entry(p.unit).and_then(BarEntry::pie)
                && key != p.key
            {
                p.key = key;
                *gradient = pie(progress);
            }
        }
        for (mut c, mut text) in &mut countdowns {
            if let Some(secs) = entry(c.unit).and_then(BarEntry::countdown)
                && secs != c.secs
            {
                c.secs = secs;
                text.0 = format!("{secs}s");
            }
        }
        return;
    }
    if sig == 0 {
        return;
    }

    let s = icon_size(view);
    let font = s.y * 0.25;
    let n = state.entries.len();
    let root = hud_root(&mut commands, BarRoot);

    let cell = |commands: &mut Commands, pos: Vec2, pic: Option<Handle<Image>>, alpha: f32| {
        let mut c = commands.spawn((
            ChildOf(root),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(pos.x),
                top: Val::Px(pos.y),
                width: Val::Px(s.x),
                height: Val::Px(s.y),
                ..default()
            },
        ));
        if let Some(pic) = pic {
            c.insert(ImageNode::new(pic).with_color(Color::srgba(1.0, 1.0, 1.0, alpha)));
        }
        c.id()
    };
    let fill = |commands: &mut Commands, parent: Entity, color: Color| {
        commands.spawn((ChildOf(parent), cover(), BackgroundColor(color)));
    };
    let frame = |commands: &mut Commands, parent: Entity, color: Color, width: f32| {
        commands.spawn((
            ChildOf(parent),
            Node {
                border: UiRect::all(Val::Px(width)),
                ..cover()
            },
            BorderColor::all(color),
        ));
    };
    let text = |commands: &mut Commands,
                parent: Entity,
                t: String,
                size: f32,
                left: f32,
                bottom: f32|
     -> Entity {
        commands
            .spawn((
                ChildOf(parent),
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(left),
                    bottom: Val::Px(bottom),
                    ..default()
                },
                Text::new(t),
                TextFont::from_font_size(size),
                TextColor(Color::WHITE),
                TextShadow {
                    offset: Vec2::splat(1.0),
                    color: Color::BLACK,
                },
            ))
            .id()
    };
    let hover_wash = |commands: &mut Commands, parent: Entity, hovered: bool| {
        if hovered {
            let c = if mouse_down {
                Color::srgba(0.0, 0.0, 0.0, 0.35)
            } else {
                Color::srgba(1.0, 1.0, 1.0, 0.35)
            };
            fill(commands, parent, c);
        }
    };
    let with_pie = |commands: &mut Commands, node: Entity, e: &BarEntry| {
        if let Some((progress, key)) = e.pie() {
            commands.entity(node).insert((
                pie(progress),
                BarPie {
                    unit: e.entity,
                    key,
                },
            ));
        }
    };

    for (i, e) in state.entries.iter().enumerate() {
        let pos = icon_pos(i, n, view);
        let opened = state.opened == Some(e.entity);
        let shown = e.building.map_or(e.kind, |(k, _)| k);
        let c = cell(&mut commands, pos, previews.get(shown).cloned(), 1.0);
        with_pie(&mut commands, c, e);
        if e.recharge.is_some() {
            match e.countdown() {
                Some(secs) => {
                    let t = text(
                        &mut commands,
                        c,
                        format!("{secs}s"),
                        s.y / 3.0,
                        s.x / 4.0,
                        s.y / 6.0,
                    );
                    commands.entity(t).insert(BarCountdown {
                        unit: e.entity,
                        secs,
                    });
                }
                None => {
                    text(
                        &mut commands,
                        c,
                        "Ready!".into(),
                        s.y / 4.0,
                        s.x / 6.0,
                        s.y / 4.0,
                    );
                }
            }
        }
        hover_wash(&mut commands, c, hit == Some(Hit::Icon(i)));
        if opened {
            fill(&mut commands, c, Color::srgba(1.0, 1.0, 1.0, 0.35));
            frame(&mut commands, c, GREEN, 3.5);
        } else {
            frame(&mut commands, c, GREEN, 1.5);
        }

        if opened {
            // Build options, one per row below the homebase.
            for (j, &kind) in e.options().iter().enumerate() {
                let q = pos + Vec2::new(0.0, s.y * (j + 1) as f32);
                let o = cell(&mut commands, q, previews.get(kind).cloned(), 0.75);
                if e.building.is_some_and(|(k, _)| k == kind) {
                    with_pie(&mut commands, o, e);
                }
                let queued = queued_of(e, kind);
                if queued > 0 {
                    text(&mut commands, o, queued.to_string(), font, 2.0, 2.0);
                }
                hover_wash(&mut commands, o, hit == Some(Hit::Option(j)));
                frame(&mut commands, o, DARK_GREEN, 1.5);
            }
        } else {
            // Up to three queued runs; the unit in production is on the
            // homebase icon itself.
            let mut row = 0;
            for (r, &(kind, count)) in e.runs.iter().enumerate() {
                let count = if r == 0 { count - 1 } else { count };
                if count == 0 {
                    continue;
                }
                let q = pos + Vec2::new(0.0, s.y * (row + 1) as f32);
                let o = cell(&mut commands, q, previews.get(kind).cloned(), 0.55);
                if count > 1 {
                    text(&mut commands, o, count.to_string(), font, 2.0, 2.0);
                }
                row += 1;
                if row == 3 {
                    break;
                }
            }
        }
    }
}

/// What the bar tooltip depends on: the hit, the unit behind it, its
/// kind and (for a build option) that option's queued count.
type TipKey = (Option<Hit>, Entity, UnitKind, u32);

/// kp_buildbar's icon tooltip: the unit's name.
fn bar_name(kind: UnitKind) -> String {
    kind.unitname().to_string()
}

/// How many of `kind` the homebase `e` has queued.
fn queued_of(e: &BarEntry, kind: UnitKind) -> u32 {
    e.runs
        .iter()
        .filter(|(k, _)| *k == kind)
        .map(|(_, n)| n)
        .sum()
}

/// The control-panel description of build option `j` of `e` (for the
/// tooltip): the factory's own `Produce` command with its queue count.
fn option_desc(e: &BarEntry, j: usize) -> Option<CmdDesc> {
    let kind = *e.options().get(j)?;
    let mut queued: Vec<(UnitKind, u32)> = Vec::new();
    for &(k, n) in &e.runs {
        match queued.iter_mut().find(|(q, _)| *q == k) {
            Some((_, m)) => *m += n,
            None => queued.push((k, n)),
        }
    }
    unit_commands(
        &UnitCmdState {
            kind: Some(e.kind),
            repeat: Some(false),
            queued,
            ..Default::default()
        },
        &[],
    )
    .into_iter()
    .find(|d| d.id == CmdId::Produce(kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `iconSizeX = floor(55 + (vsx − 800) / 38)`, `Y = floor(0.75 X)`;
    /// the bar hugs the top-right corner.
    #[test]
    fn geometry_matches_widget() {
        let view = Vec2::new(1920.0, 1080.0);
        assert_eq!(icon_size(view), Vec2::new(84.0, 63.0));
        assert_eq!(icon_pos(1, 2, view), Vec2::new(1920.0 - 84.0, 0.0));
        assert_eq!(icon_pos(0, 2, view), Vec2::new(1920.0 - 168.0, 0.0));
    }

    /// Hit testing covers the icons and, below the opened homebase, its
    /// build options.
    #[test]
    fn hit_test_icons_and_options() {
        let view = Vec2::new(1920.0, 1080.0);
        let kernel = BarEntry {
            entity: Entity::from_raw_u32(7).unwrap(),
            kind: UnitKind::Kernel,
            building: None,
            runs: Vec::new(),
            recharge: None,
        };
        let mut state = BarState {
            entries: vec![kernel.clone()],
            ..default()
        };
        assert_eq!(
            hit_test(Vec2::new(1900.0, 10.0), view, &state),
            Some(Hit::Icon(0))
        );
        assert_eq!(hit_test(Vec2::new(1900.0, 100.0), view, &state), None);
        state.opened = Some(kernel.entity);
        assert_eq!(
            hit_test(Vec2::new(1900.0, 100.0), view, &state),
            Some(Hit::Option(0))
        );
        assert_eq!(
            hit_test(Vec2::new(1900.0, 63.0 * 4.5), view, &state),
            Some(Hit::Option(3))
        );
        assert_eq!(hit_test(Vec2::new(1900.0, 63.0 * 5.5), view, &state), None);
    }
}
