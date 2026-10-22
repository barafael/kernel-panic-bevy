//! Command-fire abilities: NX Flag, Infection Gas, and (eventually)
//! SIGTERM airstrikes + Firewall reflector shields.
//!
//! These weapons have `commandfire=1` in their TDFs; the auto-fire path
//! in `combat_system` skips them, and they enter play only through
//! an explicit player order processed here.
//!
//! The shared mechanism is the `AreaDenialZone` entity: a volume that
//! deals DPS to units in radius until its TTL expires. Zone parameters
//! mirror upstream `LuaRules/Gadgets/areadenial.lua` ([`DenialWeapon`]):
//!
//! | Weapon    | Radius | DPS | TTL | Friendly-fire | Infects |
//! |-----------|--------|-----|-----|---------------|---------|
//! | nx        | 120    | 100 | 60s | yes           | no      |
//! | infection | 400    | 120 | 13s | no            | yes     |
//! | sigterm   | 350    | 2000| 3s  | yes           | no      |
//!
//! None of these abilities touch `Health` directly: the SIGTERM bomb is
//! a [`PendingDamage`] explosion of the `[SigTerm]` weapon, and the
//! zones' ticks and the Mine Launcher's HP cost are `Spring.AddUnitDamage`
//! calls upstream, queued here as [`Hit`]s — so the Firewall, `ARMORED`,
//! the scripts' `HitByWeaponId`, the idle-heal reset and infection all
//! apply to them exactly as to weapon fire.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::interaction::movement::{MovePath, MoveTarget};
use crate::net::NetId;
use crate::sim::DetFloat;
use crate::terrain::heightmap::Heightmap;
use crate::units::assets::meshes::{S3OModelCache, load_s3o_mesh, unit_material};
use crate::units::combat::Dying;
use crate::units::combat::{Armored, Attacker, DamageQueue, DelayedDamage, Hit, PendingDamage};
use crate::units::components::{Faction, Health, TeamId, UnitType};
use crate::units::content::cegs::CegKind;
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::content::weapons::{WeaponKind, WeaponRegistry};
use crate::units::spatial::{SpatialIndex, flat_dist_sq};
use crate::units::team_colors::{TeamColor, TeamColors};
use crate::units::teams::Alliances;
use crate::units::weapon_fx::{ExplosionEvent, PendingExplosions};

/// Filename of the SIGTERM bomber's S3O model (`signal.fbi:ObjectName`).
const SIGNAL_MODEL: &str = "signal.s3o";
/// Filename of the SIGTERM bomb's S3O model
/// (`weapons/retroweapons.tdf:[SigTerm]:model`).
const SIGTERM_BOMB_MODEL: &str = "sigterm.s3o";

/// Lazily-loaded mesh + per-faction material handles for the two
/// SIGTERM visual stages. Mirrors `DeathParticleAssets` /
/// `BuildSparkleAssets`. The 90 s SIGTERM cooldown means cache
/// misses are rare anyway, but caching keeps the per-cast spawn
/// allocation-free and avoids leaking a fresh `StandardMaterial`
/// per cast.
#[derive(Resource, Default)]
pub struct SigTermAssets {
    signal_mesh: Option<Handle<Mesh>>,
    bomb_mesh: Option<Handle<Mesh>>,
    /// One material per owner colour (caster colour-tints the visual).
    signal_material: HashMap<TeamColor, Handle<StandardMaterial>>,
    bomb_material: HashMap<TeamColor, Handle<StandardMaterial>>,
}

impl SigTermAssets {
    /// The shared body of `signal`/`bomb`: the stage's mesh (loaded
    /// once, with a primitive fallback) and its owner-coloured
    /// material (one per colour, cached).
    #[allow(clippy::too_many_arguments)]
    fn stage(
        mesh: &mut Option<Handle<Mesh>>,
        materials_map: &mut HashMap<TeamColor, Handle<StandardMaterial>>,
        model: &str,
        fallback: impl FnOnce(&mut Assets<Mesh>) -> Handle<Mesh>,
        color: TeamColor,
        meshes: &mut Assets<Mesh>,
        materials: &mut Assets<StandardMaterial>,
        images: &mut Assets<Image>,
        model_cache: &mut S3OModelCache,
    ) -> (Handle<Mesh>, Handle<StandardMaterial>) {
        let mesh = mesh
            .get_or_insert_with(|| {
                load_s3o_mesh(model, meshes, model_cache).unwrap_or_else(|| fallback(meshes))
            })
            .clone();
        // Why: `unit_material` keys textures by owner colour, not by
        // UnitKind — Signal is the right lookup for both stages of the
        // strike.
        let material = materials_map
            .entry(color)
            .or_insert_with(|| {
                unit_material(
                    UnitKind::Signal,
                    color,
                    materials,
                    images,
                    model_cache,
                    model,
                )
            })
            .clone();
        (mesh, material)
    }

    fn signal(
        &mut self,
        color: TeamColor,
        meshes: &mut Assets<Mesh>,
        materials: &mut Assets<StandardMaterial>,
        images: &mut Assets<Image>,
        model_cache: &mut S3OModelCache,
    ) -> (Handle<Mesh>, Handle<StandardMaterial>) {
        Self::stage(
            &mut self.signal_mesh,
            &mut self.signal_material,
            SIGNAL_MODEL,
            |m| m.add(Cuboid::new(18.0, 6.0, 24.0)),
            color,
            meshes,
            materials,
            images,
            model_cache,
        )
    }

    fn bomb(
        &mut self,
        color: TeamColor,
        meshes: &mut Assets<Mesh>,
        materials: &mut Assets<StandardMaterial>,
        images: &mut Assets<Image>,
        model_cache: &mut S3OModelCache,
    ) -> (Handle<Mesh>, Handle<StandardMaterial>) {
        Self::stage(
            &mut self.bomb_mesh,
            &mut self.bomb_material,
            SIGTERM_BOMB_MODEL,
            |m| m.add(Sphere::new(14.0)),
            color,
            meshes,
            materials,
            images,
            model_cache,
        )
    }
}

/// Everything spawning a SIGTERM stage's visual needs, bundled so the
/// systems that spawn them stay under Bevy's system-param limit.
#[derive(bevy::ecs::system::SystemParam)]
pub struct SigTermVisuals<'w> {
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    images: ResMut<'w, Assets<Image>>,
    model_cache: ResMut<'w, S3OModelCache>,
    assets: ResMut<'w, SigTermAssets>,
    team_colors: Option<Res<'w, TeamColors>>,
}

impl SigTermVisuals<'_> {
    /// The Signal of `owner`'s caster of `faction`.
    fn signal(&mut self, owner: u8, faction: Faction) -> (Handle<Mesh>, Handle<StandardMaterial>) {
        let color = TeamColors::lookup(self.team_colors.as_deref(), owner, faction);
        self.assets.signal(
            color,
            &mut self.meshes,
            &mut self.materials,
            &mut self.images,
            &mut self.model_cache,
        )
    }

    /// The bomb of `owner`'s caster of `faction`.
    fn bomb(&mut self, owner: u8, faction: Faction) -> (Handle<Mesh>, Handle<StandardMaterial>) {
        let color = TeamColors::lookup(self.team_colors.as_deref(), owner, faction);
        self.assets.bomb(
            color,
            &mut self.meshes,
            &mut self.materials,
            &mut self.images,
            &mut self.model_cache,
        )
    }
}

/// Recharge of the two Lua-driven abilities, in seconds. Upstream
/// `airstrike.lua` (`airstrike_reload_time = 90`) and
/// `network_reflectorshield.lua` (`reloadTime = 90`) both count it as
/// `reload * 32` sim frames — at Spring's 30 Hz that is 96 s of game
/// time (their button label counts down 32-frame "seconds" from 90).
/// Both also start the recharge in `UnitCreated`, so a fresh Terminal /
/// Firewall is not ready either (see [`initial_cooldown`]).
pub const LUA_ABILITY_RELOAD: f32 = 90.0 * 32.0 / 30.0;

/// Firewall protection zone: all allies within `FIREWALL_RADIUS` at
/// cast time gain a `Protected` component for `FIREWALL_DURATION`.
/// Upstream's `CMD_FIREWALL` is an `ICON_MAP` command run straight from
/// `CommandFallback` — `GetUnitsInCylinder(x, z, 300)` around the click
/// with no range check — so the Firewall reaches any point on the map.
pub const FIREWALL_RADIUS: f32 = 300.0;
pub const FIREWALL_DURATION: f32 = 20.0;
pub const FIREWALL_COOLDOWN: f32 = LUA_ABILITY_RELOAD;
/// Fraction of incoming non-paralyzer damage that a Protected unit
/// *takes* (`network_reflectorshield.lua` `UnitPreDamaged`: `damage*0.5`).
/// The rest is reflected back to an attacker of another team; the
/// damage pipeline (`DamageResolver::deliver`) applies both.
pub const FIREWALL_DAMAGE_TAKEN: f32 = 0.5;

/// Byte `LaunchMines` ability (§3.5): an instant command (`CMDTYPE.ICON`
/// in upstream `LuaRules/Gadgets/specialattack.lua`) that fires five
/// MineLauncher bolts from the Byte's launcher tubes; a Logic Bomb
/// appears where each one explodes:
/// > "Launches several mines in a forward arc, at the cost of 6000
/// >  hitpoints. 10s reload."
///
/// byte.bos `LaunchMines`: `sleep 10000` before `cantLaunch` clears.
pub const MINELAUNCHER_COOLDOWN: f32 = 10.0;
/// byte.bos `HITPOINT_COST`, paid through `lua_Damage` →
/// `Spring.AddUnitDamage(u, 6000, 0, -1, sigterm)` (LuaCOB.lua): a real
/// hit credited to the SIGTERM bomb, so the Firewall halves it and the
/// closed Byte's 30% rule does not.
pub const MINELAUNCHER_HP_COST: f32 = 6000.0;
/// Below this remaining HP the Byte refuses to cast so a mis-click
/// can't instantly end it. Upstream lets the Byte suicide-cast; we
/// keep a small buffer for ergonomics.
pub const MINELAUNCHER_HP_FLOOR: f32 = 100.0;
/// byte.bos `LaunchMines`: `emit-sfx 2049` (weapon 2, the MineLauncher)
/// from these pieces, in this order, while Logic Bombs are left under
/// the cap. `Create()` points them 30° down and fans them -18/9/0/9/18°
/// (launcher2 and launcher4 share 9°, as upstream).
pub const MINELAUNCHER_TUBES: [&str; 5] = [
    "launcher3",
    "launcher2",
    "launcher4",
    "launcher1",
    "launcher5",
];
/// byte.bos `LaunchMines` on a folded Byte: `start-script Open(); sleep
/// 500;` and try again.
const MINELAUNCHER_RETRY: f32 = 0.5;

