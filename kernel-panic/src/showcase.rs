//! Per-faction unit showcase mode.
//!
//! Started from the main menu's "Showcase" page, showcase mode spawns only
//! the chosen faction's homebase on Data_Cache_L1. The [`ShowcaseDirector`]
//! then:
//!
//! 1. Enqueues one of each unit the homebase can build (`factory_roster`,
//!    including the faction's mobile constructor) so every unit model /
//!    animation is on display. Hacker additionally enqueues a spare Bug:
//!    the first Bug deploys into an Exploit; the second stays a Bug, so
//!    both deployable states are visible.
//! 2. Once the constructor has emerged and is idle, issues building orders
//!    one at a time — every entry in `buildings_for(constructor.kind)` —
//!    walking it to unclaimed datavents around the homebase.
//! 3. Hacker: fires `DeployEvent` on the first fully-emerged Bug so the
//!    exploit transition shows on screen.
//!
//! Every unit is spawned on team 0 (the local player) so the whole
//! showcase is player-controllable. No AI enemies exist; game-over is
//! gated off so the mode runs forever until the player quits.

use bevy::prelude::*;

use crate::{
    interaction::movement::MoveTarget,
    terrain::geovent::{GeoventSmoker, VentClaim},
    terrain::heightmap::Heightmap,
    ui::hud::build_menu::factory_roster,
    units::{
        components::{Faction, Homebase, TeamId, UnitType},
        content::definitions::UnitKind,
        lifecycle::{
            construction::{Constructing, PendingBuild, buildings_for},
            production::Producer,
            spawning::Emerging,
        },
        mechanics::deploy::DeployEvent,
    },
};

/// First datavent ring radius from the homebase (elmos) when building on
/// a ring around the homebase (fallback if too few vents).
const RING_RADIUS: f32 = 300.0;
/// Radial spacing between consecutive ring fallback sites.
const RING_SPACING: f32 = 120.0;

/// Director state for the showcase mode. One per game; cleared by
/// `spawn_map_world` when showcase mode is not active.
#[derive(Resource)]
pub struct ShowcaseDirector {
    /// Faction being showcased.
    pub faction: Faction,
    /// Resolved once the homebase entity has been found.
    homebase: Option<Entity>,
    /// Homebase world position (for site layout + deploy targeting).
    home_pos: Option<Vec3>,
    /// Roster entries remaining to enqueue. Drained once at start.
    roster: Vec<UnitKind>,
    /// Hacker: one spare Bug left in reserve after the first Bug is deployed.
    extra_bug_enqueued: bool,
    /// Hacker: whether a DeployEvent has been fired for the first Bug.
    bug_deployed: bool,
    /// Building kinds the constructor will erect, in order.
    builds: Vec<UnitKind>,
    /// Index of the next building order to issue.
    build_index: usize,
    /// Planned site positions (one per `buildings` entry).
    sites: Vec<Vec3>,
    /// True once the homebase has been found, roster enqueued, and sites
    /// computed.
    primed: bool,
}

impl ShowcaseDirector {
    pub fn new(faction: Faction) -> Self {
        let roster = factory_roster(faction.homebase(), faction).to_vec();
        Self {
            faction,
            homebase: None,
            home_pos: None,
            roster,
            extra_bug_enqueued: false,
            bug_deployed: false,
            builds: Vec::new(),
            build_index: 0,
            sites: Vec::new(),
            primed: false,
        }
    }
}

