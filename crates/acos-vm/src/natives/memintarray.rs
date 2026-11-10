//! `android.util.MemoryIntArray` natives, backed by a real `memfd_create(2)`
//! region.
//!
//! The Settings provider hands the framework a file-descriptor-backed
//! generation counter (see `research/service-layer/REPORT.md` §3.7). The
//! framework wraps that descriptor in a real `android.util.MemoryIntArray`, so
//! every native the class declares must be supplied by Rust:
//!
//! * `nativeCreate(String, int) -> int` — create an anonymous `memfd`, size it
//!   to `size` 32-bit words and return the descriptor;
//! * `nativeOpen(int fd, boolean ownership) -> long` — map an existing
//!   descriptor (the `(Parcel)` constructor path detaches the fd from a
//!   `ParcelFileDescriptor`); the returned `long` is an ACOS mapping handle;
//! * `nativeSize(int fd) -> int` — the array length in words;
//! * `nativeGet(int fd, long ptr, int index) -> int`;
//! * `nativeSet(int fd, long ptr, int index, int value) -> void`;
//! * `nativeClose(int fd, long ptr, boolean ownership) -> void`.
//!
//! # Backing store
//!
//! ACOS does not `mmap` (the crate is `#![forbid(unsafe_code)]`). Backing the
//! array with a plain host file is observationally equivalent for the only
//! traffic that uses it: a handful of `pread`/`pwrite` 4-byte accesses per
//! launch. Descriptors are addressed by number, exactly as the Java class
//! passes them.
//!
//! `nativeCreate` keeps the freshly created descriptor alive in a per-thread
//! registry; `nativeClose(fd, _, true)` removes it (closing the fd). A
//! descriptor that arrived through a `Parcel` is owned by the parcel layer, so
//! `nativeOpen`/`nativeGet` reach it through `/proc/self/fd/<n>` and
//! `nativeClose(fd, _, false)` is a no-op.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::FileExt;

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::vm::Vm;

thread_local! {
    /// Descriptors created by `nativeCreate`, kept alive until `nativeClose`.
    static CREATED: RefCell<HashMap<RawFd, File>> = RefCell::new(HashMap::new());
}

/// A `MemoryIntArray` may hold at most 1024 words (`MemoryIntArray.MAX_SIZE`).
const MAX_SIZE: i32 = 1024;

fn open_fd(fd: RawFd) -> VmResult<File> {
    // `/proc/self/fd/<n>` re-opens the same file description without `unsafe`.
    // The descriptor must be opened read-write: `MemoryIntArray.set` writes.
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!("/proc/self/fd/{fd}"))
        .map_err(|e| VmError::Internal(format!("MemoryIntArray: open fd {fd}: {e}")))
}

/// Run `f` with a `File` for `fd`, preferring the registry entry we own.
fn with_fd<R>(fd: RawFd, f: impl FnOnce(&File) -> std::io::Result<R>) -> VmResult<R> {
    let registered = CREATED.with(|c| c.borrow().contains_key(&fd));
    if registered {
        CREATED.with(|c| {
            let map = c.borrow();
            let file = map
                .get(&fd)
                .ok_or_else(|| VmError::Internal(format!("MemoryIntArray: fd {fd} vanished")))?;
            f(file)
                .map_err(|e| VmError::Internal(format!("MemoryIntArray: io fd {fd}: {e}")))
        })
    } else {
        let file = open_fd(fd)?;
        f(&file).map_err(|e| VmError::Internal(format!("MemoryIntArray: io fd {fd}: {e}")))
    }
}

fn read_word(fd: RawFd, index: i32) -> VmResult<i32> {
    if index < 0 {
        return Err(VmError::Internal(format!(
            "MemoryIntArray.get({index}): negative index"
        )));
    }
    let mut buf = [0u8; 4];
    with_fd(fd, |file| file.read_exact_at(&mut buf, index as u64 * 4))?;
    Ok(i32::from_le_bytes(buf))
}

