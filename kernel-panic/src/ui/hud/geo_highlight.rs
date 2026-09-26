//! Kernel Panic's datavent highlight (`LuaUI/Widgets/kp_geoshighlight.lua`):
//! while a small building (minifac or special — the ones that need a
//! datavent) is being placed, every datavent gets a bright green 64×64
//! square drawn additively 2 elmos above the ground, blinking 0.4 s on /
//! 0.4 s off. Vents closer than 64 elmos to one already listed share its
//! square. (The widget's minimap blink is not ported.)

use bevy::prelude::*;

use crate::terrain::geovent::GeoventSmoker;

use super::placement::PlacementMode;

pub(super) struct GeoHighlightPlugin;

impl Plugin for GeoHighlightPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            update_geo_highlight.after(crate::map_loading::GameWorldRebuild),
        );
    }
}

#[derive(Component)]
struct GeoSquare;

/// Half the square's side (`DrawGroundHuggingSquare(…, 32, 2)`).
const HALF: f32 = 32.0;
const HOVER: f32 = 2.0;

/// `GeoSpots`: vent positions with near-duplicates (< 64 elmos) dropped.
fn geo_spots(vents: impl IntoIterator<Item = Vec3>) -> Vec<Vec3> {
    let mut spots: Vec<Vec3> = Vec::new();
    for v in vents {
        if !spots.iter().any(|s| s.xz().distance(v.xz()) < 64.0) {
            spots.push(v);
        }
    }
    spots
}

/// On for the first 0.4 s of every 0.8 s since the squares appeared.
fn blink_on(elapsed: f32) -> bool {
    elapsed.rem_euclid(0.8) < 0.4
}

#[allow(clippy::too_many_arguments)]
fn update_geo_highlight(
    mut commands: Commands,
    time: Res<Time>,
    placement: Res<PlacementMode>,
    vents: Query<&GeoventSmoker>,
    mut squares: Query<(Entity, &mut Visibility), With<GeoSquare>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut first_drawn: Local<Option<f32>>,
    mut assets: Local<Option<(Handle<Mesh>, Handle<StandardMaterial>)>>,
) {
    let placing = placement.kind.is_some_and(|k| k.is_small_building());
    if !placing {
        for (e, _) in &squares {
            commands.entity(e).despawn();
        }
        *first_drawn = None;
        return;
    }
    let now = time.elapsed_secs();
    if squares.is_empty() {
        let (mesh, mat) = assets
            .get_or_insert_with(|| {
                (
                    meshes.add(Plane3d::new(Vec3::Y, Vec2::splat(HALF))),
                    materials.add(StandardMaterial {
                        base_color: Color::srgb(0.3, 1.0, 0.4),
                        unlit: true,
                        alpha_mode: AlphaMode::Add,
                        ..default()
                    }),
                )
            })
            .clone();
        for spot in geo_spots(vents.iter().map(|v| v.pos)) {
            commands.spawn((
                GeoSquare,
                Mesh3d(mesh.clone()),
                MeshMaterial3d(mat.clone()),
                Transform::from_translation(spot + Vec3::Y * HOVER),
                Visibility::Inherited,
            ));
        }
        *first_drawn = Some(now);
        return;
    }
    let on = blink_on(now - first_drawn.unwrap_or(now));
    let want = if on {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for (_, mut vis) in &mut squares {
        if *vis != want {
            *vis = want;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_vents_share_a_square() {
        let spots = geo_spots([
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(40.0, 5.0, 0.0),
            Vec3::new(100.0, 0.0, 0.0),
        ]);
        assert_eq!(spots.len(), 2);
    }

    #[test]
    fn blinks_every_point_eight_seconds() {
        assert!(blink_on(0.1));
        assert!(!blink_on(0.5));
        assert!(blink_on(0.9));
    }
}
