//! The unit bundle: everything the game used to read from the
//! `upstream/` checkout at runtime, baked once into
//! `kernel-panic/assets/units.kpu` and embedded in the binary.
//!
//! Kernel Panic's original data — unit FBIs, weapon and explosion TDFs,
//! `MOVEINFO.TDF`, the `.s3o` models and their `.tga` textures — lives
//! in the `upstream/Kernel-Panic` git submodule (plus one engine bitmap
//! from `upstream/RecoilEngine`). Reading it at runtime tied the game to
//! a developer checkout and does not work at all on wasm, where there is
//! no filesystem. Instead the developer bakes the parsed data into one
//! file (`KP_BAKE_UNITS=kernel-panic/assets/units.kpu cargo run -p
//! kernel-panic`, see `unit-bundle.md`) which is committed and
//! `include_bytes!`d here, so native and web builds alike get their
//! registries and models from [`bundle`] without touching `upstream/`.
//!
//! Format (little-endian), after the `.kpmap` convention:
//!
//! ```text
//! magic    : 8 bytes  "kpunit1\0" — bump on any change to the payload
//! body_len : u32      length of the zstd frame
//! body     : ZSTD(postcard(UnitBundle))
//! ```
//!
//! Textures are the bulk of the data (28 MB of RGBA pixels, mostly flat
//! colour), so each one is its own zstd frame inside the payload and is
//! only decoded when a model or effect first asks for it
//! ([`UnitBundle::texture`]); the outer frame decodes in a few
//! milliseconds at startup. The bake side ([`bake`]) is native only —
//! it needs the upstream checkout and the C zstd encoder — while the
//! reader uses the pure-Rust `ruzstd`, which builds for wasm.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::LazyLock;

