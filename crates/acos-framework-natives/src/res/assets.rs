//! Asset sources: the [`AssetSource`] trait and the [`ApkAssets`] loader over
//! a zip container.
//!
//! The APK file is memory-mapped (or supplied as an owned byte buffer); it is
//! never copied wholesale. The zip central directory is **not** materialised
//! into a map at load time — it is scanned on demand, so loading a 7,880-entry
//! `framework-res.apk` allocates only the lazily-parsed resource table index.
//! Stored entries (including `resources.arsc` and compiled XML) are exposed as
//! borrowed sub-slices with zero copying; deflated entries are decompressed on
//! demand through the `zip` crate.

use std::io::{Cursor, Read};
use std::path::Path;

use memmap2::Mmap;

use crate::res::arsc::ResTableIndex;
use crate::res::error::{ResError, Result, RES_ASSET_NOT_FOUND, RES_IO, RES_ZIP};

/// Backing storage for an APK.
#[derive(Debug)]
pub enum Backing {
    /// A memory-mapped file.
    Mmap(Mmap),
    /// Owned bytes (tests / in-memory APKs).
    Bytes(Vec<u8>),
}

impl Backing {
    /// The raw bytes.
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Backing::Mmap(m) => m,
            Backing::Bytes(b) => b,
        }
    }
}

/// Metadata for one zip central-directory entry.
#[derive(Debug, Clone, Copy)]
pub struct ZipEntryInfo {
    /// Offset of the entry data inside the file.
    pub data_start: u64,
    /// Uncompressed size.
    pub size: u64,
    /// Compression method.
    pub stored: bool,
    /// Whether the entry is a directory.
    pub is_dir: bool,
}

/// A read-only source of assets and resource tables.
pub trait AssetSource {
    /// Human-readable source name (path or `<memory>`).
    fn name(&self) -> &str;
    /// The raw backing bytes of the APK.
    fn bytes(&self) -> &[u8];
    /// The `resources.arsc` bytes the table index is relative to.
    fn resource_bytes(&self) -> Option<&[u8]>;
    /// The lazily-parsed resource table index, if the APK has one.
    fn resource_table(&self) -> Option<&ResTableIndex>;
    /// Read an entry fully (decompressing if needed).
    fn read_entry(&self, name: &str) -> Result<Vec<u8>>;
    /// Borrow a stored entry without copying.
    fn open_stored(&self, name: &str) -> Option<&[u8]>;
    /// List entries whose names start with `dir`.
    fn list(&self, dir: &str) -> Result<Vec<String>>;
}

const EOCD_SIG: u32 = 0x0605_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;
const ZIP64_OFFSET: u64 = 0xFFFF_FFFF;

/// An APK loaded as an asset source.
#[derive(Debug)]
pub struct ApkAssets {
    path: String,
    debug_name: String,
    backing: Backing,
    entry_count: usize,
    resource_table: Option<ResTableIndex>,
    arsc_owned: Option<Vec<u8>>,
    arsc_range: Option<(usize, usize)>,
    loaded_mtime: Option<std::time::SystemTime>,
    loaded_size: u64,
}

