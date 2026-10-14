//! Dev tool: screenshot every launch-menu page, audit its layout, then
//! quit.
//!
//! Set `KP_MENU_SHOTS=<dir>` to enable (and optionally
//! `KP_MENU_SHOTS_WARMUP=<frames>` to let the demo play longer first;
//! both read into [`DevOptions`] at startup). After the attract-mode
//! demo has had time to load, the tool flips through each [`MenuPage`],
//! saves `<dir>/<page>.png`, writes `<dir>/<page>.layout.txt` (every
//! button and label frame's rect in logical pixels, then the problems
//! found) and appends the problems to `<dir>/issues.txt`; it exits
//! once every page is done. `scripts/menu-layout-check.sh` runs it at
//! several window sizes under a virtual X display.
//!
//! The audit flags a frame that leaves the window, two frames that
//! overlap (neither inside the other), a font under
//! [`MIN_FONT_PX`] and a frame wider than [`MAX_WIDTH_FRACTION`] of
//! the window. Not compiled for wasm (no disk).

use std::fmt::Write as _;

use bevy::prelude::*;
use bevy::ui::UiGlobalTransform;

use super::menu::{MenuButton, MenuFrame, MenuPage};
use super::save_screenshot;
use crate::game_setup::DevOptions;
use crate::sim::DetFloat;

/// Default frames (at 30 per second) to let the demo world load before
/// the first shot; paced by the clock, since a virtual display has no
/// vsync and would race through the pages in a second.
const WARMUP_FRAMES: u32 = 600;
/// Seconds between page flip and capture (menu rebuild + render).
const SETTLE_SECS: f32 = 1.0;
/// Smallest readable label on any sane window.
const MIN_FONT_PX: f32 = 11.0;
/// A frame wider than this much of the window has outgrown its page.
const MAX_WIDTH_FRACTION: f32 = 0.9;
/// Frames may touch; they overlap when both axes intersect by more.
const OVERLAP_TOLERANCE: f32 = 1.0;
/// A frame may sit on the window edge; beyond this it has left it.
const EDGE_TOLERANCE: f32 = 0.5;

const PAGES: [(MenuPage, &str); 9] = [
    (MenuPage::Main, "main"),
    (MenuPage::QuickSkirmish, "quick"),
    (MenuPage::AdvancedSkirmish, "advanced"),
    (MenuPage::MapList, "maps"),
    (MenuPage::Showcase, "showcase"),
    (MenuPage::Credits, "credits"),
    (MenuPage::Readme, "readme"),
    (MenuPage::Settings, "settings"),
    (MenuPage::Audio, "audio"),
];

pub struct MenuShotsPlugin;

impl Plugin for MenuShotsPlugin {
    fn build(&self, app: &mut App) {
        let Some(dev) = app.world().get_resource::<DevOptions>() else {
            return;
        };
        if let Some(dir) = dev.menu_shots.clone() {
            let warmup = dev.menu_shots_warmup.unwrap_or(WARMUP_FRAMES);
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("issues.txt"), "");
            app.insert_resource(ShotDir(dir, warmup))
                // After UI layout, so the audit reads this frame's rects.
                .add_systems(
                    PostUpdate,
                    take_menu_shots.after(bevy::ui::UiSystems::Layout),
                );
        }
    }
}

#[derive(Resource)]
struct ShotDir(std::path::PathBuf, u32);

