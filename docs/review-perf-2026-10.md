# Review: the 2026-10 perf branch (`perf-2026-10`)

Code review of the two commits since `main` (`bfce990`):

- `b9b8cdd` — *perf: batched effect lifetimes, interned CEG names,
  drain-buffer reuse* (+ the new `docs/perf-plan-2026-10.md`).
- `d45325c` — *rust: idiom cleanup pass, net -80 lines*.

25 files, +652/−490. Both commits are described as behaviour-preserving;
this review checks that claim against the pinned Bevy fork
(`barafael/bevy` @ `6fa5f28`) and the repo's CI gates.

**Verdict:** the behaviour-preserving claims hold up — no semantic
regression found. But **both CI gates fail** on the branch as it stands
(`cargo fmt --all --check` and `cargo clippy -- -D warnings`), so it
cannot merge until those are fixed. Both fixes are mechanical.

**Status:** everything below is addressed — see §5. All four CI gates
are green and the suite is at 858 passed / 0 failed.

## 1. Blocking: the CI gates

`.github/workflows/ci.yml` runs, on every push and PR:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy --target wasm32-unknown-unknown -p kernel-panic --features replay --locked -- -D warnings
```

Both fail on `perf-2026-10`. `main` is clean on both (verified against a
`main` worktree), so everything below is introduced by this branch.

### 1.1 New clippy error — `needless_borrow` *(fixed)*

`kernel-panic/src/units/weapon_fx/spawn.rs:679`

```text
error: this expression creates a reference which is immediately dereferenced by the compiler
  --> kernel-panic/src/units/weapon_fx/spawn.rs:679:40
   |
679 |         let (handle, _) = beam_texture(&tex2, model_cache, images)?;
   |                                        ^^^^^ help: change this to: `tex2`
