#!/usr/bin/env python3
"""Generate natives_plan.tsv from the measured ART trace work-lists.

Inputs (read in place, never copied):
  research/dynamic-trace/data/natives_activity_launch.tsv
  research/dynamic-trace/data/natives_activity_interaction.tsv
  research/dynamic-trace/data/natives_println.tsv
Output:
  research/service-layer/natives_plan.tsv

The module/complexity/notes columns are hand-curated below; the counts and the
method set are measured. Re-run after a new trace to see what moved.
"""
import csv, os, re, sys

HERE = os.path.dirname(os.path.abspath(__file__))
DATA = os.path.join(HERE, "..", "dynamic-trace", "data")

# class -> (module, complexity, note)
PLAN = {
 # ---- core: object model, strings, class/reflection ----
 "java.lang.String": ("core.lang", "M", "String storage is a VM-native byte[]/char[]; these natives are the hot path (22,881 launch enters). Implement as direct heap ops, not by re-entering the interpreter."),
 "java.lang.StringFactory": ("core.lang", "S", "Allocation-only; needs the String mirror + array writes."),
 "java.lang.Object": ("core.lang", "M", "identityHashCodeNative must be a stable 32-bit id per object (NOT the handle); internalClone for arrays/cloneables; wait/notifyAll drive the monitor implementation (VM-owned, not host condvar-only)."),
 "java.lang.System": ("core.lang", "S", "arraycopy (overlap-safe, typed), currentTimeMillis/nanoTime (monotonic clock + fixed epoch offset for determinism), mapLibraryName."),
 "java.lang.Math": ("core.lang", "S", "Transcendentals must match ART/bionic libm bit-for-bit for differential tests; use Rust libm and pin the impl (see risk R-06)."),
 "java.lang.Float": ("core.lang", "S", "Bit reinterpretation only."),
 "java.lang.Double": ("core.lang", "S", "Bit reinterpretation only."),
 "java.lang.Character": ("core.icu", "M", "getTypeImpl/getDirectionalityImpl/isDigitImpl/... are ICU-backed in libcore. Route to the same ICU data the Java side uses (see text/ICU section) so Java ICU4J and Character agree."),
 "java.lang.Throwable": ("core.lang", "M", "nativeFillInStackTrace needs the VM frame stack (ADR-0009) to materialise StackTraceElement[]."),
 "java.lang.Class": ("core.lang", "M", "getNameNative/classForName/newInstance/getDeclared*Internal/getPublicFieldRecursive: reflection is the second-largest framework surface after graphics. Needs a real Method/Field/Constructor object model (heap-allocated mirrors, ART-compatible `artMethod`-like ids)."),
 "java.lang.VMClassLoader": ("core.lang", "S", "findLoadedClass over the class-loader chain."),
 "java.lang.reflect.Array": ("core.lang", "S", "createObjectArray -> typed array allocation."),
 "java.lang.reflect.Field": ("core.lang", "M", "get/set/annotations on the mirror model; jdk.internal.reflect method handles (nGetFloatMethod) use the same ids."),
 "java.lang.reflect.Method": ("core.lang", "M", "Native invoke entry (before_main: ART's own `Method.invoke` used to enter the app)."),
 "java.lang.reflect.Constructor": ("core.lang", "M", "newInstance0 (low-level constructor invoke)."),
 "dalvik.system.VMStack": ("core.lang", "S", "getStackClass2 from the VM frame stack."),
 # ---- core: unsafe / vm runtime / threads / refs ----
 "jdk.internal.misc.Unsafe": ("core.vmrt", "M", "Pointer-arithmetic ALU on heap offsets. Hotspot pattern: AndroidX/Compose use Unsafe for atomics (§interaction: 13 natives). Needs stable array-base/field offsets and atomic semantics (compareAndSet*, getAndSet*, park/unpark)."),
 "jdk.internal.misc.VM": ("core.vmrt", "S", "getNanoTimeAdjustment."),
 "dalvik.system.VMRuntime": ("core.vmrt", "M", "Allocation/accounting natives: newUnpaddedArray/newNonMovableArray/addressOf/registerNativeAllocation*/clampGrowthLimit/vmInstructionSet/registerAppInfo/getNotifyNativeInterval. addressOf + non-movable arrays are used for direct ByteBuffers and RenderNode/SurfaceControl jlongs -> ties into the object-pinning design."),
 "dalvik.system.DexFile": ("core.vmrt", "M", "openDexFileNative/isBackedByOatFile/verifyInBackgroundNative: ACOS answers from its own DEX loader; `isBackedByOatFile` must be false and `openDexFileNative` must return the ACOS dex cookie."),
 "dalvik.system.BaseDexClassLoader": ("core.vmrt", "S", "computeClassLoaderContextsNative -> synthetic class-loader context strings."),
 "dalvik.system.VMDebug": ("core.vmrt", "S", "startMethodTracingFd/stopMethodTracing: ACOS's own tracing (needed to reproduce the ART traces for differential tests)."),
 "com.android.internal.os.ClassLoaderFactory": ("core.vmrt", "S", "createClassloaderNamespace -> the app namespace; return the ACOS namespace id/name."),
 "java.lang.Thread": ("core.thread", "M", "nativeCreate/nativeGetStatus/isInterrupted/interrupted/interrupt0/yield. Threads are VM-scheduled (ADR-0009) but blocking natives must park a real host thread (MessageQueue.nativePollOnce, Object.wait, Thread.sleep)."),
 "java.lang.ref.Reference": ("core.thread", "M", "refersTo0/getReferent drive the GC reference queues; must respect the ACOS precise-GC write barriers."),
 "libcore.util.NativeAllocationRegistry": ("core.thread", "M", "applyFreeFunction: ties a Java long handle to a Rust object's lifetime (RenderNode/Paint/Path/Bitmap). Free must be idempotent and GC-safe."),
 "libcore.io.AsynchronousCloseMonitor": ("core.io", "S", "signalBlockedThreads on fd close."),
 "java.io.FileDescriptor": ("core.io", "S", "isSocket."),
 # ---- core: io / posix ----
 "libcore.io.Linux": ("core.io", "M", "Posix layer: open/read/write/mmap/stat/access/getuid/getpid/gettid/close/setenv/android_fdsan_exchange_owner_tag. Rust std + libc; every syscall returns errno as an ErrnoException, and fd ownership follows Android (FileDescriptor/CloseGuard)."),
 "libcore.io.Memory": ("core.io", "S", "peek/poke on raw addresses: must interoperate with VMRuntime.addressOf and direct ByteBuffers."),
 "java.io.UnixFileSystem": ("core.io", "S", "getBooleanAttributes0/list0 over the ACOS VFS view of the APK dir + app data dir."),
 "java.io.FileInputStream": ("core.io", "S", "available0."),
 "java.lang.UNIXProcess": ("core.io", "S", "forkAndExec/waitForProcessExit: only reachable via some diagnostics paths; implement as a structured `unsupported` first, then real fork/exec (agent use case: none on the hello-views path)."),
 "android.util.EventLog": ("core.os", "M", "writeEvent(int,Object[]): binary event log format to a per-process buffer; needed because framework code uses it for am/wm bookkeeping."),
 # ---- parcel / binder ----
 "android.os.Parcel": ("os.parcel", "M", "THE wire format. 27 distinct natives, 1,835 nativeReadInt at launch. Implement the Android 14 Parcel binary layout exactly (alignment, string16 UTF-16LE + NUL, UTF-8 string8 selection, binder strong/weak, file descriptors, appendFrom, allowFds, markForBinder, isForRpc). Verified by round-trip against the real Java write/read paths."),
 "android.os.Binder": ("os.parcel", "M", "getCallingUid/Pid, clearCallingIdentity/restore, setThreadStrictModePolicy, flushPendingCommands, getNativeBBinderHolder. Caller identity is a per-thread Rust stack (not a real kernel binder identity)."),
 "android.os.BinderProxy": ("os.parcel", "M", "transactNative(code,Parcel,Parcel,flags) + isBinderAlive: the single outgoing chokepoint (73 launch / 106 interaction); dispatches into the in-process Rust service table. No syscall."),
 "android.util.MemoryIntArray": ("os.parcel", "M", "ashmem-like shared int array (nativeOpen/nativeSize/nativeGet) used by ViewRootImpl for the frame stats/timeline; back it with a plain host mmap."),
 "android.os.MessageQueue": ("os.looper", "M", "nativeInit/nativePollOnce/nativeWake: the ONE blocking point of the runtime. Must implement epoll semantics (message wake fd + input fd + vr fd) so the process can sleep with no timers when idle."),
 "android.os.SystemClock": ("core.os", "S", "uptimeMillis/elapsedRealtime/uptimeNanos/elapsedRealtimeNanos: boot-anchored monotonic clock shared with the vsync generator."),
 "android.os.SystemProperties": ("core.os", "M", "native_get/native_get_int/native_get_long/native_get_boolean/native_find. Served from build.prop + an ACOS property table; `native_find` returns a stable handle (SystemProperties$Handle)."),
 "android.os.Trace": ("core.os", "S", "nativeGetEnabledTags/nativeSetAppTracingAllowed: return the ACOS trace tag bitmask (0x2a for compat, see text)."),
 "android.os.Log": ("core.os", "S", "println_native: writes to the ACOS log sink with ART's exact line prefix (`I/AcosFixture: msg`) so logcat-order tests work."),
 "android.util.Log": ("core.os", "S", "println_native/isLoggable."),
 "android.os.Process": ("core.os", "S", "setThreadPriority: advisory in ACOS (VM scheduling), must not fail."),
 "android.os.GraphicsEnvironment": ("render.env", "S", "isDebuggable/setLayerPaths/setGpuStats/hintActivityLaunch: report the ACOS renderer, not HWUI; isDebuggable follows the fixture build type."),
 # ---- resources ----
 "android.content.res.AssetManager": ("res", "M", "21 natives: getResourceValue/applyStyle/resolveAttrs/retrieveAttributes/openNonAsset/openXmlAsset/asset* + configuration. Backed by crates/acos-apk (arsc.rs) + an AssetManager state object. This is the gate for themes/styles and therefore for the whole view inflation path."),
 "android.content.res.ApkAssets": ("res", "M", "nativeLoad/nativeGetStringBlock/nativeIsUpToDate: load framework-res.apk and the app APK as ACOS asset sources; IsUpToDate can answer `true` from mtime+size."),
 "android.content.res.StringBlock": ("res", "S", "nativeGetString/nativeGetStyle/nativeGetSize over the arsc string pool (reuse crates/acos-apk)."),
 "android.content.res.XmlBlock": ("res", "M", "11 natives: a streaming AXML parser over AcosApk's axml.rs (nativeCreateParseState/nativeNext/nativeGetAttribute*). 226 next() calls at launch: it must be a real pull parser."),
 "android.util.PathParser": ("render.vector", "S", "Vector drawable path data parser (nCreatePathData/nParseStringForPath)."),
 "android.graphics.drawable.VectorDrawable": ("render.vector", "M", "15 natives: the framework's native VectorDrawable tree (only used for the app icon/theme background). Could be reduced to `unsupported` + a Rust-drawn icon; keep the option open."),
 # ---- render (acos-render mapping) ----
 "android.graphics.Canvas": ("render.canvas", "M", "10 natives -> acos_render::Canvas: nInitRaster/nSave/nRestore/nRestoreToCount/nTranslate/nScale/nClipRect/nGetClipBounds/nQuickReject/nSetCompatibilityVersion."),
 "android.graphics.BaseCanvas": ("render.canvas", "S", "nDrawRoundRect (legacy overload)."),
 "android.graphics.RecordingCanvas": ("render.canvas", "M", "nCreateDisplayListCanvas/nResetDisplayListCanvas/nFinishRecording/nDrawRenderNode/nEnableZ: a Canvas that records a DisplayOp list instead of rasterising -> acos_render::RenderNode::record."),
 "android.graphics.BaseRecordingCanvas": ("render.canvas", "M", "nDrawRect/nDrawText/nDrawRoundRect/nDrawNinePatch/nDrawTextRun: record-only ops."),
 "android.graphics.Paint": ("render.paint", "M", "43 natives (the largest single class): a Rust-side Paint object keyed by jlong. nInit/nSet*/nGet* + text metrics (nGetFontMetricsInt/nGetRunCharacterAdvance/nGetCharArrayBounds) which must go through acos-render's shaping (rustybuzz) to match Minikin."),
 "android.graphics.Path": ("render.path", "M", "nInit/nMoveTo/nLineTo/nCubicTo/nApproximate/nReset/nSetFillType/nGetFillType -> acos_render::Path."),
 "android.graphics.Matrix": ("render.matrix", "S", "nCreate/nCreate(copy)/nSetTranslate/nPostTranslate/nGetValues/nIsIdentity/nReset -> Matrix3x3."),
 "android.graphics.Region": ("render.region", "M", "nativeConstructor/nativeSetRect; needed for clip/region bookkeeping."),
 "android.graphics.RenderNode": ("render.node", "M", "30 natives: HWUI's display-list node. Map 1:1 to acos_render::RenderNode (properties + DisplayOp list). jlong is the handle; nCreate/nIsValid/nHasIdentityMatrix are on the hot path."),
 "android.graphics.animation.RenderNodeAnimator": ("render.anim", "M", "Native animator driving RenderNode properties on the render thread (nCreateCanvasPropertyFloatAnimator/nSetDuration/nStart/...). Drives the ripple/press animation in the interaction trace."),
 "android.graphics.animation.NativeInterpolatorFactory": ("render.anim", "S", "createLinearInterpolator/createPathInterpolator."),
 "android.graphics.CanvasProperty": ("render.anim", "S", "nCreateFloat/nCreatePaint."),
 "android.graphics.RuntimeShader": ("render.shader", "M", "AGSL runtime shader (nativeCreateBuilder/nativeCreateShader/nativeUpdateUniforms/nativeUpdateShader). The ripple uses it in the interaction trace. Options: (i) implement an AGSL interpreter, (ii) map the few known shaders to Rust, (iii) structured `unsupported` + flat draw. Recommend (iii) first, with (i) as a follow-up."),
 "android.graphics.BitmapShader": ("render.shader", "M", "nativeCreate(tile modes + matrix)."),
 "android.graphics.BlendModeColorFilter": ("render.paint", "S", "native_CreateBlendModeFilter -> ColorFilterSpec."),
 "android.graphics.PorterDuffColorFilter": ("render.paint", "S", "Legacy blend-mode filter."),
 "android.graphics.NinePatch": ("render.bitmap", "M", "isNinePatchChunk/validateNinePatchChunk: chunk format validation; drawing goes through nDrawNinePatch."),
 "android.graphics.Bitmap": ("render.bitmap", "M", "12 natives: nativeCreate/nativeGetAllocationByteCount/nativeRowBytes/nativeHasAlpha/nativeIsPremultiplied/nativeErase/nativeConfig/nativeGetNativeFinalizer -> acos_render::Bitmap (RGBA8 buffer, no GPU)."),
 "android.graphics.ImageDecoder": ("render.bitmap", "M", "6 natives: full ImageDecoder (nCreate/nGetMimeType/nGetColorSpace/nDecodeBitmap/nGetPadding/nClose). Needed only for bitmap assets; recommend a decode path via acos-render's image module and `unsupported` for exotic formats."),
 "android.util.MemoryIntArray2": ("os.parcel", "S", "placeholder"),
 "android.graphics.HardwareRenderer": ("render.hwui", "M", "30 natives: HWUI's render thread. ACOS has no HWUI/Skia GPU thread; map the *observable* contract (createRootRenderNode/nCreateProxy/nSetSurface/nSyncAndDrawFrame/nNotifyFramePending/nAllocateBuffers) onto an acos-render scene sync on a Rust render thread, and make the rest no-ops that keep the state machine consistent (nPause/nSetStopped/nIsDrawingEnabled)."),
 "android.graphics.HardwareRendererObserver": ("render.hwui", "M", "nCreateObserver/nGetNextBuffer: frame-timeline callbacks consumed by ViewRootImpl's FrameMetrics; must be driven by the same vsync/frame-completion events."),
 "android.graphics.Typeface": ("text.typeface", "M", "8 natives: nativeCreateFromTypeface/nativeAddFontCollections/nativeReadTypefaces/nativeRegisterGenericFamily/nativeSetDefault/nativeForceSetStaticFinalField/systemFontMap. Feeds acos_render::FontCollection from .cache/boot/android-34/fonts/fonts.xml (75 MiB of fonts, mmap; there is no system_fonts.xml in 14 any more)."),
 "android.graphics.text.LineBreaker": ("text.layout", "M", "8 natives: nInit/nComputeLineBreaks/nGetLine*/nGetLineFlag -> acos_render::LineBreaker (rustybuzz + unicode-bidi). Must produce ART-compatible break offsets/widths for the 420dpi metrics."),
 "android.graphics.text.MeasuredText$Builder": ("text.layout", "M", "nInitBuilder/nAddStyleRun/nBuildMeasuredText/nFreeBuilder: the measured-text intermediate used by LineBreaker."),
 "java.lang.String.icu": ("text.icu", "S", "placeholder"),
 "com.android.icu.charset.NativeConverter": ("text.icu", "M", "9 natives: a live ICU UConverter (openConverter/getMaxBytesPerChar/encode/decode callbacks). Every String<->byte[] conversion goes through it on Android. Two options: (a) reimplement the converter state machine in Rust (utf-8/utf-16/latin1/us-ascii cover ~all app traffic), (b) embed ICU4X converter data. Recommend (a) with a converter registry, `unusable` for the rest, and a differential test vs the ART trace for UTF-8/UTF-16."),
 "com.android.icu.util.LocaleNative": ("text.icu", "S", "setDefaultNative: only needs to record the locale for LocaleList/JNI."),
 "android.graphics.Paint2": ("render.paint", "S", "placeholder"),
 # ---- view: input ----
 "android.view.MotionEvent": ("view.input", "M", "16 natives over a Rust MotionEvent struct (pointer properties/coords/axes/raw axes/action/flags/event time). Must be bit-exact with ART's packing (offsets used by VelocityTracker/native code)."),
 "android.view.InputChannel": ("view.input", "M", "nativeReadFromParcel/nativeGetToken: the channel is an ACOS socketpair carrying InputMessage structs; the token is the binder handle the window manager uses to route input."),
 "android.view.InputEventReceiver": ("view.input", "M", "nativeInit(WeakReference,InputChannel,MessageQueue)/nativeFinishInputEvent: registers the receive fd with the message queue's epoll set -> dispatches Java on the UI thread."),
 "android.view.InputEventSender": ("view.input", "M", "nativeInit/nativeSendKeyEvent: the app->system direction (e.g. back key injection from the IME), used in the interaction trace."),
 "android.view.KeyEvent": ("view.input", "S", "nativeNextId."),
 "android.view.KeyCharacterMap": ("view.input", "M", "nativeReadFromParcel/nativeGetKeyboardType/nativeGetCharacter: a key char map; a single virtual keyboard (qwerty) suffices."),
 # ---- view: surfaces / display / vsync ----
 "android.view.SurfaceControl": ("view.surface", "M", "21 natives: the window system's handle layer. nativeCreateTransaction/nativeApplyTransaction/nativeMergeTransaction/nativeSetFlags/nativeSetAlpha/nativeSetPosition/nativeSetMatrix/nativeSetFrameTimelineVsync/nativeGetHandle/nativeReadFromParcel/nativeWriteTransactionToParcel. ACOS: SurfaceControl = (layer id, parent, rect, alpha, transform, visibility) in the Rust window compositor; transactions are Rust batches applied at frame commit."),
 "android.view.Surface": ("view.surface", "M", "nativeIsValid: a Surface wraps a producer/consumer pair (BLAST). In ACOS it wraps the acos-render target buffer."),
 "android.view.SurfaceSession": ("view.surface", "S", "nativeCreate (legacy root session handle)."),
 "android.graphics.BLASTBufferQueue": ("view.surface", "M", "5 natives: the buffer queue between the app render thread and the compositor. ACOS: a single-buffer/2-buffer ring in the Rust compositor plus frame-completion callbacks; nativeUpdate/nativeGetSurface/nativeMergeWithNextTransaction."),
 "android.view.DisplayEventReceiver": ("view.vsync", "M", "nativeInit (returns an fd registered with the Looper)/nativeScheduleVsync/nativeGetLatestVsyncEventData. ACOS owns the whole ABI (both sides are our natives), so only the Java-observable contract matters: fd readable at the vsync deadline + VsyncEventData fields (frame timestamps + frame interval + vsync id)."),
 "android.hardware.OverlayProperties": ("view.surface", "S", "nReadOverlayPropertiesFromParcel/nSupportFp16ForHdr/nSupportMixedColorSpaces: canned (no HDR overlays)."),
 "android.hardware.display.DisplayManagerGlobal": ("display", "S", "nSignalNativeCallbacks: display change callback bridge."),
 # ---- misc framework ----
 "android.app.ActivityThread": ("core.os", "S", "nInitZygoteChildHeapProfiling: no-op (no zygote heap profiling)."),
 "android.app.Activity": ("core.os", "S", "getDlWarning: returns null (no 'download manager' compat warning)."),
 "android.animation.PropertyValuesHolder": ("render.anim", "M", "nCallFloatMethod/nGetFloatMethod: reflective property setter dispatch used by animations; needs the Method id model and a fast native invoke."),
 "java.lang.Runtime": ("core.vmrt", "M", "nativeLoad(String,ClassLoader,Class): the app's native-library loader. Must go through the ACOS ELF loader (acos-native-loader) and the Rust JNI ABI instead of a blind dlopen of host libs."),
 "libcore.util.CharsetUtils": ("text.icu", "S", "toUtf8Bytes: fast path over the same converter/buffer model."),
 "com.android.internal.util.VirtualRefBasePtr": ("render.node", "S", "nIncStrong/nDecStrong: refcount of the Rust RenderNode/native object."),
}

