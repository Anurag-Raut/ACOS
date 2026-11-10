//! `android.os.Parcel` native methods, backed by [`acos_parcel`].
//!
//! The real Android 14 framework bytecode (`core-oj`/`core-libart`/`core-icu4j`
//! plus the lazy `framework` jar) treats `android.os.Parcel` as a thin Java
//! wrapper around a native `Parcel*` pointer. In `--corelib=real` mode ACOS runs
//! that same bytecode, so every `native*` method declared by
//! `Landroid/os/Parcel;` must be supplied by Rust. The full set (51 methods,
//! enumerated from `.cache/boot/android-34/dex/framework/classes3.dex` with
//! `dexdump`/`acos-dex`) is:
//!
//! * lifecycle: `nativeCreate`, `nativeDestroy`, `nativeFreeBuffer`;
//! * size/position: `nativeDataSize`, `nativeDataAvail`, `nativeDataCapacity`,
//!   `nativeDataPosition`, `nativeSetDataPosition`, `nativeSetDataSize`,
//!   `nativeSetDataCapacity`;
//! * scalars/strings/arrays: `nativeWriteInt/Long/Float/Double/String8/String16/
//!   ByteArray/Blob` and the `nativeRead*` twins;
//! * marshal: `nativeMarshall`, `nativeUnmarshall`, `nativeAppendFrom`,
//!   `nativeCompareData`, `nativeCompareDataInRange`;
//! * binder/fd: `nativeWriteStrongBinder`, `nativeReadStrongBinder`,
//!   `nativeWriteFileDescriptor`, `nativeReadFileDescriptor`,
//!   `nativeMarkForBinder`, `nativeHasFileDescriptors[InRange]`;
//! * tokens/misc: `nativeWriteInterfaceToken`, `nativeEnforceInterface`,
//!   `nativeIsForRpc`, `nativePushAllowFds`, `nativeRestoreAllowFds`,
//!   `nativeMarkSensitive`, `nativeGetOpenAshmemSize`,
//!   `nativeReadCallingWorkSourceUid`, `nativeReplaceCallingWorkSourceUid`,
//!   `nativeSignalExceptionForError`, `getGlobalAllocCount`,
//!   `getGlobalAllocSize`.
//!
//! # Handle model
//!
//! `nativeCreate()` allocates a `u64` handle from a [`ParcelTable`] and returns
//! it as the Java `long mNativePtr`. Every other native resolves that handle to
//! a live [`acos_parcel::Parcel`] and delegates to the codec verified against
//! Android 34 `libbinder.so` (see `crates/acos-parcel/REPORT.md`). The table is
//! stored in a `thread_local!` because the ACOS VM is single-threaded: all
//! framework bytecode for one VM runs on one OS thread, so the store is
//! effectively *per-VM*, and parallel test VMs on different threads never share
//! a handle.
//!
//! # Binder transport plug point
//!
//! A strong binder cannot be expressed as plain bytes: Android writes a 24-byte
//! flat object whose union is a kernel handle. ACOS uses a process-local
//! [`BinderId`] instead. [`BinderObjectSink`] is the seam between this file and
//! the future Binder transport task (T-19):
//!
//! * on write, the native layer asks the sink to turn a Java `IBinder` handle
//!   into a stable [`BinderId`] (and back on read);
//! * the default sink keeps an in-memory `Handle <-> BinderId` map, so a binder
//!   written by Java and read back in the same VM round-trips;
//! * T-19 installs the real transport mapping with [`set_binder_sink`].
//!
//! File descriptors are handled the same way but inside `acos-parcel`: the
//! parcel owns a duplicate of every descriptor written to it, and
//! `nativeReadFileDescriptor` hands Java a fresh duplicate kept alive in the
//! per-thread store.
//!
//! Unsupported behaviour is never faked: unknown natives are bound to the
//! caller-visible `UNSUPPORTED_JAVA_API` path, and every codec failure becomes a
//! structured [`VmError`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};

use acos_parcel::binder::BinderId;
use acos_parcel::{Parcel, ParcelError, ParcelTable};

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, ArrayObj, Handle, Object, NULL};
use crate::vm::Vm;