/// One audited frame: what it says, where it is (logical px) and its
/// label's font size.
struct Frame {
    entity: Entity,
    name: String,
    min: Vec2,
    max: Vec2,
    font: Option<f32>,
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn take_menu_shots(
    dir: Res<ShotDir>,
    time: Res<Time>,
    mut done: Local<Option<usize>>,
    mut page: ResMut<MenuPage>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
    windows: Query<&Window>,
    nodes: Query<
        (Entity, &ComputedNode, &UiGlobalTransform, Option<&Children>),
        Or<(With<MenuButton>, With<MenuFrame>)>,
    >,
    parents: Query<&ChildOf>,
    texts: Query<(&Text, &TextFont)>,
) {
    let t = time.elapsed_secs() - dir.1 as f32 / 30.0;
    if t < 0.0 {
        return;
    }
    // Page `idx` is shown at the start of its slot and captured halfway
    // through it; `done` remembers the last step taken.
    let idx = (t / SETTLE_SECS) as usize;
    let half = (t / SETTLE_SECS).fract() >= 0.5;
    let step = idx * 2 + usize::from(half);
    if *done == Some(step) {
        return;
    }
    *done = Some(step);
    match PAGES.get(idx) {
        Some((p, name)) => {
            if !half {
                *page = *p;
            } else {
                save_screenshot(&mut commands, dir.0.join(format!("{name}.png")));
                if let Ok(window) = windows.single() {
                    let frames = collect_frames(window, &nodes, &parents, &texts);
                    let issues = audit(window, &frames, &parents);
                    write_report(&dir.0, name, window, &frames, &issues);
                }
            }
        }
        // One extra slot so the last save lands before exit.
        None if idx == PAGES.len() && half => {
            exit.write(AppExit::Success);
        }
        None => {}
    }
}

#[allow(clippy::type_complexity)]
fn collect_frames(
    window: &Window,
    nodes: &Query<
        (Entity, &ComputedNode, &UiGlobalTransform, Option<&Children>),
        Or<(With<MenuButton>, With<MenuFrame>)>,
    >,
    parents: &Query<&ChildOf>,
    texts: &Query<(&Text, &TextFont)>,
) -> Vec<Frame> {
    let scale = window.scale_factor();
    let mut frames: Vec<Frame> = nodes
        .iter()
        .map(|(entity, cnode, gtf, children)| {
            let centre = gtf.translation / scale;
            let half = cnode.size() / scale * 0.5;
            let label = children
                .into_iter()
                .flat_map(|c| c.iter())
                .find_map(|child| texts.get(child).ok());
            Frame {
                entity,
                name: label.map_or_else(
                    || format!("frame {entity}"),
                    |(text, _)| text.0.replace('\n', " / "),
                ),
                min: centre - half,
                max: centre + half,
                font: label.map(|(_, font)| match font.font_size {
                    FontSize::Px(px) => px,
                    _ => 0.0,
                }),
            }
        })
        .collect();
    // Top to bottom, left to right: the report reads like the page.
    frames.sort_by(|a, b| {
        a.min
            .y
            .total_cmp(&b.min.y)
            .then_with(|| a.min.x.total_cmp(&b.min.x))
    });
    let _ = parents;
    frames
}

fn is_ancestor(parents: &Query<&ChildOf>, ancestor: Entity, mut entity: Entity) -> bool {
    while let Ok(parent) = parents.get(entity) {
        if parent.parent() == ancestor {
            return true;
        }
        entity = parent.parent();
    }
    false
}

/// The problems with `frames` on a `window`: see the module doc.
fn audit(window: &Window, frames: &[Frame], parents: &Query<&ChildOf>) -> Vec<String> {
    let (w, h) = (window.width(), window.height());
    let mut issues = Vec::new();
    for f in frames {
        if f.min.x < -EDGE_TOLERANCE
            || f.min.y < -EDGE_TOLERANCE
            || f.max.x > w + EDGE_TOLERANCE
            || f.max.y > h + EDGE_TOLERANCE
        {
            issues.push(format!(
                "off-window: \"{}\" [{:.0} {:.0} {:.0} {:.0}] on {w:.0}x{h:.0}",
                f.name, f.min.x, f.min.y, f.max.x, f.max.y
            ));
        }
        if let Some(px) = f.font
            && px < MIN_FONT_PX
        {
            issues.push(format!("too-small: \"{}\" font {px:.1}px", f.name));
        }
        if f.max.x - f.min.x > w * MAX_WIDTH_FRACTION {
            issues.push(format!(
                "too-large: \"{}\" is {:.0}px wide on {w:.0}",
                f.name,
                f.max.x - f.min.x
            ));
        }
    }
    for (i, a) in frames.iter().enumerate() {
        for b in &frames[i + 1..] {
            if is_ancestor(parents, a.entity, b.entity) || is_ancestor(parents, b.entity, a.entity)
            {
                continue;
            }
            let dx = a.max.x.det_min(b.max.x) - a.min.x.det_max(b.min.x);
            let dy = a.max.y.det_min(b.max.y) - a.min.y.det_max(b.min.y);
            if dx > OVERLAP_TOLERANCE && dy > OVERLAP_TOLERANCE {
                issues.push(format!(
                    "overlap: \"{}\" and \"{}\" by {dx:.0}x{dy:.0}px",
                    a.name, b.name
                ));
            }
        }
    }
    issues
}

fn write_report(
    dir: &std::path::Path,
    page: &str,
    window: &Window,
    frames: &[Frame],
    issues: &[String],
) {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{page}: window {:.0}x{:.0} logical (scale {:.2}), {} frames, {} issues",
        window.width(),
        window.height(),
        window.scale_factor(),
        frames.len(),
        issues.len()
    );
    for f in frames {
        let _ = writeln!(
            out,
            "  [{:6.0} {:6.0} {:6.0} {:6.0}] font {:>5} \"{}\"",
            f.min.x,
            f.min.y,
            f.max.x,
            f.max.y,
            f.font.map_or("-".to_string(), |px| format!("{px:.1}")),
            f.name
        );
    }
    for issue in issues {
        let _ = writeln!(out, "  ! {issue}");
    }
    let _ = std::fs::write(dir.join(format!("{page}.layout.txt")), &out);
    if !issues.is_empty() {
        let mut all = String::new();
        for issue in issues {
            let _ = writeln!(all, "{page}: {issue}");
        }
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("issues.txt"))
        {
            let _ = f.write_all(all.as_bytes());
        }
    }
    info!(
        "menu shots: {page}: {} frames, {} issues",
        frames.len(),
        issues.len()
    );
}
