//! What the AI's factories build: the homebase production mix
//! (upstream `KPAI.lua::OrderHomeBase`) under the Fair-KPAI budget
//! (`KPAI_Fair.lua::UpdateFairness` → `Lack`), and the minifac spam
//! toggle (`KPAI_Fair.lua::OrderMiniFac`).

use crate::units::content::definitions::UnitKind;

/// What one homebase can build, split into the four roles KPAI's
/// `OrderHomeBase` chooses between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Roster {
    pub constructor: UnitKind,
    pub spam: UnitKind,
    pub arty: UnitKind,
    pub heavy: UnitKind,
}

/// Upstream `OrderHomeBase` role table: kernel → assembler / bit /
/// pointer / byte, hole → trojan / bug / dos / worm, carrier → gateway
/// / packet / flow / connection.
pub fn homebase_roster(homebase: UnitKind) -> Option<Roster> {
    let (constructor, spam, arty, heavy) = match homebase {
        UnitKind::Kernel => (
            UnitKind::Assembler,
            UnitKind::Bit,
            UnitKind::Pointer,
            UnitKind::Byte,
        ),
        UnitKind::Hole => (
            UnitKind::Trojan,
            UnitKind::Bug,
            UnitKind::Dos,
            UnitKind::Worm,
        ),
        UnitKind::Carrier => (
            UnitKind::Gateway,
            UnitKind::Packet,
            UnitKind::Flow,
            UnitKind::Connection,
        ),
        _ => return None,
    };
    Some(Roster {
        constructor,
        spam,
        arty,
        heavy,
    })
}

/// The swarm unit a minifac spams (upstream `AddMiniFac` /
/// `kp_autospam.lua`). Ports are teleporters: they tick the packet
/// buffer instead of producing, so they have no spam unit.
pub fn minifac_spam(minifac: UnitKind) -> Option<UnitKind> {
    match minifac {
        UnitKind::Socket => Some(UnitKind::Bit),
        UnitKind::Window => Some(UnitKind::Bug),
        _ => None,
    }
}

/// Upstream `kpunittypes.lua` buckets the fairness budget counts in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// `spam` list: bit, bug, packet, virus, exploit.
    Spam,
    /// `cons` + `heavy` + `arty` lists — Fair KPAI lumps them together
    /// as "mediums".
    Medium,
    /// `AnyBuilding`: homebases, minifacs and specials.
    Building,
}

pub fn role(kind: UnitKind) -> Option<Role> {
    match kind {
        UnitKind::Bit | UnitKind::Bug | UnitKind::Packet | UnitKind::Virus | UnitKind::Exploit => {
            Some(Role::Spam)
        }
        UnitKind::Assembler
        | UnitKind::Trojan
        | UnitKind::Gateway
        | UnitKind::Byte
        | UnitKind::Worm
        | UnitKind::Connection
        | UnitKind::Pointer
        | UnitKind::Dos
        | UnitKind::Flow => Some(Role::Medium),
        // Walls and mines aren't in upstream's `AnyBuilding` list.
        k if k.is_building() && k != UnitKind::BadBlock => Some(Role::Building),
        _ => None,
    }
}

/// Per-team unit tally by [`Role`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoleCounts {
    pub spams: i32,
    pub mediums: i32,
    pub buildings: i32,
}

impl RoleCounts {
    pub fn add(&mut self, kind: UnitKind) {
        match role(kind) {
            Some(Role::Spam) => self.spams += 1,
            Some(Role::Medium) => self.mediums += 1,
            Some(Role::Building) => self.buildings += 1,
            None => {}
        }
    }
}

/// Fair KPAI's `Lack`: how many more units of each role the AI may own
/// than its enemies. Recomputed every AI tick and decremented as orders
/// go out so several factories in one tick can't overshoot together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lack {
    pub spams: i32,
    pub mediums: i32,
    pub buildings: i32,
}

