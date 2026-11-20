//! The Android 14 `Parcel` binary codec.
//!
//! # Wire format (verified against Android 34 `libbinder.so`)
//!
//! * Every primitive is little-endian and lives at a 4-byte-aligned position.
//! * `String16` = `int32 len` (UTF-16 code units, **excluding** the NUL) +
//!   `len` UTF-16LE units + `0x0000` + zero padding to 4 bytes.
//! * `String8` = `int32 len` (UTF-8 bytes, excluding the NUL) + `len` bytes +
//!   `0x00` + zero padding to 4 bytes.
//! * `null` string = `int32 -1`.
//! * A binder or file descriptor is a 24-byte `FlatObject` (see
//!   [`crate::binder`]) whose offset is recorded in a parallel object table.
//!
//! The exact `writeString8`/`readString8Inplace`/`writeString16` byte layout
//! comes from disassembling
//! `apex/com.android.vndk.current/lib64/libbinder.so` (Android 34); see the
//! crate report for the addresses. In particular Android 34 does **not** use
//! the `(len<<1)|1` "squeezed" header the research note guessed — it writes a
//! plain `int32 len`.
//!
//! # Lenient vs strict reads
//!
//! ART's Java `Parcel.readInt()` returns `0` when the buffer is short instead
//! of throwing. To let the native layer reproduce that, every scalar reader has
//! a `*_lenient` twin returning the ART value on a short read. The strict
//! readers return [`ParcelError::Underflow`]. **The native layer must use the
//! lenient readers** to match ART's observable behaviour.

use std::collections::BTreeSet;
use std::os::fd::{BorrowedFd, RawFd};
use std::sync::{Mutex, MutexGuard};

use crate::binder::{BinderId, BinderKind, FlatObject, BINDER_TYPE_FD, FLAT_OBJECT_SIZE};
use crate::error::{ParcelError, Result};
use crate::fd::ParcelFd;

/// Round `n` up to a multiple of four, the Android `PAD_SIZE` rule.
///
/// Saturating: absurd lengths saturate instead of panicking, and the following
/// bounds check reports `PARCEL_UNDERFLOW`.
#[inline]
#[must_use]
pub const fn align4(n: usize) -> usize {
    n.saturating_add(3) & !3
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A growable parcel buffer with a read/write position and an object table.
pub struct Parcel {
    data: Vec<u8>,
    pos: usize,
    objects: Vec<usize>,
    fds: Vec<ParcelFd>,
    allow_fds: bool,
    marked: BTreeSet<u64>,
    /// Interface-token magic this parcel writes/expects; system by default.
    token_header: u32,
    /// True when this parcel is in **libbinder wire form**: every binder object
    /// may be followed by a 4-byte `Stability::Level` record. Objects that are
    /// in the offset table are stripped on import (the peer does that); a
    /// **null** binder is not in the table (`Parcel::writeObject(obj, false)`) and
    /// is always followed by that record, so [`Parcel::read_strong_binder`] skips
    /// it here. Without this a request containing a null binder (e.g.
    /// `ViewRootImpl`'s `LayoutParams.token`) desyncs the rest of the parcel by
    /// 4 bytes (L1 §3.6).
    wire_null_stability: bool,
}

impl std::fmt::Debug for Parcel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Parcel")
            .field("pos", &self.pos)
            .field("data_size", &self.data.len())
            .field("objects", &self.objects)
            .field(
                "fds",
                &self.fds.iter().map(ParcelFd::as_raw_fd).collect::<Vec<_>>(),
            )
            .field("allow_fds", &self.allow_fds)
            .finish()
    }
}