use bevy::prelude::*;
use serde::de::{Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use spring_tdf::{ExplosionDefs, UnitDefs, WeaponDefs};
use spring_unit_mesh::{S3OModel, TgaImage};
use thiserror::Error;

use super::moveinfo::MoveClassTable;

/// File magic; the version digit is bumped whenever the payload layout
/// changes so a stale bundle fails loudly instead of mis-decoding.
const MAGIC: &[u8; 8] = b"kpunit1\0";

/// The committed bake. `include_bytes!` keeps the runtime free of any
/// filesystem or asset-server plumbing: the bundle is part of the
/// binary on native and of the `.wasm` on web. An empty file is a valid
/// placeholder for a first bake (decoding it yields the empty bundle).
static BUNDLE_BYTES: &[u8] = include_bytes!("../../../assets/units.kpu");

/// One baked texture: decoded RGBA8 pixels in their own zstd frame.
#[derive(Serialize, Deserialize, Default)]
pub struct BakedTexture {
    pub width: u32,
    pub height: u32,
    frame: Blob,
}

/// The payload. Every map is keyed by the lower-cased file name
/// (`ball.s3o`, `kernel.tga`): the FBIs, s3o headers and weapon TDFs
/// mix cases freely and the original game ran on a case-insensitive VFS.
#[derive(Serialize, Deserialize, Default)]
pub struct UnitBundle {
    /// Every `units/*.fbi`, merged (keyed by lower-cased `unitname`).
    pub units: UnitDefs,
    /// `gamedata/MOVEINFO.TDF`.
    pub move_classes: MoveClassTable,
    /// Every `weapons/*.tdf`, merged.
    pub weapons: WeaponDefs,
    /// Every `gamedata/explosions/*.tdf`, merged.
    pub explosions: ExplosionDefs,
    /// Every `objects3d/*.s3o`, parsed.
    pub models: BTreeMap<String, S3OModel>,
    /// Every `unittextures/*.tga` and `bitmaps/kpsfx/*.tga`, plus the
    /// engine bitmaps the effects reference (`laserend.tga`). Where a
    /// name exists in several directories the first in the old on-disk
    /// search order wins (`unittextures` before `kpsfx`).
    pub textures: BTreeMap<String, BakedTexture>,
}

impl UnitBundle {
    /// The parsed model behind a file name (`kernel.s3o`), any case.
    pub fn model(&self, filename: &str) -> Option<&S3OModel> {
        self.models.get(&filename.to_ascii_lowercase())
    }

    /// Decode the texture behind a file name (`kernel.tga`), any case.
    /// A fresh copy per call — the caller (`S3OModelCache`) keeps it.
    pub fn texture(&self, filename: &str) -> Option<TgaImage> {
        let baked = self.textures.get(&filename.to_ascii_lowercase())?;
        match decode_zstd(&baked.frame.0) {
            Ok(pixels) if pixels.len() == (baked.width * baked.height * 4) as usize => {
                Some(TgaImage {
                    width: baked.width,
                    height: baked.height,
                    pixels,
                })
            }
            Ok(pixels) => {
                warn!(
                    "Bundled texture {filename}: {} pixel bytes for {}x{}",
                    pixels.len(),
                    baked.width,
                    baked.height
                );
                None
            }
            Err(error) => {
                warn!("Bundled texture {filename} unreadable: {error}");
                None
            }
        }
    }

    /// Whether a texture of that name (any case) was baked (the bake's
    /// reference check and the tests).
    #[cfg(any(test, not(target_arch = "wasm32")))]
    pub fn has_texture(&self, filename: &str) -> bool {
        self.textures.contains_key(&filename.to_ascii_lowercase())
    }
}

/// The embedded bundle, decoded on first use. Every registry `load()`
/// and the model cache read from here; an unreadable bundle logs once
/// and yields the empty bundle (the game boots with no units, as it did
/// without the upstream checkout).
pub fn bundle() -> &'static UnitBundle {
    static BUNDLE: LazyLock<UnitBundle> = LazyLock::new(|| {
        let start = bevy::platform::time::Instant::now();
        match decode(BUNDLE_BYTES) {
            Ok(bundle) => {
                info!(
                    "Unit bundle: {} units, {} weapons, {} CEGs, {} models, {} textures ({} KB) decoded in {:.1} ms",
                    bundle.units.units.len(),
                    bundle.weapons.weapons.len(),
                    bundle.explosions.explosions.len(),
                    bundle.models.len(),
                    bundle.textures.len(),
                    BUNDLE_BYTES.len() / 1024,
                    start.elapsed().as_secs_f64() * 1e3,
                );
                bundle
            }
            Err(error) => {
                error!("Unit bundle unreadable ({error}) — no units; re-bake with KP_BAKE_UNITS");
                UnitBundle::default()
            }
        }
    });
    &BUNDLE
}

#[derive(Debug, Error)]
pub enum BundleError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("postcard error: {0}")]
    Postcard(#[from] postcard::Error),
    #[error("not a unit bundle (bad magic; expected {})", String::from_utf8_lossy(&MAGIC[..7]))]
    BadMagic,
    #[error("bundle truncated: declared {declared} bytes, file has {actual}")]
    Truncated { declared: usize, actual: usize },
}

/// Decode a `.kpu` blob (see the module docs for the layout).
pub fn decode(bytes: &[u8]) -> Result<UnitBundle, BundleError> {
    let Some(header) = bytes.get(..MAGIC.len() + 4) else {
        return Err(BundleError::Truncated {
            declared: MAGIC.len() + 4,
            actual: bytes.len(),
        });
    };
    if &header[..MAGIC.len()] != MAGIC {
        return Err(BundleError::BadMagic);
    }
    let body_len = u32::from_le_bytes(header[MAGIC.len()..].try_into().unwrap()) as usize;
    let body = &bytes[MAGIC.len() + 4..];
    if body.len() < body_len {
        return Err(BundleError::Truncated {
            declared: body_len,
            actual: body.len(),
        });
    }
    let payload = decode_zstd(&body[..body_len])?;
    Ok(postcard::from_bytes(&payload)?)
}

