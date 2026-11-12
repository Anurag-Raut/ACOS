//! The remaining `android.os` / `android.util` natives of the console+framework
//! tier: `Trace`, process identity and priority (`Process`), `EventLog`, and the
//! Binder-independent `GraphicsEnvironment` queries.
//!
//! Two classes of answer live here, and they are kept apart on purpose:
//!
//! * **Real values** — pid, tid, uid, the `ro.debuggable` flag, the VM-side
//!   thread priority table.
//! * **Honest "not supported"** — systrace/Perfetto is not implemented, so
//!   `Trace.nativeGetEnabledTags()` returns `0` (all tags disabled, i.e. every
//!   `Trace.isTagEnabled` is false) instead of pretending to trace. The
//!   `GraphicsEnvironment` setters configure HWUI/ANGLE, which ACOS does not
//!   have, so they are accepted and dropped; the queries answer truthfully
//!   (`getShouldUseAngle` → false, `setInjectLayersPrSetDumpable` → false).
//!
//! `Process.myPid()`/`myTid()`/`myUid()` are *not* natives in the framework
//! bytecode — they call `android.system.Os.getpid/gettid/getuid`, which is
//! `libcore.io.Linux` (registered by `natives::io`). ACOS's app uid (10192) and
//! the kernel tid therefore have to be supplied by re-registering those three
//! `Linux` natives here, where the process identity lives.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::natives::os_log::current_tid;
use crate::natives::os_props;
use crate::vm::Vm;

/// The uid of the ACOS app process (a normal Android "u0_a192" app uid).
/// ACOS has no `zygote`/`installd`, so the runtime fixes it at boot; the value
/// is what the framework expects of a non-root, non-system app.
pub const APP_UID: i32 = 10192;

/// `Process.THREAD_PRIORITY_DEFAULT`.
pub const THREAD_PRIORITY_DEFAULT: i32 = 0;

/// `Process.getUidForName` / `getGidForName` name table.
///
/// Android resolves names through `getpwnam`, where only the AID names exist;
/// application uids have no passwd entry either, so an unknown name must return
/// `-1` rather than a made-up uid.
pub fn uid_for_name(name: &str) -> i32 {
    match name {
        "root" => 0,
        "system" => 1000,
        "radio" => 1001,
        "bluetooth" => 1002,
        "wifi" => 1010,
        "media" => 1013,
        "graphics" => 1023,
        "shell" => 2000,
        "nobody" => 9999,
        // The ACOS app uid, in the `u0_a<uid-10000>` naming the platform uses.
        "u0_a192" => APP_UID,
        _ => -1,
    }
}

fn priorities() -> &'static Mutex<HashMap<u32, i32>> {
    static PRIORITIES: OnceLock<Mutex<HashMap<u32, i32>>> = OnceLock::new();
    PRIORITIES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn priority_lock() -> MutexGuard<'static, HashMap<u32, i32>> {
    priorities().lock().unwrap_or_else(|p| p.into_inner())
}

/// Recorded VM-side priority for one thread (`THREAD_PRIORITY_DEFAULT` = 0).
pub fn thread_priority(tid: u32) -> i32 {
    priority_lock()
        .get(&tid)
        .copied()
        .unwrap_or(THREAD_PRIORITY_DEFAULT)
}

/// Record a VM-side thread priority. ACOS's scheduler is cooperative, so this is
/// explicitly advisory: it never changes the host thread's `nice` value.
pub fn set_thread_priority(tid: u32, priority: i32) {
    priority_lock().insert(tid, priority);
}

/// `GraphicsEnvironment.isDebuggable()` — follows the image's build type.
pub fn is_debuggable() -> bool {
    os_props::parse_i32(os_props::get("ro.debuggable").as_deref(), 0) != 0
}

fn app_tracing_allowed() -> &'static AtomicBool {
    static ALLOWED: AtomicBool = AtomicBool::new(false);
    &ALLOWED
}