impl Default for Parcel {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for Parcel {
    /// Clone the bytes and object table; every owned descriptor is duplicated.
    ///
    /// The flat-object records store a descriptor **number**, so a clone must
    /// rewrite each `BINDER_TYPE_FD` id to the number of its duplicated
    /// descriptor; otherwise `appendFrom` (which resolves the id against the
    /// clone's `fds`) cannot find it. This happens on Android's read/write-helper
    /// path (`Parcel.appendFrom` for a Bundle with a helper set).
    fn clone(&self) -> Self {
        let mut data = self.data.clone();
        let objects = self.objects.clone();
        let mut fds = Vec::with_capacity(self.fds.len());
        let mut fd_iter = self.fds.iter();
        for &o in &objects {
            if o + 16 > data.len() {
                continue;
            }
            let ty = u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
            if ty != BINDER_TYPE_FD {
                continue;
            }
            let Some(src) = fd_iter.next() else { continue };
            if let Ok(dup) = src.try_clone() {
                let raw = dup.as_raw_fd() as u64;
                data[o + 8..o + 16].copy_from_slice(&raw.to_le_bytes());
                fds.push(dup);
            }
        }
        Self {
            data,
            pos: self.pos,
            objects,
            fds,
            allow_fds: self.allow_fds,
            marked: self.marked.clone(),
            token_header: self.token_header,
            wire_null_stability: self.wire_null_stability,
        }
    }
}

impl Parcel {
    /// An empty parcel. `allowFds` starts `true`, as in libbinder's constructor.
    #[must_use]
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            pos: 0,
            objects: Vec::new(),
            fds: Vec::new(),
            allow_fds: true,
            marked: BTreeSet::new(),
            token_header: crate::token::SYSTEM_INTERFACE_TOKEN_HEADER,
            wire_null_stability: false,
        }
    }

    /// An empty parcel with room for `capacity` bytes.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let mut p = Self::new();
        p.data.reserve(capacity);
        p
    }

    // ------------------------------------------------------------------
    // buffer management
    // ------------------------------------------------------------------

    /// Ensure the backing buffer is at least `end` bytes long (zero-filled).
    fn reserve(&mut self, end: usize) {
        if end > self.data.len() {
            self.data.resize(end, 0);
        }
    }

    /// Write `bytes` verbatim (length must already be a multiple of four).
    fn write_bytes(&mut self, bytes: &[u8]) {
        debug_assert_eq!(bytes.len() % 4, 0);
        let end = self.pos + bytes.len();
        self.reserve(end);
        self.data[self.pos..end].copy_from_slice(bytes);
        self.pos = end;
    }

    /// Write `bytes` followed by zero padding up to four-byte alignment.
    fn write_padded(&mut self, bytes: &[u8]) {
        let padded = align4(bytes.len());
        let end = self.pos + padded;
        self.reserve(end);
        let start = self.pos;
        self.data[start..start + bytes.len()].copy_from_slice(bytes);
        // ACOS zeroes padding; Android leaves it unspecified. Readers skip it,
        // so this only makes ACOS output deterministic.
        self.data[start + bytes.len()..end].fill(0);
        self.pos = end;
    }

    fn check(&self, needed: usize) -> Result<()> {
        match self.pos.checked_add(needed) {
            Some(end) if end <= self.data.len() => Ok(()),
            _ => Err(ParcelError::Underflow {
                at: self.pos,
                needed,
                available: self.data.len().saturating_sub(self.pos),
            }),
        }
    }

    // ------------------------------------------------------------------
    // scalar writers
    // ------------------------------------------------------------------

    /// Write a 4-byte signed integer.
    pub fn write_int32(&mut self, v: i32) {
        self.write_bytes(&v.to_le_bytes());
    }

    /// Write a 4-byte unsigned integer.
    pub fn write_uint32(&mut self, v: u32) {
        self.write_bytes(&v.to_le_bytes());
    }

    /// Write an 8-byte signed integer.
    pub fn write_int64(&mut self, v: i64) {
        self.write_bytes(&v.to_le_bytes());
    }

    /// Write an 8-byte unsigned integer.
    pub fn write_uint64(&mut self, v: u64) {
        self.write_bytes(&v.to_le_bytes());
    }

    /// Write a 4-byte IEEE-754 float.
    pub fn write_float(&mut self, v: f32) {
        self.write_bytes(&v.to_le_bytes());
    }

    /// Write an 8-byte IEEE-754 double.
    pub fn write_double(&mut self, v: f64) {
        self.write_bytes(&v.to_le_bytes());
    }

    /// Write a boolean as an `int32` (`0`/`1`).
    pub fn write_bool(&mut self, v: bool) {
        self.write_int32(i32::from(v));
    }

    /// Write a `byte` sign-extended into an `int32`, as libbinder does.
    pub fn write_byte(&mut self, v: i8) {
        self.write_int32(i32::from(v));
    }

    /// Write a `char` (UTF-16 code unit) as an `int32`.
    pub fn write_char(&mut self, v: u16) {
        self.write_int32(i32::from(v));
    }

    /// Append `bytes` verbatim, **without** a length word and without padding.
    ///
    /// This is what a native `Flattenable` payload needs: `Parcel::write(const
    /// Flattenable&)` is `writeInt32(flattenedSize)` followed by exactly
    /// `flattenedSize` bytes, and the native `unflatten` walks those bytes field
    /// by field — so the size must be exact (padding would desynchronise the
    /// next object in the parcel). The sensor `Sensor` flatten is the case that
    /// forced this: `libs/sensor/Sensor.cpp:534-559` mixes `String8` (4-byte
    /// aligned) fields with a one-byte `bool`, so its payload only happens to be
    /// 4-aligned.
    ///
    /// The bytes must not contain flat objects (no binder, no descriptor). It is
    /// the caller's job to write the leading size when its own wire format needs
    /// one.
    pub fn write_raw_bytes(&mut self, bytes: &[u8]) {
        let end = self.pos + bytes.len();
        self.reserve(end);
        self.data[self.pos..end].copy_from_slice(bytes);
        self.pos = end;
    }

    // ------------------------------------------------------------------
    // scalar readers
    // ------------------------------------------------------------------

    /// Strict `readInt32`; errors if fewer than four bytes remain.
    pub fn read_int32(&mut self) -> Result<i32> {
        self.check(4)?;
        let mut buf = [0u8; 4];
        buf.copy_from_slice(&self.data[self.pos..self.pos + 4]);
        self.pos += 4;
        Ok(i32::from_le_bytes(buf))
    }

    /// `readInt32` with ART semantics: `0` on a short buffer.
    #[must_use]
    pub fn read_int32_lenient(&mut self) -> i32 {
        self.read_int32().unwrap_or(0)
    }

    /// Strict `readUint32`.
    pub fn read_uint32(&mut self) -> Result<u32> {
        self.check(4)?;
        let mut buf = [0u8; 4];
        buf.copy_from_slice(&self.data[self.pos..self.pos + 4]);
        self.pos += 4;
        Ok(u32::from_le_bytes(buf))
    }

    /// `readUint32` with ART semantics: `0` on a short buffer.
    #[must_use]
    pub fn read_uint32_lenient(&mut self) -> u32 {
        self.read_uint32().unwrap_or(0)
    }

    /// Strict `readInt64`.
    pub fn read_int64(&mut self) -> Result<i64> {
        self.check(8)?;
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&self.data[self.pos..self.pos + 8]);
        self.pos += 8;
        Ok(i64::from_le_bytes(buf))
    }

    /// `readInt64` with ART semantics: `0` on a short buffer.
    #[must_use]
    pub fn read_int64_lenient(&mut self) -> i64 {
        self.read_int64().unwrap_or(0)
    }

    /// Strict `readUint64`.
    pub fn read_uint64(&mut self) -> Result<u64> {
        self.check(8)?;
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&self.data[self.pos..self.pos + 8]);
        self.pos += 8;
        Ok(u64::from_le_bytes(buf))
    }

    /// Strict `readFloat`.
    pub fn read_float(&mut self) -> Result<f32> {
        self.check(4)?;
        let mut buf = [0u8; 4];
        buf.copy_from_slice(&self.data[self.pos..self.pos + 4]);
        self.pos += 4;
        Ok(f32::from_le_bytes(buf))
    }

    /// `readFloat` with ART semantics: `0.0` on a short buffer.
    #[must_use]
    pub fn read_float_lenient(&mut self) -> f32 {
        self.read_float().unwrap_or(0.0)
    }

    /// Strict `readDouble`.
    pub fn read_double(&mut self) -> Result<f64> {
        self.check(8)?;
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&self.data[self.pos..self.pos + 8]);
        self.pos += 8;
        Ok(f64::from_le_bytes(buf))
    }

    /// `readDouble` with ART semantics: `0.0` on a short buffer.
    #[must_use]
    pub fn read_double_lenient(&mut self) -> f64 {
        self.read_double().unwrap_or(0.0)
    }

    /// Strict `readBoolean` (`!= 0`).
    pub fn read_bool(&mut self) -> Result<bool> {
        Ok(self.read_int32()? != 0)
    }

    /// `readBoolean` with ART semantics: `false` on a short buffer.
    #[must_use]
    pub fn read_bool_lenient(&mut self) -> bool {
        self.read_bool().unwrap_or(false)
    }

    /// Strict `readByte`.
    pub fn read_byte(&mut self) -> Result<i8> {
        Ok(self.read_int32()? as i8)
    }

    /// Strict `readChar` (UTF-16 code unit).
    pub fn read_char(&mut self) -> Result<u16> {
        Ok(self.read_int32()? as u16)
    }

    /// Read `len` bytes verbatim (the counterpart of [`Self::write_raw_bytes`]).
    pub fn read_raw_bytes(&mut self, len: usize) -> Result<Vec<u8>> {
        self.check(len)?;
        let bytes = self.data[self.pos..self.pos + len].to_vec();
        self.pos += len;
        Ok(bytes)
    }

    // ------------------------------------------------------------------
    // String8
    // ------------------------------------------------------------------

    /// Write a `String8` (`int32 len` + UTF-8 + NUL + pad).
    pub fn write_string8(&mut self, s: &str) -> Result<()> {
        self.write_string8_opt(Some(s))
    }

    /// Write a `String8` or the Android null marker (`-1`).
    pub fn write_string8_opt(&mut self, s: Option<&str>) -> Result<()> {
        let Some(s) = s else {
            self.write_int32(-1);
            return Ok(());
        };
        let len = s.len();
        if len > i32::MAX as usize {
            return Err(ParcelError::BadLength {
                len,
                max: i32::MAX as usize,
            });
        }
        self.write_int32(len as i32);
        // Write directly into the buffer: no temporary allocation.
        let padded = align4(len + 1);
        let start = self.pos;
        let end = start + padded;
        self.reserve(end);
        self.data[start..start + len].copy_from_slice(s.as_bytes());
        self.data[start + len] = 0;
        self.data[start + len + 1..end].fill(0);
        self.pos = end;
        Ok(())
    }

    /// Strict `readString8`; `Ok(None)` for the `-1` null marker.
    pub fn read_string8(&mut self) -> Result<Option<String>> {
        let len = self.read_int32()?;
        if len < 0 {
            return Ok(None);
        }
        let len = len as usize;
        let total = align4(len.checked_add(1).ok_or(ParcelError::BadLength {
            len,
            max: i32::MAX as usize,
        })?);
        self.check(total)?;
        let start = self.pos;
        // libbinder advances the position before validating the terminator.
        self.pos += total;
        if self.data[start + len] != 0 {
            return Err(ParcelError::BadString("String8 is not NUL terminated"));
        }
        let bytes = &self.data[start..start + len];
        let s = std::str::from_utf8(bytes)
            .map_err(|_| ParcelError::BadString("String8 is not valid UTF-8"))?;
        Ok(Some(s.to_owned()))
    }

    /// `readString8` that never errors: `None` for null or a malformed buffer.
    #[must_use]
    pub fn read_string8_lenient(&mut self) -> Option<String> {
        self.read_string8().ok().flatten()
    }

    // ------------------------------------------------------------------
    // String16
    // ------------------------------------------------------------------

    /// Write a `String16` (`int32 units` + UTF-16LE + NUL + pad).
    pub fn write_string16(&mut self, s: &str) -> Result<()> {
        self.write_string16_opt(Some(s))
    }

    /// Write a `String16` or the Android null marker (`-1`).
    pub fn write_string16_opt(&mut self, s: Option<&str>) -> Result<()> {
        let Some(s) = s else {
            self.write_int32(-1);
            return Ok(());
        };
        let units = s.encode_utf16().count();
        if units > i32::MAX as usize {
            return Err(ParcelError::BadLength {
                len: units,
                max: i32::MAX as usize,
            });
        }
        self.write_int32(units as i32);
        // Write directly into the buffer: no temporary allocation.
        let byte_len = units * 2;
        let padded = align4(byte_len + 2);
        let start = self.pos;
        let end = start + padded;
        self.reserve(end);
        let mut i = start;
        for u in s.encode_utf16() {
            self.data[i..i + 2].copy_from_slice(&u.to_le_bytes());
            i += 2;
        }
        self.data[i] = 0;
        self.data[i + 1] = 0;
        self.data[i + 2..end].fill(0);
        self.pos = end;
        Ok(())
    }

    /// Strict `readString16`; `Ok(None)` for the `-1` null marker.
    pub fn read_string16(&mut self) -> Result<Option<String>> {
        let len = self.read_int32()?;
        if len < 0 {
            return Ok(None);
        }
        let units = len as usize;
        // libbinder caps String16 at u32::MAX/4 units.
        let byte_len = units
            .checked_mul(2)
            .ok_or(ParcelError::BadLength {
                len: units,
                max: u32::MAX as usize / 4,
            })?
            .checked_add(2)
            .ok_or(ParcelError::BadLength {
                len: units,
                max: u32::MAX as usize / 4,
            })?;
        let total = align4(byte_len);
        self.check(total)?;
        let start = self.pos;
        self.pos += total;
        if self.data[start + units * 2] != 0 || self.data[start + units * 2 + 1] != 0 {
            return Err(ParcelError::BadString("String16 is not NUL terminated"));
        }
        let mut v = Vec::with_capacity(units);
        for i in 0..units {
            v.push(u16::from_le_bytes([
                self.data[start + 2 * i],
                self.data[start + 2 * i + 1],
            ]));
        }
        let s = String::from_utf16(&v)
            .map_err(|_| ParcelError::BadString("String16 contains an unpaired surrogate"))?;
        Ok(Some(s))
    }

    /// `readString16` that never errors: `None` for null or a malformed buffer.
    #[must_use]
    pub fn read_string16_lenient(&mut self) -> Option<String> {
        self.read_string16().ok().flatten()
    }

    // ------------------------------------------------------------------
    // arrays
    // ------------------------------------------------------------------

    /// Write `byte[]`: `int32 len` + bytes + pad, or `-1` for null.
    pub fn write_byte_array(&mut self, v: Option<&[u8]>) -> Result<()> {
        let Some(v) = v else {
            self.write_int32(-1);
            return Ok(());
        };
        if v.len() > i32::MAX as usize {
            return Err(ParcelError::BadLength {
                len: v.len(),
                max: i32::MAX as usize,
            });
        }
        self.write_int32(v.len() as i32);
        self.write_padded(v);
        Ok(())
    }

    /// Read `byte[]`.
    pub fn read_byte_array(&mut self) -> Result<Option<Vec<u8>>> {
        let len = self.read_int32()?;
        if len < 0 {
            return Ok(None);
        }
        let n = len as usize;
        let total = align4(n);
        self.check(total)?;
        let v = self.data[self.pos..self.pos + n].to_vec();
        self.pos += total;
        Ok(Some(v))
    }

    /// Write `int[]`: `int32 count` + count*4 bytes + pad, or `-1` for null.
    pub fn write_int32_array(&mut self, v: Option<&[i32]>) -> Result<()> {
        let Some(v) = v else {
            self.write_int32(-1);
            return Ok(());
        };
        if v.len() > i32::MAX as usize {
            return Err(ParcelError::BadLength {
                len: v.len(),
                max: i32::MAX as usize,
            });
        }
        self.write_int32(v.len() as i32);
        for x in v {
            self.write_int32(*x);
        }
        Ok(())
    }

    /// Read `int[]`.
    pub fn read_int32_array(&mut self) -> Result<Option<Vec<i32>>> {
        let len = self.read_int32()?;
        if len < 0 {
            return Ok(None);
        }
        let n = len as usize;
        let bytes = n.checked_mul(4).ok_or(ParcelError::BadLength {
            len: n,
            max: i32::MAX as usize,
        })?;
        let total = align4(bytes);
        self.check(total)?;
        let mut v = Vec::with_capacity(n);
        for i in 0..n {
            let off = self.pos + 4 * i;
            v.push(i32::from_le_bytes([
                self.data[off],
                self.data[off + 1],
                self.data[off + 2],
                self.data[off + 3],
            ]));
        }
        self.pos += total;
        Ok(Some(v))
    }

    /// Write `long[]`.
    pub fn write_int64_array(&mut self, v: Option<&[i64]>) -> Result<()> {
        let Some(v) = v else {
            self.write_int32(-1);
            return Ok(());
        };
        if v.len() > i32::MAX as usize {
            return Err(ParcelError::BadLength {
                len: v.len(),
                max: i32::MAX as usize,
            });
        }
        self.write_int32(v.len() as i32);
        for x in v {
            self.write_int64(*x);
        }
        Ok(())
    }

    /// Read `long[]`.
    pub fn read_int64_array(&mut self) -> Result<Option<Vec<i64>>> {
        let len = self.read_int32()?;
        if len < 0 {
            return Ok(None);
        }
        let n = len as usize;
        let bytes = n.checked_mul(8).ok_or(ParcelError::BadLength {
            len: n,
            max: i32::MAX as usize,
        })?;
        self.check(bytes)?;
        let mut v = Vec::with_capacity(n);
        for i in 0..n {
            let off = self.pos + 8 * i;
            let mut b = [0u8; 8];
            b.copy_from_slice(&self.data[off..off + 8]);
            v.push(i64::from_le_bytes(b));
        }
        self.pos += bytes;
        Ok(Some(v))
    }

    /// Write `float[]`.
    pub fn write_float_array(&mut self, v: Option<&[f32]>) -> Result<()> {
        let Some(v) = v else {
            self.write_int32(-1);
            return Ok(());
        };
        if v.len() > i32::MAX as usize {
            return Err(ParcelError::BadLength {
                len: v.len(),
                max: i32::MAX as usize,
            });
        }
        self.write_int32(v.len() as i32);
        for x in v {
            self.write_float(*x);
        }
        Ok(())
    }

    /// Read `float[]`.
    pub fn read_float_array(&mut self) -> Result<Option<Vec<f32>>> {
        let len = self.read_int32()?;
        if len < 0 {
            return Ok(None);
        }
        let n = len as usize;
        let bytes = n.checked_mul(4).ok_or(ParcelError::BadLength {
            len: n,
            max: i32::MAX as usize,
        })?;
        self.check(bytes)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(self.read_float()?);
        }
        Ok(Some(v))
    }

    /// Write `double[]`.
    pub fn write_double_array(&mut self, v: Option<&[f64]>) -> Result<()> {
        let Some(v) = v else {
            self.write_int32(-1);
            return Ok(());
        };
        if v.len() > i32::MAX as usize {
            return Err(ParcelError::BadLength {
                len: v.len(),
                max: i32::MAX as usize,
            });
        }
        self.write_int32(v.len() as i32);
        for x in v {
            self.write_double(*x);
        }
        Ok(())
    }

    /// Read `double[]`.
    pub fn read_double_array(&mut self) -> Result<Option<Vec<f64>>> {
        let len = self.read_int32()?;
        if len < 0 {
            return Ok(None);
        }
        let n = len as usize;
        let bytes = n.checked_mul(8).ok_or(ParcelError::BadLength {
            len: n,
            max: i32::MAX as usize,
        })?;
        self.check(bytes)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(self.read_double()?);
        }
        Ok(Some(v))
    }

    /// Write `String16[]`: `int32 count` then each `String16`.
    ///
    /// This is `Parcel.writeStringArray` (AOSP `Parcel.java:1867`, which
    /// delegates to `writeString16Array`). Null *elements* are possible in a
    /// Java `String[]`; use [`Parcel::write_string16_array_nullable`] for those.
    pub fn write_string16_array(&mut self, v: Option<&[&str]>) -> Result<()> {
        match v {
            Some(v) => {
                let items: Vec<Option<&str>> = v.iter().map(|s| Some(*s)).collect();
                self.write_string16_array_nullable(Some(&items))
            }
            None => self.write_string16_array_nullable(None),
        }
    }

    /// Write `String16[]` allowing `null` elements (`int32 -1` each).
    ///
    /// A `null` element and an empty string are **not** the same on the wire:
    /// `null` is a single `-1` word, `""` is a `0` length word plus its 4-byte
    /// NUL padding. Writing one for the other desynchronises the reader.
    pub fn write_string16_array_nullable(&mut self, v: Option<&[Option<&str>]>) -> Result<()> {
        let Some(v) = v else {
            self.write_int32(-1);
            return Ok(());
        };
        if v.len() > i32::MAX as usize {
            return Err(ParcelError::BadLength {
                len: v.len(),
                max: i32::MAX as usize,
            });
        }
        self.write_int32(v.len() as i32);
        for s in v {
            self.write_string16_opt(*s)?;
        }
        Ok(())
    }

    /// Write `String8[]`: `int32 count` then each `String8`.
    ///
    /// This is `Parcel.writeString8Array` (AOSP `Parcel.java:1881`), the
    /// encoding the `android.content.pm` parcelables (`PackageInfo`,
    /// `ApplicationInfo`, `ComponentInfo`, `InstrumentationInfo`) use for their
    /// `String[]` fields. It is **not** wire-compatible with
    /// [`Parcel::write_string16_array`] = `writeStringArray`: every element's
    /// length word counts UTF-8 **bytes** and the payload is one byte per ASCII
    /// character, not two. Writing the wrong one shifts the whole rest of the
    /// parcel (found on `getPackageInfo(..., GET_SIGNATURES|GET_META_DATA)`,
    /// which made the framework allocate a 7 676 737 936-byte `String[]`).
    pub fn write_string8_array(&mut self, v: Option<&[Option<&str>]>) -> Result<()> {
        let Some(v) = v else {
            self.write_int32(-1);
            return Ok(());
        };
        if v.len() > i32::MAX as usize {
            return Err(ParcelError::BadLength {
                len: v.len(),
                max: i32::MAX as usize,
            });
        }
        self.write_int32(v.len() as i32);
        for s in v {
            self.write_string8_opt(*s)?;
        }
        Ok(())
    }

    /// Read `String16[]`.
    pub fn read_string16_array(&mut self) -> Result<Option<Vec<Option<String>>>> {
        self.read_string_array(false)
    }

    /// Read `String8[]` (`Parcel.createString8Array`).
    pub fn read_string8_array(&mut self) -> Result<Option<Vec<Option<String>>>> {
        self.read_string_array(true)
    }

    fn read_string_array(&mut self, eight_bit: bool) -> Result<Option<Vec<Option<String>>>> {
        let len = self.read_int32()?;
        if len < 0 {
            return Ok(None);
        }
        let n = len as usize;
        // Every element carries at least its 4-byte length word, so a count
        // that cannot fit in the remaining payload is malformed. Reject it
        // before allocating the backing vector: a guest `int32` of
        // `i32::MAX` must never drive a multi-GB `Vec::with_capacity`.
        let min_bytes = n.checked_mul(4).ok_or(ParcelError::BadLength {
            len: n,
            max: self.remaining() / 4,
        })?;
        if min_bytes > self.remaining() {
            return Err(ParcelError::BadLength {
                len: n,
                max: self.remaining() / 4,
            });
        }
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(if eight_bit {
                self.read_string8()?
            } else {
                self.read_string16()?
            });
        }
        Ok(Some(v))
    }

    // ------------------------------------------------------------------
    // blobs (inline form only)
    // ------------------------------------------------------------------

    /// Write an inline blob: `int32 0` marker + bytes + pad.
    ///
    /// Android's ashmem (`int32 1|2` + fd) form is not implemented; see
    /// [`ParcelError::AshmemUnsupported`].
    pub fn write_blob(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > i32::MAX as usize {
            return Err(ParcelError::BadLength {
                len: bytes.len(),
                max: i32::MAX as usize,
            });
        }
        self.write_int32(0);
        self.write_padded(bytes);
        Ok(())
    }

    /// Read an inline blob whose payload length the caller already knows.
    pub fn read_blob(&mut self, len: usize) -> Result<Option<Vec<u8>>> {
        let marker = self.read_int32()?;
        match marker {
            0 => {
                let total = align4(len);
                self.check(total)?;
                let v = self.data[self.pos..self.pos + len].to_vec();
                self.pos += total;
                Ok(Some(v))
            }
            1 | 2 => Err(ParcelError::AshmemUnsupported),
            _ => Err(ParcelError::BadString("bad blob marker")),
        }
    }

    // ------------------------------------------------------------------
    // parcelable markers
    // ------------------------------------------------------------------

    /// Write the `Parcelable` null/non-null marker (`1` for non-null, `0` for
    /// null).
    ///
    /// Mirrors `Parcel::writeRawNullableParcelable`.
    pub fn write_parcelable_marker(&mut self, is_non_null: bool) {
        self.write_int32(i32::from(is_non_null));
    }

    /// Read the `Parcelable` marker; `true` means a non-null object follows.
    pub fn read_parcelable_marker(&mut self) -> Result<bool> {
        Ok(self.read_int32()? != 0)
    }

    // ------------------------------------------------------------------
    // binders
    // ------------------------------------------------------------------

    fn write_flat_object(&mut self, obj: FlatObject) {
        self.objects.push(self.pos);
        self.write_bytes(&obj.to_bytes());
    }

    fn read_flat_object(&mut self) -> Result<FlatObject> {
        self.check(FLAT_OBJECT_SIZE)?;
        let mut bytes = [0u8; FLAT_OBJECT_SIZE];
        bytes.copy_from_slice(&self.data[self.pos..self.pos + FLAT_OBJECT_SIZE]);
        self.pos += FLAT_OBJECT_SIZE;
        Ok(FlatObject::from_bytes(&bytes))
    }

    /// Write a strong binder as a local object.
    pub fn write_strong_binder(&mut self, id: BinderId) {
        self.write_strong_binder_kind(id, BinderKind::Local);
    }

    /// Write a strong binder with an explicit local/proxy tag.
    pub fn write_strong_binder_kind(&mut self, id: BinderId, kind: BinderKind) {
        let obj = if id.is_null() {
            FlatObject::null_binder()
        } else {
            FlatObject::binder(id, kind)
        };
        self.write_flat_object(obj);
    }

    /// Write a strong binder or null in one call.
    pub fn write_strong_binder_opt(&mut self, id: Option<BinderId>) {
        match id {
            Some(id) => self.write_strong_binder(id),
            None => self.write_flat_object(FlatObject::null_binder()),
        }
    }

    /// Read a strong binder; `Ok(None)` for a null binder.
    pub fn read_strong_binder(&mut self) -> Result<Option<BinderId>> {
        let offset = self.pos;
        let obj = self.read_flat_object()?;
        if !obj.is_binder() {
            return Err(ParcelError::NotABinder {
                offset,
                ty: obj.type_,
            });
        }
        if obj.id == 0 {
            // A null binder is written without an object-table entry but *with*
            // the 4-byte stability record (`Parcel::finishFlattenBinder` always
            // writes it). Consume it so the next argument stays aligned.
            if self.wire_null_stability {
                self.check(4)?;
                self.pos += 4;
            }
            Ok(None)
        } else {
            Ok(Some(BinderId(obj.id)))
        }
    }

    /// Mark this parcel as libbinder **wire form** for the stability record that
    /// follows a null binder (see the field docs).
    pub fn set_wire_null_stability(&mut self, yes: bool) {
        self.wire_null_stability = yes;
    }

    // ------------------------------------------------------------------
    // file descriptors
    // ------------------------------------------------------------------

    /// Write a strong binder's worth of "marked" bookkeeping.
    ///
    /// libbinder's `markForBinder` only does anything for RPC binders; since
    /// ACOS's `isForRpc()` is always `false`, this records the id and otherwise
    /// leaves the wire bytes untouched.
    pub fn mark_for_binder(&mut self, id: BinderId) {
        if !id.is_null() {
            self.marked.insert(id.0);
        }
    }

    /// Binder ids passed to [`Parcel::mark_for_binder`].
    pub fn marked_binders(&self) -> impl Iterator<Item = BinderId> + '_ {
        self.marked.iter().copied().map(BinderId)
    }

    /// Duplicate `fd` and write it as a `BINDER_TYPE_FD` flat object.
    ///
    /// The parcel owns the duplicate; it is closed on `recycle`/drop.
    pub fn write_file_descriptor(&mut self, fd: BorrowedFd<'_>) -> Result<()> {
        if !self.allow_fds {
            return Err(ParcelError::FdsNotAllowed);
        }
        let dup = ParcelFd::dup(fd)?;
        let raw = dup.as_raw_fd();
        self.write_flat_object(FlatObject::file_descriptor(raw));
        self.fds.push(dup);
        Ok(())
    }

    /// Write a duplicated descriptor (`writeDupFileDescriptor`).
    ///
    /// Identical to [`Parcel::write_file_descriptor`] here because ACOS always
    /// duplicates the caller's descriptor.
    pub fn write_dup_file_descriptor(&mut self, fd: BorrowedFd<'_>) -> Result<()> {
        self.write_file_descriptor(fd)
    }

    /// Read a file descriptor owned by this parcel; `Ok(None)` for `-1`.
    pub fn read_file_descriptor(&mut self) -> Result<Option<RawFd>> {
        let offset = self.pos;
        let obj = self.read_flat_object()?;
        if !obj.is_file_descriptor() {
            return Err(ParcelError::NotAFileDescriptor {
                offset,
                ty: obj.type_,
            });
        }
        if obj.id == 0 {
            Ok(None)
        } else {
            Ok(Some(obj.id as RawFd))
        }
    }

    /// Descriptors currently owned by this parcel.
    #[must_use]
    pub fn owned_file_descriptors(&self) -> Vec<RawFd> {
        self.fds.iter().map(ParcelFd::as_raw_fd).collect()
    }

    /// Borrow one of the descriptors this parcel owns, by raw number.
    ///
    /// Safe because the borrow cannot outlive the parcel, which owns the
    /// descriptor for that whole time.
    #[must_use]
    pub fn borrowed_file_descriptor(&self, raw: RawFd) -> Option<BorrowedFd<'_>> {
        self.fds
            .iter()
            .find(|f| f.as_raw_fd() == raw)
            .map(ParcelFd::as_fd)
    }

    fn object_is_fd(&self, offset: usize) -> bool {
        offset + 4 <= self.data.len()
            && u32::from_le_bytes([
                self.data[offset],
                self.data[offset + 1],
                self.data[offset + 2],
                self.data[offset + 3],
            ]) == BINDER_TYPE_FD
    }

    /// `nativeHasFileDescriptors`: any `BINDER_TYPE_FD` object.
    #[must_use]
    pub fn has_file_descriptors(&self) -> bool {
        self.objects.iter().any(|&o| self.object_is_fd(o))
    }

    /// `nativeHasFileDescriptorsInRange(offset, length)`.
    #[must_use]
    pub fn has_file_descriptors_in_range(&self, offset: usize, len: usize) -> bool {
        let end = offset.saturating_add(len);
        self.objects
            .iter()
            .any(|&o| o >= offset && o < end && self.object_is_fd(o))
    }

    /// `nativePushAllowFds`: set the flag and return the previous value.
    pub fn push_allow_fds(&mut self, allow: bool) -> bool {
        let old = self.allow_fds;
        self.allow_fds = allow;
        old
    }

    /// `nativeRestoreAllowFds`: restore the flag.
    pub fn restore_allow_fds(&mut self, allow: bool) {
        self.allow_fds = allow;
    }

    /// Current `allowFds` state.
    #[must_use]
    pub fn allow_fds(&self) -> bool {
        self.allow_fds
    }

    /// `nativeIsForRpc`: always `false` for ACOS.
    #[must_use]
    pub fn is_for_rpc(&self) -> bool {
        false
    }

    /// Magic this parcel writes/expects in front of an interface token.
    ///
    /// Defaults to [`crate::token::SYSTEM_INTERFACE_TOKEN_HEADER`].
    #[must_use]
    pub fn interface_token_header(&self) -> u32 {
        self.token_header
    }

    /// Select the interface-token magic, e.g.
    /// [`crate::token::VNDK_INTERFACE_TOKEN_HEADER`] when matching a vendor
    /// peer.
    pub fn set_interface_token_header(&mut self, header: u32) {
        self.token_header = header;
    }

    // ------------------------------------------------------------------
    // position / size
    // ------------------------------------------------------------------

    /// Current read/write position (`nativeDataPosition`).
    #[must_use]
    pub fn data_position(&self) -> usize {
        self.pos
    }

    /// Set the read/write position (`nativeSetDataPosition`).
    pub fn set_data_position(&mut self, pos: usize) {
        self.pos = pos;
    }

    /// Logical data size (`nativeDataSize`).
    #[must_use]
    pub fn data_size(&self) -> usize {
        self.data.len()
    }

    /// Bytes left between the current position and the logical data size.
    ///
    /// Used to bound guest-supplied element counts *before* any backing-store
    /// allocation (host-safety; see `HONESTY_AUDIT.md` #3).
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    /// Truncate or zero-extend the logical data size (`setDataSize`).
    pub fn set_data_size(&mut self, size: usize) {
        self.data.resize(size, 0);
        if self.pos > size {
            self.pos = size;
        }
    }

    /// Bytes left to read (`nativeDataAvail`).
    #[must_use]
    pub fn data_avail(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    /// Allocated capacity (for the bench / pooling layer).
    #[must_use]
    pub fn data_capacity(&self) -> usize {
        self.data.capacity()
    }

    /// Offsets of every flat object (`binder`/`fd`) in the data stream.
    #[must_use]
    pub fn objects(&self) -> &[usize] {
        &self.objects
    }

    /// Number of flat objects.
    #[must_use]
    pub fn objects_count(&self) -> usize {
        self.objects.len()
    }

    /// The binder ids this parcel carries, in object-table order (MPROC2 3a).
    ///
    /// A `system_server`-side caller that receives a request has to know *which
    /// binders the caller handed over* — those belong to the caller's process,
    /// and a delivery to one of them must be routed back there. File-descriptor
    /// objects are skipped (their `id` is a descriptor, not a binder id), and an
    /// object whose offset is out of range is ignored rather than read past the
    /// buffer.
    #[must_use]
    pub fn binder_ids(&self) -> Vec<BinderId> {
        let mut out = Vec::new();
        for &o in &self.objects {
            if o + FLAT_OBJECT_SIZE > self.data.len() {
                continue;
            }
            let mut bytes = [0u8; FLAT_OBJECT_SIZE];
            bytes.copy_from_slice(&self.data[o..o + FLAT_OBJECT_SIZE]);
            let obj = FlatObject::from_bytes(&bytes);
            if obj.is_binder() && obj.id != 0 {
                out.push(BinderId(obj.id));
            }
        }
        out
    }

    // ------------------------------------------------------------------
    // append / marshal / recycle
    // ------------------------------------------------------------------

    /// Append `len` bytes from `other` starting at `offset`, copying the object
    /// offsets inside the range and duplicating any descriptors.
    ///
    /// Byte-identical to a manual copy for binder-only ranges; the object
    /// offsets are rebased from `offset` to this parcel's current position.
    pub fn append_from(&mut self, other: &Parcel, offset: usize, len: usize) -> Result<()> {
        let src_end = offset.checked_add(len).ok_or(ParcelError::BadLength {
            len,
            max: i32::MAX as usize,
        })?;
        if src_end > other.data.len() {
            return Err(ParcelError::Underflow {
                at: offset,
                needed: len,
                available: other.data.len().saturating_sub(offset),
            });
        }
        let old_pos = self.pos;
        let dst_end = old_pos.checked_add(len).ok_or(ParcelError::BadLength {
            len,
            max: i32::MAX as usize,
        })?;
        self.reserve(dst_end);
        self.data[old_pos..dst_end].copy_from_slice(&other.data[offset..src_end]);

        let mut rebased = Vec::new();
        for &o in &other.objects {
            if o < offset || o >= src_end {
                continue;
            }
            let dst = o - offset + old_pos;
            let ty = u32::from_le_bytes([
                other.data[o],
                other.data[o + 1],
                other.data[o + 2],
                other.data[o + 3],
            ]);
            if ty == BINDER_TYPE_FD {
                let id = u64::from_le_bytes([
                    other.data[o + 8],
                    other.data[o + 9],
                    other.data[o + 10],
                    other.data[o + 11],
                    other.data[o + 12],
                    other.data[o + 13],
                    other.data[o + 14],
                    other.data[o + 15],
                ]);
                let src = other
                    .fds
                    .iter()
                    .find(|f| f.as_raw_fd() as u64 == id)
                    .ok_or(ParcelError::Fd(format!(
                        "appendFrom: no owned fd for object id {id}"
                    )))?;
                let dup = src.try_clone()?;
                let raw = dup.as_raw_fd() as u64;
                self.data[dst + 8..dst + 16].copy_from_slice(&raw.to_le_bytes());
                self.fds.push(dup);
            }
            rebased.push(dst);
        }
        self.objects.extend(rebased);
        self.pos = dst_end;
        Ok(())
    }

    /// Serialise the parcel **data bytes only** (`nativeMarshall`).
    ///
    /// The object table is deliberately not serialised: ACOS binder handles are
    /// process-local, so odds are the bytes-only contract in T-01.
    #[must_use]
    pub fn marshall(&self) -> Vec<u8> {
        self.data.clone()
    }

    /// Replace the parcel contents with `bytes` (`nativeUnmarshall`).
    pub fn unmarshall(&mut self, bytes: &[u8]) {
        self.reset();
        self.data.extend_from_slice(bytes);
    }

    /// Record `offset` as an object-table entry of an **imported** parcel.
    ///
    /// [`Parcel::unmarshall`] copies bytes only, because a parcel that came from
    /// the binder driver keeps its object table out of band (the driver's
    /// `offsets` array, not part of `data`). The importer
    /// (`acos_binder_peer::import_parcel`) walks those offsets, rewrites each
    /// `flat_binder_object` into the in-process form and registers its position
    /// here, so [`Parcel::objects`] answers what the sender wrote. Without it
    /// every incoming request looks object-free, and a handler that must find a
    /// client binder in the request — an `ISynchronousResultReceiver`, an
    /// `IIntentReceiver` — never sees it.
    ///
    /// `unmarshall` copies bytes without rebuilding the object table, so a
    /// caller that imports a wire parcel (and rewrites its binder objects)
    /// records them here. The table is what `marshall`/`frame_from_parcel` walk
    /// to re-emit the wire stability records, and what `acos-services`'
    /// `writeValue` length check uses to account for the stripped records.
    pub fn register_object(&mut self, offset: usize) {
        match self.objects.binary_search(&offset) {
            // Offsets arrive in ascending order; keep the table sorted so the
            // marshaller can walk it with a single cursor.
            Ok(_) => {}
            Err(index) => self.objects.insert(index, offset),
        }
    }

    /// Replace the parcel contents with `bytes` plus the object table the wire
    /// carried (the binder driver's `offsets` array), in one step.
    ///
    /// [`Parcel::unmarshall`] copies bytes only; the importer installs each
    /// rewritten object position through [`Parcel::register_object`].
    pub fn unmarshall_with_objects(&mut self, bytes: &[u8], offsets: &[usize]) {
        self.unmarshall(bytes);
        for offset in offsets {
            if offset.saturating_add(FLAT_OBJECT_SIZE) <= self.data.len() {
                self.register_object(*offset);
            }
        }
    }

    /// `nativeFreeBuffer` / `Parcel.recycle`: drop data, objects and fds.
    pub fn recycle(&mut self) {
        self.reset();
    }

    /// Reset to an empty parcel (closes every owned descriptor).
    pub fn reset(&mut self) {
        self.data.clear();
        self.pos = 0;
        self.objects.clear();
        self.fds.clear();
        self.marked.clear();
        self.allow_fds = true;
        self.token_header = crate::token::SYSTEM_INTERFACE_TOKEN_HEADER;
        self.wire_null_stability = false;
    }

    /// `enforceNoDataAvail`: error when unread bytes remain.
    pub fn enforce_no_data_avail(&self) -> Result<()> {
        let n = self.data_avail();
        if n > 0 {
            Err(ParcelError::BadLength { len: n, max: 0 })
        } else {
            Ok(())
        }
    }
}

