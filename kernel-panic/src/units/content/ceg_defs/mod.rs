//! The CEG definitions, one module per
//! `assets/defs/explosions/*.tdf`, reached through
//! [`CegKind::def`](super::cegs::CegKind::def) and [`def`].
//!
//! Generated as a whole by the ignored `gen_ceg_defs` test in
//! `super::ceg_def`; see one module's header for what that means.

use super::ceg_def::ExplosionDef;
use super::cegs::CegKind;

pub mod corruption_bctrail;
pub mod corruption_build;
pub mod corruption_builder;
pub mod corruption_burst;
pub mod corruption_charge;
pub mod corruption_const;
pub mod corruption_dust;
pub mod corruption_infection;
pub mod corruption_shot1;
pub mod corruption_stun;
pub mod corruption_trojan_rainbow;
pub mod corruption_wmdtrail;
pub mod corruption_worm_splash;
pub mod corruption_worm_trail;
pub mod experiment_trails;
pub mod linkbeam;
pub mod mine;
pub mod mineclearer;
pub mod minelauncher;
pub mod network_build;
pub mod network_charge;
pub mod network_dissolve;
pub mod network_flowmissile;
pub mod network_flowtrail;
pub mod network_shot;
pub mod none;
pub mod oldskool;
pub mod oldskool_big;
pub mod oldskool_build;
pub mod oldskool_death;
pub mod oldskool_impact;
pub mod oldskool_shot1;
pub mod oldskool_shot2;
pub mod oldskool_vbig;
pub mod rps_fountains;
pub mod system_nx;
pub mod system_sigterm;
pub mod touhou;

/// The static behind each variant. Written with the modules, so a
/// renamed or added section lands here without a hand edit, and
/// exhaustive, so a variant without one does not compile.
pub const fn def(kind: CegKind) -> &'static ExplosionDef {
    match kind {
        CegKind::CorruptionBcTrail => &corruption_bctrail::CORRUPTION_BCTRAIL,
        CegKind::CorruptionBuilder => &corruption_builder::CORRUPTION_BUILDER,
        CegKind::CorruptionBuild => &corruption_build::CORRUPTION_BUILD,
        CegKind::CorruptionBurst => &corruption_burst::CORRUPTION_BURST,
        CegKind::CorruptionCharge => &corruption_charge::CORRUPTION_CHARGE,
        CegKind::CorruptionConst => &corruption_const::CORRUPTION_CONST,
        CegKind::CorruptionDust => &corruption_dust::CORRUPTION_DUST,
        CegKind::CorruptionInfection => &corruption_infection::CORRUPTION_INFECTION,
        CegKind::CorruptionInfectionSmoke => &corruption_infection::CORRUPTION_INFECTION_SMOKE,
        CegKind::CorruptionShot1 => &corruption_shot1::CORRUPTION_SHOT1,
        CegKind::CorruptionStun => &corruption_stun::CORRUPTION_STUN,
        CegKind::CorruptionTrojanRainbow => &corruption_trojan_rainbow::CORRUPTION_TROJAN_RAINBOW,
        CegKind::CorruptionWmdTrail => &corruption_wmdtrail::CORRUPTION_WMDTRAIL,
        CegKind::CorruptionWormSplash => &corruption_worm_splash::CORRUPTION_WORM_SPLASH,
        CegKind::CorruptionWormTrail => &corruption_worm_trail::CORRUPTION_WORM_TRAIL,
        CegKind::ExperimentTrailM => &experiment_trails::EXPERIMENT_TRAILM,
        CegKind::ExperimentTrailS => &experiment_trails::EXPERIMENT_TRAILS,
        CegKind::Linkbeam => &linkbeam::LINKBEAM,
        CegKind::Mineclearer => &mineclearer::MINECLEARER,
        CegKind::Minelauncher => &minelauncher::MINELAUNCHER,
        CegKind::Mine => &mine::MINE,
        CegKind::NetworkBuild => &network_build::NETWORK_BUILD,
        CegKind::NetworkBuild1 => &network_build::NETWORK_BUILD1,
        CegKind::NetworkCharge => &network_charge::NETWORK_CHARGE,
        CegKind::NetworkDissolve => &network_dissolve::NETWORK_DISSOLVE,
        CegKind::NetworkSpawn => &network_dissolve::NETWORK_SPAWN,
        CegKind::NetworkFlowmissile => &network_flowmissile::NETWORK_FLOWMISSILE,
        CegKind::NetworkFlowtrail => &network_flowtrail::NETWORK_FLOWTRAIL,
        CegKind::NetworkShot => &network_shot::NETWORK_SHOT,
        CegKind::NoEffect => &none::NONE,
        CegKind::OldskoolBig => &oldskool_big::OLDSKOOL_BIG,
        CegKind::OldskoolBuild => &oldskool_build::OLDSKOOL_BUILD,
        CegKind::OldskoolDeath => &oldskool_death::OLDSKOOL_DEATH,
        CegKind::OldskoolImpact => &oldskool_impact::OLDSKOOL_IMPACT,
        CegKind::OldskoolShot1 => &oldskool_shot1::OLDSKOOL_SHOT1,
        CegKind::OldskoolShot2 => &oldskool_shot2::OLDSKOOL_SHOT2,
        CegKind::Oldskool => &oldskool::OLDSKOOL,
        CegKind::OldskoolVBig => &oldskool_vbig::OLDSKOOL_VBIG,
        CegKind::RockFountain => &rps_fountains::ROCK_FOUNTAIN,
        CegKind::PaperFountain => &rps_fountains::PAPER_FOUNTAIN,
        CegKind::ScissorsFountain => &rps_fountains::SCISSORS_FOUNTAIN,
        CegKind::SystemNx => &system_nx::SYSTEM_NX,
        CegKind::SystemNxFire => &system_nx::SYSTEM_NX_FIRE,
        CegKind::SystemSigterm => &system_sigterm::SYSTEM_SIGTERM,
        CegKind::SystemSigtermFire => &system_sigterm::SYSTEM_SIGTERM_FIRE,
        CegKind::TouhouBullet => &touhou::TOUHOU_BULLET,
        CegKind::TouhouSpark => &touhou::TOUHOU_SPARK,
        CegKind::TouhouBuildFx => &touhou::TOUHOU_BUILDFX,
        CegKind::TouhouIncant => &touhou::TOUHOU_INCANT,
        CegKind::TouhouBubbletrail => &touhou::TOUHOU_BUBBLETRAIL,
    }
}