fn tracing_enabled() -> &'static AtomicBool {
    static ENABLED: AtomicBool = AtomicBool::new(false);
    &ENABLED
}

/// `Trace.nativeGetEnabledTags()`.
///
/// ACOS does not implement systrace/Perfetto, so no tag is enabled and the
/// framework takes its untraced fast path. (`research/service-layer/natives_plan.tsv`
/// suggests `0x2a` "for compat"; enabling tags there would make the framework
/// build trace sections that nobody records, so it is deliberately 0.)
pub fn enabled_tags() -> i64 {
    0
}

/// Whether `Trace.setAppTracingAllowed` was ever called with `true`.
pub fn is_app_tracing_allowed() -> bool {
    app_tracing_allowed().load(Ordering::Relaxed)
}

/// Whether `Trace.setTracingEnabled` was ever called with `true`.
pub fn is_tracing_enabled() -> bool {
    tracing_enabled().load(Ordering::Relaxed)
}

pub(crate) fn install(vm: &mut Vm) {
    trace(vm);
    process(vm);
    event_log(vm);
    graphics_environment(vm);
    os_bridge(vm);
    // `Activity.getDlWarning()` (native, `android_app_Activity.cpp`): the
    // dynamic linker's accumulated warnings. Stock `performStart` calls it only
    // for debuggable apps (W31 made `FLAG_DEBUGGABLE` truthful). The frozen VM
    // has no bionic linker, so there are no warnings: stock returns null then.
    vm.register_native(
        "Landroid/app/Activity;",
        "getDlWarning",
        "()Ljava/lang/String;",
        activity_dl_warning,
    );
}

fn activity_dl_warning(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_ref(ctx, crate::heap::NULL);
    Ok(())
}

// ---------------------------------------------------------------------------
// android.os.Trace
// ---------------------------------------------------------------------------

fn trace(vm: &mut Vm) {
    let c = "Landroid/os/Trace;";
    vm.register_native(c, "nativeGetEnabledTags", "()J", trace_enabled_tags);
    vm.register_native(
        c,
        "nativeSetAppTracingAllowed",
        "(Z)V",
        trace_set_app_tracing_allowed,
    );
    vm.register_native(
        c,
        "nativeSetTracingEnabled",
        "(Z)V",
        trace_set_tracing_enabled,
    );
    for (name, sig) in [
        ("nativeTraceBegin", "(JLjava/lang/String;)V"),
        ("nativeTraceEnd", "(J)V"),
        ("nativeAsyncTraceBegin", "(JLjava/lang/String;I)V"),
        ("nativeAsyncTraceEnd", "(JLjava/lang/String;I)V"),
        (
            "nativeAsyncTraceForTrackBegin",
            "(JLjava/lang/String;Ljava/lang/String;I)V",
        ),
        ("nativeAsyncTraceForTrackEnd", "(JLjava/lang/String;I)V"),
        ("nativeInstant", "(JLjava/lang/String;)V"),
        ("nativeInstantForTrack", "(JLjava/lang/String;Ljava/lang/String;)V"),
        ("nativeTraceCounter", "(JLjava/lang/String;J)V"),
    ] {
        vm.register_native(c, name, sig, trace_noop);
    }
}

fn trace_enabled_tags(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, enabled_tags());
    Ok(())
}

fn trace_set_app_tracing_allowed(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    app_tracing_allowed().store(vm.arg_bool(ctx, 0), Ordering::Relaxed);
    Ok(())
}

fn trace_set_tracing_enabled(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    tracing_enabled().store(vm.arg_bool(ctx, 0), Ordering::Relaxed);
    Ok(())
}