impl ApkAssets {
    /// Open and map an APK from disk.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = std::fs::File::open(path)
            .map_err(|e| ResError::new(RES_IO, format!("{}: {e}", path.display())))?;
        let meta = file.metadata().ok();
        let loaded_mtime = meta.as_ref().and_then(|m| m.modified().ok());
        let loaded_size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        // SAFETY: the fixture APKs are read-only and not modified while mapped;
        // `ApkAssets` never writes to the mapping. Documented in NATIVES.md.
        let mmap = unsafe { Mmap::map(&file) }
            .map_err(|e| ResError::new(RES_IO, format!("mmap {}: {e}", path.display())))?;
        let mut apk = Self::from_backing(Backing::Mmap(mmap), path.display().to_string())?;
        apk.loaded_mtime = loaded_mtime;
        apk.loaded_size = loaded_size;
        Ok(apk)
    }

    /// Build an APK source from owned bytes.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        Self::from_backing(Backing::Bytes(bytes), "<memory>".to_string())
    }

    fn from_backing(backing: Backing, path: String) -> Result<Self> {
        let (_central_dir, entry_count) = locate_central_directory(backing.as_slice());
        // Find `resources.arsc` and parse its table index. deflated ARSC is
        // decompressed once; stored ARSC stays a borrowed sub-slice.
        let mut arsc_range = None;
        let mut arsc_stored = false;
        if _central_dir.is_some() {
            for_each_central_entry(backing.as_slice(), |name, info| {
                if name == "resources.arsc" {
                    arsc_range = Some((info.data_start as usize, info.size as usize));
                    arsc_stored = info.stored;
                }
            });
        }
        let mut resource_table = None;
        let mut arsc_owned = None;
        if let Some((start, size)) = arsc_range {
            if arsc_stored {
                if let Some(slice) = backing.as_slice().get(start..start.saturating_add(size)) {
                    resource_table = ResTableIndex::parse(slice).ok();
                }
            } else if let Ok(mut archive) = zip::ZipArchive::new(Cursor::new(backing.as_slice())) {
                if let Ok(mut file) = archive.by_name("resources.arsc") {
                    let mut buf = Vec::new();
                    if file.read_to_end(&mut buf).is_ok() {
                        resource_table = ResTableIndex::parse(&buf).ok();
                        arsc_owned = Some(buf);
                    }
                }
            }
        }
        Ok(ApkAssets {
            path: path.clone(),
            debug_name: path,
            backing,
            entry_count,
            resource_table,
            arsc_owned,
            arsc_range,
            loaded_mtime: None,
            loaded_size: 0,
        })
    }

    /// The APK path (or `<memory>`).
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The debug name reported by `ApkAssets.nativeGetDebugName`.
    pub fn debug_name(&self) -> &str {
        &self.debug_name
    }

    /// `ApkAssets.nativeIsUpToDate`: the file's size and mtime still match.
    pub fn is_up_to_date(&self) -> bool {
        let Ok(meta) = std::fs::metadata(&self.path) else {
            return matches!(self.backing, Backing::Bytes(_));
        };
        let mtime = meta.modified().ok();
        meta.len() == self.loaded_size && mtime == self.loaded_mtime
    }

    /// The `resources.arsc` bytes the table index is relative to.
    pub fn resource_bytes(&self) -> Option<&[u8]> {
        if let Some(bytes) = &self.arsc_owned {
            return Some(bytes);
        }
        let (start, size) = self.arsc_range?;
        self.backing
            .as_slice()
            .get(start..start.saturating_add(size))
    }

    /// The raw `resources.arsc` slice, if stored (zero copy).
    pub fn arsc_slice(&self) -> Option<&[u8]> {
        if self.arsc_owned.is_some() {
            return None;
        }
        self.resource_bytes()
    }

    /// Entry metadata by exact name (scans the central directory on demand).
    pub fn entry_info(&self, name: &str) -> Option<ZipEntryInfo> {
        let name = Self::normalize(name);
        let mut found = None;
        for_each_central_entry(self.backing.as_slice(), |n, info| {
            if n == name {
                found = Some(info);
            }
        });
        found
    }

    /// All entry names (scans the central directory).
    pub fn entry_names(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.entry_count);
        for_each_central_entry(self.backing.as_slice(), |name, _| {
            out.push(name.to_string());
        });
        out
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entry_count
    }

    /// True when the APK has no entries.
    pub fn is_empty(&self) -> bool {
        self.entry_count == 0
    }

    /// Normalise an asset path.
    pub fn normalize(name: &str) -> String {
        name.trim_start_matches('/').to_string()
    }
}

impl AssetSource for ApkAssets {
    fn name(&self) -> &str {
        &self.path
    }

    fn bytes(&self) -> &[u8] {
        self.backing.as_slice()
    }

    fn resource_bytes(&self) -> Option<&[u8]> {
        self.resource_bytes()
    }

    fn resource_table(&self) -> Option<&ResTableIndex> {
        self.resource_table.as_ref()
    }

    fn read_entry(&self, name: &str) -> Result<Vec<u8>> {
        let name = Self::normalize(name);
        let Some(info) = self.entry_info(&name) else {
            return Err(ResError::new(RES_ASSET_NOT_FOUND, name));
        };
        if info.is_dir {
            return Err(ResError::new(RES_ASSET_NOT_FOUND, name));
        }
        if info.stored {
            let start = info.data_start as usize;
            let end = start.saturating_add(info.size as usize);
            return self
                .backing
                .as_slice()
                .get(start..end)
                .map(|s| s.to_vec())
                .ok_or_else(|| ResError::new(RES_ZIP, "stored entry out of range"));
        }
        let cursor = Cursor::new(self.backing.as_slice());
        let mut archive =
            zip::ZipArchive::new(cursor).map_err(|e| ResError::new(RES_ZIP, e.to_string()))?;
        let mut file = archive
            .by_name(&name)
            .map_err(|e| ResError::new(RES_ZIP, e.to_string()))?;
        let mut buf = Vec::with_capacity(info.size as usize);
        file.read_to_end(&mut buf)
            .map_err(|e| ResError::new(RES_ZIP, e.to_string()))?;
        Ok(buf)
    }

    fn open_stored(&self, name: &str) -> Option<&[u8]> {
        let name = Self::normalize(name);
        let info = self.entry_info(&name)?;
        if !info.stored {
            return None;
        }
        let start = info.data_start as usize;
        let end = start.saturating_add(info.size as usize);
        self.backing.as_slice().get(start..end)
    }

    fn list(&self, dir: &str) -> Result<Vec<String>> {
        let dir = Self::normalize(dir);
        let prefix = if dir.is_empty() || dir.ends_with('/') {
            dir
        } else {
            format!("{dir}/")
        };
        let mut out: Vec<String> = Vec::new();
        for_each_central_entry(self.backing.as_slice(), |name, _| {
            if name.starts_with(&prefix) {
                out.push(name.to_string());
            }
        });
        out.sort();
        Ok(out)
    }
}

