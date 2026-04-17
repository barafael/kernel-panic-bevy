//! Context-aware mouse cursor.
//!
//! Mirrors Spring/Kernel-Panic conventions: each command kind has its own
//! cursor (normal, attack, move, repair, ...). Frames are loaded from
//! `assets/cursors/<name>_NN.png` and cycled by `frame_advance` so animated
//! sprites work despite Bevy 0.18 having no native animated cursor.
//!
//! Other systems request a cursor by writing `CursorRequest`; the resolver
//! picks the highest-priority entry and applies it to the primary window.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy::window::{CursorIcon, CustomCursor, CustomCursorImage, PrimaryWindow};

use crate::interaction::selection::{Hovered, Selected};
use crate::units::components::{TeamId, UnitType};
use crate::units::game_over::PlayerTeam;
use crate::units::unit_registry::UnitRegistry;

pub struct CursorPlugin;

impl Plugin for CursorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CursorRequest>()
            .init_resource::<CursorState>()
            .add_systems(Startup, load_cursor_frames)
            .add_systems(
                Update,
                (resolve_context_cursor, frame_advance, apply_cursor)
                    .chain()
                    .in_set(CursorSet::Apply),
            )
            .configure_sets(Update, CursorSet::Apply);
    }
}

#[derive(SystemSet, Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum CursorSet {
    /// Resolve `CursorRequest` and update the window cursor icon.
    Apply,
}

/// Cursor variants we ship. Each maps to a folder of frame PNGs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CursorKind {
    Normal,
    Attack,
    Move,
    Patrol,
    Defend,
    Repair,
    Reclamate,
    Revive,
    Capture,
    Pickup,
    Unload,
}

impl CursorKind {
    /// File-name stem under `assets/cursors/` (matches Spring's naming).
    fn stem(self) -> &'static str {
        match self {
            CursorKind::Normal => "cursornormal",
            CursorKind::Attack => "cursorattack",
            CursorKind::Move => "cursormove",
            CursorKind::Patrol => "cursorpatrol",
            CursorKind::Defend => "cursordefend",
            CursorKind::Repair => "cursorrepair",
            CursorKind::Reclamate => "cursorreclamate",
            CursorKind::Revive => "cursorrevive",
            CursorKind::Capture => "cursorcapture",
            CursorKind::Pickup => "cursorpickup",
            CursorKind::Unload => "cursorunload",
        }
    }

    fn frame_count(self) -> usize {
        match self {
            CursorKind::Normal => 1,
            CursorKind::Attack => 9,
            CursorKind::Move => 10,
            CursorKind::Patrol => 8,
            CursorKind::Defend => 8,
            CursorKind::Repair => 9,
            CursorKind::Reclamate => 20,
            CursorKind::Revive => 14,
            CursorKind::Capture => 14,
            CursorKind::Pickup => 9,
            CursorKind::Unload => 9,
        }
    }

    /// Pixel offset from the image's top-left that represents the click point.
    /// `cursornormal` is a 26×26 arrow with its tip near (0, 0); the action
    /// cursors are 39×39 crosshair-style icons centred on their target.
    fn hotspot(self) -> (u16, u16) {
        match self {
            CursorKind::Normal => (0, 0),
            _ => (19, 19),
        }
    }

    fn all() -> &'static [CursorKind] {
        &[
            CursorKind::Normal,
            CursorKind::Attack,
            CursorKind::Move,
            CursorKind::Patrol,
            CursorKind::Defend,
            CursorKind::Repair,
            CursorKind::Reclamate,
            CursorKind::Revive,
            CursorKind::Capture,
            CursorKind::Pickup,
            CursorKind::Unload,
        ]
    }
}

/// Per-cursor frame storage. Built once at startup.
#[derive(Resource, Default)]
struct CursorState {
    frames: HashMap<CursorKind, Vec<Handle<Image>>>,
    /// Currently-displayed (kind, frame_index) so we can avoid reapplying.
    current: Option<(CursorKind, usize)>,
    /// Time accumulator for frame cycling.
    timer: Timer,
}

