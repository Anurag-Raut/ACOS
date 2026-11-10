//! AIDL interface tokens.
//!
//! Android 14's `Parcel::writeInterfaceToken` writes, in order:
//!
//! ```text
//! int32  strictModePolicy | 0x80000000   (STRICT_MODE_PENALTY_GATHER)
//! int32  workSourceUid                   (or -1 when not propagating)
//! int32  0x53595354                      (magic header)
//! String16 descriptor
//! ```
//!
//! # The header is build-dependent
//!
//! `frameworks/native/libs/binder/Parcel.cpp` selects the magic with
//! `#ifdef __ANDROID_VNDK__`:
//!
//! * **system** libbinder (what `/system/lib64/libandroid_runtime.so` links
//!   against and what the emulator framework actually uses) writes
//!   [`SYSTEM_INTERFACE_TOKEN_HEADER`] = `0x53595354` (`"SYST"`).
//! * **VNDK/vendor** libbinder writes [`VNDK_INTERFACE_TOKEN_HEADER`] =
//!   `0x564e4452` (`"VNDR"`).
//!
//! Both were disassembled (see the crate `REPORT.md`):
//!
//! | build | path | build-id | `writeInterfaceToken` | `enforceInterface` |
//! |---|---|---|---|---|
//! | system | SDK API-34 google_apis x86_64 `/system/lib64/libbinder.so` | `404ed6308cc41e47e1246a91ff68e31e` | `0x8b746` writes `0x53595354` | `0x82759` compares `0x53595354` |
//! | VNDK | legacy ACVM cache `~/.cache/acvm/w2/artapex/.../com.android.vndk.current/lib64/libbinder.so` | `390786171b247b62d573f168e77a716a` | `0x7fc90`→`0x79350` writes `0x564e4452` | `0x79771` compares `0x564e4452` |
//!
//! ACOS talks to framework services only, so the default is the **system**
//! header. A parcel's header can be switched with
//! [`Parcel::set_interface_token_header`] when a VNDK peer must be matched.
//!
//! `PENALTY_GATHER = 1 << 31` is confirmed both by the SDK source
//! (`android/os/StrictMode.java:338`) and by the `or $0x80000000` in both
//! binaries.
//!
//! `isForRpc()` is `false` for every ACOS parcel, so the three-int prefix is
//! always present.

use crate::error::Result;
use crate::registry;
use crate::Parcel;

/// `STRICT_MODE_PENALTY_GATHER`: bit 31 of the strict-mode word.
///
/// `android.os.StrictMode.PENALTY_GATHER == 1 << 31`.
pub const STRICT_MODE_PENALTY_GATHER: u32 = 0x8000_0000;

/// Magic the **system** libbinder writes. On the wire the four bytes are
/// `54 53 59 53` (`"SYST"`).
///
/// This is the default for ACOS parcels because that is the libbinder
/// `/system/lib64/libandroid_runtime.so` links against.
pub const SYSTEM_INTERFACE_TOKEN_HEADER: u32 = 0x5359_5354;

/// Magic the **VNDK/vendor** libbinder writes. On the wire the four bytes are
/// `52 44 4e 56` (`"RDNV"` when read little-endian; the source literal is
/// `B_PACK_CHARS('V','N','D','R')`).
pub const VNDK_INTERFACE_TOKEN_HEADER: u32 = 0x564e_4452;

/// Default interface-token magic: the system-build value.
pub const INTERFACE_TOKEN_HEADER: u32 = SYSTEM_INTERFACE_TOKEN_HEADER;

impl Parcel {
    /// Write the Android 14 interface-token prefix and `descriptor`.
    ///
    /// Uses this parcel's [`Parcel::interface_token_header`] (system by
    /// default), this thread's strict-mode policy and its work-source uid.
    pub fn write_interface_token(&mut self, descriptor: &str) -> Result<()> {
        let policy = registry::thread_strict_mode_policy() as u32;
        let work_source = if registry::should_propagate_work_source() {
            registry::thread_work_source_uid()
        } else {
            -1
        };
        self.write_interface_token_full(policy, work_source, descriptor)
    }

    /// Write the token prefix with explicit `strict_policy` and `work_source`.
    ///
    /// The gather bit is always OR-ed in, exactly like libbinder; the header
    /// comes from this parcel.
    pub fn write_interface_token_full(
        &mut self,
        strict_policy: u32,
        work_source: i32,
        descriptor: &str,
    ) -> Result<()> {
        let header = self.interface_token_header();
        self.write_interface_token_with_header(header, strict_policy, work_source, descriptor)
    }

    /// Write the token prefix with an explicit header (system/VNDK) as well.
    pub fn write_interface_token_with_header(
        &mut self,
        header: u32,
        strict_policy: u32,
        work_source: i32,
        descriptor: &str,
    ) -> Result<()> {
        self.write_int32((strict_policy | STRICT_MODE_PENALTY_GATHER) as i32);
        self.write_int32(work_source);
        self.write_int32(header as i32);
        self.write_string16(descriptor)
    }

    /// Read and validate the token prefix, then compare the descriptor.
    ///
    /// Matches `Parcel::enforceInterface`: the position advances past the three
    /// ints and the `String16` regardless; the strict-mode policy is installed
    /// on the thread as a side effect. The header compared is this parcel's
    /// [`Parcel::interface_token_header`].
    pub fn enforce_interface(&mut self, expected: &str) -> Result<()> {
        let want_header = self.interface_token_header();
        let policy = (self.read_int32()? as u32) & !STRICT_MODE_PENALTY_GATHER;
        registry::set_thread_strict_mode_policy(policy as i32);
        let work_source = self.read_int32()?;
        registry::set_thread_work_source_uid(work_source);
        let header = self.read_int32()? as u32;
        if header != want_header {
            return Err(crate::error::ParcelError::BadInterfaceTokenHeader {
                found: header,
                expected: want_header,
            });
        }
        let found = self.read_string16()?;
        match found {
            Some(found) if found == expected => Ok(()),
            other => Err(crate::error::ParcelError::InterfaceTokenMismatch {
                expected: expected.to_owned(),
                found: other.unwrap_or_default(),
            }),
        }
    }

    /// Read the token prefix without consuming it.
    ///
    /// Used by the dispatcher to pick a `$Stub.onTransact` target before the
    /// real `enforceInterface` call.
    pub fn peek_interface_token(&mut self) -> Result<Option<String>> {
        let saved_pos = self.data_position();
        let saved_policy = registry::thread_strict_mode_policy();
        let saved_work_source = registry::thread_work_source_uid();
        let want_header = self.interface_token_header();
        let result = (|| {
            let _policy = self.read_int32()?;
            let _work_source = self.read_int32()?;
            let header = self.read_int32()? as u32;
            if header != want_header {
                return Err(crate::error::ParcelError::BadInterfaceTokenHeader {
                    found: header,
                    expected: want_header,
                });
            }
            self.read_string16()
        })();
        self.set_data_position(saved_pos);
        registry::set_thread_strict_mode_policy(saved_policy);
        registry::set_thread_work_source_uid(saved_work_source);
        result
    }

    /// Current thread's strict-mode policy word (without the gather bit).
    #[must_use]
    pub fn thread_strict_mode_policy() -> i32 {
        registry::thread_strict_mode_policy()
    }

    /// Set this thread's strict-mode policy word.
    pub fn set_thread_strict_mode_policy(policy: i32) {
        registry::set_thread_strict_mode_policy(policy);
    }
}
