//! APK container ingest: zip entry listing, DEX files, native libs, assets and
//! signature-scheme detection.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::Path;

use crate::error::{ApkError, Result, APK_ENTRY, APK_NOT_ZIP, APK_NO_MANIFEST};
use crate::manifest::Manifest;
use crate::signing::{self, SigningInfo};

/// A single zip entry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ApkEntry {
    pub name: String,
    pub size: u64,
    pub compressed_size: u64,
    pub method: String,
    pub is_dir: bool,
}

/// A DEX file stored in the APK.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DexFile {
    pub name: String,
    pub size: usize,
    #[serde(skip)]
    pub bytes: Vec<u8>,
}

/// A native library stored in the APK.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NativeLib {
    pub abi: String,
    pub name: String,
    pub size: u64,
}

/// An opened, indexed APK. Raw manifest / resources bytes are retained so the
/// typed models can be parsed on demand without re-reading the file.
#[derive(Debug)]
pub struct Apk {
    pub source: String,
    pub entries: Vec<ApkEntry>,
    pub dex_files: Vec<DexFile>,
    pub native_libs: BTreeMap<String, Vec<NativeLib>>,
    pub assets: Vec<ApkEntry>,
    pub signing: SigningInfo,
    pub manifest_raw: Option<Vec<u8>>,
    pub arsc_raw: Option<Vec<u8>>,
    raw: Vec<u8>,
}

impl Apk {
    /// Open and index an APK from disk.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let bytes = std::fs::read(path)
            .map_err(|e| ApkError::new(crate::error::APK_IO, format!("{}: {e}", path.display())))?;
        let mut apk = Self::from_bytes(bytes)?;
        apk.source = path.display().to_string();
        Ok(apk)
    }

    /// Open and index an APK from an in-memory byte buffer.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes.clone()))
            .map_err(|e| ApkError::new(APK_NOT_ZIP, e.to_string()))?;

        let mut entries = Vec::new();
        let mut dex_files = Vec::new();
        let mut native_libs: BTreeMap<String, Vec<NativeLib>> = BTreeMap::new();
        let mut assets = Vec::new();
        let mut manifest_raw = None;
        let mut arsc_raw = None;

        for i in 0..archive.len() {
            let mut file = archive
                .by_index(i)
                .map_err(|e| ApkError::new(APK_ENTRY, e.to_string()))?;
            let name = file.name().to_string();
            let size = file.size();
            let compressed_size = file.compressed_size();
            let method = method_name(file.compression());
            let is_dir = file.is_dir();

            entries.push(ApkEntry {
                name: name.clone(),
                size,
                compressed_size,
                method: method.clone(),
                is_dir,
            });

            if is_dir {
                continue;
            }

            if name == "AndroidManifest.xml" {
                let mut buf = Vec::new();
                file.read_to_end(&mut buf)
                    .map_err(|e| ApkError::new(APK_ENTRY, format!("{name}: {e}")))?;
                manifest_raw = Some(buf);
            } else if name == "resources.arsc" {
                let mut buf = Vec::new();
                file.read_to_end(&mut buf)
                    .map_err(|e| ApkError::new(APK_ENTRY, format!("{name}: {e}")))?;
                arsc_raw = Some(buf);
            } else if is_dex_name(&name) {
                let mut buf = Vec::new();
                file.read_to_end(&mut buf)
                    .map_err(|e| ApkError::new(APK_ENTRY, format!("{name}: {e}")))?;
                dex_files.push(DexFile {
                    name: name.clone(),
                    size: buf.len(),
                    bytes: buf,
                });
            } else if let Some((abi, libname)) = parse_lib_name(&name) {
                let size = file.size();
                let entry = native_libs.entry(abi.clone()).or_default();
                entry.push(NativeLib {
                    abi,
                    name: libname,
                    size,
                });
            } else if name.starts_with("assets/") {
                assets.push(ApkEntry {
                    name,
                    size,
                    compressed_size,
                    method,
                    is_dir: false,
                });
            }
        }

        dex_files.sort_by(|a, b| dex_number(&a.name).cmp(&dex_number(&b.name)));
        for libs in native_libs.values_mut() {
            libs.sort_by(|a, b| a.name.cmp(&b.name));
        }

        let entry_names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
        let signing = signing::detect(&entry_names, &bytes);

        Ok(Apk {
            source: "<memory>".to_string(),
            entries,
            dex_files,
            native_libs,
            assets,
            signing,
            manifest_raw,
            arsc_raw,
            raw: bytes,
        })
    }

    /// The raw bytes of the APK.
    pub fn raw_bytes(&self) -> &[u8] {
        &self.raw
    }

    /// Parse the typed `AndroidManifest.xml`.
    pub fn manifest(&self) -> Result<Manifest> {
        let raw = self
            .manifest_raw
            .as_ref()
            .ok_or_else(|| ApkError::new(APK_NO_MANIFEST, "APK contains no AndroidManifest.xml"))?;
        let doc = crate::axml::parse(raw)?;
        Manifest::parse(&doc)
    }

    /// Parse the AXML DOM of `AndroidManifest.xml`.
    pub fn manifest_dom(&self) -> Result<crate::axml::XmlDocument> {
        let raw = self
            .manifest_raw
            .as_ref()
            .ok_or_else(|| ApkError::new(APK_NO_MANIFEST, "APK contains no AndroidManifest.xml"))?;
        crate::axml::parse(raw)
    }

    /// Parse `resources.arsc`, if present.
    pub fn resources(&self) -> Result<Option<crate::arsc::ResourceTable>> {
        match &self.arsc_raw {
            Some(bytes) => Ok(Some(crate::arsc::ResourceTable::parse(bytes)?)),
            None => Ok(None),
        }
    }

    /// Sorted list of native ABI directory names.
    pub fn native_abis(&self) -> Vec<String> {
        self.native_libs.keys().cloned().collect()
    }

    /// Look up the raw bytes of a DEX file by entry name.
    pub fn dex_bytes(&self, name: &str) -> Option<&[u8]> {
        self.dex_files
            .iter()
            .find(|d| d.name == name)
            .map(|d| d.bytes.as_slice())
    }
}

fn method_name(method: zip::CompressionMethod) -> String {
    match method {
        zip::CompressionMethod::Stored => "stored".to_string(),
        zip::CompressionMethod::Deflated => "deflate".to_string(),
        other => format!("{other:?}").to_ascii_lowercase(),
    }
}

fn is_dex_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("classes") else {
        return false;
    };
    let Some(num) = rest.strip_suffix(".dex") else {
        return false;
    };
    num.is_empty() || num.chars().all(|c| c.is_ascii_digit())
}

fn dex_number(name: &str) -> u32 {
    let rest = name.strip_prefix("classes").unwrap_or(name);
    let num = rest.strip_suffix(".dex").unwrap_or(rest);
    if num.is_empty() {
        1
    } else {
        num.parse().unwrap_or(u32::MAX)
    }
}

fn parse_lib_name(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix("lib/")?;
    let (abi, lib) = rest.split_once('/')?;
    if abi.is_empty() || lib.is_empty() {
        return None;
    }
    Some((abi.to_string(), lib.to_string()))
}
