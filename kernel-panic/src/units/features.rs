//! Map features: the bad blocks of Quad Core, Dual Core, Corrupted Core
//! and DigitalDivide, and Central Hub's unusable sectors — what a map's
//! SMF feature list places besides its datavents
//! (`CFeatureHandler::LoadFeaturesFromMap`).
//!
//! A feature belongs to no team ([`FEATURE_TEAM`]) and is not a unit:
//! nothing auto-targets or selects it, and the unit caps and the
//! game-over census never see it. It blocks its footprint
//! ([`crate::interaction::structures`]), a mover with enough
//! `crushStrength` drives over it and crushes it, and shots and
//! explosions hit it like a unit (a [`SpatialIndex`] entry flagged
//! `feature`; features have no armour class, so they take a weapon's
//! default damage, `CFeature::DoDamage`). Destroyed — at 0 HP or crushed
//! — it leaves its `featureDead` (`CreateWreck`): an unusable sector
//! comes down a layer at a time.
//!
//! [`SpatialIndex`]: crate::units::spatial::SpatialIndex

use bevy::prelude::*;
use spring_map::map_types::MapFeature;
use spring_tdf::FeatureDef;

use crate::net::NetId;
use crate::terrain::heightmap::Heightmap;
use crate::units::assets::meshes::{
    S3OModelCache, bundle_texture, feature_material, load_s3o_mesh, model_texture_name,
    unit_material,
};
use crate::units::combat::CollisionVolume;
use crate::units::combat::Dying;
use crate::units::components::Health;
use crate::units::components::{Faction, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::model::TgaImage;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::lifecycle::spawning::Emerging;

/// The owner of every map feature: no seat's, so friendly to no one
/// (`units::teams::Alliances`), like Spring's features of team -1.
pub const FEATURE_TEAM: u8 = 255;

/// A feature of `def` (`badblock`, `unusablesector4` …). It has no
/// [`TeamId`](crate::units::components::TeamId), so no team query meets
/// it; where an owner is needed it is [`FEATURE_TEAM`].
#[derive(Component, Clone, Copy)]
pub struct Feature {
    pub def: &'static FeatureDef,
    /// The faction whose colour it wears: a built Bad Block keeps its
    /// builder's (`CreateWreckage` passes the unit's team, which only
    /// the drawing reads); a map feature has none.
    pub owner: Option<Faction>,
}

impl Feature {
    /// The collision sphere: the model's (both shipped feature models
    /// are `badblock.s3o`'s box, radius 20 about a midpoint 8 up).
    pub const HIT_RADIUS: f32 = 20.0;
    pub const MID_Y: f32 = 8.0;

    /// `CalcFootPrintMaxInteriorRadius` of the footprint, the radius
    /// movers collide with (as for a structure).
    pub fn collision_radius(&self) -> f32 {
        self.def.footprint_x.max(self.def.footprint_z) as f32 * 8.0
    }
}

/// A mover crushed the feature this tick (`CFeature::Kill(…, crushed)`,
/// `HandleFeatureCollisions`).
#[derive(Component)]
pub struct Crushed;

/// Central Hub ships `unusablesector{1..4}.s3o` in its map archive:
/// the Bad Block's box (the same vertices, pieces and UVs) with a
/// texture of its own per stage, `Usector{N}tex1.png`, committed in
/// `assets/features/`.
const MAP_FEATURE_MODELS: [(&str, &str, &[u8]); 4] = [
    (
        "unusablesector1.s3o",
        "usector1tex1.png",
        include_bytes!("../../assets/features/usector1tex1.png"),
    ),
    (
        "unusablesector2.s3o",
        "usector2tex1.png",
        include_bytes!("../../assets/features/usector2tex1.png"),
    ),
    (
        "unusablesector3.s3o",
        "usector3tex1.png",
        include_bytes!("../../assets/features/usector3tex1.png"),
    ),
    (
        "unusablesector4.s3o",
        "usector4tex1.png",
        include_bytes!("../../assets/features/usector4tex1.png"),
    ),
];

/// The geometry `badblock.s3o` lends the map's own feature models.
const MAP_FEATURE_GEOMETRY: &str = "badblock.s3o";

/// What [`spawn_feature`] draws with.
pub struct FeatureDraw<'a> {
    pub meshes: &'a mut Assets<Mesh>,
    pub materials: &'a mut Assets<StandardMaterial>,
    pub images: &'a mut Assets<Image>,
    pub model_cache: &'a mut S3OModelCache,
}

