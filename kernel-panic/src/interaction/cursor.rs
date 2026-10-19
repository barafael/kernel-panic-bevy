//! Context-aware mouse cursor.
//!
//! Mirrors Spring/Kernel-Panic conventions: each command kind has its own
//! cursor (normal, attack, move, repair, ...). Frames are loaded from
//! `assets/cursors/<name>_NN.png` and cycled by `frame_advance` so animated
//! sprites work despite Bevy having no native animated cursor.
//!
//! Other systems request a cursor by writing `CursorRequest`; the resolver
//! picks the highest-priority entry and applies it to the primary window.
//!
//! The action cursors are drawn at [`CursorKind::scale`]× their size
//! (the arrow at its own): once a frame has
//! loaded, `scale_cursor_frames` makes a nearest-neighbour enlarged copy,
//! and that copy is what the window shows.

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension};
use bevy::window::{CursorIcon, CustomCursor, CustomCursorImage, PrimaryWindow};

use crate::interaction::selection::{Hovered, Selected};
use crate::sim::INV_GAME_SPEED;
use crate::units::components::{TeamId, UnitType};
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::teams::Alliances;

pub struct CursorPlugin;

impl Plugin for CursorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CursorRequest>()
            .init_resource::<CursorState>()
            .add_systems(Startup, load_cursor_frames)
            .add_systems(
                Update,
                (
                    resolve_context_cursor,
                    frame_advance,
                    scale_cursor_frames,
                    apply_cursor,
                )
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

    /// Pixel offset from the image's top-left that represents the click
    /// point, in the shipped image (before [`Self::scale`]).
    /// `cursornormal` is a 26×26 arrow with its tip near (0, 0); the action
    /// cursors are 39×39 crosshair-style icons centred on their target.
    fn hotspot(self) -> (u16, u16) {
        match self {
            CursorKind::Normal => (0, 0),
            _ => (19, 19),
        }
    }

    /// How much larger than the shipped PNG the cursor is drawn: the
    /// 39 px action cursors were made for far lower resolutions; the
    /// 26 px arrow reads well at its own size.
    fn scale(self) -> u16 {
        match self {
            CursorKind::Normal => 1,
            _ => 2,
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
    /// The [`CursorKind::scale`]× copy of each frame, once it has loaded.
    scaled: HashMap<CursorKind, Vec<Option<Handle<Image>>>>,
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
#[allow(clippy::type_complexity)]
fn resolve_context_cursor(
    mut request: ResMut<CursorRequest>,
    selected: Query<(&UnitType, &TeamId), With<Selected>>,
    hovered: Query<&TeamId, (With<Hovered>, With<UnitType>)>,
    unit_registry: Res<UnitRegistry>,
    alliances: Res<Alliances>,
) {
    if selected.is_empty() {
        request.set(CursorKind::Normal, 0);
        return;
    }

    let mut has_mover = false;
    let mut has_weapon = false;
    let mut has_constructor = false;
    // Track the first selected unit's owner so we can ask `friendly`
    // about the hovered unit — the cursor hint is resolved by comparing
    // the selection against what's under the cursor.
    let mut selection_team: Option<u8> = None;
    for (ut, team) in &selected {
        // A factory takes moves for the units it builds.
        if unit_registry.speed(ut.0) > 0.0 || ut.0.takes_rally_orders() {
            has_mover = true;
        }
        if !unit_registry.weapon(ut.0).is_empty() {
            has_weapon = true;
        }
        if ut.0.is_constructor() {
            has_constructor = true;
        }
        selection_team.get_or_insert(team.0);
    }

    match hovered.iter().next() {
        Some(hover_team) => {
            let is_enemy = selection_team.is_some_and(|t| !alliances.friendly(t, hover_team.0));
            if is_enemy {
                if has_weapon {
                    request.set(CursorKind::Attack, 0);
                } else {
                    request.set(CursorKind::Normal, 0);
                }
            } else if has_constructor {
                request.set(CursorKind::Repair, 0);
            } else {
                request.set(CursorKind::Normal, 0);
            }
        }
        None => {
            if has_mover {
                request.set(CursorKind::Move, 0);
            } else {
                request.set(CursorKind::Normal, 0);
            }
        }
    }
}

fn load_cursor_frames(asset_server: Res<AssetServer>, mut state: ResMut<CursorState>) {
    // One cursor frame per sim frame, ~30 fps (FEATURES.md §25). Matches
    // Spring's default; at 5 fps the Move/Attack sprites visibly stepped
    // through frames.
    state.timer = Timer::from_seconds(INV_GAME_SPEED, TimerMode::Repeating);
    for &kind in CursorKind::all() {
        let frames: Vec<Handle<Image>> = (0..kind.frame_count())
            .map(|i| asset_server.load(format!("cursors/{}_{:02}.png", kind.stem(), i)))
            .collect();
        state.scaled.insert(kind, vec![None; frames.len()]);
        state.frames.insert(kind, frames);
    }
}

/// Make the [`CursorKind::scale`]× copy of every frame that has loaded.
fn scale_cursor_frames(mut state: ResMut<CursorState>, mut images: ResMut<Assets<Image>>) {
    let CursorState { frames, scaled, .. } = &mut *state;
    for (kind, handles) in frames.iter() {
        let Some(copies) = scaled.get_mut(kind) else {
            continue;
        };
        for (handle, copy) in handles.iter().zip(copies.iter_mut()) {
            if copy.is_some() {
                continue;
            }
            if let Some(scaled_image) = images
                .get(handle)
                .and_then(|image| upscale(image, kind.scale()))
            {
                *copy = Some(images.add(scaled_image));
            }
        }
    }
}

/// `image` enlarged `scale`× by repeating pixels: crisp, like the
/// pixel-art original. `None` while it has no CPU-side pixels.
fn upscale(image: &Image, scale: u16) -> Option<Image> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let data = image.data.as_ref()?;
    let bpp = data.len().checked_div(w * h)?;
    let s = scale as usize;
    let mut out = Vec::with_capacity(data.len() * s * s);
    for row in data.chunks_exact(w * bpp) {
        let wide: Vec<u8> = row
            .chunks_exact(bpp)
            .flat_map(|px| std::iter::repeat_n(px, s).flatten().copied())
            .collect();
        for _ in 0..s {
            out.extend_from_slice(&wide);
        }
    }
    let mut scaled = Image::new(
        Extent3d {
            width: (w * s) as u32,
            height: (h * s) as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        out,
        image.texture_descriptor.format,
        // The window reads the pixels on the CPU.
        RenderAssetUsages::MAIN_WORLD,
    );
    scaled.sampler = image.sampler.clone();
    Some(scaled)
}

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
    // Only the frame count is needed to decide; the handle is cloned
    // once the cursor actually changes (most frames it doesn't).
    let Some(frame_count) = state.scaled.get(&kind).map(Vec::len) else {
        return;
    };
    if frame_count == 0 {
        return;
    }

    let prev_frame = state.current.map(|(_, f)| f).unwrap_or(0);
    let frame = if state.timer.just_finished() {
        (prev_frame + 1) % frame_count
    } else if state.current.is_none_or(|(k, _)| k != kind) {
        0
    } else {
        prev_frame.min(frame_count - 1)
    };

    if state.current == Some((kind, frame)) {
        return;
    }
    // Not scaled yet (still loading): keep the cursor as it is and try
    // again next frame.
    let Some(handle) = state.scaled[&kind][frame].clone() else {
        return;
    };
    state.current = Some((kind, frame));

    let (hx, hy) = kind.hotspot();
    commands
        .entity(*primary)
        .insert(CursorIcon::Custom(CustomCursor::Image(CustomCursorImage {
            handle,
            hotspot: (hx * kind.scale(), hy * kind.scale()),
            flip_x: false,
            flip_y: false,
            rect: None,
            texture_atlas: None,
        })));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every pixel becomes a `scale`-square block.
    #[test]
    fn upscale_repeats_pixels() {
        use bevy::render::render_resource::TextureFormat;
        let px = |i: u8| [i, i, i, 255];
        let data: Vec<u8> = [px(1), px(2), px(3), px(4)].concat();
        let image = Image::new(
            Extent3d {
                width: 2,
                height: 2,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            data,
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::MAIN_WORLD,
        );
        let s = CursorKind::Move.scale() as u32;
        let scaled = upscale(&image, s as u16).unwrap();
        assert_eq!((scaled.width(), scaled.height()), (2 * s, 2 * s));
        let data = scaled.data.unwrap();
        let at = |x: u32, y: u32| data[((y * 2 * s + x) * 4) as usize];
        for y in 0..2 * s {
            for x in 0..2 * s {
                assert_eq!(at(x, y), 1 + (x / s) as u8 + 2 * (y / s) as u8);
            }
        }
    }
}
