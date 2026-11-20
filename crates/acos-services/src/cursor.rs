//! `Cursor` over Binder: `BulkCursorDescriptor` + `CursorWindow`.
//!
//! The framework's `IContentProvider.query` reply is **not** a plain AIDL
//! return value: `ContentProviderNative.onTransact` writes a
//! `BulkCursorDescriptor` by hand, whose `window` field is a `CursorWindow`
//! (a frame of rows in a shared buffer). This module produces those bytes
//! exactly as the API-34 framework reads them, and mirrors the readers so the
//! layout is verified byte-for-byte in tests instead of by inspection.
//!
//! Sources (AOSP `android-14.0.0_r75`, `.cache/aosp/…`):
//!
//! * `base/core/java/android/content/ContentProviderNative.java`,
//!   `QUERY_TRANSACTION`: `writeNoException(); writeInt(1);
//!   d.writeToParcel(reply, 0)`; the request side writes the projection as
//!   `writeInt(count)` + `count × writeString`, then the `Bundle`, the
//!   `IContentObserver` binder and the `ICancellationSignal` binder.
//! * `base/core/java/android/database/BulkCursorDescriptor.java`,
//!   `writeToParcel`: `writeStrongBinder(cursor.asBinder())`,
//!   `writeStringArray(columnNames)`, `writeInt(wantsAllOnMoveCalls)`,
//!   `writeInt(count)`, `writeInt(window != null)` + `window.writeToParcel`.
//! * `base/core/java/android/database/CursorWindow.java`, `writeToParcel`:
//!   `writeInt(mStartPos)` + `nativeWriteToParcel(mWindowPtr)`.
//! * `base/libs/androidfw/CursorWindow.cpp`, `writeToParcel` /
//!   `createFromParcel`: the native frame.
//! * `base/core/java/android/database/BulkCursorNative.java`,
//!   `IBulkCursor.java`: the `IBulkCursor` transactions this module answers
//!   (`android.content.IBulkCursor`, codes 1..=7).
//!
//! ## CursorWindow wire form
//!
//! `CursorWindow` keeps a heap of allocations (strings/blobs, 4-byte aligned)
//! growing up from offset 0 and a stack of 16-byte `FieldSlot`s growing down
//! from the end. Below `kInlineSize` (16384 bytes) `writeToParcel` **compacts**
//! the frame on the wire — no fd, no ashmem, just bytes:
//!
//! ```text
//! writeString8(name)
//! writeUint32(numRows)
//! writeUint32(numColumns)
//! writeUint32(compactedSize)      // = heap bytes + 16 × numRows × numColumns
//! writeBool(false)                // not an ashmem fd (writeInt(0))
//! compactedSize bytes             // heap …, then every slot
//! ```
//!
//! where slot `(row, column)` sits at
//! `compactedSize - 16 - (row * numColumns + column) * 16` and is a packed
//! `{ int32 type; union { double; int64; {uint32 offset, uint32 size}; } }`
//! (12 bytes + 4 bytes of slack inside the 16-byte slot).
//!
//! **No ashmem/memfd is needed for the ACOS provider set**: the framework's own
//! default window is the inline form, which is what every Settings result set
//! fits. A frame larger than 16384 bytes is a structured error
//! ([`ServiceError::UnsupportedAndroidApi`]) rather than a truncated window; if
//! a future provider needs it, the fd is `memfd_create(2)` (the framework only
//! `mmap`s the received descriptor `PROT_READ`) — see
//! `acos_parcel::ParcelFd::memfd`.

use acos_aidl::MethodInfo;
use acos_parcel::{BinderId, Parcel};

use crate::error::{Result, ServiceError};
use crate::exception::{write_no_exception, NO_EXCEPTION};
use crate::host::{CallResult, ServiceObject};
use crate::parcelable;

/// AIDL descriptor of the cursor object handed back in the descriptor.
///
/// `IBulkCursor.java` declares `descriptor = "android.content.IBulkCursor"`
/// (the interface lives in `android.database`, the token does not).
pub const BULK_CURSOR_DESCRIPTOR: &str = "android.content.IBulkCursor";

