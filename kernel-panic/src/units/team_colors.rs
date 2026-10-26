//! Owner colours: Spring's `TeamColors`, one per seat.
//!
//! A unit is drawn in its owner's colour, not its faction's. The first
//! seat of each faction wears the faction's own colour
//! ([`Faction::color`]); every further seat of that faction takes the
//! next colour of [`ALTERNATES`] that no seat wears yet, in seat order.
//! A Network facing a Network then still tells its units from the
//! enemy's. The colours follow from the seats alone, so every peer and
//! every replay of a match paints the same.
//!
//! Presentation only: nothing the sim decides reads a colour. A unit
//! taken by another owner turns to its colour
//! ([`recolor_taken_units`]).

use bevy::prelude::*;

use crate::game_setup::GameSetup;
use crate::interaction::selection::highlight::{Highlighted, OriginalMaterial};

use super::assets::animation::PieceIndex;
use super::assets::meshes::{S3OModelCache, unit_material};
use super::assets::model_fade::{FadeMaterialCache, ModelRoot};
use super::components::{Faction, TeamId, UnitType};
use super::content::unit_registry::UnitRegistry;
use super::lifecycle::spawning::FadeMaterials;
use super::mechanics::cloak::CloakFadeMaterials;

/// Colours (linear RGB) for the seats whose faction's own colour is
/// already worn, picked to stand apart from the three faction colours
/// and from each other, most distinct first.
const ALTERNATES: [[f32; 3]; 7] = [
    [1.0, 0.85, 0.0], // yellow
    [1.0, 0.0, 1.0],  // magenta
    [0.0, 0.9, 0.9],  // cyan
    [0.9, 0.9, 0.9],  // white
    [1.0, 0.3, 0.0],  // orange
    [0.4, 0.1, 1.0],  // violet
    [1.0, 0.35, 0.6], // pink
];

/// One owner colour: a faction's own ([`Self::of_faction`]) or an
/// alternate. Small and hashable, so material caches key on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TeamColor(u8);

impl TeamColor {
    const COUNT: u8 = Faction::ALL.len() as u8 + ALTERNATES.len() as u8;

    /// `faction`'s own colour.
    pub fn of_faction(faction: Faction) -> Self {
        Self(faction.index())
    }

    pub fn color(self) -> Color {
        let i = usize::from(self.0);
        match i
            .checked_sub(Faction::ALL.len())
            .and_then(|alt| ALTERNATES.get(alt))
        {
            Some(&[r, g, b]) => Color::linear_rgb(r, g, b),
            None => Faction::ALL[i % Faction::ALL.len()].color(),
        }
    }

    /// Linear `[r, g, b]`, as explosion events carry it.
    pub fn rgb_f32(self) -> [f32; 3] {
        let c = LinearRgba::from(self.color());
        [c.red, c.green, c.blue]
    }
}

/// The match's owner colours, from its seats. Set at every match swap
/// (`begin_match`) next to [`Alliances`](super::teams::Alliances). The
/// default (no seats) gives every owner its units' faction colour.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct TeamColors {
    /// Per seat, in seat (= owner) order.
    seats: Vec<TeamColor>,
}

impl TeamColors {
    pub fn from_setup(setup: &GameSetup) -> Self {
        Self::from_factions(setup.players.iter().map(|p| p.faction))
    }

    /// The colours of seats of `factions`, in seat order.
    pub fn from_factions(factions: impl IntoIterator<Item = Faction>) -> Self {
        let factions: Vec<Faction> = factions.into_iter().collect();
        let mut seats: Vec<Option<TeamColor>> = vec![None; factions.len()];
        // Each faction's colour goes to its first seat.
        for faction in Faction::ALL {
            if let Some(seat) = factions.iter().position(|f| *f == faction) {
                seats[seat] = Some(TeamColor::of_faction(faction));
            }
        }
        // The rest take the alternates in seat order. Past the last one
        // (more seats than colours) they wrap around and share.
        let mut next = (Faction::ALL.len() as u8..TeamColor::COUNT).cycle();
        let seats = seats
            .into_iter()
            .map(|c| c.unwrap_or_else(|| TeamColor(next.next().unwrap_or(0))))
            .collect();
        Self { seats }
    }