DEFAULTS = ("core.other", "M", "Unclassified in the curated map - classify before implementing.")

# Per-method overrides. A method listed here overrides the class default.
# "L" = >1 day of agent work or a design decision is embedded in it.
L_METHODS = {
 ("android.os.Parcel", "nativeReadInt"), ("android.os.Parcel", "nativeWriteInt"),
 ("android.os.Parcel", "nativeCreate"), ("android.os.Parcel", "nativeFreeBuffer"),
 ("android.os.Parcel", "nativeReadString16"), ("android.os.Parcel", "nativeWriteString16"),
 ("android.os.Parcel", "nativeReadString8"), ("android.os.Parcel", "nativeWriteString8"),
 ("android.os.Parcel", "nativeReadStrongBinder"), ("android.os.Parcel", "nativeWriteStrongBinder"),
 ("android.os.Parcel", "nativeMarkForBinder"), ("android.os.Parcel", "nativeAppendFrom"),
 ("android.os.Parcel", "nativeReadFileDescriptor"), ("android.os.Parcel", "nativeHasFileDescriptors"),
 ("android.os.Parcel", "nativeEnforceInterface"), ("android.os.Parcel", "nativeWriteInterfaceToken"),
 ("android.os.BinderProxy", "transactNative"),
 ("android.os.Binder", "getNativeBBinderHolder"), ("android.os.Binder", "clearCallingIdentity"),
 ("android.os.MessageQueue", "nativePollOnce"), ("android.os.MessageQueue", "nativeInit"),
 ("java.lang.Class", "getNameNative"), ("java.lang.Class", "classForName"),
 ("java.lang.Class", "newInstance"), ("java.lang.Class", "getDeclaredMethodInternal"),
 ("java.lang.Class", "getDeclaredConstructorInternal"),
 ("java.lang.String", "charAt"), ("java.lang.String", "getCharsNoCheck"),
 ("java.lang.String", "fillBytesLatin1"), ("java.lang.String", "toCharArray"),
 ("java.lang.System", "arraycopy"),
 ("java.lang.ref.Reference", "refersTo0"), ("java.lang.ref.Reference", "getReferent"),
 ("java.lang.Thread", "nativeCreate"), ("java.lang.Object", "wait"),
 ("dalvik.system.VMRuntime", "newUnpaddedArray"), ("dalvik.system.VMRuntime", "newNonMovableArray"),
 ("dalvik.system.VMRuntime", "addressOf"), ("dalvik.system.VMRuntime", "registerNativeAllocation"),
 ("android.content.res.AssetManager", "nativeGetResourceValue"),
 ("android.content.res.AssetManager", "nativeApplyStyle"),
 ("android.content.res.AssetManager", "nativeResolveAttrs"),
 ("android.content.res.AssetManager", "nativeRetrieveAttributes"),
 ("android.content.res.AssetManager", "nativeOpenXmlAsset"),
 ("android.content.res.AssetManager", "nativeAssetRead"),
 ("android.content.res.AssetManager", "nativeSetApkAssets"),
 ("android.content.res.XmlBlock", "nativeCreateParseState"),
 ("android.content.res.XmlBlock", "nativeNext"),
 ("android.graphics.RenderNode", "nCreate"),
 ("android.graphics.HardwareRenderer", "nCreateProxy"),
 ("android.graphics.HardwareRenderer", "nSyncAndDrawFrame"),
 ("android.graphics.HardwareRenderer", "nCreateRootRenderNode"),
 ("android.graphics.HardwareRenderer", "nSetSurfaceControl"),
 ("android.graphics.HardwareRenderer", "nAllocateBuffers"),
 ("android.graphics.Paint", "nGetRunCharacterAdvance"),
 ("android.graphics.Paint", "nGetFontMetricsInt"),
 ("android.graphics.Paint", "nGetFontMetricsIntForText"),
 ("android.graphics.Paint", "nGetCharArrayBounds"),
 ("android.graphics.Paint", "nInit"),
 ("android.graphics.Typeface", "nativeReadTypefaces"),
 ("android.graphics.Typeface", "nativeAddFontCollections"),
 ("android.graphics.text.LineBreaker", "nComputeLineBreaks"),
 ("android.graphics.text.LineBreaker", "nInit"),
 ("android.graphics.text.MeasuredText$Builder", "nBuildMeasuredText"),
 ("android.view.SurfaceControl", "nativeCreateTransaction"),
 ("android.view.SurfaceControl", "nativeApplyTransaction"),
 ("android.view.SurfaceControl", "nativeReadFromParcel"),
 ("android.view.SurfaceControl", "nativeWriteTransactionToParcel"),
 ("android.view.SurfaceControl", "nativeMergeTransaction"),
 ("android.graphics.BLASTBufferQueue", "nativeUpdate"),
 ("android.graphics.BLASTBufferQueue", "nativeCreate"),
 ("android.graphics.BLASTBufferQueue", "nativeGetSurface"),
 ("android.view.DisplayEventReceiver", "nativeInit"),
 ("android.view.DisplayEventReceiver", "nativeScheduleVsync"),
 ("android.view.DisplayEventReceiver", "nativeGetLatestVsyncEventData"),
 ("android.view.MotionEvent", "nativeReadFromParcel"),
 ("android.view.InputChannel", "nativeReadFromParcel"),
 ("android.view.InputEventReceiver", "nativeInit"),
 ("com.android.icu.charset.NativeConverter", "openConverter"),
 ("com.android.icu.charset.NativeConverter", "encode"),
 ("com.android.icu.charset.NativeConverter", "setCallbackDecode"),
 ("jdk.internal.misc.Unsafe", "getReferenceVolatile"),
 ("jdk.internal.misc.Unsafe", "park"),
 ("libcore.io.Linux", "mmap"),
 ("libcore.io.Linux", "writeBytes"),
 ("android.graphics.ImageDecoder", "nDecodeBitmap"),
 ("android.graphics.ImageDecoder", "nCreate"),
 ("android.graphics.RuntimeShader", "nativeCreateShader"),
 ("android.graphics.animation.RenderNodeAnimator", "nCreateCanvasPropertyFloatAnimator"),
 ("android.animation.PropertyValuesHolder", "nGetFloatMethod"),
 ("android.animation.PropertyValuesHolder", "nCallFloatMethod"),
 ("android.os.Trace", "nativeGetEnabledTags"),
 ("android.os.SystemProperties", "native_find"),
 ("android.os.SystemProperties", "native_get_long"),
 ("android.util.MemoryIntArray", "nativeOpen"),
 ("libcore.util.NativeAllocationRegistry", "applyFreeFunction"),
 ("com.android.internal.util.VirtualRefBasePtr", "nIncStrong"),
 ("java.lang.Throwable", "nativeFillInStackTrace"),
 ("android.graphics.drawable.VectorDrawable", "nCreateTree"),
 ("android.graphics.drawable.VectorDrawable", "nCreateTreeFromCopy"),
}

