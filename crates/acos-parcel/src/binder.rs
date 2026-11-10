//! Binder references inside a parcel.
//!
//! Real Android writes a 24-byte `flat_binder_object` into the parcel data and
//! records its offset in a parallel object table. ACOS does the same *shape*
//! (24 bytes, same 4/8-byte alignment) but stores a stable [`BinderId`] handle
//! from [`crate::BinderRegistry`] in the union field instead of a raw kernel
//! pointer, because everything is in-process.
//!
//! The type tags are the real Android tags, computed from
//! `B_PACK_CHARS(c1,c2,c3,B_TYPE_LARGE)` in
//! `linux/android/binder.h`. Keeping them means a future differential test can
//! compare an ACOS object blob against a captured libbinder one byte-for-byte
//! in the type/flags words.

/// Offset of the object table entry inside the 24-byte flat object.
pub const FLAT_OBJECT_ID_OFFSET: usize = 8;

/// Size of a `flat_binder_object` on 64-bit Android: `type`, `flags`, a 64-bit
/// union (`binder`/`handle`) and a 64-bit `cookie`.
pub const FLAT_OBJECT_SIZE: usize = 24;

/// `BINDER_TYPE_BINDER` — a local `BBinder`.
pub const BINDER_TYPE_BINDER: u32 = 0x7362_2a85;
/// `BINDER_TYPE_WEAK_BINDER` — a local weak reference.
pub const BINDER_TYPE_WEAK_BINDER: u32 = 0x7762_2a85;
/// `BINDER_TYPE_HANDLE` — a proxy (`BpBinder`).
pub const BINDER_TYPE_HANDLE: u32 = 0x7368_2a85;
/// `BINDER_TYPE_WEAK_HANDLE` — a weak proxy.
pub const BINDER_TYPE_WEAK_HANDLE: u32 = 0x7768_2a85;
/// `BINDER_TYPE_FD` — a file descriptor.
pub const BINDER_TYPE_FD: u32 = 0x6664_2a85;

/// `FLAT_BINDER_FLAG_ACCEPTS_FDS`; libbinder writes `0x7f | this` for strong
/// binders when background scheduling is enabled.
pub const FLAT_BINDER_FLAG_ACCEPTS_FDS: u32 = 0x100;

/// Default flags libbinder stamps on a strong binder (`0x7f | ACCEPTS_FDS`).
pub const STRONG_BINDER_FLAGS: u32 = 0x7f | FLAT_BINDER_FLAG_ACCEPTS_FDS;

/// A stable process-local binder identity.
///
/// `0` is reserved for the null binder, exactly as a zeroed
/// `flat_binder_object` means "no binder" in Android.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BinderId(pub u64);

impl BinderId {
    /// The null binder (written as an object with a zero id).
    pub const NULL: BinderId = BinderId(0);

    /// `true` when this is the null binder.
    #[must_use]
    pub fn is_null(self) -> bool {
        self.0 == 0
    }
}

/// Whether a binder is a local object or a remote proxy.
///
/// Mirrors the `BINDER_TYPE_BINDER`/`BINDER_TYPE_HANDLE` split so the native
/// layer can choose the tag that matches the Java `IBinder`
/// (`Binder` vs `BinderProxy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinderKind {
    /// A locally implemented object (`android.os.Binder`).
    Local,
    /// A proxy for a remote object (`android.os.BinderProxy`).
    Proxy,
    /// A weak reference (not currently produced by ACOS).
    WeakLocal,
    /// A weak proxy (not currently produced by ACOS).
    WeakProxy,
}

impl BinderKind {
    /// Android type tag for this kind.
    #[must_use]
    pub fn type_tag(self) -> u32 {
        match self {
            Self::Local => BINDER_TYPE_BINDER,
            Self::Proxy => BINDER_TYPE_HANDLE,
            Self::WeakLocal => BINDER_TYPE_WEAK_BINDER,
            Self::WeakProxy => BINDER_TYPE_WEAK_HANDLE,
        }
    }

    /// Decode an Android type tag, if it is a binder-ish object.
    #[must_use]
    pub fn from_type_tag(tag: u32) -> Option<Self> {
        match tag {
            BINDER_TYPE_BINDER => Some(Self::Local),
            BINDER_TYPE_HANDLE => Some(Self::Proxy),
            BINDER_TYPE_WEAK_BINDER => Some(Self::WeakLocal),
            BINDER_TYPE_WEAK_HANDLE => Some(Self::WeakProxy),
            _ => None,
        }
    }
}