/// `CursorWindow::kInlineSize`: the largest frame the inline wire form carries.
pub const K_INLINE_SIZE: usize = 16384;

const SLOT_SIZE: usize = 16;

const FIELD_TYPE_NULL: i32 = 0;
const FIELD_TYPE_INTEGER: i32 = 1;
const FIELD_TYPE_FLOAT: i32 = 2;
const FIELD_TYPE_STRING: i32 = 3;
const FIELD_TYPE_BLOB: i32 = 4;

/// One cell of a provider result set.
#[derive(Debug, Clone, PartialEq)]
pub enum Field {
    /// SQL `NULL`.
    Null,
    /// An integer column.
    Long(i64),
    /// A floating-point column.
    Double(f64),
    /// A text column (stored UTF-8 with a trailing NUL, like the framework).
    Text(String),
    /// A blob column.
    Blob(Vec<u8>),
}

impl Field {
    /// Text shorthand.
    #[must_use]
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }

    /// The text value, when this is a text column.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            _ => None,
        }
    }
}

/// A materialised result set: what a Rust-backed provider returns for `query`.
#[derive(Debug, Clone, PartialEq)]
pub struct CursorResult {
    /// `CursorWindow` name (`CursorWindow: <name>` on a device).
    pub name: String,
    /// Java-side `mStartPos` (the first row this window holds).
    pub start_pos: i32,
    /// `BulkCursorDescriptor.columnNames` / the window's columns.
    pub columns: Vec<String>,
    /// Rows, each exactly `columns.len()` cells wide.
    pub rows: Vec<Vec<Field>>,
    /// `wantsAllOnMoveCalls`. `false` means the client reads rows straight out
    /// of the window and never calls `IBulkCursor.onMove` — the framework
    /// default for `MatrixCursor` and for this provider.
    pub wants_all_on_move_calls: bool,
}

impl CursorResult {
    /// A result set with a window holding every row from position 0.
    #[must_use]
    pub fn new(columns: Vec<String>, rows: Vec<Vec<Field>>) -> Self {
        Self {
            name: "acos-settings".into(),
            start_pos: 0,
            columns,
            rows,
            wants_all_on_move_calls: false,
        }
    }

    /// `BulkCursorDescriptor.count`.
    #[must_use]
    pub fn count(&self) -> i32 {
        i32::try_from(self.rows.len()).unwrap_or(i32::MAX)
    }

    /// The window's row / column dimensions.
    #[must_use]
    pub fn dimensions(&self) -> (u32, u32) {
        (
            u32::try_from(self.rows.len()).unwrap_or(u32::MAX),
            u32::try_from(self.columns.len()).unwrap_or(u32::MAX),
        )
    }