/// Every `Trace` mark is accepted and dropped: there is no trace sink.
fn trace_noop(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// android.os.Process
// ---------------------------------------------------------------------------

fn process(vm: &mut Vm) {
    let c = "Landroid/os/Process;";
    vm.register_native(c, "setThreadPriority", "(I)V", process_set_thread_priority);
    vm.register_native(
        c,
        "setThreadPriority",
        "(II)V",
        process_set_thread_priority_for,
    );
    vm.register_native(
        c,
        "getThreadPriority",
        "(I)I",
        process_get_thread_priority,
    );
    vm.register_native(c, "getUidForName", "(Ljava/lang/String;)I", process_uid_for_name);
    vm.register_native(c, "setArgV0Native", "(Ljava/lang/String;)V", process_set_argv0);
}

/// The process name recorded by `Process.setArgV0` (`ActivityThread` sets it to
/// the app package). ACOS has no separate process identity to rename, so the
/// value is recorded and exposed for debugging instead of mutating the host
/// process's `argv[0]`.
fn process_name() -> &'static Mutex<String> {
    static NAME: OnceLock<Mutex<String>> = OnceLock::new();
    NAME.get_or_init(|| Mutex::new(String::new()))
}

fn process_set_argv0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name = vm.string_to_rust(vm.arg_ref(ctx, 0)).unwrap_or_default();
    *process_name().lock().unwrap_or_else(|p| p.into_inner()) = name;
    Ok(())
}

/// The process name last set by `Process.setArgV0` (empty until set).
#[must_use]
pub fn arg_v0() -> String {
    process_name().lock().unwrap_or_else(|p| p.into_inner()).clone()
}

fn process_set_thread_priority(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let priority = vm.arg_i32(ctx, 0);
    set_thread_priority(current_tid(), priority);
    Ok(())
}

fn process_set_thread_priority_for(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let tid = vm.arg_i32(ctx, 0) as u32;
    let priority = vm.arg_i32(ctx, 1);
    set_thread_priority(tid, priority);
    Ok(())
}

fn process_get_thread_priority(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let tid = vm.arg_i32(ctx, 0) as u32;
    vm.ret_i32(ctx, thread_priority(tid));
    Ok(())
}

fn process_uid_for_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_ref(ctx, 0);
    let name = vm.string_to_rust(h).unwrap_or_default();
    vm.ret_i32(ctx, uid_for_name(&name));
    Ok(())
}

/// `android.system.Os.getpid/gettid/getuid` → `libcore.io.Linux`, plus the
/// gid/euid aliases. These re-registrations make `Process.myUid()` report
/// [`APP_UID`] (not root) and `Process.myTid()` the kernel thread id (not the
/// process id). Replacing the `natives::io` entries is deliberate: process
/// identity belongs to the process, not to the syscall shim.
fn os_bridge(vm: &mut Vm) {
    let c = "Llibcore/io/Linux;";
    vm.register_native(c, "getuid", "()I", os_getuid);
    vm.register_native(c, "geteuid", "()I", os_getuid);
    vm.register_native(c, "getgid", "()I", os_getgid);
    vm.register_native(c, "getegid", "()I", os_getgid);
    vm.register_native(c, "gettid", "()I", os_gettid);
}

fn os_getuid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, APP_UID);
    Ok(())
}

fn os_getgid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, APP_UID);
    Ok(())
}

fn os_gettid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, current_tid() as i32);
    Ok(())
}

// ---------------------------------------------------------------------------
// android.util.EventLog
// ---------------------------------------------------------------------------

fn event_log(vm: &mut Vm) {
    let c = "Landroid/util/EventLog;";
    // `writeEvent` returns the number of bytes it wrote into the event log
    // buffer. ACOS has no event log, so nothing is written and the answer is 0
    // (`readEvents`/`readEventsOnWrapping` stay unbound → UNSUPPORTED_JAVA_API).
    for sig in [
        "(IF)I",
        "(II)I",
        "(IJ)I",
        "(ILjava/lang/String;)I",
        "(I[Ljava/lang/Object;)I",
    ] {
        vm.register_native(c, "writeEvent", sig, event_log_write);
    }
}

fn event_log_write(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // Nothing is written, so the byte count is 0.
    vm.ret_i32(ctx, 0);
    Ok(())
}

// ---------------------------------------------------------------------------
// android.os.GraphicsEnvironment
// ---------------------------------------------------------------------------

