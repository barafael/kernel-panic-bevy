//! Dev tool: screenshot every launch-menu page, then quit.
//!
//! Set `KP_MENU_SHOTS=<dir>` to enable (and optionally
//! `KP_MENU_SHOTS_WARMUP=<frames>` to let the demo play longer first;
//! both read into [`DevOptions`] at startup).
//! After the attract-mode demo has had time to load, the tool flips through each [`MenuPage`], saves
//! `<dir>/<page>.png`, and exits — a quick way to review menu layout
//! without clicking through by hand. Not compiled for wasm (no disk).

use bevy::prelude::*;

use super::menu::MenuPage;
use super::save_screenshot;
use crate::game_setup::DevOptions;

/// Default frames to let the demo world load before the first shot.
const WARMUP_FRAMES: u32 = 600;
/// Frames between page flip and capture (menu rebuild + render).
const SETTLE_FRAMES: u32 = 30;

const PAGES: [(MenuPage, &str); 8] = [
    (MenuPage::Main, "main"),
    (MenuPage::QuickSkirmish, "quick"),
    (MenuPage::AdvancedSkirmish, "advanced"),
    (MenuPage::MapList, "maps"),
    (MenuPage::Showcase, "showcase"),
    (MenuPage::Credits, "credits"),
    (MenuPage::Readme, "readme"),
    (MenuPage::Settings, "settings"),
];

pub struct MenuShotsPlugin;

impl Plugin for MenuShotsPlugin {
    fn build(&self, app: &mut App) {
        let Some(dev) = app.world().get_resource::<DevOptions>() else {
            return;
        };
        if let Some(dir) = dev.menu_shots.clone() {
            let warmup = dev.menu_shots_warmup.unwrap_or(WARMUP_FRAMES);
            app.insert_resource(ShotDir(dir, warmup))
                .add_systems(Update, take_menu_shots);
        }
    }
}

#[derive(Resource)]
struct ShotDir(std::path::PathBuf, u32);

fn take_menu_shots(
    dir: Res<ShotDir>,
    mut frame: Local<u32>,
    mut page: ResMut<MenuPage>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
    *frame += 1;
    let Some(t) = frame.checked_sub(dir.1) else {
        return;
    };
    let (idx, phase) = ((t / SETTLE_FRAMES) as usize, t % SETTLE_FRAMES);
    match PAGES.get(idx) {
        Some((p, name)) => {
            if phase == 0 {
                *page = *p;
            } else if phase == SETTLE_FRAMES - 1 {
                save_screenshot(&mut commands, dir.0.join(format!("{name}.png")));
            }
        }
        // One extra settle period so the last save lands before exit.
        None if idx == PAGES.len() && phase == SETTLE_FRAMES - 1 => {
            exit.write(AppExit::Success);
        }
        None => {}
    }
}