# Trivial per-object getters/setters on opaque handles: mechanical state copies.
ACCESSOR = re.compile(r"^(n?(Set|Get)[A-Z]\w*|native_(get|find)\w*|nIs[A-Z]\w*|is[A-Z]\w*)$")

def main():
    acc = {}
    order = {}
    for phase, f in (("launch", "natives_activity_launch.tsv"),
                     ("interaction", "natives_activity_interaction.tsv"),
                     ("println", "natives_println.tsv")):
        for r in csv.DictReader(open(os.path.join(DATA, f)), delimiter="\t"):
            k = (r["class"], r["method"], r["signature"])
            e = acc.setdefault(k, {"launch": 0, "interaction": 0, "println": 0,
                                   "first_order": {}})
            e[phase] = int(r["enters"])
            e["first_order"][phase] = int(r["first_order"])
    out = []
    for k, v in acc.items():
        cls, meth, sig = k
        mod, cx, note = PLAN.get(cls, DEFAULTS)
        if (cls, meth) in L_METHODS:
            cx = "L"
        elif ACCESSOR.match(meth) and cx != "L":
            cx = "S"
        phases = "+".join(p for p in ("println", "launch", "interaction") if v[p])
        fo = min(v["first_order"].values())
        out.append({
            "class": cls, "method": meth, "signature": sig, "module": mod,
            "complexity": cx,
            "phases": phases,
            "enters_launch": v["launch"],
            "enters_interaction": v["interaction"],
            "enters_println": v["println"],
            "first_order": fo,
            "notes": note,
        })
    modrank = {"core.lang": 0, "core.icu": 1, "core.thread": 2, "core.vmrt": 3,
               "core.io": 4, "core.os": 5, "os.parcel": 6, "os.looper": 7,
               "res": 8, "render.env": 9, "render.canvas": 10, "render.paint": 11,
               "render.node": 12, "render.path": 13, "render.matrix": 14,
               "render.region": 15, "render.bitmap": 16, "render.shader": 17,
               "render.anim": 18, "render.vector": 19, "render.hwui": 20,
               "text.typeface": 21, "text.layout": 22, "text.icu": 23,
               "view.input": 24, "view.surface": 25, "view.vsync": 26,
               "display": 27, "core.other": 99}
    out.sort(key=lambda r: (modrank.get(r["module"], 50), -int(r["enters_launch"]),
                            r["class"], r["method"]))
    cols = ["module", "class", "method", "signature", "complexity", "phases",
            "enters_launch", "enters_interaction", "enters_println",
            "first_order", "notes"]
    dst = os.path.join(HERE, "natives_plan.tsv")
    with open(dst, "w", newline="") as fh:
        w = csv.DictWriter(fh, fieldnames=cols, delimiter="\t",
                           lineterminator="\n", quoting=csv.QUOTE_MINIMAL)
        w.writeheader()
        for r in out:
            w.writerow(r)
    print("wrote", dst, len(out), "rows")
    # summary
    import collections
    c = collections.Counter(r["module"] for r in out)
    cx = collections.Counter(r["complexity"] for r in out)
    print("by module:", dict(c))
    print("by complexity:", dict(cx))
    un = [r for r in out if r["module"] == "core.other"]
    print("unclassified:", len(un), [r["class"] for r in un])

if __name__ == "__main__":
    main()