// ---------------------------------------------------------------------------
// Binder transport plug point
// ---------------------------------------------------------------------------

/// Maps Java `android.os.IBinder` handles to process-local [`BinderId`]s and
/// back.
///
/// This is the seam the Binder transport task (T-19) plugs into. The default
/// implementation is an in-memory identity map; a real transport replaces it so
/// a proxy read from a parcel resolves to a live Java `IBinder`.
pub trait BinderObjectSink {
    /// Return the id to put on the wire for `binder`, or `None` to write a null
    /// binder. `NULL` is the Java null handle.
    fn binder_id(&mut self, binder: Handle) -> Option<BinderId>;

    /// Return the Java `IBinder` handle for `id`, or `None` when the transport
    /// has no object for it (the native layer then returns Java `null`).
    fn binder_object(&mut self, id: BinderId) -> Option<Handle>;
}

/// The default sink: an in-memory `Handle <-> BinderId` identity map.
#[derive(Default)]
struct DefaultBinderObjectSink {
    by_handle: HashMap<u32, BinderId>,
    by_id: HashMap<u64, u32>,
    next_id: u64,
}

impl BinderObjectSink for DefaultBinderObjectSink {
    fn binder_id(&mut self, binder: Handle) -> Option<BinderId> {
        if binder == NULL {
            return None;
        }
        if let Some(&id) = self.by_handle.get(&binder) {
            return Some(id);
        }
        let raw = self.next_id.max(1);
        self.next_id = raw.wrapping_add(1).max(1);
        let id = BinderId(raw);
        self.by_handle.insert(binder, id);
        self.by_id.insert(raw, binder);
        Some(id)
    }

    fn binder_object(&mut self, id: BinderId) -> Option<Handle> {
        self.by_id.get(&id.0).copied()
    }
}

// ---------------------------------------------------------------------------
// Per-VM (per OS thread) parcel store
// ---------------------------------------------------------------------------

struct ParcelStore {
    table: ParcelTable,
    sink: Box<dyn BinderObjectSink>,
    /// Duplicates handed to Java as `FileDescriptor.descriptor`. Kept alive for
    /// the process lifetime so a Java `FileDescriptor` never dangles.
    live_fds: Vec<OwnedFd>,
}

impl Default for ParcelStore {
    fn default() -> Self {
        Self {
            table: ParcelTable::new(),
            sink: Box::<DefaultBinderObjectSink>::default(),
            live_fds: Vec::new(),
        }
    }
}

thread_local! {
    static STORE: RefCell<ParcelStore> = RefCell::new(ParcelStore::default());
}

fn with_store<R>(f: impl FnOnce(&mut ParcelStore) -> R) -> R {
    STORE.with(|s| f(&mut s.borrow_mut()))
}

/// Install a Binder transport sink. Called by T-19 once the transport exists.
#[allow(dead_code)]
pub(crate) fn set_binder_sink(sink: Box<dyn BinderObjectSink>) {
    with_store(|s| s.sink = sink);
}

/// Map a `ParcelError` onto the structured VM error surface.
///
/// The ashmem blob form is a wire encoding ACOS does not implement; it is
/// reported as `UNSUPPORTED_JAVA_API` so callers can tell it apart from a
/// genuine codec bug.
fn perr(e: ParcelError) -> VmError {
    match e {
        ParcelError::AshmemUnsupported => {
            VmError::unsupported_api("android.os.Parcel ashmem blob", "parcel nativeReadBlob")
        }
        other => VmError::Internal(format!("parcel native: {other}")),
    }
}

/// Flatten a table operation whose closure itself returns a codec result:
/// `Result<Result<T, ParcelError>, ParcelError>` -> `VmResult<T>`.
fn flat<T>(r: Result<Result<T, ParcelError>, ParcelError>) -> VmResult<T> {
    match r {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) | Err(e) => Err(perr(e)),
    }
}

fn handle_arg(vm: &Vm, ctx: NativeCtx) -> u64 {
    vm.arg_i64(ctx, 0) as u64
}

