use crate::{EffectClass, ExplosionDefs, ParseError, Section, Tdf, UnitDefs, WeaponDefs};

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
    assert!(matches!(
        Tdf::parse("}"),
        Err(ParseError::UnmatchedCloseBrace { line: 1 })
    ));
}

#[test]
fn error_on_unclosed_section() {
    assert!(matches!(
        Tdf::parse("[W]\n{"),
        Err(ParseError::UnclosedSection { section, .. }) if section == "W"
    ));
}

// ── Strict Spring-conformance tests ─────────────────────────────────
//
// These pin behaviour against `cont/base/springcontent/gamedata/parse_tdf.lua`
// in the Recoil engine. Each test names the rule it enforces.

#[test]
fn block_comment_single_line_stripped() {
    let tdf = Tdf::parse(
        r#"
[W]
{
    a=1; /* inline */ b=2;
}
"#,
    )
    .unwrap();
    let s = &tdf.sections[0];
    assert_eq!(s.f32("a"), 1.0);
    assert_eq!(s.f32("b"), 2.0);
}

#[test]
fn block_comment_multiline_stripped() {
    // Mirrors the `/* ... */` block in upstream `units/thminifac.fbi`.
    let tdf = Tdf::parse(
        r#"
[UNITINFO]
{
    Unitname=test;
    /*UseBuildingGroundDecal=1;
    BuildingGroundDecalType=socket_base.tga;
    BuildingGroundDecalSizeX=8;*/
    MaxDamage=20000;
}
"#,
    )
    .unwrap();
    let s = &tdf.sections[0];
    assert_eq!(s.get("unitname"), Some("test"));
    assert_eq!(s.f32("maxdamage"), 20000.0);
    // Keys inside the block comment must NOT leak into entries.
    assert!(s.get("usebuildinggrounddecal").is_none());
    assert!(s.get("buildinggrounddecaltype").is_none());
    assert!(s.get("buildinggrounddecalsizex").is_none());
}

#[test]
fn block_comment_unterminated_errors() {
    assert!(matches!(
        Tdf::parse("[W]\n{\n  /* never closed\n  a=1;\n}\n"),
        Err(ParseError::UnterminatedBlockComment { .. })
    ));
}

#[test]
fn block_comment_first_close_wins_non_greedy() {
    // Spring's `^/%*.-*/()` is non-greedy: the first `*/` closes the
    // comment. The second `*/` is then a syntax error inside a value
    // context — but here we wrap it so the value remains well-formed.
    let tdf = Tdf::parse("[W]\n{\n  /* a */ b=2;\n}\n").unwrap();
    let s = &tdf.sections[0];
    assert_eq!(s.f32("b"), 2.0);
}

#[test]
fn missing_semicolon_errors() {
    assert!(matches!(
        Tdf::parse("[W]\n{\n  a=1\n}\n"),
        Err(ParseError::MissingSemicolon { .. })
    ));
}

#[test]
fn missing_equals_errors() {
    assert!(matches!(
        Tdf::parse("[W]\n{\n  bareword;\n}\n"),
        Err(ParseError::MissingEquals { .. })
    ));
}

#[test]
fn quoted_value_preserves_semicolon() {
    // Quoted values may contain `;` — the unquoted form may not.
    let tdf = Tdf::parse(
        r#"
[W]
{
    msg="hi;there";
}
"#,
    )
    .unwrap();
    let s = &tdf.sections[0];
    assert_eq!(s.get("msg"), Some("hi;there"));
}

#[test]
fn quoted_value_with_newline_errors() {
    assert!(matches!(
        Tdf::parse("[W]\n{\n  msg=\"line1\nline2\";\n}\n"),
        Err(ParseError::UnterminatedString { .. })
    ));
}

#[test]
fn unterminated_quoted_value_errors() {
    assert!(matches!(
        Tdf::parse("[W]\n{\n  msg=\"never closed;\n}\n"),
        Err(ParseError::UnterminatedString { .. })
    ));
}

#[test]
fn missing_section_close_brace_reports_section_name() {
    let err = Tdf::parse("[Foo]\n{\n  a=1;\n").unwrap_err();
    match err {
        ParseError::UnclosedSection { section, .. } => assert_eq!(section, "Foo"),
        other => panic!("expected UnclosedSection, got {other:?}"),
    }
}

