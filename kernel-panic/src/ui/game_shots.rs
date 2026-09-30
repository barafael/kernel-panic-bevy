//! Dev tool: screenshot the in-game HUD, then quit.
//!
//! Set `KP_GAME_SHOTS=<dir>` to enable (optionally `KP_GAME_SHOTS_MAP=<map
//! stem>`, default `Data_Cache_L1`; both read into [`DevOptions`] at
//! startup). The tool starts a quick skirmish as
//! System, then saves
//!
//! 1. `<dir>/homebase.png` — the Kernel selected with a few units queued
//!    (command panel with queue counts, build bar, tooltip box);
//! 2. `<dir>/esc.png` and `<dir>/esc_settings.png` — the Esc overlay and
//!    its Settings page over the running match;
//! 3. `<dir>/constructor.png` — a freshly built Assembler selected;
//! 4. `<dir>/constructor_placing.png` — the same with the Socket build
//!    command armed (active-button highlight, datavent highlight);
//!
//! and exits. Game time runs fast while waiting for the Assembler. Not
//! compiled for wasm (no disk).

use bevy::prelude::*;

use super::menu::{EscMenuOpen, EscSettingsOpen};
use super::save_screenshot;
use crate::game_setup::{AppState, DevOptions, SkirmishConfig, build_setup};
use crate::interaction::selection::Selected;
use crate::map_loading::MapCatalog;
use crate::units::components::{Homebase, TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::lifecycle::production::Producer;
use crate::units::lifecycle::spawning::Emerging;
use crate::units::player::LocalTeam;

use super::hud::command_panel::activation::ActivateCommand;
use super::hud::command_panel::commands::CmdId;

/// Frames of attract-mode demo before starting the skirmish.
const MENU_FRAMES: u32 = 90;
/// Frames between arranging a shot and capturing it.
const SETTLE: u32 = 45;
/// Give up waiting for the Assembler after this many frames.
const ASSEMBLER_TIMEOUT: u32 = 6000;

pub struct GameShotsPlugin;

impl Plugin for GameShotsPlugin {
    fn build(&self, app: &mut App) {
        let Some(dev) = app.world().get_resource::<DevOptions>() else {
            return;
        };
        if let Some(dir) = dev.game_shots.clone() {
            let map = dev
                .game_shots_map
                .clone()
                .unwrap_or_else(|| "Data_Cache_L1".into());
            app.insert_resource(Shots {
                dir,
                map,
                step: Step::Menu,
                frame: 0,
            })
            .add_systems(Update, run_shots);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Menu,
    WaitHomebase,
    HomebaseShot,
    EscShot,
    EscSettingsShot,
    WaitAssembler,
    ConstructorShot,
    PlacingShot,
    Exit,
}

#[derive(Resource)]
struct Shots {
    dir: std::path::PathBuf,
    map: String,
    step: Step,
    frame: u32,
}

impl Shots {
    fn go(&mut self, step: Step) {
        self.step = step;
        self.frame = 0;
    }

    fn shoot(&self, commands: &mut Commands, name: &str) {
        let path = self.dir.join(name);
        info!("KP_GAME_SHOTS: saving {}", path.display());
        save_screenshot(commands, path);
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn run_shots(
    mut shots: ResMut<Shots>,
    mut commands: Commands,
    catalog: Option<Res<MapCatalog>>,
    mut app_state: ResMut<NextState<AppState>>,
    local: Option<Res<LocalTeam>>,
    mut units: Query<(
        Entity,
        &UnitType,
        &TeamId,
        Has<Homebase>,
        Has<Emerging>,
        Option<&mut Producer>,
    )>,
    selected: Query<Entity, With<Selected>>,
    mut time: ResMut<Time<Virtual>>,
    mut activate: MessageWriter<ActivateCommand>,
    keys: Res<ButtonInput<KeyCode>>,
    mut esc: (ResMut<EscMenuOpen>, ResMut<EscSettingsOpen>),
    mut exit: MessageWriter<AppExit>,
) {
    shots.frame += 1;
    let team = local.map(|l| l.0);
    let mine = |t: &TeamId| Some(t.0) == team;
    match shots.step {
        Step::Menu => {
            if shots.frame < MENU_FRAMES {
                return;
            }
            let Some(catalog) = catalog else {
                return;
            };
            let names = catalog.names();
            let config = SkirmishConfig {
                map: names.iter().position(|n| *n == shots.map),
                ..default()
            };
            commands.insert_resource(build_setup(&config, &names));
            app_state.set(AppState::InGame);
            shots.go(Step::WaitHomebase);
        }
        Step::WaitHomebase => {
            // Let the match load and the homebase finish unfolding.
            time.set_relative_speed(4.0);
            let ready = units
                .iter()
                .any(|(_, _, t, home, emerging, _)| home && !emerging && mine(t));
            if !ready || shots.frame < 240 {
                return;
            }
            time.set_relative_speed(1.0);
            for (e, _, t, home, _, producer) in &mut units {
                if home && mine(t) {
                    commands.entity(e).insert(Selected);
                    if let Some(mut p) = producer {
                        for kind in [
                            UnitKind::Assembler,
                            UnitKind::Bit,
                            UnitKind::Bit,
                            UnitKind::Byte,
                        ] {
                            p.enqueue(kind);
                        }
                    }
                }
            }
            shots.go(Step::HomebaseShot);
        }
        Step::HomebaseShot => {
            if shots.frame == SETTLE {
                shots.shoot(&mut commands, "homebase.png");
            }
            if shots.frame > SETTLE + 5 {
                esc.0.0 = true;
                shots.go(Step::EscShot);
            }
        }
        Step::EscShot => {
            if shots.frame == SETTLE {
                shots.shoot(&mut commands, "esc.png");
            }
            if shots.frame > SETTLE + 5 {
                esc.1.0 = true;
                shots.go(Step::EscSettingsShot);
            }
        }
        Step::EscSettingsShot => {
            if shots.frame == SETTLE {
                shots.shoot(&mut commands, "esc_settings.png");
            }
            if shots.frame > SETTLE + 5 {
                esc.1.0 = false;
                esc.0.0 = false;
                shots.go(Step::WaitAssembler);
            }
        }
        Step::WaitAssembler => {
            time.set_relative_speed(8.0);
            let assembler = units.iter().find(|(_, ut, t, _, emerging, _)| {
                ut.0 == UnitKind::Assembler && mine(t) && !emerging
            });
            if let Some((e, ..)) = assembler {
                time.set_relative_speed(1.0);
                for s in &selected {
                    commands.entity(s).remove::<Selected>();
                }
                commands.entity(e).insert(Selected);
                shots.go(Step::ConstructorShot);
            } else if shots.frame > ASSEMBLER_TIMEOUT {
                warn!("KP_GAME_SHOTS: no Assembler appeared; exiting");
                shots.go(Step::Exit);
            }
        }
        Step::ConstructorShot => {
            if shots.frame == SETTLE {
                shots.shoot(&mut commands, "constructor.png");
            }
            if shots.frame == SETTLE + 5 {
                activate.write(ActivateCommand::click(
                    CmdId::Build(UnitKind::Socket),
                    false,
                    &keys,
                ));
                shots.go(Step::PlacingShot);
            }
        }
        Step::PlacingShot => {
            if shots.frame == SETTLE {
                shots.shoot(&mut commands, "constructor_placing.png");
            }
            if shots.frame > SETTLE + 20 {
                shots.go(Step::Exit);
            }
        }
        Step::Exit => {
            exit.write(AppExit::Success);
        }
    }
}