// ----------------------------------------------------------------------
// ParcelTable
// ----------------------------------------------------------------------

fn encode_handle(index: u32, generation: u32) -> u64 {
    (u64::from(generation) << 32) | u64::from(index)
}

fn decode_handle(handle: u64) -> (u32, u32) {
    (handle as u32, (handle >> 32) as u32)
}

struct Slot {
    generation: u32,
    parcel: Option<Parcel>,
}

/// A slab of [`Parcel`]s addressed by a stable `u64` handle.
///
/// This is the object the Java `android.os.Parcel` natives will hold as a
/// `jlong`: `nativeCreate()` allocates a handle, every `nativeWrite*` looks the
/// parcel up, and `nativeFreeBuffer`/recycle releases it. Handles carry a
/// generation so a stale `jlong` can never alias a recycled slot.
pub struct ParcelTable {
    slots: Mutex<Vec<Slot>>,
    free: Mutex<Vec<u32>>,
}

impl Default for ParcelTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ParcelTable {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            slots: Mutex::new(Vec::new()),
            free: Mutex::new(Vec::new()),
        }
    }

    /// Allocate a new empty parcel and return its handle.
    pub fn create(&self) -> u64 {
        self.create_with(Parcel::new())
    }

    /// Insert `parcel` and return its handle.
    pub fn create_with(&self, parcel: Parcel) -> u64 {
        let mut free = lock(&self.free);
        let mut slots = lock(&self.slots);
        if let Some(index) = free.pop() {
            let slot = &mut slots[index as usize];
            slot.parcel = Some(parcel);
            encode_handle(index, slot.generation)
        } else {
            let index = slots.len() as u32;
            slots.push(Slot {
                generation: 0,
                parcel: Some(parcel),
            });
            encode_handle(index, 0)
        }
    }

    /// Drop the parcel behind `handle`, invalidating the handle.
    pub fn free(&self, handle: u64) -> Result<()> {
        let (index, generation) = decode_handle(handle);
        {
            let mut slots = lock(&self.slots);
            let slot = slots
                .get_mut(index as usize)
                .ok_or(ParcelError::StaleHandle(handle))?;
            if slot.generation != generation || slot.parcel.is_none() {
                return Err(ParcelError::StaleHandle(handle));
            }
            slot.parcel = None;
            slot.generation = slot.generation.wrapping_add(1);
        }
        lock(&self.free).push(index);
        Ok(())
    }

    /// Run `f` with mutable access to the parcel behind `handle`.
    pub fn with_parcel<R>(&self, handle: u64, f: impl FnOnce(&mut Parcel) -> R) -> Result<R> {
        let (index, generation) = decode_handle(handle);
        let mut slots = lock(&self.slots);
        let slot = slots
            .get_mut(index as usize)
            .ok_or(ParcelError::StaleHandle(handle))?;
        if slot.generation != generation {
            return Err(ParcelError::StaleHandle(handle));
        }
        let parcel = slot
            .parcel
            .as_mut()
            .ok_or(ParcelError::StaleHandle(handle))?;
        Ok(f(parcel))
    }

    /// Run `f` with shared access to the parcel behind `handle`.
    pub fn with_parcel_shared<R>(&self, handle: u64, f: impl FnOnce(&Parcel) -> R) -> Result<R> {
        let (index, generation) = decode_handle(handle);
        let slots = lock(&self.slots);
        let slot = slots
            .get(index as usize)
            .ok_or(ParcelError::StaleHandle(handle))?;
        if slot.generation != generation {
            return Err(ParcelError::StaleHandle(handle));
        }
        let parcel = slot
            .parcel
            .as_ref()
            .ok_or(ParcelError::StaleHandle(handle))?;
        Ok(f(parcel))
    }

    /// `true` if `handle` currently refers to a live parcel.
    #[must_use]
    pub fn contains(&self, handle: u64) -> bool {
        let (index, generation) = decode_handle(handle);
        let slots = lock(&self.slots);
        slots
            .get(index as usize)
            .is_some_and(|s| s.generation == generation && s.parcel.is_some())
    }

    /// Number of live parcels.
    #[must_use]
    pub fn len(&self) -> usize {
        lock(&self.slots)
            .iter()
            .filter(|s| s.parcel.is_some())
            .count()
    }

    /// `true` when no parcels are live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