/// Terminal `SIGTERM` airstrike: a [`SigTermSignal`] takes off from
/// the Terminal, flies to the target and drops a [`SigTermBomb`] that
/// falls and detonates. Upstream (`airstrike.lua`) the Signal is a real
/// airplane (`CStrafeAirMoveType`) given an attack order; here it is
/// flown by the same rules with signal.fbi's numbers and the engine's
/// airplane defaults, per 30 Hz sim frame. Neither stage carries a
/// `UnitType`, which is what makes them untargetable — same effect as
/// upstream's `Category=NUKE VTOL` + every ground unit's
/// `NoChaseCategory=VTOL`.
pub const SIGTERM_COOLDOWN: f32 = LUA_ABILITY_RELOAD;
/// The height the Signal holds above the ground: `CStrafeAirMoveType`'s
/// `wantedHeight = cruiseAlt × 1.5` (signal.fbi `cruiseAlt=200`).
pub const SIGTERM_CRUISE_HEIGHT: f32 = 300.0;
/// Thrust, elmos/frame²: signal.fbi `Acceleration=2`.
const SIGTERM_ACC: f32 = 2.0;
/// Air drag per frame (`SetMaxSpeed`: `1 / (maxSpeed · 1.1 / accRate)
/// − wingAngle² · wingDrag`, `MaxVelocity=8`): with the thrust above, a
/// top speed of about 6.8 elmos/frame (205 elmos/s).
const SIGTERM_DRAG: f32 = 1.0 / (8.0 * 1.1 / SIGTERM_ACC) - 0.08 * 0.08 * 0.07;
/// The engine's airplane turn radius (`turnRadius` default 500 elmos):
/// a turn at speed `v` is at most `v / 500` radians per frame.
const SIGTERM_TURN_RADIUS: f32 = 500.0;
/// While climbing out it swings toward the target at `maxRudder`
/// (0.004) per elmo/frame of climb (`UpdateTakeOff`).
const SIGTERM_TAKEOFF_RUDDER: f32 = 0.004;
/// Most it banks into a turn, radians (`maxBank` default 0.8), and how
/// fast the bank follows, radians per frame.
const SIGTERM_MAX_BANK: f32 = 0.8;
const SIGTERM_BANK_RATE: f32 = 0.04;
/// Steepest climb or dive, radians (`maxPitch` default 0.45).
const SIGTERM_MAX_PITCH: f32 = 0.45;
/// It looks this many frames ahead for higher ground (`UpdateFlying`).
const SIGTERM_LOOKAHEAD_FRAMES: f32 = 40.0;
/// The bomb's gravity, elmos/frame² (`[SigTerm]` `myGravity=.3`).
const SIGTERM_BOMB_GRAVITY: f32 = 0.3;
/// The bomb's release window: `CBombDropper::TestRange` drops when the
/// predicted landing point is within `salvoDelay · speed · 0.5` of the
/// target (`burstrate=.6` → 18 frames).
const SIGTERM_SALVO_FRAMES: f32 = 18.0;
/// signal.bos `Create`: `call-script lua_Destroy()` after 120 s.
const SIGTERM_MAX_FRAMES: u32 = 120 * 30;
/// Gravity on the broken-up airframe (the engine's plane `myGravity`).
const SIGTERM_WRECK_GRAVITY: f32 = 0.4;

/// SIGTERM stage 1: the Signal in the air. `tick_sigterm_signals` flies
/// it one sim frame at a time and drops a [`SigTermBomb`] once the bomb
/// would land on the target.
#[derive(Component, Debug, Clone)]
pub struct SigTermSignal {
    pub target: Vec3,
    pub pos: Vec3,
    /// Elmos per frame.
    pub vel: Vec3,
    /// Horizontal heading (unit XZ vector) and speed along it.
    pub heading: Vec2,
    pub speed: f32,
    /// Roll into the current turn, radians (+ = right wing down).
    pub bank: f32,
    /// Climbing out of the Terminal (`UpdateTakeOff`), not flying yet.
    pub taking_off: bool,
    /// Flying on straight past a target too close to turn onto, to come
    /// around for another pass.
    pub extending: bool,
    pub frames: u32,
    pub owner_team: u8,
    pub owner_faction: Faction,
}

/// SIGTERM stage 2: the falling bomb, carrying the Signal's velocity
/// under its own gravity (`CBombDropper::FireImpl`). Detonates where it
/// enters the ground.
#[derive(Component, Debug, Clone)]
pub struct SigTermBomb {
    pub pos: Vec3,
    /// Elmos per frame.
    pub vel: Vec3,
    pub owner_team: u8,
    pub owner_faction: Faction,
}

/// The Signal after the drop: signal.bos `FireWeapon1` destroys it and
/// its `Killed` lets the airframe fall apart (`explode … type FALL`). A
/// visual only: it tumbles down and vanishes on the ground.
#[derive(Component, Debug, Clone)]
pub struct SigTermWreck {
    pub vel: Vec3,
    pub spin: Vec3,
}

/// byte.bos `LaunchMines` in progress: the Byte unfolds (the animation
/// treats it as a reason to open) and [`tick_mine_launches`] fires once
/// it is open, checking every [`MINELAUNCHER_RETRY`] seconds like the
/// script's `sleep 500` loop. A second cast restarts it (`signal
/// SIG_MINE`); orders don't touch it.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
#[component(storage = "SparseSet")]
pub struct MineLaunch {
    /// Seconds until the next "is it open yet?" check.
    pub retry_in: f32,
}

/// Queued request to spawn a Logic Bomb, drained next frame by
/// [`spawning::spawn_queued_mines`]. Lives as a resource so the
/// command-fire path doesn't have to carry the 7-argument
/// `spawn_unit` parameter pack.
#[derive(Debug, Clone)]
pub struct MineSpawn {
    pub position: Vec3,
    pub faction: Faction,
    pub team: u8,
}

#[derive(Resource, Default, Clone)]
pub struct MineSpawnQueue(Vec<MineSpawn>);

impl MineSpawnQueue {
    pub fn push(&mut self, spawn: MineSpawn) {
        self.0.push(spawn);
    }

    pub fn drain(&mut self) -> std::vec::Drain<'_, MineSpawn> {
        self.0.drain(..)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// While present, the unit takes `FIREWALL_DAMAGE_TAKEN` of any
/// incoming non-paralyzer damage and the rest is reflected to an
/// attacker of another team (see `DamageResolver::deliver`).
#[derive(Component, Debug, Clone)]
pub struct Protected {
    pub remaining: f32,
}

/// Per-caster cooldown in seconds before the command-fire ability is
/// available again. Matches upstream weapon `reloadtime` (30s for nx,
/// longer for Infection) so the ability cadence feels right without
/// reading TDF values at runtime.
#[derive(Component)]
pub struct CommandFireCooldown {
    pub remaining: f32,
}

/// The recharge a freshly created `kind` starts with. Upstream's
/// `airstrike.lua` / `network_reflectorshield.lua` stamp the full
/// reload in `UnitCreated` — which fires when the nanoframe appears, so
/// the timer runs down during construction. The weapon-driven abilities
/// (NX Flag, Infection, Mine Launch) start loaded.
pub fn initial_cooldown(kind: UnitKind) -> Option<CommandFireCooldown> {
    match kind {
        UnitKind::Terminal => Some(CommandFireCooldown {
            remaining: SIGTERM_COOLDOWN,
        }),
        UnitKind::Firewall => Some(CommandFireCooldown {
            remaining: FIREWALL_COOLDOWN,
        }),
        _ => None,
    }
}

/// Fallback cast ranges (elmos) for the weapon-backed abilities, used
/// only when the TDF registry is unavailable (tests / missing upstream
/// data). Values are the TDF `range=` of each weapon.
const NX_RANGE: f32 = 1400.0; // retroweapons.tdf [nx]
const INFECTION_RANGE: f32 = 2000.0; // corruptionweapons.tdf [Infection]

/// What bounds where `kind`'s ability may land.
///
/// NX Flag and Infection are real weapons upstream, so the
/// engine only fires them at targets its `TestRange` accepts (the unit
/// walks into range first if it can move) — for the Obelisk's ballistic
/// Infection that reach depends on the height difference (about 1780
/// elmos on level ground, not its `range=2000`). SIGTERM and Firewall
/// are Lua `ICON_MAP` commands executed in `CommandFallback` without any
/// distance check: the Signal bomber flies to the target, the Firewall
/// shield lands wherever the player clicked. Mine Launch is an instant
/// command: the Byte fires where its tubes point, wherever it is.
#[derive(Clone, Copy)]
enum CastReachLimit<'a> {
    MapWide,
    Weapon(&'a spring_tdf::WeaponDef),
    /// The weapon is missing from the registry: its TDF `range=` as a
    /// flat cylinder.
    Fallback(f32),
}

fn cast_limit(kind: UnitKind, weapons: &WeaponRegistry) -> CastReachLimit<'_> {
    // A variant compare and an array read — this runs per pending caster
    // per tick, so no name hashing here.
    let (weapon, fallback) = match kind {
        UnitKind::Pointer => (WeaponKind::Nx, NX_RANGE),
        UnitKind::Obelisk => (WeaponKind::Infection, INFECTION_RANGE),
        _ => return CastReachLimit::MapWide,
    };
    match weapons.get(weapon) {
        Some(def) if def.range > 0.0 => CastReachLimit::Weapon(def),
        _ => CastReachLimit::Fallback(fallback),
    }
}

/// A mobile caster walking toward an out-of-range ability target.
/// [`advance_pending_casts`] fires the ability once the target is
/// in reach ([`cast_limit`]), mirroring how Spring's command-fire attack
/// order approaches before shooting. Any new order clears it
/// (`interaction::clear_orders`, the replace-branch of
/// `apply_ordered_command`, Stop); losing its `MoveTarget` without
/// reaching range (e.g. stuck) abandons the cast.
#[derive(Component, Debug, Clone, Copy)]
#[component(storage = "SparseSet")]
pub struct PendingCommandFire {
    pub target: Vec3,
}

/// Event: a selected unit should fire its command-fire ability at
/// `target`. The weapon and source-unit resolution happens when the
/// event is processed so the hotkey handler doesn't need to know which
/// slot (Weapon1 vs Weapon2) holds the ability.
#[derive(Message, Debug, Clone)]
pub struct CommandFireEvent {
    pub attacker: Entity,
    pub target: Vec3,
}

/// The weapons `areadenial.lua` watches (`weaponInfo`): their explosion
/// leaves a zone that keeps dealing their damage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenialWeapon {
    SigTerm,
    Nx,
    Infection,
}

/// One `weaponInfo` row of `areadenial.lua`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DenialParams {
    /// `GetUnitsInSphere` radius around the explosion.
    pub radius: f32,
    /// `damage` per second (the gadget deals `damage / 30` per frame).
    pub dps: f32,
    /// Zone lifetime in seconds (`ttl` sim frames).
    pub ttl: f32,
    /// `damageFriendly`: allies of the owner are hit too.
    pub damage_friendly: bool,
}

impl DenialWeapon {
    /// `areadenial.lua` `gadget:Initialize`.
    pub fn params(self) -> DenialParams {
        let (radius, dps, ttl_frames, damage_friendly) = match self {
            DenialWeapon::SigTerm => (350.0, 2000.0, 100.0, true),
            DenialWeapon::Nx => (120.0, 100.0, 1800.0, true),
            DenialWeapon::Infection => (400.0, 120.0, 400.0, false),
        };
        DenialParams {
            radius,
            dps,
            ttl: crate::sim::frames_to_secs(ttl_frames),
            damage_friendly,
        }
    }

    /// The weapon the zone's `AddUnitDamage` reports (the Infection gas
    /// infects through it; SIGTERM's bypasses a closed Byte's armour and
    /// hits homebases for 10%).
    fn weapon(self) -> WeaponKind {
        match self {
            DenialWeapon::SigTerm => WeaponKind::SigTerm,
            DenialWeapon::Nx => WeaponKind::Nx,
            DenialWeapon::Infection => WeaponKind::Infection,
        }
    }
}

/// A persistent area-denial volume ([`DenialWeapon::params`]): until its
/// TTL runs out, every unit within the radius takes the weapon's damage
/// per second, as `Spring.AddUnitDamage` calls credited to `owner`.
#[derive(Component, Debug, Clone)]
pub struct AreaDenialZone {
    pub weapon: DenialWeapon,
    pub center: Vec3,
    /// Seconds left.
    pub remaining: f32,
    /// The unit credited with the damage (`w.owner`): the caster, and —
    /// once that is gone — a homebase or minifac of `owner_team`
    /// (the gadget's handicap workaround), else nobody.
    pub owner: Option<Entity>,
    pub owner_team: u8,
    pub owner_faction: Faction,
}

