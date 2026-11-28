//! Host-backed `ContentProvider` (`content://com.acos.picker/...`) for ACOS's
//! zero-overhead File & Photo Picker.
//!
//! When an app fires `ACTION_OPEN_DOCUMENT`, `ACTION_CREATE_DOCUMENT`,
//! `ACTION_GET_CONTENT`, `ACTION_PICK` (or an `ACTION_CHOOSER` wrapping one of
//! them) and no installed guest app handles it, ACOS captures the intent in
//! [`crate::intents::IntentState`]. A human (`acos view`) or agent
//! (`acos_service_ctl {module: "intents", op: "pick_file", ...}`) then picks a
//! host file.
//!
//! Rather than copying bytes into the guest or running an extra guest picker
//! process (0 ms boot overhead, 0 MiB guest RAM, 0% idle CPU), the host
//! registers the chosen file in [`PickerRegistry`], mints a scoped
//! `content://com.acos.picker/<id>/<filename>` URI, and delivers it to the
//! waiting activity via `ActivityResultItem` with
//! `FLAG_GRANT_READ_URI_PERMISSION` (plus `FLAG_GRANT_WRITE_URI_PERMISSION` for
//! `ACTION_CREATE_DOCUMENT`).
//!
//! When the guest app resolves `content://com.acos.picker/...` through
//! `ContentResolver`:
//!
//! * `IActivityManager.getContentProvider("com.acos.picker")` returns the
//!   in-process [`PickerProviderService`] binder registered in
//!   [`crate::provider::ProviderRegistry`].
//! * `openFile` (14) / `openAssetFile` (15) / `openTypedAssetFile` (23) open
//!   the registered host file with the requested mode (`"r"`, or `"w"`/`"rw"`
//!   only when explicitly registered writable) and pass the OS file descriptor
//!   over Binder (`SCM_RIGHTS`).
//! * `query` (1) returns an inline `CursorWindow` with standard `OpenableColumns`
//!   and `DocumentsContract.Document` columns (`_display_name`, `_size`,
//!   `mime_type`, `document_id`, `_id`, `flags`, `last_modified`).
//! * `getType` (2) / `getTypeAsync` (29) / `getTypeAnonymousAsync` (32) and
//!   `getStreamTypes` (22) return the file's MIME type.
//!
//! Security boundary: the provider ONLY serves URIs whose numeric `<id>` was
//! explicitly registered by the host in this session's [`PickerRegistry`]. Any
//! traversal segment (`..`, `.`, `\`, percent-encoded `%2e`/`%2f`/`%5c`) or
//! unknown id is refused before touching the filesystem.

use std::collections::{BTreeSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use acos_aidl::MethodInfo;
use acos_parcel::{Parcel, ParcelFd};

use crate::cursor::{uri_string, CursorResult, Field, QueryRequest};
use crate::error::{Result, ServiceError};
use crate::exception::write_no_exception;
use crate::host::{CallResult, PendingDelivery, ServiceObject};
use crate::parcelable;
use crate::value::Value;

/// ContentProvider authority served by the host-side picker.
pub const PICKER_AUTHORITY: &str = "com.acos.picker";

/// `IContentProvider` AIDL descriptor.
pub const CONTENT_PROVIDER_DESCRIPTOR: &str = "android.content.IContentProvider";

/// Synthetic package / class reported in `ProviderInfo` for `com.acos.picker`.
pub const PICKER_PACKAGE: &str = "com.acos.picker";
/// Synthetic `ProviderInfo.name`.
pub const PICKER_CLASS: &str = "com.acos.picker.PickerProvider";
/// Synthetic `ProviderInfo.processName`.
pub const PICKER_PROCESS: &str = "com.acos.picker";

/// Maximum entries retained per session (oldest evicted when full).
pub const MAX_PICKER_ENTRIES: usize = 64;

/// `IContentProvider` transaction codes (AOSP API 34 `IContentProvider.java`).
mod tx {
    pub const GET_TYPE: u32 = 2;
    pub const OPEN_FILE: u32 = 14;
    pub const OPEN_ASSET_FILE: u32 = 15;
    pub const GET_STREAM_TYPES: u32 = 22;
    pub const OPEN_TYPED_ASSET_FILE: u32 = 23;
    pub const CREATE_CANCELLATION_SIGNAL: u32 = 24;
    pub const GET_TYPE_ASYNC: u32 = 29;
    pub const GET_TYPE_ANONYMOUS_ASYNC: u32 = 32;
}

/// `AssetFileDescriptor.UNKNOWN_LENGTH`: the asset runs to the end of the file.
const ASSET_UNKNOWN_LENGTH: i64 = -1;

/// `DatabaseUtils.writeExceptionToParcel`'s code for `FileNotFoundException`.
const DB_EX_FILE_NOT_FOUND: i32 = 1;

/// `DocumentsContract.Document.FLAG_SUPPORTS_WRITE`.
const DOC_FLAG_SUPPORTS_WRITE: i64 = 0x2;

/// One host file exposed through `content://com.acos.picker/<id>/<name>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerEntry {
    /// 1-based monotonic id inside this session.
    pub id: u64,
    /// Full `content://com.acos.picker/<id>/<encoded_name>` URI handed to the app.
    pub uri: String,
    /// Canonical host path backing this URI.
    pub host_path: PathBuf,
    /// Sanitized display name reported in `OpenableColumns.DISPLAY_NAME`.
    pub display_name: String,
    /// MIME type reported in `getType` and `DocumentsContract.Document.COLUMN_MIME_TYPE`.
    pub mime_type: String,
    /// True only for `ACTION_CREATE_DOCUMENT` targets where guest writes are allowed.
    pub writable: bool,
    /// The backing file is a copy `acos view` staged for this entry, deleted
    /// when the entry ends. Never set for a caller's own file.
    pub staged: bool,
}

