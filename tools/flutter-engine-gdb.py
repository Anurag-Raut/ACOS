#!/usr/bin/env python3
"""Out-of-process breakpoint driver for the raw-loaded Flutter engine (W119-A).

The engine is mapped by `acos-native-loader` with `MAP_ANONYMOUS`, so
`/proc/<pid>/maps` carries no path to match on and gdb cannot find
`libflutter.so` by name.  The harness child therefore publishes its load bias
(`W119A_BASE` line + `<scratch>/base.txt`), and this driver derives the base
from the *running* engine instead:

    base = *(uint64_t*)rsp - 0x559bf3

`0x559bf3` is the instruction after `call timerfd_settime@plt` in
`fml::MessageLoopAndroid::WakeUp`, so the first hit of our
`acos_timerfd_settime` shim identifies the engine copy that really executes.

That matters: **two copies of libflutter.so are mapped** (the harness's
`Loader::load` and the JNI `System.loadLibrary` path).  Breakpoints installed on
the first copy never fire; see `results/flutter/FIRST_FRAME.md` (W119-A §1).

Usage (child of `cargo test -p acos-jni --test flutter_hello`, run directly):

    ACOS_FLUTTER_W93A_CHILD=1 \
    ACOS_FLUTTER_W93A_LIBDIR=$PWD/.cache/flutter-1/lib-x86_64 \
    ACOS_FLUTTER_W93A_OUT=/tmp/acos-w119a-gdb \
    ACOS_FLUTTER_W93A_APK=$PWD/fixtures/build/apps/flutter-hello.apk \
    gdb --batch -x tools/flutter-engine-gdb.py -ex run --args \
        $CARGO_TARGET_DIR/debug/deps/flutter_hello-* \
        --exact flutter_child_worker --nocapture

Offsets are for the pinned engine in `.cache/flutter-1` (Flutter 3.47 x86_64,
build id fe9fabe9...).  They are *verified at runtime*: `read_engine_bytes`
prints the first bytes at each site so a mismatched build fails loudly instead
of silently reporting "breakpoint never hit".
"""

import os
import struct
import sys

import gdb

LOG_PATH = os.environ.get(
    "W119A_GDB_LOG", "/tmp/w119a-engine.log"
)
BIAS_FILE = os.environ.get("W119A_BIAS_FILE", "/tmp/acos-w119a-gdb/base.txt")

