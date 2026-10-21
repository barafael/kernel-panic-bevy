# Enum-shaped problems: an inventory of stringly- and int-typed closed sets

Follow-up to the registry refactor in `docs/review-perf-2026-10.md` §6
(`2b8b1eb` `CegKind`, `f3a7496` `WeaponKind`), which replaced two
string-keyed registries with `#[derive(strum::IntoStaticStr,
strum::VariantArray)]` enums. This document asks what else in the tree
has that shape — a **closed set** of values written as a string or an
integer — and answers it for every candidate found.

Nothing here is implemented. §2 is a defect in the enum work that merged
with PR #2. §3 is the shortlist. §4 is what was rejected and why, which is
the useful half: most of the flagged sites should be left alone, and two
of them were flagged on bad evidence.

**Verdict:** five candidates are genuinely enum-shaped. Two are clearly
worth doing. One (`AiDifficulty`) is blocked on persistence, one
(`Facing`) on the `.kpmap` format, and one (census categories) is small
enough to do whenever the file is next open. The sweep also turned up a
doc comment in already-landed work that describes data which is not
shipped.

## 1. The bar

`WeaponKind` was worth doing for one reason: `WeaponRegistry::get(name)`
did a `to_ascii_lowercase()` allocation and a hash lookup **per shot**,
and the set of names was frozen at compile time because `build.rs:34-44`
embeds every `assets/defs/` file with `include_str!` and
`net/compat.rs:91` hashes those texts into the netplay compatibility
key.

That combination is rare, and applying it as a test rejects most of what
a grep-based sweep turns up. The string or integer work in most candidates
happens **at load time**, once per unit definition, not per frame. So
"removes an allocation" is not the argument for anything below. Four
arguments that do survive:

1. **One predicate spelled several times.** A variant compare that has to
   agree in one place, versus string compares that have to agree in
   several (§3.1).
2. **A `_ =>` catch-all over a set that is actually closed.** Exhaustiveness
   checking is free and currently not being used (§3.3, §3.4).
3. **A magic value meaning "none".** `""` for unarmed, `0` for a sentinel,
   `"other"` for uncategorised — an `Option` or a variant says it in the
   type (§3.1, §3.5).
4. **A `panic!` that a type would make a compile error** (§3.2).

Two facts about the tree decide the rest:

- **Compile-time closure.** `kernel-panic/build.rs:34-44` embeds
  `assets/defs/{units/*.fbi, weapons/*.tdf, explosions/*.tdf,
  features/*.tdf, moveinfo.tdf}` as `include_str!` text;
  `units/content/defs.rs:26` pulls it in with `include!`. A name from
  that data can only fail to resolve against non-shipped content, so a
  `find()`-style total lookup is safe — that is what makes `by_kind`
  infallible in both registries.
- **What crosses the wire.** `net/wire.rs` carries a `String` in exactly
  two messages (`Joined{name}` `:167`, `Chat{text}` `:177`) and
  `net/state.rs` hashes integer discriminants, never names. So the
  registries never touched the wire and renumbering them was free. A
  candidate that *is* serialised is not free (§3.3, §3.4).

Summary of what survives:

| # | Site | Shape | Justification | Blocked on |
| --- | --- | --- | --- | --- |
| 3.1 | `UnitRegistry::weapon() -> &str` | string | one predicate in three places; `""` means two things | nothing |
| 3.2 | `SoundId(u8)` | int | `panic!` becomes a compile error; scan becomes a map hit | nothing |
| 3.3 | `AiDifficulty(u8)` {1..=4} | int | `clamp(1, 4)` proves the set; `_ =>` catch-alls | lobby wire + replay files |
| 3.4 | `Facing` (`spring-map`) | int | 4 values, West via `_ =>`, `% 4` instead of exhaustiveness | `.kpmap` format |
| 3.5 | census categories (`net/state.rs`) | string | `&[&str]` membership list + `"other"` sentinel | nothing |

