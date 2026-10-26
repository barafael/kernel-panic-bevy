# CEGs in source: plan (2026-10)

Move the 50 explosion generators from `kernel-panic/assets/defs/explosions/*.tdf`
(embedded by `build.rs`, parsed on first use by `spring-tdf`, joined to
`CegKind` at load) into Rust source, one `static ExplosionDef` per
`CegKind` variant. First of three steps — CEGs, then weapons, then
units — and the one to do first: a weapon's `explosiongenerator` and
`cegTag` can only become `Option<CegKind>` fields once the CEGs are
variants with definitions behind them, and a unit's `[SFXTypes]` table
can only become `[Option<CegKind>; N]` for the same reason.

Follows `docs/review-perf-2026-10.md` §6 and §7 (the `CegKind` /
`WeaponKind` refactor and its second pass) and
`docs/enum-shaped-inventory.md` §2.1 (the `find` doc that cites
non-shipped data). All line numbers are against `65c434d`.

**Status:** the CEG step is implemented — `009a8c2` wrote the 50 defs
as source (§2, §5.1–5.2), `ccb1207` ran the runtime off them and
deleted the registry (§4, §5.3), `568a777` threaded the weapon's
damage through (§3.1), `3a94ed2` acted on the branch review (shared
`y`/`x` buffers per spawned instance, the `cegTag` trails' remaining
`ttl` in the damage slot, `CanAttack`). Two deviations from the text
below, both deliberate: `kernel-panic/assets/defs/explosions/` is
**not** deleted (§4) — the 38 TDFs stay on disk, un-embedded, as the
generator's input and the equivalence test's reference, because
`upstream/` is a submodule CI does not check out and two of its file
names differ in case; and the CEG texts simply left the compat hash
(§4) rather than being replaced by a code hash, since CEG entities are
fx-hash only. The weapons and units steps named in the first paragraph
have not been started; `CegKind::find` and `resolve_texture(&str)`
stay alive for them (§4).

## 0. What the enum step left half-done

`2b8b1eb` put the *identity* of every CEG in source (`units/content/
cegs.rs`, 50 variants) and left the *definition* in data. The two are
joined at load by `CegRegistry::from_defs` (`weapon_fx/ceg.rs:257-268`)
through `CegKind::find`, and the join can fail in either direction, so
the type proves nothing about presence:

- `CegRegistry::get(kind) -> Option<&ExplosionDef>` (`ceg.rs:289`), with
  `validate_bindings` (`:273`) warning at start-up for a variant without
  a section and `from_defs` logging a section without a variant.
- `spawn_ceg(...) -> bool` (`ceg.rs:542`): `false` when the def is
  absent, and `spawn.rs:265` draws the generic sphere burst in that
  case.
- A test (`ceg.rs:1336`, added in `a0b3c20`) pins that every variant has
  a section, because nothing in the types does.
- Two of the three places that hold a CEG reference *inside the CEG
  data* resolve it by string at spawn time: `spawn_delayed`
  (`ceg.rs:987`) runs `CegKind::find(&props.explosion_generator)` per
  spawner effect and silently queues nothing on a miss.

Every reference the shipped data makes does resolve (checked for this
plan: 61 `[SFXTypes]` entries in the FBIs, 42 `explosiongenerator=` and
7 `cegTag=` in the weapon TDFs, 3 spawner targets — all 50 variants, no
misses). So the whole `Option` / `bool` / warning layer exists for data
that is never shipped, and the tests that exercise the miss path are the
only thing that does.

## 1. Census: what has to be expressed in Rust

| | count |
| --- | --- |
| TDF files / sections (`CegKind` variants) | 39 / 50 |
| TDF lines | 2060 |
| Effect layers | 68 |
| … `CSimpleParticleSystem` | 55 |
| … `CBitmapMuzzleFlame` | 9 |
| … `CExpGenSpawner` | 3 |
| … `explspike` | 1 |
| `[groundflash]` blocks | 12 (in 11 sections; 5 with `ttl > 0`) |
| Distinct texture names | 18 (`square` 22, `circle` 14, `none` 7, `hline` 7, …) |
| Property lines with expression ops | 17 |

The 17 expression lines are the whole "CEG language" the data uses:

```
sizeGrowth=8 r2;            ttl=12 r4;              length=10 r7;
delay=10 i10;  delay=8 i8;  delay=2 i3;             damage=0 i1;  (×3)
pos=-300 r600, 1.0, -300 r600;   pos=-32 r64, 0, 0;   pos=0,0,-32 r64;
pos=0,0 i23,0;               dir=-1 r2, -1 r2, -1 r2;
gravity=0, -0.1 d0.01, 0;                        (system_sigterm.tdf:82)
pos=d1 y0 -1 r2 x0, 0, -1 r2 x0;                 (touhou.tdf:75)
```

Four ops the parser models (`Add`, `Rand`, `Index`, `Damage`,
`spring-tdf/src/explosion.rs:65-77`) and two it does not (`y` yank,
`x` multiply-by-buffer — parsed as `CegOp::Other` and skipped at eval,
`:103`). The earlier estimate of "19 lines, three op kinds" in the
conversation that produced this plan was wrong on both counts; the
table above is from grepping the files.

Everything else in the 2060 lines is a constant, a texture name, a
colour map, a class name or a flag.

## 2. Target shape

### 2.1 One static per variant, no registry

```rust
// units/content/cegs.rs
impl CegKind {
    /// The definition. Total: every variant has one, by construction.
    pub const fn def(self) -> &'static ExplosionDef {
        match self {
            CegKind::OldskoolShot1 => &defs::oldskool::SHOT1,
            CegKind::NoEffect => &defs::NONE,
            // … exhaustive: a variant without a def is a compile error.
        }
    }
}
```

`CegRegistry`, `load`, `from_defs`, `validate_bindings`, `get`, the
`Res<CegRegistry>` parameters and the seven test fixtures that
`insert_resource(CegRegistry::load())` (`tick.rs:1012-1541`,
`spawn.rs:1336`) all go. `spawn_ceg` takes `CegKind`, calls `.def()`,
and returns `()`.

### 2.2 The def types move into the game and lose their strings

