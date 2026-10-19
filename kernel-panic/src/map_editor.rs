//! The datavent editor (`KP_MAP_EDIT=<stem>`): annotate which way the
//! building on each datavent faces, and save it into the map.
//!
//! Every datavent carries a factory facing in its `.kpmap`
//! (`spring_map::map_types::Datavent::facing`: `0` south … `3` west). A
//! building placed on the vent spawns turned that way, and a factory's
//! units leave along it (`units::lifecycle::production`). The shipped
//! facings were computed once by the map converter (face the map centre,
//! unless the exit is blocked: `spring_map::datavent`); this editor lets
//! a person choose them instead.
//!
//! It boots a sandbox match on the map with one System seat, so a
//! facing can be tried at once: build a Socket on the vent and watch its
//! Bits leave. Every datavent shows an arrow along its facing. Hover a
//! vent and press `V` to turn it clockwise (`Shift+V`: counter-
//! clockwise); the change applies to the running match at once (to
//! buildings started after it). `Ctrl+S` writes the facings into the
//! `.kpmap` ([`spring_map::baked::set_datavent_facings`]: only those
//! bytes of the payload change).
//!
//! The map's bytes are part of the lobby and replay hash
//! (`net::compat`): an edited map no longer matches older builds, and a
//! golden replay on it must be re-recorded. Hex Farm's datavents are
//! rolled per match, so it has nothing to annotate.

use std::path::PathBuf;

use bevy::gizmos::config::GizmoConfigStore;
use bevy::prelude::*;
use spring_map::datavent::facing_vector;

use crate::game_setup::{AppState, DevOptions, GameSetup, PlayerSpec};
use crate::map_loading::MapCatalog;
use crate::rendering::camera::RtsCamera;
use crate::terrain::geovent::GeoventSmoker;
use crate::units::components::Faction;

/// Frames of menu before starting the match (the map catalog must be
/// scanned first; as the `bot` dev tool).
const MENU_FRAMES: u32 = 60;

/// How close (elmos, in XZ) the cursor must be to a vent's centre to
/// hover it: a vent building is 4×4 squares, 32 elmos across.
const HOVER_RADIUS: f32 = 20.0;

/// Length of a facing arrow, elmos: a little past the edge of a vent
/// building, where the factory's units come out.
const ARROW_LENGTH: f32 = 56.0;

/// The maps the golden replays play on (`headless::golden`): editing
/// one changes its hash, so the replays must be re-recorded.
const GOLDEN_MAPS: [&str; 4] = [
    "Speed_Balls_16_Way",
    "Quad_Core",
    "Dual_Core",
    "Marble_Madness_Map",
];

pub struct MapEditorPlugin;