/// [`FeatureDraw`] as system parameters.
#[derive(bevy::ecs::system::SystemParam)]
pub struct FeatureAssets<'w> {
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    images: ResMut<'w, Assets<Image>>,
    model_cache: ResMut<'w, S3OModelCache>,
}

impl FeatureAssets<'_> {
    pub fn draw(&mut self) -> FeatureDraw<'_> {
        FeatureDraw {
            meshes: &mut self.meshes,
            materials: &mut self.materials,
            images: &mut self.images,
            model_cache: &mut self.model_cache,
        }
    }
}

/// Spawn every feature of the map's list that has a definition (the
/// datavents are the terrain's), in list order, standing on the ground
/// (`GetHeightReal`; the stored height is ignored). Returns how many.
pub fn spawn_map_features(
    features: &[MapFeature],
    heightmap: &Heightmap,
    commands: &mut Commands,
    draw: &mut FeatureDraw,
) -> usize {
    let defs = &crate::units::content::defs::defs().features;
    let mut spawned = 0;
    for feature in features {
        if feature.feature_type.is_geovent() {
            continue;
        }
        let name = feature.feature_type.to_string();
        let Some(def) = defs.get(&name) else {
            warn!("map feature without a definition: {name}");
            continue;
        };
        let pos = heightmap.place(feature.x, feature.z);
        spawn_feature(def, pos, None, commands, draw);
        spawned += 1;
    }
    spawned
}

/// One feature of `def` standing at `pos`, in `owner`'s colours.
pub fn spawn_feature(
    def: &'static FeatureDef,
    pos: Vec3,
    owner: Option<Faction>,
    commands: &mut Commands,
    draw: &mut FeatureDraw,
) -> Entity {
    let FeatureDraw {
        meshes,
        materials,
        images,
        model_cache,
    } = draw;
    let feature = Feature { def, owner };
    let entity = commands
        .spawn((
            feature,
            NetId::default(),
            Health::full(def.health),
            CollisionVolume::from_s3o(Feature::HIT_RADIUS, Feature::MID_Y),
            // S3O models face +Z, Bevy's forward is -Z (see
            // `spawn_unit_facing`); every map feature has heading 0.
            Transform::from_translation(pos)
                .with_rotation(Quat::from_rotation_y(std::f32::consts::PI)),
            Visibility::default(),
        ))
        .id();
    let object = def.object.to_ascii_lowercase();
    let look = match MAP_FEATURE_MODELS
        .iter()
        .find(|(model, ..)| *model == object)
    {
        Some(&(_, texture, png)) => Some((MAP_FEATURE_GEOMETRY, texture.to_string(), Some(png))),
        None => model_texture_name(&object, model_cache).map(|tex| (object.as_str(), tex, None)),
    };
    let Some((geometry, texture, png)) = look else {
        warn!("feature {} has no model {object}", def.name);
        return entity;
    };
    let Some(mesh) = load_s3o_mesh(geometry, meshes, model_cache) else {
        return entity;
    };
    let material = match (owner, png) {
        // Its model's own texture in its owner's colours, as the unit was.
        (Some(faction), None) => unit_material(
            UnitKind::BadBlock,
            faction,
            materials,
            images,
            model_cache,
            geometry,
        ),
        _ => feature_material(
            &texture,
            |cache| match png {
                Some(bytes) => decode_png(bytes),
                None => bundle_texture(&texture, cache),
            },
            materials,
            images,
            model_cache,
        ),
    };
    commands
        .entity(entity)
        .insert((Mesh3d(mesh), MeshMaterial3d(material)));
    entity
}

