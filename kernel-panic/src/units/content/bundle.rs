//! The unit bundle: Kernel Panic's `.s3o` models and `.tga` textures,
//! baked once into `kernel-panic/assets/units.kpu` and embedded in the
//! binary, so native and web builds alike get their models from
//! [`bundle`] without any filesystem. The unit, weapon, explosion and
//! move-class definitions are text (`defs`), not part of the bundle.
//!
//! The bake (`KP_BAKE_UNITS`, the S3O and TGA parsers of
//! `spring-unit-mesh`, the C zstd encoder) read the `upstream/`
//! submodules; it is in the git history (README.md). Models and
//! textures never gain fields, so nothing needs re-baking; should a
//! field on [`S3OModel`] ever change, the bake is the place to start
//! from, and the magic below must be bumped with it.
//!
//! Format (little-endian), after the `.kpmap` convention:
//!
//! ```text
//! magic    : 8 bytes  "kpunit5\0" — bumped on any change to the payload
//! body_len : u32      length of the zstd frame
//! body     : ZSTD(postcard(UnitBundle))
//! ```
//!
//! Textures are the bulk of the data (37 MB of RGBA pixels, mostly flat
//! colour), so each one is its own zstd frame inside the payload and is
//! only decoded when a model or effect first asks for it
//! ([`UnitBundle::texture`]); the outer frame decodes in a few
//! milliseconds at startup. The reader is the pure-Rust `ruzstd`, which
//! builds for wasm.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::LazyLock;

use super::model::{S3OModel, TgaImage};
use bevy::prelude::*;
use serde::de::{Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// File magic; the version digit is bumped whenever the payload layout
/// changes so a stale bundle fails loudly instead of mis-decoding.
const MAGIC: &[u8; 8] = b"kpunit5\0";

/// The committed bake. `include_bytes!` keeps the runtime free of any
/// filesystem or asset-server plumbing: the bundle is part of the
/// binary on native and of the `.wasm` on web.
static BUNDLE_BYTES: &[u8] = include_bytes!("../../../assets/units.kpu");

/// The embedded bundle as committed (a replay and the lobby stamp
/// its hash, `net::compat`).
pub fn bundle_bytes() -> &'static [u8] {
    BUNDLE_BYTES
}

/// One baked texture: decoded RGBA8 pixels in their own zstd frame.
#[derive(Serialize, Deserialize, Default)]
pub struct BakedTexture {
    pub width: u32,
    pub height: u32,
    frame: Blob,
}

/// The payload. Both maps are keyed by the lower-cased file name
/// (`ball.s3o`, `kernel.tga`): the FBIs, s3o headers and weapon TDFs
/// mix cases freely and the original game ran on a case-insensitive VFS.
#[derive(Serialize, Deserialize, Default)]
pub struct UnitBundle {
    /// Every `objects3d/*.s3o`, parsed.
    pub models: BTreeMap<String, S3OModel>,
    /// Every `unittextures/*.tga` and `bitmaps/kpsfx/*.tga`, plus the
    /// three engine bitmaps the effects reference (`laserend.tga`,
    /// `laserfalloff.tga`, `flare.tga`). Where a name existed in several
    /// directories the first in the old on-disk search order won
    /// (`unittextures` before `kpsfx`).
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

    /// Whether a texture of that name (any case) was baked.
    #[cfg(test)]
    pub fn has_texture(&self, filename: &str) -> bool {
        self.textures.contains_key(&filename.to_ascii_lowercase())
    }
}

/// The embedded bundle, decoded on first use. The model cache and the
/// unit registry's model radii read from here; an unreadable bundle
/// logs once and yields the empty bundle (every unit then draws as the
/// fallback cylinder).
pub fn bundle() -> &'static UnitBundle {
    static BUNDLE: LazyLock<UnitBundle> = LazyLock::new(|| {
        let start = bevy::platform::time::Instant::now();
        match decode(BUNDLE_BYTES) {
            Ok(bundle) => {
                info!(
                    "Unit bundle: {} models, {} textures ({} KB) decoded in {:.1} ms",
                    bundle.models.len(),
                    bundle.textures.len(),
                    BUNDLE_BYTES.len() / 1024,
                    start.elapsed().as_secs_f64() * 1e3,
                );
                bundle
            }
            Err(error) => {
                error!("Unit bundle unreadable ({error}) — no models");
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

/// Inflate one zstd frame. The bake pledged the content size, so the
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::content::definitions::ALL_UNIT_KINDS;
    use crate::units::content::defs::defs;
    use crate::units::content::weapons::WeaponKind;

    /// The committed bundle decodes and has, for every unit kind, its
    /// model and that model's `texture1`; every weapon model and every
    /// texture a model names is bundled too.
    #[test]
    fn bundle_covers_every_unit_kind() {
        let bundle = bundle();
        let defs = defs();
        assert!(!bundle.models.is_empty(), "empty bundle");
        for kind in ALL_UNIT_KINDS {
            let def = defs
                .units
                .units
                .get(kind.unitname())
                .unwrap_or_else(|| panic!("{kind:?}: no FBI"));
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
        }
        for (name, weapon) in &defs.weapons.weapons {
            let model = weapon.model.trim().trim_end_matches(';');
            if !model.is_empty() {
                assert!(
                    bundle.model(model).is_some(),
                    "weapon {name}: model {model} not bundled"
                );
            }
        }
        for (name, model) in &bundle.models {
            for tex in [&model.texture1, &model.texture2] {
                assert!(
                    tex.is_empty() || bundle.has_texture(tex),
                    "{name}: texture {tex} not bundled"
                );
            }
        }
        // The models and effect textures the code names directly
        // (`command_fire.rs`: the Signal's flag and the SigTerm bomb).
        for name in ["signal.s3o", "sigterm.s3o", "octashot.s3o"] {
            assert!(bundle.model(name).is_some(), "{name} not bundled");
        }
        for name in [
            "laserend.tga",
            "laserfalloff.tga",
            "flare.tga",
            "hexgrid.tga",
            "whitecircle.tga",
            "arrow.tga",
            "dosray.tga",
        ] {
            assert!(bundle.has_texture(name), "{name} not bundled");
        }
        // Lookups are case-insensitive, like the original game's VFS.
        assert!(bundle.model("Kernel.S3O").is_some());
    }

    /// Every unit's weapons and `ExplodeAs` are defined.
    #[test]
    fn every_unit_weapon_is_defined() {
        let defs = defs();
        for kind in ALL_UNIT_KINDS {
            let def = &defs.units.units[kind.unitname()];
            for weapon in [&def.weapon1, &def.weapon2, &def.explode_as] {
                // A build laser is a constructor's tool rather than a
                // weapon, so it is exempt here exactly as it is in
                // `UnitRegistry::weapon`.
                if weapon.is_empty()
                    || WeaponKind::find(weapon).is_some_and(|kind| kind.is_build_laser())
                {
                    continue;
                }
                assert!(
                    defs.weapons
                        .weapons
                        .keys()
                        .any(|w| w.eq_ignore_ascii_case(weapon)),
                    "{kind:?}: weapon {weapon} not defined"
                );
            }
        }
    }

    #[test]
    fn rejects_foreign_and_truncated_blobs() {
        assert!(matches!(
            decode(b"kpmapv4\0\0\0\0\0"),
            Err(BundleError::BadMagic)
        ));
        assert!(matches!(
            decode(b"kpunit5\0\xff\0\0\0abc"),
            Err(BundleError::Truncated { .. })
        ));
        assert!(matches!(decode(b""), Err(BundleError::Truncated { .. })));
    }
}