fn new_byte_array(vm: &mut Vm, bytes: &[u8]) -> VmResult<Handle> {
    vm.alloc(Object::Array(ArrayObj {
        elem_desc: "B".to_string(),
        data: ArrayData::Byte(bytes.iter().map(|&b| b as i8).collect()),
    }))
}

fn byte_array_slice(vm: &Vm, h: Handle, offset: i32, len: i32) -> Option<Vec<u8>> {
    match vm.heap.get(h) {
        Some(Object::Array(ArrayObj {
            data: ArrayData::Byte(v),
            ..
        })) => {
            let offset = offset.max(0) as usize;
            let len = len.max(0) as usize;
            let end = offset.saturating_add(len).min(v.len());
            Some(v[offset.min(v.len())..end].iter().map(|&b| b as u8).collect())
        }
        _ => None,
    }
}

fn write_byte_array_into(vm: &mut Vm, h: Handle, bytes: &[u8]) -> bool {
    if let Some(Object::Array(ArrayObj {
        data: ArrayData::Byte(v),
        ..
    })) = vm.heap.get_mut(h)
    {
        if bytes.len() <= v.len() {
            for (i, &b) in bytes.iter().enumerate() {
                v[i] = b as i8;
            }
            return true;
        }
    }
    false
}

/// The integer fd stored in a `java.io.FileDescriptor` (Android 14 field
/// `descriptor`; older trees use `fd`).
fn fd_of(vm: &Vm, h: Handle) -> Option<RawFd> {
    if h == NULL {
        return None;
    }
    vm.get_instance_field_by_name(h, "descriptor")
        .or_else(|| vm.get_instance_field_by_name(h, "fd"))
        .map(|v| v as i32)
        .filter(|&fd| fd >= 0)
}

fn make_file_descriptor(vm: &mut Vm, raw: RawFd) -> VmResult<Handle> {
    let cid = vm.load_class("Ljava/io/FileDescriptor;")?;
    let obj = vm.alloc_instance(cid)?;
    vm.set_instance_field_by_name(obj, "descriptor", raw as u32);
    vm.set_instance_field_by_name(obj, "fd", raw as u32);
    Ok(obj)
}

fn compare_bytes(a: &[u8], b: &[u8]) -> i32 {
    for (x, y) in a.iter().zip(b.iter()) {
        if x != y {
            return i32::from(*x) - i32::from(*y);
        }
    }
    a.len() as i32 - b.len() as i32
}

// ---------------------------------------------------------------------------
// lifecycle / data position
// ---------------------------------------------------------------------------

fn native_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = with_store(|s| s.table.create());
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

fn native_destroy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let _ = with_store(|s| s.table.free(handle));
    Ok(())
}

fn native_free_buffer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    with_store(|s| s.table.with_parcel(handle, Parcel::recycle)).map_err(perr)
}

fn native_data_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let n = with_store(|s| s.table.with_parcel_shared(handle, Parcel::data_size)).map_err(perr)?;
    vm.ret_i32(ctx, n as i32);
    Ok(())
}

fn native_data_avail(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let n = with_store(|s| s.table.with_parcel_shared(handle, Parcel::data_avail)).map_err(perr)?;
    vm.ret_i32(ctx, n as i32);
    Ok(())
}

fn native_data_capacity(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let n = with_store(|s| s.table.with_parcel_shared(handle, Parcel::data_capacity)).map_err(perr)?;
    vm.ret_i32(ctx, n as i32);
    Ok(())
}

fn native_data_position(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let n = with_store(|s| s.table.with_parcel_shared(handle, Parcel::data_position)).map_err(perr)?;
    vm.ret_i32(ctx, n as i32);
    Ok(())
}

fn native_set_data_position(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let pos = vm.arg_i32(ctx, 1).max(0) as usize;
    with_store(|s| s.table.with_parcel(handle, |p| p.set_data_position(pos))).map_err(perr)
}

fn native_set_data_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let size = vm.arg_i32(ctx, 1).max(0) as usize;
    with_store(|s| s.table.with_parcel(handle, |p| p.set_data_size(size))).map_err(perr)
}