/// The 24-byte in-parcel flat object record.
///
/// Layout (little-endian, matching `flat_binder_object`):
///
/// | offset | size | field |
/// |--------|------|-------|
/// | 0      | 4    | `type_` (`BINDER_TYPE_*`) |
/// | 4      | 4    | `flags` |
/// | 8      | 8    | `id` (binder id, or raw fd for `BINDER_TYPE_FD`) |
/// | 16     | 8    | `cookie` (reserved / zero in ACOS) |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlatObject {
    /// Android `BINDER_TYPE_*` tag.
    pub type_: u32,
    /// `flat_binder_object` flags.
    pub flags: u32,
    /// Binder id (or raw fd for `BINDER_TYPE_FD`); `0` means null.
    pub id: u64,
    /// Reserved; always `0` in ACOS.
    pub cookie: u64,
}

impl FlatObject {
    /// A strong binder object of the given kind.
    #[must_use]
    pub fn binder(id: BinderId, kind: BinderKind) -> Self {
        Self {
            type_: if id.is_null() {
                BINDER_TYPE_BINDER
            } else {
                kind.type_tag()
            },
            flags: STRONG_BINDER_FLAGS,
            id: id.0,
            cookie: 0,
        }
    }

    /// A null binder, written exactly like Android: a
    /// `BINDER_TYPE_BINDER` with a zero id.
    #[must_use]
    pub fn null_binder() -> Self {
        Self {
            type_: BINDER_TYPE_BINDER,
            flags: STRONG_BINDER_FLAGS,
            id: 0,
            cookie: 0,
        }
    }

    /// A file-descriptor object carrying `fd`.
    #[must_use]
    pub fn file_descriptor(fd: i32) -> Self {
        Self {
            type_: BINDER_TYPE_FD,
            flags: STRONG_BINDER_FLAGS,
            id: fd as u64,
            cookie: 0,
        }
    }

    /// `true` when this object is a binder tag (null binders included).
    #[must_use]
    pub fn is_binder(&self) -> bool {
        BinderKind::from_type_tag(self.type_).is_some()
    }

    /// `true` when this object is a file descriptor.
    #[must_use]
    pub fn is_file_descriptor(&self) -> bool {
        self.type_ == BINDER_TYPE_FD
    }

    /// Encode into the 24-byte on-wire form.
    #[must_use]
    pub fn to_bytes(self) -> [u8; FLAT_OBJECT_SIZE] {
        let mut out = [0u8; FLAT_OBJECT_SIZE];
        out[0..4].copy_from_slice(&self.type_.to_le_bytes());
        out[4..8].copy_from_slice(&self.flags.to_le_bytes());
        out[8..16].copy_from_slice(&self.id.to_le_bytes());
        out[16..24].copy_from_slice(&self.cookie.to_le_bytes());
        out
    }

    /// Decode from the 24-byte on-wire form.
    #[must_use]
    pub fn from_bytes(bytes: &[u8; FLAT_OBJECT_SIZE]) -> Self {
        let mut ty = [0u8; 4];
        ty.copy_from_slice(&bytes[0..4]);
        let mut flags = [0u8; 4];
        flags.copy_from_slice(&bytes[4..8]);
        let mut id = [0u8; 8];
        id.copy_from_slice(&bytes[8..16]);
        let mut cookie = [0u8; 8];
        cookie.copy_from_slice(&bytes[16..24]);
        Self {
            type_: u32::from_le_bytes(ty),
            flags: u32::from_le_bytes(flags),
            id: u64::from_le_bytes(id),
            cookie: u64::from_le_bytes(cookie),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_tags_match_b_pack_chars() {
        fn b_pack(c1: u8, c2: u8, c3: u8, c4: u8) -> u32 {
            (u32::from(c1) << 24) | (u32::from(c2) << 16) | (u32::from(c3) << 8) | u32::from(c4)
        }
        assert_eq!(BINDER_TYPE_BINDER, b_pack(b's', b'b', b'*', 0x85));
        assert_eq!(BINDER_TYPE_WEAK_BINDER, b_pack(b'w', b'b', b'*', 0x85));
        assert_eq!(BINDER_TYPE_HANDLE, b_pack(b's', b'h', b'*', 0x85));
        assert_eq!(BINDER_TYPE_WEAK_HANDLE, b_pack(b'w', b'h', b'*', 0x85));
        assert_eq!(BINDER_TYPE_FD, b_pack(b'f', b'd', b'*', 0x85));
    }

    #[test]
    fn flat_object_is_24_bytes_round_trip() {
        let obj = FlatObject::binder(BinderId(0x1234_5678_9abc_def0), BinderKind::Proxy);
        let bytes = obj.to_bytes();
        assert_eq!(bytes.len(), 24);
        assert_eq!(&bytes[0..4], &BINDER_TYPE_HANDLE.to_le_bytes());
        assert_eq!(&bytes[8..16], &0x1234_5678_9abc_def0u64.to_le_bytes());
        assert_eq!(FlatObject::from_bytes(&bytes), obj);
    }

    #[test]
    fn null_binder_shape() {
        let obj = FlatObject::null_binder();
        assert!(obj.is_binder());
        assert!(!obj.is_file_descriptor());
        assert_eq!(obj.type_, BINDER_TYPE_BINDER);
        assert_eq!(obj.id, 0);
    }
}