/// Inflate one zstd frame. The bake pledges the content size, so the
/// output buffer is allocated once.
fn decode_zstd(frame: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut decoder = ruzstd::decoding::StreamingDecoder::new(frame)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut out = Vec::with_capacity(decoder.decoder.content_size() as usize);
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

/// An owned byte run serialized as postcard bytes (varint length + raw
/// bytes, the same wire shape as `Vec<u8>`) but decoded as one slice
/// copy instead of serde's per-element `visit_u8` sequence path.
#[derive(Default)]
struct Blob(Vec<u8>);

impl Serialize for Blob {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for Blob {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct BlobVisitor;
        impl<'de> Visitor<'de> for BlobVisitor {
            type Value = Blob;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a byte run")
            }
            fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Blob, E> {
                Ok(Blob(v.to_vec()))
            }
            fn visit_byte_buf<E: serde::de::Error>(self, v: Vec<u8>) -> Result<Blob, E> {
                Ok(Blob(v))
            }
        }
        deserializer.deserialize_byte_buf(BlobVisitor)
    }
}

/// The bake: read the upstream checkout the way the registries used to
/// at startup, and write the bundle. Native only (filesystem + the C
/// zstd encoder); the comparison test below also uses [`bake::collect`]
/// as the reference the embedded bundle must match.
#[cfg(not(target_arch = "wasm32"))]
pub mod bake {
    use std::io::Write;
    use std::path::{Path, PathBuf};

    use spring_tdf::{ExplosionDefs, UnitDefs, WeaponDefs};
    use thiserror::Error;

    use super::super::definitions::ALL_UNIT_KINDS;
    use super::super::moveinfo::MoveClassTable;
    use super::super::tdf_loader;
    use super::{BakedTexture, Blob, MAGIC, UnitBundle};

    /// Directories the runtime used to search for a model or texture, in
    /// search order (the first hit won; the bake keeps that precedence).
    const MODEL_DIR: &str = "upstream/Kernel-Panic/objects3d";
    const TEXTURE_DIRS: &[&str] = &[
        "upstream/Kernel-Panic/unittextures",
        // Beam / CEG textures (arrow, dosray, bytemegabeam, whitecircle…).
        "upstream/Kernel-Panic/bitmaps/kpsfx",
    ];
    /// Engine default bitmaps the effects rely on (`laserend` for
    /// `explspike` streaks) — the only files taken from the 24 MB
    /// RecoilEngine bitmap directory.
    const ENGINE_BITMAP_DIR: &str = "upstream/RecoilEngine/cont/base/bitmaps/bitmaps";
    const ENGINE_BITMAPS: &[&str] = &["laserend.tga"];
    /// Models the code names directly rather than through a def
    /// (`command_fire.rs`: the Signal's flag and the SigTerm bomb).
    const CODE_MODELS: &[&str] = &["signal.s3o", "sigterm.s3o"];

    #[derive(Debug, Error)]
    pub enum BakeError {
        #[error("upstream directory not found: {0} (is the upstream/ submodule checked out?)")]
        MissingDir(String),
        #[error("{path}: {error}")]
        Io {
            path: PathBuf,
            #[source]
            error: std::io::Error,
        },
        #[error("{path}: {error}")]
        Parse { path: PathBuf, error: String },
        #[error("encode error: {0}")]
        Encode(#[from] super::BundleError),
    }

    fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> BakeError + '_ {
        move |error| BakeError::Io {
            path: path.to_path_buf(),
            error,
        }
    }

    fn upstream_dir(leaf: &str) -> Result<PathBuf, BakeError> {
        tdf_loader::find_upstream_dir(leaf).ok_or_else(|| BakeError::MissingDir(leaf.to_string()))
    }