    /// The compacted native frame (`CursorWindow::writeToParcel`).
    ///
    /// Heap allocations come first (row-major `putString`/`putBlob` order), then
    /// the 16-byte slots with slot 0 last — exactly the bytes
    /// `CursorWindow::createFromParcel` re-reads into a read-only window.
    pub fn window_bytes(&self) -> Result<Vec<u8>> {
        let num_columns = self.columns.len();
        let mut heap: Vec<u8> = Vec::new();
        let mut slots: Vec<[u8; SLOT_SIZE]> = Vec::with_capacity(self.rows.len() * num_columns);

        for (row_index, row) in self.rows.iter().enumerate() {
            if row.len() != num_columns {
                return Err(ServiceError::BadReply {
                    interface: "android.database.CursorWindow".into(),
                    method: "putRow".into(),
                    detail: format!(
                        "row {row_index} has {} cells, projection has {num_columns} columns",
                        row.len()
                    ),
                });
            }
            for field in row {
                let mut slot = [0u8; SLOT_SIZE];
                match field {
                    Field::Null => {
                        slot[0..4].copy_from_slice(&FIELD_TYPE_NULL.to_le_bytes());
                    }
                    Field::Long(value) => {
                        slot[0..4].copy_from_slice(&FIELD_TYPE_INTEGER.to_le_bytes());
                        slot[4..12].copy_from_slice(&value.to_le_bytes());
                    }
                    Field::Double(value) => {
                        slot[0..4].copy_from_slice(&FIELD_TYPE_FLOAT.to_le_bytes());
                        slot[4..12].copy_from_slice(&value.to_le_bytes());
                    }
                    Field::Text(text) => {
                        let mut bytes = text.as_bytes().to_vec();
                        bytes.push(0); // `putString` stores the trailing NUL
                        let size = bytes.len();
                        let offset = heap.len();
                        heap.extend_from_slice(&bytes);
                        while !heap.len().is_multiple_of(4) {
                            heap.push(0);
                        }
                        slot[0..4].copy_from_slice(&FIELD_TYPE_STRING.to_le_bytes());
                        slot[4..8].copy_from_slice(
                            &u32::try_from(offset).unwrap_or(u32::MAX).to_le_bytes(),
                        );
                        slot[8..12].copy_from_slice(
                            &u32::try_from(size).unwrap_or(u32::MAX).to_le_bytes(),
                        );
                    }
                    Field::Blob(bytes) => {
                        let size = bytes.len();
                        let offset = heap.len();
                        heap.extend_from_slice(bytes);
                        while !heap.len().is_multiple_of(4) {
                            heap.push(0);
                        }
                        slot[0..4].copy_from_slice(&FIELD_TYPE_BLOB.to_le_bytes());
                        slot[4..8].copy_from_slice(
                            &u32::try_from(offset).unwrap_or(u32::MAX).to_le_bytes(),
                        );
                        slot[8..12].copy_from_slice(
                            &u32::try_from(size).unwrap_or(u32::MAX).to_le_bytes(),
                        );
                    }
                }
                slots.push(slot);
            }
        }

        let compacted = heap.len() + slots.len() * SLOT_SIZE;
        if compacted > K_INLINE_SIZE {
            // A real device would have inflated the window into ashmem and
            // written the fd. ACOS has no provider result set that large, so
            // say so instead of silently truncating the cursor.
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: "android.database.CursorWindow".into(),
                code: compacted as u32,
                method: "writeToParcel(>16 KiB ashmem window)".into(),
            });
        }

        let mut out = heap;
        // Slot 0 is the *last* 16 bytes of the frame; the stack grows downward.
        for slot in slots.iter().rev() {
            out.extend_from_slice(slot);
        }
        Ok(out)
    }

    /// Write the Java half (`mStartPos`) plus the native frame.
    pub fn write_window_to(&self, parcel: &mut Parcel) -> Result<()> {
        let (num_rows, num_columns) = self.dimensions();
        let bytes = self.window_bytes()?;
        parcel.write_int32(self.start_pos);
        parcel
            .write_string8(&self.name)
            .map_err(ServiceError::Parcel)?;
        parcel.write_uint32(num_rows);
        parcel.write_uint32(num_columns);
        parcel.write_uint32(u32::try_from(bytes.len()).unwrap_or(u32::MAX));
        parcel.write_int32(0); // `writeBool(false)`: inline frame, not an fd
        debug_assert_eq!(bytes.len() % 4, 0, "compacted window is 4-byte aligned");
        for word in bytes.chunks_exact(4) {
            parcel.write_int32(i32::from_le_bytes([word[0], word[1], word[2], word[3]]));
        }
        Ok(())
    }

    /// The `ContentProviderNative` QUERY_TRANSACTION reply body, after the
    /// exception header: the `writeInt(1)` marker, the
    /// `BulkCursorDescriptor`, and the window.
    pub fn write_query_reply(&self, parcel: &mut Parcel, cursor: BinderId) -> Result<()> {
        parcel.write_int32(1); // writeTypedObject presence marker
        parcel.write_strong_binder(cursor);
        let names: Vec<&str> = self.columns.iter().map(String::as_str).collect();
        parcel
            .write_string16_array(Some(&names))
            .map_err(ServiceError::Parcel)?;
        parcel.write_int32(i32::from(self.wants_all_on_move_calls));
        parcel.write_int32(self.count());
        parcel.write_int32(1); // window != null
        self.write_window_to(parcel)
    }
}

