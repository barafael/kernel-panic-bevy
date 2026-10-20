//! The rule "only the fixed tick changes sim state; input goes through
//! [`SimCommand`](super::SimCommand)s", checked on the schedule's access
//! metadata.
//!
//! The test builds the whole game (the headless app: every plugin,
//! UI and dev tools included), initialises every schedule and collects
//! what the fixed tick's systems (`FixedFirst` … `FixedLast`) write:
//! components and resources. Every system of every other schedule
//! (`Update`, `PostUpdate`, state transitions, …) that writes one of
//! those too must be listed in [`FRAME_LOOP_WRITES`] with the reason it
//! is harmless; otherwise the test fails and names it. An exclusive
//! system outside the tick (it could write anything) must be listed in
//! [`FRAME_LOOP_EXCLUSIVE`].
//!
//! What it can't see: writes through `Commands` (an `insert` of a sim
//! component from an `Update` system), observers and hooks. Those are
//! covered by convention — input systems take [`super::Issue`] or the
//! [`super::CommandBus`] and no sim component — and by the determinism
//! tests that vary the frame pacing (`headless::tests`, `zeros`: extra
//! `Update`s between ticks) and the selection (`KP_NET_SELECT`).

use std::collections::BTreeSet;

use bevy::ecs::component::ComponentId;
use bevy::ecs::schedule::Schedules;
use bevy::ecs::system::SystemAccess;

use crate::headless::{Executor, build_app};

/// The fixed tick's schedules.
const FIXED: &[&str] = &[
    "FixedFirst",
    "FixedPreUpdate",
    "FixedUpdate",
    "FixedPostUpdate",
    "FixedLast",
];

/// Frame-loop systems writing a type the tick writes too, and why that
/// is fine: (system name part, written type part, reason). An empty
/// system part matches any system.
const FRAME_LOOP_WRITES: &[(&str, &str, &str)] = &[
    (
        "",
        "kernel_panic::net::CommandBus",
        "the bus is how input reaches the sim: issued commands apply at a tick head",
    ),
    (
        "",
        "bevy_asset::assets::Assets<",
        "render assets; the tick builds FX meshes and materials but never reads them back",
    ),
    ("", "GizmoStorage<", "debug drawing"),
    (
        "",
        "global_transform::GlobalTransform",
        "transform propagation; the sim reads `Transform` / `SimPose`",
    ),
    ("", "visibility::Visibility", "rendering"),
    (
        "",
        "mesh_material::MeshMaterial3d<",
        "which material a piece draws: the tick swaps them (fades, cloak, a taken unit's owner colour) but decides nothing by them",
    ),
    (
        "",
        "meshes::S3OModelCache",
        "the model cache: the same models whoever loads them first",
    ),
    (
        "",
        "kernel_panic::audio::SoundQueue",
        "the sim's sound outbox, drained by the frame loop",
    ),
    (
        "interpolate_sim_pose",
        "",
        "the drawn pose between ticks; `restore_sim_pose` puts the sim pose back in `FixedFirst`",
    ),
    (
        "spawn_prepared_map",
        "",
        "match setup: the map's world, spawned alike on every peer before tick 1",
    ),
    (
        "camera_smoothing",
        "transform::Transform",
        "the camera's own transform",
    ),
    (
        "update_health_bars",
        "transform::Transform",
        "health bar entities",
    ),
    (
        "update_formation_preview",
        "transform::Transform",
        "formation preview markers",
    ),
    ("sync_ghosts", "transform::Transform", "placement ghosts"),
    (
        "drive_turntable",
        "transform::Transform",
        "the map selector's preview mesh, on a render layer of its own, menu only",
    ),
    (
        "hex_farm_draw",
        "HexFarmView",
        "draw-only fields (animations, meshes); the tick's fields are `finished`, `released`, `owners`",
    ),
    (
        "update_minimap",
        "MinimapState",
        "the minimap image and its repaint timer",
    ),
];

