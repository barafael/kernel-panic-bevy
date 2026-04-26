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

// ── Integration tests: real upstream weapon files ───────────────────

mod real_files {
    use std::path::Path;

    use crate::{Tdf, UnitDefs, WeaponDefs};

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

    fn find_units_dir() -> Option<&'static str> {
        const CANDIDATES: &[&str] = &[
            "upstream/Kernel-Panic/units",
            "kernel-panic/upstream/Kernel-Panic/units",
            "../upstream/Kernel-Panic/units",
        ];
        CANDIDATES.iter().copied().find(|p| Path::new(p).is_dir())
    }

    fn load_unit_file(dir: &str, filename: &str) -> crate::UnitDefs {
        let path = format!("{dir}/{filename}");
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
        let tdf = Tdf::parse(&text).unwrap_or_else(|e| panic!("failed to parse {path}: {e}"));
        crate::UnitDefs::from_tdf(&tdf)
    }

    #[test]
    fn parse_kernel_fbi() {
        let Some(dir) = find_units_dir() else {
            eprintln!("skipping: units dir not found");
            return;
        };
        let defs = load_unit_file(dir, "kernel.fbi");
        let kernel = defs.get("kernel").expect("kernel unit");
        assert_eq!(kernel.max_health, 40000.0);
        assert_eq!(kernel.side, "CPU");
        assert!(kernel.commander);
        assert!(kernel.builder);
        assert_eq!(kernel.object_name, "kernel.s3o");
        assert_eq!(kernel.weapon2, "homebaseshieldgood");
    }

    #[test]
    fn parse_bit_fbi() {
        let Some(dir) = find_units_dir() else {
            eprintln!("skipping: units dir not found");
            return;
        };
        let defs = load_unit_file(dir, "bit.fbi");
        let bit = defs.get("bit").expect("bit unit");
        assert_eq!(bit.max_health, 600.0);
        assert_eq!(bit.max_velocity, 3.0);
        assert_eq!(bit.weapon1, "Line");
        assert_eq!(bit.movement_class, "LIGHT");
        assert!(bit.can_move);
    }

    #[test]
    fn parse_connection_fbi() {
        let Some(dir) = find_units_dir() else {
            eprintln!("skipping: units dir not found");
            return;
        };
        let defs = load_unit_file(dir, "connection.fbi");
        let conn = defs.get("connection").expect("connection unit");
        assert_eq!(conn.max_health, 15000.0);
        assert_eq!(conn.side, "NET");
        assert_eq!(conn.weapon1, "GaussCannon");
        assert_eq!(conn.max_velocity, 1.5);
    }

    #[test]
    fn parse_worm_fbi() {
        let Some(dir) = find_units_dir() else {
            eprintln!("skipping: units dir not found");
            return;
        };
        let defs = load_unit_file(dir, "worm.fbi");
        let worm = defs.get("worm").expect("worm unit");
        assert_eq!(worm.max_health, 12000.0);
        assert_eq!(worm.weapon1, "Wormbite");
        assert_eq!(worm.weapon2, "Wormsplash");
    }

    #[test]
    fn parse_logic_bomb_fbi() {
        let Some(dir) = find_units_dir() else {
            eprintln!("skipping: units dir not found");
            return;
        };
        let defs = load_unit_file(dir, "logic_bomb.fbi");
        let bomb = defs.get("logic_bomb").expect("logic_bomb unit");
        assert_eq!(bomb.max_health, 300.0);
        assert!(bomb.kamikaze);
        assert!(bomb.init_cloaked);
        assert_eq!(bomb.weapon1, "end_game_logic_bomb");
    }

    #[test]
    fn parse_signal_fbi() {
        let Some(dir) = find_units_dir() else {
            eprintln!("skipping: units dir not found");
            return;
        };
        let defs = load_unit_file(dir, "signal.fbi");
        let signal = defs.get("signal").expect("signal unit");
        assert!(signal.can_fly);
        assert_eq!(signal.cruise_alt, 200.0);
        assert_eq!(signal.max_velocity, 8.0);
    }

    #[test]
    fn parse_all_kp_unit_files() {
        let Some(dir) = find_units_dir() else {
            eprintln!("skipping: units dir not found");
            return;
        };
        let kp_units = [
            "kernel.fbi",
            "assembler.fbi",
            "bit.fbi",
            "byte.fbi",
            "pointer.fbi",
            "socket.fbi",
            "firewall.fbi",
            "hole.fbi",
            "bug.fbi",
            "exploit.fbi",
            "worm.fbi",
            "virus.fbi",
            "dos.fbi",
            "window.fbi",
            "logic_bomb.fbi",
            "connection.fbi",
            "port.fbi",
            "packet.fbi",
            "signal.fbi",
        ];
        for filename in &kp_units {
            let defs = load_unit_file(dir, filename);
            assert!(
                !defs.units.is_empty(),
                "{filename} should contain at least one unit definition"
            );
            for (name, u) in &defs.units {
                assert!(
                    !name.is_empty(),
                    "unit id should not be empty in {filename}"
                );
                assert!(
                    u.max_health > 0.0,
                    "{name} in {filename}: max_health should be positive"
                );
                assert!(
                    !u.object_name.is_empty(),
                    "{name} in {filename}: must have an object_name"
                );
            }
        }
    }

    #[test]
    fn parse_bit_fbi_sfx_types() {
        let Some(dir) = find_units_dir() else {
            eprintln!("skipping: units dir not found");
            return;
        };
        let path = format!("{dir}/bit.fbi");
        let text = std::fs::read_to_string(&path).unwrap();
        let tdf = Tdf::parse(&text).unwrap();
        let defs = UnitDefs::from_tdf(&tdf);
        let bit = defs.get("bit").expect("bit unit");
        // Bit's FBI declares both `explosiongenerator0=custom:oldskool_shot1`
        // and `explosiongenerator1=custom:oldskool_shot2`; the COB's
        // `emit-sfx 1025 from gunpoint` maps to index 1 (arrowflare).
        assert_eq!(bit.sfx_types.len(), 2);
        assert_eq!(bit.sfx_types[0], "custom:oldskool_shot1");
        assert_eq!(bit.sfx_types[1], "custom:oldskool_shot2");
    }

    #[test]
    fn parse_pointer_fbi_single_sfx_type() {
        let Some(dir) = find_units_dir() else {
            eprintln!("skipping: units dir not found");
            return;
        };
        let path = format!("{dir}/pointer.fbi");
        let text = std::fs::read_to_string(&path).unwrap();
        let tdf = Tdf::parse(&text).unwrap();
        let defs = UnitDefs::from_tdf(&tdf);
        let p = defs.get("pointer").expect("pointer unit");
        // Pointer declares only generator 0 — emit-sfx 1024 from the
        // `FireWeapon1` COB body plays the soft-blue `oldskool_shot1`.
        assert_eq!(p.sfx_types.len(), 1);
        assert_eq!(p.sfx_types[0], "custom:oldskool_shot1");
    }

    fn find_explosions_dir() -> Option<&'static str> {
        const CANDIDATES: &[&str] = &[
            "upstream/Kernel-Panic/gamedata/explosions",
            "kernel-panic/upstream/Kernel-Panic/gamedata/explosions",
            "../upstream/Kernel-Panic/gamedata/explosions",
        ];
        CANDIDATES.iter().copied().find(|p| Path::new(p).is_dir())
    }

    #[test]
    fn parse_corruption_burst_explosion() {
        let Some(dir) = find_explosions_dir() else {
            eprintln!("skipping: explosions dir not found");
            return;
        };
        let path = format!("{dir}/corruption_burst.tdf");
        let text = std::fs::read_to_string(&path).unwrap();
        let tdf = Tdf::parse(&text).unwrap();
        let defs = crate::ExplosionDefs::from_tdf(&tdf);
        let burst = defs.get("corruption_burst").expect("corruption_burst");
        assert!(!burst.effects.is_empty());
        assert_eq!(
            burst.effects[0].class,
            crate::EffectClass::SimpleParticleSystem
        );
    }

    #[test]
    fn parse_linkbeam_explosion() {
        let Some(dir) = find_explosions_dir() else {
            eprintln!("skipping: explosions dir not found");
            return;
        };
        let path = format!("{dir}/linkbeam.tdf");
        let text = std::fs::read_to_string(&path).unwrap();
        let tdf = Tdf::parse(&text).unwrap();
        let defs = crate::ExplosionDefs::from_tdf(&tdf);
        let beam = defs.get("linkbeam").expect("linkbeam");
        assert!(!beam.effects.is_empty());
        assert_eq!(beam.effects[0].class, crate::EffectClass::BitmapMuzzleFlame);
    }

    #[test]
    fn parse_all_explosion_files() {
        let Some(dir) = find_explosions_dir() else {
            eprintln!("skipping: explosions dir not found");
            return;
        };
        let entries = std::fs::read_dir(dir).unwrap();
        let mut count = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "tdf") {
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
                let tdf = Tdf::parse(&text)
                    .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()));
                let defs = crate::ExplosionDefs::from_tdf(&tdf);
                // Every file should produce at least one explosion def.
                assert!(
                    !defs.explosions.is_empty(),
                    "{} should contain at least one explosion",
                    path.display()
                );
                count += defs.explosions.len();
            }
        }
        assert!(count > 10, "expected many explosion defs, got {count}");
    }

    /// Pin the parsed structure of System-faction CEGs against the
    /// actual upstream files. If an upstream rename or a parser
    /// regression drops `oldskool_impact`'s yellow→red→black gradient,
    /// or downgrades the `system_nx` spawner to `count=1`, or loses the
    /// `particleSpeed=12 d-.5` damage-scaled decay on `system_sigterm`,
    /// this test catches it before it shows up as a visual bug.
    #[test]
    fn parse_system_faction_ceg_corpus_details() {
        use crate::{EffectClass, EffectProperties, EvalCtx};
        let Some(dir) = find_explosions_dir() else {
            eprintln!("skipping: explosions dir not found");
            return;
        };

        // Aggregate every *.tdf in the folder so we can query named
        // CEGs regardless of which file they live in.
        let mut merged = crate::ExplosionDefs::default();
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "tdf") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let tdf = Tdf::parse(&text).unwrap();
            merged.merge(crate::ExplosionDefs::from_tdf(&tdf));
        }

        // ── oldskool_shot1 — bit muzzle + fly-path CEG ────────────────
        let shot1 = merged.get("oldskool_shot1").expect("oldskool_shot1");
        assert_eq!(shot1.effects.len(), 1);
        let EffectProperties::Particle(p) = &shot1.effects[0].properties else {
            panic!("expected Particle");
        };
        assert_eq!(p.texture, "circle");
        // White-faint → blue-faint → transparent-black (3 stops).
        assert_eq!(p.color_map.stops.len(), 3);
        let blue_stop = p.color_map.stops[1];
        assert!(
            blue_stop[2] > 0.9 && blue_stop[0] < 0.3,
            "stop 1 should be dominantly blue, got {blue_stop:?}"
        );
        assert_eq!(p.emit_vector, crate::EmitVector::Direction);

        // ── oldskool — byte MegaBeam impact (yellow→red→black squares) ─
        let old = merged.get("oldskool").expect("oldskool");
        // Three top-level emitters: [squarecloud], [tracers] + ground-flash.
        let particle_count = old
            .effects
            .iter()
            .filter(|e| matches!(e.class, EffectClass::SimpleParticleSystem))
            .count();
        assert_eq!(particle_count, 2, "oldskool has squarecloud + tracers");
        assert!(old.ground_flash.is_some());

        // ── oldskool_impact — pointer Geometric impact (big white flash) ─
        let imp = merged.get("oldskool_impact").expect("oldskool_impact");
        assert!(
            imp.effects.len() >= 3,
            "oldskool_impact has circle+bigcircle+tracers"
        );
        // `bigcircle` is a single 128-elmo flash — check one emitter's
        // particleSize reaches that magnitude.
        let has_big = imp.effects.iter().any(|e| {
            if let EffectProperties::Particle(p) = &e.properties {
                p.particle_size.eval(&EvalCtx::default()) >= 64.0
            } else {
                false
            }
        });
        assert!(
            has_big,
            "oldskool_impact must contain the big-flash emitter"
        );

        // ── system_nx — 240 delayed respawns of system_nx_fire ─────────
        let nx = merged.get("system_nx").expect("system_nx");
        let spawner = nx
            .effects
            .iter()
            .find(|e| e.class == EffectClass::ExpGenSpawner)
            .expect("system_nx has a CExpGenSpawner");
        assert_eq!(spawner.count, 240);
        let EffectProperties::Spawner(s) = &spawner.properties else {
            panic!("expected Spawner");
        };
        assert_eq!(s.explosion_generator, "system_nx_fire");
        // `delay=8 i8` → spawn #n fires at 8 + 8n frames. Check the
        // last one: 8 + 8*239 = 1920.
        assert_eq!(
            s.delay.eval(&EvalCtx {
                index: 239,
                ..Default::default()
            }),
            1920.0
        );

        // ── system_sigterm — 20 respawns + damage-scaled rising speed ──
        let sig = merged.get("system_sigterm").expect("system_sigterm");
        let rising = sig
            .effects
            .iter()
            .find(|e| e.class == EffectClass::ExpGenSpawner)
            .expect("system_sigterm has a CExpGenSpawner");
        assert_eq!(rising.count, 20);

        // ── system_sigterm_fire — squarecloud with speed=12 d-.5 ──────
        let fire = merged
            .get("system_sigterm_fire")
            .expect("system_sigterm_fire");
        let EffectProperties::Particle(p) = &fire.effects[0].properties else {
            panic!("expected Particle");
        };
        // With damage=0 the speed is the literal 12. With damage=10 it
        // falls to 12 + 10 * -0.5 = 7.
        assert_eq!(
            p.particle_speed.eval(&EvalCtx {
                damage: 0.0,
                ..Default::default()
            }),
            12.0
        );
        assert_eq!(
            p.particle_speed.eval(&EvalCtx {
                damage: 10.0,
                ..Default::default()
            }),
            7.0
        );
        // gravity has a d-component on Y: `0, -0.1 d0.01, 0`.
        let grav_zero = p.gravity.eval(&EvalCtx {
            damage: 0.0,
            ..Default::default()
        });
        assert!((grav_zero[1] - -0.1).abs() < 1e-4);
        let grav_damaged = p.gravity.eval(&EvalCtx {
            damage: 100.0,
            ..Default::default()
        });
        assert!(
            (grav_damaged[1] - (-0.1 + 100.0 * 0.01)).abs() < 1e-4,
            "gravity.y with damage=100: {}",
            grav_damaged[1]
        );

        // ── linkbeam — CBitmapMuzzleFlame reaches through front_texture ─
        let link = merged.get("linkbeam").expect("linkbeam");
        let EffectProperties::Flame(f) = &link.effects[0].properties else {
            panic!("expected Flame");
        };
        assert!(
            !f.side_texture.is_empty(),
            "linkbeam must carry a side texture"
        );
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