// ---------------------------------------------------------------------------
// Reader mirror (framework layout, used by tests and by ART-side evidence)
// ---------------------------------------------------------------------------

/// One decoded `CursorWindow` field slot.
#[derive(Debug, Clone, PartialEq)]
pub enum WindowField {
    /// `FIELD_TYPE_NULL`.
    Null,
    /// `FIELD_TYPE_INTEGER`.
    Long(i64),
    /// `FIELD_TYPE_FLOAT`.
    Double(f64),
    /// `FIELD_TYPE_STRING` (UTF-8 without the trailing NUL).
    Text(String),
    /// `FIELD_TYPE_BLOB`.
    Blob(Vec<u8>),
}

/// A read-only `CursorWindow` rebuilt from its wire bytes, mirroring
/// `CursorWindow::createFromParcel`.
#[derive(Debug, Clone, PartialEq)]
pub struct CursorWindowView {
    /// `mName`.
    pub name: String,
    /// Java `mStartPos`.
    pub start_pos: i32,
    /// `mNumRows`.
    pub num_rows: u32,
    /// `mNumColumns`.
    pub num_columns: u32,
    /// `mSize` (the frame the reader allocates; here the received bytes).
    pub size: u32,
    /// The frame bytes.
    pub data: Vec<u8>,
}

impl CursorWindowView {
    /// Read `mStartPos` + the native frame.
    pub fn read_from(parcel: &mut Parcel) -> Result<Self> {
        let start_pos = parcel.read_int32()?;
        let name = parcel.read_string8()?.unwrap_or_default();
        let num_rows = parcel.read_uint32()?;
        let num_columns = parcel.read_uint32()?;
        let size = parcel.read_uint32()?;
        let slot_bytes = u64::from(num_rows) * u64::from(num_columns) * SLOT_SIZE as u64;
        if slot_bytes > u64::from(size) {
            return Err(ServiceError::BadRequest {
                interface: "android.database.CursorWindow".into(),
                method: "createFromParcel".into(),
                detail: format!("size {size} cannot hold {num_rows}×{num_columns} slots"),
            });
        }
        let is_ashmem = parcel.read_int32()? != 0;
        if is_ashmem {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: "android.database.CursorWindow".into(),
                code: 1,
                method: "createFromParcel(ashmem fd)".into(),
            });
        }
        let size_usize = usize::try_from(size).map_err(|_| ServiceError::BadRequest {
            interface: "android.database.CursorWindow".into(),
            method: "createFromParcel".into(),
            detail: "window size out of range".into(),
        })?;
        // Guest-controlled size (sandbox §5 gate 1): an inline window cannot be
        // larger than what is left in the parcel, so refuse before allocating.
        if size_usize > parcel.remaining() {
            return Err(ServiceError::BadRequest {
                interface: "android.database.CursorWindow".into(),
                method: "createFromParcel".into(),
                detail: format!(
                    "window size {size_usize} exceeds the {} bytes left in the parcel",
                    parcel.remaining()
                ),
            });
        }
        let mut data = Vec::with_capacity(size_usize);
        for _ in 0..size_usize.div_ceil(4) {
            data.extend_from_slice(&parcel.read_int32()?.to_le_bytes());
        }
        data.truncate(size_usize);
        Ok(Self {
            name,
            start_pos,
            num_rows,
            num_columns,
            size,
            data,
        })
    }

    /// `CursorWindow::getFieldSlot` + the type-specific getter.
    #[must_use]
    pub fn field(&self, row: u32, column: u32) -> Option<WindowField> {
        if row >= self.num_rows || column >= self.num_columns {
            return None;
        }
        let index = u64::from(row) * u64::from(self.num_columns) + u64::from(column);
        let size = u64::from(self.size);
        let slot = size.checked_sub(SLOT_SIZE as u64 + index * SLOT_SIZE as u64)?;
        let at = usize::try_from(slot).ok()?;
        let bytes = self.data.get(at..at + SLOT_SIZE)?;
        let kind = i32::from_le_bytes(bytes[0..4].try_into().ok()?);
        match kind {
            FIELD_TYPE_NULL => Some(WindowField::Null),
            FIELD_TYPE_INTEGER => Some(WindowField::Long(i64::from_le_bytes(
                bytes[4..12].try_into().ok()?,
            ))),
            FIELD_TYPE_FLOAT => Some(WindowField::Double(f64::from_le_bytes(
                bytes[4..12].try_into().ok()?,
            ))),
            FIELD_TYPE_STRING | FIELD_TYPE_BLOB => {
                let offset = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
                let len = u32::from_le_bytes(bytes[8..12].try_into().ok()?) as usize;
                if offset + len > self.data.len() {
                    return None;
                }
                let raw = &self.data[offset..offset + len];
                if kind == FIELD_TYPE_STRING {
                    let text = raw.strip_suffix(&[0]).unwrap_or(raw);
                    Some(WindowField::Text(
                        String::from_utf8_lossy(text).into_owned(),
                    ))
                } else {
                    Some(WindowField::Blob(raw.to_vec()))
                }
            }
            _ => None,
        }
    }

    /// `CursorWindow.getString(row, column)` for a text cell.
    #[must_use]
    pub fn string(&self, row: u32, column: u32) -> Option<String> {
        match self.field(row, column)? {
            WindowField::Text(text) => Some(text),
            _ => None,
        }
    }

    /// `CursorWindow.getLong(row, column)`.
    #[must_use]
    pub fn long(&self, row: u32, column: u32) -> Option<i64> {
        match self.field(row, column)? {
            WindowField::Long(value) => Some(value),
            _ => None,
        }
    }
}