impl PickerEntry {
    /// Current byte size of the backing file on disk (0 if unreadable).
    #[must_use]
    pub fn size_bytes(&self) -> u64 {
        std::fs::metadata(&self.host_path)
            .map(|m| m.len())
            .unwrap_or(0)
    }

    /// Last-modified timestamp in epoch milliseconds, if available.
    #[must_use]
    pub fn last_modified_ms(&self) -> i64 {
        std::fs::metadata(&self.host_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .unwrap_or(0)
    }

    /// Wire JSON representation for control op replies.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "uri": self.uri,
            "display_name": self.display_name,
            "mime_type": self.mime_type,
            "size": self.size_bytes(),
            "writable": self.writable,
        })
    }
}

/// Bounded per-session registry mapping `content://com.acos.picker/...` URIs to
/// explicitly approved host files.
#[derive(Debug, Default)]
pub struct PickerRegistry {
    next_id: u64,
    entries: VecDeque<PickerEntry>,
    /// Staged files this registry owns and must delete. Ownership is per file,
    /// not per entry, so it survives evicting one of several entries serving it.
    owned: BTreeSet<PathBuf>,
}

impl PickerRegistry {
    /// Empty registry (zero allocations until a file is picked).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a host file and mint a scoped `content://com.acos.picker/...` URI.
    ///
    /// * When `writable` is `false` (default for `OPEN_DOCUMENT` / `GET_CONTENT` /
    ///   `PICK`), `host_path` must already exist as a regular file.
    /// * When `writable` is `true` (`ACTION_CREATE_DOCUMENT`), `host_path` may
    ///   either be an existing regular file or a new file inside an existing
    ///   parent directory (created empty on registration so `openFile` / `query`
    ///   can operate immediately).
    pub fn register_file(
        &mut self,
        host_path: &Path,
        display_name: Option<&str>,
        mime_type: Option<&str>,
        writable: bool,
    ) -> Result<PickerEntry> {
        let canonical = resolve_host_path(host_path, writable)?;
        let fallback_name = canonical
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        let name = sanitize_display_name(display_name.unwrap_or(fallback_name));
        let mime = mime_type
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| infer_mime_type(&name));