fn graphics_environment(vm: &mut Vm) {
    let c = "Landroid/os/GraphicsEnvironment;";
    vm.register_native(c, "isDebuggable", "()Z", ge_is_debuggable);
    vm.register_native(
        c,
        "getShouldUseAngle",
        "(Ljava/lang/String;)Z",
        ge_should_use_angle,
    );
    vm.register_native(
        c,
        "setInjectLayersPrSetDumpable",
        "()Z",
        ge_set_inject_layers_pr_set_dumpable,
    );
    for (name, sig) in [
        ("hintActivityLaunch", "()V"),
        ("nativeToggleAngleAsSystemDriver", "(Z)V"),
        (
            "setAngleInfo",
            "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;)V",
        ),
        ("setDebugLayers", "(Ljava/lang/String;)V"),
        ("setDebugLayersGLES", "(Ljava/lang/String;)V"),
        (
            "setDriverPathAndSphalLibraries",
            "(Ljava/lang/String;Ljava/lang/String;)V",
        ),
        (
            "setGpuStats",
            "(Ljava/lang/String;Ljava/lang/String;JJLjava/lang/String;I)V",
        ),
        ("setLayerPaths", "(Ljava/lang/ClassLoader;Ljava/lang/String;)V"),
    ] {
        vm.register_native(c, name, sig, ge_config_accepted);
    }
}

fn ge_is_debuggable(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, is_debuggable());
    Ok(())
}

/// ACOS renders with its own software rasteriser; it never selects ANGLE.
fn ge_should_use_angle(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, false);
    Ok(())
}

/// "Inject layers per set-dumpable" is a HWUI debug feature ACOS does not have.
fn ge_set_inject_layers_pr_set_dumpable(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, false);
    Ok(())
}

/// Renderer configuration (GL driver paths, layer debug flags, ANGLE info, GPU
/// stats) is accepted and dropped: it only configures HWUI, which ACOS replaces.
fn ge_config_accepted(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_reports_disabled() {
        assert_eq!(enabled_tags(), 0, "no systrace/Perfetto sink in ACOS");
        assert!(!is_app_tracing_allowed());
        assert!(!is_tracing_enabled());
        assert!(current_tid() > 0);
    }

    #[test]
    fn uid_and_name_table_match_android_aids() {
        assert_eq!(APP_UID, 10192);
        assert_eq!(uid_for_name("root"), 0);
        assert_eq!(uid_for_name("system"), 1000);
        assert_eq!(uid_for_name("shell"), 2000);
        assert_eq!(uid_for_name("u0_a192"), APP_UID);
        assert_eq!(
            uid_for_name("dev.acos.fixtures.helloviews"),
            -1,
            "app names are not passwd entries; ART returns -1"
        );
        assert_eq!(uid_for_name(""), -1);
    }

    #[test]
    fn thread_priorities_are_vm_side_and_default_to_zero() {
        let tid = 4_242_424;
        assert_eq!(thread_priority(tid), THREAD_PRIORITY_DEFAULT);
        set_thread_priority(tid, -8);
        assert_eq!(thread_priority(tid), -8);
        set_thread_priority(tid, THREAD_PRIORITY_DEFAULT);
        assert_eq!(thread_priority(tid), THREAD_PRIORITY_DEFAULT);
    }

    #[test]
    fn debuggable_follows_the_image_build_type() {
        // The seeded table comes from build.prop (ro.debuggable=1) or the
        // embedded ART capture (also 1); either way the reference image is
        // userdebug.
        assert_eq!(os_props::get("ro.debuggable").as_deref(), Some("1"));
        assert!(is_debuggable());
    }

    #[test]
    fn pid_and_tid_are_real() {
        let main_tid = current_tid();
        assert!(main_tid > 0);
        // A test thread is a real kernel thread, not the process id.
        let spawned = std::thread::spawn(current_tid).join().unwrap();
        assert!(spawned > 0);
        assert_ne!(spawned, std::process::id());
        assert_ne!(spawned, main_tid);
    }
}