#[test]
fn missing_section_open_brace_errors() {
    // Header without a `{` afterwards.
    assert!(matches!(
        Tdf::parse("[W]\n  a=1;\n"),
        Err(ParseError::MissingOpenBrace { section, .. }) if section == "W"
    ));
}

#[test]
fn unclosed_section_header_errors() {
    assert!(matches!(
        Tdf::parse("[W\n{\n}\n"),
        Err(ParseError::UnclosedSectionHeader { .. })
    ));
}

#[test]
fn comment_between_header_and_brace_ok() {
    // Spring's `EatWhite` runs after `]`, so a comment can sit between
    // the header and the opening brace.
    let tdf = Tdf::parse(
        r#"
[W] // trailing comment on the header
/* block comment between */
{
    a=1;
}
"#,
    )
    .unwrap();
    assert_eq!(tdf.sections[0].f32("a"), 1.0);
}

#[test]
fn top_level_kv_stored_in_root_entries() {
    // `parse_tdf.lua` accepts a value at the root scope and stores it
    // in the root table; we surface those via `Tdf::root_entries`.
    let tdf = Tdf::parse("globalKey=42;\n[Sec]\n{\n}\n").unwrap();
    assert_eq!(
        tdf.root_entries.get("globalkey").map(String::as_str),
        Some("42")
    );
    assert_eq!(tdf.sections.len(), 1);
}

#[test]
fn multiple_semicolons_collapsed() {
    // `;+` matches one or more — the chain is consumed.
    let tdf = Tdf::parse("[W]\n{\n  a=1;;;\n  b=2;\n}\n").unwrap();
    let s = &tdf.sections[0];
    assert_eq!(s.f32("a"), 1.0);
    assert_eq!(s.f32("b"), 2.0);
}

#[test]
fn inline_whitespace_around_equals_ok() {
    // `key  =  value;` is valid: tabs/spaces between `key` and `=`.
    let tdf = Tdf::parse("[W]\n{\n  range\t = \t320 ;\n}\n").unwrap();
    assert_eq!(tdf.sections[0].f32("range"), 320.0);
}

#[test]
fn key_then_newline_before_equals_errors() {
    // Newline between key and `=` is invalid (Spring's `[ \t]*` excludes
    // newlines).
    assert!(matches!(
        Tdf::parse("[W]\n{\n  range\n  =320;\n}\n"),
        Err(ParseError::MissingEquals { .. })
    ));
}

#[test]
fn empty_value_ok() {
    let tdf = Tdf::parse("[W]\n{\n  empty=;\n  emptyq=\"\";\n}\n").unwrap();
    let s = &tdf.sections[0];
    assert_eq!(s.get("empty"), Some(""));
    assert_eq!(s.get("emptyq"), Some(""));
}