/// One slot in [`CursorRequest`].
#[derive(Debug, Clone, Copy)]
pub struct PendingCursor {
    pub kind: CursorKind,
    /// Higher wins. Tie-break: most recently set.
    pub priority: u8,
}

/// Cursor request inbox. Other systems write here each frame; the resolver
/// reads and clears it. Last-writer-wins for equal priority.
#[derive(Resource, Default)]
pub struct CursorRequest(Option<PendingCursor>);

impl CursorRequest {
    /// Set the cursor for this frame if the new priority is at least as high
    /// as the current one.
    pub fn set(&mut self, kind: CursorKind, priority: u8) {
        let beats = self.0.is_none_or(|c| priority >= c.priority);
        if beats {
            self.0 = Some(PendingCursor { kind, priority });
        }
    }
}

/// Default-priority context resolver. Other systems (build placement, order
/// modes) can override later by writing to `CursorRequest` with a higher
/// priority.
fn resolve_context_cursor(
    mut request: ResMut<CursorRequest>,
    selected: Query<(), With<Selected>>,
    selected_with_speed: Query<&UnitType, With<Selected>>,
    hovered: Query<&TeamId, (With<Hovered>, With<UnitType>)>,
    player: Res<PlayerTeam>,
    unit_registry: Res<UnitRegistry>,
) {
    if selected.is_empty() {
        request.set(CursorKind::Normal, 0);
        return;
    }

    let selection_can_act = selected_with_speed
        .iter()
        .any(|ut| unit_registry.speed(ut.0) > 0.0);
    if !selection_can_act {
        request.set(CursorKind::Normal, 0);
        return;
    }

    match hovered.iter().next() {
        Some(team) if team.0 != player.0 => request.set(CursorKind::Attack, 0),
        Some(_) => request.set(CursorKind::Normal, 0),
        None => request.set(CursorKind::Move, 0),
    }
}

fn load_cursor_frames(asset_server: Res<AssetServer>, mut state: ResMut<CursorState>) {
    state.timer = Timer::from_seconds(FRAME_PERIOD_SECS, TimerMode::Repeating);
    for &kind in CursorKind::all() {
        let frames: Vec<Handle<Image>> = (0..kind.frame_count())
            .map(|i| asset_server.load(format!("cursors/{}_{:02}.png", kind.stem(), i)))
            .collect();
        state.frames.insert(kind, frames);
    }
}

/// ~5 fps animation — slower than Spring's default for a calmer feel.
const FRAME_PERIOD_SECS: f32 = 1.0 / 5.0;

fn frame_advance(time: Res<Time>, mut state: ResMut<CursorState>) {
    state.timer.tick(time.delta());
}

fn apply_cursor(
    mut request: ResMut<CursorRequest>,
    mut state: ResMut<CursorState>,
    primary: Single<Entity, With<PrimaryWindow>>,
    mut commands: Commands,
) {
    let kind = request
        .0
        .take()
        .map(|c| c.kind)
        .unwrap_or(CursorKind::Normal);
    let frames = state.frames.get(&kind).cloned();
    let Some(frames) = frames else { return };
    if frames.is_empty() {
        return;
    }

    let prev_frame = state.current.map(|(_, f)| f).unwrap_or(0);
    let frame = if state.timer.just_finished() {
        (prev_frame + 1) % frames.len()
    } else if state.current.is_none_or(|(k, _)| k != kind) {
        0
    } else {
        prev_frame.min(frames.len() - 1)
    };

    if state.current == Some((kind, frame)) {
        return;
    }
    state.current = Some((kind, frame));

    let (hx, hy) = kind.hotspot();
    commands
        .entity(*primary)
        .insert(CursorIcon::Custom(CustomCursor::Image(CustomCursorImage {
            handle: frames[frame].clone(),
            hotspot: (hx, hy),
            flip_x: false,
            flip_y: false,
            rect: None,
            texture_atlas: None,
        })));
}