/// Exclusive systems outside the tick, and why each is fine.
const FRAME_LOOP_EXCLUSIVE: &[(&str, &str)] = &[
    ("bevy_ecs::apply_deferred", "flushes commands"),
    ("message_update_system", "message buffers"),
    ("run_fixed_main", "runs the tick itself"),
    (
        "net::driver::drive_fixed_main",
        "the lockstep tick driver: runs the tick itself",
    ),
    (
        "net::replay_log::save_on_exit",
        "reads the match to save its replay file as the game quits; writes only the session's \"kept\" flag",
    ),
    (
        "net::lobby::back_to_room",
        "moves a match's connection into the lobby (`BackToRoom` → `LobbyRoom`)",
    ),
    ("run_main", "the main schedule"),
    ("handle_internal_asset_events", "asset server"),
    ("bevy_scene::", "scene spawning (no scenes in a match)"),
    ("bevy_world_serialization::", "world asset spawning"),
    ("bevy_state::state::transitions::", "state transitions"),
    ("probe_msaa_support", "render settings at startup"),
    (
        "map_loading::poll_map_load",
        "match setup: loads the map, swaps the match",
    ),
    (
        "map_loading::prepare_game_entry",
        "match setup: prepares the match the menu asked for",
    ),
];

/// Systems whose access Bevy can't bound, and why each is fine.
const FRAME_LOOP_UNBOUNDED: &[(&str, &str)] = &[(
    "bevy_animation::animate_targets",
    "glTF animation players; KP's units animate through COB scripts in the tick",
)];

fn allowed(system: &str, written: &str) -> bool {
    FRAME_LOOP_WRITES
        .iter()
        .any(|(s, w, _)| system.contains(s) && written.contains(w))
}

#[test]
fn frame_loop_never_writes_sim_state() {
    let mut app = build_app(Executor::Single);
    let world = app.world_mut();
    let mut schedules = world.remove_resource::<Schedules>().unwrap();
    for (_, schedule) in schedules.iter_mut() {
        schedule.initialize(world).unwrap();
    }

    // What the tick writes.
    let mut sim_writes: BTreeSet<ComponentId> = BTreeSet::new();
    let mut fixed_systems = 0;
    for (label, schedule) in schedules.iter() {
        if !FIXED.contains(&format!("{label:?}").as_str()) {
            continue;
        }
        for (_, system) in schedule.systems_with_access().unwrap() {
            fixed_systems += 1;
            if let SystemAccess::Shared(access) = system.access()
                && let Some(writes) = access.combined_access().writes().as_finite_set()
            {
                sim_writes.extend(writes.iter());
            }
        }
    }
    assert!(
        fixed_systems > 100 && sim_writes.len() > 50,
        "the tick's schedules weren't found ({fixed_systems} systems)"
    );

    let name = |id: ComponentId| {
        world
            .components()
            .get_name(id)
            .map(|n| n.to_string())
            .unwrap_or_default()
    };
    let mut offenders = Vec::new();
    for (label, schedule) in schedules.iter() {
        let label = format!("{label:?}");
        if FIXED.contains(&label.as_str()) {
            continue;
        }
        for (_, system) in schedule.systems_with_access().unwrap() {
            let sys = system.system().name().to_string();
            match system.access() {
                SystemAccess::None => {}
                SystemAccess::Exclusive => {
                    if !FRAME_LOOP_EXCLUSIVE.iter().any(|(s, _)| sys.contains(s)) {
                        offenders.push(format!("{label}: {sys} (exclusive)"));
                    }
                }
                SystemAccess::Shared(access) => {
                    let Some(writes) = access.combined_access().writes().as_finite_set() else {
                        if !FRAME_LOOP_UNBOUNDED.iter().any(|(s, _)| sys.contains(s)) {
                            offenders.push(format!("{label}: {sys} (writes everything)"));
                        }
                        continue;
                    };
                    for id in writes.iter().filter(|id| sim_writes.contains(id)) {
                        let written = name(id);
                        if !allowed(&sys, &written) {
                            offenders.push(format!("{label}: {sys} writes {written}"));
                        }
                    }
                }
            }
        }
    }
    world.insert_resource(schedules);
    assert!(
        offenders.is_empty(),
        "frame-loop systems write state the sim tick writes. Issue a `SimCommand` \
         through the `CommandBus` instead; if the write is presentation only, list it \
         in `net::audit::FRAME_LOOP_WRITES` with the reason:\n  {}",
        offenders.join("\n  ")
    );
}
