//! `libandroid` asset API (`AAssetManager`, `AAsset`, `AAssetDir`).
//!
//! Android's `AAsset*` functions are thin wrappers over the Java
//! `AssetManager`'s native `ApkAssets`. ACOS owns a real asset engine
//! (`acos-framework-natives::res`), so the loader forwards each call to the
//! installed [`NdkHost`](super::NdkHost) and the host answers from the
//! same APK/`ApkAssets` the framework uses. There is no placeholder file and no
//! empty buffer: a missing asset is `NULL`, a short read is the real byte count.

use std::os::raw::{c_char, c_int, c_void};

use super::{c_string, handle, handle_id, host_failed, with_host, NdkResult};

const LIB: &str = "libandroid";

/// `AASSET_MODE_UNKNOWN` — read the whole entry up front.
pub const AASSET_MODE_UNKNOWN: c_int = 0;
/// `AASSET_MODE_RANDOM`.
pub const AASSET_MODE_RANDOM: c_int = 1;
/// `AASSET_MODE_STREAMING`.
pub const AASSET_MODE_STREAMING: c_int = 2;
/// `AASSET_MODE_BUFFER` — read the whole entry up front.
pub const AASSET_MODE_BUFFER: c_int = 3;

fn name(p: *const c_char) -> Option<String> {
    // SAFETY: the guest passes a NUL-terminated string by contract.
    unsafe { c_string(p) }
}

/// `AAssetManager_fromJava(JNIEnv*, jobject)`.
///
/// # Safety
/// `env`/`manager` are guest JNI pointers; the host validates the object.
pub unsafe extern "C" fn aassetmanager_fromjava(
    env: *mut c_void,
    manager: *mut c_void,
) -> *mut c_void {
    match with_host(|h| h.asset_manager_from_java(env as usize, manager as usize)) {
        Ok(id) => handle(id),
        Err(e) => host_failed(LIB, "AAssetManager_fromJava", e, std::ptr::null_mut()),
    }
}

/// `AAssetManager_open(AAssetManager*, const char* filename, int mode)`.
///
/// # Safety
/// `manager` must come from [`aassetmanager_fromjava`]; `filename` is NUL-terminated.
pub unsafe extern "C" fn aassetmanager_open(
    manager: *mut c_void,
    filename: *const c_char,
    mode: c_int,
) -> *mut c_void {
    let (Some(mgr), Some(file)) = (handle_id(manager), name(filename)) else {
        return std::ptr::null_mut();
    };
    match with_host(|h| h.asset_open(mgr, &file, mode)) {
        Ok(id) => handle(id),
        Err(e) => host_failed(LIB, "AAssetManager_open", e, std::ptr::null_mut()),
    }
}

/// `AAssetManager_openDir(AAssetManager*, const char* dirName)`.
///
/// # Safety
/// `manager` must come from [`aassetmanager_fromjava`]; `dir` is NUL-terminated.
pub unsafe extern "C" fn aassetmanager_opendir(
    manager: *mut c_void,
    dir: *const c_char,
) -> *mut c_void {
    let (Some(mgr), Some(dir)) = (handle_id(manager), name(dir)) else {
        return std::ptr::null_mut();
    };
    match with_host(|h| h.asset_dir_open(mgr, &dir)) {
        Ok(id) => handle(id),
        Err(e) => host_failed(LIB, "AAssetManager_openDir", e, std::ptr::null_mut()),
    }
}

/// `AAsset_read(AAsset*, void* buf, size_t count)`.
///
/// # Safety
/// `buf` must be writable for `count` bytes.
pub unsafe extern "C" fn aasset_read(asset: *mut c_void, buf: *mut c_void, count: usize) -> c_int {
    let Some(id) = handle_id(asset) else {
        return -1;
    };
    if count == 0 {
        return 0;
    }
    if buf.is_null() {
        return -1;
    }
    let out = std::slice::from_raw_parts_mut(buf as *mut u8, count);
    match with_host(|h| h.asset_read(id, out)) {
        Ok(n) => n.min(c_int::MAX as usize) as c_int,
        Err(e) => host_failed(LIB, "AAsset_read", e, -1),
    }
}