## 2. Defects in the enum work that is already landed

### 2.1 `CegKind::find`'s doc cites data that is not shipped

`units/content/cegs.rs:201-205` justifies the `None` contract like this:

> the shipped data has `custom:sawfunc` (a weapon's
> `explosiongenerator` with no section behind it) and the FBI
> `[SFXTypes]` tables carry `VOID` plus two *weapon* names

All three clauses are false.

- `sawfunc` appears once in the tree, on a commented-out line
  (`assets/defs/weapons/retroweapons.tdf:39`:
  `//explosiongenerator=custom:sawfunc;`). It is not in the parsed data.
- Every one of the 61 `[SFXTypes] explosiongenerator*` values in
  `assets/defs/units/*.fbi` is `custom:…`. There is no `VOID` and no
  weapon name in any `[SFXTypes]` table.
- `VOID` is an `OnlyTargetCategory*` value (`assembler.fbi:66`,
  `counter.fbi:48`, `kernel.fbi:68`, …) — a target category, never passed
  to `CegKind::find`. The code says as much two files away:
  `unit_registry.rs:330-332` documents "`VOID` (Byte's mine launcher, all
  build lasers) therefore disables auto-targeting entirely".
  `minekiller` and `end_game_logic_bomb` are `[UNITINFO] Weapon1` keys
  (`counter.fbi:47`, `logic_bomb.fbi:49`). `[SFXTypes]` is a
  brace-delimited nested block and both of them sit *after* its closing
  `}`, so they belong to `[UNITINFO]` and go to `WeaponKind::find`, not
  `CegKind::find`.

The test comment repeats the claim — `cegs.rs:270-271`, "All three appear
in the shipped data and must keep falling back to the generic burst", on
`names_that_are_not_cegs_resolve_to_nothing`.

The assertions themselves stay correct: they pin `find`'s contract for
names that are not CEGs, which is the contract callers depend on. Only
the prose is wrong, and it is wrong in the direction that matters — it
tells a future reader the shipped data exercises the miss path when it
does not, so a change that made those names resolve would not be caught
by data, only by this test. Fix is a comment rewrite; no behaviour change.

### 2.2 `WeaponKind::Minekiller` attributes a weapon to a unit the game does not model

`units/content/weapons.rs:143-144` documents `Minekiller` as "Counter's
and MineBlaster's mine sweep, and MineBlaster's `ExplodeAs`". That is true
of the FBI data — `counter.fbi:47` does say `Weapon1=minekiller` — but
`counter` is not a `UnitKind` variant, so no spawnable unit fires it (see
§5). The comment is accurate about the data and misleading about the game.
Same class of problem as §2.1: a comment that asserts reachability the
data does not support.

## 3. Worth doing

### 3.1 `UnitRegistry::weapon()` still hands out a `&str`

The refactor stopped at the unit→weapon boundary. `unit_registry.rs:659`
is still:

```rust
/// Primary weapon TDF section name, or `""` if unarmed / only has BuildLaser.
pub fn weapon(&self, kind: UnitKind) -> &str {
    self.def(kind).map_or("", |d| {
        let w = d.weapon1.as_str();
        if w.eq_ignore_ascii_case("BuildLaser") || w.eq_ignore_ascii_case("BuildLaserNoEffect")
        { "" } else { w }
    })
}
```

Eight call sites take that `&str`, and they split cleanly in two. Four
only ask "is this unit armed" — `!unit_registry.weapon(kind).is_empty()`
(`interaction/ability.rs:505`, `interaction/selection/right_click.rs:186`,
`interaction/cursor.rs:211`, `net/apply.rs:557`) — which with an
`Option<WeaponKind>` is `is_some()`. Three re-resolve the name straight
back into a variant (`units/combat/mod.rs:777`,
`units/lifecycle/spawning/mod.rs:512`, `units/lifecycle/bookkeeping.rs:104`)
and one is a test (`units/content/weapons.rs:498`). The third of those
needs a comment to be readable — `bookkeeping.rs:100-102`, "`weapon()`
already filters BuildLaser" — which is the tell that the filter's meaning
lives at the call sites rather than in the type. Meanwhile "is this a
build laser" is spelled three times, in two types, with two deliberately
different meanings:

| Site | Type | Matches | Means |
| --- | --- | --- | --- |
| `unit_registry.rs:662` | `&str`, case-insensitive | both names | "not a combat weapon" |
| `bundle.rs:298-299` | `&str`, case-insensitive | both names | "skip this in the defs test" |
| `weapon_fx/spawn.rs:32` | `WeaponKind` | `BuildLaser` only | "spawn `BuildSparkle`" |

The third is correct and documented — `BuildLaserNoEffect` intentionally
suppresses impact particles — but nothing in the code says the first two
are the same predicate as each other and a *different* predicate from the
third. Returning `Option<WeaponKind>` from `weapon()` makes that explicit:
the first two become one `matches!(kind, BuildLaser | BuildLaserNoEffect)`
helper, the third keeps its own narrow name, and `""`'s double meaning
(unarmed *or* build-laser-only) splits into `None` and a filtered variant.

This is tidying up after `f3a7496`, not a new idea: the perf argument for
pre-binding weapon slots at load was already answered in
`docs/review-perf-2026-10.md` §6 (`find` is an allocation-free map hit),
and still does not apply. The argument is that the predicate is now
expressible as a variant compare and currently is not. No wire, no hash,
no determinism exposure.

### 3.2 `SoundId(u8)` is the old `WeaponId`

`audio/mod.rs:75` — `pub struct SoundId(u8)`, an index into the 26-entry
static `SOUND_FILES` (`:44`), handed out by a case-insensitive linear
scan:

```rust
const fn named(name: &str) -> SoundId { /* while-loop over SOUND_FILES */ }
panic!("sound name not in SOUND_FILES");
```

and `from_name` (`:101`), which does `SOUND_FILES.iter().position(|f|
f.eq_ignore_ascii_case(name))` for every TDF `soundstart=` / `soundhit=`
value at load.

That is structurally identical to the `WeaponId(u16)` / `intern()` pair
`f3a7496` deleted, and the same two gains apply: `named`'s `panic!`
disappears because a variant is declared rather than searched for (a typo
in one of the `pub const BUILT: SoundId` style constants becomes a compile
error instead of a panic at first use), and `from_name` becomes a static
name→variant map hit the way `CEG_BY_NAME` is. The `u8` also caps the
bank at 255 entries with `i as u8` truncating silently in const context
if the list outgrows it — an enum removes that ceiling.

33 references, presentation only: `SoundId` appears nowhere in `net/`, so
it is neither hashed nor serialised. Lowest risk item in this document.

### 3.3 `AiDifficulty(u8)` — `clamp(1, 4)` is the closed-set proof

The set is `{1, 2, 3, 4}` ("Easy … Extreme") and the code says so by
clamping rather than by typing: `net/lobby.rs:1673`,
`room.difficulty = difficulty.clamp(1, 4)`. It is carried as a bare `u8`
in seven places — `game_setup.rs:53`, `:382`, `:563`
(`pub struct AiDifficulty(pub u8)`), `net/lobby.rs:448`, `:522`,
`net/replay_log.rs:88`, `headless/mod.rs:88` — and consumed by integer
matches with catch-alls: `game_setup.rs:461` (`match difficulty { 1 => … }`)
and the test at `:780`, whose fourth arm is `_ =>`. The field directly
above it, `SkirmishConfig::grouping` (`:381`, `difficulty` at `:382`), is
already a real enum — `Grouping` at `:419` — so the precedent is two lines
away in the same struct.

