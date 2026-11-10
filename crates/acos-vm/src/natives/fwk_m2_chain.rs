//! Small `ActivityThread` natives found by driving the real
//! `handleBindApplication` (M2 chain).
//!
//! Each native here is a genuine framework call site on the path from
//! `ApplicationThread.bindApplication` to `Application.onCreate`. They are
//! implemented with their real semantics for an interpreter that has no ART
//! heap and no heapprofd, and each is documented with the framework code that
//! calls it. Nothing here fakes a lifecycle callback: the M2 gate still reads
//! `ActivityThread.mInitialApplication` and the `AcosFixture` log.

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::heap::NULL;
use crate::vm::Vm;

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    vm.register_native(
        "Landroid/app/ActivityThread;",
        "nInitZygoteChildHeapProfiling",
        "()V",
        n_init_zygote_child_heap_profiling,
    );
    vm.register_native(
        "Lcom/android/internal/os/ClassLoaderFactory;",
        "createClassloaderNamespace",
        "(Ljava/lang/ClassLoader;ILjava/lang/String;Ljava/lang/String;ZLjava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
        create_classloader_namespace,
    );
    Ok(())
}

/// `ActivityThread.nInitZygoteChildHeapProfiling()`.
///
/// `handleBindApplication` calls this once per app process. On a platform built
/// with heapprofd it arms the zygote child's heap profiler from
/// `persist.device_config.runtime_native_boot.profile_sampling_interval`; when
/// the profiler is not configured (the normal case) it is a no-op. ACOS has no
/// ART heap, no heapprofd and no device-config service, so there is nothing to
/// arm — the structural result matches the platform's own unprotected path.
/// Recorded in `results/m2/M2_LOG.md` rather than hidden.
fn n_init_zygote_child_heap_profiling(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

/// `ClassLoaderFactory.createClassloaderNamespace(...)`.
///
/// After building the real `PathClassLoader` (the framework class loader over
/// the staged APK DEX, `acos-runtime::apk_loader`), `createClassLoader` calls
/// this native to install the dynamic-linker namespace for the app's native
/// library search path; a non-null return is turned into an
/// `UnsatisfiedLinkError`.
///
/// ACOS has no dynamic linker and no dlopen namespaces: DEX classes come from
/// the real class loader and ACOS does not service `System.loadLibrary`. There
/// is nothing to install, so the truthful error string is `null` (success).
/// This does not hide a missing DEX/class-loader behaviour (the class loader is
/// the real framework one); a dlopen namespace would only be observable through
/// native library loading, which is not on the M2 path.
fn create_classloader_namespace(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_ref(ctx, NULL);
    Ok(())
}