/// A committed PNG as an RGBA texture.
fn decode_png(bytes: &[u8]) -> Option<TgaImage> {
    let image = Image::from_buffer(
        bytes,
        bevy::image::ImageType::Extension("png"),
        bevy::image::CompressedImageFormats::NONE,
        true,
        bevy::image::ImageSampler::Default,
        bevy::asset::RenderAssetUsages::MAIN_WORLD,
    )
    .ok()?;
    let rgba = image.try_into_dynamic().ok()?.to_rgba8();
    Some(TgaImage {
        width: rgba.width(),
        height: rgba.height(),
        pixels: rgba.into_raw(),
    })
}

/// `CFeature::DoDamage` at 0 HP, or a crush ([`Crushed`]: `Kill` deals
/// `health + 1`): the feature is gone and its `featureDead` takes its
/// place, at full health. In [`NetId`] order, so the wrecks' ids match
/// on every peer.
#[allow(clippy::type_complexity)]
pub fn destroy_features(
    features: Query<(Entity, &Feature, &Health, &Transform, &NetId, Has<Crushed>)>,
    mut assets: FeatureAssets,
    mut commands: Commands,
) {
    let defs = &crate::units::content::defs::defs().features;
    let mut gone: Vec<_> = features
        .iter()
        .filter(|(_, f, health, _, _, crushed)| {
            *crushed || (f.def.destructable && health.current <= 0.0)
        })
        .map(|(entity, f, _, tf, id, _)| (*id, entity, *f, tf.translation))
        .collect();
    gone.sort_unstable_by_key(|(id, ..)| *id);
    for (_, entity, feature, pos) in gone {
        commands.entity(entity).despawn();
        if let Some(wreck) = (!feature.def.feature_dead.is_empty())
            .then(|| defs.get(&feature.def.feature_dead))
            .flatten()
        {
            spawn_feature(wreck, pos, feature.owner, &mut commands, &mut assets.draw());
        }
    }
}

/// `CUnit::FinishedBuilding` of an `IsFeature=1` unit, the Bad Block:
/// the finished unit is gone without a death blast (`KillUnit(…,
/// reclaimed)`), and its `Corpse` feature stands in its place in its
/// owner's colours. The engine runs it for a unit created complete too
/// (`PostInit`), so a map event's Bad Blocks turn the same. In
/// [`NetId`] order.
#[allow(clippy::type_complexity)]
pub fn turn_into_features(
    units: Query<
        (Entity, &UnitType, &Faction, &Transform, &NetId),
        (Without<Emerging>, Without<Dying>),
    >,
    registry: Res<UnitRegistry>,
    mut assets: FeatureAssets,
    mut commands: Commands,
) {
    let defs = &crate::units::content::defs::defs().features;
    let mut finished: Vec<_> = units
        .iter()
        .filter_map(|(entity, kind, faction, tf, id)| {
            let def = defs.get(registry.finished_feature(kind.0)?)?;
            Some((*id, entity, def, *faction, tf.translation))
        })
        .collect();
    finished.sort_unstable_by_key(|(id, ..)| *id);
    for (_, entity, def, faction, pos) in finished {
        commands.entity(entity).despawn();
        spawn_feature(def, pos, Some(faction), &mut commands, &mut assets.draw());
    }
}

