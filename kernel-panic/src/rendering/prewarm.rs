//! First uses that would cost a match a frame, paid while its map
//! loads instead ([`warm_match`], from `spawn_prepared_map`).
//!
//! Two kinds:
//!
//! - **Render pipelines.** Every `StandardMaterial` draw needs the
//!   pipeline for its vertex layout, blend, cull mode and depth bias,
//!   compiled the first time an entity of that combination is drawn.
//!   On Linux and Windows that happens on a task pool, and the variant
//!   draws nothing until it is done: a 0.08 s beam or a short CEG can
//!   be missed outright. On macOS and the web it compiles on the spot
//!   (`pipeline_cache.rs` `create_pipeline_task` blocks there), and
//!   the frame stalls for each one — on the web, the main thread.
//!   [`VARIANTS`] lists the combinations that first draw mid-match;
//!   one tiny entity of each stands for a few frames, so every one is
//!   specialised and queued during the load. Compilation goes on
//!   without them: the pipeline cache keeps what it was asked for.
//! - **Unit and shot assets.** A unit kind's first spawn in an owner's
//!   colour colours its texture pixel by pixel (`unit_material`) and
//!   builds its piece meshes (`piece_layout`); a weapon's first shot
//!   decodes its textures from the bundle. Both are cached for the
//!   app's life, so the load does it once for every kind the seated
//!   factions build.

use bevy::camera::visibility::NoFrustumCulling;
use bevy::prelude::*;
use bevy::render::render_resource::Face;
use strum::VariantArray;

use crate::game_setup::PlayerSpec;
use crate::interaction::selection::highlight::GLOW_DEPTH_BIAS;
use crate::units::assets::meshes::{piece_layout, unit_material};
use crate::units::components::Faction;
use crate::units::content::definitions::UnitKind;
use crate::units::lifecycle::spawning::SpawnContext;
use crate::units::team_colors::TeamColors;

pub(super) struct PrewarmPlugin;

impl Plugin for PrewarmPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, retire_warmups);
    }
}

/// One pipeline variant to warm: the vertex layout (S3O pieces and
/// primitive meshes carry position, normal and UV; the effect batches
/// add a vertex colour) and the material's pipeline key.
struct Variant {
    vertex_colors: bool,
    alpha_mode: AlphaMode,
    cull_mode: Option<Face>,
    depth_bias: f32,
}

const fn variant(
    vertex_colors: bool,
    alpha_mode: AlphaMode,
    cull_mode: Option<Face>,
    depth_bias: f32,
) -> Variant {
    Variant {
        vertex_colors,
        alpha_mode,
        cull_mode,
        depth_bias,
    }
}

const BACK: Option<Face> = Some(Face::Back);

/// The variants that first draw mid-match. Bevy keys `Add` and
/// `Premultiplied` to one blend; they are listed by what uses them.
const VARIANTS: &[Variant] = &[
    // Models: units (warm from the start already), their double-sided
    // shatter shards (`debris`).
    variant(false, AlphaMode::Opaque, BACK, 0.0),
    variant(false, AlphaMode::Opaque, None, 0.0),
    // Emerge and cloak fades (`model_fade`), the placement ghost,
    // health bars, the order marker.
    variant(false, AlphaMode::Blend, BACK, 0.0),
    // The ONS shield dome.
    variant(false, AlphaMode::Blend, None, 0.0),
    // Death and impact bursts, the geo highlight.
    variant(false, AlphaMode::Add, BACK, 0.0),
    // The selection glow (`highlight`).
    variant(false, AlphaMode::Add, BACK, GLOW_DEPTH_BIAS),
    // Effect batches: beams and bolts, non-additive beams, CEG
    // particles and flames, smoke trails, spikes, ground flashes and
    // geovent puffs.
    variant(true, AlphaMode::Add, BACK, 0.0),
    variant(true, AlphaMode::Blend, BACK, 0.0),
    variant(true, AlphaMode::Premultiplied, None, 0.0),
    variant(true, AlphaMode::Premultiplied, BACK, 0.0),
    variant(true, AlphaMode::Add, None, 0.0),
];

/// Frames a warm-up entity stands: enough for its mesh and material
/// to reach the render world and its pipeline to be queued.
const WARMUP_FRAMES: u8 = 8;

/// A warm-up draw and the frames it has left.
#[derive(Component)]
struct PipelineWarmup(u8);

