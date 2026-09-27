# The unit bundle (`kernel-panic/assets/units.kpu`)

Everything the game needs from the original Kernel Panic data — unit
FBIs, weapon and explosion (CEG) TDFs, `MOVEINFO.TDF`, the `.s3o`
models and the `.tga` textures — is baked once into a single file,
`kernel-panic/assets/units.kpu`, which is committed and embedded in the
binary with `include_bytes!`. The runtime never reads `upstream/`; native
and web builds get their registries and models from the same bytes.
The reader lives in `kernel-panic/src/units/content/bundle.rs`.

## Re-baking

The bake needs the `upstream/Kernel-Panic` and `upstream/RecoilEngine`
submodules checked out. It is a dev mode of the game binary (the parsers
and registries live in its modules), selected by `KP_BAKE_UNITS`:

```sh
KP_BAKE_UNITS=kernel-panic/assets/units.kpu cargo run -p kernel-panic
```

It writes the bundle and exits without starting the game, printing a
summary and a warning for every unit kind, model or texture the game
would ask for that the checkout does not have. Rebuild afterwards — the
bundle is compiled in — and commit the new file. The bake is
deterministic (inputs sorted, `BTreeMap`s throughout), so an unchanged
upstream produces a byte-identical bundle.

Because the file is `include_bytes!`d, the crate does not compile
without it; for a first bake from scratch create an empty placeholder
(`: > kernel-panic/assets/units.kpu`) — it decodes as the empty bundle,
and the bake never reads it.

Re-bake when:

- the upstream submodules move (new unit stats, models, effects);
- the parsed types gain fields (`spring_tdf::{UnitDef, WeaponDef, …}`,
  `spring_unit_mesh::S3OModel`, `MoveClassTable`): postcard is not
  self-describing, so the layout is versioned by the file magic
  (`kpunit1\0`) — bump it in `bundle.rs` and re-bake, and a stale bundle
  fails loudly instead of mis-decoding.

The test `bundle_matches_upstream_checkout` compares the committed
bundle against a fresh bake whenever the submodules are present, so
`cargo test -p kernel-panic` catches a forgotten re-bake.

## Contents and format

```text
magic    : 8 bytes  "kpunit1\0"
body_len : u32      little-endian length of the zstd frame
body     : ZSTD(postcard(UnitBundle))
```

`UnitBundle` holds the merged `units/*.fbi` (`UnitDefs`), the parsed
`MOVEINFO.TDF` (`MoveClassTable`), the merged `weapons/*.tdf`
(`WeaponDefs`), the merged `gamedata/explosions/*.tdf`
(`ExplosionDefs`), every `objects3d/*.s3o` parsed (`S3OModel`), and the
textures: every `unittextures/*.tga` and `bitmaps/kpsfx/*.tga` plus the
engine bitmaps the effects reference (`laserend.tga` from
`RecoilEngine/cont/base/bitmaps/bitmaps`). All maps are keyed by the
lower-cased file name; lookups are case-insensitive like the original
game's VFS. Where a name exists in several directories the first in the
old on-disk search order wins (`unittextures` before `kpsfx`).

Textures are the bulk of the data (37 MB of RGBA pixels, mostly flat
colour), so each is its own zstd frame inside the payload and is only
decoded when a model or effect first asks for it; the outer frame
decodes in ~10-35 ms at startup. The whole file is ~1.7 MB. The encoder
(C `zstd`, level 19) links on native only; the reader is the pure-Rust
`ruzstd`, as for `.kpmap` maps.

## Deploy

The bundle is a committed build product, unlike the `.kpmap` maps which
`.github/workflows/deploy.yml` bakes on every deploy: the workflow does
not check out the upstream submodules, so it ships whatever bundle is in
the repository.