        self.next_id += 1;
        let id = self.next_id;
        let uri = format!(
            "content://{PICKER_AUTHORITY}/{id}/{}",
            percent_encode_segment(&name)
        );
        if writable {
            // The app's saved output now: never deleted by the registry.
            self.owned.remove(&canonical);
            for other in &mut self.entries {
                if other.host_path == canonical {
                    other.staged = false;
                }
            }
        }
        let entry = PickerEntry {
            id,
            uri,
            staged: self.owned.contains(&canonical),
            host_path: canonical,
            display_name: name,
            mime_type: mime,
            writable,
        };
        // Insert first: the new entry may serve the same file as an evicted one.
        self.entries.push_back(entry.clone());
        while self.entries.len() > MAX_PICKER_ENTRIES {
            if let Some(evicted) = self.entries.pop_front() {
                self.release_if_unserved(&evicted.host_path);
            }
        }
        Ok(entry)
    }

    /// Hand ownership of entry `id`'s backing file to the registry, so it is
    /// deleted once no entry serves it any more (eviction) or at
    /// [`Self::release_staged`]. Refused (`false`) for a file any entry may
    /// write, which is the app's saved output, and for any path outside the
    /// viewer's upload directories.
    pub fn mark_staged(&mut self, id: u64) -> bool {
        let Some(path) = self
            .entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.host_path.clone())
        else {
            return false;
        };
        if !is_staged_upload_path(&path)
            || self.entries.iter().any(|e| e.writable && e.host_path == path)
        {
            return false;
        }
        for entry in &mut self.entries {
            if entry.host_path == path {
                entry.staged = true;
            }
        }
        self.owned.insert(path);
        true
    }

    /// End of session: drop every entry serving an owned file and delete it.
    pub fn release_staged(&mut self) {
        let owned = std::mem::take(&mut self.owned);
        self.entries.retain(|e| !owned.contains(&e.host_path));
        for path in &owned {
            // Best effort: a file the user already removed is not an error.
            let _ = std::fs::remove_file(path);
        }
    }

    /// Delete an owned file once no live entry serves it.
    fn release_if_unserved(&mut self, path: &Path) {
        if !self.entries.iter().any(|e| e.host_path == path) && self.owned.remove(path) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Resolve a `content://com.acos.picker/...` URI to its registered entry.
    ///
    /// Rejects non-picker authorities, traversal segments (`.`, `..`, `\`,
    /// percent-encoded `%2e`/`%2f`/`%5c`), and unregistered ids.
    pub fn resolve_uri(&self, uri: &str) -> Result<&PickerEntry> {
        let id = parse_picker_uri_id(uri)?;
        self.entries
            .iter()
            .find(|e| e.id == id)
            .ok_or_else(|| ServiceError::BadRequest {
                interface: CONTENT_PROVIDER_DESCRIPTOR.into(),
                method: "picker".into(),
                detail: format!("picker URI {uri:?} is not registered in this session"),
            })
    }

    /// Open a registered entry's host file with Android `ParcelFileDescriptor`
    /// mode semantics (`"r"`, `"w"`, `"wt"`, `"wa"`, `"rw"`, `"rwt"`).
    pub fn open_file(&self, uri: &str, mode: &str) -> Result<(ParcelFd, u64)> {
        let entry = self.resolve_uri(uri)?;
        let file = open_with_android_mode(&entry.host_path, mode, entry.writable, uri)?;
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        let owned = OwnedFd::from(file);
        Ok((ParcelFd::from_owned(owned), len))
    }

    /// Answer `ContentResolver.query(uri, projection, ...)` for a registered
    /// picker entry.
    pub fn query(&self, uri: &str, projection: Option<&[String]>) -> Result<CursorResult> {
        let entry = self.resolve_uri(uri)?;
        let columns: Vec<String> = match projection {
            Some(cols) if !cols.is_empty() => cols.to_vec(),
            _ => vec![
                "_display_name".to_string(),
                "_size".to_string(),
                "mime_type".to_string(),
                "document_id".to_string(),
            ],
        };
        let size = i64::try_from(entry.size_bytes()).unwrap_or(i64::MAX);
        let mtime = entry.last_modified_ms();
        let flags = if entry.writable {
            DOC_FLAG_SUPPORTS_WRITE
        } else {
            0
        };
        let row: Vec<Field> = columns
            .iter()
            .map(|col| match col.as_str() {
                "_display_name" => Field::Text(entry.display_name.clone()),
                "_size" => Field::Long(size),
                "mime_type" => Field::Text(entry.mime_type.clone()),
                "document_id" => Field::Text(entry.id.to_string()),
                "_id" => Field::Long(i64::try_from(entry.id).unwrap_or(i64::MAX)),
                "flags" => Field::Long(flags),
                "last_modified" => Field::Long(mtime),
                _ => Field::Null,
            })
            .collect();
        Ok(CursorResult {
            name: "acos-picker".to_string(),
            start_pos: 0,
            columns,
            rows: vec![row],
            wants_all_on_move_calls: false,
        })
    }

    /// Answer `getType(uri)`.
    pub fn get_type(&self, uri: &str) -> Result<String> {
        Ok(self.resolve_uri(uri)?.mime_type.clone())
    }

    /// Answer `getStreamTypes(uri, mimeTypeFilter)`.
    pub fn get_stream_types(&self, uri: &str, mime_filter: &str) -> Result<Vec<String>> {
        let mime = &self.resolve_uri(uri)?.mime_type;
        if mime_filter_matches(mime_filter, mime) {
            Ok(vec![mime.clone()])
        } else {
            Ok(Vec::new())
        }
    }

    /// Number of currently registered entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no entries are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every currently registered entry, oldest first.
    #[must_use]
    pub fn list(&self) -> Vec<&PickerEntry> {
        self.entries.iter().collect()
    }
}

/// In-process `IContentProvider` serving `content://com.acos.picker/...`.
pub struct PickerProviderService {
    registry: Arc<Mutex<PickerRegistry>>,
    raw_deliveries: Vec<PendingDelivery>,
}

impl PickerProviderService {
    /// Construct a provider backed by `registry`.
    #[must_use]
    pub fn new(registry: Arc<Mutex<PickerRegistry>>) -> Self {
        Self {
            registry,
            raw_deliveries: Vec::new(),
        }
    }

    fn write_pfd(reply: &mut Parcel, fd: &ParcelFd) -> Result<()> {
        reply.write_int32(0); // mCommFd == null
        reply.write_file_descriptor(fd.as_fd())?;
        Ok(())
    }