/// `AAsset_seek64(AAsset*, off64_t offset, int whence)`.
///
/// # Safety
/// `asset` must be a live asset handle.
pub unsafe extern "C" fn aasset_seek(asset: *mut c_void, offset: i64, whence: c_int) -> i64 {
    match asset_seek_impl(asset, offset, whence) {
        NdkResult::Ok(v) => v,
        Err(e) => host_failed(LIB, "AAsset_seek", e, -1),
    }
}

/// `AAsset_seek(AAsset*, off_t, int)` — 32-bit bionic entry.
///
/// # Safety
/// `asset` must be a live asset handle.
pub unsafe extern "C" fn aasset_seek32(asset: *mut c_void, offset: i32, whence: c_int) -> i32 {
    match asset_seek_impl(asset, offset as i64, whence) {
        NdkResult::Ok(v) => v.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
        Err(e) => host_failed(LIB, "AAsset_seek", e, -1),
    }
}

fn asset_seek_impl(asset: *mut c_void, offset: i64, whence: c_int) -> NdkResult<i64> {
    let id = handle_id(asset).ok_or_else(|| "AAsset_seek: NULL asset".to_string())?;
    with_host(|h| h.asset_seek(id, offset, whence))
}

/// `AAsset_getLength64(AAsset*)`.
///
/// # Safety
/// `asset` must be a live asset handle.
pub unsafe extern "C" fn aasset_getlength64(asset: *mut c_void) -> i64 {
    let Some(id) = handle_id(asset) else { return 0 };
    match with_host(|h| h.asset_length(id)) {
        Ok(n) => n,
        Err(e) => host_failed(LIB, "AAsset_getLength64", e, 0),
    }
}

/// `AAsset_getLength(AAsset*)` (32-bit bionic entry).
///
/// # Safety
/// `asset` must be a live asset handle.
pub unsafe extern "C" fn aasset_getlength(asset: *mut c_void) -> i32 {
    aasset_getlength64(asset).clamp(0, i32::MAX as i64) as i32
}

/// `AAsset_getRemainingLength64(AAsset*)`.
///
/// # Safety
/// `asset` must be a live asset handle.
pub unsafe extern "C" fn aasset_getremaininglength64(asset: *mut c_void) -> i64 {
    let Some(id) = handle_id(asset) else { return 0 };
    match with_host(|h| h.asset_remaining(id)) {
        Ok(n) => n,
        Err(e) => host_failed(LIB, "AAsset_getRemainingLength64", e, 0),
    }
}

/// `AAsset_getRemainingLength(AAsset*)`.
///
/// # Safety
/// `asset` must be a live asset handle.
pub unsafe extern "C" fn aasset_getremaininglength(asset: *mut c_void) -> i32 {
    aasset_getremaininglength64(asset).clamp(0, i32::MAX as i64) as i32
}

/// `AAsset_getBuffer(AAsset*)` — the whole entry, valid until close.
///
/// # Safety
/// `asset` must be a live asset handle.
pub unsafe extern "C" fn aasset_getbuffer(asset: *mut c_void) -> *const c_void {
    let Some(id) = handle_id(asset) else {
        return std::ptr::null();
    };
    match with_host(|h| h.asset_buffer(id)) {
        Ok(p) => p,
        Err(e) => host_failed(LIB, "AAsset_getBuffer", e, std::ptr::null()),
    }
}

/// `AAsset_isAllocated(AAsset*)`.
///
/// # Safety
/// `asset` must be a live asset handle.
pub unsafe extern "C" fn aasset_isallocated(asset: *mut c_void) -> c_int {
    let Some(id) = handle_id(asset) else { return 0 };
    match with_host(|h| h.asset_is_allocated(id)) {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(e) => host_failed(LIB, "AAsset_isAllocated", e, 0),
    }
}

/// `AAsset_close(AAsset*)`.
///
/// # Safety
/// `asset` must be a live asset handle; it is invalid afterwards.
pub unsafe extern "C" fn aasset_close(asset: *mut c_void) {
    let Some(id) = handle_id(asset) else { return };
    if let Err(e) = with_host(|h| h.asset_close(id)) {
        host_failed::<()>(LIB, "AAsset_close", e, ());
    }
}

