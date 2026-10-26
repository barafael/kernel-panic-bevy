//! The automatic tip dispenser, bottom centre — zwzsg's
//! `kp_automatic_tip_dispenser.lua` ("Teach you to play the game, one
//! tip at a time").
//!
//! Every [`TIP_PERIOD`] seconds of real time the widget rebuilds its
//! list of tips from what the player has (units, selection, the
//! Network's buffer), weighted towards the units they field and select,
//! and draws one at random — never the one just shown. Most tips carry
//! an original voice line, `sounds/tips/<n>.ogg`: `n < 1000` is the
//! tip's number for the player's side (System 1xxx, Hacker 2xxx,
//! Network 3xxx), larger numbers name one file. The generic tips fall
//! back to the Network voice where a side has no recording (Hacker has
//! none at all), as upstream does in `widget:Initialize`.
//!
//! Clicking the box toggles the voice and says so ("Voice: ON/OFF",
//! the Network voice's 3091 / 3092). Here the toggle is the Audio
//! page's "Tips voice" knob, so it is kept between runs; the wheel over
//! the box resizes its font. Settings → Tips hides the widget, as
//! upstream's widget list (F11) does.
//!
//! Left out: the tips about Spring's own interface (F11 widgets,
//! Ctrl+F1–F5 camera modes, Alt+F, start position picking), the Heroes
//! of Mainframe and Rock-Paper-Scissors modes, the Touhou side and the
//! old Hacker units, none of which the remake has.

use bevy::audio::{AudioPlayer, AudioSource, PlaybackSettings, Volume};
use bevy::ecs::message::Messages;
use bevy::input::InputSystems;
use bevy::input::mouse::MouseWheel;
use bevy::picking::Pickable;
use bevy::picking::PickingSystems;
use bevy::picking::hover::Hovered as PointerOver;
use bevy::prelude::*;
use bevy::text::{FontSize, LineHeight};
use bevy::window::PrimaryWindow;

