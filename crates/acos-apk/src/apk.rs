//! APK container ingest: zip entry listing, DEX files, native libs, assets and
//! signature-scheme detection.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use crate::error::{
    ApkError, Result, APK_ENTRY, APK_IO, APK_NOT_ZIP, APK_NO_MANIFEST, APK_SIGNING_MALFORMED,
    NATIVE_LIB_NOT_FOUND, UNSUPPORTED_ABI, ZIP_SLIP,
};
use crate::manifest::Manifest;
use crate::signing::{self, SigningInfo};

/// The host ABI ACOS can execute (the only ABI `acos-native-loader` maps).
///
/// This is deliberately a single value: the confining sandbox and the bionic
/// compatibility table are x86_64-only. An APK that ships only other ABIs is a
/// structured [`UNSUPPORTED_ABI`] error, never a silent fallback.
pub const HOST_ABI: &str = "x86_64";

/// The device ABI list in preference order — this host's `Build.SUPPORTED_ABIS`.
///
/// AOSP's `PackageManager` is the authority here, not a guess of ours:
/// `NativeLibraryHelper.findSupportedAbi` walks the APK's `lib/<abi>/` entries
/// and keeps the ABI whose index in this list is **lowest** ("The entry that
/// comes in first (i.e. with a lower index) has the higher priority."), returns
/// `NO_NATIVE_LIBRARIES` when the APK has no `lib/` at all, and
/// `INSTALL_FAILED_NO_MATCHING_ABIS` when it has `lib/` but no match
/// (`frameworks/base/core/jni/com_android_internal_content_NativeLibraryHelper.cpp`).
/// The decision is **per package**, and
/// `iterateOverNativeFiles` then extracts only the chosen ABI's directory.
pub const SUPPORTED_ABIS: [&str; 1] = [HOST_ABI];

/// The package-level ABI verdict for one APK, as an AOSP install would decide it.
///
/// See [`Apk::abi_gate`] for the rule and [`SUPPORTED_ABIS`] for the citation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiGate {
    /// The APK has no `lib/` entries (pure Java/Kotlin): no ABI question.
    NoNative,
    /// A host ABI directory exists, so that ABI is the package's ABI.
    Supported {
        /// The host ABI the package installs under ([`HOST_ABI`] today).
        abi: String,
    },
    /// The APK ships native code, but no directory for any host ABI.
    Unsupported {
        /// Every ABI directory the APK does provide, sorted.
        apk_abis: Vec<String>,
        /// A bounded, deterministic `lib/<abi>/<name>` sample as evidence.
        libs_sample: Vec<String>,
    },
}