/// `AAssetManager_close(AAssetManager*)` — releases the Java-side manager.
///
/// The Java object owns the underlying `AssetManager`; the C handle is only a
/// bridge reference, so closing it while the Java object lives is a no-op that
/// matches the NDK's ref-counted contract.
///
/// # Safety
/// `manager` must be NULL or a handle from [`aassetmanager_fromjava`].
pub unsafe extern "C" fn aassetmanager_close(_manager: *mut c_void) {}

/// `AAssetDir_getNextFileName(AAssetDir*)`.
///
/// # Safety
/// `dir` must come from [`aassetmanager_opendir`]. The returned string is owned
/// by the `AAssetDir` until the next call/close.
pub unsafe extern "C" fn aassetdir_getnextfilename(dir: *mut c_void) -> *const c_char {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ffi::CString;

    thread_local! {
        static NAMES: RefCell<HashMap<u64, CString>> = RefCell::new(HashMap::new());
    }

    let Some(id) = handle_id(dir) else {
        return std::ptr::null();
    };
    match with_host(|h| h.asset_dir_next(id)) {
        Ok(Some(next)) => {
            let Ok(c) = CString::new(next) else {
                return std::ptr::null();
            };
            let ptr = c.as_ptr();
            NAMES.with(|n| n.borrow_mut().insert(id, c));
            ptr
        }
        Ok(None) => std::ptr::null(),
        Err(e) => host_failed(LIB, "AAssetDir_getNextFileName", e, std::ptr::null()),
    }
}

/// `AAssetDir_rewind(AAssetDir*)`.
///
/// # Safety
/// `dir` must be a live `AAssetDir`.
pub unsafe extern "C" fn aassetdir_rewind(dir: *mut c_void) {
    let Some(id) = handle_id(dir) else { return };
    if let Err(e) = with_host(|h| h.asset_dir_rewind(id)) {
        host_failed::<()>(LIB, "AAssetDir_rewind", e, ());
    }
}

/// `AAssetDir_close(AAssetDir*)`.
///
/// # Safety
/// `dir` must be a live `AAssetDir`; it is invalid afterwards.
pub unsafe extern "C" fn aassetdir_close(dir: *mut c_void) {
    let Some(id) = handle_id(dir) else { return };
    if let Err(e) = with_host(|h| h.asset_dir_close(id)) {
        host_failed::<()>(LIB, "AAssetDir_close", e, ());
    }
}

/// Table entries for the asset API.
pub fn table() -> Vec<(&'static str, *const c_void)> {
    vec![
        (
            "AAssetManager_fromJava",
            aassetmanager_fromjava as *const c_void,
        ),
        ("AAssetManager_open", aassetmanager_open as *const c_void),
        (
            "AAssetManager_openDir",
            aassetmanager_opendir as *const c_void,
        ),
        ("AAssetManager_close", aassetmanager_close as *const c_void),
        ("AAsset_read", aasset_read as *const c_void),
        ("AAsset_seek", aasset_seek32 as *const c_void),
        ("AAsset_seek64", aasset_seek as *const c_void),
        ("AAsset_getLength", aasset_getlength as *const c_void),
        ("AAsset_getLength64", aasset_getlength64 as *const c_void),
        (
            "AAsset_getRemainingLength",
            aasset_getremaininglength as *const c_void,
        ),
        (
            "AAsset_getRemainingLength64",
            aasset_getremaininglength64 as *const c_void,
        ),
        ("AAsset_getBuffer", aasset_getbuffer as *const c_void),
        ("AAsset_isAllocated", aasset_isallocated as *const c_void),
        ("AAsset_close", aasset_close as *const c_void),
        (
            "AAssetDir_getNextFileName",
            aassetdir_getnextfilename as *const c_void,
        ),
        ("AAssetDir_rewind", aassetdir_rewind as *const c_void),
        ("AAssetDir_close", aassetdir_close as *const c_void),
    ]
}
