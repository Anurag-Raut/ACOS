//! APK container ingest: zip entry listing, DEX files, native libs, assets and
//! signature-scheme detection.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use crate::error::{
    ApkError, Result, APK_ENTRY, APK_IO, APK_NOT_ZIP, APK_NO_MANIFEST, NATIVE_LIB_NOT_FOUND,
    UNSUPPORTED_ABI, ZIP_SLIP,
};
use crate::manifest::Manifest;
use crate::signing::{self, SigningInfo};

/// The host ABI ACOS can execute (the only ABI `acos-native-loader` maps).
///
/// This is deliberately a single value: the confining sandbox and the bionic
/// compatibility table are x86_64-only. An APK that ships only other ABIs is a
/// structured [`UNSUPPORTED_ABI`] error, never a silent fallback.
pub const HOST_ABI: &str = "x86_64";

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
    /// Zip compression method (`stored` / `deflate`).
    pub method: String,
    /// Compressed size on disk.
    pub compressed_size: u64,
    /// Byte offset of the entry's payload in the APK file.
    ///
    /// For `stored` entries this is the offset an `android_dlopen_ext`-style
    /// `mmap(fd, offset, size, PROT_EXEC)` maps directly; for `deflate` entries
    /// the bytes must be extracted first.
    pub data_offset: u64,
    /// Whether the payload is stored uncompressed (mmap-able in place).
    pub uncompressed_in_apk: bool,
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
                let compressed_size = file.compressed_size();
                let data_offset = file.data_start();
                let uncompressed_in_apk = matches!(file.compression(), zip::CompressionMethod::Stored);
                let method = method_name(file.compression());
                let entry = native_libs.entry(abi.clone()).or_default();
                entry.push(NativeLib {
                    abi,
                    name: libname,
                    size,
                    method,
                    compressed_size,
                    data_offset,
                    uncompressed_in_apk,
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

    /// Whether the APK ships any native code at all.
    pub fn has_native_libs(&self) -> bool {
        self.native_libs.values().any(|libs| !libs.is_empty())
    }

    /// Refuse to run an APK whose native code cannot execute on this host.
    ///
    /// An APK with no `lib/` entries passes (it has no native code to reject).
    /// An APK whose only ABI directories exclude `requested` fails with
    /// [`UNSUPPORTED_ABI`], naming every ABI the APK does provide so an agent can
    /// pick a split or rebuild for the host.
    pub fn check_native_abi(&self, requested: &str) -> Result<()> {
        let available = self.native_abis();
        if available.is_empty() || available.iter().any(|a| a == requested) {
            return Ok(());
        }
        Err(ApkError::new(
            UNSUPPORTED_ABI,
            format!(
                "APK has no native libraries for ABI {requested:?}; available: [{}]",
                available.join(", ")
            ),
        ))
    }

    /// Find one native library entry by ABI and file name.
    pub fn find_native_lib(&self, abi: &str, name: &str) -> Option<&NativeLib> {
        self.native_libs
            .get(abi)
            .and_then(|libs| libs.iter().find(|l| l.name == name))
    }

    /// Extract one native library into `dest_dir`, never trusting its name.
    ///
    /// The ABI directory and the file name are both validated as single path
    /// components; a name containing `/`, `\\`, `..` or a NUL is rejected with
    /// [`ZIP_SLIP`]. The written path is re-checked to stay under `dest_dir`,
    /// so a hostile archive cannot write outside the app's VFS lib directory.
    pub fn extract_native_lib(
        &self,
        abi: &str,
        name: &str,
        dest_dir: impl AsRef<Path>,
    ) -> Result<PathBuf> {
        let abi = safe_component(abi).ok_or_else(|| {
            ApkError::new(ZIP_SLIP, format!("ABI directory {abi:?} is not a plain name"))
        })?;
        let file = safe_component(name)
            .ok_or_else(|| ApkError::new(ZIP_SLIP, format!("library name {name:?} is not a plain name")))?;
        if self.find_native_lib(abi, file).is_none() {
            return Err(ApkError::new(
                NATIVE_LIB_NOT_FOUND,
                format!("APK has no lib/{abi}/{file}"),
            ));
        }
        let entry_name = format!("lib/{abi}/{file}");
        let mut archive = zip::ZipArchive::new(Cursor::new(self.raw.as_slice()))
            .map_err(|e| ApkError::new(APK_NOT_ZIP, e.to_string()))?;
        let mut entry = archive
            .by_name(&entry_name)
            .map_err(|e| ApkError::new(NATIVE_LIB_NOT_FOUND, format!("{entry_name}: {e}")))?;
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| ApkError::new(APK_ENTRY, format!("{entry_name}: {e}")))?;

        let dest_dir = dest_dir.as_ref();
        std::fs::create_dir_all(dest_dir).map_err(|e| {
            ApkError::new(APK_IO, format!("create {}: {e}", dest_dir.display()))
        })?;
        let target = dest_dir.join(file);
        // Belt and braces: the joined path must still be a direct child of the
        // destination directory. `file` is a single component, so this can only
        // fail if the filesystem itself surprises us.
        if target.parent() != Some(dest_dir) {
            return Err(ApkError::new(
                ZIP_SLIP,
                format!("{} escapes {}", target.display(), dest_dir.display()),
            ));
        }
        std::fs::write(&target, &bytes)
            .map_err(|e| ApkError::new(APK_IO, format!("write {}: {e}", target.display())))?;
        Ok(target)
    }

    /// Extract every native library for `abi` into `dest_dir`.
    ///
    /// Returns `(name, path)` pairs in the APK's deterministic (sorted) order.
    pub fn extract_native_libs(
        &self,
        abi: &str,
        dest_dir: impl AsRef<Path>,
    ) -> Result<Vec<(String, PathBuf)>> {
        self.check_native_abi(abi)?;
        let dest_dir = dest_dir.as_ref();
        let names: Vec<String> = self
            .native_libs
            .get(abi)
            .map(|libs| libs.iter().map(|l| l.name.clone()).collect())
            .unwrap_or_default();
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let path = self.extract_native_lib(abi, &name, dest_dir)?;
            out.push((name, path));
        }
        Ok(out)
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

/// A single, safe path component: no separators, no traversal, not empty.
fn safe_component(name: &str) -> Option<&str> {
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return None;
    }
    Some(name)
}

fn parse_lib_name(name: &str) -> Option<(String, String)> {
    // A native library entry must be exactly `lib/<abi>/<file>` with no further
    // separators: `lib/x86_64/sub/evil.so` is rejected here (the `lib` part
    // contains `/`), so it never reaches extraction.
    let rest = name.strip_prefix("lib/")?;
    let (abi, lib) = rest.split_once('/')?;
    if safe_component(abi).is_none() || safe_component(lib).is_none() {
        return None;
    }
    Some((abi.to_string(), lib.to_string()))
}