The blocker is persistence, not shape: the value crosses the lobby wire
(29 references in `net/`) and is written into replay files
(`replay_log.rs:88`). An enum therefore needs `#[repr(u8)]` with explicit
`Easy = 1` … `Extreme = 4` discriminants and a checked conversion at both
deserialise sites — the `clamp` cannot just be deleted, because a replay
or a peer from an older build can still hand over an out-of-range byte.
Worth doing, but as its own change with a replay-compat test, not folded
into a sweep.

### 3.4 `Facing` in spring-map

`spring-map/src/datavent.rs:27-30` declares `FACING_SOUTH = 0`,
`FACING_EAST = 1`, `FACING_NORTH = 2`, `FACING_WEST = 3` as four `u8`
consts, and `facing_vector` (`:33`) matches on `facing % 4` with
`_ => (-1.0, 0.0)` for West. The modulo is doing the work an exhaustive
match would do, and the catch-all means a fifth constant added later
silently reads as West.

Four values, one crate, no network use. The numbers are frozen by the
`.kpmap` file format, so the enum needs explicit discriminants 0..=3 and
`from_u8`/`as u8` conversions at the format boundary — same shape of
change as §3.3 but smaller and with only a file format to keep honest.

### 3.5 Census categories

`net/state.rs:48` `CATEGORIES: &[(&str, &str)]` (20 entries) and `:75`
`SIM_CATEGORIES: &[&str]` (5 entries) drive the per-category entity
census, and the set is consumed the stringly way:

- `:218` `.map_or("other", |(_, c, _)| *c)` — `"other"` is a sentinel
  standing for "not in the list"
- `:220` `if cat == "projectile"`
- `:371-372` `filter(|(c, _)| **c != "other")` and
  `SIM_CATEGORIES.contains(cat)`

An enum with an explicit `Other` variant turns all three into `matches!`
and makes "which categories are sim-relevant" a property of the variant
list instead of a parallel `&[&str]` that can drift from `CATEGORIES`.

Two things keep this small rather than important. The names never leave
the process: the hash is built from `rank` integers (`:175`,
`min_by_key(|(_, _, rank)| *rank)` at `:216`), the strings are only
`BTreeMap` keys for the report, and `capture` is the desync-check and
harness path, not a per-frame system. And converting it changes the
census report's *ordering* (a `BTreeMap` over variants instead of over
strings), which is output-only but should be checked against the
harness's expected output.

## 4. Considered and rejected

Recording these because each was flagged by a sweep and each looks
plausible until checked.