/// `setDataCapacity` is a pure allocator hint in libbinder. The Rust `Vec`
/// grows on demand, so this is a no-op; the value is still consumed so the
/// signature stays honest.
fn native_set_data_capacity(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = handle_arg(vm, ctx);
    let _ = vm.arg_i32(ctx, 1);
    Ok(())
}

// ---------------------------------------------------------------------------
// writers
// ---------------------------------------------------------------------------

fn native_write_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let v = vm.arg_i32(ctx, 1);
    with_store(|s| s.table.with_parcel(handle, |p| p.write_int32(v))).map_err(perr)?;
    vm.ret_i32(ctx, 0);
    Ok(())
}

fn native_write_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let v = vm.arg_i64(ctx, 1);
    with_store(|s| s.table.with_parcel(handle, |p| p.write_int64(v))).map_err(perr)?;
    vm.ret_i32(ctx, 0);
    Ok(())
}

fn native_write_float(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let v = vm.arg_f32(ctx, 1);
    with_store(|s| s.table.with_parcel(handle, |p| p.write_float(v))).map_err(perr)?;
    vm.ret_i32(ctx, 0);
    Ok(())
}

fn native_write_double(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let v = vm.arg_f64(ctx, 1);
    with_store(|s| s.table.with_parcel(handle, |p| p.write_double(v))).map_err(perr)?;
    vm.ret_i32(ctx, 0);
    Ok(())
}

fn native_write_string8(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let s = vm.string_to_rust(vm.arg_ref(ctx, 1));
    flat(with_store(|s_| s_.table.with_parcel(handle, |p| p.write_string8_opt(s.as_deref()))))
}

fn native_write_string16(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let s = vm.string_to_rust(vm.arg_ref(ctx, 1));
    flat(with_store(|s_| s_.table.with_parcel(handle, |p| p.write_string16_opt(s.as_deref()))))
}

fn native_write_byte_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let arr = vm.arg_ref(ctx, 1);
    let offset = vm.arg_i32(ctx, 2);
    let len = vm.arg_i32(ctx, 3);
    let bytes = byte_array_slice(vm, arr, offset, len).unwrap_or_default();
    flat(with_store(|s| s.table.with_parcel(handle, |p| p.write_byte_array(Some(&bytes)))))
}

/// `writeBlob` uses the inline form: `int32 len` then `int32 0` marker then the
/// bytes. (Android's ashmem form is out of scope; see `acos-parcel`.)
fn native_write_blob(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let arr = vm.arg_ref(ctx, 1);
    let offset = vm.arg_i32(ctx, 2);
    let len = vm.arg_i32(ctx, 3);
    let bytes = byte_array_slice(vm, arr, offset, len).unwrap_or_default();
    flat(with_store(|s| {
        s.table.with_parcel(handle, |p| {
            p.write_int32(bytes.len() as i32);
            p.write_blob(&bytes)
        })
    }))
}

fn native_write_strong_binder(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let binder = vm.arg_ref(ctx, 1);
    with_store(|s| {
        let id = s.sink.binder_id(binder);
        s.table.with_parcel(handle, |p| p.write_strong_binder_opt(id))
    })
    .map_err(perr)
}

fn native_write_file_descriptor(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let raw = fd_of(vm, vm.arg_ref(ctx, 1));
    flat(with_store(|s| {
        s.table.with_parcel(handle, |p| match raw {
            // `/proc/self/fd/N` yields a safe `BorrowedFd` for the dup; the
            // parcel then owns its own duplicate.
            Some(fd) => match std::fs::File::open(format!("/proc/self/fd/{fd}")) {
                Ok(file) => p.write_file_descriptor(file.as_fd()),
                Err(e) => Err(ParcelError::Fd(format!("open /proc/self/fd/{fd}: {e}"))),
            },
            None => Err(ParcelError::Fd("writeFileDescriptor on a null fd".to_string())),
        })
    }))
}

fn native_write_interface_token(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let descriptor = vm.string_to_rust(vm.arg_ref(ctx, 1)).unwrap_or_default();
    flat(with_store(|s| s.table.with_parcel(handle, |p| p.write_interface_token(&descriptor))))
}