/// A decoded `BulkCursorDescriptor` (the `Cursor` half of the reply).
#[derive(Debug, Clone, PartialEq)]
pub struct BulkCursorDescriptorView {
    /// `cursor` (`IBulkCursor` binder).
    pub cursor: Option<BinderId>,
    /// `columnNames`.
    pub column_names: Vec<String>,
    /// `wantsAllOnMoveCalls`.
    pub wants_all_on_move_calls: bool,
    /// `count`.
    pub count: i32,
    /// `window` (the frame the client reads rows out of).
    pub window: Option<CursorWindowView>,
}

/// Read a full `IContentProvider.query` reply the way
/// `ContentProviderProxy.query` does: exception header, presence marker,
/// `BulkCursorDescriptor`.
pub fn read_query_reply(parcel: &mut Parcel) -> Result<Option<BulkCursorDescriptorView>> {
    let code = parcel.read_int32()?;
    if code != NO_EXCEPTION {
        // `DatabaseUtils.readExceptionFromParcel`: message follows.
        let message = parcel.read_string16()?.unwrap_or_default();
        return Err(ServiceError::BadRequest {
            interface: "android.content.IContentProvider".into(),
            method: "query".into(),
            detail: format!("android exception {code}: {message}"),
        });
    }
    if parcel.read_int32()? == 0 {
        return Ok(None);
    }
    let cursor = parcel.read_strong_binder()?;
    let column_names = parcel
        .read_string16_array()?
        .unwrap_or_default()
        .into_iter()
        .map(|name| name.unwrap_or_default())
        .collect();
    let wants_all_on_move_calls = parcel.read_int32()? != 0;
    let count = parcel.read_int32()?;
    let has_window = parcel.read_int32()? != 0;
    let window = if has_window {
        Some(CursorWindowView::read_from(parcel)?)
    } else {
        None
    };
    Ok(Some(BulkCursorDescriptorView {
        cursor,
        column_names,
        wants_all_on_move_calls,
        count,
        window,
    }))
}

