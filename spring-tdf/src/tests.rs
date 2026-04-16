use crate::{Section, Tdf, WeaponDefs};

// ── Unit tests: TDF parser ──────────────────────────────────────────

#[test]
fn empty_input() {
    let tdf = Tdf::parse("").unwrap();
    assert!(tdf.sections.is_empty());
}

#[test]
fn whitespace_only() {
    let tdf = Tdf::parse("   \n\n  \t  \n").unwrap();
    assert!(tdf.sections.is_empty());
}

#[test]
fn comments_only() {
    let tdf = Tdf::parse("// just a comment\n// another").unwrap();
    assert!(tdf.sections.is_empty());
}

#[test]
fn single_section_flat() {
    let tdf = Tdf::parse(
        r#"
[Rock]
{
    name=Rock's Weapon;
    range=256;
    reloadtime=0.5;
}
"#,
    )
    .unwrap();
    assert_eq!(tdf.sections.len(), 1);
    let s = &tdf.sections[0];
    assert_eq!(s.name, "Rock");
    assert_eq!(s.get("name"), Some("Rock's Weapon"));
    assert_eq!(s.f32("range"), 256.0);
    assert_eq!(s.f32("reloadtime"), 0.5);
}

#[test]
fn nested_section() {
    let tdf = Tdf::parse(
        r#"
[Weapon]
{
    range=100;
    [DAMAGE]
    {
        default=50;
        heavy=200;
    }
}
"#,
    )
    .unwrap();
    let s = &tdf.sections[0];
    let damage = s.child("DAMAGE").unwrap();
    assert_eq!(damage.f32("default"), 50.0);
    assert_eq!(damage.f32("heavy"), 200.0);
}

#[test]
fn case_insensitive_key_lookup() {
    let tdf = Tdf::parse(
        r#"
[W]
{
    RGBcolor=128 0 0;
    WeaponType=BeamLaser;
}
"#,
    )
    .unwrap();
    let s = &tdf.sections[0];
    // Keys are stored lowercase; lookup is case-insensitive.
    assert_eq!(s.get("rgbcolor"), Some("128 0 0"));
    assert_eq!(s.get("RGBcolor"), Some("128 0 0"));
    assert_eq!(s.get("weapontype"), Some("BeamLaser"));
}

#[test]
fn case_insensitive_section_lookup() {
    let tdf = Tdf::parse(
        r#"
[Rock]
{
    [DAMAGE]
    {
        default=10;
    }
}
"#,
    )
    .unwrap();
    assert!(tdf.section("rock").is_some());
    assert!(tdf.section("ROCK").is_some());
    let s = tdf.section("Rock").unwrap();
    assert!(s.child("damage").is_some());
    assert!(s.child("DAMAGE").is_some());
}

#[test]
fn inline_comment_stripped() {
    let tdf = Tdf::parse(
        r#"
[W]
{
    impulseBoost=0; //Thanks, Argh.
    explosiongenerator=custom:none;// for unknown reason
}
"#,
    )
    .unwrap();
    let s = &tdf.sections[0];
    assert_eq!(s.f32("impulseboost"), 0.0);
    assert_eq!(s.get("explosiongenerator"), Some("custom:none"));
}

#[test]
fn decimal_shorthand() {
    let tdf = Tdf::parse(
        r#"
[W]
{
    reloadtime=.5;
    mygravity=.3;
    beamtime=0.27;
}
"#,
    )
    .unwrap();
    let s = &tdf.sections[0];
    assert!((s.f32("reloadtime") - 0.5).abs() < f32::EPSILON);
    assert!((s.f32("mygravity") - 0.3).abs() < 0.001);
    assert!((s.f32("beamtime") - 0.27).abs() < 0.001);
}

#[test]
fn very_small_values() {
    let tdf = Tdf::parse(
        r#"
[W]
{
    [DAMAGE]
    {
        default=0.0000000001;
    }
}
"#,
    )
    .unwrap();
    let damage = tdf.sections[0].child("DAMAGE").unwrap();
    assert!(damage.f32("default") > 0.0);
    assert!(damage.f32("default") < 0.001);
}