```

Cause: `beam_texture_names` now returns `(&str, &str)` instead of
`(String, String)`. Three call sites consume it; two were converted
(`beam_texture(tex1, …)`, `CegRegistry::resolve_texture(tex2)`) and this
one was missed. `&String` → `&str` is a real coercion that the lint
tolerates, but `& &str` is a genuinely needless borrow — hence new.

Fix: drop the `&`.

```rust
-        let (handle, _) = beam_texture(&tex2, model_cache, images)?;
+        let (handle, _) = beam_texture(tex2, model_cache, images)?;
```

This breaks the native clippy job **and** the wasm32 clippy job (same
line, same lint).

### 1.2 `cargo fmt --all --check` *(fixed)*

14 diffs across 11 files, all of them in this branch's changed set:

| File | Line |
| --- | --- |
| `kernel-panic/src/interaction/movement.rs` | 314 |
| `kernel-panic/src/map_loading/match_scope.rs` | 165 |
| `kernel-panic/src/map_loading/mod.rs` | 493 |
| `kernel-panic/src/net/session.rs` | 1406 |
| `kernel-panic/src/ui/menu.rs` | 1161 |
| `kernel-panic/src/units/combat/mod.rs` | 920, 1556, 1762 |
| `kernel-panic/src/units/content/weapons.rs` | 191, 199 |
| `kernel-panic/src/units/mechanics/command_fire.rs` | 90 |
| `kernel-panic/src/units/weapon_fx/spawn.rs` | 1259, 1269 |
| `kernel-panic/src/units/weapon_fx/tick.rs` | 132 |

`cargo fmt --all` clears all of them.

### 1.3 What *was* verified green

```text
cargo check --workspace --all-targets   # clean
cargo test  --workspace                 # 657 + 111 + 49 + 26 + 6 + 5 + 3 passed, 0 failed
```

The 20 ignored tests are the cross-platform determinism matrix entries;
the headless determinism suite that runs locally is green, so the sim
hash is untouched (as the commits claim).

## 2. Correctness: the risky changes, checked

Each item below was verified against the pinned Bevy rev, not against the
changelog.

### 2.1 `pick_map`: `NextState::Pending` → `NextState::set`

`bevy_state/src/state/resources.rs:198`:

```rust
pub fn set(&mut self, state: S) {
    *self = Self::Pending(state);
}
```

`set` *is* `NextState::Pending`, so the swap is exactly equivalent. The
one new risk is the `ResMut<NextState<AppState>>` parameter: it panics
if the resource is missing. `main.rs:252` calls
`.init_state::<game_setup::AppState>()` on the same `App` that gets
`MapLoadingPlugin`, and `init_state` inserts `NextState` at build time —
before `Startup` runs `pick_map`. `MapLoadingPlugin` is used nowhere
else (no test app builds it), so there is no second site to break. ✓

### 2.2 Match teardown: per-root `despawn` → one `despawn_all_where`

`bevy_ecs/src/world/mod.rs:1698`. The bulk path:

- collects matching entities first, then despawns them, so the query and
  the despawns cannot interfere;
- tolerates already-despawned entities — the `retain` keeps only those
  where `world.entities.get(e)` is `Ok(None)`, which is the same
  tolerance the old `if let Ok(entity) = world.get_entity_mut(e)` check
  gave;
- still despawns descendants, via the `Children` `on_despawn` component
  hook (`bevy_ecs/src/relationship/mod.rs:332`) queued during the
  despawn and applied by the single trailing `world.flush()`;
- frees the entity indices with one `free_many` instead of one free per
  root — the intended win.

The existing test `teardown_spares_engine_entities`
(`map_loading/match_scope.rs:277`) already covers a doomed parent with a
doomed child (`unit` / `unit_piece`) plus the kept trees (camera, menu),
and it passes. ✓

The despawn *order* changes (Bevy pushes the doomed entities to the
front of a `VecDeque`, i.e. reverse query order). That is immaterial
here: the whole match tree is destroyed, and the sim hash is keyed by
`NetId`, never `Entity`.

### 2.3 CEG expiry: per-entity `despawn` → `despawn_all_where`

`Commands::despawn_all_where` is a deferred command, so the predicate
runs at the flush point, not inside the tick loop. That is safe here
because `CegParticle`, `CegFlame` and `CegSpike` are written by exactly
one system each (`tick_ceg_particles` / `tick_ceg_flames` /
`tick_ceg_spikes`), and the predicate reads the same field the loop just
wrote (`life` / `life_frames` / `alpha`). The dead ones were skipped by
the draw loop and are matched by the predicate afterwards.

`tick_ceg_ground_flashes` and `tick_ceg_delayed_spawns` deliberately keep
per-entity despawns — the flash's predicate reads the *pre-increment*
age, and the delayed timers fire spawn logic on expiry rather than just
dying. The plan doc records this. ✓

### 2.4 `spawn_batch` and determinism

`World::spawn_batch` reserves one contiguous entity range, so the entity
IDs are the same consecutive run a sequence of `spawn`s would have
produced. The *free* order differs from the old per-entity despawn
(bulk frees in reverse query order), which changes which IDs get reused
later — harmless, because the sim hash is keyed by `NetId`:

> `net/state.rs:1-6` — *"a canonical state hash keyed by [`NetId`] (never
> by [`Entity`], whose index differs between machines)"*

So the batched spawns and the bulk despawns cannot move the hash, and
the green determinism suite agrees. ✓

### 2.5 `DamageQueue`: `drain()` → `take()` / `recycle()`

The queue's invariant is documented at `combat/damage.rs:82-87`: there is
deliberately no `clear()`, because producers fire on both sides of a
drain within a frame and an eager clear silently dropped damage.

`take()` swaps `pending` with `spare` (`mem::take` empties `spare`, so
the queue is left holding the previous spare's allocation), and
`recycle()` hands the processed buffer back. Traced across two drains
(`apply_damage`, then `apply_death_blasts` behind `death_system`): the
two buffers alternate roles and both keep their allocation, so there is
no per-tick `Vec` growth.

The invariant still holds: producers that push *during* `explode` land
in the fresh `pending` (the spare), not in the buffer being drained, so
they survive to the next drain. ✓

### 2.6 `resolve_weapon` (three call sites folded into one)

`units/combat/mod.rs:759`. The helper reproduces the original match
arms exactly, including the empty-weapon-name case:

- binding present → `(Some(id), Some(by_id(id)))` — identical;
- no binding, name empty → `(None, None)` — identical, because
  `WeaponRegistry::intern("")` and `get("")` both yield `None`;
- no binding, name set → `(intern(name), get(name))` — identical.

`combat_system` and `attack_ground_system` had both calls already;
`attack_target_system` had only `get(name)`. See §3.1 for the one
consequence of the fold. ✓

### 2.7 `attack_target_system`: folding `target_stats` into `target_q`

```rust
-    target_q: Query<&GlobalTransform, Without<Dying>>,
-    target_stats: Query<&UnitStats>,
+    target_q: Query<(&GlobalTransform, Option<&UnitStats>), Without<Dying>>,
```

The old `target_stats` query had *no* `Without<Dying>` filter, so in
principle a Dying target could still yield stats. In practice it could
not: `target_q.get(order.target)` (which does exclude `Dying`) already
failed first and took the "target died / despawned: stand down" branch,
so the stats were never read for a Dying target. The fold is
equivalent. ✓

### 2.8 Iterator rewrites: tie-break parity

| Site | Old | New | Tie-break |
| --- | --- | --- | --- |
| `map_events/hex_farm.rs:645` | `best.is_none_or(\|(b,_)\| d2 < b)` | `min_by(total_cmp)` | both keep the **first** of equal distances ✓ |
| `ui/menu.rs:1274` | `idx >= hit_stack` | `max_by_key(stack.0)` | both keep the **last** of equal keys ✓ |
| `interaction/movement.rs:312` | manual `break` on `rev <= since` | `take_while(rev > since)` | equivalent on the ascending `changes` ring ✓ |
| `interaction/structures.rs:173` | nested `while` loops with `'outer` | nested `step_by(2).any(any(..))` | equivalent ✓ |
| `interaction/movement.rs:1142` | `cache.get` then `cache.remove` | one `cache.remove`, entry re-inserted | `seen` sweep still sees it ✓ |