/// Warm what the match about to start would otherwise build on first
/// use: every seat's units in its colour, every shot's textures, and
/// the pipelines of [`VARIANTS`].
pub fn warm_match(players: &[PlayerSpec], ctx: &mut SpawnContext) {
    let t0 = bevy::platform::time::Instant::now();
    for (owner, player) in players.iter().enumerate() {
        let color = TeamColors::lookup(ctx.team_colors.as_deref(), owner as u8, player.faction);
        for &kind in UnitKind::VARIANTS
            .iter()
            .filter(|k| builds(player.faction, **k))
        {
            let model = ctx.unit_registry.model(kind);
            unit_material(
                kind,
                color,
                &mut ctx.materials,
                &mut ctx.images,
                &mut ctx.model_cache,
                model,
            );
            piece_layout(model, &mut ctx.meshes, &mut ctx.model_cache);
        }
    }
    crate::units::weapon_fx::warm_shot_textures(
        &ctx.weapon_registry,
        &mut ctx.model_cache,
        &mut ctx.images,
    );
    spawn_warmups(&mut ctx.commands, &mut ctx.meshes, &mut ctx.materials);
    debug!(
        "  warmed unit and shot assets in {:.0}ms",
        t0.elapsed().as_secs_f64() * 1000.0
    );
}

/// Whether a seat of `faction` builds or spawns `kind`: its own units,
/// and the Debugger and Bad Blocks every faction places.
fn builds(faction: Faction, kind: UnitKind) -> bool {
    kind.faction() == faction || matches!(kind, UnitKind::Debug | UnitKind::BadBlock)
}

/// One warm-up draw per [`VARIANTS`] entry: a sub-pixel cube with an
/// invisible colour, never culled, so the camera draws it.
fn spawn_warmups(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) {
    let plain = meshes.add(Cuboid::from_length(1.0));
    let colored = {
        let mut mesh = Mesh::from(Cuboid::from_length(1.0));
        let count = mesh.count_vertices();
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; count]);
        meshes.add(mesh)
    };
    for v in VARIANTS {
        let material = materials.add(StandardMaterial {
            base_color: Color::NONE,
            unlit: true,
            alpha_mode: v.alpha_mode,
            cull_mode: v.cull_mode,
            depth_bias: v.depth_bias,
            ..default()
        });
        let mesh = if v.vertex_colors { &colored } else { &plain };
        commands.spawn((
            PipelineWarmup(WARMUP_FRAMES),
            Mesh3d(mesh.clone()),
            MeshMaterial3d(material),
            Transform::from_scale(Vec3::splat(1e-4)),
            NoFrustumCulling,
        ));
    }
}

/// Count the warm-up draws down and despawn them.
fn retire_warmups(mut warmups: Query<(Entity, &mut PipelineWarmup)>, mut commands: Commands) {
    for (entity, mut warmup) in &mut warmups {
        warmup.0 = warmup.0.saturating_sub(1);
        if warmup.0 == 0 {
            commands.entity(entity).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    /// Every faction builds its homebase and the shared Debugger, and
    /// no other faction's units.
    #[test]
    fn a_seat_warms_its_own_roster() {
        assert!(builds(Faction::Network, UnitKind::Carrier));
        assert!(builds(Faction::Network, UnitKind::Debug));
        assert!(builds(Faction::Hacker, UnitKind::BadBlock));
        assert!(!builds(Faction::Network, UnitKind::Kernel));
        assert!(!builds(Faction::System, UnitKind::Hole));
    }

    /// The warm-up draws go after their frames.
    #[test]
    fn warmups_retire() {
        let mut app = App::new();
        app.init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .add_systems(Update, retire_warmups);
        app.world_mut()
            .run_system_once(
                |mut commands: Commands,
                 mut meshes: ResMut<Assets<Mesh>>,
                 mut materials: ResMut<Assets<StandardMaterial>>| {
                    spawn_warmups(&mut commands, &mut meshes, &mut materials);
                },
            )
            .unwrap();
        let count = |app: &mut App| {
            app.world_mut()
                .query::<&PipelineWarmup>()
                .iter(app.world())
                .count()
        };
        assert_eq!(count(&mut app), VARIANTS.len());
        for _ in 0..WARMUP_FRAMES {
            app.update();
        }
        assert_eq!(count(&mut app), 0);
    }
}