    /// The colour of `owner`'s units of `faction`: its seat's, or, for
    /// an owner without a seat (map-event neutrals), the faction's own.
    pub fn of(&self, owner: u8, faction: Faction) -> TeamColor {
        self.seats
            .get(usize::from(owner))
            .copied()
            .unwrap_or(TeamColor::of_faction(faction))
    }

    /// [`Self::of`], reading a world that may have no colours (bare
    /// test worlds): then every unit wears its faction's colour.
    pub fn lookup(colors: Option<&Self>, owner: u8, faction: Faction) -> TeamColor {
        colors.map_or(TeamColor::of_faction(faction), |c| c.of(owner, faction))
    }
}

/// The colour a unit is drawn in: its owner's when it spawned, updated
/// in place when another owner takes it (`recolor_taken_units`). Every
/// unit carries one from its spawn, so it never moves the unit to
/// another archetype.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Component)]
pub struct UnitColor(pub TeamColor);

/// A unit another owner took (`SimCommand::Take`) turns to its new
/// owner's colour, as Spring draws a unit in its current team's
/// (`CUnit::ChangeTeam`). Every model node wearing the old shared
/// material gets the new one, and what an emerge or cloak fade or a
/// highlight puts back afterwards is the new one too; a fade variant
/// turns into the new material's at the same step. Compares colours
/// rather than watching `Changed<TeamId>`, so a unit taken mid-fade
/// isn't missed.
/// Writes components in place, so no unit changes archetype.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn recolor_taken_units(
    colors: Res<TeamColors>,
    registry: Res<UnitRegistry>,
    mut units: Query<(
        Entity,
        &TeamId,
        &Faction,
        &UnitType,
        &mut UnitColor,
        Option<&mut FadeMaterials>,
        Option<&mut CloakFadeMaterials>,
    )>,
    nodes: Query<&Children, Or<(With<UnitType>, With<PieceIndex>, With<ModelRoot>)>>,
    mut worn: Query<(
        &mut MeshMaterial3d<StandardMaterial>,
        Option<&mut OriginalMaterial>,
    )>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut cache: ResMut<S3OModelCache>,
    mut fades: ResMut<FadeMaterialCache>,
    mut commands: Commands,
) {
    for (entity, team, faction, kind, mut color, fade, cloak) in &mut units {
        let want = colors.of(team.0, *faction);
        if color.0 == want {
            continue;
        }
        let model = registry.model(kind.0);
        let mut material =
            |c: TeamColor| unit_material(kind.0, c, &mut materials, &mut images, &mut cache, model);
        let (old, new) = (material(color.0), material(want));
        color.0 = want;
        // A fade's variant of the old material turns into the same
        // step's variant of the new one.
        let mut repaint = |h: &mut Handle<StandardMaterial>| {
            if *h == old {
                *h = new.clone();
            } else if let Some((source, step)) = fades.source_of(h)
                && source == old
                && let Some(variant) = fades.variant(&new, step, &mut materials)
            {
                *h = variant;
            }
        };
        let swapped = fade
            .into_iter()
            .flat_map(|f| f.into_inner().overrides.iter_mut())
            .chain(
                cloak
                    .into_iter()
                    .flat_map(|c| c.into_inner().overrides.iter_mut()),
            );
        for (_, original) in swapped {
            if *original == old {
                *original = new.clone();
            }
        }
        let mut highlighted = false;
        let mut stack = vec![entity];
        while let Some(node) = stack.pop() {
            if let Ok(children) = nodes.get(node) {
                stack.extend(children.iter());
            }
            let Ok((mut wearing, original)) = worn.get_mut(node) else {
                continue;
            };
            match original {
                // A highlight's shared bright variant stays as it is:
                // the piece wears what it restores to, and the
                // highlight brightens that again.
                Some(mut original) => {
                    repaint(&mut original.0);
                    wearing.0 = original.0.clone();
                    highlighted = true;
                }
                None => repaint(&mut wearing.0),
            }
        }
        if highlighted {
            commands.entity(entity).try_remove::<Highlighted>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use Faction::{Hacker, Network, System};

    #[test]
    fn first_seat_of_a_faction_keeps_its_colour() {
        let c = TeamColors::from_factions([System, Hacker]);
        assert_eq!(c.of(0, System), TeamColor::of_faction(System));
        assert_eq!(c.of(1, Hacker), TeamColor::of_faction(Hacker));
    }

    #[test]
    fn mirror_seats_get_distinct_colours() {
        let c = TeamColors::from_factions([Network, Network]);
        assert_eq!(c.of(0, Network), TeamColor::of_faction(Network));
        assert_ne!(c.of(1, Network), c.of(0, Network));
        assert_ne!(c.of(1, Network).color(), Network.color());
    }

    #[test]
    fn eight_seats_wear_eight_colours() {
        let c = TeamColors::from_factions([
            Network, System, Network, Hacker, Network, Network, System, Hacker,
        ]);
        let mut seen: Vec<TeamColor> = (0..8).map(|o| c.of(o, System)).collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 8);
        // Seat 1 is the first System, seat 3 the first Hacker.
        assert_eq!(c.of(1, System), TeamColor::of_faction(System));
        assert_eq!(c.of(3, Hacker), TeamColor::of_faction(Hacker));
    }

    #[test]
    fn unseated_owners_wear_their_faction() {
        let c = TeamColors::from_factions([Network, Network]);
        assert_eq!(c.of(200, Hacker), TeamColor::of_faction(Hacker));
        assert_eq!(
            TeamColors::lookup(None, 1, Network),
            TeamColor::of_faction(Network)
        );
    }

    /// A Network unit taken by the second Network seat turns from the
    /// faction's blue to that seat's colour, and so does what its
    /// highlight restores.
    #[test]
    fn a_taken_unit_wears_its_new_owner_colour() {
        use bevy::ecs::system::RunSystemOnce;

        use crate::units::content::definitions::UnitKind;

        let mut world = World::new();
        world.init_resource::<Assets<StandardMaterial>>();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<S3OModelCache>();
        world.init_resource::<FadeMaterialCache>();
        world.insert_resource(UnitRegistry::load());
        let colors = TeamColors::from_factions([Network, Network]);
        world.insert_resource(colors.clone());
        let kind = UnitKind::Packet;
        let blue = TeamColor::of_faction(Network);
        let material = |world: &mut World, c: TeamColor| {
            world
                .run_system_once(
                    move |mut m: ResMut<Assets<StandardMaterial>>,
                          mut i: ResMut<Assets<Image>>,
                          mut cache: ResMut<S3OModelCache>,
                          r: Res<UnitRegistry>| {
                        unit_material(kind, c, &mut m, &mut i, &mut cache, r.model(kind))
                    },
                )
                .unwrap()
        };
        let old = material(&mut world, blue);
        let root = world
            .spawn((UnitType(kind), Network, TeamId(0), UnitColor(blue)))
            .id();
        let plain = world
            .spawn((PieceIndex, MeshMaterial3d(old.clone()), ChildOf(root)))
            .id();
        let lit = world
            .spawn((
                PieceIndex,
                MeshMaterial3d(Handle::<StandardMaterial>::default()),
                OriginalMaterial(old.clone()),
                ChildOf(root),
            ))
            .id();
        world.run_system_once(recolor_taken_units).unwrap();
        let wearing = |world: &World, e: Entity| {
            world
                .get::<MeshMaterial3d<StandardMaterial>>(e)
                .unwrap()
                .0
                .clone()
        };
        assert_eq!(wearing(&world, plain), old, "its owner's own colour");

        world.entity_mut(root).insert(TeamId(1));
        world.run_system_once(recolor_taken_units).unwrap();
        let new = material(&mut world, colors.of(1, Network));
        assert_ne!(new, old);
        assert_eq!(
            world.get::<UnitColor>(root).unwrap().0,
            colors.of(1, Network)
        );
        assert_eq!(wearing(&world, plain), new);
        assert_eq!(wearing(&world, lit), new);
        assert_eq!(world.get::<OriginalMaterial>(lit).unwrap().0, new);
    }

    #[test]
    fn alternates_differ_from_every_faction() {
        for i in Faction::ALL.len() as u8..TeamColor::COUNT {
            let alt = TeamColor(i).color();
            assert!(Faction::ALL.iter().all(|f| f.color() != alt));
        }
    }
}
