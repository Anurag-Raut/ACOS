//! The `android.content.res` engine: `AssetManager`, `ApkAssets`,
//! `StringBlock`, `XmlBlock`, themes and style resolution.
//!
//! The module is VM-independent: every entry point is a plain Rust function or
//! method over integer handles and slices. The later native shim maps the JNI
//! signatures onto these APIs; `NATIVES.md` records the mapping.

pub mod arsc;
pub mod assets;
pub(crate) mod bytes;
pub mod config;
pub mod error;
pub mod manager;
pub mod strpool;
pub mod theme;
pub mod value;
pub mod xmlblock;

pub use arsc::{Bag, PackageIndex, ResTableIndex, ResolvedEntry, TypeChunk};
pub use assets::{ApkAssets, AssetHandle, AssetSource, Backing, ZipEntryInfo};
pub use config::ResConfig;
pub use error::{ResError, Result};
pub use manager::{AssetManager, ConfigurationNative, Lookup};
pub use strpool::{StringBlock, StringPool, StyleSpan};
pub use theme::Theme;
pub use value::{
    ResValue, TypedValueOut, STYLE_ASSET_COOKIE, STYLE_CHANGING_CONFIGURATIONS, STYLE_DATA,
    STYLE_DENSITY, STYLE_NUM_ENTRIES, STYLE_RESOURCE_ID, STYLE_SOURCE_RESOURCE_ID, STYLE_TYPE,
};
pub use xmlblock::{
    XmlAttribute, XmlBlock, XmlDocument, XmlEvent, XmlParserState, XmlTag, END_DOCUMENT, END_TAG,
    START_DOCUMENT, START_TAG, TEXT,
};
