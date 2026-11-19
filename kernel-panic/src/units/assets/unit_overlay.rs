//! Two gadgets draw a unit a second time over itself (`gl.Unit(u, true)`)
//! with a texture laid on through object-linear texgen — texture
//! coordinates taken from the piece's own vertex coordinates:
//!
//! - `network_reflectorshield.lua`: every unit a Firewall protects
//!   ([`Protected`]) glows green — `bitmaps/kpsfx/shieldEffect.png`,
//!   additive, `gl.Color(.1, 1, .1)`, T = `0.1·y + phase` with the phase
//!   moving `.03` a drawn frame, so the bands climb the unit.
//! - `network_build.lua`: a unit a Gateway or Carrier is building is
//!   covered by `unittextures/network_buildeffect.tga` (half-transparent
//!   grey up to a bright edge, clear past it), S = `z · .25/radius + .25
//!   − progress · .5`: the cover slides off from the front as the build
//!   goes on. Its colour is the team colour lightened toward white by a
//!   new random amount every frame (`1 − (1 − c)·o²`, `o` in 0‥.5).
//!
//! Each model piece gets a child sharing its mesh with a
//! [`UnitOverlayMaterial`]; the piece's animation, visibility (a hidden
//! piece, the fog) and despawn carry over to it. Rendering only: no sim
//! state is read back from these entities.

use bevy::asset::embedded_path;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::light::NotShadowCaster;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::picking::Pickable;
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;

use super::animation::PieceIndex;
use crate::sim::DetFloat;
use crate::units::components::{Faction, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::lifecycle::spawning::{BuildDriven, Emerging};
use crate::units::mechanics::command_fire::Protected;
use crate::units::team_colors::{TeamColor, UnitColor};

fn shader() -> ShaderRef {
    ShaderRef::Path(
        bevy::asset::AssetPath::from_path_buf(embedded_path!("unit_overlay.wesl"))
            .with_source("embedded"),
    )
}

/// A piece drawn again with a texgen'd texture (`unit_overlay.wesl`).
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct UnitOverlayMaterial {
    /// `gl.Color`, multiplying the texel.
    #[uniform(0)]
    pub color: LinearRgba,
    /// `GL_OBJECT_PLANE` of S: `s = dot(s_plane, (x, y, z, 1))`.
    #[uniform(1)]
    pub s_plane: Vec4,
    /// `GL_OBJECT_PLANE` of T.
    #[uniform(2)]
    pub t_plane: Vec4,
    #[texture(3)]
    #[sampler(4)]
    pub texture: Handle<Image>,
    /// x: the `gl.PolygonOffset` stand-in, the fraction of the depth the
    /// surface is moved toward the eye.
    #[uniform(5)]
    pub params: Vec4,
    pub alpha_mode: AlphaMode,
}

impl Material for UnitOverlayMaterial {
    fn vertex_shader() -> ShaderRef {
        shader()
    }

    fn fragment_shader() -> ShaderRef {
        shader()
    }

    fn alpha_mode(&self) -> AlphaMode {
        self.alpha_mode
    }

    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }

    /// The shader reads the positions only (`gl.Unit(u, true)`'s raw draw
    /// with texgen'd coordinates); back faces culled (`gl.Culling(GL.BACK)`).
    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        let vertex_layout = layout
            .0
            .get_layout(&[Mesh::ATTRIBUTE_POSITION.at_shader_location(0)])?;
        descriptor.vertex.buffers = vec![vertex_layout];
        Ok(())
    }
}

/// `network_reflectorshield.lua`'s `gl.Color(.1, 1, .1, 1)`.
const SHIELD_COLOR: [f32; 3] = [0.1, 1.0, 0.1];
/// `phase = phase + .03` every drawn frame; at 60 frames a second.
const SHIELD_PHASE_PER_SEC: f32 = 0.03 * 60.0;
/// `gl.PolygonOffset(-10, -10)` / `(-2, -2)`, as depth fractions.
const SHIELD_DEPTH_NUDGE: f32 = 0.002;
const BUILD_DEPTH_NUDGE: f32 = 0.0004;

/// The overlays' textures and the one shield material every protected
/// unit shares (its phase is global).
#[derive(Resource)]
pub struct UnitOverlayAssets {
    shield: Handle<UnitOverlayMaterial>,
    build_texture: Option<Handle<Image>>,
    phase: f32,
}