impl AreaDenialZone {
    pub fn new(
        weapon: DenialWeapon,
        center: Vec3,
        owner: Option<Entity>,
        owner_team: u8,
        owner_faction: Faction,
    ) -> Self {
        Self {
            weapon,
            center,
            remaining: weapon.params().ttl,
            owner,
            owner_team,
            owner_faction,
        }
    }
}

/// What to do with a cast order given the caster's position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CastReach {
    /// Target inside the ability's range (or the ability is map-wide).
    Fire,
    /// Out of range, but the caster can walk into range first.
    Approach,
    /// Out of range and the caster is stationary: refuse the order,
    /// as Spring does for a building told to attack beyond its range.
    Refuse,
}

/// `gravity`: the map's ([`crate::sim::MapGravity`]), for ballistic reach.
fn cast_reach(
    limit: CastReachLimit,
    caster: Vec3,
    target: Vec3,
    mobile: bool,
    gravity: f32,
) -> CastReach {
    let in_reach = match limit {
        CastReachLimit::MapWide => true,
        // `CWeapon::TestRange` against a ground position. The caster's
        // origin stands in for its aim-from piece.
        CastReachLimit::Weapon(def) => {
            crate::units::combat::aim::in_weapon_range(def, caster, target, true, gravity)
        }
        CastReachLimit::Fallback(range) => flat_dist_sq(caster, target) <= range * range,
    };
    match (in_reach, mobile) {
        (true, _) => CastReach::Fire,
        (false, true) => CastReach::Approach,
        (false, false) => CastReach::Refuse,
    }
}

/// Fire the command-fire ability of every [`PendingCommandFire`] caster
/// that has walked into range, and drop pending casts whose approach
/// walk ended (stuck / pathing gave up) short of range. Runs right
/// before [`process_command_fire`], which receives the in-range event.
#[allow(clippy::type_complexity)]
pub fn advance_pending_casts(
    weapons: Res<WeaponRegistry>,
    gravity: Option<Res<crate::sim::MapGravity>>,
    casters: Query<
        (
            Entity,
            &UnitType,
            &GlobalTransform,
            &PendingCommandFire,
            Has<MoveTarget>,
            &NetId,
        ),
        Without<Dying>,
    >,
    mut events: MessageWriter<CommandFireEvent>,
    mut commands: Commands,
) {
    let gravity = gravity.as_deref().copied().unwrap_or_default().0;
    // Unit-id order: the casts resolve in the order of their events
    // (`sim`).
    for (entity, unit, gtf, pending, moving, _) in casters.iter().sort::<&NetId>() {
        match cast_reach(
            cast_limit(unit.0, &weapons),
            gtf.translation(),
            pending.target,
            true,
            gravity,
        ) {
            CastReach::Fire => {
                // In range: stop the approach walk where it stands and
                // cast — Spring halts an attacking unit once it can fire.
                // Why an empty path instead of removing the order: the
                // walk then ends the way any move order does, and the
                // command queue advances to a shift-queued follow-up
                // (`movement::advance_command_queue`) as usual.
                let mut caster = commands.entity(entity);
                caster.remove::<PendingCommandFire>();
                if moving {
                    caster.insert(MovePath::finished());
                }
                events.write(CommandFireEvent {
                    attacker: entity,
                    target: pending.target,
                });
            }
            _ if !moving => {
                commands.entity(entity).remove::<PendingCommandFire>();
            }
            _ => {}
        }
    }
}

/// Drain queued `CommandFireEvent`s into persistent
/// `AreaDenialZone` entities. A unit identifies its command-fire
/// weapon by looking at its `UnitKind` — NX Flag for Pointer, Infection
/// for Obelisk. Units without a registered ability are ignored.
///
/// Weapon-backed abilities honour their weapon's reach ([`cast_limit`]): a
/// mobile caster ordered beyond it walks into range first (see
/// [`PendingCommandFire`]), a stationary one refuses the order.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn process_command_fire(
    mut events: MessageReader<CommandFireEvent>,
    casters: Query<
        (
            &UnitType,
            &TeamId,
            &Faction,
            &GlobalTransform,
            Option<&CommandFireCooldown>,
            Has<PendingCommandFire>,
            Has<Armored>,
        ),
        Without<Dying>,
    >,
    unit_registry: Res<UnitRegistry>,
    weapons: Res<WeaponRegistry>,
    protect_targets: Query<(Entity, &TeamId, &GlobalTransform), With<Health>>,
    health_q: Query<&Health>,
    live_units: Query<(&UnitType, &TeamId), Without<Dying>>,
    mine_spawns: Res<MineSpawnQueue>,
    mut pending_explosions: ResMut<PendingExplosions>,
    mut visuals: SigTermVisuals,
    gravity: Option<Res<crate::sim::MapGravity>>,
    mut sounds: Option<ResMut<crate::audio::SoundQueue>>,
    mut commands: Commands,
    alliances: Res<Alliances>,
) {
    let gravity = gravity.as_deref().copied().unwrap_or_default().0;
    for event in events.read() {
        let Ok((unit, team, faction, gtf, cd, was_pending, armored)) = casters.get(event.attacker)
        else {
            continue;
        };
        if cd.is_some_and(|c| c.remaining > 0.0) {
            continue;
        }
        match cast_reach(
            cast_limit(unit.0, &weapons),
            gtf.translation(),
            event.target,
            unit_registry.speed(unit.0) > 0.0,
            gravity,
        ) {
            CastReach::Fire => {}
            CastReach::Approach => {
                // A fresh order: drop whatever the caster was doing and
                // walk toward the target; `advance_pending_casts` casts
                // once it's in range.
                crate::interaction::clear_orders(&mut commands.entity(event.attacker)).insert((
                    PendingCommandFire {
                        target: event.target,
                    },
                    MoveTarget(event.target),
                ));
                continue;
            }
            CastReach::Refuse => continue,
        }
        if was_pending {
            // An in-range cast supersedes an earlier cast's approach
            // walk — stop walking toward the stale target.
            commands
                .entity(event.attacker)
                .remove::<(PendingCommandFire, MoveTarget, MovePath)>();
        }

        if unit.0 == UnitKind::Firewall {
            apply_firewall(
                event.target,
                team.0,
                &alliances,
                &protect_targets,
                &mut commands,
            );
            // `network_reflectorshield.lua:99`: `PlaySoundFile("sounds/built.wav")`.
            if let Some(sounds) = sounds.as_deref_mut() {
                sounds.play(
                    crate::audio::SoundId::Built,
                    crate::audio::Channel::General,
                    None,
                );
            }
            commands.entity(event.attacker).insert(CommandFireCooldown {
                remaining: FIREWALL_COOLDOWN,
            });
            continue;
        }

        if unit.0 == UnitKind::Byte {
            // byte.bos `LaunchMines`: `if(get ARMORED) return 0` — a
            // Byte still in its post-build spawn protection can't cast.
            if armored {
                continue;
            }
            // Why: `spawn_queued_mines` drops every mine past the Logic
            // Bomb cap, so a cast with no room left would burn 6000 HP
            // and the reload for nothing — refuse it up front instead.
            let limit = unit_registry.team_limit(UnitKind::LogicBomb);
            if logic_bombs_at_cap(team.0, limit, &live_units, &mine_spawns) {
                continue;
            }
            if !health_q.get(event.attacker).is_ok_and(can_pay_for_mines) {
                continue;
            }
            // The target is not used: the mines go where the tubes point.
            commands
                .entity(event.attacker)
                .insert(MineLaunch { retry_in: 0.0 });
            continue;
        }

        if unit.0 == UnitKind::Terminal {
            // `airstrike.lua`: the Signal is created on the Terminal,
            // facing the way the Terminal faces, and given an attack
            // order on the point; it takes off from there. No `UnitType`
            // on either stage → not in the spatial index → not
            // targetable.
            let caster_pos = gtf.translation();
            let facing = gtf.forward().xz().normalize_or(Vec2::NEG_Y);
            let signal = SigTermSignal {
                target: event.target,
                pos: caster_pos,
                vel: Vec3::ZERO,
                heading: facing,
                speed: 0.0,
                bank: 0.0,
                taking_off: true,
                extending: false,
                frames: 0,
                owner_team: team.0,
                owner_faction: *faction,
            };
            let (signal_mesh, signal_material) = visuals.signal(team.0, *faction);
            commands.spawn((
                Transform::from_translation(caster_pos).with_rotation(signal.orientation()),
                signal,
                Mesh3d(signal_mesh),
                MeshMaterial3d(signal_material),
                Visibility::default(),
            ));
            commands.entity(event.attacker).insert(CommandFireCooldown {
                remaining: SIGTERM_COOLDOWN,
            });
            continue;
        }

        if unit.0 == UnitKind::Pointer {
            // The NX Flag is weapon 2 of the Pointer: `specialattack.lua`
            // turns the order into an attack command (replacing the
            // current one), and the script fires it only once the cube
            // is open and aimed — [`tick_nx_casts`] does the rest.
            crate::interaction::clear_orders(&mut commands.entity(event.attacker)).insert(
                NxCast::Aiming {
                    target: event.target,
                },
            );
            continue;
        }

        let Some(ability) = ability_for(unit.0) else {
            continue;
        };
        // Upstream's shell detonates through its
        // `explosiongenerator=custom:…`; replay that CEG so the cast
        // lands as fire-and-spread, not a silent debuff. Drained by
        // `spawn_pending_explosions` in the same tick chain.
        if let Some(ceg) = ability.explosion_ceg {
            pending_explosions.events.push(ExplosionEvent {
                pos: event.target,
                up: Vec3::Y,
                rgb: faction.rgb_f32(),
                radius: ability.weapon.params().radius,
                ceg: Some(ceg),
                damage: weapons
                    .get(ability.weapon.weapon())
                    .map_or(0.0, |def| def.damage.default),
            });
            // The blast is a `CGameHelper::Explosion` of the ability weapon.
            if let (Some(sounds), Some(def)) =
                (sounds.as_deref_mut(), weapons.get(ability.weapon.weapon()))
            {
                sounds.explosion(def, event.target);
            }
        }
        commands.spawn(AreaDenialZone::new(
            ability.weapon,
            event.target,
            Some(event.attacker),
            team.0,
            *faction,
        ));
        commands.entity(event.attacker).insert(CommandFireCooldown {
            remaining: ability.cooldown,
        });
    }
}

/// pointer.bos `aimingSpecial`: the Pointer is busy with its NX shot.
/// While present, weapon 1 neither aims nor fires (`AimWeapon1`:
/// `if (aimingSpecial) return 0`). Removed with the unit's orders
/// ([`crate::interaction::clear_orders`]) and when the recovery ends.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
#[component(storage = "SparseSet")]
pub enum NxCast {
    /// Waiting for the cube to open and turn onto `target`.
    Aiming { target: Vec3 },
    /// `FireWeapon2`'s `sleep 2000` after the shot.
    Recovering { left: f32 },
}

/// pointer.bos `FireWeapon2`: `sleep 2000` (61 frames) before
/// `aimingSpecial` clears and weapon 1 may aim again.
const NX_WEAPON1_BLOCK: f32 = 61.0 / 30.0;

/// The NX Flag's denial zone (`areadenial.lua`: radius 120, 100 dps,
/// 60 s, hurts allies too), centred where the shell exploded and
/// credited to the Pointer that fired it.
pub fn nx_zone(
    center: Vec3,
    owner: Entity,
    owner_team: u8,
    owner_faction: Faction,
) -> AreaDenialZone {
    AreaDenialZone::new(
        DenialWeapon::Nx,
        center,
        Some(owner),
        owner_team,
        owner_faction,
    )
}