# (symbol offset, tag, cap) -- offsets from `nm`/objdump of the pinned engine.
SITES = [
    (0x559bb0, "WAKEUP", 40),        # fml::MessageLoopAndroid::WakeUp
    (0x555582, "REG", 80),           # MessageLoopTaskQueues::RegisterTask
    (0x554d92, "POST", 80),          # MessageLoopImpl::PostTask (entry)
    (0x554db4, "RUNEXP", 80),        # MessageLoopImpl::RunExpiredTasksNow
    (0x554e66, "CMP", 200),          # task_time vs captured now
    (0x555121, "PRIMARY", 80),       # invocation of the popped task
    (0x55518c, "SUB", 80),           # invocation of merged sub-queue tasks
    (0x9d0010, "FIRECB", 20),        # VsyncWaiter::FireCallback
    (0x9d04de, "FRAMETASK", 20),     # FireCallback::$_0 (frame task body)
    (0x9cfb88, "SECCB", 20),         # VsyncWaiter::ScheduleSecondaryCallback
    (0x5387d6, "AWAIT", 20),         # VsyncWaiterAndroid::AwaitVSync
    (0x9acdaa, "REQFRAME", 20),      # Animator::RequestFrame
    (0x9ad1c6, "AWCB", 20),          # Animator::AwaitVSync::$_0
    (0x9c3522, "ONANIMBEGIN", 20),   # Shell::OnAnimatorBeginFrame
    # W128-A: platform-view -> surface -> raster -> present, and the pointer
    # path.  These are the sites that decide whether a frame can be rasterised.
    (0x52fabf, "SURFJNI", 8),        # flutter::SurfaceCreated(JNIEnv*,..)
    (0x52fe6b, "SURFWIN", 8),        # flutter::SurfaceWindowChanged(JNIEnv*,..)
    (0x52b2d2, "NOTIFYCR", 8),       # PlatformViewAndroid::NotifyCreated::$_0()
    (0x52b5e0, "NSWC", 8),           # PlatformViewAndroid::NotifySurfaceWindowChanged::$_0()
    (0x52aee4, "CREATEREND", 8),     # PlatformViewAndroid::CreateRenderingSurface
    (0x529936, "SURFFACT", 8),       # AndroidSurfaceFactoryImpl::CreateSurface
    (0x5299b1, "RAPI", 8),           # after AndroidContext::RenderingApi() -> eax
    (0x539a10, "SW_SETWIN", 8),      # AndroidSurfaceSoftware::SetNativeWindow
    (0x5397c0, "GL_SETWIN", 8),      # AndroidSurfaceGLSkia::SetNativeWindow
    (0x539a60, "SW_ACQ", 8),         # AndroidSurfaceSoftware::AcquireBackingStore
    (0x539b36, "SW_PRES", 8),        # AndroidSurfaceSoftware::PresentBackingStore
    (0x9b6426, "RDRAW", 8),          # Rasterizer::Draw
    (0x9c384a, "ONADRAW", 8),        # Shell::OnAnimatorDraw
    (0x9db026, "RENDER", 40),        # FfiDispatcher<PlatformConfigurationNativeApi::Render>::Call
    (0x9fae5c, "ADDVIEW", 8),        # PlatformConfiguration::AddView
    (0x530571, "PTRJNI", 8),         # flutter::DispatchPointerDataPacket(JNIEnv*,..)
    (0x530592, "PTRBUF", 8),         # GetDirectBufferAddress returned: r15=buf, ebp=len
    (0x9c1f98, "PTRSHELL", 8),       # Shell::OnPlatformViewDispatchPointerDataPacket
    (0x9b3a5a, "DPD_DISPATCH", 8),   # DefaultPointerDataDispatcher::DispatchPacket
    (0x9af204, "DODISPATCH", 8),     # Engine::DoDispatchPacket
    (0x9b0237, "PTROUTSIZE", 8),     # converted record bytes (cmp $0x120)
    (0x9b02ca, "PTRDARTINVOKE", 8),  # tonic::DartInvoke(_dispatchPointerDataPacket)
    (0x9d482d, "DARTINVOKEFIELD", 8),# tonic::DartInvokeField (Dart object field)
    (0x5301b2, "SVJNI", 8),          # flutter::SetViewportMetrics(JNIEnv*,..)
    (0x9c1764, "SVSHELL", 8),        # Shell::OnPlatformViewSetViewportMetrics
    (0x9b64d2, "RDTRYWAIT", 12),     # Rasterizer::Draw: Semaphore::TryWait result
    (0x9b686f, "RDDROP", 12),        # Rasterizer::Draw: drop-frame path
    (0x9b556e, "DTS_UNSAFE", 12),    # Rasterizer::DrawToSurfacesUnsafe
    (0x9b5326, "DTS", 12),           # Rasterizer::DrawToSurfaces
    (0x5399ae, "SW_GPUSURF", 8),     # AndroidSurfaceSoftware::CreateGPUSurface
    (0x5515fc, "SW_ACQFRAME", 12),   # GPUSurfaceSoftware::AcquireFrame
    (0x872e74, "SF_SUBMIT", 12),     # SurfaceFrame::Submit
    (0x9b5609, "SURFV", 12),         # virtual call on the rasterizer surface
]

_S = {"base": None, "log": None, "installed": False, "sites": []}


def log(msg):
    if _S["log"] is None:
        _S["log"] = open(LOG_PATH, "w", buffering=1)
    _S["log"].write(msg + "\n")


def u64(addr):
    try:
        return int.from_bytes(
            bytes(gdb.selected_inferior().read_memory(addr, 8)), "little"
        )
    except gdb.error:
        return 0


def reg(name):
    return int(gdb.parse_and_eval("$" + name))


def closure_vtable(closure):
    """`std::function<void()>`: the `__f_` pointer lives at +0x20."""
    f = u64(closure + 0x20)
    return u64(f) if f else 0


