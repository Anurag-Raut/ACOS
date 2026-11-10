//! Owned file descriptors held by a [`crate::Parcel`].
//!
//! Android hands real file descriptors out of a parcel (`nativeReadFileDescriptor`)
//! and expects the parcel to own a duplicate of everything written. ACOS uses the
//! safe `rustix` wrappers for `memfd_create(2)` and `dup(2)`, so this crate never
//! needs `unsafe` — the whole crate is `#![forbid(unsafe_code)]`.

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};

use crate::error::ParcelError;

/// An owned file descriptor referenced by a `BINDER_TYPE_FD` flat object.
///
/// Dropping this closes the descriptor, which is exactly the lifetime Android
/// gives the parcel's fd: closed on `Parcel.recycle()`/destruction.
#[derive(Debug)]
pub struct ParcelFd {
    fd: OwnedFd,
}

impl ParcelFd {
    /// Wrap an already-owned descriptor.
    #[must_use]
    pub fn from_owned(fd: OwnedFd) -> Self {
        Self { fd }
    }

    /// Create a fresh anonymous `memfd_create(2)` region.
    ///
    /// This is the ACOS substitute for `ashmem` for the traffic documented in
    /// `IGraphicsStats`/`MemoryIntArray`; the descriptor behaves like an
    /// ordinary unlinked file.
    pub fn memfd(name: &str) -> Result<Self, ParcelError> {
        let fd = rustix::fs::memfd_create(name, rustix::fs::MemfdFlags::CLOEXEC)
            .map_err(|e| ParcelError::Fd(format!("memfd_create({name}): {e}")))?;
        Ok(Self { fd })
    }

    /// Duplicate a borrowed descriptor so the parcel owns its own copy.
    pub fn dup(fd: BorrowedFd<'_>) -> Result<Self, ParcelError> {
        let owned = rustix::io::dup(fd).map_err(|e| ParcelError::Fd(format!("dup: {e}")))?;
        Ok(Self { fd: owned })
    }

    /// Borrow the descriptor.
    #[must_use]
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Raw number, as stored in the flat object record.
    #[must_use]
    pub fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Consume the wrapper and hand back the owned descriptor.
    #[must_use]
    pub fn into_owned(self) -> OwnedFd {
        self.fd
    }

    /// Duplicate this descriptor.
    pub fn try_clone(&self) -> Result<Self, ParcelError> {
        Self::dup(self.fd.as_fd())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Seek, SeekFrom, Write};

    use super::*;

    #[test]
    fn memfd_round_trips_content() {
        let owned = ParcelFd::memfd("acos-parcel-test").unwrap();
        let mut file = std::fs::File::from(owned.into_owned());
        file.write_all(b"hello memfd").unwrap();
        file.flush().unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut out = Vec::new();
        file.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"hello memfd");
    }

    #[test]
    fn dup_shares_file_description() {
        let owned = ParcelFd::memfd("acos-parcel-dup").unwrap();
        let dup = owned.try_clone().unwrap();
        assert_ne!(owned.as_raw_fd(), dup.as_raw_fd());

        let mut writer = std::fs::File::from(owned.into_owned());
        writer.write_all(b"abc").unwrap();
        writer.seek(SeekFrom::Start(0)).unwrap();
        let mut reader = std::fs::File::from(dup.into_owned());
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"abc");
    }

    #[test]
    fn memfd_sets_cloexec() {
        let owned = ParcelFd::memfd("flags").unwrap();
        let flags = rustix::io::fcntl_getfd(owned.as_fd()).unwrap();
        assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
        assert!(owned.as_raw_fd() >= 0);
    }
}