#[test]
fn empty_value() {
    let tdf = Tdf::parse(
        r#"
[W]
{
    model=;
    texture2=none;
}
"#,
    )
    .unwrap();
    let s = &tdf.sections[0];
    assert_eq!(s.get("model"), Some(""));
    assert_eq!(s.get("texture2"), Some("none"));
}

#[test]
fn bool_parsing() {
    let tdf = Tdf::parse(
        r#"
[W]
{
    turret=1;
    ballistic=0;
    beamweapon=1;
}
"#,
    )
    .unwrap();
    let s = &tdf.sections[0];
    assert!(s.bool("turret"));
    assert!(!s.bool("ballistic"));
    assert!(s.bool("beamweapon"));
    assert!(!s.bool("nonexistent"));
}

#[test]
fn multiple_top_level_sections() {
    let tdf = Tdf::parse(
        r#"
[Rock]
{
    range=256;
}
[Paper]
{
    range=256;
}
[Scissors]
{
    range=256;
}
"#,
    )
    .unwrap();
    assert_eq!(tdf.sections.len(), 3);
    assert_eq!(tdf.sections[0].name, "Rock");
    assert_eq!(tdf.sections[1].name, "Paper");
    assert_eq!(tdf.sections[2].name, "Scissors");
}

#[test]
fn deeply_nested() {
    let tdf = Tdf::parse(
        r#"
[A]
{
    [B]
    {
        [C]
        {
            val=42;
        }
    }
}
"#,
    )
    .unwrap();
    let c = tdf.sections[0].child("B").unwrap().child("C").unwrap();
    assert_eq!(c.f32("val"), 42.0);
}

#[test]
fn error_on_unmatched_close_brace() {
    let result = Tdf::parse("}");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("unexpected closing brace"));
}

#[test]
fn error_on_unclosed_section() {
    let result = Tdf::parse("[W]\n{");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("unexpected end of input"));
}

#[test]
fn missing_key_returns_none() {
    let s = Section::default();
    assert_eq!(s.get("anything"), None);
    assert_eq!(s.f32("anything"), 0.0);
    assert!(!s.bool("anything"));
}

// ── Unit tests: WeaponDef extraction ────────────────────────────────

#[test]
fn weapon_def_from_beam() {
    let tdf = Tdf::parse(
        r#"
[BugShot]
{
    name=Failure;
    rendertype=1;
    beamweapon=1;
    RGBcolor=128 0 0;
    thickness=8;
    turret=1;
    range=320;
    reloadtime=.5;
    areaofeffect=8;
    explosiongenerator=custom:corruption_shot1;
    [DAMAGE]
    {
        default=130;
    }
}
"#,
    )
    .unwrap();
    let defs = WeaponDefs::from_tdf(&tdf);
    let w = defs.get("BugShot").unwrap();
    assert_eq!(w.id, "BugShot");
    assert_eq!(w.name, "Failure");
    assert!(w.beam_weapon);
    assert!(!w.ballistic);
    assert_eq!(w.rgb_color, [128.0, 0.0, 0.0]);
    assert_eq!(w.range, 320.0);
    assert!((w.reload_time - 0.5).abs() < f32::EPSILON);
    assert_eq!(w.damage.default, 130.0);
}

#[test]
fn weapon_def_melee() {
    let tdf = Tdf::parse(
        r#"
[Wormbite]
{
    name=chomp();
    WeaponType=Melee;
    turret=1;
    range=200;
    reloadtime=6;
    [DAMAGE]
    {
        default=3200;
    }
}
"#,
    )
    .unwrap();
    let defs = WeaponDefs::from_tdf(&tdf);
    let w = defs.get("Wormbite").unwrap();
    assert_eq!(w.weapon_type, "Melee");
    assert_eq!(w.range, 200.0);
    assert_eq!(w.damage.default, 3200.0);
}