/// The request half of `IContentProvider.query`, decoded the way
/// `ContentProviderNative.onTransact` decodes it (the generated ACOS table
/// records `writeStringArray` as a single `Str16`, so the real layout is read
/// here).
#[derive(Debug, Clone, PartialEq)]
pub struct QueryRequest {
    /// `android.net.Uri` payload (`content://settings/secure`).
    pub uri: String,
    /// The requested projection (`None` for "all columns").
    pub projection: Option<Vec<String>>,
    /// `queryArgs` (`android:query-arg-sql-selection`, …).
    pub query_args: Option<crate::value::Bundle>,
    /// `IContentObserver` binder (the cursor adaptor's own observer).
    pub observer: Option<BinderId>,
    /// `ICancellationSignal` binder.
    pub cancellation_signal: Option<BinderId>,
}

impl QueryRequest {
    /// Read the arguments after the interface token.
    pub fn read(parcel: &mut Parcel) -> Result<Self> {
        let attribution = parcelable::read_payload("android.content.AttributionSource", parcel)?;
        let uri = parcelable::read_payload("android.net.Uri", parcel)?;
        let uri_text = uri_string(&uri).ok_or_else(|| ServiceError::BadRequest {
            interface: "android.content.IContentProvider".into(),
            method: "query".into(),
            detail: "could not decode the Uri argument".into(),
        })?;
        let count = parcel.read_int32()?;
        let projection = if count < 0 {
            None
        } else {
            // Guest-controlled count: each String16 takes at least 4 bytes.
            let cap = usize::try_from(count)
                .unwrap_or(0)
                .min(parcel.remaining() / 4);
            let mut columns = Vec::with_capacity(cap);
            for _ in 0..count {
                columns.push(parcel.read_string16()?.unwrap_or_default());
            }
            Some(columns)
        };
        let query_args = parcelable::read_bundle(parcel)?;
        let observer = parcel.read_strong_binder()?;
        let cancellation_signal = parcel.read_strong_binder()?;
        let _ = attribution;
        Ok(Self {
            uri: uri_text,
            projection,
            query_args,
            observer,
            cancellation_signal,
        })
    }
}