The `hex_farm` rewrite also preserves the old filter placement: the
original computed `d2` for every candidate but only kept those with
`d2 <= r2`, and the new code filters on `d2` before the `min_by`. ✓

`total_cmp` vs `<` only differs for `-0.0`/`0.0` and NaN;
`flat_dist_sq` is a sum of squares, so neither occurs. ✓

## 3. Minor notes and follow-ups

### 3.1 `attack_target_system` gained an allocation in the legacy path *(fixed)*

`resolve_weapon` returns *both* the id and the def, but this call site
discards the id:

```rust
let (_, weapon_def) = resolve_weapon(weapon_binding, &unit_registry, &weapon_registry, unit_type.0);
```

The old code called only `weapon_registry.get(name)`, which does one
`to_ascii_lowercase()` allocation. `resolve_weapon` calls `intern(name)`
*and* `get(name)` — and `get` itself calls `intern`, so the fallback
path now does **two** lowercasing allocations instead of one
(`units/content/weapons.rs:247` allocates a `String` per `intern`).

The blast radius is small: only units *without* a `WeaponBinding` reach
the fallback (every unit spawned through `spawn_unit` gets one,
`units/lifecycle/spawning/mod.rs:518`). But this system runs per chaser
per tick, so it is a hot path whenever it does hit.

Either give `resolve_weapon` a def-only variant for this call site, or
make `WeaponRegistry::intern` non-allocating (case-insensitive lookup
against the already-lowercase keys).

### 3.2 Three `Arc<str>` sites still allocate per event *(fixed)*

The registry path interns (`WeaponRegistry::ceg_names`), so
`ceg_name(id)` is a refcount bump. These are not interned and allocate a
fresh `Arc<str>` per event:

- `units/mechanics/command_fire.rs:824` — `ceg_name: ceg.into()`
- `units/mechanics/command_fire.rs:1596` — `ceg_name: "system_sigterm".into()`
- `units/weapon_fx/ceg.rs:951` — `target_ceg: …as_str().into()` (pre-existing)

All three are low-volume (command-fire detonations, SIGTERM bombs, CEG
spawner effects), so this is a tidy-up rather than a hot spot — but the
plan's "no allocation per shot or death" only holds for the registry
path.

### 3.3 `beam_texture` still allocates on the miss path *(fixed)*

`units/weapon_fx/spawn.rs:545`:

```rust
None => load_beam_texture(&format!("{tex1}.tga"), model_cache, images)?,
```