impl AbiGate {
    /// Whether this verdict refuses the APK (the documented `UNSUPPORTED_ABI`).
    #[must_use]
    pub fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported { .. })
    }
}

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
    /// DER X.509 **signer** certificates (the platform's `PackageInfo.signatures`).
    pub signing_certificates: Vec<Vec<u8>>,
    /// Why `signing_certificates` is empty despite a signature block being
    /// present. An error here is never papered over by a lower scheme.
    pub signing_certificates_error: Option<String>,
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
        // v1 signature blocks (`META-INF/*.RSA|DSA|EC`), read after the index
        // pass: their bytes are the PKCS#7 fallback for the signing certificate.
        let mut v1_blocks_to_read: Vec<(usize, String, u64)> = Vec::new();

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
                let uncompressed_in_apk =
                    matches!(file.compression(), zip::CompressionMethod::Stored);
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
            } else if is_v1_signature_block(&name) {
                v1_blocks_to_read.push((i, name, size));
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
        // A `.RSA`/`.DSA`/`.EC` block is a v1 signature only with its matching
        // `META-INF/<stem>.SF`; pairing prevents an unrelated asset-shaped
        // entry from being treated as one. Read every candidate through the
        // size/ratio caps: the size fields are untrusted.
        let sf_stems: BTreeSet<String> = entry_names
            .iter()
            .filter(|n| n.to_ascii_uppercase().starts_with("META-INF/"))
            .filter(|n| n.to_ascii_uppercase().ends_with(".SF"))
            .map(|n| v1_stem(n))
            .collect();
        let mut v1_blocks: Vec<Vec<u8>> = Vec::new();
        let mut signing_certificates_error: Option<String> = None;
        // Both the number of candidate blocks and their sizes are untrusted, so
        // the count and the declared total are checked *before* any block is
        // read (a ~50 MB APK with ~10k pairs would otherwise make ingest read
        // ~10 GB into host memory). A cap violation is a hard signing error, not
        // a silent subset: dropping blocks could change which signer wins.
        let v1_candidates: Vec<(usize, String, u64)> = v1_blocks_to_read
            .into_iter()
            .filter(|(_, name, _)| sf_stems.contains(&v1_stem(name)))
            .collect();
        let declared_total: u64 = v1_candidates.iter().map(|(_, _, size)| *size).sum();
        let mut v1_cap_exceeded = false;
        if v1_candidates.len() > signing::MAX_V1_SIGNATURE_BLOCKS {
            signing_certificates_error = Some(
                ApkError::new(
                    APK_SIGNING_MALFORMED,
                    format!(
                        "{} v1 signature blocks exceed cap {}",
                        v1_candidates.len(),
                        signing::MAX_V1_SIGNATURE_BLOCKS
                    ),
                )
                .to_string(),
            );
            v1_cap_exceeded = true;
        } else if declared_total > signing::MAX_TOTAL_V1_SIGNATURE_BYTES as u64 {
            signing_certificates_error = Some(
                ApkError::new(
                    APK_SIGNING_MALFORMED,
                    format!(
                        "v1 signature blocks declare {declared_total} bytes, exceeding total cap {}",
                        signing::MAX_TOTAL_V1_SIGNATURE_BYTES
                    ),
                )
                .to_string(),
            );
            v1_cap_exceeded = true;
        }
        // The declared sizes are untrusted, so the bytes actually read are
        // summed too, and reading stops the moment the total cap is passed.
        let mut v1_total_read: u64 = 0;
        if !v1_cap_exceeded {
            for (i, name, _) in v1_candidates {
                let Ok(mut file) = archive.by_index(i) else {
                    signing_certificates_error =
                        Some(format!("{name}: unreadable signature block"));
                    continue;
                };
                match read_signature_block(
                    &name,
                    file.size() as usize,
                    file.compressed_size() as usize,
                    &mut file,
                ) {
                    Ok(buf) => {
                        v1_total_read += buf.len() as u64;
                        if v1_total_read > signing::MAX_TOTAL_V1_SIGNATURE_BYTES as u64 {
                            signing_certificates_error = Some(
                                ApkError::new(
                                    APK_SIGNING_MALFORMED,
                                    format!(
                                        "v1 signature blocks exceed total cap {} bytes",
                                        signing::MAX_TOTAL_V1_SIGNATURE_BYTES
                                    ),
                                )
                                .to_string(),
                            );
                            v1_blocks.clear();
                            v1_cap_exceeded = true;
                            break;
                        }
                        v1_blocks.push(buf);
                    }
                    Err(e) => signing_certificates_error = Some(e.to_string()),
                }
            }
        }
        let signing_certificates = if v1_cap_exceeded {
            // No further reads and no partial signer set after a cap violation.
            Vec::new()
        } else {
            match signing::certificates(&bytes, &v1_blocks) {
                Ok(certs) => certs,
                Err(e) => {
                    if signing_certificates_error.is_none() {
                        signing_certificates_error = Some(e.to_string());
                    }
                    Vec::new()
                }
            }
        };

        Ok(Apk {
            source: "<memory>".to_string(),
            entries,
            dex_files,
            native_libs,
            assets,
            signing,
            signing_certificates,
            signing_certificates_error,
            manifest_raw,
            arsc_raw,
            raw: bytes,
        })
    }

    /// The raw bytes of the APK.
    pub fn raw_bytes(&self) -> &[u8] {
        &self.raw
    }

    /// Parse the typed `AndroidManifest.xml`, resolving `<meta-data
    /// android:value="@...">` references against `resources.arsc` when present.
    pub fn manifest(&self) -> Result<Manifest> {
        let raw = self
            .manifest_raw
            .as_ref()
            .ok_or_else(|| ApkError::new(APK_NO_MANIFEST, "APK contains no AndroidManifest.xml"))?;
        let doc = crate::axml::parse(raw)?;
        let res = if self.arsc_raw.is_some()
            && (Manifest::has_meta_data_value_reference(&doc)
                || Manifest::has_provider_init_order_reference(&doc))
        {
            self.resources().ok().flatten()
        } else {
            None
        };
        Manifest::parse_with_resources(&doc, res.as_ref())
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

    /// The package-level ABI decision, exactly as AOSP's `PackageManager` makes
    /// it (see [`SUPPORTED_ABIS`]).
    ///
    /// The decision is **per package, never per library**: the first ABI in
    /// [`SUPPORTED_ABIS`] that has a `lib/<abi>/` directory wins and *that*
    /// directory is the one an install would extract. An APK that carries
    /// `lib/x86_64/liba.so` and `lib/arm64-v8a/libb.so` therefore installs as an
    /// x86_64 package — `liba.so` works and the first `dlopen("libb.so")` fails
    /// with the app's own `UnsatisfiedLinkError`, which is what the platform
    /// does and what ACOS reproduces. Only an APK with **no** directory for a
    /// host ABI is [`AbiGate::Unsupported`].
    #[must_use]
    pub fn abi_gate(&self) -> AbiGate {
        let abis: Vec<String> = self
            .native_libs
            .iter()
            .filter(|(_, libs)| !libs.is_empty())
            .map(|(abi, _)| abi.clone())
            .collect();
        if abis.is_empty() {
            return AbiGate::NoNative;
        }
        for host in SUPPORTED_ABIS {
            if abis.iter().any(|a| a == host) {
                return AbiGate::Supported {
                    abi: host.to_string(),
                };
            }
        }
        AbiGate::Unsupported {
            apk_abis: abis,
            libs_sample: self.native_libs_sample(8),
        }
    }

    /// Up to `max` `lib/<abi>/<name>` entries, ABI then name order.
    ///
    /// Evidence for an ABI refusal: a caller sees *which* libraries exist and
    /// under which ABI, instead of a bare "arm64" claim.
    #[must_use]
    pub fn native_libs_sample(&self, max: usize) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for (abi, libs) in &self.native_libs {
            let mut files: Vec<&str> = libs.iter().map(|l| l.name.as_str()).collect();
            files.sort_unstable();
            for name in files {
                if names.len() >= max {
                    return names;
                }
                names.push(format!("lib/{abi}/{name}"));
            }
        }
        names
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
            ApkError::new(
                ZIP_SLIP,
                format!("ABI directory {abi:?} is not a plain name"),
            )
        })?;
        let file = safe_component(name).ok_or_else(|| {
            ApkError::new(
                ZIP_SLIP,
                format!("library name {name:?} is not a plain name"),
            )
        })?;
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
        // The zip header is guest-controlled, so never size the allocation
        // from it directly: cap by the real archive length and a fixed ceiling
        // (W67A #14), then verify the read against the declared size.
        const MAX_NATIVE_LIB_BYTES: u64 = 256 * 1024 * 1024;
        let declared = entry.size();
        let initial = declared
            .min(self.raw.len() as u64)
            .min(MAX_NATIVE_LIB_BYTES) as usize;
        let mut bytes = Vec::with_capacity(initial);
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| ApkError::new(APK_ENTRY, format!("{entry_name}: {e}")))?;
        if bytes.len() as u64 != declared {
            return Err(ApkError::new(
                APK_ENTRY,
                format!(
                    "{entry_name}: declared {declared} bytes but read {}",
                    bytes.len()
                ),
            ));
        }

        let dest_dir = dest_dir.as_ref();
        std::fs::create_dir_all(dest_dir)
            .map_err(|e| ApkError::new(APK_IO, format!("create {}: {e}", dest_dir.display())))?;
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

    /// Raw bytes of any zip entry, or `None` when it is absent / unreadable.
    ///
    /// Used by the static native-library cross-check ([`crate::native`]) to read
    /// the ELF headers of the libraries the APK ships. A read error is `None`:
    /// the check must never turn `inspect` into an error.
    pub fn entry_bytes(&self, name: &str) -> Option<Vec<u8>> {
        if let Some(dex) = self.dex_files.iter().find(|d| d.name == name) {
            return Some(dex.bytes.clone());
        }
        let mut archive = zip::ZipArchive::new(Cursor::new(self.raw.as_slice())).ok()?;
        let mut entry = archive.by_name(name).ok()?;
        const MAX_BYTES: u64 = 256 * 1024 * 1024;
        let declared = entry.size();
        if declared > MAX_BYTES {
            return None;
        }
        let mut bytes = Vec::with_capacity(declared.min(self.raw.len() as u64) as usize);
        std::io::Read::read_to_end(&mut entry, &mut bytes).ok()?;
        Some(bytes)
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

/// `META-INF/<stem>.SF` and `META-INF/<stem>.RSA|DSA|EC` must pair.
fn v1_stem(name: &str) -> String {
    let upper = name.to_ascii_uppercase();
    for ext in [".SF", ".RSA", ".DSA", ".EC"] {
        if let Some(stem) = upper.strip_suffix(ext) {
            return stem.to_string();
        }
    }
    upper
}

/// Read one untrusted v1 signature block with an absolute size cap and a
/// compression-ratio cap, both checked before/while decompressing.
fn read_signature_block(
    name: &str,
    declared: usize,
    compressed: usize,
    reader: &mut impl Read,
) -> std::result::Result<Vec<u8>, ApkError> {
    if declared > signing::MAX_SIGNATURE_BLOCK_BYTES {
        return Err(ApkError::new(
            APK_SIGNING_MALFORMED,
            format!(
                "{name}: signature block size {declared} exceeds cap {}",
                signing::MAX_SIGNATURE_BLOCK_BYTES
            ),
        ));
    }
    if compressed > 0 && declared > compressed.saturating_mul(signing::MAX_SIGNATURE_BLOCK_RATIO) {
        return Err(ApkError::new(
            APK_SIGNING_MALFORMED,
            format!("{name}: signature block compression ratio exceeds cap"),
        ));
    }
    let mut buf = Vec::new();
    // The declared size is untrusted, so cap the actual read as well.
    reader
        .take(signing::MAX_SIGNATURE_BLOCK_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| ApkError::new(APK_SIGNING_MALFORMED, format!("{name}: {e}")))?;
    if buf.len() > signing::MAX_SIGNATURE_BLOCK_BYTES {
        return Err(ApkError::new(
            APK_SIGNING_MALFORMED,
            format!("{name}: signature block exceeds cap"),
        ));
    }
    Ok(buf)
}

/// `META-INF/*.RSA` / `*.DSA` / `*.EC`: a v1 (JAR) signature block, whose
/// PKCS#7 payload carries the signing certificate.
fn is_v1_signature_block(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.starts_with("META-INF/")
        && (upper.ends_with(".RSA") || upper.ends_with(".DSA") || upper.ends_with(".EC"))
}

#[cfg(test)]
mod signature_cap_tests {
    use super::*;
    use std::io::Cursor;

    /// Build a stored (uncompressed) zip from `(name, bytes)` entries, so the
    /// per-block compression-ratio cap never masks the cap under test.
    fn stored_zip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in entries {
            zip.start_file(name.as_str(), options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn v1_pair(stem: &str, len: usize) -> Vec<(String, Vec<u8>)> {
        vec![
            (format!("META-INF/{stem}.SF"), vec![b'S'; len]),
            (format!("META-INF/{stem}.RSA"), vec![b'R'; len]),
        ]
    }

    #[test]
    fn too_many_v1_signature_blocks_is_rejected() {
        let mut entries = Vec::new();
        for i in 0..=signing::MAX_V1_SIGNATURE_BLOCKS {
            entries.extend(v1_pair(&format!("S{i}"), 64));
        }
        let apk = Apk::from_bytes(stored_zip(&entries)).expect("zip should open");
        let err = apk
            .signing_certificates_error
            .expect("cap violation must be an error");
        assert!(err.starts_with(APK_SIGNING_MALFORMED), "{err}");
        assert!(err.contains("exceed cap"), "{err}");
        assert!(apk.signing_certificates.is_empty());
    }

    #[test]
    fn total_v1_signature_bytes_over_cap_is_rejected() {
        // Five blocks of 900 KiB: each is under the per-block cap and the count
        // is under the block cap, but 4.5 MiB exceeds the 4 MiB total.
        let block = 900 * 1024;
        let mut entries = Vec::new();
        for i in 0..5 {
            entries.extend(v1_pair(&format!("B{i}"), block));
        }
        let apk = Apk::from_bytes(stored_zip(&entries)).expect("zip should open");
        let err = apk
            .signing_certificates_error
            .expect("cap violation must be an error");
        assert!(err.starts_with(APK_SIGNING_MALFORMED), "{err}");
        assert!(err.contains("total cap"), "{err}");
        assert!(apk.signing_certificates.is_empty());
    }

    #[test]
    fn one_normal_v1_block_still_yields_its_signer() {
        // A real `.SF` + `.RSA` pair from a fixture APK, re-zipped alone so the
        // v1 (PKCS#7) path — not the v2/v3 block — supplies the signer.
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/hello-jni.apk");
        let mut archive =
            zip::ZipArchive::new(Cursor::new(std::fs::read(fixture).unwrap())).unwrap();
        let mut entries = Vec::new();
        let mut rsa = Vec::new();
        for name in ["META-INF/ANDROIDD.SF", "META-INF/ANDROIDD.RSA"] {
            let mut file = archive.by_name(name).expect("fixture v1 entry");
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).unwrap();
            if name.ends_with(".RSA") {
                rsa = buf.clone();
            }
            entries.push((name.to_string(), buf));
        }
        // The uncapped, direct v1 parse is the reference result.
        let direct = signing::certificates(&[], &[rsa]).expect("v1 PKCS#7 parses");
        assert!(!direct.is_empty());

        let apk = Apk::from_bytes(stored_zip(&entries)).expect("zip should open");
        assert_eq!(apk.signing_certificates_error, None);
        assert_eq!(apk.signing_certificates, direct);
        assert_eq!(apk.signing_certificates.len(), 1);
        assert_eq!(apk.signing_certificates[0][0], 0x30, "DER SEQUENCE");
    }

    #[test]
    fn declared_size_over_cap_is_rejected_before_reading() {
        let mut reader = Cursor::new(vec![0u8; 8]);
        let err = read_signature_block(
            "META-INF/A.RSA",
            signing::MAX_SIGNATURE_BLOCK_BYTES + 1,
            1,
            &mut reader,
        )
        .unwrap_err();
        assert_eq!(err.code(), APK_SIGNING_MALFORMED);
        assert!(err.message().contains("exceeds cap"));
    }

    #[test]
    fn compression_ratio_over_cap_is_rejected() {
        let mut reader = Cursor::new(vec![0u8; 8]);
        let err = read_signature_block(
            "META-INF/A.RSA",
            signing::MAX_SIGNATURE_BLOCK_RATIO * 2,
            1,
            &mut reader,
        )
        .unwrap_err();
        assert_eq!(err.code(), APK_SIGNING_MALFORMED);
        assert!(err.message().contains("ratio"));
    }

    #[test]
    fn a_reader_that_lies_about_its_size_is_still_capped() {
        // The zip central directory can declare a small size while the stream
        // yields more; `read_to_end` must stop at the cap.
        let mut reader = Cursor::new(vec![0u8; signing::MAX_SIGNATURE_BLOCK_BYTES + 10]);
        let err = read_signature_block("META-INF/A.RSA", 16, 16, &mut reader).unwrap_err();
        assert_eq!(err.code(), APK_SIGNING_MALFORMED);
    }
}