    /// Every picker asset is a whole file. A declared length would make
    /// `ContentResolver.openFileDescriptor` throw "Not a whole file" and would
    /// bound `openOutputStream` to the size at open time (0 for a new target).
    fn write_afd(reply: &mut Parcel, fd: &ParcelFd) -> Result<()> {
        Self::write_pfd(reply, fd)?;
        reply.write_int64(0); // mStartOffset
        reply.write_int64(ASSET_UNKNOWN_LENGTH); // mLength
        reply.write_int32(0); // mExtras == null
        Ok(())
    }

    fn queue_get_type_async(&mut self, callback: acos_parcel::BinderId, mime: Option<&str>) {
        let mut bundle_parcel = Parcel::new();
        if bundle_parcel
            .write_interface_token(crate::a11y_bind::REMOTE_CALLBACK)
            .is_err()
        {
            return;
        }
        // `IRemoteCallback$Stub.onTransact` reads `Parcel.readTypedObject(Bundle.CREATOR)`
        // (int presence marker + Bundle payload).
        bundle_parcel.write_int32(1);
        let mut bundle = crate::value::Bundle::new();
        if let Some(mime) = mime {
            bundle
                .entries
                .push(("result".to_string(), Value::Str(Some(mime.to_string()))));
        }
        if parcelable::write_bundle(&mut bundle_parcel, Some(&bundle)).is_err() {
            return;
        }
        self.raw_deliveries.push(PendingDelivery {
            target: callback,
            interface: crate::a11y_bind::REMOTE_CALLBACK.to_string(),
            code: 1,
            method: "sendResult".to_string(),
            data: bundle_parcel,
        });
    }
}

