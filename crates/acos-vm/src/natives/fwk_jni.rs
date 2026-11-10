//! Framework JNI-load boundary and the statsd sink (milestone M2).
//!
//! Two framework-edge natives are needed to bring `ActivityThread` through
//! `handleBindApplication` on the real Android 14 bytecode:
//!
//! * `java.lang.Runtime.nativeLoad(String, ClassLoader, Class)String` — the
//!   last step of `System.loadLibrary`. ACOS does not load ELF JNI libraries
//!   yet (ADR-0014); the framework libraries the M2 path touches
//!   (`libstats_jni`, `libandroid_runtime`, …) are served by Rust natives
//!   instead. Returning `null` means "loaded" to the framework, exactly as
//!   ART does when the library is found. A library whose methods are not
//!   supplied still surfaces as a structured `UNSUPPORTED_JAVA_API` the first
//!   time one of its natives is invoked, so nothing is silently faked.
//! * `android.util.StatsLog.writeImpl(byte[];II)V` — the native half of
//!   `StatsLog.write`. It hands a serialized statsd event to the platform
//!   daemon. There is no statsd in an ACOS process (ADR-0008 replaces
//!   system_server/zygote, and `StatsLog.<clinit>` itself is what pulls
//!   `libstats_jni` in). With no daemon attached the event is discarded, which
//!   is also what a device without statsd does. The app-visible compat/stats
//!   *decisions* are made in Java (`ChangeReporter`, `Compatibility`); only the
//!   fire-and-forget telemetry is dropped here, so no app API observes it.
//!
//! Both are declared divergences and are recorded in `results/m2/M2_LOG.md`.
//! Anything that would change observable app behaviour stays a structured
//! `UNSUPPORTED_JAVA_API`.

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::heap::NULL;
use crate::vm::Vm;

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    // `System.loadLibrary` → `Runtime.nativeLoad` (private static native).
    // A `null` return means success; a non-null `String` is the error message.
    vm.register_native(
        "Ljava/lang/Runtime;",
        "nativeLoad",
        "(Ljava/lang/String;Ljava/lang/ClassLoader;Ljava/lang/Class;)Ljava/lang/String;",
        jni_native_load,
    );
    // The statsd wire sink: no daemon, event discarded.
    vm.register_native(
        "Landroid/util/StatsLog;",
        "writeImpl",
        "([BII)V",
        statsd_write_impl,
    );
    Ok(())
}

/// Report a successful `dlopen` without loading an ELF object.
///
/// ACOS serves the framework's JNI surface from Rust. If a later call reaches a
/// native method with no Rust implementation, the VM raises
/// `UNSUPPORTED_JAVA_API` naming it, so a missing library is never hidden past
/// the point where its behaviour matters.
fn jni_native_load(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // With a `--native` launch, `acos-jni` has installed the real JNI loader
    // host: route `Runtime.nativeLoad` through it so the APK's `lib/<abi>/*.so`
    // resolve, map and run `JNI_OnLoad`. Without a host (the default), keep the
    // framework-edge behaviour: report the library as loaded and serve its
    // natives from Rust.
    if crate::natives::jni_glue::host_installed() {
        return crate::natives::jni_glue::load_library_impl(vm, ctx, true);
    }
    vm.ret_ref(ctx, NULL);
    Ok(())
}

/// Discard a serialized statsd event: no statsd daemon is attached.
fn statsd_write_impl(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}