Today's `spring_tdf::{ExplosionDef, EffectLayer, EffectClass,
EffectProperties, ParticleProperties, FlameProperties,
SpawnerProperties, SpikeProperties, GroundFlash, CegExpr, CegVec3,
ColorMap, EmitVector}` are already a plain struct tree
(`explosion.rs:338-480`). The source form is the same tree with four
changes, in a new `units/content/ceg_def.rs`:

| today (`spring-tdf`) | in source | why |
| --- | --- | --- |
| `Vec<EffectLayer>`, `Vec<CegOp>`, `Vec<[f32; 4]>`, `String` | `&'static [..]`, `&'static str` | every def is a `static`; no `LazyLock`, no load cost, no heap |
| `EffectLayer { name, class: EffectClass, properties }` | `EffectLayer { name, properties }` | the class is the `EffectProperties` variant; `EffectClass::{Stars, Other}` and `EffectProperties::Raw` have no shipped use (§1) and are deleted |
| `texture: String` | `texture: Option<CegTexture>` | 18 names → enum (§2.4); `none` is `None` |
| `explosion_generator: String` | `target: CegKind` | the spawner's reference becomes a variant; `spawn_delayed`'s `find` and early return go |
| `CegExpr(Vec<CegOp>)` with `Other(char, f32)` | `enum CegExpr { Lit(f32), Ops(&'static [CegOp]) }`, `CegOp::{Add, Rand, Index, Damage, Yank(u8), MulBuf(u8)}` | `Lit` is what `literal()`/`is_literal()` compute today for the ~1900 constant properties; the op set is closed (§3.2) |

The spring-tdf copies stay where they are for now: `WeaponDef` still
holds `explosion_generator: String` / `ceg_tag: String` until step 2,
and nothing else in the game reads `ExplosionDefs` once this lands
(`defs.rs:39` is the only other user).

### 2.3 What a def looks like

`oldskool_shot1.tdf` (41 lines), today:

```
[oldskool_shot1]
{
	[circle]
	{
		class=CSimpleParticleSystem;
		[properties]
		{
			sizegrowth=0.1;
			sizemod=1;
			pos=0, 1.0, 0;
			emitVector=dir;
			…
			Texture=circle;
			colorMap=1 1 1 .1   .2 .2 1 .1   0 0 0 0;
			…
		}
		air=1; ground=1; water=1;
	}
	[groundflash] { flashSize = 16; … ttl = 0; … }
}
```

in source:

```rust
/// `[oldskool_shot1]` — the System faction's bullet impact.
/// Referenced by 20 `[SFXTypes]` entries and 7 weapons.
pub static SHOT1: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "circle",
        count: 1,
        air: true, ground: true, water: true,
        properties: Particle(ParticleProperties {
            texture: Some(CegTexture::Circle),
            color_map: ColorMap(&[
                [1.0, 1.0, 1.0, 0.1],
                [0.2, 0.2, 1.0, 0.1],
                [0.0, 0.0, 0.0, 0.0],
            ]),
            num_particles: Lit(1.0),
            particle_life: Lit(12.0),
            particle_life_spread: Lit(2.0),
            particle_speed: Lit(0.3),
            particle_speed_spread: Lit(0.2),
            particle_size: Lit(8.0),
            particle_size_spread: Lit(2.0),
            size_growth: Lit(0.1),
            size_mod: Lit(1.0),
            emit_rot: Lit(0.0),
            emit_rot_spread: Lit(0.0),
            airdrag: Lit(1.0),
            gravity: ZERO,
            pos: vec3(0.0, 1.0, 0.0),
            emit_vector: EmitVector::Direction,
            directional: false,
            always_visible: false,
        }),
    }],
    // `[groundflash]` with `ttl = 0`: the engine draws nothing (§3.4).
    ground_flash: None,
};
```

and the one line in the data that uses every op kind,
`touhou.tdf:75`:

```rust
pos: CegVec3 {
    x: Ops(&[Damage(1.0), Yank(0), Add(-1.0), Rand(2.0), MulBuf(0)]),
    y: Lit(0.0),
    z: Ops(&[Add(-1.0), Rand(2.0), MulBuf(0)]),
},
```

Roughly 1500 lines of Rust for 2060 of TDF. The colour maps are the
one place the Rust is more readable than the TDF (rows, not a flat
float run); the rest is one-to-one.

### 2.4 Textures

`CEG_TEXTURES` (`ceg.rs:305-337`, 30 aliases) becomes
`enum CegTexture` with `const fn file(self) -> &'static str`. The CEG
data uses 17 of the aliases plus `none`; the other 13 (`bytelaser`,
`pointertrail`, `flowtrail`, `dosray`, `hexgrid`, …) are reached from
weapon TDF `texture1`/`texture2` through `CegRegistry::resolve_texture`
(`spawn.rs:547`, `:1111`, `:1141`). The enum carries all 30 and
`resolve_texture(&str) -> Option<CegTexture>` stays as a free function
for the weapon path until step 2 makes `WeaponDef.texture1` a
`CegTexture` and deletes it.

### 2.5 Layout

`units/content/ceg_defs/` with one module per upstream TDF file
(`corruption_shot1.rs`, `system_sigterm.rs`, `touhou.rs`, …, 39 files)
and the `def()` match in `cegs.rs`. One file per upstream file is
deliberate: a reviewer diffs `ceg_defs/touhou.rs` against
`upstream/Kernel-Panic/gamedata/explosions/touhou.tdf` and nothing
else. A single 1500-line file is the alternative; it greps better and
reviews worse.

## 3. Fidelity decisions the move forces

Writing the data as typed Rust means every place the current pipeline
is lenient has to be decided rather than defaulted. Four of them:

### 3.1 `d` (damage) ops evaluate against zero today

Both `EvalCtx` constructions in `ceg.rs` pass `damage: 0.0` (`:691`,
`:890`) and `spawn_delayed` uses `EvalCtx::default()` (`:993`). Upstream
passes the exploding weapon's damage (`CCustomExplosionGenerator::
Explosion(…, damage, …)`). Two shipped lines depend on it:
`system_sigterm.tdf:82` (`gravity=0, -0.1 d0.01, 0` — the SIGTERM
cloud falls faster for a bigger bomb) and `touhou.tdf:75`
(`[touhou_buildFX]`, the Touhou builders' build sparkle: its `pos`
spread scales with damage; with `d = 0` and `x0` a no-op the spread
today is ±1 elmo plus nothing).

Decision for this plan: **thread damage through.** `ExplosionEvent`
(`shared.rs:146`) does not carry it; add `damage: f32`, fill it from
the weapon def at the three emit sites (`combat/mod.rs`,
`combat/lifecycle.rs`, `command_fire.rs`), pass it into `spawn_ceg` and
from a spawner into its children as upstream does (the spawner's own
`damage` expression, evaluated per index, *is* the child's damage —
that is what `damage=0 i1` means). Writing the folded constants
instead and documenting the deviation is the fallback if the emit
sites turn out awkward; either way the def says what it does.

### 3.2 `y` / `x` ops

`CegOp::Other` exists so the parser round-trips opcodes it does not
implement. In source there is nothing to round-trip: the op set is
whatever the six variants say. Upstream (`ExplosionGenerator.cpp`
`ParseExplosionCode`): `y<n>` stores the accumulator in buffer *n* and
resets it to zero, `x<n>` multiplies the accumulator by buffer *n*. The
buffers are per spawned instance, shared by every property of it
(upstream's `float buffer[16]` in `ExecuteExplosionCode`), which is how
a `y0` in `pos`'s x axis reaches the `x0` in its z axis. The remaining
upstream ops (`a` add-buffer, `p` pow, `q` pow-buffer, `m` sawtooth,
`k` discrete, `s` sine — `ExplosionGenerator.cpp:878-888`) are not in
the data and are not added; a future def that wants one adds the
variant.

### 3.3 `texture=none` layers

Seven layers. Today `resolve_texture("none")` is `None` and
`spawn_particle_system` / `spawn_flame` return early (`ceg.rs:655`,
`:799`). Keep them as `texture: None` with the same early return —
seven lines, and the layer stays visible to a reader comparing against
upstream.

### 3.4 `[groundflash]` with `ttl = 0`

Seven of the twelve. The engine draws nothing for them
(`flashAgeSpeed = 0`; `ceg.rs:615` already checks `gf.ttl > 0.0`).
Write them as `ground_flash: None` with a one-line comment naming the
upstream block, and drop the `ttl` check. The five live ones
(`oldskool_vbig`, `oldskool_big`, `mine`, `network_dissolve`,
`network_spawn`) stay
`Some`.

### 3.5 `[none]`

`CegKind::NoEffect` is `ExplosionDef { effects: &[], ground_flash: None }`
like any other def. The "authored empty vs. lookup miss" distinction
the review had to spell out (`review-perf-2026-10.md` §7) stops
existing because there are no misses.

## 4. What changes, file by file

- `units/content/cegs.rs`: gains `def()`. **`find` stays** — it is still
  the bridge for the three callers whose names come from files still
  in TDF: `weapons.rs:375-376` (`explosiongenerator`, `cegTag`),
  `combat/mod.rs:588` and `command_fire.rs:977` (`[SFXTypes]`). It
  loses only the `spawn_delayed` caller here; step 2 removes the first,
  step 3 the other two, and then `find`, `CEG_BY_NAME` and the
  case-insensitive scan are deleted. Fix the `find` doc per
  `enum-shaped-inventory.md` §2.1 while in the file.
- `units/content/ceg_def.rs` (new): the types of §2.2.
- `units/content/ceg_defs/*.rs` (new): the 50 statics.
- `units/content/defs.rs`: `explosions` field, the `EXPLOSION_TDFS`
  loop in `parse_all`, the `texts()` arm. `build.rs:36`: the
  `assets/defs/explosions` catalog entry. `kernel-panic/assets/defs/
  explosions/` deleted; the originals stay at
  `upstream/Kernel-Panic/gamedata/explosions/`.
- `weapon_fx/ceg.rs`: `CegRegistry` and its impl (`:52-300`, minus
  `resolve_texture`, which moves next to `CegTexture`); `spawn_ceg`
  signature (no registry, no `bool`, plus `damage`); the `Raw` arm;
  `spawn_delayed`'s `find`; `tick_ceg_delayed_spawns`'s `Res`;
  `CegSpawnCtx.ceg_registry` (`:235`); `CegDelayedSpawn` gains the
  child damage. The `the_shipped_tdfs_define_every_ceg_kind` test is
  deleted — it is now the exhaustive match.
- `weapon_fx/spawn.rs`: two `Res<CegRegistry>` params; the
  `if !ceg_spawned { spawn_impact_burst(…) }` branch at `:265` is
  unreachable from a `CegKind` and goes (a `None` muzzle already draws
  nothing, `:147`). `weapon_fx/mod.rs:92` `insert_resource`, `:56`
  re-export.
- `weapon_fx/tick.rs`, `spawn.rs` tests: the seven `CegRegistry::load()`
  fixtures.
- `net/compat.rs:88-95`: the explosion texts drop out of `units_hash`
  with no replacement. CEG entities are **fx-hash only**
  (`net/state.rs:75-81`: `SIM_CATEGORIES` is units, projectiles,
  sigterm, area denial; the `ceg_*` categories are visuals), so two
  peers with different CEG definitions do not desync. Note:
  `perf-plan-2026-10.md` §1 says `net/state.rs` hashes the CEG
  components "into the sim hash"; that is the fx hash, which the
  desync check does not read.
- `spring-tdf`: the `explosion` module, its `pub use` (`lib.rs:27`) and
  seven tests (`tests.rs:973-1240`) become game-unused. Keep them: the
  generator in §5 runs on this parser, and `upstream/` tooling may
  want it. Decide at step 3.

## 5. Procedure

1. **Generate, don't type.** A `#[test] #[ignore]` in `kernel-panic`
   (or a `scripts/gen_ceg_defs.rs` example) that walks
   `defs().explosions` and prints the §2.3 form, `rustfmt`-stable, one
   output file per source TDF, with the TDF's own trailing comments
   (`// flash size`) carried over. Run once, commit the output, keep the
   generator under `scripts/` until step 3 is done.
2. **Hand-tidy** only what the generator cannot know: a doc comment per
   def naming who references it (from the census in §0 — the FBI
   `[SFXTypes]` users and the weapons), and the §3 decisions.
3. **Equivalence gate, before deleting anything.** A test that, for
   every `CegKind`, converts `CegRegistry::load().get(kind)` (old) into
   the new types and asserts `PartialEq` against `kind.def()`, with the
   §3 cases as explicit allowed differences. Land the new defs with
   both paths alive and this test green; delete the old path in the
   next commit.
4. **Replays.** The five `.kpreplay` files in `replays/` must still
   play to the same sim hash — they will, by §4 (fx only), and running
   them is the cheap proof. The fx census per tick
   (`weapon_fx/mod.rs:41`) should match a pre-change run to the
   particle, give or take §3.1.
5. The four gates, as in `perf-plan-2026-10.md`:

   ```sh
   cargo fmt --all --check
   cargo clippy --workspace --all-targets --locked -- -D warnings
   cargo clippy --target wasm32-unknown-unknown -p kernel-panic --features replay --locked -- -D warnings
   cargo test --workspace
   ```

About a day. The generator is an hour; the equivalence test is the
half-day, because it is where every §3 decision gets checked against
the parsed data rather than against memory.

## 6. Not doing, and the skeptical half

- **Not weapons or units.** Separate plans; this one deliberately leaves
  `find` and `resolve_texture(&str)` alive with their remaining callers
  named (§4) so the next step knows what it deletes.
- **Not `const fn` constructors or helper macros** beyond `Lit`,
  `Ops(&[..])`, `vec3(..)` and `ZERO`. Statics of struct literals are
  the whole point: zero load cost, and the text is the data.
- **Not reordering or renaming `CegKind` variants.** Presentation-only
  per `cegs.rs:25-28`, but there is no reason to touch them.
- **Modding.** This closes the "drop a TDF in `assets/defs/explosions`"
  path. It was never open: `build.rs` embeds the directory at compile
  time and `net::compat` refuses a peer whose embedded text differs, so
  a new CEG already needed a rebuild of every client. `additional-
  factions.txt` is the only proposal for new content, and a new faction
  adds variants and statics in source the way the animation drivers
  already do (`Cargo.toml`, the `spring-cob` note).
- **Risk: 1500 lines of literals.** Mitigated by §5.3, which is
  non-negotiable: the TDFs are not deleted until the equivalence test
  has passed against them. The §3.1 damage change is the one
  behavioural change and is isolated to two defs; it can land as its own
  commit after the move if the diff is clearer that way.
- **What this does not buy.** No hot-path win: `CegRegistry::get` was
  already an array read. The gain is the deleted layer (§0) and typed
  references inside the data (§2.2). If that is not worth a day, the
  current state is coherent and can stay.