impl ServiceObject for PickerProviderService {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        _args: &[Value],
    ) -> Result<CallResult> {
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: method.name.clone(),
        })
    }

    fn content_query(
        &mut self,
        interface: &str,
        data: &mut Parcel,
    ) -> Result<Option<CursorResult>> {
        if interface != CONTENT_PROVIDER_DESCRIPTOR {
            return Ok(None);
        }
        let req = QueryRequest::read(data)?;
        let registry = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cursor = registry.query(&req.uri, req.projection.as_deref())?;
        Ok(Some(cursor))
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
    ) -> Result<bool> {
        if interface != CONTENT_PROVIDER_DESCRIPTOR {
            return Ok(false);
        }
        match code {
            tx::GET_TYPE => {
                let _attr = parcelable::read_payload("android.content.AttributionSource", data)?;
                let uri = read_uri(data, "getType")?;
                let mime = self
                    .registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_type(&uri)?;
                reply.reset();
                write_no_exception(reply);
                reply.write_string16(&mime)?;
                Ok(true)
            }
            tx::GET_TYPE_ASYNC => {
                let _attr = parcelable::read_payload("android.content.AttributionSource", data)?;
                let uri = read_uri(data, "getTypeAsync")?;
                let callback = data.read_strong_binder()?;
                let mime = self
                    .registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_type(&uri)
                    .ok();
                if let Some(cb) = callback {
                    self.queue_get_type_async(cb, mime.as_deref());
                }
                Ok(true)
            }
            tx::GET_TYPE_ANONYMOUS_ASYNC => {
                let uri = read_uri(data, "getTypeAnonymousAsync")?;
                let callback = data.read_strong_binder()?;
                let mime = self
                    .registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_type(&uri)
                    .ok();
                if let Some(cb) = callback {
                    self.queue_get_type_async(cb, mime.as_deref());
                }
                Ok(true)
            }
            tx::OPEN_FILE => {
                let _attr = parcelable::read_payload("android.content.AttributionSource", data)?;
                let uri = read_uri(data, "openFile")?;
                let mode = data.read_string16()?.unwrap_or_else(|| "r".to_string());
                let _signal = data.read_strong_binder()?;
                let (fd, _len) = self
                    .registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .open_file(&uri, &mode)?;
                reply.reset();
                write_no_exception(reply);
                reply.write_int32(1); // ParcelFileDescriptor != null
                Self::write_pfd(reply, &fd)?;
                Ok(true)
            }
            tx::OPEN_ASSET_FILE => {
                let _attr = parcelable::read_payload("android.content.AttributionSource", data)?;
                let uri = read_uri(data, "openAssetFile")?;
                let mode = data.read_string16()?.unwrap_or_else(|| "r".to_string());
                let _signal = data.read_strong_binder()?;
                let (fd, _len) = self
                    .registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .open_file(&uri, &mode)?;
                reply.reset();
                write_no_exception(reply);
                reply.write_int32(1); // AssetFileDescriptor != null
                Self::write_afd(reply, &fd)?;
                Ok(true)
            }
            tx::GET_STREAM_TYPES => {
                let _attr = parcelable::read_payload("android.content.AttributionSource", data)?;
                let uri = read_uri(data, "getStreamTypes")?;
                let filter = data.read_string16()?.unwrap_or_else(|| "*/*".to_string());
                let types = self
                    .registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_stream_types(&uri, &filter)?;
                reply.reset();
                write_no_exception(reply);
                reply.write_int32(i32::try_from(types.len()).unwrap_or(0));
                for item in &types {
                    reply.write_string16(item)?;
                }
                Ok(true)
            }
            tx::OPEN_TYPED_ASSET_FILE => {
                let _attr = parcelable::read_payload("android.content.AttributionSource", data)?;
                let uri = read_uri(data, "openTypedAssetFile")?;
                let filter = data.read_string16()?.unwrap_or_else(|| "*/*".to_string());
                let _opts = parcelable::read_bundle(data)?;
                let _signal = data.read_strong_binder()?;
                let registry = self
                    .registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if registry.get_stream_types(&uri, &filter)?.is_empty() {
                    // AOSP `ContentProvider.openTypedAssetFile` throws
                    // `FileNotFoundException`; the proxy reads it in this form.
                    reply.reset();
                    reply.write_int32(DB_EX_FILE_NOT_FOUND);
                    reply.write_string16(&format!("Can't open {uri} as type {filter}"))?;
                    return Ok(true);
                }
                let (fd, _len) = registry.open_file(&uri, "r")?;
                drop(registry);
                reply.reset();
                write_no_exception(reply);
                reply.write_int32(1); // AssetFileDescriptor != null
                Self::write_afd(reply, &fd)?;
                Ok(true)
            }
            tx::CREATE_CANCELLATION_SIGNAL => {
                // A null `ICancellationSignal`: the client's `setRemote(null)`
                // is a no-op and the query/open that follows runs to completion
                // (every picker operation answers immediately).
                reply.reset();
                write_no_exception(reply);
                reply.write_strong_binder_opt(None);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn drain_raw_deliveries(&mut self) -> Vec<PendingDelivery> {
        std::mem::take(&mut self.raw_deliveries)
    }
}

fn read_uri(parcel: &mut Parcel, method: &str) -> Result<String> {
    let uri = parcelable::read_payload("android.net.Uri", parcel)?;
    uri_string(&uri).ok_or_else(|| ServiceError::BadRequest {
        interface: CONTENT_PROVIDER_DESCRIPTOR.into(),
        method: method.into(),
        detail: "could not decode the Uri argument".into(),
    })
}

fn resolve_host_path(host_path: &Path, writable: bool) -> Result<PathBuf> {
    if host_path.as_os_str().is_empty() {
        return Err(ServiceError::BadRequest {
            interface: CONTENT_PROVIDER_DESCRIPTOR.into(),
            method: "pick_file".into(),
            detail: "host file path cannot be empty".into(),
        });
    }
    if host_path.exists() {
        let meta = std::fs::metadata(host_path).map_err(|e| ServiceError::BadRequest {
            interface: CONTENT_PROVIDER_DESCRIPTOR.into(),
            method: "pick_file".into(),
            detail: format!("cannot stat {}: {e}", host_path.display()),
        })?;
        if !meta.is_file() {
            return Err(ServiceError::BadRequest {
                interface: CONTENT_PROVIDER_DESCRIPTOR.into(),
                method: "pick_file".into(),
                detail: format!("{} is not a regular file", host_path.display()),
            });
        }
        return std::fs::canonicalize(host_path).map_err(|e| ServiceError::BadRequest {
            interface: CONTENT_PROVIDER_DESCRIPTOR.into(),
            method: "pick_file".into(),
            detail: format!("cannot canonicalize {}: {e}", host_path.display()),
        });
    }
    if !writable {
        return Err(ServiceError::BadRequest {
            interface: CONTENT_PROVIDER_DESCRIPTOR.into(),
            method: "pick_file".into(),
            detail: format!("host file {} does not exist", host_path.display()),
        });
    }
    let parent = host_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let canonical_parent = std::fs::canonicalize(parent).map_err(|e| ServiceError::BadRequest {
        interface: CONTENT_PROVIDER_DESCRIPTOR.into(),
        method: "pick_file".into(),
        detail: format!("parent directory {} does not exist: {e}", parent.display()),
    })?;
    let file_name = host_path
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && *n != "." && *n != "..")
        .ok_or_else(|| ServiceError::BadRequest {
            interface: CONTENT_PROVIDER_DESCRIPTOR.into(),
            method: "pick_file".into(),
            detail: format!("invalid destination filename in {}", host_path.display()),
        })?;
    let target = canonical_parent.join(file_name);
    // Create the empty destination file immediately so `openFile`/`query` see a
    // real file and writeability is verified up front.
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&target)
        .map_err(|e| ServiceError::BadRequest {
            interface: CONTENT_PROVIDER_DESCRIPTOR.into(),
            method: "pick_file".into(),
            detail: format!("cannot create writable target {}: {e}", target.display()),
        })?;
    Ok(target)
}