impl FromWorld for UnitOverlayAssets {
    fn from_world(world: &mut World) -> Self {
        let repeat = ImageSampler::Descriptor(ImageSamplerDescriptor {
            address_mode_u: ImageAddressMode::Repeat,
            address_mode_v: ImageAddressMode::Repeat,
            ..ImageSamplerDescriptor::linear()
        });
        let shield_image = Image::from_buffer(
            include_bytes!("../../../assets/bitmaps/kpsfx/shieldEffect.png"),
            bevy::image::ImageType::Extension("png"),
            bevy::image::CompressedImageFormats::NONE,
            true,
            repeat.clone(),
            bevy::asset::RenderAssetUsages::RENDER_WORLD,
        )
        .expect("shieldEffect.png decodes");
        let build_image = crate::units::content::bundle::bundle()
            .texture("network_buildeffect.tga")
            .map(|tga| {
                let mut image = Image::new(
                    bevy::render::render_resource::Extent3d {
                        width: tga.width,
                        height: tga.height,
                        depth_or_array_layers: 1,
                    },
                    bevy::render::render_resource::TextureDimension::D2,
                    tga.pixels,
                    bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
                    bevy::asset::RenderAssetUsages::RENDER_WORLD,
                );
                image.sampler = repeat;
                image
            });
        let mut images = world.resource_mut::<Assets<Image>>();
        let shield_texture = images.add(shield_image);
        let build_texture = build_image.map(|image| images.add(image));
        let shield = world
            .resource_mut::<Assets<UnitOverlayMaterial>>()
            .add(UnitOverlayMaterial {
                color: Color::srgb(SHIELD_COLOR[0], SHIELD_COLOR[1], SHIELD_COLOR[2]).into(),
                s_plane: Vec4::ZERO,
                t_plane: Vec4::new(0.0, 0.1, 0.0, 0.0),
                texture: shield_texture,
                params: Vec4::new(SHIELD_DEPTH_NUDGE, 0.0, 0.0, 0.0),
                // `gl.Blending(GL.SRC_ALPHA, GL.ONE)`.
                alpha_mode: AlphaMode::Add,
            });
        Self {
            shield,
            build_texture,
            phase: 0.0,
        }
    }
}

/// A unit a Gateway or Carrier is building (`network_build.lua`'s
/// `constructions`, filled in `UnitCreated` from the builder's kind):
/// `radius` is `Spring.GetUnitRadius`.
#[derive(Component, Debug, Clone, Copy)]
pub struct NetworkBuild {
    radius: f32,
}

/// The overlay children one effect has put on a unit's pieces.
#[derive(Component, Debug)]
pub struct ShieldOverlay(Vec<Entity>);

/// The build effect's overlay children and their (per-unit) material.
#[derive(Component, Debug)]
pub struct BuildOverlay {
    pieces: Vec<Entity>,
    material: Handle<UnitOverlayMaterial>,
}

/// Give every model piece of `unit` an overlay child in `material`.
/// Empty while the unit's pieces have not been spawned yet.
fn dress(
    unit: Entity,
    material: &Handle<UnitOverlayMaterial>,
    children: &Query<&Children>,
    pieces: &Query<&Mesh3d, With<PieceIndex>>,
    commands: &mut Commands,
) -> Vec<Entity> {
    children
        .iter_descendants(unit)
        .filter_map(|piece| {
            let mesh = pieces.get(piece).ok()?;
            Some(
                commands
                    .spawn((
                        Mesh3d(mesh.0.clone()),
                        MeshMaterial3d(material.clone()),
                        Transform::IDENTITY,
                        Visibility::Inherited,
                        NotShadowCaster,
                        Pickable::IGNORE,
                        ChildOf(piece),
                    ))
                    .id(),
            )
        })
        .collect()
}

fn undress(overlays: &[Entity], commands: &mut Commands) {
    for &overlay in overlays {
        commands.entity(overlay).try_despawn();
    }
}

/// `network_build.lua` `UnitCreated`: a new buildee whose builder is a
/// Gateway or Carrier (the factory or constructor that started it,
/// `BuildDriven::solo_builder`) gets the build effect until finished.
pub fn mark_network_builds(
    new: Query<(Entity, &UnitType, &BuildDriven), Added<BuildDriven>>,
    builders: Query<&UnitType>,
    registry: Res<UnitRegistry>,
    mut commands: Commands,
) {
    for (entity, kind, driven) in &new {
        let by_network = driven
            .solo_builder
            .and_then(|b| builders.get(b).ok())
            .is_some_and(|b| matches!(b.0, UnitKind::Gateway | UnitKind::Carrier));
        if by_network {
            commands.entity(entity).insert(NetworkBuild {
                radius: registry.model_radius(kind.0).det_max(1.0),
            });
        }
    }
}