/// Drive pending NX shots. An aiming cast re-stamps the Pointer's
/// `AimTarget` each frame (`AimWeapon2`: body heading and gunbase pitch,
/// launch at 45° for `trajectoryheight=1`); once the cube is open and on
/// target the shell leaves the `gunpoint` as a real missile that explodes
/// with the weapon's damage and `system_nx` CEG where it lands and drops
/// the denial zone there, the 30 s reload starts, and the cast turns into
/// the script's 2 s recovery. A cast whose target has left the weapon's
/// range is dropped.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn tick_nx_casts(
    time: Res<Time>,
    mut casters: Query<
        (
            Entity,
            &mut NxCast,
            &GlobalTransform,
            &UnitType,
            &TeamId,
            &Faction,
            &NetId,
        ),
        // A paralyzed Pointer's script doesn't run its weapons.
        (Without<Dying>, Without<crate::units::combat::Stunned>),
    >,
    weapons: Res<WeaponRegistry>,
    unit_registry: Res<UnitRegistry>,
    pieces: crate::units::combat::PieceLookup,
    mut pending_attacks: ResMut<crate::units::weapon_fx::PendingAttacks>,
    mut sounds: Option<ResMut<crate::audio::SoundQueue>>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    let nx = WeaponKind::Nx;
    let Some(weapon_def) = weapons.get(nx) else {
        return;
    };
    let launch = crate::units::combat::AimLaunch::of(Some(weapon_def), pieces.map_gravity());
    for (entity, mut cast, gtf, unit, team, faction, _) in casters.iter_mut().sort::<&NetId>() {
        let target = match *cast {
            NxCast::Recovering { left } => {
                if left <= dt {
                    commands.entity(entity).remove::<NxCast>();
                } else {
                    *cast = NxCast::Recovering { left: left - dt };
                }
                continue;
            }
            NxCast::Aiming { target } => target,
        };
        if cast_reach(
            cast_limit(unit.0, &weapons),
            gtf.translation(),
            target,
            true,
            pieces.map_gravity(),
        ) != CastReach::Fire
        {
            commands
                .entity(entity)
                .remove::<(NxCast, crate::units::combat::AimTarget)>();
            continue;
        }
        // `AimWeapon2` while closed: nothing turns; the stop that follows
        // the approach walk opens the cube and the aim resumes.
        commands
            .entity(entity)
            .insert(crate::units::combat::AimTarget {
                pos: target,
                launch,
            });
        let open = pieces
            .animator
            .get(entity)
            .ok()
            .and_then(|a| a.driver.is_open())
            .unwrap_or(true);
        if !open || pieces.muzzle_buried(entity, gtf) {
            continue;
        }
        if !crate::units::combat::aim_gates_pass(entity, unit.0, gtf, target, launch, true, &pieces)
        {
            continue;
        }

        // FireWeapon2: `emit-sfx 1024 from gunpoint`, then the shell.
        let muzzle = pieces.muzzle(entity, gtf);
        let distance = muzzle.pos.distance(target);
        let muzzle_ceg = crate::units::assets::animation::fire_weapon_sfx(unit.0)
            .and_then(|index| unit_registry.sfx_type(unit.0, index))
            .and_then(CegKind::find);
        pending_attacks
            .events
            .push(crate::units::weapon_fx::AttackEvent {
                attacker_pos: muzzle.pos,
                target_pos: target,
                weapon: nx,
                muzzle_ceg,
                delayed_hit: Some(crate::units::weapon_fx::DelayedHitInfo {
                    target: None,
                    attacker: entity,
                    attacker_distance: distance,
                    muzzle_dir: muzzle.dir,
                    launch_dir: None,
                    on_impact: Some(crate::units::weapon_fx::ImpactEffect::NxZone {
                        owner_team: team.0,
                        owner_faction: *faction,
                    }),
                }),
                arc_from: None,
            });
        if let Some(sounds) = sounds.as_deref_mut() {
            sounds.weapon_fired(weapon_def, muzzle.pos);
        }
        let Some(ability) = ability_for(unit.0) else {
            continue;
        };
        *cast = NxCast::Recovering {
            left: NX_WEAPON1_BLOCK,
        };
        commands
            .entity(entity)
            .remove::<crate::units::combat::AimTarget>()
            .insert((
                CommandFireCooldown {
                    remaining: ability.cooldown,
                },
                crate::units::lifecycle::script_triggers::JustFired,
            ));
    }
}

/// [`tick_mine_launches`]' random stream (the bolts' `sprayangle`).
pub struct MineLaunchRng;
impl crate::sim::RngStream for MineLaunchRng {
    const SALT: u64 = 0x4D49_4E45;
}

/// byte.bos `LaunchMines` for every Byte with a [`MineLaunch`]: once the
/// Byte is open, pay the 6000 HP ([`minelauncher_cost`]) and fire a
/// MineLauncher bolt from each tube while Logic Bombs are left under the
/// cap — straight along the tube (`emit-sfx` aims one elmo ahead of the
/// piece), sprayed like `CLaserCannon::FireImpl`, flying its full range
/// until it explodes on the ground or a unit, where `Launcher.lua` puts
/// a Logic Bomb ([`ImpactEffect::LogicBomb`]). Then the 10 s reload
/// starts. Still folded: check again in half a second, while the
/// animation unfolds it. A Byte that can no longer pay gives up.
///
/// [`ImpactEffect::LogicBomb`]: crate::units::weapon_fx::ImpactEffect::LogicBomb
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn tick_mine_launches(
    time: Res<Time>,
    mut bytes: Query<
        (
            Entity,
            &mut MineLaunch,
            &GlobalTransform,
            &TeamId,
            &Faction,
            &Health,
            &NetId,
        ),
        (Without<Dying>, Without<crate::units::combat::Stunned>),
    >,
    weapons: Res<WeaponRegistry>,
    unit_registry: Res<UnitRegistry>,
    live_units: Query<(&UnitType, &TeamId), Without<Dying>>,
    pieces: crate::units::combat::PieceLookup,
    mut pending_attacks: ResMut<crate::units::weapon_fx::PendingAttacks>,
    mut delayed_damage: ResMut<DelayedDamage>,
    mut rng: ResMut<crate::sim::SimRng<MineLaunchRng>>,
    mut sounds: Option<ResMut<crate::audio::SoundQueue>>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    let launcher = WeaponKind::MineLauncher;
    let Some(weapon_def) = weapons.get(launcher) else {
        return;
    };
    let spray = weapon_def.spray_sin();
    let travel = weapon_def.laser_travel(weapon_def.range);
    let limit = unit_registry.team_limit(UnitKind::LogicBomb);
    let counts =
        crate::units::lifecycle::bookkeeping::team_kind_counts(UnitKind::LogicBomb, &live_units);
    for (entity, mut launch, gtf, team, faction, health, _) in bytes.iter_mut().sort::<&NetId>() {
        if launch.retry_in > dt {
            launch.retry_in -= dt;
            continue;
        }
        if !can_pay_for_mines(health) {
            commands.entity(entity).remove::<MineLaunch>();
            continue;
        }
        let Ok(animator) = pieces.animator.get(entity) else {
            commands.entity(entity).remove::<MineLaunch>();
            continue;
        };
        if animator.driver.is_open() == Some(false) {
            launch.retry_in = MINELAUNCHER_RETRY;
            continue;
        }
        let tubes: Vec<usize> = MINELAUNCHER_TUBES
            .iter()
            .filter_map(|name| animator.rig.piece(name))
            .collect();
        commands
            .entity(entity)
            .remove::<MineLaunch>()
            .insert(CommandFireCooldown {
                remaining: MINELAUNCHER_COOLDOWN,
            });
        delayed_damage.push_now(minelauncher_cost(entity));
        // `lua_GetLogicBombLeft`: `maxThisUnit - #GetTeamUnitsByDefs`.
        let count = counts.get(&team.0).copied().unwrap_or(0);
        let left = limit.map_or(usize::MAX, |l| l.saturating_sub(count) as usize);
        for &tube in tubes.iter().take(left) {
            let Some(muzzle) = pieces.piece_emit(entity, gtf, tube) else {
                continue;
            };
            let mut dir = muzzle.dir;
            if spray > 0.0 {
                dir = (dir + crate::rng::random_unit_sphere(&mut rng) * spray).normalize_or(dir);
            }
            if let Some(sounds) = sounds.as_deref_mut() {
                sounds.weapon_fired(weapon_def, muzzle.pos);
            }
            pending_attacks
                .events
                .push(crate::units::weapon_fx::AttackEvent {
                    attacker_pos: muzzle.pos,
                    target_pos: muzzle.pos + dir * travel,
                    weapon: launcher,
                    muzzle_ceg: None,
                    delayed_hit: Some(crate::units::weapon_fx::DelayedHitInfo {
                        target: None,
                        attacker: entity,
                        attacker_distance: travel,
                        muzzle_dir: dir,
                        launch_dir: None,
                        on_impact: Some(crate::units::weapon_fx::ImpactEffect::LogicBomb {
                            owner_team: team.0,
                            owner_faction: *faction,
                        }),
                    }),
                    arc_from: None,
                });
        }
    }
}

/// Whether `team` has no room left under the Logic Bomb `UnitRestricted`
/// cap `limit` ([`UnitRegistry::team_limit`]; `None` = uncapped),
/// counting live mines plus mines already queued this frame.
fn logic_bombs_at_cap(
    team: u8,
    limit: Option<u32>,
    live_units: &Query<(&UnitType, &TeamId), Without<Dying>>,
    mine_spawns: &MineSpawnQueue,
) -> bool {
    let Some(limit) = limit else {
        return false;
    };
    let live = crate::units::lifecycle::bookkeeping::team_kind_count(
        UnitKind::LogicBomb,
        team,
        live_units,
    );
    let queued = mine_spawns.0.iter().filter(|m| m.team == team).count() as u32;
    live + queued >= limit
}

/// byte.bos `LaunchMines`: `if((get LUA1)<(HITPOINT_COST)) return 0`,
/// plus [`MINELAUNCHER_HP_FLOOR`].
fn can_pay_for_mines(health: &Health) -> bool {
    health.current >= MINELAUNCHER_HP_COST + MINELAUNCHER_HP_FLOOR
}

/// byte.bos `lua_Damage(HITPOINT_COST)` → LuaCOB.lua `Damage`:
/// `Spring.AddUnitDamage(u, 6000, 0, -1, sigterm)` — no attacker, and
/// credited to the SIGTERM bomb.
fn minelauncher_cost(byte: Entity) -> Hit {
    Hit {
        target: byte,
        attacker: None,
        damage: MINELAUNCHER_HP_COST,
        weapon: Some(WeaponKind::SigTerm),
        paralyze_time: None,
        impulse: Vec3::ZERO,
    }
}

/// network_reflectorshield.lua: every unit of the caster's ally team
/// (`GetUnitAllyTeam(u) == ownTeam`) in the radius is protected.
fn apply_firewall(
    center: Vec3,
    caster_team: u8,
    alliances: &Alliances,
    targets: &Query<(Entity, &TeamId, &GlobalTransform), With<Health>>,
    commands: &mut Commands,
) {
    let radius_sq = FIREWALL_RADIUS * FIREWALL_RADIUS;
    for (entity, team, gtf) in targets.iter() {
        if !alliances.friendly(team.0, caster_team) {
            continue;
        }
        if gtf.translation().distance_squared(center) > radius_sq {
            continue;
        }
        commands.entity(entity).insert(Protected {
            remaining: FIREWALL_DURATION,
        });
    }
}

pub fn tick_protection(
    time: Res<Time>,
    mut query: Query<(Entity, &mut Protected)>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (entity, mut protected) in &mut query {
        protected.remaining -= dt;
        if protected.remaining <= 0.0 {
            commands.entity(entity).remove::<Protected>();
        }
    }
}