/// Whether `path` has the shape `acos view` stages uploads under:
/// `<artifacts>/picker/pick-*` or `<registry>/picker-uploads/<session>/pick-*`.
fn is_staged_upload_path(path: &Path) -> bool {
    let parent = path.parent();
    let name_is = |dir: Option<&Path>, name: &str| {
        dir.and_then(Path::file_name).is_some_and(|n| n == name)
    };
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("pick-"))
        && (name_is(parent, "picker")
            || name_is(parent.and_then(Path::parent), "picker-uploads"))
}

fn open_with_android_mode(
    path: &Path,
    mode: &str,
    writable: bool,
    uri: &str,
) -> Result<File> {
    let mut opts = OpenOptions::new();
    let needs_write = match mode {
        "r" => {
            opts.read(true);
            false
        }
        "w" | "wt" => {
            opts.write(true).create(true).truncate(true);
            true
        }
        "wa" => {
            opts.write(true).create(true).append(true);
            true
        }
        "rw" => {
            opts.read(true).write(true).create(true);
            true
        }
        "rwt" => {
            opts.read(true).write(true).create(true).truncate(true);
            true
        }
        other => {
            return Err(ServiceError::BadRequest {
                interface: CONTENT_PROVIDER_DESCRIPTOR.into(),
                method: "openFile".into(),
                detail: format!("unsupported openFile mode {other:?}"),
            });
        }
    };
    if needs_write && !writable {
        return Err(ServiceError::BadRequest {
            interface: CONTENT_PROVIDER_DESCRIPTOR.into(),
            method: "openFile".into(),
            detail: format!("picker URI {uri:?} is read-only (mode {mode:?} denied)"),
        });
    }
    opts.open(path).map_err(|e| ServiceError::BadRequest {
        interface: CONTENT_PROVIDER_DESCRIPTOR.into(),
        method: "openFile".into(),
        detail: format!("cannot open backing file for {uri:?}: {e}"),
    })
}

fn parse_picker_uri_id(uri: &str) -> Result<u64> {
    let prefix = format!("content://{PICKER_AUTHORITY}/");
    let Some(rest) = uri.strip_prefix(&prefix) else {
        return Err(ServiceError::BadRequest {
            interface: CONTENT_PROVIDER_DESCRIPTOR.into(),
            method: "picker".into(),
            detail: format!("URI {uri:?} does not belong to authority {PICKER_AUTHORITY}"),
        });
    };
    let path_part = rest.split(['?', '#']).next().unwrap_or(rest);
    if path_part.contains('\\') || path_part.contains('\0') {
        return Err(ServiceError::BadRequest {
            interface: CONTENT_PROVIDER_DESCRIPTOR.into(),
            method: "picker".into(),
            detail: format!("picker URI {uri:?} contains forbidden path characters"),
        });
    }
    let lower = path_part.to_ascii_lowercase();
    if lower.contains("%2e") || lower.contains("%2f") || lower.contains("%5c") {
        return Err(ServiceError::BadRequest {
            interface: CONTENT_PROVIDER_DESCRIPTOR.into(),
            method: "picker".into(),
            detail: format!("picker URI {uri:?} contains forbidden encoded path traversal"),
        });
    }
    let mut segments = path_part.split('/');
    let Some(first) = segments.next() else {
        return Err(ServiceError::BadRequest {
            interface: CONTENT_PROVIDER_DESCRIPTOR.into(),
            method: "picker".into(),
            detail: format!("picker URI {uri:?} is missing an entry id"),
        });
    };
    for seg in std::iter::once(first).chain(segments) {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(ServiceError::BadRequest {
                interface: CONTENT_PROVIDER_DESCRIPTOR.into(),
                method: "picker".into(),
                detail: format!("picker URI {uri:?} contains invalid path segment {seg:?}"),
            });
        }
    }
    first.parse::<u64>().map_err(|_| ServiceError::BadRequest {
        interface: CONTENT_PROVIDER_DESCRIPTOR.into(),
        method: "picker".into(),
        detail: format!("picker URI {uri:?} has non-numeric entry id {first:?}"),
    })
}

/// Sanitize a display filename so it can never contain path separators,
/// traversal markers, or control characters.
#[must_use]
pub fn sanitize_display_name(raw: &str) -> String {
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(raw)
        .trim();
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && *c != '/' && *c != '\\' && *c != '\0')
        .take(128)
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "file".to_string()
    } else {
        cleaned
    }
}