impl Lack {
    /// Upstream `UpdateFairness`: start from the tweakable head start
    /// `{spams=8, mediums=2, buildings=1}`, add every enemy unit and
    /// subtract every allied one. `slack` (from [`AiDifficulty`]) widens
    /// the head start: 0 reproduces Fair KPAI exactly, harder settings
    /// let the AI get further ahead.
    ///
    /// [`AiDifficulty`]: crate::game_setup::AiDifficulty
    pub fn compute(slack: usize, own: RoleCounts, enemy: RoleCounts) -> Self {
        let slack = slack as i32;
        Self {
            spams: 8 + 4 * slack + enemy.spams - own.spams,
            mediums: 2 + slack / 2 + enemy.mediums - own.mediums,
            buildings: 1 + slack / 4 + enemy.buildings - own.buildings,
        }
    }
}

/// One `OrderHomeBase` decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomebaseOrder {
    Constructor,
    Heavy,
    Arty,
    /// Queue this many spam units.
    Spam(u32),
    /// Budget exhausted — leave the factory idle this tick.
    Nothing,
}

/// Upstream spam batch per `OrderHomeBase` call (`for i = 1,3`).
const SPAM_BATCH: i32 = 3;

/// `KPAI_Fair.lua::OrderHomeBase`, with the dice passed in so the
/// decision stays a pure, testable function.
///
/// - `constructors`: constructors of this homebase's kind the team owns.
/// - `force`: KPAI `forceSize` — every unit the team owns.
/// - `buffer`: the team's Network packet buffer (counts as army).
/// - `roll`: `math.random(1000)`, i.e. 1..=1000.
/// - `heavy_coin`: `math.random(2) == 1`.
///
/// With `n` constructors, `n*200 < roll` builds another constructor
/// (so the 5th never comes); otherwise a roll in the top
/// `20 * (force + buffer)` builds one heavy or arty — big armies shift
/// toward them — and the rest queue a batch of spam.
pub fn choose_homebase_order(
    constructors: u32,
    force: u32,
    buffer: u32,
    roll: u32,
    heavy_coin: bool,
    lack: &Lack,
) -> HomebaseOrder {
    let roll = roll as i64;
    // Fair KPAI never lets the budget starve the very first constructor.
    if (constructors as i64) * 200 < roll && (lack.mediums > 0 || constructors == 0) {
        return HomebaseOrder::Constructor;
    }
    let heavy_threshold = 1000 - (force as i64 + buffer as i64) * 20;
    if roll > heavy_threshold && lack.mediums > 0 {
        return if heavy_coin {
            HomebaseOrder::Heavy
        } else {
            HomebaseOrder::Arty
        };
    }
    if lack.spams > 0 {
        return HomebaseOrder::Spam(SPAM_BATCH.min(lack.spams) as u32);
    }
    HomebaseOrder::Nothing
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLENTY: Lack = Lack {
        spams: 100,
        mediums: 100,
        buildings: 100,
    };

    #[test]
    fn rosters_match_upstream_order_home_base() {
        let k = homebase_roster(UnitKind::Kernel).unwrap();
        assert_eq!(
            (k.constructor, k.spam, k.arty, k.heavy),
            (
                UnitKind::Assembler,
                UnitKind::Bit,
                UnitKind::Pointer,
                UnitKind::Byte
            )
        );
        let h = homebase_roster(UnitKind::Hole).unwrap();
        assert_eq!(
            (h.constructor, h.spam, h.arty, h.heavy),
            (
                UnitKind::Trojan,
                UnitKind::Bug,
                UnitKind::Dos,
                UnitKind::Worm
            )
        );
        let c = homebase_roster(UnitKind::Carrier).unwrap();
        assert_eq!(
            (c.constructor, c.spam, c.arty, c.heavy),
            (
                UnitKind::Gateway,
                UnitKind::Packet,
                UnitKind::Flow,
                UnitKind::Connection
            )
        );
        assert!(homebase_roster(UnitKind::Socket).is_none());
    }

    /// No constructors: any roll above 0 builds one. With 5 the
    /// `n*200 < roll` test can never pass.
    #[test]
    fn constructor_odds_fall_with_count() {
        assert_eq!(
            choose_homebase_order(0, 0, 0, 1, true, &PLENTY),
            HomebaseOrder::Constructor
        );
        assert_eq!(
            choose_homebase_order(2, 0, 0, 401, true, &PLENTY),
            HomebaseOrder::Constructor
        );
        assert_eq!(
            choose_homebase_order(2, 0, 0, 400, true, &PLENTY),
            HomebaseOrder::Spam(3)
        );
        assert_ne!(
            choose_homebase_order(5, 0, 0, 1000, true, &PLENTY),
            HomebaseOrder::Constructor
        );
    }

    /// A roll above `1000 - 20*(force+buffer)` buys a heavy/arty; the
    /// packet buffer counts toward the army size.
    #[test]
    fn big_armies_buy_heavies() {
        // force 10 → threshold 800.
        assert_eq!(
            choose_homebase_order(5, 10, 0, 801, true, &PLENTY),
            HomebaseOrder::Heavy
        );
        assert_eq!(
            choose_homebase_order(5, 10, 0, 801, false, &PLENTY),
            HomebaseOrder::Arty
        );
        assert_eq!(
            choose_homebase_order(5, 10, 0, 800, true, &PLENTY),
            HomebaseOrder::Spam(3)
        );
        // force 5 + buffer 5 behaves like force 10.
        assert_eq!(
            choose_homebase_order(5, 5, 5, 801, true, &PLENTY),
            HomebaseOrder::Heavy
        );
    }

    /// The Fair-KPAI budget gates every branch: no mediums → no heavy
    /// (falls through to spam), spam batch capped by the spam budget,
    /// nothing at all once both run out — except the first constructor.
    #[test]
    fn lack_budget_caps_orders() {
        let no_mediums = Lack {
            spams: 2,
            mediums: 0,
            buildings: 0,
        };
        assert_eq!(
            choose_homebase_order(1, 60, 0, 900, true, &no_mediums),
            HomebaseOrder::Spam(2)
        );
        let broke = Lack {
            spams: 0,
            mediums: 0,
            buildings: 0,
        };
        assert_eq!(
            choose_homebase_order(1, 60, 0, 900, true, &broke),
            HomebaseOrder::Nothing
        );
        assert_eq!(
            choose_homebase_order(0, 60, 0, 900, true, &broke),
            HomebaseOrder::Constructor
        );
    }

    /// `UpdateFairness`: head start {8, 2, 1} + enemies − allies; the
    /// difficulty slack widens it.
    #[test]
    fn lack_is_head_start_plus_enemy_minus_own() {
        let own = RoleCounts {
            spams: 10,
            mediums: 3,
            buildings: 2,
        };
        let enemy = RoleCounts {
            spams: 4,
            mediums: 1,
            buildings: 2,
        };
        assert_eq!(
            Lack::compute(0, own, enemy),
            Lack {
                spams: 2,
                mediums: 0,
                buildings: 1
            }
        );
        let hard = Lack::compute(8, own, enemy);
        assert!(hard.spams > 2 && hard.mediums > 0 && hard.buildings > 1);
    }

    #[test]
    fn roles_follow_kpunittypes() {
        let mut c = RoleCounts::default();
        for k in [
            UnitKind::Bit,
            UnitKind::Exploit,
            UnitKind::Byte,
            UnitKind::Assembler,
            UnitKind::Flow,
            UnitKind::Kernel,
            UnitKind::Terminal,
            UnitKind::BadBlock,
        ] {
            c.add(k);
        }
        assert_eq!(
            c,
            RoleCounts {
                spams: 2,
                mediums: 3,
                buildings: 2
            }
        );
    }
}