/// Tick the per-caster `CommandFireCooldown` down to zero, then
/// remove the component so the caster becomes eligible again.
pub fn tick_command_fire_cooldown(
    time: Res<Time>,
    mut query: Query<(Entity, &mut CommandFireCooldown)>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (entity, mut cd) in &mut query {
        cd.remaining -= dt;
        if cd.remaining <= 0.0 {
            commands.entity(entity).remove::<CommandFireCooldown>();
        }
    }
}

/// Tick every active `AreaDenialZone` (`areadenial.lua` `GameFrame`):
/// each unit within the radius takes `dps·dt` as an `AddUnitDamage`
/// hit — the flat amount for every armour class, but through the whole
/// damage pipeline ([`DelayedDamage`] → `DamageResolver::deliver`), so
/// the Firewall, `ARMORED`, the scripts and infection (Infection gas)
/// all apply. A zone whose owner died is handed to a homebase, else a
/// minifac, of the owner's team (upstream picks one at random; the
/// first found here), else credited to nobody.
#[allow(clippy::too_many_arguments)]
pub fn tick_area_denial(
    time: Res<Time>,
    mut zones: Query<(Entity, &mut AreaDenialZone)>,
    units: Query<(Entity, &UnitType, &TeamId, &NetId), Without<Dying>>,
    spatial: Res<SpatialIndex>,
    mut delayed: ResMut<DelayedDamage>,
    mut commands: Commands,
    alliances: Res<Alliances>,
) {
    let dt = time.delta_secs();
    for (zone_entity, mut zone) in &mut zones {
        zone.remaining -= dt;
        if zone.remaining <= 0.0 {
            commands.entity(zone_entity).despawn();
            continue;
        }
        if zone.owner.is_none_or(|owner| !units.contains(owner)) {
            zone.owner = substitute_owner(zone.owner_team, &units);
        }

        let params = zone.weapon.params();
        let (center, owner_team) = (zone.center, zone.owner_team);
        let attacker = zone.owner.map(|entity| Attacker {
            entity,
            team: owner_team,
            faction: zone.owner_faction,
        });
        let weapon = Some(zone.weapon.weapon());
        let damage = params.dps * dt;
        let radius_sq = params.radius * params.radius;
        spatial.query_radius(center, params.radius, |candidate| {
            // `GetUnitsInSphere`: units, not map features.
            if candidate.feature {
                return;
            }
            // areadenial.lua: `damageFriendly or not AreTeamsAllied`.
            if !params.damage_friendly && alliances.friendly(candidate.team, owner_team) {
                return;
            }
            // `GetUnitsInSphere`: the unit's position within the sphere.
            if candidate.pos.distance_squared(center) > radius_sq {
                return;
            }
            delayed.push_now(Hit {
                target: candidate.entity,
                attacker,
                damage,
                weapon,
                paralyze_time: None,
                impulse: Vec3::ZERO,
            });
        });
    }
}

/// `areadenial.lua`'s stand-in owner: a homebase of `team`, else a
/// minifac, else nobody.
fn substitute_owner(
    team: u8,
    units: &Query<(Entity, &UnitType, &TeamId, &NetId), Without<Dying>>,
) -> Option<Entity> {
    // Upstream picks `sel[math.random(1,#sel)]` among the team's
    // homebases/minifacs; the port instead takes the lowest-NetId one so
    // the choice is independent of ECS storage order (the shuffled-replay
    // fuzz). Raw `units.iter()` order changes between storage layouts.
    let of_team = |pick: fn(UnitKind) -> bool| {
        units
            .iter()
            .sort::<&NetId>()
            .find(|(_, unit, t, _)| t.0 == team && pick(unit.0))
            .map(|(entity, _, _, _)| entity)
    };
    of_team(UnitKind::is_homebase).or_else(|| of_team(UnitKind::is_minifac))
}

/// Fly every in-flight Signal bomber. On arrival, drop a
/// [`SigTermBomb`] at the release point and despawn the Signal.
/// Model orientation along `dir`, rolled by `bank`: S3O models point
/// their nose along +Z, Bevy's `looking_to` points −Z, hence the half
/// turn.
fn nose_along(dir: Vec3, bank: f32) -> Quat {
    let Some(dir) = dir.try_normalize() else {
        return Quat::IDENTITY;
    };
    // Straight up or down: keep the wings level to the side.
    let up = if dir.y.abs() > 0.999 {
        Vec3::Z
    } else {
        Vec3::Y
    };
    let look = Transform::IDENTITY.looking_to(dir, up).rotation;
    Quat::from_axis_angle(dir, -bank) * look * Quat::from_rotation_y(std::f32::consts::PI)
}

/// Whole sim frames in `dt` (one per fixed tick).
fn sim_frames(dt: f32) -> u32 {
    (dt * crate::sim::GAME_SPEED).round().det_max(0.0) as u32
}

impl SigTermSignal {
    /// Nose along the flight path, rolled into the turn; straight up
    /// while it rises off the pad (signal.bos `Create`: the nose-up
    /// launch pose).
    fn orientation(&self) -> Quat {
        let dir = if self.vel.length_squared() > 1e-6 {
            self.vel
        } else if self.taking_off {
            Vec3::Y
        } else {
            Vec3::new(self.heading.x, 0.0, self.heading.y)
        };
        nose_along(dir, self.bank)
    }

    /// One sim frame of flight (`CStrafeAirMoveType::UpdateTakeOff` /
    /// `UpdateFlying`, simplified to what a straight bombing run needs).
    /// Returns the bomb once it should drop.
    fn step(&mut self, ground: &impl Fn(Vec2) -> f32) -> Option<SigTermBomb> {
        self.frames += 1;
        let to_target = (self.target - self.pos).xz();
        let want = to_target.normalize_or(self.heading);
        // Signed angle from the heading to the target.
        let turn_to = bevy::math::ops::atan2(self.heading.perp_dot(want), self.heading.dot(want));
        let height = self.pos.y - ground(self.pos.xz());

        let turn;
        if self.taking_off {
            // Straight up (the same thrust and drag), swinging toward the
            // target as it climbs; forward thrust only once it is 40 %
            // of the way up.
            self.vel.y = (self.vel.y + SIGTERM_ACC) * (1.0 - SIGTERM_DRAG);
            turn = turn_to.det_clamp(
                -SIGTERM_TAKEOFF_RUDDER * self.vel.y,
                SIGTERM_TAKEOFF_RUDDER * self.vel.y,
            );
            if height > SIGTERM_CRUISE_HEIGHT * 0.4 {
                self.speed = (self.speed + SIGTERM_ACC) * (1.0 - SIGTERM_DRAG);
            }
            // Flying once high enough or fast enough (`UpdateTakeOff`:
            // `wantedHeight`, or 0.8 of `maxSpeed`).
            if height > SIGTERM_CRUISE_HEIGHT || self.speed >= 0.8 * 8.0 {
                self.taking_off = false;
            }
        } else {
            self.speed = (self.speed + SIGTERM_ACC) * (1.0 - SIGTERM_DRAG);
            let max_turn = self.speed / SIGTERM_TURN_RADIUS;
            // Turning toward a target too close to the side would only
            // circle it: coming out of the turn the plane must still have
            // a straight run of a bomb's forward travel to let go before
            // the target. That run is the tangent from the turn circle's
            // centre to the target; while it is too short (the target
            // inside the circle or just past it) the plane flies on
            // straight and comes around for another pass, like a bomber
            // that has overshot. It turns back in only with a fifth more
            // than it needs, so the tangent can't shrink below the run
            // mid-turn and stop it again.
            let side = if turn_to >= 0.0 { 1.0 } else { -1.0 };
            let centre = self.pos.xz() + self.heading.perp() * side * SIGTERM_TURN_RADIUS;
            let fall = (self.pos.y - ground(self.target.xz())).det_max(0.0);
            let mut run = self.speed * (2.0 * fall / SIGTERM_BOMB_GRAVITY).sqrt();
            if self.extending {
                run *= 1.2;
            }
            self.extending = centre.distance_squared(self.target.xz())
                < SIGTERM_TURN_RADIUS * SIGTERM_TURN_RADIUS + run * run;
            turn = if self.extending {
                0.0
            } else {
                turn_to.det_clamp(-max_turn, max_turn)
            };
            // Hold the cruise height over the higher of the ground below
            // and the ground it reaches in 40 frames; climb and dive no
            // steeper than `maxPitch`.
            let ahead = self.pos.xz() + self.heading * self.speed * SIGTERM_LOOKAHEAD_FRAMES;
            let floor = ground(self.pos.xz()).det_max(ground(ahead));
            let max_climb = self.speed * bevy::math::ops::tan(SIGTERM_MAX_PITCH);
            let want_vy = ((floor + SIGTERM_CRUISE_HEIGHT - self.pos.y) * 0.1)
                .det_clamp(-max_climb, max_climb);
            self.vel.y += (want_vy - self.vel.y).det_clamp(-0.3, 0.3);
        }
        if turn != 0.0 {
            let (sin, cos) = (bevy::math::ops::sin(turn), bevy::math::ops::cos(turn));
            self.heading = Vec2::new(
                self.heading.x * cos - self.heading.y * sin,
                self.heading.x * sin + self.heading.y * cos,
            )
            .normalize_or(self.heading);
        }
        // Rolled into the turn in proportion to how hard it turns, once
        // well clear of the ground (`GetAileronDeflection`: 0.6 of the
        // wanted height).
        let max_turn = (self.speed / SIGTERM_TURN_RADIUS).det_max(1e-6);
        let want_bank = if height > SIGTERM_CRUISE_HEIGHT * 0.6 {
            (turn / max_turn).det_clamp(-1.0, 1.0) * SIGTERM_MAX_BANK
        } else {
            0.0
        };
        self.bank += (want_bank - self.bank).det_clamp(-SIGTERM_BANK_RATE, SIGTERM_BANK_RATE);
        self.vel = Vec3::new(
            self.heading.x * self.speed,
            self.vel.y,
            self.heading.y * self.speed,
        );
        self.pos += self.vel;
        self.pos.y = self.pos.y.det_max(ground(self.pos.xz()));

        // `CBombDropper::TestRange`: drop when a bomb let go now would
        // land within `salvoDelay · speed · 0.5` of the target — or at
        // the end of its 120 s (`Killed` still drops it).
        let expired = self.frames >= SIGTERM_MAX_FRAMES;
        if self.taking_off && !expired {
            return None;
        }
        let fall = (self.pos.y - ground(self.target.xz())).det_max(0.0);
        let fall_frames = (2.0 * fall / SIGTERM_BOMB_GRAVITY).sqrt();
        let landing = self.pos.xz() + self.vel.xz() * fall_frames;
        let drop_dist = SIGTERM_SALVO_FRAMES * self.speed * 0.5;
        if !expired && landing.distance(self.target.xz()) > drop_dist {
            return None;
        }
        // `FireImpl`: the bomb starts with the plane's velocity, nudged
        // toward the target by at most one elmo per frame.
        let nudge = if fall_frames > 0.0 {
            ((self.target.xz() - landing) / fall_frames).clamp_length_max(1.0)
        } else {
            Vec2::ZERO
        };
        Some(SigTermBomb {
            pos: self.pos - Vec3::Y * 6.0,
            vel: self.vel + Vec3::new(nudge.x, 0.0, nudge.y),
            owner_team: self.owner_team,
            owner_faction: self.owner_faction,
        })
    }
}

/// The ground height under an XZ point (flat 0 without a heightmap).
fn ground_at(hm: Option<&Heightmap>) -> impl Fn(Vec2) -> f32 + '_ {
    move |p: Vec2| hm.map_or(0.0, |hm| hm.sample(p.x, p.y))
}