| Candidate | Why it looks enum-shaped | Why it is not worth doing |
| --- | --- | --- |
| `MovementClass` (`moveinfo.rs:114`) | 3 values, looked up via `self.defs.get(&class.to_ascii_lowercase())` — an allocation per lookup. Both sides are closed: the FBIs use `light` ×13, `medium` ×9, `heavy` ×3, and `moveinfo.tdf` defines exactly `CLASS0`/`CLASS1`/`CLASS2` | `def_for` runs once per unit **at registry build** (`unit_registry.rs:189`), so 25 times per load; `params_for` is `#[cfg(test)]` (`:120`). No perf case, and the enum adds a parse boundary for nothing |
| `weaponType` shim (`spring-tdf/src/weapon.rs:345-387`) | `derived_weapon_type` matches `self.weapon_type.as_str()` against verbatim CamelCase arms, while the data spells the *key* four ways (`WeaponType`, `weaponType`, `weapontype`) | Checked: all 8 shipped values (`Shield` ×4, `MissileLauncher` ×3, `LaserCannon` ×3, `BeamLaser` ×3, `Melee` ×2, `StarburstLauncher`, `Cannon`, `AircraftBomb`) match an arm exactly. Keys are lowercased at storage, so the key-casing variance is harmless. Fragility only — a test pinning "value casing matches the arms" is cheaper than an enum |
| matchbox `RELIABLE`/`UNRELIABLE` (`net/matchbox.rs:118-119`) | Two `usize` consts duplicating the `Channel` enum in `net/transport.rs:42` | The suggestion was `Channel::Reliable as usize`, which is **worse**: it couples the enum's discriminant order to the matchbox wire channel number. The explicit `match` at `matchbox.rs:509-510` exists precisely to avoid that. Leave alone |
| `ArmorClass` missing `rocky`/`papery`/`irony` | `rpsweapons.tdf:39-41,83-85,127-129` and `retroweapons.tdf:280-282` author the whole rock-paper-scissors damage triangle (`[Rock] irony=600; papery=5; rocky=20`), and `armor_class()` has no arm that can return such a class — so the multipliers are dead data | Not a gap in the enum: `UnitKind` has no `Rock`/`Paper`/`Scissors` variant, so there is no unit to assign the class to. Adding the variants would be wrong. See §5 |
| Unit category token lists (`Category`, `OnlyTargetCategory*`, `BadTargetCategory*`) | 17 distinct tokens across the FBIs (`unit`, `factory`, `builder`, `construction`, `notfactory`, `target`, `notarget`, `edible`, `fast`, `mine`, `nuke`, `vtol`, `void`, `rock`, `paper`, `scissors`, `commander`), compared with `eq_ignore_ascii_case` per token, and `unit_registry.rs:319-325` scans `no_chase_category` for a hardcoded `"VTOL"` | Set-valued, so the shape is a bitset, not an enum — more churn than the payoff. And the intersection is already precomputed at load into the `[attacker][candidate]` bool matrices, so no per-frame string work. The hardcoded `"VTOL"` is a real smell; fix that on its own |
| FBI `side`, `Corpse` | Named fields that look like closed sets | Dead data — `UnitDef::side` has no reader outside `spring-tdf/src/tests.rs`. Delete or ignore, do not enum-ify |
| `NetId`, `TeamId`, piece indices, the 256-slot `TerrainTypes` reader, quantizer bins | Integer ids | Genuinely open value spaces (per-entity or per-map counters). An enum would be a lie |
| `net/state.rs` hashed bitflags, `u32::MAX` sentinels | Magic integers | Frozen by the lockstep protocol; changing the representation changes the hash. Correctly int-typed |

## 5. Closed is not reachable

21 of the 48 FBI files in `assets/defs/units/` are not `UnitKind`
variants: `rock`, `paper`, `scissors`, `counter`, `fairy`, `marisa`,
`alice`, `reimu`, `hand`, `thbase`, `thminifac`, `exparty`, `expbase`,
`expcon`, `expmobile`, `expscout`, `bugold`, `holeold`, `trojanold`,
`windowold`, `wormold`. `UnitKind` is the closed set of units the game can
spawn (`UnitType(pub UnitKind)`, one byte on the wire), so the data layer
is roughly twice the modelled layer.

Three consequences worth stating:

- `WeaponKind`'s 46 variants include weapons no spawnable unit can
  currently fire — `Rock`, `Paper`, `Scissors` are reachable only from the
  unmodelled FBIs.
- The `rocky`/`papery`/`irony` `[DAMAGE]` keys are dead for the same
  reason, not because `ArmorClass` is missing variants.
- The `flyer` `[DAMAGE]` key is commented out in the data while
  `ArmorClass::Flyer` exists and is assigned (`UnitKind::Flow`), so the
  enum is ahead of the data here rather than behind it.

Closing the registry over the *data* set rather than the *reachable* set
is still the right call — it is what makes
`the_shipped_tdfs_define_every_variant` mean something, and it means
adding a unit needs no registry edit. But "closed" and "reachable" are
different properties, and §2.2 is what happens when a comment conflates
them.

## 6. Method

Three passes over `kernel-panic/src`, the `spring-*` crates and
`assets/defs` — one for stringly-typed closed sets, one for integer ones,
one quantitative audit of the asset files (every key with a closed value
vocabulary, with counts) — followed by hand verification of every claim
this document makes. That verification step removed two findings that
were asserted confidently and are wrong (the `weaponType` case-sensitivity
"bug" and the `ArmorClass` "dropped data", both in §4), and found the
defect in §2.1.
