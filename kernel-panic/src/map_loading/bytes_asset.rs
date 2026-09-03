//! A minimal raw-bytes asset type.
//!
//! Web builds fetch baked `.kpmap` maps through the Bevy asset server
//! (HTTP), which needs an [`Asset`] type for the payload — std::fs
//! doesn't exist there. The loader accepts any file; the `.kpmap`
//! extension mapping is declared for completeness.

use bevy::asset::{Asset, AssetLoader, LoadContext};
use bevy::asset::io::Reader;
use bevy::reflect::TypePath;
use std::sync::Arc;

/// Opaque file bytes, cheap to clone.
#[derive(Asset, TypePath)]
pub struct BytesAsset(pub Arc<[u8]>);

/// Reads the whole stream into a [`BytesAsset`].
#[derive(Default, TypePath)]
pub struct BytesLoader;

impl AssetLoader for BytesLoader {
    type Asset = BytesAsset;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &Self::Settings,
        _load_context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        Ok(BytesAsset(bytes.into()))
    }

    fn extensions(&self) -> &[&str] {
        &["kpmap"]
    }
}

/// Asset path of a cataloged map, relative to the asset root
/// (`kernel-panic/assets`, shipped as `assets/` by the web build).
pub fn map_asset_path(stem: &str) -> String {
    format!("maps/{stem}.kpmap")
}