/// Fly every Signal; on the drop, spawn the bomb and break the Signal up
/// into a falling wreck.
#[allow(clippy::too_many_arguments)]
pub fn tick_sigterm_signals(
    time: Res<Time>,
    mut signals: Query<(Entity, &mut SigTermSignal, &mut Transform)>,
    heightmap: Option<Res<Heightmap>>,
    mut visuals: SigTermVisuals,
    mut commands: Commands,
) {
    let frames = sim_frames(time.delta_secs());
    let ground = ground_at(heightmap.as_deref());
    for (entity, mut signal, mut transform) in &mut signals {
        let mut dropped = None;
        for _ in 0..frames {
            if let Some(bomb) = signal.step(&ground) {
                dropped = Some(bomb);
                break;
            }
        }
        transform.translation = signal.pos;
        transform.rotation = signal.orientation();
        let Some(bomb) = dropped else {
            continue;
        };
        let (bomb_mesh, bomb_material) = visuals.bomb(signal.owner_team, signal.owner_faction);
        commands.spawn((
            Transform::from_translation(bomb.pos).with_rotation(nose_along(bomb.vel, 0.0)),
            bomb,
            Mesh3d(bomb_mesh),
            MeshMaterial3d(bomb_material),
            Visibility::default(),
        ));
        // The airframe tumbles down from where it let go.
        commands
            .entity(entity)
            .remove::<SigTermSignal>()
            .insert(SigTermWreck {
                vel: signal.vel,
                spin: Vec3::new(0.11, 0.03, 0.07),
            });
    }
}

/// The broken-up Signal falls, tumbling, and vanishes on the ground.
pub fn tick_sigterm_wrecks(
    time: Res<Time>,
    mut wrecks: Query<(Entity, &mut SigTermWreck, &mut Transform)>,
    heightmap: Option<Res<Heightmap>>,
    mut commands: Commands,
) {
    let frames = sim_frames(time.delta_secs());
    let ground = ground_at(heightmap.as_deref());
    for (entity, mut wreck, mut transform) in &mut wrecks {
        for _ in 0..frames {
            wreck.vel.y -= SIGTERM_WRECK_GRAVITY;
            wreck.vel *= 0.99;
            transform.translation += wreck.vel;
            let spin = Quat::from_scaled_axis(wreck.spin);
            transform.rotation = spin * transform.rotation;
        }
        if transform.translation.y <= ground(transform.translation.xz()) {
            commands.entity(entity).despawn();
        }
    }
}

/// Fall + detonate every in-flight SIGTERM bomb.
///
/// Each frame the bomb falls under the `[SigTerm]` weapon's gravity,
/// nose along its path. Where it enters the ground the weapon explodes
/// — an ordinary [`PendingDamage`] explosion (450 elmo damage radius
/// from `areaofeffect=900`, `edgeeffectiveness=.8`, the armour table),
/// owned by nobody since the Signal destroyed itself on release — and
/// leaves its [`AreaDenialZone`] tail behind; the bomb then despawns.
pub fn tick_sigterm_bombs(
    time: Res<Time>,
    mut bombs: Query<(Entity, &mut SigTermBomb, &mut Transform)>,
    heightmap: Option<Res<Heightmap>>,
    weapons: Res<WeaponRegistry>,
    mut damage_queue: ResMut<DamageQueue>,
    mut pending_explosions: ResMut<PendingExplosions>,
    mut commands: Commands,
) {
    let frames = sim_frames(time.delta_secs());
    let hm = heightmap.as_deref();
    let ground = ground_at(hm);
    let sigterm = weapons.get(WeaponKind::SigTerm);
    for (entity, mut bomb, mut transform) in &mut bombs {
        let mut impact = None;
        for _ in 0..frames {
            let prev = bomb.pos;
            bomb.vel.y -= SIGTERM_BOMB_GRAVITY;
            let vel = bomb.vel;
            bomb.pos += vel;
            let floor = ground(bomb.pos.xz());
            impact = hm
                .and_then(|hm| hm.ground_hit(prev, bomb.pos))
                .or((bomb.pos.y <= floor).then(|| Vec3::new(bomb.pos.x, floor, bomb.pos.z)));
            if impact.is_some() {
                break;
            }
        }
        transform.translation = bomb.pos;
        transform.rotation = nose_along(bomb.vel, 0.0);
        let Some(at) = impact else {
            continue;
        };

        if sigterm.is_some() {
            damage_queue.push(PendingDamage::unowned_explosion(WeaponKind::SigTerm, at));
        }
        // The bomb detonates through upstream's
        // `explosiongenerator=custom:system_sigterm`
        // (retroweapons.tdf:597) — without this the blast is silent.
        pending_explosions.events.push(ExplosionEvent {
            pos: at,
            up: Vec3::Y,
            rgb: bomb.owner_faction.rgb_f32(),
            radius: sigterm.map_or(0.0, |def| def.area_of_effect),
            ceg: Some(CegKind::SystemSigterm),
            damage: sigterm.map_or(0.0, |def| def.damage.default),
        });
        // `areadenial.lua` `Explosion`: the Signal is dead, so the zone
        // goes to `LastSignalTeam` and the first tick finds it an owner.
        commands.spawn(AreaDenialZone::new(
            DenialWeapon::SigTerm,
            at,
            None,
            bomb.owner_team,
            bomb.owner_faction,
        ));
        commands.entity(entity).despawn();
    }
}

/// Definition of a unit's command-fire ability: the denial weapon it
/// drops ([`DenialWeapon::params`]) and the weapon's own `reloadtime`
/// for cooldown.
struct Ability {
    weapon: DenialWeapon,
    cooldown: f32,
    /// Upstream's `explosiongenerator=custom:…` for the impacting
    /// shell, replayed through [`PendingExplosions`] on cast. `None`
    /// where the weapon has no generator (Infection's gas has no CEG
    /// upstream).
    explosion_ceg: Option<CegKind>,
}

