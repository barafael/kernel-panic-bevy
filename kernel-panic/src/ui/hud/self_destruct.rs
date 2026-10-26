//! The self-destruct countdown (`Ctrl+D`) over every unit of the
//! player's side that counts down: the seconds left, rounded up, as
//! Spring logs "self-destruct in Ns" for the player's own units
//! (`CUnit::SlowUpdate`, Unit.cpp:1026). A spectator sees everyone's.

use bevy::picking::Pickable;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;

use crate::game_setup::AppState;
use crate::rendering::camera::RtsCamera;
use crate::sim::DetFloat;
use crate::units::combat::{CollisionVolume, Dying, SelfDestructCountdown};
use crate::units::components::TeamId;
use crate::units::player::LocalTeam;
use crate::units::teams::Alliances;

/// The label over `unit`.
#[derive(Component)]
struct Countdown {
    unit: Entity,
}

/// A unit without a [`CollisionVolume`]: its sphere's height and radius
/// (elmos).
const FALLBACK_RADIUS: f32 = 20.0;
/// The label's width; the number centres in it.
const WIDTH: f32 = 64.0;
const FONT_SIZE: f32 = 28.0;
const COLOR: Color = Color::srgb(1.0, 0.35, 0.25);

pub(super) struct SelfDestructPlugin;

impl Plugin for SelfDestructPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, show_countdowns.run_if(in_state(AppState::InGame)));
    }
}

/// Whole seconds left, rounded up: what Spring's log says on the slow
/// updates it logs.
fn seconds_left(remaining: f32) -> u32 {
    remaining.ceil().det_max(1.0) as u32
}

/// The countdowns this player sees: their side's, everyone's for a
/// spectator.
fn shown(local: LocalTeam, alliances: Option<&Alliances>, team: u8) -> bool {
    local
        .seat()
        .is_none_or(|seat| alliances.map_or(seat == team, |a| a.friendly(seat, team)))
}

#[allow(clippy::type_complexity)]
fn show_countdowns(
    mut commands: Commands,
    local: Option<Res<LocalTeam>>,
    alliances: Option<Res<Alliances>>,
    camera: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    units: Query<
        (
            Entity,
            &SelfDestructCountdown,
            &TeamId,
            &GlobalTransform,
            Option<&CollisionVolume>,
            Option<&Visibility>,
        ),
        (Without<Dying>, Without<Countdown>),
    >,
    mut labels: Query<(Entity, &Countdown, &mut Text, &mut Node, &mut Visibility)>,
) {
    let local = local.map_or(LocalTeam(crate::game_setup::SPECTATOR_TEAM), |l| *l);
    let camera = camera.single().ok();
    // Screen up, in the world: the top of a unit's sphere as seen.
    let up = camera.map_or(Vec3::Y, |(_, tf)| *tf.up());
    let mut due: HashMap<Entity, (Vec3, u32)> = units
        .iter()
        .filter(|(_, _, team, _, _, vis)| {
            shown(local, alliances.as_deref(), team.0)
                && vis.is_none_or(|v| *v != Visibility::Hidden)
        })
        .map(|(e, countdown, _, tf, volume, _)| {
            let (centre, radius) = volume.map_or(
                (
                    tf.translation() + Vec3::Y * FALLBACK_RADIUS,
                    FALLBACK_RADIUS,
                ),
                |v| (v.center(tf), v.radius),
            );
            (e, (centre + up * radius, seconds_left(countdown.remaining)))
        })
        .collect();
    for (label, countdown, mut text, mut node, mut vis) in &mut labels {
        let Some((at, seconds)) = due.remove(&countdown.unit) else {
            commands.entity(label).despawn();
            continue;
        };
        let number = seconds.to_string();
        if text.0 != number {
            text.0 = number;
        }
        match camera.and_then(|(cam, tf)| cam.world_to_viewport(tf, at).ok()) {
            // The number stands on the sphere's top.
            Some(p) => {
                let (left, top) = (Val::Px(p.x - WIDTH / 2.0), Val::Px(p.y - FONT_SIZE * 1.2));
                if node.left != left || node.top != top {
                    node.left = left;
                    node.top = top;
                }
                vis.set_if_neq(Visibility::Inherited);
            }
            None => {
                vis.set_if_neq(Visibility::Hidden);
            }
        }
    }
    // New ones show from the next frame, placed.
    for (unit, (_, seconds)) in due {
        commands.spawn((
            Countdown { unit },
            Text::new(seconds.to_string()),
            TextFont::from_font_size(FONT_SIZE),
            TextColor(COLOR),
            TextShadow {
                offset: Vec2::splat(2.0),
                color: Color::BLACK,
            },
            // Across the node's width, so `Center` centres it there.
            TextLayout::new(Justify::Center, LineBreak::WordBoundary),
            Node {
                position_type: PositionType::Absolute,
                width: Val::Px(WIDTH),
                ..default()
            },
            // Under every HUD panel.
            GlobalZIndex(-3),
            Visibility::Hidden,
            Pickable::IGNORE,
            DespawnOnExit(AppState::InGame),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    /// Rounded up, never 0: a Bit's 1.5 s reads 2, then 1.
    #[test]
    fn the_seconds_round_up() {
        assert_eq!(seconds_left(5.5), 6);
        assert_eq!(seconds_left(5.0), 5);
        assert_eq!(seconds_left(1.5), 2);
        assert_eq!(seconds_left(0.2), 1);
        assert_eq!(seconds_left(0.0), 1);
    }

    /// One label per unit of the player's side counting down; it goes
    /// once the countdown is called off.
    #[test]
    fn a_label_follows_each_countdown_of_the_side() {
        let mut world = World::new();
        world.insert_resource(LocalTeam(0));
        world.insert_resource(Alliances::from_seats([(0, 0), (1, 0), (2, 1)]));
        let fuse = || SelfDestructCountdown { remaining: 3.2 };
        let at = GlobalTransform::default();
        let mine = world.spawn((fuse(), TeamId(0), at)).id();
        let ally = world.spawn((fuse(), TeamId(1), at)).id();
        world.spawn((fuse(), TeamId(2), at));
        world.spawn((TeamId(0), at));
        world.run_system_once(show_countdowns).unwrap();
        let labels = |world: &mut World| {
            let mut q = world.query::<(&Countdown, &Text)>();
            let mut v: Vec<(Entity, String)> =
                q.iter(world).map(|(c, t)| (c.unit, t.0.clone())).collect();
            v.sort();
            v
        };
        let mut want = vec![(mine, "4".to_string()), (ally, "4".to_string())];
        want.sort();
        assert_eq!(labels(&mut world), want);
        world.entity_mut(mine).remove::<SelfDestructCountdown>();
        world.run_system_once(show_countdowns).unwrap();
        assert_eq!(labels(&mut world), vec![(ally, "4".to_string())]);
    }
}
