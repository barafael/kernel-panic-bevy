# Deferred Prompts

Items struck through (~~…~~) were addressed in commits 2026-04-17..18.
Everything else is still open; most require focused design work.

## Done

* ~~`std::process::exit` → error handling (item 16)~~
* ~~remove map cycling (item 17)~~
* ~~`load_current_map` / `load_map_at_index` redundant (item 18)~~
* ~~`ALL_UNIT_KINDS` should be derived (item 23)~~
* ~~`SHOWCASE_KINDS` filter instead of hardcoded list (item 24)~~
* ~~special numerical / string values → enums (weapon_type → WeaponCategory) (item 25)~~
* ~~large enum variants: blank lines + doc comments (item 22)~~
* ~~colors → named constants (UI_PANEL_TINT / UI_ROW_BG / UI_OVERLAY_BLACK) (item 36)~~
* ~~`ORDERS` → struct with named fields (item 35)~~
* ~~tighten spring-cob public API (item 32, partial)~~
* ~~functions taking fn pointers → design simplifications (`draw_dashed_polyline`) (item 27, partial)~~

## Still open

### Performance / profiling

* any Bevy performance improvements we can make?
* find which performance tweaks Spring gets away with. Do they apply to us?
* use `cargo-flamegraph`. Do you find anything interesting?

### UX / UI

* dedicated UI pass — match the original layout/styling as closely as possible.
* double-click to select same-kind units in view.
* unit groups (Ctrl-1..9, 1..9 recall).
* fix unit placement when using builders, just like the original.
* no fog of war — entire map always visible, but buildings/units should
  only be revealed when built.
* fix the skybox to match original Kernel Panic.
* glyph_zero / glyph_one: asset vs procedural — which is faster?
* `Interaction`'s default impl and similar patterns — verify we're using
  `Derive(Default)` where possible.

### Multiplayer / deployment

* investigate server-authoritative multiplayer via matchbox + lightyear.
* start on `web-assets-plan.md`.
* implement the game UI (broader than the style pass).

### Rendering / VFX

* investigate the post-processing pipeline of this game vs Spring/KP.

### Housekeeping

* use a walk-dir crate — picked one first. (Not justified: all walks are 1-deep.)
* python-style terse variable names — standalone sweep, not a mechanical fix.
  Most hits so far are math idioms (`t`, `u`, `p`), which stay.
* another pass on special numerical / string values. (Ongoing; covered
  weapon_type so far.)
* simplify `pub fn load_asset_from_disk<T, E: fmt::Display>`.
  (Kept generic — three callers each pass a different parser.)
* reduce arg count on Bevy system fns. Candidates: `ai::ai_brain`,
  `morph::process_morph`, `network_buffer::process_dispatch`,
  `map_loading::load_map`. SystemParam bundle for spawn resources
  would collapse ~35 lines.
* `println`/`eprintln` → logging. Only test output left; fine.
* Inspect lib crate public APIs for idiom/shape (ongoing).
* reduce Bevy system param count (structured SystemParam bundle).
* `cargo llvm-cov` coverage gaps — which tests to add.
* find error paths silently dropped with `let _` / `.ok()` and log them.
  (Two found, both intentional.)
* find `Option`-returning functions that internally map a `Result`.
  (Several, but each wraps a disk-load failure and the caller wants the
  `Option` ergonomics — leaving alone.)