/// The `Uri` argument as text.
///
/// `Uri.writeToParcel` writes an int type tag (`StringUri = 1`, `Opaque = 2`,
/// `Hierarchical = 3`) and then `writeString8(toString())`, so the string is
/// field 1 of the decoded payload.
#[must_use]
pub fn uri_string(uri: &crate::value::Parcelable) -> Option<String> {
    let payload = uri.field(1)?;
    match payload {
        crate::value::Value::Str(Some(s)) | crate::value::Value::Str8(Some(s)) => Some(s.clone()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// IBulkCursor service
// ---------------------------------------------------------------------------

/// The local binder handed back in `BulkCursorDescriptor.cursor`.
///
/// `BulkCursorNative.onTransact` (AOSP) defines seven transactions; the client
/// (`BulkCursorToCursorAdaptor`) only needs them when it moves outside the
/// window it was given or closes the cursor. Every reply here is the framework's
/// own shape (`writeNoException` first), never a silent success.
#[derive(Debug)]
pub struct BulkCursorService {
    result: CursorResult,
    closed: bool,
}

impl BulkCursorService {
    /// Wrap a materialised result set.
    #[must_use]
    pub fn new(result: CursorResult) -> Self {
        Self {
            result,
            closed: false,
        }
    }

    /// The rows this cursor serves (tests / evidence).
    #[must_use]
    pub fn result(&self) -> &CursorResult {
        &self.result
    }
}

/// `IBulkCursor` transaction codes (`IBulkCursor.java`, API 34).
pub mod bulk_cursor {
    /// `getWindow(int position) -> CursorWindow`.
    pub const GET_CURSOR_WINDOW: u32 = 1;
    /// `deactivate()`.
    pub const DEACTIVATE: u32 = 2;
    /// `requery(IContentObserver) -> int`.
    pub const REQUERY: u32 = 3;
    /// `onMove(int position)`.
    pub const ON_MOVE: u32 = 4;
    /// `getExtras() -> Bundle`.
    pub const GET_EXTRAS: u32 = 5;
    /// `respond(Bundle) -> Bundle`.
    pub const RESPOND: u32 = 6;
    /// `close()`.
    pub const CLOSE: u32 = 7;
}

impl ServiceObject for BulkCursorService {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        _args: &[crate::value::Value],
    ) -> Result<CallResult> {
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: format!("{} is a raw interface", method.name),
        })
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
    ) -> Result<bool> {
        if interface != BULK_CURSOR_DESCRIPTOR {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code,
                method: "<not IBulkCursor>".into(),
            });
        }
        match code {
            bulk_cursor::GET_CURSOR_WINDOW => {
                let _position = data.read_int32()?;
                reply.reset();
                write_no_exception(reply);
                reply.write_int32(1);
                self.result.write_window_to(reply)?;
            }
            bulk_cursor::DEACTIVATE | bulk_cursor::ON_MOVE => {
                if code == bulk_cursor::ON_MOVE {
                    let _position = data.read_int32()?;
                }
                reply.reset();
                write_no_exception(reply);
            }
            bulk_cursor::REQUERY => {
                // `requery(IContentObserver) -> int` followed by `getExtras()`.
                let _observer = data.read_strong_binder()?;
                reply.reset();
                write_no_exception(reply);
                reply.write_int32(self.result.count());
                parcelable::write_bundle(reply, None)?;
            }
            bulk_cursor::GET_EXTRAS => {
                reply.reset();
                write_no_exception(reply);
                parcelable::write_bundle(reply, None)?;
            }
            bulk_cursor::RESPOND => {
                let _extras = parcelable::read_bundle(data)?;
                reply.reset();
                write_no_exception(reply);
                parcelable::write_bundle(reply, None)?;
            }
            bulk_cursor::CLOSE => {
                self.closed = true;
                reply.reset();
                write_no_exception(reply);
            }
            _ => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code,
                    method: format!("IBulkCursor code {code}"),
                })
            }
        }
        Ok(true)
    }

    fn finished(&self) -> bool {
        self.closed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CursorResult {
        CursorResult::new(
            vec!["_id".into(), "name".into(), "value".into()],
            vec![
                vec![
                    Field::Long(1),
                    Field::Text("auto_caps".into()),
                    Field::Text("1".into()),
                ],
                vec![
                    Field::Long(2),
                    Field::Text("show_password".into()),
                    Field::Text("0".into()),
                ],
            ],
        )
    }

    #[test]
    fn window_round_trips_through_the_framework_layout() {
        let cursor = sample();
        let mut parcel = Parcel::new();
        cursor.write_window_to(&mut parcel).expect("write window");
        parcel.set_data_position(0);
        let window = CursorWindowView::read_from(&mut parcel).expect("read window");
        assert_eq!(window.name, "acos-settings");
        assert_eq!(window.start_pos, 0);
        assert_eq!((window.num_rows, window.num_columns), (2, 3));
        assert_eq!(window.long(0, 0), Some(1));
        assert_eq!(window.string(0, 1).as_deref(), Some("auto_caps"));
        assert_eq!(window.string(1, 2).as_deref(), Some("0"));
        // The frame is exactly the compacted size (no trailing slack).
        assert_eq!(parcel.data_avail(), 0);
    }

    #[test]
    fn oversized_window_is_a_structured_error_not_a_truncation() {
        let cursor = CursorResult::new(
            vec!["value".into()],
            vec![vec![Field::Text("x".repeat(K_INLINE_SIZE + 1))]],
        );
        let err = cursor.window_bytes().expect_err("must not truncate");
        assert_eq!(err.code(), crate::codes::UNSUPPORTED_ANDROID_API);
    }
}