#[cfg(test)]
mod tests {
    use bevy::ecs::system::RunSystemOnce;

    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<Assets<Image>>()
            .init_resource::<S3OModelCache>();
        app
    }

    fn spawn(app: &mut App, name: &str, pos: Vec3) -> Entity {
        let def = crate::units::content::defs::defs()
            .features
            .get(name)
            .unwrap();
        app.world_mut()
            .run_system_once(move |mut commands: Commands, mut assets: FeatureAssets| {
                spawn_feature(def, pos, None, &mut commands, &mut assets.draw())
            })
            .unwrap()
    }

    fn features(app: &mut App) -> Vec<(&'static str, Vec3, f32)> {
        app.world_mut()
            .query::<(&Feature, &Transform, &Health)>()
            .iter(app.world())
            .map(|(f, tf, h)| (f.def.name.as_str(), tf.translation, h.current))
            .collect()
    }

    /// A destroyed unusable sector leaves the next stage in its place at
    /// full health, down to the last, which leaves nothing; so does a
    /// Bad Block. Damage short of 0 leaves a feature be.
    #[test]
    fn destroyed_features_leave_their_feature_dead() {
        let mut app = app();
        let at = Vec3::new(100.0, 5.0, 200.0);
        let sector = spawn(&mut app, "unusablesector4", at);
        let block = spawn(&mut app, "badblock", Vec3::new(300.0, 5.0, 200.0));
        app.world_mut().get_mut::<Health>(block).unwrap().current = 1.0;
        app.world_mut().get_mut::<Health>(sector).unwrap().current = 0.0;
        app.world_mut().run_system_once(destroy_features).unwrap();
        let mut now = features(&mut app);
        now.sort_by(|a, b| a.0.cmp(b.0));
        assert_eq!(
            now,
            vec![
                ("badblock", Vec3::new(300.0, 5.0, 200.0), 1.0),
                ("unusablesector3", at, 400.0)
            ]
        );

        for next in ["unusablesector2", "unusablesector1"] {
            for mut health in app
                .world_mut()
                .query::<&mut Health>()
                .iter_mut(app.world_mut())
            {
                health.current = 0.0;
            }
            app.world_mut().run_system_once(destroy_features).unwrap();
            assert_eq!(features(&mut app), vec![(next, at, 400.0)]);
        }
        for mut health in app
            .world_mut()
            .query::<&mut Health>()
            .iter_mut(app.world_mut())
        {
            health.current = 0.0;
        }
        app.world_mut().run_system_once(destroy_features).unwrap();
        assert!(features(&mut app).is_empty());
    }

    /// A crush (`Kill`) destroys a feature whatever its health.
    #[test]
    fn a_crushed_feature_is_destroyed() {
        let mut app = app();
        let block = spawn(&mut app, "badblock", Vec3::ZERO);
        app.world_mut().entity_mut(block).insert(Crushed);
        app.world_mut().run_system_once(destroy_features).unwrap();
        assert!(features(&mut app).is_empty());
    }

    /// A finished Bad Block unit (built, or spawned complete) turns into
    /// a `badblock` feature where it stood, in its builder's colours;
    /// one still under construction stays a unit.
    #[test]
    fn a_finished_bad_block_turns_into_its_feature() {
        let mut app = app();
        app.insert_resource(UnitRegistry::load());
        let at = Vec3::new(40.0, 3.0, 80.0);
        let built = app
            .world_mut()
            .spawn((
                UnitType(UnitKind::BadBlock),
                Faction::Hacker,
                Transform::from_translation(at),
                NetId::default(),
            ))
            .id();
        let framed = app
            .world_mut()
            .spawn((
                UnitType(UnitKind::BadBlock),
                Faction::System,
                Transform::default(),
                NetId::default(),
                Emerging {
                    target_y: 0.0,
                    remaining: 1.0,
                    total: 2.0,
                    rally_point: None,
                    rally_then: None,
                    style: crate::units::lifecycle::spawning::EmergeStyle::Rise,
                },
            ))
            .id();
        // Any other finished unit stays one.
        let socket = app
            .world_mut()
            .spawn((
                UnitType(UnitKind::Socket),
                Faction::System,
                Transform::default(),
                NetId::default(),
            ))
            .id();
        app.world_mut().run_system_once(turn_into_features).unwrap();
        assert!(app.world().get_entity(built).is_err());
        assert!(app.world().get_entity(framed).is_ok());
        assert!(app.world().get_entity(socket).is_ok());
        let turned: Vec<_> = app
            .world_mut()
            .query::<(&Feature, &Transform, &Health)>()
            .iter(app.world())
            .map(|(f, tf, h)| (f.def.name.as_str(), f.owner, tf.translation, h.current))
            .collect();
        assert_eq!(turned, vec![("badblock", Some(Faction::Hacker), at, 400.0)]);
    }
}