/// Keep the Firewall's glow on exactly the protected units, and move
/// its bands.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn sync_shield_overlays(
    time: Res<Time<Real>>,
    mut assets: ResMut<UnitOverlayAssets>,
    mut materials: ResMut<Assets<UnitOverlayMaterial>>,
    bare: Query<Entity, (With<Protected>, Without<ShieldOverlay>)>,
    ended: Query<(Entity, &ShieldOverlay), Without<Protected>>,
    children: Query<&Children>,
    pieces: Query<&Mesh3d, With<PieceIndex>>,
    mut commands: Commands,
) {
    for entity in &bare {
        let dressed = dress(entity, &assets.shield, &children, &pieces, &mut commands);
        if !dressed.is_empty() {
            commands.entity(entity).insert(ShieldOverlay(dressed));
        }
    }
    for (entity, overlay) in &ended {
        undress(&overlay.0, &mut commands);
        commands.entity(entity).remove::<ShieldOverlay>();
    }
    assets.phase = (assets.phase + SHIELD_PHASE_PER_SEC * time.delta_secs()).rem_euclid(1.0);
    let phase = assets.phase;
    if let Some(mut shield) = materials.get_mut(&assets.shield) {
        shield.t_plane.w = phase;
    }
}

/// Keep the build effect on the Network buildees, sweep it with their
/// progress and flicker its colour; take it off once they are done.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn sync_build_overlays(
    assets: Res<UnitOverlayAssets>,
    mut materials: ResMut<Assets<UnitOverlayMaterial>>,
    bare: Query<(Entity, &NetworkBuild), (With<Emerging>, Without<BuildOverlay>)>,
    building: Query<(
        &NetworkBuild,
        &BuildOverlay,
        &Emerging,
        Option<&UnitColor>,
        &Faction,
    )>,
    done: Query<(Entity, Option<&BuildOverlay>), (With<NetworkBuild>, Without<Emerging>)>,
    children: Query<&Children>,
    pieces: Query<&Mesh3d, With<PieceIndex>>,
    mut flicker: Local<u32>,
    mut commands: Commands,
) {
    for (entity, overlay) in &done {
        if let Some(overlay) = overlay {
            undress(&overlay.pieces, &mut commands);
        }
        commands
            .entity(entity)
            .remove::<(NetworkBuild, BuildOverlay)>();
    }
    let Some(texture) = assets.build_texture.clone() else {
        return;
    };
    for (entity, build) in &bare {
        let material = materials.add(UnitOverlayMaterial {
            color: LinearRgba::WHITE,
            s_plane: Vec4::new(0.0, 0.0, 0.25 / build.radius, 0.25),
            // `gl.TexGen(GL.T, ..., 1, 1, 0, 0)`.
            t_plane: Vec4::new(1.0, 1.0, 0.0, 0.0),
            texture: texture.clone(),
            params: Vec4::new(BUILD_DEPTH_NUDGE, 0.0, 0.0, 0.0),
            // The draw state the gadget leaves blending in:
            // `SRC_ALPHA, ONE_MINUS_SRC_ALPHA`.
            alpha_mode: AlphaMode::Blend,
        });
        let dressed = dress(entity, &material, &children, &pieces, &mut commands);
        if dressed.is_empty() {
            materials.remove(&material);
        } else {
            commands.entity(entity).insert(BuildOverlay {
                pieces: dressed,
                material,
            });
        }
    }
    for (build, overlay, emerging, color, faction) in &building {
        let Some(mut material) = materials.get_mut(&overlay.material) else {
            continue;
        };
        let progress = if emerging.total > 0.0 {
            (1.0 - emerging.remaining / emerging.total).det_clamp(0.0, 1.0)
        } else {
            1.0
        };
        material.s_plane = Vec4::new(0.0, 0.0, 0.25 / build.radius, 0.25 - progress * 0.5);
        // `o = math.random(0, 500) / 1000`; `1 - (1 - c)·o²` per channel.
        *flicker = flicker.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let o = (*flicker >> 16) as f32 % 501.0 / 1000.0;
        // `Spring.GetTeamColor(Spring.GetUnitTeam(u))`.
        let team_color = color
            .map_or(TeamColor::of_faction(*faction), |c| c.0)
            .color()
            .to_srgba();
        let lift = |c: f32| 1.0 - (1.0 - c) * o * o;
        material.color = Color::srgb(
            lift(team_color.red),
            lift(team_color.green),
            lift(team_color.blue),
        )
        .into();
    }
}