use crate::game_setup::{AppState, GameSetup};
use crate::interaction::selection::Selected;
use crate::map_loading::MatchScopeAppExt;
use crate::rendering::settings::{RenderSettings, VolumeKnob};
use crate::sim::{DetFloat, SimTick};
use crate::units::combat::Dying;
use crate::units::components::{Faction, TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::mechanics::network_buffer::PacketBuffer;
use crate::units::player::LocalTeam;

/// `TipPeriod`: how long each tip is displayed, in seconds.
const TIP_PERIOD: f32 = 10.0;

/// The voice files shipped under `assets/sounds/tips/` (the web build
/// cannot list a directory). Upstream's `sounds/tips` minus the lines
/// of the tips left out (see the module docs).
const TIP_VOICES: &[u16] = &[
    1012, 1021, 1026, 1031, 1032, 1033, 1034, 1035, 1036, 1037, 1038, 1039, 1040, 1041, 1042, 1043,
    1044, 1051, 1052, 1053, 1054, 1055, 1056, 1057, 1058, 1059, 1060, 1061, 1062, 1063, 1064, 1065,
    1066, 1071, 1072, 1073, 1081, 1082, 1083, 1084, 1085, 1086, 1108, 1109, 1116, 1117, 1118, 1119,
    1125, 3012, 3021, 3026, 3031, 3032, 3033, 3034, 3035, 3036, 3037, 3038, 3039, 3040, 3041, 3042,
    3043, 3044, 3051, 3052, 3053, 3054, 3055, 3056, 3057, 3058, 3059, 3061, 3062, 3063, 3064, 3065,
    3066, 3071, 3072, 3073, 3081, 3082, 3083, 3084, 3085, 3086, 3088, 3091, 3092, 3104, 3105, 3106,
    3107, 3114, 3115, 3123, 3124, 3127,
];

/// The tips "that do not contain side specific words": a side without
/// its own recording of one uses the Network voice (`3000 + n`).
const GENERIC_TIPS: &[u16] = &[
    87, 88, 711, 712, 713, 11, 12, 21, 22, 23, 24, 25, 26, 27, 71, 72, 73, 81, 82, 83, 84, 85, 86,
];

/// "Voice: ON" / "Voice: OFF" — upstream has them in the Network voice
/// only.
const VOICE_ON: u16 = 3091;
const VOICE_OFF: u16 = 3092;

/// The voice file a tip plays for `side`, if there is one.
fn voice_file(sound: u16, side: Faction) -> Option<u16> {
    let has = |n: u16| TIP_VOICES.contains(&n);
    if sound >= 1000 {
        return has(sound).then_some(sound);
    }
    let base = match side {
        Faction::System => 1000,
        Faction::Hacker => 2000,
        Faction::Network => 3000,
    };
    if has(base + sound) {
        Some(base + sound)
    } else if GENERIC_TIPS.contains(&sound) && has(3000 + sound) {
        Some(3000 + sound)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// The tip list
// ---------------------------------------------------------------------------

const HOMEBASE: &[UnitKind] = &[UnitKind::Kernel, UnitKind::Hole, UnitKind::Carrier];
const MINIFAC: &[UnitKind] = &[UnitKind::Socket, UnitKind::Window, UnitKind::Port];
const SPECIAL: &[UnitKind] = &[UnitKind::Terminal, UnitKind::Obelisk, UnitKind::Firewall];
const CONS: &[UnitKind] = &[UnitKind::Assembler, UnitKind::Trojan, UnitKind::Gateway];
const SPAM: &[UnitKind] = &[
    UnitKind::Bit,
    UnitKind::Bug,
    UnitKind::Packet,
    UnitKind::Virus,
    UnitKind::Exploit,
];
const HEAVY: &[UnitKind] = &[UnitKind::Byte, UnitKind::Worm, UnitKind::Connection];
const ARTY: &[UnitKind] = &[UnitKind::Pointer, UnitKind::Dos, UnitKind::Flow];

/// What the tips look at: the local team's units by kind (with whether
/// one of them is selected), its unit count, the selection size, the
/// game frame and the Network buffer.
#[derive(Default, Debug, Clone)]
pub(crate) struct Census {
    kinds: Vec<(UnitKind, u32, bool)>,
    total: u32,
    selected: u32,
    frame: u64,
    buffer: u32,
    /// The seat's faction, for a team without the units that tell
    /// (`Spring.GetTeamInfo`'s side).
    seat_side: Option<Faction>,
}

impl Census {
    fn add(&mut self, kind: UnitKind, selected: bool) {
        self.total += 1;
        match self.kinds.iter_mut().find(|(k, _, _)| *k == kind) {
            Some((_, n, sel)) => {
                *n += 1;
                *sel |= selected;
            }
            None => self.kinds.push((kind, 1, selected)),
        }
    }

    fn count(&self, kinds: &[UnitKind]) -> u32 {
        self.kinds
            .iter()
            .filter(|(k, _, _)| kinds.contains(k))
            .map(|(_, n, _)| n)
            .sum()
    }

    fn my(&self, kind: UnitKind) -> u32 {
        self.count(&[kind])
    }

    /// `Prevalence`: the share of the team's units of these kinds.
    fn prevalence(&self, kinds: &[UnitKind]) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            self.count(kinds) as f32 / self.total as f32
        }
    }

    fn is_selected(&self, kinds: &[UnitKind]) -> bool {
        self.kinds
            .iter()
            .any(|(k, _, sel)| *sel && kinds.contains(k))
    }

    /// `PSC`: prevalence and selection, weighted.
    fn psc(&self, kinds: &[UnitKind], prevalence_w: f32, selected_w: f32) -> f32 {
        prevalence_w * self.prevalence(kinds)
            + if self.is_selected(kinds) {
                selected_w
            } else {
                0.0
            }
    }

    /// `GetFixedSide`: the side the team's units say, in upstream's
    /// order (homebases, then minifacs, constructors, specials), else
    /// the seat's.
    fn side(&self) -> Option<Faction> {
        use UnitKind::*;
        let order = [
            (Kernel, Faction::System),
            (Hole, Faction::Hacker),
            (Carrier, Faction::Network),
            (Socket, Faction::System),
            (Window, Faction::Hacker),
            (Port, Faction::Network),
            (Assembler, Faction::System),
            (Trojan, Faction::Hacker),
            (Gateway, Faction::Network),
            (Terminal, Faction::System),
            (Obelisk, Faction::Hacker),
            (Firewall, Faction::Network),
        ];
        order
            .iter()
            .find(|(kind, _)| self.my(*kind) > 0)
            .map(|(_, side)| *side)
            .or(self.seat_side)
    }
}

/// `GetFixedUnitNames`, singular and plural (upstream's `MakePlural`
/// spelled out).
struct Names {
    homebase: &'static str,
    minifac: &'static str,
    minifacs: &'static str,
    special: &'static str,
    specials: &'static str,
    cons: &'static str,
    conss: &'static str,
    spams: &'static str,
    heavies: &'static str,
    arty: &'static str,
    arties: &'static str,
}

fn names(side: Option<Faction>) -> Names {
    match side {
        Some(Faction::System) => Names {
            homebase: "Kernel",
            minifac: "Socket",
            minifacs: "Sockets",
            special: "Terminal",
            specials: "Terminals",
            cons: "Assembler",
            conss: "Assemblers",
            spams: "Bits",
            heavies: "Bytes",
            arty: "Pointer",
            arties: "Pointers",
        },
        Some(Faction::Hacker) => Names {
            homebase: "Security Hole",
            minifac: "Window",
            minifacs: "Windows",
            special: "Obelisk",
            specials: "Obelisks",
            cons: "Trojan",
            conss: "Trojans",
            spams: "Bugs",
            heavies: "Worms",
            arty: "DoS",
            arties: "DoS",
        },
        Some(Faction::Network) => Names {
            homebase: "Carrier",
            minifac: "Port",
            minifacs: "Ports",
            special: "Firewall",
            specials: "Firewalls",
            cons: "Gateway",
            conss: "Gateways",
            spams: "Packets",
            heavies: "Connections",
            arty: "Flow",
            arties: "Flows",
        },
        None => Names {
            homebase: "HomeBase",
            minifac: "MiniFac",
            minifacs: "MiniFacs",
            special: "Special",
            specials: "Specials",
            cons: "Constructor",
            conss: "Constructors",
            spams: "Spaems",
            heavies: "Heavies",
            arty: "Arty",
            arties: "Arties",
        },
    }
}

/// "a" or "an" before a word, as upstream's `aan`.
fn aan(word: &str) -> String {
    let vowel = word
        .chars()
        .next()
        .is_some_and(|c| "aeiou".contains(c.to_ascii_lowercase()));
    format!("{} {word}", if vowel { "an" } else { "a" })
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Tip {
    weight: f32,
    pub text: String,
    pub sound: Option<u16>,
}

#[derive(Default)]
struct Tips(Vec<Tip>);

impl Tips {
    fn add(&mut self, weight: f32, text: impl Into<String>, sound: Option<u16>) {
        self.0.push(Tip {
            weight,
            text: text.into(),
            sound,
        });
    }
    /// Weight 1 with a voice line (`AddTip(text, sound)`).
    fn voiced(&mut self, text: impl Into<String>, sound: u16) {
        self.add(1.0, text, Some(sound));
    }
}

/// `GetTipsList`: every tip that applies right now.
pub(crate) fn tips_list(c: &Census) -> Vec<Tip> {
    use UnitKind::*;
    let un = names(c.side());
    let mut t = Tips::default();

    // Always shown tips
    t.voiced("Click that blue box to disable tips voice over", 88);

    // General interface tips
    t.voiced("Use Mouse Wheel to zoom in and out", 12);

    let minifacs = c.count(MINIFAC);
    let cons = c.count(CONS);
    let homebases = c.count(HOMEBASE);
    if minifacs + cons == 0 {
        // Beginning: Getting the homebase to build the first constructor
        if c.total == 0 {
            if c.frame > 90 {
                t.voiced("Oops! Looks like you're dead.", 21);
            } else {
                t.voiced("Game is loading.", 26);
            }
        } else if homebases >= 1 {
            if c.selected == 0 {
                t.voiced(
                    format!("Select your {} and build some {}", un.homebase, un.conss),
                    31,
                );
                t.voiced(
                    format!("Select your {} by left clicking it.", un.homebase),
                    32,
                );
            } else {
                t.voiced(
                    format!(
                        "On the left is a \"BuildMenu.\"\nLeft-Click the {} icon in it.",
                        un.cons
                    ),
                    33,
                );
            }
            t.voiced(format!("Build some {}!", un.conss), 34);
            t.voiced(
                format!("Use your {} to make one {}", un.homebase, un.cons),
                35,
            );
            t.voiced(
                format!(
                    "Alternatively, you can command your {}\nwith the build bar on the top right corner.",
                    un.homebase
                ),
                36,
            );
        }
    } else if cons >= 1 && minifacs == 0 {
        // Beginning: Getting constructor(s) to build the first minifac
        if !c.is_selected(CONS) {
            t.voiced(format!("Select {} to make a {}", un.cons, un.minifac), 37);
        }
        t.voiced(format!("Build some {}!", un.minifacs), 38);
        t.voiced(format!("Use your {} to make a {}", un.cons, un.minifac), 39);
        if c.count(SPECIAL) >= 1 {
            t.voiced(
                format!("These are no {}!\nThese are {}!", un.minifacs, un.specials),
                40,
            );
            t.voiced(format!("Nooooooo! Don't make {} now!", aan(un.special)), 41);
            t.voiced(
                format!("It is really not the time to make {}.", un.specials),
                42,
            );
            t.voiced(format!("Make {} not {}", un.minifacs, un.specials), 43);
            t.voiced(
                format!(
                    "The {} is a late game unit.\nWhat you need now is {}.",
                    un.special,
                    aan(un.minifac)
                ),
                44,
            );
        }
    } else {
        // Once the player has started getting stuff done
        if cons >= 1 {
            t.voiced(
                format!("Expand! You never have enough {}!", un.minifacs),
                51,
            );
        }
        if homebases + minifacs >= 1 {
            let w = if cons == 0 {
                20.0
            } else {
                1.0 + 5.0 * (1.0 - 5.0 * c.prevalence(CONS).det_min(0.2))
            };
            t.add(w, format!("You need {} to expand!", un.conss), Some(52));
        }
        if c.prevalence(SPAM) < 0.9 {
            t.voiced(format!("Spaem more {}!", un.spams), 53);
        }
        if homebases >= 1 {
            t.voiced(
                format!(
                    "The build bar on the top right corner\nlet you control your {} production\nanywhere, anytime, without having to select it",
                    un.homebase
                ),
                54,
            );
        }
        if homebases + minifacs + c.count(SPAM) > c.my(Virus) {
            let w = 2.0 + c.psc(SPAM, 1.0, 1.0);
            t.add(
                w,
                format!(
                    "{} are your spam units:\nLight, fast, cheap, expandable.",
                    un.spams
                ),
                Some(55),
            );
            t.add(
                w,
                format!("{} must be used in numbers to be effective.", un.spams),
                Some(56),
            );
        }
        if homebases + c.count(HEAVY) >= 1 {
            let w = 1.0 + 4.0 * c.psc(HEAVY, 1.0, 1.0);
            t.add(
                w,
                format!(
                    "{} are your heavy units:\nSlow, heavily armored, short ranged, expensive.",
                    un.heavies
                ),
                Some(57),
            );
            t.add(
                w,
                format!("{} are best used against smaller enemies.", un.heavies),
                Some(58),
            );
        }
        if homebases + c.count(ARTY) >= 1 {
            let w = 1.0 + 4.0 * c.psc(ARTY, 1.0, 1.0);
            t.add(
                w,
                format!("{} are your arty:\nFrail and expensive.", un.arties),
                Some(59),
            );
            if !un.arty.eq_ignore_ascii_case("flow") {
                t.add(
                    w,
                    format!("Never send {} to the frontline alone.", un.arties),
                    Some(60),
                );
            }
        }
        if homebases + cons >= 1 {
            let w = 1.0 + 2.0 * c.psc(CONS, 1.0, 1.0);
            t.add(
                w,
                format!(
                    "{} are your constructors:\nUse them to raise new {}, not to fight.",
                    un.conss, un.minifacs
                ),
                Some(61),
            );
            t.add(
                w,
                format!("{} raison d'etre is to build, not to attack.", un.conss),
                Some(62),
            );
            t.add(
                1.0 + 4.0 * c.psc(CONS, 1.0, 1.0),
                format!(
                    "{} are equipped with a radar\nto see cloacked units.",
                    un.conss
                ),
                Some(63),
            );
        }
        if cons + minifacs >= 1 && homebases >= 1 {
            t.add(
                0.3 + 8.0 * c.psc(MINIFAC, 1.0, 1.0),
                format!(
                    "Every {} gives a 20% boost to your {} buildspeed.\n(In addition to spawning {})",
                    un.minifac, un.homebase, un.spams
                ),
                Some(64),
            );
        }
        if c.count(SPECIAL) >= 1 {
            t.add(
                1.0 + c.psc(SPECIAL, 20.0, 1.0),
                format!("You need in priority {}, not {}", un.minifacs, un.specials),
                Some(65),
            );
            t.add(
                1.0 + c.psc(SPECIAL, 3.0, 9.0),
                format!("{} must be fired manually", un.specials),
                Some(66),
            );
        }

        if c.my(Bug) >= 4 {
            t.add(
                1.0 + c.psc(&[Bug], 2.0, 4.0),
                "Bugs can flip into \"exploit\", a static cannon,\nwith Bombard or Deploy button!",
                Some(2101),
            );
        }
        if c.my(Exploit) >= 1 {
            let w = 1.0 + c.psc(&[Exploit], 2.0, 8.0);
            t.add(w, "Exploits deal more damage at longer range.", Some(2102));
            t.add(w, "Exploits are more fragile than bugs.", Some(2103));
        }
        if c.my(Port) >= 1 {
            t.add(
                1.0 + c.psc(&[Port], 2.0, 14.0),
                "Ports don't directly spawn unit\nInstead they increase the \"Buffer\"",
                Some(3104),
            );
        }
        if c.buffer >= 8 {
            let w = 1.0 + 5.0 * c.buffer as f32 / (1.0 + c.total as f32);
            t.add(
                w,
                "Use the Dispatch button to create packets from buffer.",
                Some(3105),
            );
            t.add(
                w,
                "The \"Buffer\" represent virtual units\nthat can be materialised at every Port or Connection.",
                Some(3106),
            );
        }
        if c.my(Packet) >= 1 {
            t.add(
                1.0 + c.psc(&[Packet], 2.0, 4.0),
                "Packets can re-enter the buffer\nby right clicking any Port or Connection",
                Some(3107),
            );
        }

        if c.my(Byte) >= 1 {
            let w = 1.0 + 4.0 * c.psc(&[Byte], 1.0, 1.0);
            t.add(w, "Bytes are more armored when closed.", Some(1108));
            t.add(
                w,
                "Bytes' special ability is the mine-launcher:\nCost quite a chunk of health,\nBut mighty against swarms.",
                Some(1109),
            );
        }
        if c.my(Worm) >= 1 {
            let w = 1.0 + 2.0 * c.psc(&[Worm], 1.0, 1.0);
            t.add(
                w,
                "Worms do not attack when cloacked,\nunless you switch AutoHold off.",
                Some(2110),
            );
            t.add(
                w,
                "Press Ctrl+A then set AutoHold off\nfor current and future worms\nto autoattack when cloacked.",
                Some(2111),
            );
            t.add(
                w,
                "Worms' bite is infectious:\nCasualties turn into virus.",
                Some(2112),
            );
            t.add(
                w,
                "Worms have a seismic detector\nto sense moving units.",
                Some(2113),
            );
        }
        if c.my(Connection) >= 1 {
            let w = 1.0 + 4.0 * c.psc(&[Connection], 1.0, 1.0);
            t.add(
                w,
                "Connection act as teleport beacons:\nUse the dispatch button!",
                Some(3114),
            );
            t.add(
                w,
                "Connection can\ndraw packets from / refill buffer with\npackets, but do not generate any.",
                Some(3115),
            );
        }

        if c.my(Kernel) + c.my(Pointer) >= 1 {
            let w = 1.0 + 2.0 * c.psc(&[Pointer], 1.0, 1.0);
            t.add(
                w,
                "Pointers are artillery:\nLightly armored and expensive,\nbut with a powerful long ranged weapon.",
                Some(1116),
            );
            t.add(
                w,
                "Use Pointers to destroy buildings\nand heavy units from afar.",
                Some(1117),
            );
            t.add(
                w,
                "Pointers's weapon is guided:\nIt will lock and home on moving target.",
                Some(1118),
            );
            if c.my(Pointer) >= 1 {
                let w = 1.0 + c.psc(&[Pointer], 2.0, 4.0);
                t.add(
                    w,
                    "Do NOT let the enemy come close to your Pointers.",
                    Some(1119),
                );
                t.add(
                    w,
                    "Pointer's secondary weapon is the NX flag,\nwhich deals damage over time over area.",
                    None,
                );
            }
            // Upstream tests `CountTheirs(flow)`, a number, which Lua
            // takes as true even at 0: the tip is always in.
            t.add(
                9.0,
                "Pointers' tracking shot\nmakes them great against flow!",
                None,
            );
        }
        if c.my(Hole) + c.my(Dos) >= 1 {
            t.add(
                1.0 + 6.0 * c.psc(&[Dos], 1.0, 1.0),
                "DoS are paralyzers:\nThey cannot kill.",
                Some(2122),
            );
        }
        if c.my(Carrier) + c.my(Flow) >= 1 {
            let w = 1.0 + 4.0 * c.psc(&[Flow], 1.0, 1.0);
            t.add(
                w,
                "Flows are the only flying units in this game.",
                Some(3123),
            );
            if c.my(Flow) >= 1 {
                t.add(
                    w,
                    format!("Flows get a speed increase with every {}", un.minifac),
                    Some(3124),
                );
            }
        }

        if c.my(Terminal) >= 1 {
            t.add(
                1.0 + c.psc(&[Terminal], 3.0, 9.0),
                "Terminal does reduced damage toward buildings.\nBest use it against large groups of light units.",
                Some(1125),
            );
        }
        if c.my(Obelisk) >= 1 {
            t.add(
                1.0 + c.psc(&[Obelisk], 3.0, 9.0),
                "Obelisk gas cloud casualties are turned into virus",
                Some(2126),
            );
        }
        if c.my(Firewall) >= 1 {
            t.add(
                1.0 + c.psc(&[Firewall], 3.0, 9.0),
                "The firewall casts a protection on your unit:\n- Halve the damage taken\n- Send that half back to attacker",
                Some(3127),
            );
        }

        let badblocks = c.my(BadBlock);
        if badblocks >= 1 {
            t.add(12.0, "You cannot win by walling yourself in!", Some(71));
        }
        t.add(
            1.0 + 12.0 * badblocks as f32,
            "The best defense is offense.",
            Some(72),
        );
        if c.my(Debug) >= 1 {
            t.add(
                30.0,
                "The Debugger sole purpose\nis to remove Bad Blocks and Logic Bombs.",
                Some(73),
            );
        }

        t.voiced(
            "Left click to select a unit\nKeep button down to drag a selection box",
            81,
        );
        t.voiced("Left click in empty area to deselect units.", 82);
        t.voiced(
            "Right click to issue default order\nKeep button down to move in formation",
            83,
        );
        // The command panel sits on the left (`KP_CtrlPanel.txt`
        // `x_pos 0`); upstream's text and its recording say "right".
        t.voiced(
            "Left click an action on the menu on the left\nThen left click in the terrain\nTo give specific orders.",
            84,
        );
        t.voiced(
            "Many commands can be issued over a whole area\nBy keeping button down and dragging a box or disc.",
            85,
        );
        t.voiced("Use the SHIFT key to enqueue orders", 86);
    }
    t.0
}

/// `GetRandomTip`: a weighted draw (`roll` in `[0, 1)`) that never
/// repeats `current`.
pub(crate) fn pick_tip(mut tips: Vec<Tip>, current: &str, roll: f32) -> Option<Tip> {
    if tips.len() <= 1 {
        return tips.pop();
    }
    for tip in &mut tips {
        if tip.text == current {
            tip.weight = 0.0;
        }
    }
    let total: f32 = tips.iter().map(|t| t.weight).sum();
    let d = total * roll;
    let mut w = 0.0;
    for tip in &tips {
        w += tip.weight;
        // Upstream's `w >= d` alone takes a zero-weight tip on a roll
        // of exactly 0: the current one again.
        if w >= d && tip.weight > 0.0 {
            return Some(tip.clone());
        }
    }
    Some(Tip {
        weight: 0.0,
        text: "Could not fetch tip".into(),
        sound: None,
    })
}

// ---------------------------------------------------------------------------
// The widget
// ---------------------------------------------------------------------------

/// The dispenser's state over a match.
#[derive(Resource, Default)]
struct TipDispenser {
    text: String,
    /// Seconds until the next tip; the first comes at once.
    next_in: f32,
    /// The wheel's font size (`FontSize`), once the player resized it.
    font_size: Option<f32>,
}

/// The tip voice files, loaded when the app plays audio.
///
/// As `AudioSource`s, not decoded [`Sfx`](crate::audio::Sfx): loading
/// a line as an `Sfx` when it was picked (and dropping it once played)
/// stalled the frame its tip came up in for 0.4–1 s (`KP_PROFILE`,
/// 2026-10), where the `AudioSource`'s header parse at play is a few
/// ms once per tip; held as PCM, all 102 lines would be tens of MB.
#[derive(Resource)]
struct TipVoiceBank(Vec<(u16, Handle<AudioSource>)>);

impl TipVoiceBank {
    fn get(&self, n: u16) -> Option<&Handle<AudioSource>> {
        self.0.iter().find(|(k, _)| *k == n).map(|(_, h)| h)
    }
}

fn load_tip_voices(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(TipVoiceBank(
        TIP_VOICES
            .iter()
            .map(|&n| (n, assets.load(format!("sounds/tips/{n}.ogg"))))
            .collect(),
    ));
}

/// The volume the click toggle restores ("Voice: ON") after muting.
#[derive(Resource)]
struct VoiceBeforeMute(u8);

impl Default for VoiceBeforeMute {
    fn default() -> Self {
        Self(100)
    }
}

/// The full-width row the box is centred in, the box (the pointer's
/// hit area) and its text.
#[derive(Component)]
struct TipRoot;
#[derive(Component)]
struct TipBox;
#[derive(Component)]
struct TipText;

/// One voice line playing; dropped with the match.
#[derive(Component)]
#[require(crate::map_loading::MatchEntity)]
struct TipVoice;

pub(super) struct TipsPlugin;

impl Plugin for TipsPlugin {
    fn build(&self, app: &mut App) {
        if app.is_plugin_added::<bevy::audio::AudioPlugin>() {
            app.add_systems(Startup, load_tip_voices);
        }
        app.init_match_resource::<TipDispenser>()
            .init_resource::<VoiceBeforeMute>()
            .add_systems(
                PreUpdate,
                tip_box_input
                    .after(InputSystems)
                    .after(PickingSystems::Hover)
                    .run_if(in_state(AppState::InGame)),
            )
            .add_systems(
                Update,
                dispense_tips
                    .run_if(in_state(AppState::InGame))
                    .after(crate::map_loading::GameWorldRebuild),
            );
    }
}

/// The local player's tips are for a seated player only: upstream's
/// widget hides for a spectator (replays, the attract demo).
fn census(
    local: Option<&LocalTeam>,
    units: &Query<(&UnitType, &TeamId, Has<Selected>), Without<Dying>>,
    tick: Option<&SimTick>,
    buffer: Option<&PacketBuffer>,
    setup: Option<&GameSetup>,
) -> Option<Census> {
    let team = local?.seat()?;
    let mut c = Census {
        frame: tick.map_or(0, |t| t.0),
        buffer: buffer.map_or(0, |b| b.peek(team)),
        seat_side: setup
            .and_then(|s| s.players.get(team as usize))
            .map(|p| p.faction),
        ..default()
    };
    for (kind, owner, selected) in units {
        if owner.0 == team {
            c.add(kind.0, selected);
        }
        if selected {
            c.selected += 1;
        }
    }
    Some(c)
}

/// Play one voice line at the Tips voice volume (`volume` percent).
fn play_voice(commands: &mut Commands, bank: Option<&TipVoiceBank>, n: u16, gain: f32) {
    if gain <= 0.0 {
        return;
    }
    if let Some(handle) = bank.and_then(|b| b.get(n)) {
        commands.spawn((
            AudioPlayer(handle.clone()),
            PlaybackSettings::DESPAWN.with_volume(Volume::Linear(gain)),
            TipVoice,
        ));
    }
}

fn font_size(dispenser: &TipDispenser, window: &Window) -> f32 {
    dispenser
        .font_size
        .unwrap_or_else(|| (4.0 + window.height() / 80.0).det_max(8.0))
}

/// A click on the box toggles the voice (`widget:MousePress` takes any
/// button); the wheel over it resizes the font, and neither reaches the
/// world (selection, the camera's zoom).
#[allow(clippy::too_many_arguments)]
fn tip_box_input(
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    mut wheel: ResMut<Messages<MouseWheel>>,
    boxes: Query<&PointerOver, With<TipBox>>,
    mut dispenser: ResMut<TipDispenser>,
    mut settings: ResMut<RenderSettings>,
    mut before_mute: ResMut<VoiceBeforeMute>,
    bank: Option<Res<TipVoiceBank>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut commands: Commands,
) {
    if !boxes.iter().any(|over| over.get()) {
        return;
    }
    let pressed: Vec<MouseButton> = mouse.get_just_pressed().copied().collect();
    for button in &pressed {
        mouse.clear_just_pressed(*button);
    }
    for button in mouse.get_just_released().copied().collect::<Vec<_>>() {
        mouse.clear_just_released(button);
    }
    if !pressed.is_empty() {
        let voice = settings.audio.voice;
        let on = voice == 0;
        if on {
            settings.audio.voice = before_mute.0.max(1);
        } else {
            before_mute.0 = voice;
            settings.audio.voice = 0;
        }
        dispenser.text = format!("Voice: {}", if on { "ON" } else { "OFF" });
        // The announcement sounds at the voice's volume, muted or not.
        let gain = settings.audio.linear(VolumeKnob::Master)
            * f32::from(if on { settings.audio.voice } else { voice })
            / 100.0;
        play_voice(
            &mut commands,
            bank.as_deref(),
            if on { VOICE_ON } else { VOICE_OFF },
            gain,
        );
    }
    let mut ticks = 0.0;
    for event in wheel.drain() {
        ticks += event.y;
    }
    if ticks != 0.0
        && let Ok(window) = windows.single()
    {
        let size = font_size(&dispenser, window);
        // Upstream: wheel up shrinks, down grows, never below 2.
        dispenser.font_size = Some((size - ticks.signum()).det_max(2.0));
    }
}

/// Upstream's colours: the box `(0.5, 1, 1)` at half alpha, its outline
/// opaque, and a faint wash of the same over the text.
const BOX_FILL: Color = Color::srgba(0.5, 1.0, 1.0, 0.5);
const BOX_LINE: Color = Color::srgb(0.5, 1.0, 1.0);
const BOX_WASH: Color = Color::srgba(0.5, 1.0, 1.0, 0.15);

#[allow(clippy::too_many_arguments)]
fn dispense_tips(
    time: Res<Time<Real>>,
    mut dispenser: ResMut<TipDispenser>,
    settings: Res<RenderSettings>,
    local: Option<Res<LocalTeam>>,
    // Like the command panel and the TAKE button, a unit in its death
    // animation is already gone to the census.
    units: Query<(&UnitType, &TeamId, Has<Selected>), Without<Dying>>,
    tick: Option<Res<SimTick>>,
    buffer: Option<Res<PacketBuffer>>,
    setup: Option<Res<GameSetup>>,
    bank: Option<Res<TipVoiceBank>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    roots: Query<Entity, With<TipRoot>>,
    mut texts: Query<(&mut Text, &mut TextFont, &mut LineHeight), With<TipText>>,
    mut nodes: Query<&mut Node, With<TipBox>>,
    mut commands: Commands,
) {
    // Shown to a seated player with tips on; the census (a pass over
    // every unit) only when a new tip is due.
    if !settings.tips
        || local
            .as_deref()
            .copied()
            .and_then(LocalTeam::seat)
            .is_none()
    {
        for e in &roots {
            commands.entity(e).despawn();
        }
        return;
    }

    dispenser.next_in -= time.delta_secs();
    if dispenser.next_in <= 0.0
        && let Some(census) = census(
            local.as_deref(),
            &units,
            tick.as_deref(),
            buffer.as_deref(),
            setup.as_deref(),
        )
    {
        dispenser.next_in = TIP_PERIOD;
        let side = census.side().unwrap_or(Faction::Network);
        let tip = pick_tip(
            tips_list(&census),
            &dispenser.text,
            crate::rng::clock_f64() as f32,
        );
        dispenser.text = tip.as_ref().map_or_else(String::new, |t| t.text.clone());
        if let Some(n) = tip.and_then(|t| t.sound).and_then(|s| voice_file(s, side)) {
            play_voice(
                &mut commands,
                bank.as_deref(),
                n,
                settings.audio.linear(VolumeKnob::Voice),
            );
        }
    }

    let Ok(window) = windows.single() else {
        return;
    };
    let size = font_size(&dispenser, window);
    let pad = Val::Px(size / 2.0);
    if let Ok((mut text, mut font, mut line)) = texts.single_mut() {
        if text.0 != dispenser.text {
            text.0 = dispenser.text.clone();
        }
        if font.font_size != FontSize::Px(size) {
            font.font_size = FontSize::Px(size);
            *line = LineHeight::Px(size);
            for mut node in &mut nodes {
                node.padding = UiRect::all(pad);
                node.bottom = pad;
            }
        }
        return;
    }
    if !roots.is_empty() {
        // Spawned this frame; the text exists from the next.
        return;
    }
    // Centred on the bottom edge, half a line up (upstream's "Much
    // better" placement).
    let wrapper = commands
        .spawn((
            TipRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                bottom: Val::Px(0.0),
                width: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                ..default()
            },
            Pickable::IGNORE,
            DespawnOnExit(AppState::InGame),
        ))
        .id();
    let tip_box = commands
        .spawn((
            ChildOf(wrapper),
            TipBox,
            Node {
                bottom: pad,
                padding: UiRect::all(pad),
                border: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BackgroundColor(BOX_FILL),
            BorderColor::all(BOX_LINE),
            PointerOver::default(),
        ))
        .id();
    commands.spawn((
        ChildOf(tip_box),
        TipText,
        Text::new(dispenser.text.clone()),
        TextFont::from_font_size(size),
        LineHeight::Px(size),
        TextColor(Color::WHITE),
        TextLayout::new(Justify::Left, LineBreak::NoWrap),
        TextShadow {
            offset: Vec2::splat(1.0),
            color: Color::BLACK,
        },
        Pickable::IGNORE,
    ));
    commands.spawn((
        ChildOf(tip_box),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        BackgroundColor(BOX_WASH),
        Pickable::IGNORE,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn census(units: &[(UnitKind, u32)]) -> Census {
        let mut c = Census {
            frame: 1000,
            ..default()
        };
        for &(kind, n) in units {
            for _ in 0..n {
                c.add(kind, false);
            }
        }
        c
    }

    fn texts(c: &Census) -> Vec<String> {
        tips_list(c).into_iter().map(|t| t.text).collect()
    }

    #[test]
    fn a_lone_homebase_is_told_to_build_constructors() {
        let tips = texts(&census(&[(UnitKind::Kernel, 1)]));
        assert!(tips.contains(&"Select your Kernel and build some Assemblers".to_string()));
        assert!(tips.contains(&"Build some Assemblers!".to_string()));
        assert!(!tips.iter().any(|t| t.starts_with("Expand!")));
    }

    #[test]
    fn a_constructor_without_minifacs_is_told_to_build_one() {
        let tips = texts(&census(&[
            (UnitKind::Carrier, 1),
            (UnitKind::Gateway, 1),
            (UnitKind::Firewall, 1),
        ]));
        assert!(tips.contains(&"Select Gateway to make a Port".to_string()));
        assert!(tips.contains(&"Nooooooo! Don't make a Firewall now!".to_string()));
    }

    #[test]
    fn the_dead_are_told_so() {
        let tips = texts(&census(&[]));
        assert!(tips.contains(&"Oops! Looks like you're dead.".to_string()));
    }

    #[test]
    fn unit_tips_follow_the_army() {
        let c = census(&[
            (UnitKind::Hole, 1),
            (UnitKind::Window, 2),
            (UnitKind::Bug, 5),
            (UnitKind::Worm, 1),
        ]);
        let tips = texts(&c);
        assert!(tips.iter().any(|t| t.starts_with("Bugs can flip")));
        assert!(tips.iter().any(|t| t.starts_with("Worms' bite")));
        assert!(!tips.iter().any(|t| t.starts_with("Bytes")));
        assert!(tips.contains(&"Spaem more Bugs!".to_string()));
    }

    #[test]
    fn the_side_comes_from_the_units_then_the_seat() {
        let mut c = census(&[(UnitKind::Bit, 1)]);
        assert_eq!(c.side(), None);
        c.seat_side = Some(Faction::Hacker);
        assert_eq!(c.side(), Some(Faction::Hacker));
        c.add(UnitKind::Port, false);
        assert_eq!(c.side(), Some(Faction::Network));
    }

    #[test]
    fn voices_fall_back_to_the_network_recording() {
        // System has its own line for tip 12, Hacker none.
        assert_eq!(voice_file(12, Faction::System), Some(1012));
        assert_eq!(voice_file(12, Faction::Hacker), Some(3012));
        // Side-specific tips without a recording stay silent.
        assert_eq!(voice_file(31, Faction::Hacker), None);
        // System never recorded tip 88; it is generic.
        assert_eq!(voice_file(88, Faction::System), Some(3088));
        // Numbered lines play as they are, if they exist.
        assert_eq!(voice_file(1108, Faction::Network), Some(1108));
        assert_eq!(voice_file(2110, Faction::Hacker), None);
    }

    #[test]
    fn the_draw_never_repeats_the_current_tip() {
        let tips = tips_list(&census(&[(UnitKind::Kernel, 1)]));
        let first = tips[0].text.clone();
        for i in 0..100 {
            let pick = pick_tip(tips.clone(), &first, i as f32 / 100.0).unwrap();
            assert_ne!(pick.text, first);
        }
    }

    #[test]
    fn every_voice_file_ships() {
        let dir = crate::paths::assets_dir().join("sounds/tips");
        for n in TIP_VOICES {
            let bytes = std::fs::read(dir.join(format!("{n}.ogg")))
                .unwrap_or_else(|e| panic!("tip {n}: {e}"));
            // Git LFS: a checkout without `git lfs pull` holds pointers.
            assert!(
                bytes.starts_with(b"OggS"),
                "tip {n} is not Ogg (an LFS pointer? run `git lfs pull`)"
            );
        }
        let shipped = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(shipped, TIP_VOICES.len(), "a file not in TIP_VOICES");
    }

    #[test]
    fn every_voiced_tip_resolves_for_some_side() {
        // A typo in a tip number would silence it for everyone.
        let c = census(&[
            (UnitKind::Kernel, 1),
            (UnitKind::Socket, 1),
            (UnitKind::Assembler, 1),
            (UnitKind::Byte, 1),
            (UnitKind::Pointer, 1),
            (UnitKind::Terminal, 1),
        ]);
        for tip in tips_list(&c) {
            if let Some(s) = tip.sound {
                assert!(
                    voice_file(s, Faction::System).is_some(),
                    "{:?} ({s}) has no System voice",
                    tip.text
                );
            }
        }
    }

    /// The web player's Tips voice tab (`web/music.html`) lists exactly
    /// the shipped lines, as `[n, "text"]` rows.
    #[test]
    fn web_player_lists_every_tip_voice() {
        let html = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web/music.html"),
        )
        .expect("web/music.html");
        let listed: Vec<u16> = html
            .lines()
            .filter_map(|line| line.trim_start().strip_prefix('['))
            .filter_map(|rest| rest.split_once(", \"").and_then(|(n, _)| n.parse().ok()))
            .collect();
        assert_eq!(listed, TIP_VOICES);
    }
}