class Site(gdb.Breakpoint):
    def __init__(self, spec, tag, cap):
        super().__init__(spec)
        self.tag = tag
        self.cap = cap
        self.n = 0

    def stop(self):
        self.n += 1
        if self.n > self.cap:
            return False
        tid = gdb.selected_thread().num
        base = _S["base"]
        if self.tag == "WAKEUP":
            log("WAKEUP %d tid=%d time=%d" % (self.n, tid, reg("rsi")))
        elif self.tag == "REG":
            cl = reg("rdx")
            v = closure_vtable(cl)
            log(
                "REG %d tid=%d queue=%d target=%d grade=%d vt=+0x%x"
                % (self.n, tid, reg("rsi"), reg("rcx"), reg("r8") & 0xFFFFFFFF,
                   (v - base) if v else 0)
            )
        elif self.tag == "POST":
            impl = reg("rdi")
            term = int.from_bytes(
                bytes(gdb.selected_inferior().read_memory(impl + 0x20, 1)), "little"
            )
            v = closure_vtable(reg("rsi"))
            log(
                "POST %d tid=%d impl=0x%x qid=%d terminated=%d target=%d vt=+0x%x"
                % (self.n, tid, impl, u64(impl + 0x18), term, reg("rdx"),
                   (v - base) if v else 0)
            )
        elif self.tag == "CMP":
            t = reg("r13")
            target = u64(t + 0x40)
            now = reg("rax")
            log(
                "CMP %d tid=%d target=%d now=%d delta_us=%d"
                % (self.n, tid, target, now, (target - now) // 1000)
            )
        elif self.tag in ("PRIMARY", "SUB"):
            if self.tag == "PRIMARY":
                obj = u64(reg("rsp") + 0xB0)
            else:
                obj = u64(reg("rbx") + 0x20)
            v = u64(obj) if obj else 0
            log(
                "%s %d tid=%d obj=0x%x vt=+0x%x"
                % (self.tag, self.n, tid, obj, (v - base) if v else 0)
            )
        elif self.tag == "SURFV":
            rdi = reg("rdi")
            vt = u64(rdi) if rdi > 0x1000 else 0
            tgt = u64(vt + 0x38) if vt else 0
            log("SURFV %d tid=%d this=0x%x vt=+0x%x slot38=+0x%x" % (
                self.n, tid, rdi, (vt - base) if vt else 0,
                (tgt - base) if tgt else 0))
        elif self.tag == "RDTRYWAIT":
            log("RDTRYWAIT %d tid=%d ok=%d" % (self.n, tid, reg("al") & 0xFF))
        elif self.tag == "SVJNI":
            log("SVJNI %d tid=%d view_or_shell=0x%x dpr=%d" % (
                self.n, tid, reg("rdx"), reg("xmm0") if False else 0))
        elif self.tag == "SVSHELL":
            m = reg("rdx")
            try:
                raw = bytes(gdb.selected_inferior().read_memory(m, 0x40))
                doubles = [
                    struct.unpack_from("<d", raw, i * 8)[0] for i in range(8)
                ]
            except gdb.error:
                doubles = [float("nan")] * 8
            log("SVSHELL %d tid=%d view_id=%d metrics=0x%x d=%r" % (
                self.n, tid, reg("rsi"), m, doubles))
        elif self.tag == "RAPI":
            log("RAPI %d tid=%d rendering_api=%d" % (self.n, tid, reg("eax")))
        elif self.tag == "PTRBUF":
            log("PTRBUF %d tid=%d buf=0x%x len=%d" % (
                self.n, tid, reg("r15"), reg("ebp")))
        elif self.tag == "DPD_DISPATCH":
            log("DPD_DISPATCH %d tid=%d this=0x%x ts=%d" % (
                self.n, tid, reg("rdi"), reg("rdx")))
        elif self.tag == "DODISPATCH":
            pkt = u64(reg("rsi"))
            log("DODISPATCH %d tid=%d packet=0x%x begin=0x%x end=0x%x" % (
                self.n, tid, pkt, u64(pkt), u64(pkt + 8)))
        elif self.tag == "PTROUTSIZE":
            log("PTROUTSIZE %d tid=%d bytes=%d records=%d" % (
                self.n, tid, reg("rax"), reg("rax") // 0x120))
        elif self.tag == "PTRDARTINVOKE":
            log("PTRDARTINVOKE %d tid=%d closure=0x%x" % (
                self.n, tid, reg("rdi")))
        elif self.tag == "DARTINVOKEFIELD":
            log("DARTINVOKEFIELD %d tid=%d obj=0x%x" % (
                self.n, tid, reg("rdi")))
        elif self.tag in ("RDTRYWAIT",):
            log("%s %d tid=%d ok=%d" % (self.tag, self.n, tid, reg("al") & 0xFF))
        elif self.tag in ("SURFJNI", "SURFWIN", "NOTIFYCR", "NSWC",
                          "CREATEREND", "SURFFACT", "SW_SETWIN", "GL_SETWIN",
                          "SW_ACQ", "SW_PRES", "RDRAW", "ONADRAW",
                          "ADDVIEW", "PTRJNI", "PTRSHELL", "RDDROP",
                          "DTS_UNSAFE", "DTS", "SW_GPUSURF", "SW_ACQFRAME",
                          "SF_SUBMIT"):
            extra = ""
            if self.tag == "SURFJNI":
                extra = " jni_env=0x%x shell=0x%x surface=0x%x" % (
                    reg("rdi"), reg("rdx"), reg("rcx"))
            elif self.tag in ("SURFWIN", "NOTIFYCR", "NSWC", "CREATEREND"):
                extra = " self=0x%x arg=0x%x" % (reg("rdi"), reg("rsi"))
            elif self.tag == "SW_SETWIN":
                extra = " self=0x%x window=0x%x" % (reg("rdi"), reg("rsi"))
            elif self.tag == "GL_SETWIN":
                extra = " self=0x%x window=0x%x" % (reg("rdi"), reg("rsi"))
            elif self.tag == "SW_ACQ":
                size = 0
                try:
                    size = int.from_bytes(
                        bytes(gdb.selected_inferior().read_memory(reg("rsi"), 8)),
                        "little")
                except gdb.error:
                    pass
                extra = " self=0x%x size=%dx%d" % (
                    reg("rdi"), size & 0xFFFFFFFF, (size >> 32) & 0xFFFFFFFF)
            elif self.tag == "ADDVIEW":
                extra = " view_id=%d metrics=0x%x" % (reg("rsi"), reg("rdx"))
            elif self.tag == "PTRJNI":
                extra = " shell=0x%x packet=0x%x count=%d" % (
                    reg("rdx"), reg("rcx"), reg("r8"))
            log("%s %d tid=%d%s" % (self.tag, self.n, tid, extra))
        else:
            log("%s %d tid=%d" % (self.tag, self.n, tid))
        return False


class DetectBase(gdb.Breakpoint):
    """Derive the *executing* engine copy from the shim's return address."""

    def __init__(self):
        super().__init__(
            "acos_native_loader::bionic::engine_gaps::acos_timerfd_settime"
        )

    def stop(self):
        if _S["installed"]:
            return False
        ret = u64(reg("rsp"))
        base = ret - 0x559BF3
        if base <= 0:
            return False
        _S["base"] = base
        _S["installed"] = True
        log("ENGINE_BASE 0x%x (from timerfd_settime return)" % base)
        # Fail loudly if the offsets do not match this engine build.
        for off, tag, cap in SITES:
            got = bytes(gdb.selected_inferior().read_memory(base + off, 4)).hex()
            log("SITE +0x%x %s bytes=%s" % (off, tag, got))
        for off, tag, cap in SITES:
            try:
                _S["sites"].append(Site("*0x%x" % (base + off), tag, cap))
            except gdb.error as e:
                log("INSTALL FAIL +0x%x %s: %s" % (off, tag, e))
        log("INSTALLED %d sites" % len(_S["sites"]))
        return False


DetectBase()


def _dump(_ev=None):
    log("DONE base=%s sites=%d" % (_S["base"], len(_S["sites"])))


gdb.events.exited.connect(_dump)