/// Registers the overlay material, its shader and the two effects.
pub struct UnitOverlayPlugin;

impl Plugin for UnitOverlayPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "unit_overlay.wesl");
        app.add_plugins(MaterialPlugin::<UnitOverlayMaterial>::default())
            .init_resource::<UnitOverlayAssets>()
            .add_systems(
                Update,
                (
                    mark_network_builds,
                    sync_build_overlays,
                    sync_shield_overlays,
                )
                    .chain(),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    fn world() -> World {
        let mut world = World::new();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<Assets<UnitOverlayMaterial>>();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Time<Real>>();
        world.insert_resource(UnitRegistry::load());
        world.init_resource::<UnitOverlayAssets>();
        world
    }

    /// A unit with one model piece under a model root.
    fn unit(world: &mut World, kind: UnitKind) -> (Entity, Entity) {
        let mesh = world.resource_mut::<Assets<Mesh>>().add(Cuboid::default());
        let unit = world
            .spawn((UnitType(kind), Faction::Network, Transform::default()))
            .id();
        let root = world.spawn((Transform::default(), ChildOf(unit))).id();
        let piece = world
            .spawn((
                PieceIndex,
                Mesh3d(mesh),
                Transform::default(),
                ChildOf(root),
            ))
            .id();
        (unit, piece)
    }

    fn overlays(world: &World, piece: Entity) -> usize {
        world
            .get::<Children>(piece)
            .map_or(0, |children| children.len())
    }

    /// network_build.lua: a Gateway's buildee is covered while it is
    /// built, a Socket's (an Assembler's) is not, and the cover goes
    /// when the build is done.
    #[test]
    fn the_build_effect_covers_network_buildees_until_done() {
        let mut world = world();
        let (gateway, _) = unit(&mut world, UnitKind::Gateway);
        let (assembler, _) = unit(&mut world, UnitKind::Assembler);
        let buildee = |world: &mut World, builder| {
            let (unit, piece) = unit(world, UnitKind::Port);
            world.entity_mut(unit).insert((
                BuildDriven {
                    solo_builder: Some(builder),
                    ..BuildDriven::default()
                },
                Emerging {
                    target_y: 0.0,
                    remaining: 3.0,
                    total: 4.0,
                    rally_point: None,
                    rally_then: None,
                    style: crate::units::lifecycle::spawning::EmergeStyle::Rise,
                },
            ));
            (unit, piece)
        };
        let (network, network_piece) = buildee(&mut world, gateway);
        let (system, system_piece) = buildee(&mut world, assembler);
        world.run_system_once(mark_network_builds).unwrap();
        world.run_system_once(sync_build_overlays).unwrap();
        assert_eq!(overlays(&world, network_piece), 1);
        assert_eq!(overlays(&world, system_piece), 0);
        world.run_system_once(sync_build_overlays).unwrap();
        let material = world.get::<BuildOverlay>(network).unwrap().material.clone();
        let s = world
            .resource::<Assets<UnitOverlayMaterial>>()
            .get(&material)
            .unwrap()
            .s_plane;
        assert!((s.w - (0.25 - 0.25 * 0.5)).abs() < 1e-6, "a quarter built");
        assert!(world.get::<NetworkBuild>(system).is_none());

        world.entity_mut(network).remove::<Emerging>();
        world.run_system_once(sync_build_overlays).unwrap();
        assert_eq!(overlays(&world, network_piece), 0, "finished: uncovered");
        assert!(world.get::<NetworkBuild>(network).is_none());
    }

    /// network_reflectorshield.lua: exactly the protected units glow.
    #[test]
    fn the_shield_glow_follows_the_protection() {
        let mut world = world();
        let (unit, piece) = unit(&mut world, UnitKind::Bit);
        world.run_system_once(sync_shield_overlays).unwrap();
        assert_eq!(overlays(&world, piece), 0);
        world.entity_mut(unit).insert(Protected { remaining: 1.0 });
        world.run_system_once(sync_shield_overlays).unwrap();
        assert_eq!(overlays(&world, piece), 1);
        world.entity_mut(unit).remove::<Protected>();
        world.run_system_once(sync_shield_overlays).unwrap();
        assert_eq!(overlays(&world, piece), 0);
    }
}