#[test]
fn weapon_def_shield() {
    let tdf = Tdf::parse(
        r#"
[Shield]
{
    weaponType=Shield;
    IsShield=1;
    shieldradius=128;
    shieldpower=0;
    [DAMAGE]
    {
        default=10;
    }
}
"#,
    )
    .unwrap();
    let defs = WeaponDefs::from_tdf(&tdf);
    let w = defs.get("Shield").unwrap();
    assert!(w.is_shield);
    assert_eq!(w.weapon_type, "Shield");
}

#[test]
fn damage_map_for_type() {
    let tdf = Tdf::parse(
        r#"
[Rock]
{
    [DAMAGE]
    {
        default=50;
        irony=600;
        papery=5;
        rocky=20;
    }
}
"#,
    )
    .unwrap();
    let defs = WeaponDefs::from_tdf(&tdf);
    let w = defs.get("Rock").unwrap();
    assert_eq!(w.damage.default, 50.0);
    assert_eq!(w.damage.for_type("irony"), 600.0);
    assert_eq!(w.damage.for_type("papery"), 5.0);
    assert_eq!(w.damage.for_type("rocky"), 20.0);
    // Unknown armor type falls back to default.
    assert_eq!(w.damage.for_type("unknown"), 50.0);
}

// ── Proptest ────────────────────────────────────────────────────────

mod proptests {
    use proptest::prelude::*;

    use crate::Tdf;

    /// Generate a valid TDF identifier (alphanumeric + underscore, starting with a letter).
    fn arb_ident() -> impl Strategy<Value = String> {
        "[A-Za-z][A-Za-z0-9_]{0,15}".prop_map(String::from)
    }

    /// Generate a TDF-safe value (no semicolons, braces, brackets, or slashes that form comments).
    fn arb_value() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop::char::range('!', '~').prop_filter("TDF-safe char", |c| {
                !matches!(c, ';' | '{' | '}' | '[' | ']')
            }),
            0..20,
        )
        .prop_map(|chars| {
            let s: String = chars.into_iter().collect();
            // Strip any accidental `//` sequences.
            s.replace("//", "")
        })
    }

    /// Generate a single `key=value;` line.
    fn arb_entry() -> impl Strategy<Value = (String, String)> {
        (arb_ident(), arb_value())
    }

    /// Generate a flat section with some key-value pairs.
    fn arb_flat_section() -> impl Strategy<Value = String> {
        (arb_ident(), prop::collection::vec(arb_entry(), 0..8)).prop_map(|(name, entries)| {
            let mut s = format!("[{name}]\n{{\n");
            for (k, v) in &entries {
                s.push_str(&format!("    {k}={v};\n"));
            }
            s.push_str("}\n");
            s
        })
    }

    /// Generate a complete TDF document with 1..4 sections.
    fn arb_tdf_document() -> impl Strategy<Value = String> {
        prop::collection::vec(arb_flat_section(), 1..4).prop_map(|sections| sections.join("\n"))
    }

    proptest! {
        #[test]
        fn parse_never_panics(input in ".*") {
            let _ = Tdf::parse(&input);
        }

        #[test]
        fn valid_tdf_always_parses(doc in arb_tdf_document()) {
            let tdf = Tdf::parse(&doc).expect("generated TDF should parse");
            prop_assert!(!tdf.sections.is_empty());
        }

        #[test]
        fn keys_are_lowercased(name in arb_ident(), key in arb_ident(), value in arb_value()) {
            let doc = format!("[{name}]\n{{\n    {key}={value};\n}}\n");
            let tdf = Tdf::parse(&doc).unwrap();
            let section = &tdf.sections[0];
            // The stored key must be lowercase.
            for k in section.entries.keys() {
                prop_assert_eq!(k, &k.to_ascii_lowercase());
            }
        }

        #[test]
        fn section_names_preserved(name in arb_ident()) {
            let doc = format!("[{name}]\n{{\n}}\n");
            let tdf = Tdf::parse(&doc).unwrap();
            prop_assert_eq!(&tdf.sections[0].name, &name);
        }

        #[test]
        fn roundtrip_key_values(
            name in arb_ident(),
            entries in prop::collection::vec(arb_entry(), 1..6),
        ) {
            let mut doc = format!("[{name}]\n{{\n");
            for (k, v) in &entries {
                doc.push_str(&format!("    {k}={v};\n"));
            }
            doc.push_str("}\n");

            let tdf = Tdf::parse(&doc).unwrap();
            let section = &tdf.sections[0];
            // Duplicate keys: last value wins (TDF semantics). Build
            // a map of the expected last-wins values to compare.
            let mut expected = std::collections::HashMap::new();
            for (k, v) in &entries {
                expected.insert(k.to_ascii_lowercase(), v.trim().to_string());
            }
            for (k, v) in &expected {
                let stored = section.get(k).unwrap_or("");
                prop_assert_eq!(stored, v.as_str());
            }
        }

        #[test]
        fn nested_sections_round_trip(
            parent in arb_ident(),
            child in arb_ident(),
            val in "[0-9]{1,5}",
        ) {
            let doc = format!(
                "[{parent}]\n{{\n    [{child}]\n    {{\n        x={val};\n    }}\n}}\n"
            );
            let tdf = Tdf::parse(&doc).unwrap();
            let p = &tdf.sections[0];
            prop_assert_eq!(&p.name, &parent);
            let c = p.children.iter().find(|c| c.name == child);
            prop_assert!(c.is_some(), "child section '{}' missing", child);
            prop_assert_eq!(c.unwrap().get(&"x".to_string()), Some(val.as_str()));
        }
    }
}