#[test]
fn section_name_with_metacharacters_preserved() {
    // Mirrors `[Team%i(%s) is no more]` in upstream `gamedata/messages.tdf`.
    let tdf = Tdf::parse("[Team%i(%s) is no more]\n{\n  tr1=hello;\n}\n").unwrap();
    assert_eq!(tdf.sections[0].name, "Team%i(%s) is no more");
    assert_eq!(tdf.sections[0].get("tr1"), Some("hello"));
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

#[test]
fn dyn_damage_multiplier_inverted_grows_with_distance() {
    let tdf = Tdf::parse(
        r#"
[BugCannon]
{
    range=1200;
    dynDamageExp=1;
    dynDamageInverted=1;
    dynDamageRange=700;
}
"#,
    )
    .unwrap();
    let defs = WeaponDefs::from_tdf(&tdf);
    let w = defs.get("BugCannon").unwrap();
    // Inverted + exp=1 → linear ramp from 0 at point-blank to 1 at range.
    assert!((w.dyn_damage_multiplier(0.0) - 0.0).abs() < 1e-5);
    assert!((w.dyn_damage_multiplier(350.0) - 0.5).abs() < 1e-5);
    assert!((w.dyn_damage_multiplier(700.0) - 1.0).abs() < 1e-5);
    // Beyond dyn_damage_range the multiplier clamps at 1.
    assert_eq!(w.dyn_damage_multiplier(1400.0), 1.0);
}

#[test]
fn dyn_damage_multiplier_flat_when_exp_zero() {
    let tdf = Tdf::parse(
        r#"
[Plain]
{
    range=400;
}
"#,
    )
    .unwrap();
    let defs = WeaponDefs::from_tdf(&tdf);
    let w = defs.get("Plain").unwrap();
    assert_eq!(w.dyn_damage_multiplier(0.0), 1.0);
    assert_eq!(w.dyn_damage_multiplier(200.0), 1.0);
    assert_eq!(w.dyn_damage_multiplier(400.0), 1.0);
}

// ── Unit tests: UnitDef extraction ─────────────────────────────────

#[test]
fn unit_def_from_fbi_basic() {
    let tdf = Tdf::parse(
        r#"
[UNITINFO]
{
    Name=Bit;
    Unitname=bit;
    Description=fast attack unit;
    Side=CPU;
    MaxDamage=600;
    BuildCostMetal=10;
    BuildTime=240;
    CanMove=1;
    MaxVelocity=3;
    MovementClass=LIGHT;
    ObjectName=ball.s3o;
    Weapon1=Line;
    CanAttack=1;
    SightDistance=512;
    Category=FAST EDIBLE UNIT NOTFACTORY TARGET;
    FootprintX=2;
    FootprintZ=2;
}
"#,
    )
    .unwrap();
    let defs = UnitDefs::from_tdf(&tdf);
    let bit = defs.get("bit").unwrap();
    assert_eq!(bit.name, "Bit");
    assert_eq!(bit.id, "bit");
    assert_eq!(bit.side, "CPU");
    assert_eq!(bit.max_health, 600.0);
    assert_eq!(bit.build_time, 240.0);
    assert!(bit.can_move);
    assert_eq!(bit.max_velocity, 3.0);
    assert_eq!(bit.movement_class, "LIGHT");
    assert_eq!(bit.object_name, "ball.s3o");
    assert_eq!(bit.weapon1, "Line");
    assert!(bit.can_attack);
    assert_eq!(bit.sight_distance, 512.0);
    assert_eq!(bit.footprint_x, 2.0);
    assert!(!bit.builder);
    assert!(!bit.commander);
}

#[test]
fn unit_def_homebase() {
    let tdf = Tdf::parse(
        r#"
[UNITINFO]
{
    Name=kernel;
    Unitname=kernel;
    Side=CPU;
    MaxDamage=40000;
    BuildTime=3200;
    Commander=1;
    Builder=1;
    WorkerTime=128;
    CanMove=1;
    MaxVelocity=0;
    ObjectName=kernel.s3o;
    Weapon1=BuildLaser;
    Weapon2=homebaseshieldgood;
    Weapon3=homebaseshieldbad;
    SightDistance=512;
    FootprintX=8;
    FootprintZ=8;
}
"#,
    )
    .unwrap();
    let defs = UnitDefs::from_tdf(&tdf);
    let kernel = defs.get("kernel").unwrap();
    assert!(kernel.commander);
    assert!(kernel.builder);
    assert_eq!(kernel.worker_time, 128.0);
    assert_eq!(kernel.max_velocity, 0.0);
    assert_eq!(kernel.weapon2, "homebaseshieldgood");
    assert_eq!(kernel.weapon3, "homebaseshieldbad");
}

#[test]
fn unit_def_weapon_inline_comment_stripped() {
    let tdf = Tdf::parse(
        r#"
[UNITINFO]
{
    Unitname=port;
    Name=Port;
    Side=NET;
    MaxDamage=20000;
    ObjectName=network_minifac.s3o;
    Weapon1=BuildLaser;//Unused
}
"#,
    )
    .unwrap();
    let defs = UnitDefs::from_tdf(&tdf);
    let port = defs.get("port").unwrap();
    assert_eq!(port.weapon1, "BuildLaser");
}

#[test]
fn unit_def_kamikaze() {
    let tdf = Tdf::parse(
        r#"
[UNITINFO]
{
    Unitname=logic_bomb;
    Name=Logic Bomb;
    Side=CPU;
    MaxDamage=300;
    ObjectName=logic_bomb.s3o;
    Kamikaze=1;
    Init_Cloaked=1;
    Weapon1=end_game_logic_bomb;
}
"#,
    )
    .unwrap();
    let defs = UnitDefs::from_tdf(&tdf);
    let bomb = defs.get("logic_bomb").unwrap();
    assert!(bomb.kamikaze);
    assert!(bomb.init_cloaked);
    assert_eq!(bomb.weapon1, "end_game_logic_bomb");
}

/// The engine adds 25 to `KamikazeDistance` (`UnitDef.cpp:508`): the
/// Logic Bomb's 64 triggers at 89 elmos; units without the tag get 0.
#[test]
fn unit_def_kamikaze_distance_adds_engine_fudge() {
    let tdf = Tdf::parse(
        "[UNITINFO]\n{\nUnitname=logic_bomb;\nKamikaze=1;\nKamikazeDistance=64;\n}\n\
         [UNITINFO]\n{\nUnitname=bit;\n}",
    )
    .unwrap();
    let defs = UnitDefs::from_tdf(&tdf);
    assert_eq!(defs.get("logic_bomb").unwrap().kamikaze_distance, 89.0);
    assert_eq!(defs.get("bit").unwrap().kamikaze_distance, 0.0);
}

/// `AutoHeal` is parsed (Flow: 30 HP/s always) and the idle pair takes
/// the engine defaults (10 HP/s after 600 frames) when absent, as for
/// the Flow and the Logic Bomb; an explicit `IdleAutoHeal=0` stays 0.
#[test]
fn unit_def_auto_heal_and_engine_idle_defaults() {
    let tdf = Tdf::parse(
        "[UNITINFO]\n{\nUnitname=flow;\nAutoHeal=30;\n}\n\
         [UNITINFO]\n{\nUnitname=mineblaster;\nIdleAutoHeal=0;\nIdleTime=900;\n}",
    )
    .unwrap();
    let defs = UnitDefs::from_tdf(&tdf);
    let flow = defs.get("flow").unwrap();
    assert_eq!(flow.auto_heal, 30.0);
    assert_eq!(flow.idle_auto_heal, 10.0);
    assert_eq!(flow.idle_time, 600.0);
    let debug = defs.get("mineblaster").unwrap();
    assert_eq!(debug.auto_heal, 0.0);
    assert_eq!(debug.idle_auto_heal, 0.0);
    assert_eq!(debug.idle_time, 900.0);
}

/// `CanAttack` defaults to on (`UnitDef.cpp:381`); the builders'
/// `CanAttack=0` turns it off.
#[test]
fn unit_def_can_attack_defaults_on() {
    let tdf = Tdf::parse(
        "[UNITINFO]\n{\nUnitname=gateway;\n}\n[UNITINFO]\n{\nUnitname=trojan;\nCanAttack=0;\n}",
    )
    .unwrap();
    let defs = UnitDefs::from_tdf(&tdf);
    assert!(defs.get("gateway").unwrap().can_attack);
    assert!(!defs.get("trojan").unwrap().can_attack);
}

/// `SelfDestructCountdown` is parsed (1 on KP's mobile units) with the
/// engine default 5 when absent (the buildings).
#[test]
fn unit_def_self_destruct_countdown() {
    let tdf = Tdf::parse(
        "[UNITINFO]\n{\nUnitname=bit;\nSelfDestructCountdown=1;\n}\n\
         [UNITINFO]\n{\nUnitname=kernel;\n}",
    )
    .unwrap();
    let defs = UnitDefs::from_tdf(&tdf);
    assert_eq!(defs.get("bit").unwrap().self_destruct_countdown, 1.0);
    assert_eq!(defs.get("kernel").unwrap().self_destruct_countdown, 5.0);
}

/// bug.fbi `WeaponMainDir1=0 0 1; MaxAngleDif1=270;` → a forward arc
/// whose cosine limit is `cos(135°)`; without the tags the arc is the
/// whole sphere (`cos 180° = -1`).
#[test]
fn unit_def_weapon_arc() {
    let tdf = Tdf::parse(
        "[UNITINFO]\n{\nUnitname=bug;\nWeaponMainDir1=0 0 2;\nMaxAngleDif1=270;\n}\n\
         [UNITINFO]\n{\nUnitname=bit;\n}",
    )
    .unwrap();
    let defs = UnitDefs::from_tdf(&tdf);
    let bug = defs.get("bug").unwrap();
    assert_eq!(bug.weapon_main_dir1, [0.0, 0.0, 1.0]);
    assert!((bug.max_angle_dif1_cos - libm::cosf(135f32.to_radians())).abs() < 1e-6);
    let bit = defs.get("bit").unwrap();
    assert_eq!(bit.weapon_main_dir1, [0.0, 0.0, 1.0]);
    assert!((bit.max_angle_dif1_cos + 1.0).abs() < 1e-6);
}

#[test]
fn unit_def_case_insensitive_lookup() {
    let tdf = Tdf::parse(
        r#"
[UNITINFO]
{
    Unitname=Bit;
    Name=Bit;
    Side=CPU;
    MaxDamage=600;
    ObjectName=ball.s3o;
}
"#,
    )
    .unwrap();
    let defs = UnitDefs::from_tdf(&tdf);
    assert!(defs.get("bit").is_some());
    assert!(defs.get("BIT").is_some());
    assert!(defs.get("Bit").is_some());
}

#[test]
fn unit_def_buildpic_parsed_case_insensitive() {
    // FBIs use both `buildpic=` and `BuildPic=` — parser must accept either.
    let lower = Tdf::parse(
        r#"
[UNITINFO]
{
    Unitname=bit;
    Name=Bit;
    ObjectName=ball.s3o;
    buildpic=bit.pcx;
}
"#,
    )
    .unwrap();
    assert_eq!(
        UnitDefs::from_tdf(&lower).get("bit").unwrap().build_pic,
        "bit.pcx"
    );

    let upper = Tdf::parse(
        r#"
[UNITINFO]
{
    Unitname=logic_bomb;
    Name=Logic Bomb;
    ObjectName=logic_bomb.s3o;
    BuildPic=logic_bomb.pcx;
}
"#,
    )
    .unwrap();
    assert_eq!(
        UnitDefs::from_tdf(&upper)
            .get("logic_bomb")
            .unwrap()
            .build_pic,
        "logic_bomb.pcx"
    );
}

#[test]
fn unit_def_buildpic_missing_is_empty() {
    // Some FBIs (kernel.fbi, signal.fbi) have no BuildPic field.
    let tdf = Tdf::parse(
        r#"
[UNITINFO]
{
    Unitname=kernel;
    Name=Kernel;
    ObjectName=kernel.s3o;
}
"#,
    )
    .unwrap();
    assert_eq!(
        UnitDefs::from_tdf(&tdf).get("kernel").unwrap().build_pic,
        ""
    );
}

#[test]
fn unit_def_damage_modifier_default() {
    // When DamageModifier is absent, should default to 1.0
    let tdf = Tdf::parse(
        r#"
[UNITINFO]
{
    Unitname=test;
    Name=Test;
    ObjectName=test.s3o;
}
"#,
    )
    .unwrap();
    let defs = UnitDefs::from_tdf(&tdf);
    let unit = defs.get("test").unwrap();
    assert_eq!(unit.damage_modifier, 1.0);
}

#[test]
fn unit_def_flying_unit() {
    let tdf = Tdf::parse(
        r#"
[UNITINFO]
{
    Unitname=signal;
    Name=SIGTERM;
    Side=CPU;
    MaxDamage=600;
    ObjectName=signal.s3o;
    canFly=1;
    cruiseAlt=200;
    MaxVelocity=8;
    CanMove=1;
}
"#,
    )
    .unwrap();
    let defs = UnitDefs::from_tdf(&tdf);
    let signal = defs.get("signal").unwrap();
    assert!(signal.can_fly);
    assert_eq!(signal.cruise_alt, 200.0);
    assert_eq!(signal.max_velocity, 8.0);
}

// ── Unit tests: ExplosionDef extraction ────────────────────────────

#[test]
fn explosion_def_particle_system() {
    let tdf = Tdf::parse(
        r#"
[corruption_burst]
{
    [burst]
    {
        class=CSimpleParticleSystem;
        [properties] {
            Texture=circle;
            colorMap=.4 0 0 1   .3 0 0 .8   0 0 0 0;
            numParticles=30;
            particleLife=24;
            particleSpeed=5;
            particleSize=4;
            airdrag=.98;
            directional=0;
        }
        air=1;
        ground=1;
        water=1;
    }
}
"#,
    )
    .unwrap();
    let defs = ExplosionDefs::from_tdf(&tdf);
    let burst = defs.get("corruption_burst").unwrap();
    assert_eq!(burst.id, "corruption_burst");
    assert_eq!(burst.effects.len(), 1);
    assert!(burst.ground_flash.is_none());

    let effect = &burst.effects[0];
    assert_eq!(effect.name, "burst");
    assert_eq!(effect.class, EffectClass::SimpleParticleSystem);
    assert!(effect.air);
    assert!(effect.ground);
    assert!(effect.water);
    assert_eq!(effect.count, 1);

    match &effect.properties {
        crate::EffectProperties::Particle(p) => {
            use crate::EvalCtx;
            let ctx = EvalCtx::default();
            assert_eq!(p.texture, "circle");
            assert_eq!(p.num_particles.eval(&ctx), 30.0);
            assert_eq!(p.particle_life.eval(&ctx), 24.0);
            assert_eq!(p.particle_speed.eval(&ctx), 5.0);
            assert_eq!(p.particle_size.eval(&ctx), 4.0);
            assert!((p.airdrag.eval(&ctx) - 0.98).abs() < 0.001);
            assert!(!p.directional);
        }
        _ => panic!("expected Particle properties"),
    }
}

#[test]
fn explosion_def_ground_flash() {
    let tdf = Tdf::parse(
        r#"
[oldskool]
{
    [groundflash]
    {
        flashSize = 16;
        flashAlpha = 0;
        circleGrowth = 6.4;
        circleAlpha = 0;
        ttl = 8;
        color = 1,0.6,0.6;
    }
}
"#,
    )
    .unwrap();
    let defs = ExplosionDefs::from_tdf(&tdf);
    let expl = defs.get("oldskool").unwrap();
    assert!(expl.effects.is_empty());
    let flash = expl.ground_flash.as_ref().unwrap();
    assert_eq!(flash.flash_size, 16.0);
    assert_eq!(flash.ttl, 8.0);
    assert!((flash.circle_growth - 6.4).abs() < 0.01);
    assert!((flash.color[0] - 1.0).abs() < 0.01);
    assert!((flash.color[1] - 0.6).abs() < 0.01);
}

#[test]
fn explosion_def_bitmap_flame() {
    let tdf = Tdf::parse(
        r#"
[linkbeam]
{
    [beam]
    {
        class=CBitmapMuzzleFlame;
        [properties]
        {
            sideTexture=linkbeam;
            frontTexture=none;
            dir=dir;
            size=5;
            length=356;
            ttl=60;
        }
        air=1;
        ground=1;
        water=1;
        count=1;
    }
}
"#,
    )
    .unwrap();
    let defs = ExplosionDefs::from_tdf(&tdf);
    let expl = defs.get("linkbeam").unwrap();
    assert_eq!(expl.effects.len(), 1);

    let effect = &expl.effects[0];
    assert_eq!(effect.class, EffectClass::BitmapMuzzleFlame);

    match &effect.properties {
        crate::EffectProperties::Flame(f) => {
            use crate::EvalCtx;
            let ctx = EvalCtx::default();
            assert_eq!(f.side_texture, "linkbeam");
            assert_eq!(f.front_texture, "none");
            assert_eq!(f.size.eval(&ctx), 5.0);
            assert_eq!(f.length.eval(&ctx), 356.0);
            assert_eq!(f.ttl.eval(&ctx), 60.0);
            // `dir=dir` in a CBitmapMuzzleFlame only parses the tokens
            // literally — there's no `EmitVector` enum on this struct;
            // the result is `[d, i, r]` all parsed as unknown opcodes
            // and reduced to an empty expression (0.0). Verified below.
            assert_eq!(f.dir.eval(&ctx), [0.0, 0.0, 0.0]);
        }
        _ => panic!("expected Flame properties"),
    }
}

#[test]
fn explosion_def_spawner() {
    let tdf = Tdf::parse(
        r#"
[corruption_infection]
{
    [smoke]
    {
        class=CExpGenSpawner;
        [properties]
        {
            delay=10 i10;
            explosionGenerator=custom:corruption_infection_smoke;
            dir=0,1,0;
        }
        air=1;
        ground=1;
        water=1;
        count=40;
    }
}
"#,
    )
    .unwrap();
    let defs = ExplosionDefs::from_tdf(&tdf);
    let expl = defs.get("corruption_infection").unwrap();
    assert_eq!(expl.effects.len(), 1);

    let effect = &expl.effects[0];
    assert_eq!(effect.class, EffectClass::ExpGenSpawner);
    assert_eq!(effect.count, 40);

    match &effect.properties {
        crate::EffectProperties::Spawner(s) => {
            use crate::EvalCtx;
            // The parser strips the `custom:` prefix at storage time so
            // lookups through `ExplosionDefs::get(name)` work regardless
            // of whether callers pass the bare or prefixed form.
            assert_eq!(s.explosion_generator, "corruption_infection_smoke");
            // `delay=10 i10` → first spawn at 10, sixth at 60.
            assert_eq!(
                s.delay.eval(&EvalCtx {
                    index: 0,
                    ..Default::default()
                }),
                10.0
            );
            assert_eq!(
                s.delay.eval(&EvalCtx {
                    index: 5,
                    ..Default::default()
                }),
                60.0
            );
        }
        _ => panic!("expected Spawner properties"),
    }
}

#[test]
fn explosion_def_multiple_effects() {
    let tdf = Tdf::parse(
        r#"
[oldskool]
{
    [squarecloud]
    {
        class=CSimpleParticleSystem;
        [properties]
        {
            Texture=square;
            numParticles=15;
            particleSize=14;
        }
        air=1;
        ground=1;
    }
    [tracers]
    {
        class=CSimpleParticleSystem;
        [properties]
        {
            Texture=hline;
            numParticles=15;
            directional=1;
        }
        air=1;
        ground=1;
    }
    [groundflash]
    {
        flashSize = 16;
        ttl = 0;
        color = 1,0.6,0.6;
    }
}
"#,
    )
    .unwrap();
    let defs = ExplosionDefs::from_tdf(&tdf);
    let expl = defs.get("oldskool").unwrap();
    assert_eq!(expl.effects.len(), 2);
    assert_eq!(expl.effects[0].name, "squarecloud");
    assert_eq!(expl.effects[1].name, "tracers");
    assert!(expl.ground_flash.is_some());
}

#[test]
fn explosion_def_empty() {
    let tdf = Tdf::parse(
        r#"
[none]
{
}
"#,
    )
    .unwrap();
    let defs = ExplosionDefs::from_tdf(&tdf);
    let expl = defs.get("none").unwrap();
    assert!(expl.effects.is_empty());
    assert!(expl.ground_flash.is_none());
}

// ── Proptest ────────────────────────────────────────────────────────

mod proptests {
    use proptest::prelude::*;

    use crate::Tdf;

    /// Generate a valid TDF identifier (alphanumeric + underscore, starting with a letter).
    fn arb_ident() -> impl Strategy<Value = String> {
        "[A-Za-z][A-Za-z0-9_]{0,15}".prop_map(String::from)
    }

    /// Generate a TDF-safe value (no semicolons, braces, brackets,
    /// quotes, or slashes that form comments).
    ///
    /// Spring's strict tokenizer treats a leading `"` as the start of a
    /// quoted string and `/* … */` as a block comment, so we exclude
    /// those characters and post-strip any `//` or `/*` runs the
    /// generator may stitch together.
    fn arb_value() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop::char::range('!', '~').prop_filter("TDF-safe char", |c| {
                !matches!(c, ';' | '{' | '}' | '[' | ']' | '"' | '=')
            }),
            0..20,
        )
        .prop_map(|chars| {
            let s: String = chars.into_iter().collect();
            s.replace("//", "").replace("/*", "")
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
            prop_assert_eq!(c.unwrap().get("x"), Some(val.as_str()));
        }
    }
}