impl Plugin for MapEditorPlugin {
    fn build(&self, app: &mut App) {
        let Some(map) = app
            .world()
            .get_resource::<DevOptions>()
            .and_then(|dev| dev.map_edit.clone())
        else {
            return;
        };
        app.insert_resource(VentEditor::new(map))
            .init_gizmo_group::<VentEditorGizmos>()
            .add_systems(Startup, configure_gizmos)
            .add_systems(Update, boot.run_if(in_state(AppState::Menu)))
            .add_systems(
                Update,
                (attach, hover, edit, save, draw, help)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// The editor's arrows: thick, and drawn over the terrain and the vent
/// smoke so a vent in a pit or behind a ramp still shows its facing.
#[derive(Default, Reflect)]
struct VentEditorGizmos;

impl GizmoConfigGroup for VentEditorGizmos {}

fn configure_gizmos(mut store: ResMut<GizmoConfigStore>) {
    let (config, _) = store.config_mut::<VentEditorGizmos>();
    config.line.width = 4.0;
    config.depth_bias = -1.0;
}

/// One datavent of the map file, matched to its vent in the match.
struct EditVent {
    /// The vent entity (`GeoventSmoker`), to apply a change live.
    smoker: Entity,
    /// World position of the vent.
    pos: Vec3,
}

#[derive(Resource)]
struct VentEditor {
    map: String,
    menu_frames: u32,
    started: bool,
    /// The `.kpmap` this match was loaded from.
    path: Option<PathBuf>,
    /// Its bytes as last read or written.
    file: Vec<u8>,
    /// The facings being edited, in the file's datavent order.
    facings: Vec<u8>,
    /// The facings in `file`.
    saved: Vec<u8>,
    /// The file's datavent positions (XZ), once read.
    positions: Option<Vec<Vec2>>,
    /// `vents[i]` is datavent `i`, once matched to the match's vents.
    vents: Vec<EditVent>,
    /// Matched (or given up on).
    attached: bool,
    /// Frames spent waiting for the match's vents.
    attach_frames: u32,
    /// The hovered datavent's index.
    hovered: Option<usize>,
    /// The last save or load problem, shown in the help panel.
    status: String,
}

impl VentEditor {
    fn new(map: String) -> Self {
        Self {
            map,
            menu_frames: 0,
            started: false,
            path: None,
            file: Vec::new(),
            facings: Vec::new(),
            saved: Vec::new(),
            positions: None,
            vents: Vec::new(),
            attached: false,
            attach_frames: 0,
            hovered: None,
            status: String::new(),
        }
    }

    fn unsaved(&self) -> usize {
        self.facings
            .iter()
            .zip(&self.saved)
            .filter(|(a, b)| a != b)
            .count()
    }
}

/// Start the sandbox match once the menu has scanned the map catalog.
fn boot(
    mut editor: ResMut<VentEditor>,
    mut commands: Commands,
    catalog: Option<Res<MapCatalog>>,
    mut app_state: ResMut<NextState<AppState>>,
) {
    if editor.started {
        return;
    }
    editor.menu_frames += 1;
    if editor.menu_frames < MENU_FRAMES {
        return;
    }
    let Some(catalog) = catalog else {
        return;
    };
    let Some(path) = catalog
        .0
        .iter()
        .find(|p| {
            p.file_stem()
                .is_some_and(|s| s.to_string_lossy() == editor.map)
        })
        .cloned()
    else {
        error!(
            "KP_MAP_EDIT: map {} not in the catalog {:?}",
            editor.map,
            catalog.names()
        );
        editor.started = true;
        return;
    };
    commands.insert_resource(GameSetup {
        map: editor.map.clone(),
        players: vec![PlayerSpec {
            faction: Faction::System,
            ally_team: 0,
            ai: false,
        }],
        difficulty: 1,
        demo: false,
        showcase: None,
        sandbox: true,
        seed: crate::game_setup::random_seed(),
        rules: Default::default(),
    });
    app_state.set(AppState::InGame);
    editor.path = Some(path);
    editor.started = true;
}

/// Frames to wait for the match's vents before reporting a datavent
/// that has none (10 s; the map loads in well under that).
const ATTACH_TIMEOUT_FRAMES: u32 = 600;

/// Read the map file's datavents, then match each to its vent in the
/// match. The vents spawn while the map loads, and on the first frames
/// the menu demo's vents (another map) may still be around, so this
/// retries every frame until every datavent has its vent.
fn attach(mut editor: ResMut<VentEditor>, smokers: Query<(Entity, &GeoventSmoker)>) {
    if editor.attached {
        return;
    }
    if editor.positions.is_none() {
        let Some(path) = editor.path.clone() else {
            return;
        };
        let file = match std::fs::read(&path) {
            Ok(file) => file,
            Err(e) => {
                editor.status = format!("can't read {}: {e}", path.display());
                editor.attached = true;
                return;
            }
        };
        let map = match spring_map::baked::read_baked_map(&file) {
            Ok(map) => map,
            Err(e) => {
                editor.status = format!("can't decode {}: {e}", path.display());
                editor.attached = true;
                return;
            }
        };
        if map.lua_compositing.is_some() || map.datavents.is_empty() {
            editor.status = "this map's datavents are rolled per match: nothing to annotate".into();
            editor.attached = true;
            return;
        }
        editor.positions = Some(map.datavents.iter().map(|v| Vec2::new(v.x, v.z)).collect());
        editor.facings = map.datavents.iter().map(|v| v.facing).collect();
        editor.saved = editor.facings.clone();
        editor.file = file;
    }
    let Some(positions) = editor.positions.as_ref() else {
        return;
    };
    // Datavent `i` at `at` without a vent is the `Err`.
    let matched: Result<Vec<EditVent>, (usize, Vec2)> = positions
        .iter()
        .enumerate()
        .map(|(i, at)| {
            smokers
                .iter()
                .find(|(_, s)| (s.pos.xz() - *at).length_squared() < 1.0)
                .map(|(smoker, s)| EditVent { smoker, pos: s.pos })
                .ok_or((i, *at))
        })
        .collect();
    let vents = match matched {
        Ok(vents) => vents,
        Err((i, at)) => {
            editor.attach_frames += 1;
            if editor.attach_frames == ATTACH_TIMEOUT_FRAMES {
                editor.status = format!(
                    "datavent {i} at ({}, {}) has no vent in the match",
                    at.x, at.y
                );
            }
            return;
        }
    };
    editor.vents = vents;
    editor.attached = true;
    editor.status.clear();
    info!(
        "KP_MAP_EDIT: {} datavents on {}",
        editor.vents.len(),
        editor.map
    );
}

/// The datavent under the cursor: the ray meets each vent's ground
/// plane within [`HOVER_RADIUS`] of its centre.
fn hover(
    mut editor: ResMut<VentEditor>,
    windows: Query<&Window>,
    camera: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
) {
    let ray = windows.single().ok().and_then(|window| {
        let cursor = window.cursor_position()?;
        let (camera, transform) = camera.single().ok()?;
        camera.viewport_to_world(transform, cursor).ok()
    });
    editor.hovered = ray.and_then(|ray| {
        editor
            .vents
            .iter()
            .enumerate()
            .filter_map(|(i, vent)| {
                let t = (vent.pos.y - ray.origin.y) / ray.direction.y;
                let hit = ray.origin + ray.direction * t;
                let d = Vec2::new(hit.x - vent.pos.x, hit.z - vent.pos.z).length();
                (t > 0.0 && d < HOVER_RADIUS).then_some((i, d))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    });
}

/// `V` turns the hovered datavent clockwise (seen from above), `Shift+V`
/// counter-clockwise, and the match's vent with it.
fn edit(
    mut editor: ResMut<VentEditor>,
    keys: Res<ButtonInput<KeyCode>>,
    mut smokers: Query<&mut GeoventSmoker>,
) {
    if !keys.just_pressed(KeyCode::KeyV) {
        return;
    }
    let Some(i) = editor.hovered else {
        return;
    };
    // South (+z) → west (−x) → north → east is clockwise from above.
    let step = if keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]) {
        1
    } else {
        3
    };
    let facing = (editor.facings[i] + step) % 4;
    editor.facings[i] = facing;
    if let Ok(mut smoker) = smokers.get_mut(editor.vents[i].smoker) {
        smoker.facing = facing;
    }
}

/// `Ctrl+S` writes the facings into the `.kpmap`.
fn save(mut editor: ResMut<VentEditor>, keys: Res<ButtonInput<KeyCode>>) {
    if !keys.just_pressed(KeyCode::KeyS)
        || !keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight])
    {
        return;
    }
    let Some(path) = editor.path.clone() else {
        return;
    };
    if editor.vents.is_empty() {
        return;
    }
    let file = match spring_map::baked::set_datavent_facings(&editor.file, &editor.facings) {
        Ok(file) => file,
        Err(e) => {
            editor.status = format!("not saved: {e}");
            return;
        }
    };
    // Write a sibling and rename it over the map: a crash mid-write
    // leaves the old file intact.
    let tmp = path.with_extension("kpmap.tmp");
    if let Err(e) = std::fs::write(&tmp, &file).and_then(|()| std::fs::rename(&tmp, &path)) {
        editor.status = format!("not saved: {}: {e}", path.display());
        return;
    }
    editor.file = file;
    editor.saved = editor.facings.clone();
    editor.status = format!("saved {}", path.display());
    if GOLDEN_MAPS.contains(&editor.map.as_str()) {
        editor.status += "\nthis map has golden replays: re-record them \
            (cargo run --release -p kernel-panic --features replay -- --record-golden)";
    }
    info!("KP_MAP_EDIT: {}", editor.status);
}

/// An arrow along each datavent's facing: yellow when hovered, orange
/// when changed and not saved, cyan otherwise.
fn draw(editor: Res<VentEditor>, mut gizmos: Gizmos<VentEditorGizmos>) {
    for (i, vent) in editor.vents.iter().enumerate() {
        let color = if editor.hovered == Some(i) {
            Color::srgb(1.0, 0.95, 0.2)
        } else if editor.facings[i] != editor.saved[i] {
            Color::srgb(1.0, 0.5, 0.1)
        } else {
            Color::srgb(0.2, 0.9, 1.0)
        };
        let (dx, dz) = facing_vector(editor.facings[i]);
        let start = vent.pos + Vec3::Y * 4.0;
        let end = start + Vec3::new(dx, 0.0, dz) * ARROW_LENGTH;
        gizmos.arrow(start, end, color).with_tip_length(18.0);
        gizmos.circle(
            Isometry3d::new(start, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
            HOVER_RADIUS,
            color,
        );
    }
}

#[derive(Component)]
struct EditorHelp;

/// The help panel: keys, the vent count, unsaved changes, the last save.
fn help(
    mut commands: Commands,
    editor: Res<VentEditor>,
    mut panel: Query<&mut Text, With<EditorHelp>>,
) {
    let facing_name = |f: u8| ["south", "east", "north", "west"][f as usize % 4];
    let mut text = format!(
        "Datavent editor: {} ({} datavents)\n\
         Hover a vent: V turns it clockwise, Shift+V counter-clockwise\n\
         Ctrl+S saves into the map file",
        editor.map,
        editor.vents.len()
    );
    if let Some(i) = editor.hovered {
        text += &format!("\nvent {i}: faces {}", facing_name(editor.facings[i]));
    }
    let unsaved = editor.unsaved();
    if unsaved > 0 {
        text += &format!("\n{unsaved} unsaved change(s)");
    }
    if !editor.status.is_empty() {
        text += &format!("\n{}", editor.status);
    }
    if let Ok(mut panel) = panel.single_mut() {
        if panel.0 != text {
            panel.0 = text;
        }
        return;
    }
    commands.spawn((
        EditorHelp,
        Text::new(text),
        TextFont::from_font_size(16.0),
        TextColor(Color::srgb(0.9, 0.95, 1.0)),
        Node {
            position_type: PositionType::Absolute,
            // Right of the minimap.
            top: Val::Px(12.0),
            left: Val::Px(230.0),
            padding: UiRect::all(Val::Px(6.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
        GlobalZIndex(50),
        Pickable::IGNORE,
        DespawnOnExit(AppState::InGame),
    ));
}

#[cfg(test)]
mod tests {
    use bevy::ecs::system::RunSystemOnce;

    use super::*;

    /// A world with the editor on a scratch copy of `map`, attached to
    /// one spawned vent per datavent.
    fn editor_world(map: &str) -> (World, PathBuf) {
        let source = crate::paths::assets_dir()
            .join("maps")
            .join(format!("{map}.kpmap"));
        let dir =
            std::env::temp_dir().join(format!("kp-map-editor-{}-{}", std::process::id(), map));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{map}.kpmap"));
        std::fs::copy(&source, &path).unwrap();

        let mut world = World::new();
        let mut editor = VentEditor::new(map.into());
        editor.path = Some(path.clone());
        world.insert_resource(editor);
        let datavents = spring_map::baked::read_baked_map(&std::fs::read(&path).unwrap())
            .unwrap()
            .datavents;
        for vent in &datavents {
            world.spawn(GeoventSmoker {
                pos: Vec3::new(vent.x, 10.0, vent.z),
                facing: vent.facing,
                emit_timer: 0.0,
                rng: 1,
            });
        }
        world.run_system_once(attach).unwrap();
        assert_eq!(world.resource::<VentEditor>().vents.len(), datavents.len());
        (world, path)
    }

    fn press(world: &mut World, keys: &[KeyCode]) {
        let mut input = ButtonInput::<KeyCode>::default();
        for &key in keys {
            input.press(key);
        }
        world.insert_resource(input);
    }

    /// `V` turns the hovered vent clockwise, in the match too; `Ctrl+S`
    /// writes it, and nothing else, into the map file.
    #[test]
    fn turn_and_save() {
        let (mut world, path) = editor_world("Central_Hub");
        let before: Vec<u8> = world.resource::<VentEditor>().facings.clone();
        world.resource_mut::<VentEditor>().hovered = Some(3);

        press(&mut world, &[KeyCode::KeyV]);
        world.run_system_once(edit).unwrap();
        let turned = (before[3] + 3) % 4;
        let editor = world.resource::<VentEditor>();
        assert_eq!(editor.facings[3], turned, "clockwise: one step back");
        assert_eq!(editor.unsaved(), 1);
        let smoker = editor.vents[3].smoker;
        assert_eq!(world.get::<GeoventSmoker>(smoker).unwrap().facing, turned);

        press(&mut world, &[KeyCode::ControlLeft, KeyCode::KeyS]);
        world.run_system_once(save).unwrap();
        assert_eq!(world.resource::<VentEditor>().unsaved(), 0);
        let saved: Vec<u8> = spring_map::baked::read_baked_map(&std::fs::read(&path).unwrap())
            .unwrap()
            .datavents
            .iter()
            .map(|v| v.facing)
            .collect();
        let mut expected = before;
        expected[3] = turned;
        assert_eq!(saved, expected);
        assert!(!path.with_extension("kpmap.tmp").exists());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// `Shift+V` turns the other way; nothing is hovered, nothing turns.
    #[test]
    fn shift_turns_back_and_no_hover_does_nothing() {
        let (mut world, path) = editor_world("Quad_Core");
        let before: Vec<u8> = world.resource::<VentEditor>().facings.clone();

        press(&mut world, &[KeyCode::KeyV]);
        world.run_system_once(edit).unwrap();
        assert_eq!(world.resource::<VentEditor>().facings, before);

        world.resource_mut::<VentEditor>().hovered = Some(0);
        press(&mut world, &[KeyCode::ShiftLeft, KeyCode::KeyV]);
        world.run_system_once(edit).unwrap();
        assert_eq!(
            world.resource::<VentEditor>().facings[0],
            (before[0] + 1) % 4
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