`beam_texture_names` no longer allocates, but this `format!` still does,
once per beam/laser shot whose texture is not in the CEG registry.

### 3.4 `ceg_names` and `defs` must stay parallel *(fixed)*

`units/content/weapons.rs:188-216`. The two vectors are index-parallel
and `ceg_name(id)` indexes `ceg_names` directly, so a divergence would
panic (or return the wrong CEG). Two places make that coupling easy to
break later:

- `ceg_names.push(...)` runs *before* `defs.push(def)`;
- the overwrite path reads the def back out of `self.defs` after the
  move, rather than building the `Arc<str>` first.

```rust
self.ceg_names.push(def.explosion_generator.as_str().into());
self.defs.push(def);
```

is both clearer and order-independent. A `debug_assert_eq!` on the two
lengths would catch a future divergence.

### 3.5 The plan doc is good; the checklist is not *(fixed)*

`docs/perf-plan-2026-10.md` records the API notes (the explicit `()`
filter argument — `despawn_all_where::<&T, _>` is E0283), the
determinism reasoning, and an honest "skipped, with the reason" section
for the four rejected steps. Worth adding `cargo fmt --all --check` and
`cargo clippy -- -D warnings` to the per-step checklist, since both were
missed here.

### 3.6 Housekeeping: untracked, unignored artefacts *(fixed)*

`git status` shows three untracked paths, none of them in `.gitignore`:

```text
.claude/worktrees/                  54 GB   (5 agent worktrees, each with a target/)
pending/                            4 KB
render-settings-comparison.mp4      19 MB
```

`.claude/worktrees/` is 54 GB of build artefacts sitting inside the
working tree. A `git add -A` would sweep all three in. Either move the
worktrees outside the repo or add the paths to `.gitignore`.

## 4. Checklist before merging

- [x] `cargo fmt --all`
- [x] fix the `needless_borrow` in `spawn.rs` (`beam_texture(&tex2, …)` →
      `beam_texture(tex2, …)`)
- [x] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [x] `cargo clippy --target wasm32-unknown-unknown -p kernel-panic --features replay --locked -- -D warnings`
- [x] `cargo test --workspace` — 858 passed, 0 failed
- [x] §3.1 (the `attack_target_system` allocation) and §3.4 (the
      parallel-vector assertion)

## 5. Follow-up (implemented)

What each finding turned into. All four CI gates green, suite at
858 passed / 0 failed (one new test).

| Finding | Fix |
| --- | --- |
| §1.1 clippy | `beam_texture(tex2, …)` — the `&` dropped |
| §1.2 fmt | `cargo fmt --all`; 14 diffs, all pure reflow, no semantic change |
| §3.1 | `resolve_weapon` interns once and derives the def with `by_id` (since §6: resolves a `WeaponKind`) |
| §3.2 | new `CegName` enum; `ExplosionEvent.ceg_name` is no longer `Arc<str>` (since §6: `CegKind`, `CegName` deleted) |
| §3.3 | `tga_filename` spells `{name}.tga` into a stack buffer |
| §3.4 | CEG name interned before the `def` move, `defs` pushed first, `assert_parallel` |
| §3.5 | fmt + both clippy jobs added to the plan's per-step gate |
| §3.6 | `.claude/worktrees/`, `/*.mp4`, `/pending` added to `.gitignore` |

Two notes on the fixes that were not purely mechanical.

**§3.1** — `get(name)` is `intern(name).map(by_id)`, so the fold was
doing the lowercasing twice. Interning once and mapping the id through
`by_id` is the same lookup `get` would have finished, so the fallback
path is back to one allocation — and the two call sites that had always
paired `intern` + `get` (`combat_system`, `attack_ground_system`) drop
to one as well, which is better than `main`.

**§3.2** — interning the scripted CEG names against the `WeaponRegistry`
is not possible: `ability.explosion_ceg`, `SfxKind::Ceg(…)` and the
SIGTERM bomb name are CEG names, not weapon names, so they have no
registry slot to borrow from. A global string interner (mutex or
thread-local) is more machinery than three low-volume sites deserve, so
the event carries a `CegName` instead:

```rust
pub enum CegName {
    Static(&'static str),   // hard-coded: casts, SIGTERM, script emits
    Shared(Arc<str>),       // registry-interned: every shot and death
}
```

Both arms clone in a word, so no event path allocates, and it derefs to
`str` for the `CegRegistry` lookup that is the only reader. `DeathBlast`
keeps its `Arc<str>` (it is always registry-interned) and converts at
the event site.

`CegDelayedSpawn::target_ceg` deliberately stays an `Arc<str>`: a CEG
spawner's `explosion_generator` is parsed out of the TDF, so there is no
`&'static` form to borrow — it was already one allocation shared by
refcount across the `count` timers, which is the floor. Its comment now
says so.

**§3.6** — the paths are ignored rather than deleted. `.claude/` is not
ignored wholesale: `.claude/scheduled_tasks.lock` is tracked and stays
visible. The 54 GB under `.claude/worktrees/` is still on disk — moving
or pruning those worktrees is a separate call, since they may hold
unmerged work.

## 6. Follow-up: the string registries (`2b8b1eb`, `f3a7496`)

§3.2's `CegName` and §3.1's `intern`/`by_id` have since been superseded:
CEGs and weapons are named by enum variants now.

| Before | After |
| --- | --- |
| `CegName::{Static, Shared}` + `Arc<str>` CEG names | `CegKind` — 50 variants, `units/content/cegs.rs` |
| `ExplosionEvent.ceg_name: CegName` | `ceg: Option<CegKind>` |
| `AttackEvent.muzzle_ceg: Option<Arc<str>>` | `Option<CegKind>` |
| `WeaponId(u16)` handed out by `intern()` | `WeaponKind` — 46 variants, one per TDF section |
| `WeaponRegistry`: `Vec<WeaponDef>` + `names` + `index` + `infection_secs` + `KnownWeapons` | `Box<[Option<WeaponDef>]>` indexed by `kind.index()` |
| `weapon_infection_duration(&str)` | `WeaponKind::infection_duration()` |

Worth being honest about why: this is **not** a perf win. `WeaponId` was
already a dense integer index; the move relocates the same index from
runtime to compile time. What it buys is that an invented weapon or CEG
is no longer representable, the registry loses four parallel side tables
whose only job was making names cheap, and the pattern matches
`UnitKind`, which was already `VariantArray` + array-indexed. The set is
closed because the definitions are embedded at compile time and hashed
into `net::compat`, so peers already have to agree on it.

The invariants that keep behaviour identical:

- `WeaponKind::BuildLaser` is variant 0, so the `BUILD_LASER == 0`
  sentinel that gates the build-laser fx still holds; it is a real TDF
  section, so a loaded registry carries the authored def there instead
  of a placeholder.
- `by_kind` stays infallible like `by_id` was. Callers that used to
  degrade on an absent weapon (the SIGTERM credit, the area-denial cost,
  the SIGTERM blast radius) ask `get`/`is_present` instead, so a bare
  test registry behaves as before;
  `the_shipped_tdfs_define_every_variant` pins that production has all
  46.
- `find(name)` is the one entry point for names that arrive from data
  (FBI `Weapon1`/`Weapon2`/`ExplodeAs`, the shield and script paths) and
  returns `None` for a variant these TDFs leave undefined — which is
  what keeps `by_kind` from ever panicking on bad data. It stays
  case-insensitive because the shipped FBIs need it: `counter.fbi` says
  `minekiller`, `rock.fbi` says `rock`.
- `CegKind::NoEffect` is the authored empty `[none]` section, which
  suppresses the generic burst (GaussCannon depends on it); that is
  distinct from `find` → `None`, which keeps the fallback.

Neither enum crosses the wire — nothing in `net/wire.rs`, `net/codec.rs`
or `net/replay_log.rs` names a weapon or a CEG, and the sim hash is
FNV-1a over unit components keyed by `NetId` — so renumbering variants
is netplay- and replay-safe.

Gates: fmt, both clippy jobs, and `cargo test --workspace` at 865 passed
/ 0 failed / 20 ignored. The crate holds 685 `#[test]` where `main` had
677 — one from the review fix, five for `CegKind`, seven for
`WeaponKind`, less the infection test `damage.rs` no longer owns — so
nothing was lost to the mechanical edits.