    /// Files of `dir` with `extension` (any case), sorted by name so the
    /// bake is reproducible whatever order the directory lists them in.
    fn files_with_ext(dir: &Path, extension: &str) -> Result<Vec<PathBuf>, BakeError> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .map_err(io_err(dir))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case(extension))
            })
            .collect();
        files.sort();
        Ok(files)
    }

    fn file_key(path: &Path) -> String {
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase()
    }

    /// Read the upstream checkout into a bundle. Every parse failure is
    /// an error: the bake must be exact, not best-effort.
    pub fn collect() -> Result<UnitBundle, BakeError> {
        let mut units = UnitDefs::default();
        for (_, tdf) in tdf_loader::load_all_tdf_files(&upstream_dir("units")?, "fbi") {
            units.units.extend(UnitDefs::from_tdf(&tdf).units);
        }

        let moveinfo = upstream_dir("gamedata")?.join("MOVEINFO.TDF");
        let move_classes = MoveClassTable::from_tdf(
            &tdf_loader::load_tdf_file(&moveinfo).map_err(|error| BakeError::Parse {
                path: moveinfo.clone(),
                error: error.to_string(),
            })?,
        );

        let mut weapons = WeaponDefs::default();
        for (_, tdf) in tdf_loader::load_all_tdf_files(&upstream_dir("weapons")?, "tdf") {
            weapons.weapons.extend(WeaponDefs::from_tdf(&tdf).weapons);
        }

        let mut explosions = ExplosionDefs::default();
        for (_, tdf) in
            tdf_loader::load_all_tdf_files(&upstream_dir("gamedata/explosions")?, "tdf")
        {
            explosions.merge(ExplosionDefs::from_tdf(&tdf));
        }

        let mut bundle = UnitBundle {
            units,
            move_classes,
            weapons,
            explosions,
            ..UnitBundle::default()
        };

        let model_dir = crate::paths::from_project_root(MODEL_DIR);
        for path in files_with_ext(&model_dir, "s3o")? {
            let data = std::fs::read(&path).map_err(io_err(&path))?;
            let model = spring_unit_mesh::parse_s3o(&data).map_err(|error| BakeError::Parse {
                path: path.clone(),
                error: error.to_string(),
            })?;
            bundle.models.insert(file_key(&path), model);
        }

        let mut texture_files: Vec<PathBuf> = Vec::new();
        for dir in TEXTURE_DIRS {
            texture_files.extend(files_with_ext(&crate::paths::from_project_root(dir), "tga")?);
        }
        for name in ENGINE_BITMAPS {
            texture_files.push(crate::paths::from_project_root(&format!("{ENGINE_BITMAP_DIR}/{name}")));
        }
        for path in texture_files {
            let key = file_key(&path);
            if bundle.textures.contains_key(&key) {
                continue; // earlier search directory wins, as on disk
            }
            let data = std::fs::read(&path).map_err(io_err(&path))?;
            let tga = spring_unit_mesh::parse_tga(&data).map_err(|error| BakeError::Parse {
                path: path.clone(),
                error: error.to_string(),
            })?;
            let frame = encode_zstd(&tga.pixels).map_err(io_err(&path))?;
            bundle.textures.insert(
                key,
                BakedTexture {
                    width: tga.width,
                    height: tga.height,
                    frame: Blob(frame),
                },
            );
        }
        Ok(bundle)
    }

    /// Names the game will ask for that the bundle does not have — a
    /// missing unit kind's FBI, model or texture would show up in-game
    /// as the cylinder / flat-colour fallbacks.
    pub fn missing_references(bundle: &UnitBundle) -> Vec<String> {
        let mut missing = Vec::new();
        let mut models: Vec<(String, String)> = Vec::new();
        for kind in ALL_UNIT_KINDS {
            match bundle.units.units.get(kind.unitname()) {
                Some(def) => models.push((def.object_name.clone(), format!("{kind:?}"))),
                None => missing.push(format!("unit {} ({kind:?})", kind.unitname())),
            }
        }
        for (name, weapon) in &bundle.weapons.weapons {
            let model = weapon.model.trim().trim_end_matches(';');
            models.push((model.to_string(), format!("weapon {name}")));
        }
        models.extend(CODE_MODELS.iter().map(|m| (m.to_string(), "code".to_string())));
        for (name, from) in models {
            if !name.is_empty() && bundle.model(&name).is_none() {
                missing.push(format!("model {name} ({from})"));
            }
        }
        for (name, model) in &bundle.models {
            for tex in [&model.texture1, &model.texture2] {
                if !tex.is_empty() && !bundle.has_texture(tex) {
                    missing.push(format!("texture {tex} ({name})"));
                }
            }
        }
        missing
    }

    /// Serialize to the `.kpu` wire format.
    pub fn encode(bundle: &UnitBundle) -> Result<Vec<u8>, BakeError> {
        let payload = postcard::to_allocvec(bundle).map_err(super::BundleError::from)?;
        let body = encode_zstd(&payload).map_err(super::BundleError::from)?;
        let mut out = Vec::with_capacity(MAGIC.len() + 4 + body.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// `KP_BAKE_UNITS=<out>`: bake and write, reporting to stderr.
    pub fn run(out: &Path) -> Result<(), BakeError> {
        let bundle = collect()?;
        for what in missing_references(&bundle) {
            eprintln!("bake-units: warning: {what} not found in upstream/");
        }
        let bytes = encode(&bundle)?;
        std::fs::write(out, &bytes).map_err(io_err(out))?;
        let pixels: usize = bundle
            .textures
            .values()
            .map(|t| (t.width * t.height * 4) as usize)
            .sum();
        eprintln!(
            "bake-units: {} units, {} move classes, {} weapons, {} CEGs, {} models, {} textures ({:.1} MB of pixels)",
            bundle.units.units.len(),
            bundle.move_classes.len(),
            bundle.weapons.weapons.len(),
            bundle.explosions.explosions.len(),
            bundle.models.len(),
            bundle.textures.len(),
            pixels as f64 / 1e6,
        );
        eprintln!("Wrote {} ({} bytes)", out.display(), bytes.len());
        Ok(())
    }

    /// Encode a zstd frame at the slowest / smallest level (the bake is
    /// offline), pledging the content size so the reader allocates once.
    fn encode_zstd(body: &[u8]) -> std::io::Result<Vec<u8>> {
        let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 19)?;
        encoder.set_pledged_src_size(Some(body.len() as u64))?;
        encoder.include_contentsize(true)?;
        encoder.write_all(body)?;
        encoder.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::content::definitions::ALL_UNIT_KINDS;

    /// The committed bundle decodes and has, for every unit kind, its
    /// FBI, its model, that model's `texture1` and its weapon.
    #[test]
    fn bundle_covers_every_unit_kind() {
        let bundle = bundle();
        assert!(!bundle.units.units.is_empty(), "empty bundle — re-bake with KP_BAKE_UNITS");
        for kind in ALL_UNIT_KINDS {
            let def = bundle
                .units
                .units
                .get(kind.unitname())
                .unwrap_or_else(|| panic!("{kind:?}: no FBI in the bundle"));
            let model = bundle
                .model(&def.object_name)
                .unwrap_or_else(|| panic!("{kind:?}: model {} not bundled", def.object_name));
            assert!(
                bundle.has_texture(&model.texture1),
                "{kind:?}: texture1 {} of {} not bundled",
                model.texture1,
                def.object_name
            );
            let tex = bundle.texture(&model.texture1).expect("texture decodes");
            assert_eq!(tex.pixels.len(), (tex.width * tex.height * 4) as usize);
            for weapon in [&def.weapon1, &def.weapon2] {
                if weapon.is_empty()
                    || weapon.eq_ignore_ascii_case("BuildLaser")
                    || weapon.eq_ignore_ascii_case("BuildLaserNoEffect")
                {
                    continue;
                }
                assert!(
                    bundle
                        .weapons
                        .weapons
                        .keys()
                        .any(|w| w.eq_ignore_ascii_case(weapon)),
                    "{kind:?}: weapon {weapon} not bundled"
                );
            }
            if !def.explode_as.is_empty() {
                assert!(
                    bundle
                        .weapons
                        .weapons
                        .keys()
                        .any(|w| w.eq_ignore_ascii_case(&def.explode_as)),
                    "{kind:?}: ExplodeAs {} not bundled",
                    def.explode_as
                );
            }
        }
        // The models and effect textures the code names directly.
        for name in ["signal.s3o", "sigterm.s3o", "octashot.s3o"] {
            assert!(bundle.model(name).is_some(), "{name} not bundled");
        }
        for name in ["laserend.tga", "hexgrid.tga", "whitecircle.tga", "arrow.tga", "dosray.tga"] {
            assert!(bundle.has_texture(name), "{name} not bundled");
        }
        // Lookups are case-insensitive, like the original game's VFS.
        assert!(bundle.model("Kernel.S3O").is_some());
    }

    /// Bit-for-bit: the embedded bundle is what the upstream checkout
    /// bakes to today, so every registry built from it matches the
    /// TDF-built one. Skipped where the upstream submodule is absent.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn bundle_matches_upstream_checkout() {
        let fresh = match bake::collect() {
            Ok(fresh) => fresh,
            Err(bake::BakeError::MissingDir(dir)) => {
                eprintln!("skip: upstream/{dir} not checked out");
                return;
            }
            Err(error) => panic!("bake failed: {error}"),
        };
        let baked = bundle();
        assert_eq!(
            format!("{:?}", fresh.units),
            format!("{:?}", baked.units),
            "unit defs differ — re-bake with KP_BAKE_UNITS"
        );
        assert_eq!(format!("{:?}", fresh.move_classes), format!("{:?}", baked.move_classes));
        assert_eq!(format!("{:?}", fresh.weapons), format!("{:?}", baked.weapons));
        assert_eq!(format!("{:?}", fresh.explosions), format!("{:?}", baked.explosions));
        assert_eq!(
            fresh.models.keys().collect::<Vec<_>>(),
            baked.models.keys().collect::<Vec<_>>()
        );
        for (name, model) in &fresh.models {
            assert_eq!(format!("{model:?}"), format!("{:?}", baked.models[name]), "{name}");
        }
        assert_eq!(
            fresh.textures.keys().collect::<Vec<_>>(),
            baked.textures.keys().collect::<Vec<_>>()
        );
        for name in fresh.textures.keys() {
            let (a, b) = (fresh.texture(name).unwrap(), baked.texture(name).unwrap());
            assert_eq!((a.width, a.height), (b.width, b.height), "{name}");
            assert!(a.pixels == b.pixels, "{name}: pixels differ");
        }
        assert!(
            bake::missing_references(baked).is_empty(),
            "dangling references: {:?}",
            bake::missing_references(baked)
        );
    }

    /// The registries built from the bundle agree with the TDF-built
    /// ones on what the game reads from them.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn registries_from_bundle_match_tdf_built() {
        use crate::units::content::unit_registry::UnitRegistry;
        use crate::units::content::weapons::WeaponRegistry;
        use crate::units::weapon_fx::CegRegistry;

        let fresh = match bake::collect() {
            Ok(fresh) => fresh,
            Err(bake::BakeError::MissingDir(_)) => return,
            Err(error) => panic!("bake failed: {error}"),
        };
        let (units, tdf_units) = (
            UnitRegistry::load(),
            UnitRegistry::from_defs(fresh.units.clone(), fresh.move_classes.clone()),
        );
        for &kind in ALL_UNIT_KINDS {
            assert_eq!(
                format!("{:?}", units.def(kind)),
                format!("{:?}", tdf_units.def(kind)),
                "{kind:?}"
            );
            assert_eq!(units.move_def(kind), tdf_units.move_def(kind), "{kind:?}");
            assert_eq!(units.heat_produced(kind), tdf_units.heat_produced(kind), "{kind:?}");
        }
        let (weapons, tdf_weapons) = (WeaponRegistry::load(), WeaponRegistry::from_defs(&fresh.weapons));
        for name in fresh.weapons.weapons.keys() {
            assert_eq!(
                format!("{:?}", weapons.get(name)),
                format!("{:?}", tdf_weapons.get(name)),
                "{name}"
            );
        }
        let (cegs, tdf_cegs) = (CegRegistry::load(), CegRegistry::from_defs(fresh.explosions.clone()));
        for name in fresh.explosions.explosions.keys() {
            assert_eq!(format!("{:?}", cegs.get(name)), format!("{:?}", tdf_cegs.get(name)), "{name}");
        }
    }

    #[test]
    fn rejects_foreign_and_truncated_blobs() {
        assert!(matches!(decode(b"kpmapv4\0\0\0\0\0"), Err(BundleError::BadMagic)));
        assert!(matches!(decode(b"kpunit1\0\xff\0\0\0abc"), Err(BundleError::Truncated { .. })));
        assert!(matches!(decode(b""), Err(BundleError::Truncated { .. })));
    }
}