fn percent_encode_segment(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Best-effort MIME type inference from filename extension.
#[must_use]
pub fn infer_mime_type(filename: &str) -> String {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("svg") => "image/svg+xml",
        Some("heic" | "heif") => "image/heic",
        Some("pdf") => "application/pdf",
        Some("txt" | "log") => "text/plain",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("html" | "htm") => "text/html",
        Some("md") => "text/markdown",
        Some("zip") => "application/zip",
        Some("mp4" | "m4v") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("ogg") => "audio/ogg",
        Some("apk") => "application/vnd.android.package-archive",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn mime_filter_matches(filter: &str, mime: &str) -> bool {
    let f = filter.trim();
    if f.is_empty() || f == "*" || f == "*/*" {
        return true;
    }
    if let Some((f_top, f_sub)) = f.split_once('/') {
        if let Some((m_top, m_sub)) = mime.split_once('/') {
            let top_ok = f_top == "*" || f_top.eq_ignore_ascii_case(m_top);
            let sub_ok = f_sub == "*" || f_sub.eq_ignore_ascii_case(m_sub);
            return top_ok && sub_ok;
        }
    }
    f.eq_ignore_ascii_case(mime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn temp_file(name: &str, contents: &[u8]) -> (PathBuf, PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(1);
        let dir = std::env::temp_dir().join(format!(
            "acos-picker-test-{}-{}-{}",
            std::process::id(),
            crate::intents::now_ms(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(name);
        std::fs::write(&path, contents).expect("write temp file");
        (dir, path)
    }

    #[test]
    fn register_open_query_and_get_type_work_end_to_end() {
        let (dir, path) = temp_file("sample photo.png", b"\x89PNG\r\n\x1a\npayload");
        let mut reg = PickerRegistry::new();
        let entry = reg
            .register_file(&path, None, None, false)
            .expect("register");
        assert!(entry.uri.starts_with("content://com.acos.picker/1/"));
        assert_eq!(entry.display_name, "sample photo.png");
        assert_eq!(entry.mime_type, "image/png");

        // Open read-only and read exact bytes back from the ParcelFd.
        let (fd, len) = reg.open_file(&entry.uri, "r").expect("open_file r");
        assert_eq!(len, 15);
        let cloned = fd.as_fd().try_clone_to_owned().expect("clone fd");
        let mut f = File::from(cloned);
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).expect("read fd");
        assert_eq!(buf, b"\x89PNG\r\n\x1a\npayload");

        // Write mode on a read-only entry must be refused.
        assert!(reg.open_file(&entry.uri, "w").is_err());

        // Query returns _display_name and _size.
        let cursor = reg
            .query(
                &entry.uri,
                Some(&["_display_name".into(), "_size".into(), "mime_type".into()]),
            )
            .expect("query");
        assert_eq!(cursor.rows.len(), 1);
        assert_eq!(cursor.rows[0][0], Field::Text("sample photo.png".into()));
        assert_eq!(cursor.rows[0][1], Field::Long(15));
        assert_eq!(cursor.rows[0][2], Field::Text("image/png".into()));

        assert_eq!(reg.get_type(&entry.uri).expect("get_type"), "image/png");
        assert_eq!(
            reg.get_stream_types(&entry.uri, "image/*")
                .expect("stream types"),
            vec!["image/png".to_string()]
        );
        assert!(reg
            .get_stream_types(&entry.uri, "text/*")
            .expect("stream types mismatch")
            .is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_path_traversal_and_unregistered_uris() {
        let (dir, path) = temp_file("ok.txt", b"hello");
        let mut reg = PickerRegistry::new();
        let _entry = reg.register_file(&path, None, None, false).expect("register");

        for bad in [
            "content://com.acos.picker/1/../../etc/passwd",
            "content://com.acos.picker/1/%2e%2e/etc/passwd",
            "content://com.acos.picker/1/%2Fetc%2Fpasswd",
            "content://com.acos.picker/../1/ok.txt",
            "content://com.acos.picker/1/..\\secret",
            "content://com.acos.picker/999/ok.txt",
            "content://other.authority/1/ok.txt",
            "file:///etc/passwd",
        ] {
            assert!(
                reg.resolve_uri(bad).is_err(),
                "must reject dangerous/unregistered URI {bad}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writable_create_document_entry_supports_write_and_updates_size() {
        let dir = std::env::temp_dir().join(format!(
            "acos-picker-create-{}-{}",
            std::process::id(),
            crate::intents::now_ms()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let target = dir.join("export.json");

        let mut reg = PickerRegistry::new();
        let entry = reg
            .register_file(&target, Some("export.json"), Some("application/json"), true)
            .expect("register writable");
        assert!(entry.writable);

        let (fd, _) = reg.open_file(&entry.uri, "wt").expect("open wt");
        {
            use std::io::Write;
            let cloned = fd.as_fd().try_clone_to_owned().expect("clone");
            let mut f = File::from(cloned);
            f.write_all(br#"{"saved":true}"#).expect("write");
        }
        assert_eq!(
            std::fs::read_to_string(&target).expect("read back"),
            r#"{"saved":true}"#
        );
        let cursor = reg
            .query(&entry.uri, Some(&["_size".into()]))
            .expect("query after write");
        assert_eq!(cursor.rows[0][0], Field::Long(14));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn staged_uploads_are_deleted_with_their_entry_and_other_files_are_not() {
        let (dir, own) = temp_file("pick-own.txt", b"mine");
        let uploads = dir.join("picker");
        std::fs::create_dir_all(&uploads).expect("mkdir");
        let upload = |name: &str| {
            let path = uploads.join(name);
            std::fs::write(&path, b"copy").expect("stage");
            path
        };
        let mut reg = PickerRegistry::new();

        // A caller's own file and a writable target are never owned.
        let mine = reg.register_file(&own, None, None, false).expect("own");
        assert!(!reg.mark_staged(mine.id));
        let saved_path = upload("pick-saved");
        let saved = reg
            .register_file(&saved_path, None, None, true)
            .expect("writable");
        assert!(!reg.mark_staged(saved.id));

        let evicted_path = upload("pick-evicted");
        let evicted = reg
            .register_file(&evicted_path, None, None, false)
            .expect("staged");
        assert!(reg.mark_staged(evicted.id));
        for _ in 0..MAX_PICKER_ENTRIES {
            reg.register_file(&own, None, None, false).expect("fill");
        }
        assert!(!evicted_path.exists(), "eviction deletes the staged copy");
        assert!(own.exists() && saved_path.exists());

        let ending_path = upload("pick-ending");
        let ending = reg
            .register_file(&ending_path, None, None, false)
            .expect("staged");
        assert!(reg.mark_staged(ending.id));
        reg.release_staged();
        assert!(!ending_path.exists(), "session end deletes the staged copy");
        assert!(reg.resolve_uri(&ending.uri).is_err());
        assert!(own.exists() && saved_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_replacement_for_the_evicted_oldest_entry_keeps_its_file() {
        let (dir, own) = temp_file("pick-own.txt", b"mine");
        let uploads = dir.join("picker");
        std::fs::create_dir_all(&uploads).expect("mkdir");
        let path = uploads.join("pick-again");
        std::fs::write(&path, b"copy").expect("stage");
        let mut reg = PickerRegistry::new();
        let oldest = reg.register_file(&path, None, None, false).expect("oldest");
        assert!(reg.mark_staged(oldest.id));
        for _ in 1..MAX_PICKER_ENTRIES {
            reg.register_file(&own, None, None, false).expect("fill");
        }
        let again = reg.register_file(&path, None, None, false).expect("again");
        assert!(reg.resolve_uri(&oldest.uri).is_err(), "oldest was evicted");
        assert!(reg.open_file(&again.uri, "r").is_ok(), "the new URI is readable");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_alias_keeps_a_staged_file_owned_until_it_goes_too() {
        let (dir, own) = temp_file("pick-own.txt", b"mine");
        let uploads = dir.join("picker");
        std::fs::create_dir_all(&uploads).expect("mkdir");
        let stage = |reg: &mut PickerRegistry, name: &str| {
            let path = uploads.join(name);
            std::fs::write(&path, b"copy").expect("stage");
            let entry = reg.register_file(&path, None, None, false).expect("staged");
            assert!(reg.mark_staged(entry.id));
            // The alias is not marked staged by its caller.
            let alias = reg.register_file(&path, None, None, false).expect("alias");
            (path, entry, alias)
        };

        // Session end releases a file whose first entry was already evicted.
        let mut reg = PickerRegistry::new();
        let (path, first, alias) = stage(&mut reg, "pick-ending");
        for _ in 0..MAX_PICKER_ENTRIES - 1 {
            reg.register_file(&own, None, None, false).expect("fill");
        }
        assert!(reg.resolve_uri(&first.uri).is_err());
        assert!(reg.resolve_uri(&alias.uri).is_ok() && path.exists());
        reg.release_staged();
        assert!(!path.exists(), "session end deletes the staged copy");
        assert!(reg.resolve_uri(&alias.uri).is_err());

        // So does evicting the last entry that serves it.
        let mut reg = PickerRegistry::new();
        let (path, _, alias) = stage(&mut reg, "pick-evicted");
        for _ in 0..MAX_PICKER_ENTRIES {
            reg.register_file(&own, None, None, false).expect("fill");
        }
        assert!(reg.resolve_uri(&alias.uri).is_err());
        assert!(!path.exists(), "the last eviction deletes the staged copy");

        // A writable alias turns the file into the app's output: kept.
        let mut reg = PickerRegistry::new();
        let (path, _, _) = stage(&mut reg, "pick-written");
        reg.register_file(&path, None, None, true).expect("writable");
        reg.release_staged();
        assert!(path.exists() && own.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn registry_evicts_oldest_entries_at_capacity() {
        let (dir, path) = temp_file("evict.txt", b"x");
        let mut reg = PickerRegistry::new();
        let first = reg.register_file(&path, None, None, false).expect("first");
        for _ in 0..MAX_PICKER_ENTRIES {
            reg.register_file(&path, None, None, false).expect("fill");
        }
        assert_eq!(reg.len(), MAX_PICKER_ENTRIES);
        assert!(
            reg.resolve_uri(&first.uri).is_err(),
            "oldest entry must be evicted once MAX_PICKER_ENTRIES is exceeded"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