// ---------------------------------------------------------------------------
// readers
// ---------------------------------------------------------------------------

fn native_read_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let v = with_store(|s| s.table.with_parcel(handle, Parcel::read_int32_lenient)).map_err(perr)?;
    vm.ret_i32(ctx, v);
    Ok(())
}

fn native_read_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let v = with_store(|s| s.table.with_parcel(handle, Parcel::read_int64_lenient)).map_err(perr)?;
    vm.ret_i64(ctx, v);
    Ok(())
}

fn native_read_float(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let v = with_store(|s| s.table.with_parcel(handle, Parcel::read_float_lenient)).map_err(perr)?;
    vm.ret_f32(ctx, v);
    Ok(())
}

fn native_read_double(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let v = with_store(|s| s.table.with_parcel(handle, Parcel::read_double_lenient)).map_err(perr)?;
    vm.ret_f64(ctx, v);
    Ok(())
}

fn native_read_string8(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let s = with_store(|s_| s_.table.with_parcel(handle, Parcel::read_string8_lenient)).map_err(perr)?;
    match s {
        Some(s) => {
            let h = vm.new_string(&s)?;
            vm.ret_ref(ctx, h);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn native_read_string16(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let s = with_store(|s_| s_.table.with_parcel(handle, Parcel::read_string16_lenient)).map_err(perr)?;
    match s {
        Some(s) => {
            let h = vm.new_string(&s)?;
            vm.ret_ref(ctx, h);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn native_create_byte_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let bytes = flat(with_store(|s| s.table.with_parcel(handle, Parcel::read_byte_array)))?;
    match bytes {
        Some(b) => {
            let h = new_byte_array(vm, &b)?;
            vm.ret_ref(ctx, h);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn native_read_byte_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let dest = vm.arg_ref(ctx, 1);
    let dest_len = vm.arg_i32(ctx, 2).max(0) as usize;
    let bytes = flat(with_store(|s| s.table.with_parcel(handle, Parcel::read_byte_array)))?;
    let ok = match bytes {
        Some(b) if b.len() == dest_len => write_byte_array_into(vm, dest, &b),
        _ => false,
    };
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn native_read_blob(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let len = with_store(|s| s.table.with_parcel(handle, Parcel::read_int32_lenient)).map_err(perr)?;
    if len < 0 {
        vm.ret_ref(ctx, NULL);
        return Ok(());
    }
    let bytes = flat(with_store(|s| s.table.with_parcel(handle, |p| p.read_blob(len as usize))))?;
    match bytes {
        Some(b) => {
            let h = new_byte_array(vm, &b)?;
            vm.ret_ref(ctx, h);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn native_read_strong_binder(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let id = flat(with_store(|s| s.table.with_parcel(handle, Parcel::read_strong_binder)))?;
    let obj = match id {
        Some(id) => with_store(|s| s.sink.binder_object(id)).unwrap_or(NULL),
        None => NULL,
    };
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn native_read_file_descriptor(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let owned = flat(with_store(|s| {
        s.table.with_parcel(handle, |p| {
            let raw = p.read_file_descriptor()?;
            Ok(raw.and_then(|raw| {
                p.borrowed_file_descriptor(raw)
                    .and_then(|b| b.try_clone_to_owned().ok())
            }))
        })
    }))?;
    match owned {
        Some(fd) => {
            let raw = fd.as_raw_fd();
            with_store(|s| s.live_fds.push(fd));
            let obj = make_file_descriptor(vm, raw)?;
            vm.ret_ref(ctx, obj);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// marshalling / append / compare
// ---------------------------------------------------------------------------

fn native_marshall(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let bytes = with_store(|s| s.table.with_parcel_shared(handle, Parcel::marshall)).map_err(perr)?;
    let h = new_byte_array(vm, &bytes)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn native_unmarshall(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let arr = vm.arg_ref(ctx, 1);
    let offset = vm.arg_i32(ctx, 2);
    let len = vm.arg_i32(ctx, 3);
    let bytes = byte_array_slice(vm, arr, offset, len).unwrap_or_default();
    with_store(|s| s.table.with_parcel(handle, |p| p.unmarshall(&bytes))).map_err(perr)
}

fn native_append_from(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let other = vm.arg_i64(ctx, 1) as u64;
    let offset = vm.arg_i32(ctx, 2).max(0) as usize;
    let len = vm.arg_i32(ctx, 3).max(0) as usize;
    let src = with_store(|s| s.table.with_parcel_shared(other, Clone::clone)).map_err(perr)?;
    flat(with_store(|s| s.table.with_parcel(handle, |p| p.append_from(&src, offset, len))))
}

fn native_compare_data(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let other = vm.arg_i64(ctx, 1) as u64;
    let a = with_store(|s| s.table.with_parcel_shared(handle, Parcel::marshall)).map_err(perr)?;
    let b = with_store(|s| s.table.with_parcel_shared(other, Parcel::marshall)).map_err(perr)?;
    vm.ret_i32(ctx, compare_bytes(&a, &b));
    Ok(())
}

fn native_compare_data_in_range(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let a_off = vm.arg_i32(ctx, 1).max(0) as usize;
    let other = vm.arg_i64(ctx, 2) as u64;
    let b_off = vm.arg_i32(ctx, 3).max(0) as usize;
    let len = vm.arg_i32(ctx, 4).max(0) as usize;
    let a = with_store(|s| s.table.with_parcel_shared(handle, Parcel::marshall)).map_err(perr)?;
    let b = with_store(|s| s.table.with_parcel_shared(other, Parcel::marshall)).map_err(perr)?;
    let a_end = a_off.saturating_add(len).min(a.len());
    let b_end = b_off.saturating_add(len).min(b.len());
    let equal = a.get(a_off.min(a.len())..a_end) == b.get(b_off.min(b.len())..b_end);
    vm.ret_bool(ctx, equal);
    Ok(())
}

// ---------------------------------------------------------------------------
// tokens / fds / misc
// ---------------------------------------------------------------------------

fn native_enforce_interface(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let descriptor = vm.string_to_rust(vm.arg_ref(ctx, 1)).unwrap_or_default();
    let result = with_store(|s| s.table.with_parcel(handle, |p| p.enforce_interface(&descriptor)));
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) | Err(e) => {
            Err(vm.throw_new("Ljava/lang/SecurityException;", Some(&e.to_string())))
        }
    }
}

fn native_has_file_descriptors(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let v = with_store(|s| s.table.with_parcel_shared(handle, Parcel::has_file_descriptors))
        .map_err(perr)?;
    vm.ret_bool(ctx, v);
    Ok(())
}

fn native_has_file_descriptors_in_range(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let offset = vm.arg_i32(ctx, 1).max(0) as usize;
    let len = vm.arg_i32(ctx, 2).max(0) as usize;
    let v = with_store(|s| {
        s.table
            .with_parcel_shared(handle, |p| p.has_file_descriptors_in_range(offset, len))
    })
    .map_err(perr)?;
    vm.ret_bool(ctx, v);
    Ok(())
}

fn native_is_for_rpc(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let v = with_store(|s| s.table.with_parcel_shared(handle, Parcel::is_for_rpc)).map_err(perr)?;
    vm.ret_bool(ctx, v);
    Ok(())
}

fn native_push_allow_fds(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let allow = vm.arg_bool(ctx, 1);
    let prev =
        with_store(|s| s.table.with_parcel(handle, |p| p.push_allow_fds(allow))).map_err(perr)?;
    vm.ret_bool(ctx, prev);
    Ok(())
}

fn native_restore_allow_fds(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let allow = vm.arg_bool(ctx, 1);
    with_store(|s| s.table.with_parcel(handle, |p| p.restore_allow_fds(allow))).map_err(perr)
}

fn native_mark_for_binder(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let binder = vm.arg_ref(ctx, 1);
    with_store(|s| {
        let id = s.sink.binder_id(binder);
        s.table.with_parcel(handle, |p| {
            if let Some(id) = id {
                p.mark_for_binder(id);
            }
        })
    })
    .map_err(perr)
}

fn native_mark_sensitive(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = handle_arg(vm, ctx);
    Ok(())
}

fn native_get_open_ashmem_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = handle_arg(vm, ctx);
    vm.ret_i64(ctx, 0);
    Ok(())
}

/// `readCallingWorkSourceUid` peeks at the second header word (offset 4)
/// without moving the read position. `Binder.UNSET_WORKSOURCE` is `-1`.
fn native_read_calling_work_source_uid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let uid = with_store(|s| {
        s.table.with_parcel(handle, |p| {
            if p.data_size() >= 8 {
                let saved = p.data_position();
                p.set_data_position(4);
                let uid = p.read_int32_lenient();
                p.set_data_position(saved);
                uid
            } else {
                -1
            }
        })
    })
    .map_err(perr)?;
    vm.ret_i32(ctx, uid);
    Ok(())
}

fn native_replace_calling_work_source_uid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = handle_arg(vm, ctx);
    let uid = vm.arg_i32(ctx, 1);
    let updated = with_store(|s| {
        s.table.with_parcel(handle, |p| {
            if p.data_size() < 8 {
                return false;
            }
            let saved = p.data_position();
            p.set_data_position(4);
            p.write_int32(uid);
            p.set_data_position(saved);
            true
        })
    })
    .map_err(perr)?;
    vm.ret_bool(ctx, updated);
    Ok(())
}

fn native_signal_exception_for_error(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let code = vm.arg_i32(ctx, 0);
    if code == 0 {
        return Ok(());
    }
    Err(vm.throw_new(
        "Ljava/lang/RuntimeException;",
        Some(&format!("Parcel error {code}")),
    ))
}

fn get_global_alloc_count(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, 0);
    Ok(())
}

fn get_global_alloc_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, 0);
    Ok(())
}

// ---------------------------------------------------------------------------
// registration
// ---------------------------------------------------------------------------

/// Register every `android.os.Parcel` native against the VM registry.
pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let c = "Landroid/os/Parcel;";

    // lifecycle / data position
    vm.register_native(c, "nativeCreate", "()J", native_create);
    vm.register_native(c, "nativeDestroy", "(J)V", native_destroy);
    vm.register_native(c, "nativeFreeBuffer", "(J)V", native_free_buffer);
    vm.register_native(c, "nativeDataSize", "(J)I", native_data_size);
    vm.register_native(c, "nativeDataAvail", "(J)I", native_data_avail);
    vm.register_native(c, "nativeDataCapacity", "(J)I", native_data_capacity);
    vm.register_native(c, "nativeDataPosition", "(J)I", native_data_position);
    vm.register_native(c, "nativeSetDataPosition", "(JI)V", native_set_data_position);
    vm.register_native(c, "nativeSetDataSize", "(JI)V", native_set_data_size);
    vm.register_native(c, "nativeSetDataCapacity", "(JI)V", native_set_data_capacity);

    // writers
    vm.register_native(c, "nativeWriteInt", "(JI)I", native_write_int);
    vm.register_native(c, "nativeWriteLong", "(JJ)I", native_write_long);
    vm.register_native(c, "nativeWriteFloat", "(JF)I", native_write_float);
    vm.register_native(c, "nativeWriteDouble", "(JD)I", native_write_double);
    vm.register_native(c, "nativeWriteString8", "(JLjava/lang/String;)V", native_write_string8);
    vm.register_native(c, "nativeWriteString16", "(JLjava/lang/String;)V", native_write_string16);
    vm.register_native(c, "nativeWriteByteArray", "(J[BII)V", native_write_byte_array);
    vm.register_native(c, "nativeWriteBlob", "(J[BII)V", native_write_blob);
    vm.register_native(
        c,
        "nativeWriteStrongBinder",
        "(JLandroid/os/IBinder;)V",
        native_write_strong_binder,
    );
    vm.register_native(
        c,
        "nativeWriteFileDescriptor",
        "(JLjava/io/FileDescriptor;)V",
        native_write_file_descriptor,
    );
    vm.register_native(
        c,
        "nativeWriteInterfaceToken",
        "(JLjava/lang/String;)V",
        native_write_interface_token,
    );

    // readers
    vm.register_native(c, "nativeReadInt", "(J)I", native_read_int);
    vm.register_native(c, "nativeReadLong", "(J)J", native_read_long);
    vm.register_native(c, "nativeReadFloat", "(J)F", native_read_float);
    vm.register_native(c, "nativeReadDouble", "(J)D", native_read_double);
    vm.register_native(c, "nativeReadString8", "(J)Ljava/lang/String;", native_read_string8);
    vm.register_native(c, "nativeReadString16", "(J)Ljava/lang/String;", native_read_string16);
    vm.register_native(c, "nativeCreateByteArray", "(J)[B", native_create_byte_array);
    vm.register_native(c, "nativeReadByteArray", "(J[BI)Z", native_read_byte_array);
    vm.register_native(c, "nativeReadBlob", "(J)[B", native_read_blob);
    vm.register_native(
        c,
        "nativeReadStrongBinder",
        "(J)Landroid/os/IBinder;",
        native_read_strong_binder,
    );
    vm.register_native(
        c,
        "nativeReadFileDescriptor",
        "(J)Ljava/io/FileDescriptor;",
        native_read_file_descriptor,
    );

    // marshalling / append / compare
    vm.register_native(c, "nativeMarshall", "(J)[B", native_marshall);
    vm.register_native(c, "nativeUnmarshall", "(J[BII)V", native_unmarshall);
    vm.register_native(c, "nativeAppendFrom", "(JJII)V", native_append_from);
    vm.register_native(c, "nativeCompareData", "(JJ)I", native_compare_data);
    vm.register_native(
        c,
        "nativeCompareDataInRange",
        "(JIJII)Z",
        native_compare_data_in_range,
    );

    // tokens / fds / misc
    vm.register_native(
        c,
        "nativeEnforceInterface",
        "(JLjava/lang/String;)V",
        native_enforce_interface,
    );
    vm.register_native(c, "nativeHasFileDescriptors", "(J)Z", native_has_file_descriptors);
    vm.register_native(
        c,
        "nativeHasFileDescriptorsInRange",
        "(JII)Z",
        native_has_file_descriptors_in_range,
    );
    vm.register_native(c, "nativeIsForRpc", "(J)Z", native_is_for_rpc);
    vm.register_native(c, "nativePushAllowFds", "(JZ)Z", native_push_allow_fds);
    vm.register_native(c, "nativeRestoreAllowFds", "(JZ)V", native_restore_allow_fds);
    vm.register_native(
        c,
        "nativeMarkForBinder",
        "(JLandroid/os/IBinder;)V",
        native_mark_for_binder,
    );
    vm.register_native(c, "nativeMarkSensitive", "(J)V", native_mark_sensitive);
    vm.register_native(c, "nativeGetOpenAshmemSize", "(J)J", native_get_open_ashmem_size);
    vm.register_native(
        c,
        "nativeReadCallingWorkSourceUid",
        "(J)I",
        native_read_calling_work_source_uid,
    );
    vm.register_native(
        c,
        "nativeReplaceCallingWorkSourceUid",
        "(JI)Z",
        native_replace_calling_work_source_uid,
    );
    vm.register_native(c, "nativeSignalExceptionForError", "(I)V", native_signal_exception_for_error);
    vm.register_native(c, "getGlobalAllocCount", "()J", get_global_alloc_count);
    vm.register_native(c, "getGlobalAllocSize", "()J", get_global_alloc_size);

    Ok(())
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_sink_round_trips_handles() {
        let mut sink = DefaultBinderObjectSink::default();
        let a = sink.binder_id(7).unwrap();
        let b = sink.binder_id(9).unwrap();
        assert_ne!(a, b);
        assert_eq!(sink.binder_id(7), Some(a));
        assert_eq!(sink.binder_object(a), Some(7));
        assert_eq!(sink.binder_object(b), Some(9));
        assert_eq!(sink.binder_id(NULL), None);
    }

    #[test]
    fn compare_bytes_matches_memcmp_order() {
        assert_eq!(compare_bytes(b"abc", b"abc"), 0);
        assert!(compare_bytes(b"abc", b"abd") < 0);
        assert!(compare_bytes(b"abc", b"ab") > 0);
    }
}
