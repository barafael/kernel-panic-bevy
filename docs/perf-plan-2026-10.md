# Performance / modern-Bevy plan (2026-10 review)

Follow-up to `docs/perf-improvements.md`, from the post-0.20 review of the
whole tree. Baseline harness unchanged:
`KP_PROFILE=1 KP_EXIT_AFTER=3000 KP_TIME_SCALE=4 …` (`src/profile.rs`),
per-system trace via `KP_TRACE=1`, effect census per tick
(`weapon_fx/mod.rs`).

Everything here is against the pinned Bevy 0.20 fork
(`barafael/bevy` @ `6fa5f28`), so every API named below exists in the
rev we build — verified against the checkout, not the changelog.

## 1. CEG effects: batch the spawns, batch the despawns (do first)

The render side is already one dynamic mesh per material
(`weapon_fx/batch.rs`), so a `CegParticle` entity is pure ECS overhead:
one component, no `Transform`, no render node of its own.

- `ceg.rs` `spawn_particle_system` (~:718): one `commands.spawn()` per
  particle in a nested loop → collect into a `Vec` and
  `Commands::spawn_batch`. Order is preserved, so the per-projectile
  RNG streams and query iteration order are untouched.
- Same for `spawn_flame` (~:780) and the spike loop (~:861).
- Tick loops (`tick_ceg_particles`/`flames`/`ground_flashes`, ~:1105,
  :1142, :1194): replace per-entity `commands.entity(e).despawn()` with
  one `Commands::despawn_all_where::<&T, _>(|t| t.dead())` per system
  (0.20 bulk-despawn path, 1.5–1.7× at 1k–10k entities).
- **Determinism note:** `net/state.rs` hashes `::CegParticle`,
  `::CegFlame`, `::CegGroundFlash`, `::CegSpike`, `::CegDelayedSpawn`
  into the sim hash. Batching changes *when* the world applies the
  commands (same tick, same order), not the resulting state, so the
  hash is unaffected. The headless determinism tests must stay green.

## 2. String churn on the shot path (do first)

- `spawn.rs` `beam_texture_names() -> (String, String)` allocates two
  Strings per beam/laser shot. Every returned value is either empty, a
  `&'static` default, or a trimmed slice of the `WeaponDef` — return
  `(&str, &str)` borrowed from the def instead.
- `ExplosionEvent.ceg_name: String` (`weapon_fx/shared.rs`) is cloned
  per attack (`combat/mod.rs`) and per death blast
  (`combat/lifecycle.rs`). Intern: `Arc<str>` owned by the cached
  per-unit/per-weapon stats (the `Arc<str>` pattern already exists at
  `ceg.rs` delayed spawns), cloned as a refcount bump.

## 3. Per-frame allocations with an established fix (do first)

- `ground_move.rs` ground-collision system (~:2472): collects **and
  sorts all map features every tick**; the set is static per map.
  Cache the sorted snapshot in a `Local`, rebuild on add/remove.
- `damage.rs` splash pass (~:708, :857): `drain().collect()` +
  per-explosion `Vec` → reuse buffers (the `Local` pattern at :698 is
  the house style).
- `ground_move.rs` (~:2509/:2512): per-run `crushed`/`knocked` Vecs →
  `Local`.
- `movement.rs` `CommandQueue::remove(0)` (~:564): memmove per pop.
  Only if the component's serialization/hash shape stays identical —
  otherwise leave; queues are short.

## 4. Render check: fx batch entities vs shadows

`batch.rs` batch entities carry `NoFrustumCulling` but no
`NotShadowCaster`: a world-space dynamic mesh covering the map is a
shadow-map candidate for free. Verify against the shadow setup; add if
they cast.

## 5. Match teardown: bulk despawn

`match_scope.rs` `despawn_match_entities` despawns roots one at a time
— the visible hitch at match swap. Invert the keep-list into a
`Without<KeepAcrossMatch>`-style filter and use `Commands::despawn_all`
(0.20 bulk path); keep the stray-warning sweep.

## 6. Menu state machine → nested states (scoped pilot)

`ui/menu.rs` runs `MenuPage` as a plain resource + four Esc-overlay
bool resources, and `maintain_launch_menu` rebuilds the whole page tree
on a watched-tuple `is_changed()`. The low-risk slice:

- Nested states for the Esc overlay (`Closed/Main/Settings/Audio/
  ResignConfirm`) + `DespawnOnEnter`, replacing the bool resources and
  the `Local<Option<EscMenuDrawn>>` tuple diff.
- `EditableText` (0.19) for the lobby text fields, replacing
  `lobby_page.rs` `type_into_fields` (IME/paste/selection for free).
- `bsn!` pilot on the esc menu only — it is two releases old and the
  release notes say to expect rough edges; no wholesale `menu.rs`
  migration.
- `map_loading/mod.rs` legacy `insert_resource(NextState::Pending(…))`
  → `next_state.set`.