fn write_word(fd: RawFd, index: i32, value: i32) -> VmResult<()> {
    if index < 0 {
        return Err(VmError::Internal(format!(
            "MemoryIntArray.set({index}): negative index"
        )));
    }
    with_fd(fd, |file| file.write_all_at(&value.to_le_bytes(), index as u64 * 4))
}

fn native_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _name = vm
        .java_string_to_rust(vm.arg_ref(ctx, 0))
        .unwrap_or_default();
    let size = vm.arg_i32(ctx, 1);
    if !(0..=MAX_SIZE).contains(&size) {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalArgumentException;",
            Some("Max size is 1024"),
        ));
    }
    let memfd = acos_parcel::ParcelFd::memfd("MemoryIntArray")
        .map_err(|e| VmError::Internal(format!("MemoryIntArray.nativeCreate: {e}")))?;
    let owned = memfd.into_owned();
    let fd = owned.as_raw_fd();
    let file = File::from(owned);
    file.set_len(size as u64 * 4)
        .map_err(|e| VmError::Internal(format!("MemoryIntArray.nativeCreate ftruncate: {e}")))?;
    CREATED.with(|c| c.borrow_mut().insert(fd, file));
    vm.ret_i32(ctx, fd);
    Ok(())
}

fn native_open(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = vm.arg_i32(ctx, 0);
    // The handle is only a non-null marker for the Java field `mMemoryAddr`;
    // actual accesses go through `fd`.
    let handle = i64::from(fd) + 1;
    vm.ret_i64(ctx, handle);
    Ok(())
}

fn native_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = vm.arg_i32(ctx, 0);
    let len = with_fd(fd, |file| file.metadata().map(|m| m.len()))? / 4;
    vm.ret_i32(ctx, len as i32);
    Ok(())
}

fn native_get(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = vm.arg_i32(ctx, 0);
    let index = vm.arg_i32(ctx, 2);
    let value = read_word(fd, index)?;
    vm.ret_i32(ctx, value);
    Ok(())
}

fn native_set(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = vm.arg_i32(ctx, 0);
    let index = vm.arg_i32(ctx, 2);
    let value = vm.arg_i32(ctx, 3);
    write_word(fd, index, value)
}

fn native_close(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = vm.arg_i32(ctx, 0);
    let ownership = vm.arg_bool(ctx, 2);
    if ownership {
        CREATED.with(|c| c.borrow_mut().remove(&fd));
    }
    Ok(())
}

pub(crate) fn install(vm: &mut Vm) {
    let c = "Landroid/util/MemoryIntArray;";
    vm.register_native(c, "nativeCreate", "(Ljava/lang/String;I)I", native_create);
    vm.register_native(c, "nativeOpen", "(IZ)J", native_open);
    vm.register_native(c, "nativeSize", "(I)I", native_size);
    vm.register_native(c, "nativeGet", "(IJI)I", native_get);
    vm.register_native(c, "nativeSet", "(IJII)V", native_set);
    vm.register_native(c, "nativeClose", "(IJZ)V", native_close);
}

/// Read one word through the same backing store the natives use (tests).
#[cfg(test)]
#[must_use]
pub fn test_read_word(fd: RawFd, index: i32) -> Option<i32> {
    read_word(fd, index).ok()
}

/// Write one word through the same backing store the natives use (tests).
#[cfg(test)]
#[must_use]
pub fn test_write_word(fd: RawFd, index: i32, value: i32) -> bool {
    write_word(fd, index, value).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::OwnedFd;

    #[test]
    fn create_size_get_set_round_trip() {
        let memfd = acos_parcel::ParcelFd::memfd("memintarray-test").unwrap();
        let owned: OwnedFd = memfd.into_owned();
        let fd = owned.as_raw_fd();
        // Keep the descriptor alive for the test.
        let file = File::from(owned);
        file.set_len(8).unwrap();
        assert!(test_write_word(fd, 0, 7));
        assert_eq!(test_read_word(fd, 0), Some(7));
        assert_eq!(test_read_word(fd, 1), Some(0));
        // Keep `file` alive until here.
        drop(file);
    }
}
