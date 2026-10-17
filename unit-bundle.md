# The unit data (`kernel-panic/assets/defs/`, `kernel-panic/assets/units.kpu`)

Everything the game needs from the original Kernel Panic's unit data is
committed in this repository and embedded in the binary; the runtime
never reads `upstream/`, and native and web builds get the same bytes.

## The definitions: text

`kernel-panic/assets/defs/` holds the original files verbatim, with
lower-cased names:

```text
units/*.fbi        the 48 unit definitions
weapons/*.tdf      the weapons, one file per faction
explosions/*.tdf   the CEG explosion generators
moveinfo.tdf       MOVEINFO.TDF, the move classes
```

`build.rs` lists them into `UNIT_FBIS`, `WEAPON_TDFS`, `EXPLOSION_TDFS`
and `MOVEINFO_TDF` with `include_str!`, and `units::content::defs`
parses them with `spring-tdf` on first use (a few milliseconds). They
are text, not part of the bundle, because the sim keeps porting engine
behaviours that read another FBI or TDF key: reading a new key is a
field on the typed def (`spring_tdf::{UnitDef, WeaponDef, …}`) and a
line in its `from_section`, with nothing to re-bake. The files are
parsed in name order, so where two define the same name the later file
wins.

The test `defs::tests::every_def_file_parses` fails the build on a
file that does not parse, and the audio and bundle tests check the
cross-references (every weapon's sounds, every unit's model).

## The models and textures: the bundle

`kernel-panic/assets/units.kpu` carries the parsed `objects3d/*.s3o`
models and the RGBA pixels of every `unittextures/*.tga` and
`bitmaps/kpsfx/*.tga`, plus the three engine bitmaps the effects
reference (`laserend.tga`, `laserfalloff.tga`, `flare.tga` from the
engine's `base/bitmaps`). It is `include_bytes!`d by
`units::content::bundle`; the types are `units::content::model`.

```text
magic    : 8 bytes  "kpunit5\0"
body_len : u32      little-endian length of the zstd frame
body     : ZSTD(postcard(UnitBundle { models, textures }))
```

Both maps are keyed by the lower-cased file name; lookups are
case-insensitive like the original game's VFS. Textures are the bulk
of the data (37 MB of RGBA pixels, mostly flat colour), so each is its
own zstd frame inside the payload and is only decoded when a model or
effect first asks for it; the outer frame decodes in ~10-35 ms at
startup. The whole file is ~1.7 MB. The reader is the pure-Rust
`ruzstd`, as for `.kpmap` maps.

Models and textures never gain fields, so there is nothing to re-bake.
The bake that produced the file (the `KP_BAKE_UNITS` mode of the game
binary, the `spring-unit-mesh` S3O and TGA parsers, the C zstd encoder
at level 19, and the `bundle_matches_upstream_checkout` test that
compared the committed file against the submodules) was removed in the
commit after `a8567de`; that commit is the last with the tools:

```sh
git show a8567de:kernel-panic/src/units/content/bundle.rs   # the `bake` module
git checkout a8567de -- spring-unit-mesh
```

Should `S3OModel` ever need another field, start from there and bump
the magic, so a stale bundle fails loudly instead of mis-decoding.

## Compatibility

A peer's build hash (`net::compat::Build`) covers the bundle bytes and
every definition file, so two builds with different unit data refuse
to play each other.
