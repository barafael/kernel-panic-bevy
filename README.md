# Kernel Panic (Rust / Bevy remake)

A Rust and Bevy remake of [Kernel Panic](https://springrts.com/wiki/Kernel_Panic),
the Spring RTS game about computers (`kernel-panic/kernel-panic.md` has its
history). Playable natively and in the browser; lockstep multiplayer over
WebRTC (`docs/multiplayer-plan.md`).

```sh
cargo run --release -p kernel-panic              # the game
cargo run --release -p kernel-panic Central_Hub  # straight into a map
cargo test --workspace                           # needs `git lfs pull` (the music)
```

## Assets

The original game is frozen, so its content lives in this repository in
the formats the game reads, with no conversion step anywhere:

- `kernel-panic/assets/defs/`: every unit, weapon, explosion and move
  class definition, the original text files, parsed at startup.
- `kernel-panic/assets/units.kpu`: every model and texture
  (`unit-bundle.md`). Both are embedded in the binary.
- `kernel-panic/assets/maps/*.kpmap`: the 18 maps (`spring-map/src/baked.rs`
  documents the format). Native reads them from `assets/`, the web build
  fetches them.
- `kernel-panic/assets/{sounds,music,cursors,unitpics,bitmaps}`: the
  sound effects, the soundtrack (Git LFS), the mouse cursors, the build
  pictures and the tooltip background.

Provenance and licences: `original-game-license-info.txt`.

The maps were converted once from the original Spring `.sd7` / `.sdz`
archives. The converter (`spring-map`'s `bake-map`), the Spring archive,
SMF, SMT and SMD readers, the captured Lua gadget outputs, the
`spring-map-gen` crate that generated Circular Buffer and Stack Overflow
(and its `map-viewer`), and the CI bake step were all removed in the
commit after `81f4e96`; that commit is the last with the tools and the
source archives, should a map ever need regenerating:

```sh
git show 81f4e96:spring-map/src/bin/bake-map.rs
git checkout 81f4e96 -- spring-map spring-map-gen kernel-panic/assets/maps
```

There is no editable sidecar for the baked data (start positions, terrain
types, datavent facings all live inside the `.kpmap`); if one is ever
needed, the reader is the place to add it.

The datavent facings (which way a building on each vent faces, and so
which way a Socket's or Window's units leave) can be edited in place:

```sh
KP_MAP_EDIT=Central_Hub cargo run -p kernel-panic
```

opens a sandbox match on the map with an arrow on every datavent. Hover
a vent: `V` turns it clockwise, `Shift+V` counter-clockwise (the match
takes the change at once, so a Socket built there shows it); `Ctrl+S`
writes the facings into the `.kpmap` and changes nothing else in it
(`kernel-panic/src/map_editor.rs`). An edited map has a new hash: the
golden replays on it (Speed_Balls_16_Way, Quad_Core, Dual_Core,
Marble_Madness_Map) must be re-recorded.

The models and textures were likewise baked once from the
`upstream/Kernel-Panic` and `upstream/RecoilEngine` submodules
(`unit-bundle.md` has the pointer into history), and the sound effects
and the soundtrack were converted once by `scripts/bake-sounds.sh` and
`scripts/bake-music.sh`, both last present at `a8567de`:

```sh
git show a8567de:scripts/bake-sounds.sh
git show a8567de:scripts/bake-music.sh
```

The submodules are reference material only (the Lua gadgets and engine
sources the port quotes); no build, test or workflow reads them.