/// A readable asset (the payload behind `nativeOpenAsset` /
/// `nativeOpenNonAsset`).
#[derive(Debug, Clone)]
pub struct AssetHandle {
    /// Asset cookie (index of the source + 1).
    pub cookie: i32,
    /// Asset name.
    pub name: String,
    /// Asset contents.
    pub data: Vec<u8>,
    /// Current read position.
    pub position: usize,
}

impl AssetHandle {
    /// Create a handle at position 0.
    pub fn new(cookie: i32, name: impl Into<String>, data: Vec<u8>) -> Self {
        AssetHandle {
            cookie,
            name: name.into(),
            data,
            position: 0,
        }
    }

    /// `nativeAssetGetLength`.
    pub fn length(&self) -> u64 {
        self.data.len() as u64
    }

    /// `nativeAssetGetRemainingLength`.
    pub fn remaining_length(&self) -> u64 {
        self.data.len().saturating_sub(self.position) as u64
    }

    /// `nativeAssetRead`.
    pub fn read(&mut self, out: &mut [u8], off: usize, len: usize) -> i32 {
        let avail = self.data.len().saturating_sub(self.position);
        let n = len.min(avail);
        if off.saturating_add(n) > out.len() {
            return -1;
        }
        out[off..off + n].copy_from_slice(&self.data[self.position..self.position + n]);
        self.position += n;
        n as i32
    }

    /// `nativeAssetReadChar`.
    pub fn read_char(&mut self) -> i32 {
        match self.data.get(self.position) {
            Some(b) => {
                self.position += 1;
                *b as i32
            }
            None => -1,
        }
    }

    /// `nativeAssetSeek` (`whence`: 0 = SET, 1 = CUR, 2 = END).
    pub fn seek(&mut self, offset: i64, whence: i32) -> i64 {
        let base = match whence {
            0 => 0i64,
            1 => self.position as i64,
            2 => self.data.len() as i64,
            _ => return -1,
        };
        let target = base.saturating_add(offset);
        if target < 0 || target > self.data.len() as i64 {
            return -1;
        }
        self.position = target as usize;
        self.position as i64
    }
}

fn u16le(data: &[u8], at: usize) -> Option<u16> {
    data.get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn u32le(data: &[u8], at: usize) -> Option<u32> {
    data.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Find the end-of-central-directory record.
fn find_eocd(data: &[u8]) -> Option<usize> {
    if data.len() < 22 {
        return None;
    }
    let min = data.len().saturating_sub(22 + 65535);
    let mut pos = data.len() - 22;
    loop {
        if u32le(data, pos) == Some(EOCD_SIG) {
            return Some(pos);
        }
        if pos == min {
            return None;
        }
        pos -= 1;
    }
}

/// Locate `(central_dir_offset, central_dir_size, entry_count)`.
fn locate_central_directory(data: &[u8]) -> (Option<(usize, usize)>, usize) {
    let Some(eocd) = find_eocd(data) else {
        return (None, 0);
    };
    let total = u16le(data, eocd + 10).unwrap_or(0) as usize;
    let size = u32le(data, eocd + 12).unwrap_or(0) as usize;
    let offset = u32le(data, eocd + 16).unwrap_or(0) as usize;
    if offset + size > data.len() {
        return (None, 0);
    }
    (Some((offset, size)), total)
}

/// Iterate every central-directory entry, computing the data offset of stored
/// entries from the local header.
fn for_each_central_entry(data: &[u8], mut f: impl FnMut(&str, ZipEntryInfo)) {
    let (Some((offset, size)), total) = locate_central_directory(data) else {
        return;
    };
    let end = (offset + size).min(data.len());
    let mut pos = offset;
    let mut seen = 0usize;
    while pos + 46 <= end && seen < total {
        if u32le(data, pos) != Some(CENTRAL_SIG) {
            break;
        }
        let method = u16le(data, pos + 10).unwrap_or(0);
        let uncompressed = u32le(data, pos + 24).unwrap_or(0) as u64;
        let name_len = u16le(data, pos + 28).unwrap_or(0) as usize;
        let extra_len = u16le(data, pos + 30).unwrap_or(0) as usize;
        let comment_len = u16le(data, pos + 32).unwrap_or(0) as usize;
        let local_off = u32le(data, pos + 42).unwrap_or(0) as u64;
        let Some(name_bytes) = data.get(pos + 46..pos + 46 + name_len) else {
            break;
        };
        let name = String::from_utf8_lossy(name_bytes);
        let is_dir = name.ends_with('/');
        // Compute the stored data offset from the local header.
        let mut data_start = 0u64;
        if method == 0 && local_off != ZIP64_OFFSET {
            let lo = local_off as usize;
            if u32le(data, lo) == Some(LOCAL_SIG) {
                let ln = u16le(data, lo + 26).unwrap_or(0) as u64;
                let le = u16le(data, lo + 28).unwrap_or(0) as u64;
                data_start = local_off + 30 + ln + le;
            }
        }
        f(
            &name,
            ZipEntryInfo {
                data_start,
                size: uncompressed,
                stored: method == 0,
                is_dir,
            },
        );
        seen += 1;
        pos += 46 + name_len + extra_len + comment_len;
    }
}