fn ability_for(kind: UnitKind) -> Option<Ability> {
    match kind {
        UnitKind::Pointer => Some(Ability {
            weapon: DenialWeapon::Nx,
            cooldown: 30.0,
            explosion_ceg: Some(CegKind::SystemNx),
        }),
        UnitKind::Obelisk => Some(Ability {
            weapon: DenialWeapon::Infection,
            cooldown: 40.0,
            explosion_ceg: None,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::test_support::InitSim;

    #[test]
    fn nx_flag_ability_matches_upstream() {
        let a = ability_for(UnitKind::Pointer).unwrap();
        assert_eq!(a.weapon, DenialWeapon::Nx);
        assert_eq!(a.cooldown, 30.0);
    }

    #[test]
    fn infection_ability_matches_upstream() {
        let a = ability_for(UnitKind::Obelisk).unwrap();
        assert_eq!(a.weapon, DenialWeapon::Infection);
        assert_eq!(a.cooldown, 40.0);
        // Upstream infection.lua: `infector[WeaponDefNames["infection"].id]
        // = 30` frames @ 30 fps → 1 s infection window per DPS tick.
        assert_eq!(WeaponKind::Infection.infection_duration(), Some(1.0));
    }

    /// `areadenial.lua` `weaponInfo`, ttl in sim frames.
    #[test]
    fn denial_params_match_areadenial_lua() {
        let p = DenialWeapon::SigTerm.params();
        assert_eq!((p.radius, p.dps, p.damage_friendly), (350.0, 2000.0, true));
        assert_eq!(p.ttl, 100.0 / 30.0);
        let p = DenialWeapon::Nx.params();
        assert_eq!((p.radius, p.dps, p.damage_friendly), (120.0, 100.0, true));
        assert_eq!(p.ttl, 60.0);
        let p = DenialWeapon::Infection.params();
        assert_eq!((p.radius, p.dps, p.damage_friendly), (400.0, 120.0, false));
        assert_eq!(p.ttl, 400.0 / 30.0);
    }

    /// Weapon abilities are limited by their weapon's reach — the TDF
    /// `range=` as a fallback when the registry lacks the weapon — and
    /// the two Lua abilities and the untargeted Launch Mines reach
    /// anywhere.
    #[test]
    fn cast_reach_classifies_orders() {
        let empty = WeaponRegistry::default();
        let reach = |kind, target: Vec3, mobile| {
            cast_reach(
                cast_limit(kind, &empty),
                Vec3::ZERO,
                target,
                mobile,
                crate::sim::MAP_GRAVITY_PER_FRAME2,
            )
        };
        let near = Vec3::new(500.0, 300.0, 0.0);
        let far = Vec3::new(2000.0, 0.0, 0.0);
        assert_eq!(reach(UnitKind::Pointer, near, false), CastReach::Fire);
        assert_eq!(reach(UnitKind::Pointer, far, true), CastReach::Approach);
        assert_eq!(reach(UnitKind::Pointer, far, false), CastReach::Refuse);
        // Launch Mines has no target to reach: the Byte fires in place.
        for kind in [UnitKind::Terminal, UnitKind::Firewall, UnitKind::Byte] {
            assert_eq!(reach(kind, far * 10.0, false), CastReach::Fire);
        }
    }

    /// The Obelisk's Infection is a ballistic Cannon: `TestRange` gives
    /// it about 1780 elmos on level ground, not its `range=2000`.
    #[test]
    fn infection_reach_follows_the_cannon_range() {
        let tdf = spring_tdf::Tdf::parse(
            "[Infection]\n{\nballistic=1;\nturret=1;\nrange=2000;\nweaponvelocity=800;\n\
             startvelocity=800;\nmyGravity=.4;\ncommandfire=1;\n}",
        )
        .unwrap();
        let mut weapons = WeaponRegistry::default();
        let def = spring_tdf::WeaponDefs::from_tdf(&tdf)
            .get("Infection")
            .cloned()
            .unwrap();
        weapons.insert_for_test(WeaponKind::Infection, def);
        let reach = |x: f32| {
            cast_reach(
                cast_limit(UnitKind::Obelisk, &weapons),
                Vec3::ZERO,
                Vec3::X * x,
                false,
                crate::sim::MAP_GRAVITY_PER_FRAME2,
            )
        };
        assert_eq!(reach(1700.0), CastReach::Fire);
        assert_eq!(reach(1900.0), CastReach::Refuse);
    }

    /// Terminal and Firewall start with a full recharge (upstream
    /// `UnitCreated`); weapon casters start loaded.
    #[test]
    fn lua_abilities_start_recharging() {
        assert!((LUA_ABILITY_RELOAD - 96.0).abs() < 1e-4);
        for kind in [UnitKind::Terminal, UnitKind::Firewall] {
            let cd = initial_cooldown(kind).expect("starts recharging");
            assert_eq!(cd.remaining, LUA_ABILITY_RELOAD);
        }
        for kind in [UnitKind::Pointer, UnitKind::Obelisk, UnitKind::Byte] {
            assert!(initial_cooldown(kind).is_none());
        }
    }

    fn cast_app(units: &[(&str, f32)]) -> App {
        let mut defs = spring_tdf::UnitDefs::default();
        for (name, velocity) in units {
            defs.units.insert(
                (*name).into(),
                spring_tdf::UnitDef {
                    max_velocity: *velocity,
                    ..Default::default()
                },
            );
        }
        let mut app = App::new();
        app.add_message::<CommandFireEvent>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<Assets<Image>>()
            .init_resource::<S3OModelCache>()
            .init_resource::<SigTermAssets>()
            .init_resource::<MineSpawnQueue>()
            .init_resource::<PendingExplosions>()
            .init_resource::<DelayedDamage>()
            .init_resource::<Alliances>()
            .insert_resource(UnitRegistry::for_test(defs))
            .insert_resource(WeaponRegistry::default());
        app
    }

    fn spawn_caster(app: &mut App, kind: UnitKind, pos: Vec3) -> Entity {
        app.world_mut()
            .spawn((
                UnitType(kind),
                TeamId(0),
                Faction::System,
                Health::full(15_000.0),
                GlobalTransform::from_translation(pos),
            ))
            .id()
    }

    fn cast(app: &mut App, attacker: Entity, target: Vec3) {
        use bevy::ecs::system::RunSystemOnce;
        app.world_mut()
            .write_message(CommandFireEvent { attacker, target });
        app.world_mut()
            .run_system_once(process_command_fire)
            .unwrap();
        // Each `run_system_once` builds a fresh reader that would replay
        // this event; drop it so later runs see only new casts.
        app.world_mut()
            .resource_mut::<Messages<CommandFireEvent>>()
            .clear();
    }

    fn zone_count(app: &mut App) -> usize {
        app.world_mut()
            .query::<&AreaDenialZone>()
            .iter(app.world())
            .count()
    }

    /// A stationary Obelisk refuses an Infection order beyond its range:
    /// no gas, no cooldown spent.
    #[test]
    fn stationary_caster_refuses_out_of_range() {
        let mut app = cast_app(&[]);
        let obelisk = spawn_caster(&mut app, UnitKind::Obelisk, Vec3::ZERO);
        cast(&mut app, obelisk, Vec3::new(2500.0, 0.0, 0.0));
        assert_eq!(zone_count(&mut app), 0);
        assert!(app.world().get::<CommandFireCooldown>(obelisk).is_none());

        cast(&mut app, obelisk, Vec3::new(1500.0, 0.0, 0.0));
        assert_eq!(zone_count(&mut app), 1);
        assert!(app.world().get::<CommandFireCooldown>(obelisk).is_some());
    }

    /// A move or stop order ends a pending NX shot like any other order
    /// (the engine drops the attack command), so weapon 1 is not held
    /// hostage by a cast the player abandoned.
    #[test]
    fn a_new_order_cancels_a_pending_nx_cast() {
        use bevy::ecs::system::RunSystemOnce;
        let mut app = App::new();
        let pointer = app
            .world_mut()
            .spawn(NxCast::Aiming { target: Vec3::X })
            .id();
        app.world_mut()
            .run_system_once(move |mut commands: Commands| {
                crate::interaction::clear_orders(&mut commands.entity(pointer));
            })
            .unwrap();
        assert!(app.world().get::<NxCast>(pointer).is_none());
    }

    /// A Pointer ordered beyond NX range walks toward the target instead
    /// of casting; once in range `advance_pending_casts` stops it and
    /// the shot is handed to the script side (`NxCast`): the zone only
    /// lands once the cube has opened, aimed and fired.
    #[test]
    fn mobile_caster_walks_into_range_then_casts() {
        use bevy::ecs::system::RunSystemOnce;
        let mut app = cast_app(&[("pointer", 2.0)]);
        let pointer = spawn_caster(&mut app, UnitKind::Pointer, Vec3::ZERO);
        let target = Vec3::new(3000.0, 0.0, 0.0);
        cast(&mut app, pointer, target);
        assert_eq!(zone_count(&mut app), 0);
        assert_eq!(
            app.world()
                .get::<PendingCommandFire>(pointer)
                .map(|p| p.target),
            Some(target)
        );
        assert_eq!(
            app.world().get::<MoveTarget>(pointer).map(|m| m.0),
            Some(target)
        );

        // Still out of range: the approach continues.
        app.world_mut()
            .run_system_once(advance_pending_casts)
            .unwrap();
        assert!(app.world().get::<PendingCommandFire>(pointer).is_some());

        // Walked on to 1300 elmos from the target — inside NX's 1400.
        app.world_mut()
            .entity_mut(pointer)
            .insert(GlobalTransform::from_translation(Vec3::new(
                1700.0, 0.0, 0.0,
            )));
        app.world_mut()
            .run_system_once(advance_pending_casts)
            .unwrap();
        app.world_mut()
            .run_system_once(process_command_fire)
            .unwrap();
        assert_eq!(zone_count(&mut app), 0);
        assert_eq!(
            app.world().get::<NxCast>(pointer).copied(),
            Some(NxCast::Aiming { target })
        );
        assert!(app.world().get::<PendingCommandFire>(pointer).is_none());
        // The attack command replaces the approach walk: the caster
        // stands and aims where it is.
        assert!(app.world().get::<MoveTarget>(pointer).is_none());
        assert!(app.world().get::<MovePath>(pointer).is_none());
    }

    /// An approach whose move order vanished (new order, stuck) is
    /// abandoned rather than casting later out of the blue.
    #[test]
    fn pending_cast_dropped_when_walk_ends() {
        use bevy::ecs::system::RunSystemOnce;
        let mut app = cast_app(&[("pointer", 2.0)]);
        let pointer = spawn_caster(&mut app, UnitKind::Pointer, Vec3::ZERO);
        app.world_mut()
            .entity_mut(pointer)
            .insert(PendingCommandFire {
                target: Vec3::new(5000.0, 0.0, 0.0),
            });
        app.world_mut()
            .run_system_once(advance_pending_casts)
            .unwrap();
        assert!(app.world().get::<PendingCommandFire>(pointer).is_none());
    }

    /// SIGTERM stays map-wide: a Terminal casts at any distance.
    #[test]
    fn sigterm_has_no_range_limit() {
        let mut app = cast_app(&[]);
        let terminal = spawn_caster(&mut app, UnitKind::Terminal, Vec3::ZERO);
        cast(&mut app, terminal, Vec3::new(20_000.0, 0.0, 0.0));
        let signals = app
            .world_mut()
            .query::<&SigTermSignal>()
            .iter(app.world())
            .count();
        assert_eq!(signals, 1);
    }

    #[test]
    fn units_without_abilities_return_none() {
        assert!(ability_for(UnitKind::Bit).is_none());
        assert!(ability_for(UnitKind::Kernel).is_none());
    }

    /// Launch Mines takes no target: wherever the click (or `D`) was, a
    /// Byte that can launch is marked to launch once it has unfolded —
    /// the HP and the reload are only spent when the mines fly
    /// ([`tick_mine_launches`]).
    #[test]
    fn minelauncher_cast_marks_the_byte_to_launch() {
        let mut app = cast_app(&[]);
        let byte = spawn_caster(&mut app, UnitKind::Byte, Vec3::ZERO);
        cast(&mut app, byte, Vec3::new(20_000.0, 0.0, -9_000.0));
        assert_eq!(
            app.world().get::<MineLaunch>(byte),
            Some(&MineLaunch { retry_in: 0.0 })
        );
        assert!(app.world().get::<PendingCommandFire>(byte).is_none());
        assert!(app.world().get::<CommandFireCooldown>(byte).is_none());
        assert!(app.world().resource::<DelayedDamage>().is_empty());
    }

    /// byte.bos `LaunchMines` returns at once while the Byte is still
    /// `ARMORED` from its build, or can't pay the 6000 HP.
    #[test]
    fn minelauncher_refuses_while_armored_or_low_on_hp() {
        let mut app = cast_app(&[]);
        let byte = spawn_caster(&mut app, UnitKind::Byte, Vec3::ZERO);
        app.world_mut()
            .entity_mut(byte)
            .insert(Armored { after_build: 6.0 });
        cast(&mut app, byte, Vec3::ZERO);
        assert!(app.world().get::<MineLaunch>(byte).is_none());

        app.world_mut().entity_mut(byte).remove::<Armored>();
        app.world_mut().entity_mut(byte).insert(Health {
            current: MINELAUNCHER_HP_COST + MINELAUNCHER_HP_FLOOR - 1.0,
            max: 15_000.0,
        });
        cast(&mut app, byte, Vec3::ZERO);
        assert!(app.world().get::<MineLaunch>(byte).is_none());
    }

    /// Mines already on the map plus mines queued this frame count
    /// against the team's Logic Bomb cap; other teams don't.
    #[test]
    fn logic_bomb_cap_counts_live_and_queued_mines() {
        use bevy::ecs::system::RunSystemOnce;
        // logic_bomb.fbi `UnitRestricted=64`.
        let limit = 64;
        let mut app = App::new();
        for _ in 0..limit - 1 {
            app.world_mut()
                .spawn((UnitType(UnitKind::LogicBomb), TeamId(0)));
        }
        let mut queue = MineSpawnQueue::default();
        let check = move |queue: MineSpawnQueue| {
            move |live: Query<(&UnitType, &TeamId), Without<Dying>>| {
                (
                    logic_bombs_at_cap(0, Some(limit), &live, &queue),
                    logic_bombs_at_cap(1, Some(limit), &live, &queue),
                )
            }
        };
        let (own, other) = app
            .world_mut()
            .run_system_once(check(queue.clone()))
            .unwrap();
        assert!(!own, "one slot left");
        assert!(!other);

        queue.push(MineSpawn {
            position: Vec3::ZERO,
            faction: Faction::System,
            team: 0,
        });
        let (own, other) = app.world_mut().run_system_once(check(queue)).unwrap();
        assert!(own, "the queued mine fills the last slot");
        assert!(!other);
    }

    /// One sim frame of every SIGTERM stage.
    fn sigterm_frame(app: &mut App) {
        use bevy::ecs::system::RunSystemOnce;
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(1.0 / 30.0));
        app.world_mut()
            .run_system_once(tick_sigterm_signals)
            .unwrap();
        app.world_mut().run_system_once(tick_sigterm_bombs).unwrap();
        app.world_mut()
            .run_system_once(tick_sigterm_wrecks)
            .unwrap();
    }

    fn sigterm_app() -> App {
        let (weapons, _) = sigterm_weapons();
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_sim()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<Assets<Image>>()
            .init_resource::<S3OModelCache>()
            .init_resource::<SigTermAssets>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingExplosions>()
            .insert_resource(weapons);
        app
    }

    /// The whole airstrike over flat ground: the Signal rises straight
    /// off the Terminal (facing away from the target), turns toward it,
    /// cruises at its wanted height, and lets the bomb go before the
    /// target — which then lands on it.
    #[test]
    fn sigterm_takes_off_flies_and_bombs_the_target() {
        let mut app = sigterm_app();
        let target = Vec3::new(1600.0, 0.0, 400.0);
        let signal = app
            .world_mut()
            .spawn((
                SigTermSignal {
                    target,
                    pos: Vec3::ZERO,
                    vel: Vec3::ZERO,
                    // Facing away from the target.
                    heading: Vec2::new(-1.0, 0.0),
                    speed: 0.0,
                    bank: 0.0,
                    taking_off: true,
                    extending: false,
                    frames: 0,
                    owner_team: 0,
                    owner_faction: Faction::System,
                },
                Transform::default(),
            ))
            .id();

        // The first frames: straight up.
        for _ in 0..10 {
            sigterm_frame(&mut app);
        }
        let s = app.world().get::<SigTermSignal>(signal).unwrap().clone();
        assert!(s.pos.y > 40.0, "rising: {}", s.pos);
        assert!(s.pos.xz().length() < 1.0, "straight up: {}", s.pos);

        let mut top = 0.0f32;
        let mut released_at = None;
        for frame in 0..40 * 30 {
            if let Some(s) = app.world().get::<SigTermSignal>(signal) {
                top = top.det_max(s.pos.y);
                released_at = Some((frame, s.pos));
            }
            sigterm_frame(&mut app);
            if !app.world().resource::<DamageQueue>().is_empty() {
                break;
            }
        }
        assert!(
            (top - SIGTERM_CRUISE_HEIGHT).abs() < 40.0,
            "cruises near {SIGTERM_CRUISE_HEIGHT}: topped out at {top}"
        );
        let (_, release) = released_at.unwrap();
        let blasts: Vec<_> = app
            .world()
            .resource::<DamageQueue>()
            .iter_snapshot_for_test()
            .cloned()
            .collect();
        assert_eq!(blasts.len(), 1, "one bomb, one blast");
        let hit = blasts[0].impact_pos;
        assert!(
            hit.xz().distance(target.xz()) < 80.0,
            "lands on the target: {hit} vs {target}"
        );
        assert!(
            release.xz().distance(target.xz()) > 150.0,
            "let go well before the target, not above it: {release}"
        );
        // The airframe broke up when it let go, and fell away.
        assert!(app.world().get::<SigTermSignal>(signal).is_none());
    }

    /// A target close beside, ahead of or behind the Terminal — inside
    /// the Signal's turning circle — is bombed after one go-around, not
    /// circled until the Signal expires.
    #[test]
    fn sigterm_comes_around_for_a_close_target() {
        for (target, heading) in [
            (Vec3::new(450.0, 0.0, 150.0), Vec2::Y),
            (Vec3::new(450.0, 0.0, 150.0), Vec2::NEG_Y),
            (Vec3::new(200.0, 0.0, 0.0), Vec2::X),
            (Vec3::new(0.0, 0.0, 300.0), Vec2::NEG_Y),
        ] {
            let mut app = sigterm_app();
            app.world_mut().spawn((
                SigTermSignal {
                    target,
                    pos: Vec3::ZERO,
                    vel: Vec3::ZERO,
                    heading,
                    speed: 0.0,
                    bank: 0.0,
                    taking_off: true,
                    extending: false,
                    frames: 0,
                    owner_team: 0,
                    owner_faction: Faction::System,
                },
                Transform::default(),
            ));
            let mut frames = 0;
            while app.world().resource::<DamageQueue>().is_empty() {
                sigterm_frame(&mut app);
                frames += 1;
                assert!(frames < 30 * 30, "{target}: still circling");
            }
            let hit = app
                .world()
                .resource::<DamageQueue>()
                .iter_snapshot_for_test()
                .next()
                .unwrap()
                .impact_pos;
            assert!(
                hit.xz().distance(target.xz()) < 80.0,
                "{target}: lands on the target: {hit}"
            );
        }
    }

    /// The bomb keeps the plane's speed and falls under `myGravity=.3`:
    /// from 300 elmos up at 6.8 elmos/frame it lands about 45 frames and
    /// 300 elmos later (Spring's `CBombDropper` arc), not straight down.
    #[test]
    fn sigterm_bomb_falls_on_a_forward_arc() {
        let mut app = sigterm_app();
        let bomb = app
            .world_mut()
            .spawn((
                SigTermBomb {
                    pos: Vec3::new(0.0, 300.0, 0.0),
                    vel: Vec3::new(6.8, 0.0, 0.0),
                    owner_team: 0,
                    owner_faction: Faction::System,
                },
                Transform::default(),
            ))
            .id();
        for _ in 0..20 {
            sigterm_frame(&mut app);
        }
        // 20 frames: 136 elmos on, 63 down (0.3·20·21/2).
        let pos = app.world().get::<SigTermBomb>(bomb).unwrap().pos;
        assert!((pos.x - 136.0).abs() < 0.01, "{pos}");
        assert!((pos.y - (300.0 - 63.0)).abs() < 0.01, "{pos}");
        let mut frames = 20;
        while app.world().get::<SigTermBomb>(bomb).is_some() {
            sigterm_frame(&mut app);
            frames += 1;
            assert!(frames < 100);
        }
        assert!((44..=46).contains(&frames), "{frames} frames");
        let hit = app
            .world()
            .resource::<DamageQueue>()
            .iter_snapshot_for_test()
            .next()
            .unwrap()
            .impact_pos;
        assert!((hit.x - 306.0).abs() < 8.0 && hit.y.abs() < 1e-3, "{hit}");
    }

    fn sigterm_weapons() -> (WeaponRegistry, WeaponKind) {
        let mut weapons = WeaponRegistry::default();
        let id = weapons.insert_for_test(
            WeaponKind::SigTerm,
            spring_tdf::WeaponDef {
                damage: spring_tdf::DamageMap {
                    default: 10_000.0,
                    ..Default::default()
                },
                area_of_effect: 900.0,
                edge_effectiveness: 0.8,
                ..Default::default()
            },
        );
        (weapons, id)
    }

    /// On detonation the bomb entity despawns, the `[SigTerm]` weapon
    /// explodes at the impact site as an unowned [`PendingDamage`] (the
    /// damage pipeline resolves its 450 elmo blast), and the
    /// `AreaDenialZone` tail appears, owned by nobody until its first
    /// tick finds the team a stand-in.
    #[test]
    fn sigterm_bomb_explodes_and_spawns_denial_zone_on_detonation() {
        use bevy::ecs::system::RunSystemOnce;

        let (weapons, sigterm) = sigterm_weapons();
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_sim()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingExplosions>()
            .insert_resource(weapons);

        let target = Vec3::new(10.0, 0.0, 20.0);
        // Falling straight onto the target, one frame from the ground.
        app.world_mut().spawn((
            SigTermBomb {
                pos: target + Vec3::Y * 4.0,
                vel: Vec3::new(0.0, -8.0, 0.0),
                owner_team: 1,
                owner_faction: Faction::System,
            },
            Transform::from_translation(target),
        ));

        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(1.0 / 30.0));
        app.world_mut().run_system_once(tick_sigterm_bombs).unwrap();

        // Bomb is gone, zone is present.
        let bombs_left = app
            .world_mut()
            .query::<&SigTermBomb>()
            .iter(app.world())
            .count();
        assert_eq!(bombs_left, 0, "bomb should have despawned after detonate");

        let blasts: Vec<_> = app
            .world()
            .resource::<DamageQueue>()
            .iter_snapshot_for_test()
            .cloned()
            .collect();
        assert_eq!(blasts.len(), 1);
        assert_eq!(blasts[0].weapon, sigterm);
        assert_eq!(blasts[0].target, None);
        assert_eq!(blasts[0].impact_pos, target);

        let mut zones_q = app.world_mut().query::<&AreaDenialZone>();
        let zones: Vec<_> = zones_q.iter(app.world()).cloned().collect();
        assert_eq!(zones.len(), 1);
        assert_eq!(zones[0].weapon, DenialWeapon::SigTerm);
        assert_eq!(zones[0].remaining, DenialWeapon::SigTerm.params().ttl);
        assert_eq!(zones[0].owner, None);
        assert_eq!(zones[0].owner_team, 1);

        // The detonation replays upstream's `custom:system_sigterm`
        // explosion generator (retroweapons.tdf:597).
        let explosions = app.world().resource::<PendingExplosions>();
        assert_eq!(explosions.events.len(), 1, "detonation plays a CEG");
        assert_eq!(explosions.events[0].ceg, Some(CegKind::SystemSigterm));
        assert_eq!(explosions.events[0].pos, target);
        assert_eq!(explosions.events[0].radius, 900.0);
    }

    /// World for running zones through the real damage pipeline.
    fn zone_app(weapons: WeaponRegistry) -> App {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_sim()
            .init_resource::<DamageQueue>()
            .init_resource::<DelayedDamage>()
            .init_resource::<SpatialIndex>()
            .init_resource::<crate::units::teams::Alliances>()
            .init_resource::<crate::interaction::ground_move::PendingImpulses>()
            .insert_resource(UnitRegistry::empty())
            .insert_resource(weapons);
        app
    }

    fn zone_unit(app: &mut App, kind: UnitKind, team: u8, pos: Vec3) -> Entity {
        use crate::units::spatial::SpatialEntry;
        let entity = app
            .world_mut()
            .spawn((
                UnitType(kind),
                TeamId(team),
                Faction::Hacker,
                GlobalTransform::from_translation(pos),
                Health::full(1000.0),
            ))
            .id();
        app.world_mut()
            .resource_mut::<SpatialIndex>()
            .insert_for_test(SpatialEntry {
                entity,
                pos,
                hit_radius: 0.0,
                mid_y: 0.0,
                up: Vec3::Y,
                team,
                kind,
                hp_positive: true,
                is_flying: false,
                cloaked: false,
                detected_by: 0,
                feature: false,
            });
        entity
    }

    /// Why: zone ticks used to subtract HP directly, skipping the
    /// Firewall, the idle-heal reset and infection's rules (they
    /// infected enemy Viruses). As `AddUnitDamage` hits they go through
    /// `DamageResolver::deliver`: an Infection-gas tick halves on a
    /// protected unit, infects the enemy for the Obelisk's team but
    /// never a Virus, and spares allies (`damageFriendly=false`).
    #[test]
    fn denial_ticks_go_through_the_damage_pipeline() {
        use bevy::ecs::system::RunSystemOnce;
        let mut weapons = WeaponRegistry::default();
        weapons.insert_for_test(WeaponKind::Infection, spring_tdf::WeaponDef::default());
        let mut app = zone_app(weapons);
        let obelisk = zone_unit(&mut app, UnitKind::Obelisk, 1, Vec3::X * 1000.0);
        let enemy = zone_unit(&mut app, UnitKind::Bit, 0, Vec3::X * 10.0);
        let shielded = zone_unit(&mut app, UnitKind::Bit, 0, Vec3::X * -10.0);
        let virus = zone_unit(&mut app, UnitKind::Virus, 0, Vec3::Z * 10.0);
        let ally = zone_unit(&mut app, UnitKind::Bit, 1, Vec3::Z * -10.0);
        app.world_mut()
            .entity_mut(shielded)
            .insert(Protected { remaining: 20.0 });
        app.world_mut().spawn(AreaDenialZone::new(
            DenialWeapon::Infection,
            Vec3::ZERO,
            Some(obelisk),
            1,
            Faction::Hacker,
        ));

        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(1));
        app.world_mut().run_system_once(tick_area_denial).unwrap();
        app.world_mut()
            .run_system_once(crate::units::combat::apply_damage)
            .unwrap();

        let hp = |app: &App, e: Entity| app.world().get::<Health>(e).unwrap().current;
        assert!((hp(&app, enemy) - 880.0).abs() < 1e-3);
        assert!((hp(&app, shielded) - 940.0).abs() < 1e-3, "Firewall halves");
        assert!((hp(&app, ally) - 1000.0).abs() < 1e-3, "gas spares allies");
        let infected = app
            .world()
            .get::<crate::units::combat::Infected>(enemy)
            .expect("gas infects");
        assert_eq!(infected.attacker_team, 1);
        assert!(
            app.world()
                .get::<crate::units::combat::Infected>(virus)
                .is_none()
        );
        // Reflected onto the Obelisk next frame.
        assert!(!app.world().resource::<DelayedDamage>().is_empty());
    }

    /// `areadenial.lua`: once the owner is gone the zone is credited to
    /// a homebase of the team (else a minifac, else nobody).
    #[test]
    fn orphaned_zone_finds_a_stand_in_owner() {
        use bevy::ecs::system::RunSystemOnce;
        let mut app = zone_app(WeaponRegistry::default());
        let socket = zone_unit(&mut app, UnitKind::Socket, 1, Vec3::X * 2000.0);
        let zone = app
            .world_mut()
            .spawn(AreaDenialZone::new(
                DenialWeapon::SigTerm,
                Vec3::ZERO,
                None,
                1,
                Faction::System,
            ))
            .id();
        let tick = |app: &mut App| {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_millis(33));
            app.world_mut().run_system_once(tick_area_denial).unwrap();
            app.world().get::<AreaDenialZone>(zone).unwrap().owner
        };
        assert_eq!(tick(&mut app), Some(socket), "minifac when no homebase");
        let kernel = zone_unit(&mut app, UnitKind::Kernel, 1, Vec3::Z * 2000.0);
        assert_eq!(tick(&mut app), Some(socket), "a live stand-in is kept");
        app.world_mut()
            .entity_mut(socket)
            .insert(Dying { timer: 1.0 });
        assert_eq!(tick(&mut app), Some(kernel));
    }
}