// ── Integration tests: real upstream weapon files ───────────────────

mod real_files {
    use std::path::Path;

    use crate::{Tdf, WeaponDefs};

    fn find_weapons_dir() -> Option<&'static str> {
        const CANDIDATES: &[&str] = &[
            "upstream/Kernel-Panic/weapons",
            "kernel-panic/upstream/Kernel-Panic/weapons",
            "../upstream/Kernel-Panic/weapons",
        ];
        CANDIDATES.iter().copied().find(|p| Path::new(p).is_dir())
    }

    fn load_weapon_file(dir: &str, filename: &str) -> (Tdf, WeaponDefs) {
        let path = format!("{dir}/{filename}");
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
        let tdf = Tdf::parse(&text).unwrap_or_else(|e| panic!("failed to parse {path}: {e}"));
        let defs = WeaponDefs::from_tdf(&tdf);
        (tdf, defs)
    }

    #[test]
    fn parse_rps_weapons() {
        let Some(dir) = find_weapons_dir() else {
            eprintln!("skipping: weapons dir not found");
            return;
        };
        let (tdf, defs) = load_weapon_file(dir, "rpsweapons.tdf");

        assert_eq!(tdf.sections.len(), 3, "Rock/Paper/Scissors");

        let rock = defs.get("Rock").expect("Rock weapon");
        assert_eq!(rock.name, "Rock's Weapon");
        assert!(rock.beam_weapon);
        assert_eq!(rock.range, 256.0);
        assert!((rock.reload_time - 0.5).abs() < f32::EPSILON);
        assert_eq!(rock.damage.default, 50.0);
        assert_eq!(rock.damage.for_type("irony"), 600.0);
        assert_eq!(rock.damage.for_type("papery"), 5.0);

        let paper = defs.get("Paper").expect("Paper weapon");
        assert_eq!(paper.damage.for_type("rocky"), 600.0);

        let scissors = defs.get("Scissors").expect("Scissors weapon");
        assert_eq!(scissors.damage.for_type("papery"), 600.0);
    }

    #[test]
    fn parse_corruption_weapons() {
        let Some(dir) = find_weapons_dir() else {
            eprintln!("skipping: weapons dir not found");
            return;
        };
        let (_, defs) = load_weapon_file(dir, "corruptionweapons.tdf");

        let bugshot = defs.get("BugShot").expect("BugShot");
        assert_eq!(bugshot.name, "Failure");
        assert!(bugshot.beam_weapon);
        assert_eq!(bugshot.rgb_color, [128.0, 0.0, 0.0]);
        assert_eq!(bugshot.range, 320.0);
        assert_eq!(bugshot.damage.default, 130.0);

        let dos = defs.get("DOS_Beam").expect("DOS_Beam");
        assert!(dos.beam_laser);
        assert!(dos.large_beam_laser);
        assert!(dos.paralyzer);
        assert_eq!(dos.paralyze_time, 5.0);

        let wormbite = defs.get("Wormbite").expect("Wormbite");
        assert_eq!(wormbite.weapon_type, "Melee");
        assert_eq!(wormbite.range, 200.0);

        let infection = defs.get("Infection").expect("Infection");
        assert!(infection.ballistic);
        assert!(infection.command_fire);
        assert_eq!(infection.range, 2000.0);
    }

    #[test]
    fn parse_network_weapons() {
        let Some(dir) = find_weapons_dir() else {
            eprintln!("skipping: weapons dir not found");
            return;
        };
        let (_, defs) = load_weapon_file(dir, "networkweapons.tdf");

        let packet = defs.get("PacketBeam").expect("PacketBeam");
        assert_eq!(packet.weapon_type, "BeamLaser");
        assert!(packet.beam_burst);
        assert_eq!(packet.range, 250.0);

        let flow = defs.get("FlowMissile").expect("FlowMissile");
        assert_eq!(flow.weapon_type, "StarburstLauncher");
        assert!(flow.tracks);
        assert!(flow.smoke_trail);
    }

    #[test]
    fn parse_experiment_weapons() {
        let Some(dir) = find_weapons_dir() else {
            eprintln!("skipping: weapons dir not found");
            return;
        };
        let (_, defs) = load_weapon_file(dir, "experimentweapons.tdf");

        let stationary = defs.get("ExpStationary").expect("ExpStationary");
        assert_eq!(stationary.weapon_type, "MissileLauncher");
        assert_eq!(stationary.range, 4000.0);
        assert!(stationary.tracks);

        let mobile = defs.get("ExpMobile").expect("ExpMobile");
        assert_eq!(mobile.range, 1300.0);
    }

    #[test]
    fn parse_onsshield() {
        let Some(dir) = find_weapons_dir() else {
            eprintln!("skipping: weapons dir not found");
            return;
        };
        let (_, defs) = load_weapon_file(dir, "onsshield.tdf");

        let good = defs.get("homebaseshieldgood").expect("homebaseshieldgood");
        assert!(good.is_shield);
        assert_eq!(good.weapon_type, "Shield");
    }

    #[test]
    fn parse_all_weapon_files_without_error() {
        let Some(dir) = find_weapons_dir() else {
            eprintln!("skipping: weapons dir not found");
            return;
        };
        let files = [
            "rpsweapons.tdf",
            "corruptionweapons.tdf",
            "networkweapons.tdf",
            "experimentweapons.tdf",
            "retroweapons.tdf",
            "touhouweapons.tdf",
            "onsshield.tdf",
        ];
        for filename in &files {
            let path = format!("{dir}/{filename}");
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
            let tdf = Tdf::parse(&text).unwrap_or_else(|e| panic!("failed to parse {path}: {e}"));
            let defs = WeaponDefs::from_tdf(&tdf);
            assert!(
                !defs.weapons.is_empty(),
                "{filename} should contain at least one weapon"
            );
            for (name, w) in &defs.weapons {
                assert!(!name.is_empty(), "weapon name should not be empty");
                assert!(
                    w.range >= 0.0,
                    "{name} in {filename}: range should be non-negative"
                );
            }
        }
    }
}