/// Main showcase director system. Runs once per frame in Update, before
/// the Produce set so enqueue + PendingBuild insertion are visible to
/// production/construction on the same frame.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn showcase_director(
    mut director: ResMut<ShowcaseDirector>,
    heightmap: Option<Res<Heightmap>>,
    mut homebases: Query<
        (
            Entity,
            &UnitType,
            &TeamId,
            &GlobalTransform,
            &mut Producer,
            Option<&Homebase>,
        ),
        With<Homebase>,
    >,
    constructors: Query<
        (
            Entity,
            &TeamId,
            &UnitType,
            Option<&MoveTarget>,
            Option<&PendingBuild>,
            Option<&Constructing>,
        ),
        Without<Homebase>,
    >,
    vents: Query<&GeoventSmoker, Without<VentClaim>>,
    bugs: Query<
        (Entity, &UnitType, &TeamId),
        (
            Without<Homebase>,
            Without<Emerging>,
            Without<crate::units::combat::Dying>,
        ),
    >,
    mut deploy_writer: MessageWriter<DeployEvent>,
    mut commands: Commands,
) {
    let hm = match heightmap {
        Some(h) => h,
        None => return,
    };
    let d = &mut *director;

    // ------------------------------------------------------------------
    // Phase 1 — find the team-0 homebase once.
    // ------------------------------------------------------------------
    if d.homebase.is_none() {
        let found = homebases.iter_mut().find(|(_, ut, team, ..)| {
            team.0 == 0 && matches!(ut.0, UnitKind::Kernel | UnitKind::Hole | UnitKind::Carrier)
        });
        if let Some((entity, _ut, _team, gtf, _prod, _homebase)) = found {
            d.homebase = Some(entity);
            d.home_pos = Some(gtf.translation());
        } else {
            return; // homebase hasn't spawned yet
        }
    }

    // ------------------------------------------------------------------
    // Phase 2 — enqueue factory roster into the homebase Producer.
    // ------------------------------------------------------------------
    if !d.primed {
        let home_pos = d.home_pos.unwrap();

        // --- Enqueue roster ---
        if let Ok((_, _, _, _, mut producer, _)) = homebases.get_mut(d.homebase.unwrap()) {
            for &kind in &d.roster {
                producer.enqueue(kind);
            }
            // Hacker: add one extra Bug after the roster so the second Bug
            // (which deploys first from the roster) leaves a Bug visible.
            if d.faction == Faction::Hacker && !d.extra_bug_enqueued {
                producer.enqueue(UnitKind::Bug);
                d.extra_bug_enqueued = true;
            }
        }

        // --- Compute building sites ---
        let ctor_kind = d.faction.constructor();
        d.builds = buildings_for(ctor_kind).to_vec();
        let (world_w, world_d) = hm.world_size();

        // Nearest unclaimed vents to the homebase.
        let mut vent_sites: Vec<Vec3> = vents
            .iter()
            .map(|v| v.pos)
            .filter(|p| {
                // Must be reachable — not too far outside the map.
                p.x > 0.0
                    && p.x < world_w
                    && p.z > 0.0
                    && p.z < world_d
                    // Must not be the homebase position itself.
                    && p.distance_squared(home_pos) > 60.0 * 60.0
            })
            .collect();
        vent_sites.sort_by(|a, b| {
            a.distance_squared(home_pos)
                .partial_cmp(&b.distance_squared(home_pos))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Assign: use nearest vents first, pad with ring offsets.
        let needed = d.builds.len();
        d.sites = vent_sites.into_iter().take(needed).collect();
        if d.sites.len() < needed {
            let missing = needed - d.sites.len();
            let (world_w, world_d) = hm.world_size();
            let cx = home_pos.x.clamp(RING_RADIUS, world_w - RING_RADIUS);
            let cz = home_pos.z.clamp(RING_RADIUS, world_d - RING_RADIUS);
            for i in 0..missing {
                let angle =
                    std::f32::consts::TAU * i as f32 / missing as f32;
                let r = RING_RADIUS + (i as f32) * RING_SPACING;
                let rx = (cx + r * angle.cos()).clamp(0.0, world_w);
                let rz = (cz + r * angle.sin()).clamp(0.0, world_d);
                d.sites.push(hm.place(rx, rz));
            }
        }

        d.primed = true;
        info!(
            "Showcase({:?}): primed — {} units enqueued, {} build sites",
            d.faction,
            d.roster.len() + if d.faction == Faction::Hacker { 1 } else { 0 },
            d.builds.len(),
        );
    }

    // ------------------------------------------------------------------
    // Phase 3 — issue building orders to the idle constructor.
    // ------------------------------------------------------------------
    if d.primed && d.build_index < d.builds.len() {
        let team0 = TeamId(0);
        if let Some((entity, _team, _ut, _mt, _pb, _c)) = constructors.iter().find(|(
            _entity,
            team,
            ut,
            mt,
            pb,
            c,
        )| {
            team.0 == team0.0
                && ut.0 == d.faction.constructor()
                && mt.is_none()
                && pb.is_none()
                && c.is_none()
        }) {
            let site = d.sites[d.build_index];
            let kind = d.builds[d.build_index];
            commands
                .entity(entity)
                .insert(MoveTarget(site))
                .insert(PendingBuild { kind, site });
            d.build_index += 1;
            info!(
                "Showcase({:?}): builder → {:?} at ({:.0}, {:.0}, {:.0})",
                d.faction, kind, site.x, site.y, site.z,
            );
        }
    }

    // ------------------------------------------------------------------
    // Phase 4 — Hacker: deploy the first fully-emerged Bug.
    // ------------------------------------------------------------------
    if d.primed
        && d.faction == Faction::Hacker
        && !d.bug_deployed
        && let Some((bug_entity, _, _)) = bugs
            .iter()
            .find(|(_, ut, team)| team.0 == 0 && ut.0 == UnitKind::Bug)
    {
        deploy_writer.write(DeployEvent { entity: bug_entity });
        d.bug_deployed = true;
        info!("Showcase(Hacker): deploying first Bug → Exploit");
    }
}

/// Plugin that registers the showcase director system. Added to the app
/// from `main.rs`.
pub struct ShowcasePlugin;

impl Plugin for ShowcasePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            showcase_director
                .before(crate::units::GameplaySet::Produce)
                .run_if(in_state(crate::game_setup::AppState::InGame))
                .run_if(resource_exists::<ShowcaseDirector>),
        );
    }
}