Full `MenuPage`-as-sub-state conversion of `maintain_launch_menu` is
deliberately **not** in this plan: the signature-hash rebuild is a
performance decision (no per-frame relayout), and the file is the
largest UI surface we have.

## 7. Explicitly not doing (reviewed and rejected)

- Parallelizing `FixedUpdate` — divergence + measured 1.12 vs 1.69 ms
  at ~400 units (`units/mod.rs` comment).
- Events/observers over the sim's queue-resources — the write-then-
  drain discipline is load-bearing for Produce→Resolve lockstep order.
- CEG-as-resource-pool (dropping the entities entirely) — would work
  (single component, batched rendering) but `state::capture` hashes
  those components; re-baselining desync coverage is only worth it if
  steps 1–2 leave the census still hot.
- `init_match_resource` → resources-as-components — elegance, not
  speed; the registry's explicit ordering (seed first) is a feature.
- `AssetServer` migration for `S3OModelCache`; per-piece-entity
  rework; `bsn!` everywhere.

## Order

1 → 2 → 3 → 4 → 5 → 6, measuring with the census after 1–2.
Each step lands with the CI gates green, not just the suite — the
first pass ran only `cargo check` + `cargo test` per step and landed
with `fmt` and `clippy` red (see `review-perf-2026-10.md` §1):

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy --target wasm32-unknown-unknown -p kernel-panic --features replay --locked -- -D warnings
cargo test --workspace   # headless determinism included
```

## Outcomes (implemented 2026-10)

Done, each landed with `cargo check` clean and the suite green (the
gates above were added to the loop by the review follow-up):

- **1.** `spawn_batch` in `spawn_particle_system` / `spawn_flame` /
  `spawn_spikes` / `spawn_delayed`; `despawn_all_where` in the
  particle / flame / spike tick systems. `tick_ceg_ground_flashes` and
  `tick_ceg_delayed_spawns` deliberately keep per-entity despawn: the
  flash's despawn predicate reads the *pre-increment* age (a bulk
  predicate on the stored age shifts the float boundary by rounding),
  and the delayed timers fire spawn logic on expiry, not just die.
  API note: the filter parameter needs an explicit `()` —
  `despawn_all_where::<&T, _>` is E0283.
- **2.** `beam_texture_names` returns `(&str, &str)` borrowed from the
  def (an inner `fn`, not a closure — a closure can't express the
  lifetime tie). `DeathBlast.ceg_name` is an `Arc<str>` interned per
  weapon in `WeaponRegistry::ceg_names` (`ceg_name(id)` = refcount
  bump); `ExplosionEvent.ceg_name` is a `CegName`, which adds a
  `&'static str` arm so the scripted detonations (ability casts, the
  SIGTERM bomb, a script's `SFX_CEG_BASE` emit) borrow instead of
  allocating per event — the registry path alone did not make the
  "no allocation per shot or death" claim true. `tga_filename` spells
  the un-aliased `{name}.tga` beam lookup into a stack buffer for the
  same reason. `PendingExplosions` is not in the sim hash, so the type
  changes are hash-neutral.
- **3.** `DamageQueue` gained `take`/`recycle` — the two drain points
  ping-pong two `Vec` allocations per tick instead of `collect`ing;
  `DamagePass` gained a `splash_mapped` `Local` for the per-explosion
  `(Hit, due-frame)` mapping. `crushed`/`knocked` in the ground
  collision system are `Local`s.
- **5.** `despawn_match_entities` is one
  `World::despawn_all_where::<Entity, Or<…>>` — single deferred flush
  plus `free_many` instead of a recursive `despawn` (and flush) per
  root. Verified against the pinned rev: recursion and
  already-despawned tolerance are built in, and no `On<Despawn>`
  observers exist whose ordering could matter.
- **6 (partial).** `pick_map`'s legacy
  `insert_resource(NextState::Pending(…))` → `NextState::set`.

Skipped, with the reason (the skeptical half):

- **3, feature sort:** caching the sorted feature order means
  re-`get`ting each entity per tick — slower than sorting ~200 items,
  and features are *not* static per match (Bad Blocks turn into
  features, crushes remove them). The collect+sort stays.
- **3, `CommandQueue`:** `Vec`→`VecDeque` touches 8+ construction
  sites and `queue_build`'s signature to save a memmove on sub-five
  queues — the plan's own escape clause applies.
- **4:** no-op. `shadow_maps_enabled: false` on the only directional
  light (`map_loading/mod.rs`), so nothing casts; `NotShadowCaster`
  would decorate nothing.
- **6, `EditableText` lobby fields:** not a drop-in. The editing state
  *is* the `LobbyRoom` resource (netcode reads it), the field buttons
  double as the display, Space/Enter on an empty field belong to the
  menu's nav, and the page rebuild is signature-hashed — swapping in
  the widget is a lobby-input redesign, not a pilot. Same verdict for
  the Esc-overlay nested-state conversion: the bool resources + the
  `Local` diff tuple are cheap, and the change fans out across every
  `MenuAction::Goto` handler. Both stay follow-ups.

